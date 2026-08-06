import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import "./settings.css";

type SettingsData = {
  shortcut: string;
  maxResults: number;
  fontSize: number;
  indexIntervalSecs: number;
  autostart: boolean;
  popupWidth: number;
  popupHeight: number;
  popupPosition: "left" | "center" | "right";
  remoteServerEnabled: boolean;
  remoteServerPort: number;
  remoteServerToken: string;
  searchMode: "local" | "remote" | "hybrid";
  remoteUrl: string;
  remoteToken: string;
  remoteTimeoutMs: number;
  posFilterEnabled: boolean;
};

type FolderRow = {
  id: number;
  path: string;
  publicPath: string;
  enabled: boolean;
  indexedCount: number;
};
type ExcludePathRow = { id: number; path: string };
type SearchWordRow = {
  id: number;
  word: string;
  reading?: string;
  posLabel?: string;
};

type TabId = "howto" | "folders" | "words" | "options" | "remote" | "credits";

type IndexProgressPayload = {
  folderId: number;
  current: number;
  total: number;
  phase: "counting" | "indexing";
};

function formatIndexProgress(p: IndexProgressPayload | null): string {
  if (!p) return "処琁E��…";
  if (p.phase === "counting") return "ファイル数を確認中…";
  return `${p.current.toLocaleString()} / ${p.total.toLocaleString()}`;
}

/** Left-hand-friendly shortcuts that avoid common Windows / IME reserved combos. */
const SHORTCUT_OPTIONS = [
  { value: "Ctrl+Alt+A", label: "Ctrl + Alt + A�E�推奨�E�E },
  { value: "Ctrl+Alt+Space", label: "Ctrl + Alt + Space" },
  { value: "Alt+Shift+Z", label: "Shift + Alt + Z" },
] as const;

const POPUP_POSITION_OPTIONS = [
  { value: "left", label: "左" },
  { value: "center", label: "中央" },
  { value: "right", label: "右" },
] as const;

const SEARCH_MODE_OPTIONS = [
  { value: "local", label: "ローカルのみ" },
  { value: "remote", label: "リモート�Eみ" },
  { value: "hybrid", label: "ハイブリチE���E�ローカル�E�リモート！E },
] as const;

const TABS: { id: TabId; label: string }[] = [
  { id: "howto", label: "操作方況E },
  { id: "folders", label: "検索対象フォルダ" },
  { id: "words", label: "検索ワード登録" },
  { id: "options", label: "吁E��設宁E },
  { id: "remote", label: "リモーチE },
  { id: "credits", label: "クレジチE��" },
];

const APP_VERSION = "1.4.5";

/** Direct runtime dependencies shown for attribution (not an exhaustive transitive list). */
const THIRD_PARTY_LICENSES: { name: string; license: string; note?: string }[] = [
  { name: "Tauri", license: "Apache-2.0 OR MIT" },
  { name: "@tauri-apps/api / plugins", license: "Apache-2.0 OR MIT" },
  { name: "React / React DOM", license: "MIT" },
  { name: "Tantivy", license: "MIT" },
  { name: "Lindera / lindera-tantivy", license: "MIT", note: "形態素解析辞書 IPADIC を埋め込み" },
  { name: "IPADIC�E�Eindera 経由�E�E, license: "IPADIC 独自ライセンス" },
  { name: "rusqlite / SQLite", license: "MIT / Public Domain" },
  { name: "axum / tower-http", license: "MIT" },
  { name: "tokio", license: "MIT" },
  { name: "reqwest", license: "Apache-2.0 OR MIT" },
  { name: "serde / serde_json", license: "Apache-2.0 OR MIT" },
  { name: "notify", license: "CC0-1.0" },
  { name: "walkdir", license: "MIT OR Unlicense" },
  { name: "pdf-extract", license: "MIT" },
  { name: "calamine", license: "MIT" },
  { name: "encoding_rs", license: "Apache-2.0 OR MIT", note: "HTML の斁E��コード判定�EチE��ーチE },
  { name: "quick-xml / zip", license: "MIT" },
  { name: "rwml", license: "MIT" },
  { name: "rjtd-core", license: "Apache-2.0" },
  { name: "arboard / enigo", license: "Apache-2.0 OR MIT / MIT" },
  { name: "windows (Rust crate)", license: "MIT OR Apache-2.0" },
];

function newToken() {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
    return crypto.randomUUID();
  }
  return `argos-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}

export default function Settings() {
  const [settings, setSettings] = useState<SettingsData | null>(null);
  const [folders, setFolders] = useState<FolderRow[]>([]);
  const [excludes, setExcludes] = useState<ExcludePathRow[]>([]);
  const [searchWords, setSearchWords] = useState<SearchWordRow[]>([]);
  const [folderInput, setFolderInput] = useState("");
  const [excludeInput, setExcludeInput] = useState("");
  const [wordInput, setWordInput] = useState("");
  const [editingWordId, setEditingWordId] = useState<number | null>(null);
  const [editingWordDraft, setEditingWordDraft] = useState("");
  const [publicPathDrafts, setPublicPathDrafts] = useState<Record<number, string>>({});
  const [publicPathOpenId, setPublicPathOpenId] = useState<number | null>(null);
  const [message, setMessage] = useState("");
  const [indexing, setIndexing] = useState(false);
  const [busyFolderId, setBusyFolderId] = useState<number | null>(null);
  const [indexProgress, setIndexProgress] = useState<IndexProgressPayload | null>(
    null,
  );
  const [tab, setTab] = useState<TabId>("howto");
  const [lanIp, setLanIp] = useState<string | null>(null);
  const [testing, setTesting] = useState(false);

  async function reload() {
    const s = await invoke<SettingsData>("get_settings");
    if (!SHORTCUT_OPTIONS.some((o) => o.value === s.shortcut)) {
      s.shortcut = SHORTCUT_OPTIONS[0].value;
    }
    if (!POPUP_POSITION_OPTIONS.some((o) => o.value === s.popupPosition)) {
      s.popupPosition = "center";
    }
    if (!SEARCH_MODE_OPTIONS.some((o) => o.value === s.searchMode)) {
      s.searchMode = "local";
    }
    if (typeof s.posFilterEnabled !== "boolean") {
      s.posFilterEnabled = true;
    }
    setSettings(s);
    const nextFolders = await invoke<FolderRow[]>("list_folders");
    setFolders(nextFolders);
    setPublicPathDrafts(
      Object.fromEntries(nextFolders.map((f) => [f.id, f.publicPath ?? ""])),
    );
    setExcludes(await invoke<ExcludePathRow[]>("list_exclude_paths"));
    setSearchWords(await invoke<SearchWordRow[]>("list_search_words"));
    try {
      const ip = await invoke<string | null>("get_lan_ip_hint");
      setLanIp(ip);
    } catch {
      setLanIp(null);
    }
  }

  useEffect(() => {
    void reload().catch(console.error);
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    listen("folders-updated", () => {
      void invoke<FolderRow[]>("list_folders")
        .then(setFolders)
        .catch(console.error);
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    listen("search-words-updated", () => {
      void invoke<SearchWordRow[]>("list_search_words")
        .then(setSearchWords)
        .catch(console.error);
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    listen<IndexProgressPayload>("index-progress", (event) => {
      const p = event.payload;
      setIndexProgress(p);
      setBusyFolderId(p.folderId);
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      unlisten?.();
    };
  }, []);

  // Settings window stays mounted; refresh when opening this tab.
  useEffect(() => {
    if (tab !== "words") return;
    void invoke<SearchWordRow[]>("list_search_words")
      .then(setSearchWords)
      .catch(console.error);
  }, [tab]);

  async function saveSettings() {
    if (!settings) return;
    const saved = await invoke<SettingsData>("update_settings", { settings });
    setSettings(saved);
    setMessage("設定を保存しました�E�ショートカチE��変更は再起動後に反映�E�E);
  }

  async function addFolder() {
    const path = folderInput.trim();
    if (!path) return;
    setIndexing(true);
    try {
      const row = await invoke<FolderRow>("add_folder", { path });
      setFolderInput("");
      setFolders((prev) =>
        prev.some((f) => f.id === row.id) ? prev : [...prev, row],
      );
      setPublicPathDrafts((prev) => ({
        ...prev,
        [row.id]: row.publicPath ?? "",
      }));
      setBusyFolderId(row.id);
      setIndexProgress(null);
      const stats = await invoke<{
        indexed: number;
        skipped: number;
        errors: number;
      }>("run_reindex_folder", { id: row.id });
      setMessage(
        `フォルダを追加しました�E�このフォルダのみ索弁E 登録 ${stats.indexed} / スキチE�E ${stats.skipped} / エラー ${stats.errors}�E�。以降�E変更は自動監視されます。`,
      );
      await reload();
    } catch (e) {
      setMessage(`失敁E ${String(e)}`);
      await reload().catch(() => undefined);
    } finally {
      setBusyFolderId(null);
      setIndexProgress(null);
      setIndexing(false);
    }
  }

  async function browseFolder() {
    const selected = await openDialog({
      directory: true,
      multiple: false,
      title: "検索対象フォルダを選抁E,
    });
    if (typeof selected === "string" && selected) {
      setFolderInput(selected);
    }
  }

  async function browseExcludeFolder() {
    const selected = await openDialog({
      directory: true,
      multiple: false,
      title: "除外フォルダを選抁E,
    });
    if (typeof selected === "string" && selected) {
      setExcludeInput(selected);
    }
  }

  async function savePublicPath(id: number) {
    const publicPath = (publicPathDrafts[id] ?? "").trim();
    setIndexing(true);
    setBusyFolderId(id);
    setIndexProgress(null);
    try {
      await invoke("update_folder_public_path", { id, publicPath });
      const stats = await invoke<{
        indexed: number;
        skipped: number;
        errors: number;
      }>("run_reindex_folder", { id });
      setMessage(
        `公開パスを更新しました�E�このフォルダのみ再索弁E 登録 ${stats.indexed} / スキチE�E ${stats.skipped} / エラー ${stats.errors}�E�`,
      );
      await reload();
    } catch (e) {
      setMessage(`失敁E ${String(e)}`);
    } finally {
      setBusyFolderId(null);
      setIndexProgress(null);
      setIndexing(false);
    }
  }

  async function removeFolder(id: number) {
    await invoke("remove_folder", { id });
    await reload();
  }

  async function runReindexFolder(id: number) {
    setIndexing(true);
    setBusyFolderId(id);
    setIndexProgress(null);
    try {
      const stats = await invoke<{
        indexed: number;
        skipped: number;
        errors: number;
      }>("run_reindex_folder", { id });
      setMessage(
        `完亁E��このフォルダのみ�E�E 登録 ${stats.indexed} / スキチE�E ${stats.skipped} / エラー ${stats.errors}`,
      );
      await reload();
    } catch (e) {
      setMessage(`失敁E ${String(e)}`);
    } finally {
      setBusyFolderId(null);
      setIndexProgress(null);
      setIndexing(false);
    }
  }

  async function addExclude() {
    const path = excludeInput.trim();
    if (!path) return;
    await invoke("add_exclude_path", { path });
    setExcludeInput("");
    await reload();
  }

  async function removeExclude(id: number) {
    await invoke("remove_exclude_path", { id });
    await reload();
  }

  async function addSearchWord() {
    const word = wordInput.trim();
    if (!word) return;
    try {
      await invoke("add_search_word", { word });
      setWordInput("");
      setMessage("検索ワードを追加しました");
      await reload();
    } catch (e) {
      setMessage(`追加失敁E ${String(e)}`);
    }
  }

  async function saveSearchWord(id: number) {
    const word = editingWordDraft.trim();
    if (!word) return;
    try {
      await invoke("update_search_word", { id, word });
      setEditingWordId(null);
      setEditingWordDraft("");
      setMessage("検索ワードを更新しました");
      await reload();
    } catch (e) {
      setMessage(`更新失敁E ${String(e)}`);
    }
  }

  async function removeSearchWord(id: number) {
    await invoke("remove_search_word", { id });
    if (editingWordId === id) {
      setEditingWordId(null);
      setEditingWordDraft("");
    }
    await reload();
  }

  async function clearAllSearchWords() {
    if (searchWords.length === 0) return;
    const ok = window.confirm(
      `登録済みの検索ワーチE${searchWords.length} 件をすべて削除しますか�E�\nこ�E操作�E取り消せません。`,
    );
    if (!ok) return;
    try {
      const n = await invoke<number>("clear_search_words");
      setEditingWordId(null);
      setEditingWordDraft("");
      setMessage(`検索ワードをすべて削除しました�E�E{n} 件�E�`);
      await reload();
    } catch (e) {
      setMessage(`一括削除失敁E ${String(e)}`);
    }
  }

  function parseSearchWordsCsv(text: string): {
    word: string;
    reading: string;
    posLabel: string;
  }[] {
    const lines = text.replace(/^\uFEFF/, "").split(/\r?\n/);
    const out: { word: string; reading: string; posLabel: string }[] = [];
    for (const line of lines) {
      const trimmed = line.trim();
      if (!trimmed || trimmed.startsWith("#")) continue;
      const parts: string[] = [];
      let cur = "";
      let inQuotes = false;
      for (let i = 0; i < trimmed.length; i += 1) {
        const ch = trimmed[i];
        if (ch === '"') {
          inQuotes = !inQuotes;
          continue;
        }
        if (ch === "," && !inQuotes) {
          parts.push(cur.trim());
          cur = "";
          continue;
        }
        cur += ch;
      }
      parts.push(cur.trim());
      if (parts[0]?.toLowerCase() === "surface" || parts[0] === "表層形") continue;
      const word = parts[0] ?? "";
      if (!word) continue;
      if (parts.length >= 3) {
        out.push({
          word,
          posLabel: parts[1] || "ユーザ辞書",
          reading: parts[2] || "",
        });
      } else if (parts.length === 2) {
        out.push({ word, posLabel: parts[1] || "ユーザ辞書", reading: "" });
      } else {
        out.push({ word, posLabel: "ユーザ辞書", reading: "" });
      }
    }
    return out;
  }

  function escapeCsv(s: string): string {
    if (/[",\n\r]/.test(s)) return `"${s.replace(/"/g, '""')}"`;
    return s;
  }

  function importSearchWordsCsv() {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = ".csv,text/csv,text/plain";
    input.onchange = async () => {
      const file = input.files?.[0];
      if (!file) return;
      try {
        const text = await file.text();
        const entries = parseSearchWordsCsv(text);
        if (entries.length === 0) {
          setMessage("CSV に有効な行がありません");
          return;
        }
        const result = await invoke<{
          added: number;
          updated: number;
          skipped: number;
        }>("import_search_words", { entries });
        setMessage(
          `CSV 取り込み: 追加 ${result.added} / 更新 ${result.updated} / スキチE�E ${result.skipped}`,
        );
        await reload();
      } catch (err) {
        setMessage(`CSV 取り込み失敁E ${String(err)}`);
      }
    };
    input.click();
  }

  function exportSearchWordsCsv() {
    const lines = searchWords.map((w) => {
      const reading = w.reading ?? "";
      const pos = w.posLabel || "ユーザ辞書";
      if (reading || (w.posLabel && w.posLabel !== "ユーザ辞書")) {
        return `${escapeCsv(w.word)},${escapeCsv(pos)},${escapeCsv(reading)}`;
      }
      return escapeCsv(w.word);
    });
    const blob = new Blob([`${lines.join("\n")}\n`], {
      type: "text/csv;charset=utf-8",
    });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = "argos-search-words.csv";
    a.click();
    URL.revokeObjectURL(url);
    setMessage("CSV を書き�Eしました");
  }

  async function runReindex() {
    setIndexing(true);
    setIndexProgress(null);
    setMessage("全フォルダを�E構築中…");
    try {
      const stats = await invoke<{
        indexed: number;
        skipped: number;
        errors: number;
      }>("run_reindex");
      setMessage(
        `完亁E���Eフォルダ再構築！E 登録 ${stats.indexed} / スキチE�E ${stats.skipped} / エラー ${stats.errors}`,
      );
      await reload();
    } catch (e) {
      setMessage(`失敁E ${String(e)}`);
    } finally {
      setBusyFolderId(null);
      setIndexProgress(null);
      setIndexing(false);
    }
  }

  async function testRemote() {
    if (!settings) return;
    setTesting(true);
    setMessage("接続テスト中…");
    try {
      // Persist current form values first so the backend uses them
      const saved = await invoke<SettingsData>("update_settings", { settings });
      setSettings(saved);
      const msg = await invoke<string>("test_remote_connection");
      setMessage(msg);
    } catch (e) {
      setMessage(`接続テスト失敁E ${String(e)}`);
    } finally {
      setTesting(false);
    }
  }

  const clientUrlHint =
    lanIp && settings
      ? `http://${lanIp}:${settings.remoteServerPort}`
      : settings
        ? `http://<こ�EPCのLAN IP>:${settings.remoteServerPort}`
        : "";

  if (!settings) {
    return <div className="settings loading">読み込み中…</div>;
  }

  return (
    <div className="settings">
      <nav className="settings-tabs" role="tablist" aria-label="設定グルーチE>
        {TABS.map((t) => (
          <button
            key={t.id}
            type="button"
            role="tab"
            id={`tab-${t.id}`}
            aria-selected={tab === t.id}
            aria-controls={`panel-${t.id}`}
            className={tab === t.id ? "active" : undefined}
            onClick={() => {
              setTab(t.id);
              setMessage("");
              setEditingWordId(null);
              setEditingWordDraft("");
            }}
          >
            {t.label}
          </button>
        ))}
      </nav>

      {tab === "howto" ? (
        <div
          className="tab-panel"
          role="tabpanel"
          id="panel-howto"
          aria-labelledby="tab-howto"
        >
          <section>
            <h2>Argos とは</h2>
            <p className="muted">
              任意�Eアプリで斁E���Eを選択しショートカチE��を押すと、登録フォルダ冁E�E PDF / DOCX / DOC /
              JTD / XLS / XLSX / TXT / Markdown / HTML / JSON から全斁E��索し、結果を�EチE�EアチE�Eで表示します、E
            </p>
          </section>

          <section>
            <h2>便利な使ぁE��</h2>
            <ul className="howto-tips">
              <li>
                メールめE��書の閲覧・編雁E��に、相手名・件名�Eキーワードを選択して検索し、E��連賁E��をすぐ参照できまぁE
              </li>
              <li>
                ブラウザめE��ャチE��で気になった語句を選び、手允E�Eフォルダから根拠めE��去賁E��を探せまぁE
              </li>
              <li>
                アプリを�Eり替えずポップアチE�Eで結果を確認し、忁E��ならそのままファイルを開けまぁE
              </li>
            </ul>
          </section>

          <section>
            <h2>はじめに</h2>
            <ol className="howto-steps">
              <li>
                「検索対象フォルダ」タブで検索したぁE��ォルダを追加しまぁE
              </li>
              <li>「今すぐインチE��クス」を実行して全斁E��索用の索引を作�EしまぁE/li>
              <li>
                任意�Eアプリで斁E���Eを選択し <kbd>{settings.shortcut}</kbd> を押しまぁE
              </li>
              <li>ポップアチE�Eで結果を確認しまぁE/li>
            </ol>
          </section>

          <section>
            <h2>ポップアチE�Eの操佁E/h2>
            <ul className="howto-keys">
              <li>
                <kbd>Enter</kbd>
                <span>選択中の結果を開ぁE/span>
              </li>
              <li>
                <kbd>Ctrl</kbd>+<kbd>Enter</kbd>
                <span>プレビューを表示</span>
              </li>
              <li>
                <kbd>Shift</kbd>+<kbd>Enter</kbd>
                <span>ファイルのフォルダを開ぁE/span>
              </li>
              <li>
                <kbd>ↁE/kbd> / <kbd>ↁE/kbd>
                <span>結果を移勁E/span>
              </li>
              <li>
                <kbd>Esc</kbd>
                <span>ポップアチE�Eを閉じる</span>
              </li>
            </ul>
          </section>

          <section>
            <h2>インチE��クスの共有！EAN�E�E/h2>
            <p className="muted">
              同じ LAN 上�E別 PC から、この PC の索引を検索できます。ファイル自体をコピ�Eする忁E���Eありません、E
            </p>
            <h3 className="howto-subhead">索引がある PC�E��Eスト！E/h3>
            <ol className="howto-steps">
              <li>「検索対象フォルダ」でフォルダを追加し、「今すぐインチE��クス」を実行しまぁE/li>
              <li>
                クライアントからもファイルを開けるようにする場合�E、フォルダの「�E開パス�E�ENC�E�」に共有パス�E�侁E{" "}
                <code>\\192.168.0.8\共有名</code>
                �E�を設定してから再インチE��クスします。すでに UNC で登録してぁE��場合�E空のままで構いません
              </li>
              <li>「リモート」タブで「リモート検索サーバを有効にする」をオンにしまぁE/li>
              <li>
                共有トークンを控えます（�E回有効化時に乱数が�E動生成されます。クライアントに同じ値を�Eれます！E
              </li>
              <li>
                Windows ファイアウォールでポ�Eト（既宁E<code>17890</code>�E��E受信を許可しまぁE
              </li>
            </ol>
            <h3 className="howto-subhead">検索する側の PC�E�クライアント！E/h3>
            <ol className="howto-steps">
              <li>
                「リモート」タブで検索モードを「リモート�Eみ」また�E「ハイブリチE��」にしまぁE
              </li>
              <li>
                リモーチEURL�E�侁E <code>http://192.168.0.8:17890</code>
                �E�とホストと同じト�Eクンを�E力しまぁE
              </li>
              <li>「接続テスト」で確認してから設定を保存しまぁE/li>
            </ol>
            <ul className="howto-tips">
              <li>
                ト�Eクンは <code>%APPDATA%\Argos\argos.db</code>{" "}
                に保存されます。漏洩が疑われる場合�Eホストで「トークンを�E生�E」し、�Eクライアントを更新してください
              </li>
              <li>
                ホストが <code>C:\...</code>{" "}
                などローカルパスだけを索引してぁE��と、クライアントではプレビューはできてもファイルを開けなぁE��とがありまぁE
              </li>
              <li>詳細な頁E��は「リモート」タブでも設定�E確認できまぁE/li>
            </ul>
          </section>

          <section>
            <h2>ヒンチE/h2>
            <ul className="howto-tips">
              <li>登録フォルダ冁E�Eファイル変更は自動で監視され、索引に反映されまぁE/li>
              <li>ショートカチE��キーの変更はアプリ再起動後に反映されまぁE/li>
              <li>
                チE�Eタは <code>%APPDATA%\Argos\</code> に保存されまぁE
              </li>
            </ul>
          </section>
        </div>
      ) : null}

      {tab === "folders" ? (
        <div
          className="tab-panel"
          role="tabpanel"
          id="panel-folders"
          aria-labelledby="tab-folders"
        >
          <section>
            <h2>検索対象フォルダ</h2>
            <p className="muted">
              ここに追加したフォルダ冁E�E斁E��が検索対象になります、EAN
              公開が忁E��な場合�Eみ、各フォルダの「UNC」から�E有パスを設定できます、E
            </p>
            <div className="row">
              <input
                placeholder="侁E D:\事件 また�E \\他PC\共朁E
                value={folderInput}
                onChange={(e) => setFolderInput(e.target.value)}
              />
              <button type="button" onClick={() => void browseFolder()}>
                参�E…
              </button>
              <button
                type="button"
                disabled={indexing}
                onClick={() => void addFolder()}
              >
                追加
              </button>
            </div>
            <ul className="folder-list">
              {folders.length === 0 ? (
                <li className="empty">フォルダがまだ登録されてぁE��せん</li>
              ) : (
                folders.map((f) => {
                  const publicOpen = publicPathOpenId === f.id;
                  const publicDraft =
                    publicPathDrafts[f.id] ?? f.publicPath ?? "";
                  const hasPublicPath = publicDraft.trim().length > 0;
                  const isBusy = busyFolderId === f.id;
                  return (
                    <li
                      key={f.id}
                      className={
                        isBusy ? "folder-item is-busy" : "folder-item"
                      }
                    >
                      <div className="folder-item-top">
                        <span className="folder-main">
                          <span className="folder-path">{f.path}</span>
                          <span className="folder-count">
                            {f.indexedCount.toLocaleString()} 件
                          </span>
                        </span>
                        {isBusy ? (
                          <span
                            className="folder-busy"
                            title="こ�Eフォルダの索引を処琁E��でぁE
                          >
                            {indexProgress &&
                            indexProgress.folderId === f.id
                              ? formatIndexProgress(indexProgress)
                              : "処琁E��…"}
                          </span>
                        ) : (
                          <span className="folder-actions">
                            <button
                              type="button"
                              className={
                                publicOpen || hasPublicPath
                                  ? "folder-public-toggle is-active"
                                  : "folder-public-toggle"
                              }
                              aria-expanded={publicOpen}
                              title={
                                hasPublicPath
                                  ? `LAN公開用のUNCパスを表示・編雁E��設定渁E ${publicDraft}�E�`
                                  : "LAN公開用のUNCパスを表示・編雁E
                              }
                              onClick={() =>
                                setPublicPathOpenId(publicOpen ? null : f.id)
                              }
                            >
                              UNC
                              {hasPublicPath && !publicOpen ? " ✁E : ""}
                            </button>
                            <button
                              type="button"
                              disabled={indexing}
                              title="こ�Eフォルダだけ索引を読み込み直ぁE
                              onClick={() => void runReindexFolder(f.id)}
                            >
                              読込
                            </button>
                            <button
                              type="button"
                              className="danger"
                              disabled={indexing}
                              title="こ�Eフォルダを検索対象から削除する"
                              onClick={() => void removeFolder(f.id)}
                            >
                              削除
                            </button>
                          </span>
                        )}
                      </div>
                      {publicOpen && !isBusy ? (
                        <label className="folder-public">
                          <span className="field-label">
                            公開パス�E�ENC�E� ELAN 上�E別 PC から開く場合に設宁E
                          </span>
                          <span className="folder-public-row">
                            <input
                              placeholder="侁E \\こ�EPC名\共有名�E�空なら上記パスをそのまま使用�E�E
                              value={publicDraft}
                              onChange={(e) =>
                                setPublicPathDrafts((prev) => ({
                                  ...prev,
                                  [f.id]: e.target.value,
                                }))
                              }
                            />
                            <button
                              type="button"
                              disabled={indexing}
                              title="UNCパスを保存し、このフォルダだけ�E索引すめE
                              onClick={() => void savePublicPath(f.id)}
                            >
                              保孁E
                            </button>
                          </span>
                        </label>
                      ) : null}
                    </li>
                  );
                })
              )}
            </ul>
            <p className="field-hint">
              フォルダ追加時�Eこ�Eフォルダだけ�E動で索引されます。UNC
              は忁E��なときだけ「UNC」から設定してください、E
            </p>
          </section>

          <section>
            <h2>除外フォルダ</h2>
            <p className="muted">検索・インチE��クスから除外するパスを指定します、E/p>
            <div className="row">
              <input
                placeholder="除外するフォルダパス"
                value={excludeInput}
                onChange={(e) => setExcludeInput(e.target.value)}
              />
              <button type="button" onClick={() => void browseExcludeFolder()}>
                参�E…
              </button>
              <button type="button" onClick={() => void addExclude()}>
                追加
              </button>
            </div>
            <ul>
              {excludes.length === 0 ? (
                <li className="empty">除外パスはありません</li>
              ) : (
                excludes.map((f) => (
                  <li key={f.id}>
                    <span>{f.path}</span>
                    <button
                      type="button"
                      className="danger"
                      onClick={() => void removeExclude(f.id)}
                    >
                      削除
                    </button>
                  </li>
                ))
              )}
            </ul>
          </section>

          <section>
            <h2>インチE��クス</h2>
            <p className="muted">
              登録フォルダ冁E�E PDF / DOCX / DOC / JTD / XLS / XLSX / TXT / Markdown / HTML / JSON
              を検索用に登録します。既存フォルダのファイル変更は自動監視されるため、E��常はフォルダ追加時�E自動索引だけで十�Eです。�Eフォルダ再構築�E、索引�E不整合を直すときだけ使ってください、E
            </p>
            <button
              type="button"
              className="primary"
              disabled={indexing}
              onClick={() => void runReindex()}
            >
              {indexing
                ? indexProgress
                  ? formatIndexProgress(indexProgress)
                  : "処琁E��…"
                : "全フォルダを�E構篁E}
            </button>
            {message ? <p className="msg">{message}</p> : null}
          </section>
        </div>
      ) : null}

      {tab === "words" ? (
        <div
          className="tab-panel"
          role="tabpanel"
          id="panel-words"
          aria-labelledby="tab-words"
        >
          <section>
            <h2>検索ワード登録</h2>
            <p className="muted">
              法律用語などの褁E��語を登録すると、検索時に隣接フレーズとして扱われます（索引�E刁E��は変えなぁE��め、E��刁E��でもヒチE��します）、E
              吁E��設定�E品詞フィルタと連携し、登録語�Eの助詞�E除外されません。検索ポップアチE�Eの「＋」から挿入・そ�E場登録もできます、E
            </p>
            <div className="row">
              <input
                placeholder="侁E 弁済による代佁E/ 損害賠儁E
                value={wordInput}
                onChange={(e) => setWordInput(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    e.preventDefault();
                    void addSearchWord();
                  }
                }}
              />
              <button type="button" onClick={() => void addSearchWord()}>
                追加
              </button>
            </div>
            <div className="row" style={{ marginTop: "0.5rem" }}>
              <button type="button" onClick={() => importSearchWordsCsv()}>
                CSV 取り込み
              </button>
              <button
                type="button"
                onClick={() => exportSearchWordsCsv()}
                disabled={searchWords.length === 0}
              >
                CSV 書き�EぁE
              </button>
              <button
                type="button"
                className="danger"
                onClick={() => void clearAllSearchWords()}
                disabled={searchWords.length === 0}
              >
                すべて削除
              </button>
            </div>
            <p className="field-hint">
              CSV 形弁E 1列（表層�E�また�E 表層,品詁E読み。法令から抽出した用語リスト�E一括登録向けです、E
            </p>
            <ul>
              {searchWords.length === 0 ? (
                <li className="empty">検索ワード�Eまだ登録されてぁE��せん</li>
              ) : (
                searchWords.map((w) => (
                  <li key={w.id} className="search-word-item">
                    {editingWordId === w.id ? (
                      <>
                        <input
                          className="search-word-edit-input"
                          value={editingWordDraft}
                          autoFocus
                          onChange={(e) => setEditingWordDraft(e.target.value)}
                          onKeyDown={(e) => {
                            if (e.key === "Enter") {
                              e.preventDefault();
                              void saveSearchWord(w.id);
                            }
                            if (e.key === "Escape") {
                              setEditingWordId(null);
                              setEditingWordDraft("");
                            }
                          }}
                        />
                        <span className="search-word-actions">
                          <button type="button" onClick={() => void saveSearchWord(w.id)}>
                            保孁E
                          </button>
                          <button
                            type="button"
                            onClick={() => {
                              setEditingWordId(null);
                              setEditingWordDraft("");
                            }}
                          >
                            キャンセル
                          </button>
                        </span>
                      </>
                    ) : (
                      <>
                        <span>{w.word}</span>
                        <span className="search-word-actions">
                          <button
                            type="button"
                            onClick={() => {
                              setEditingWordId(w.id);
                              setEditingWordDraft(w.word);
                            }}
                          >
                            編雁E
                          </button>
                          <button
                            type="button"
                            className="danger"
                            onClick={() => void removeSearchWord(w.id)}
                          >
                            削除
                          </button>
                        </span>
                      </>
                    )}
                  </li>
                ))
              )}
            </ul>
            {message ? <p className="msg">{message}</p> : null}
          </section>
        </div>
      ) : null}

      {tab === "options" ? (
        <div
          className="tab-panel"
          role="tabpanel"
          id="panel-options"
          aria-labelledby="tab-options"
        >
          <section className="options-form">
            <h2>吁E��設宁E/h2>
            <label>
              <span className="field-label">ショートカチE��キー</span>
              <span className="field-leader" aria-hidden="true" />
              <select
                value={settings.shortcut}
                onChange={(e) => setSettings({ ...settings, shortcut: e.target.value })}
              >
                {SHORTCUT_OPTIONS.map((o) => (
                  <option key={o.value} value={o.value}>
                    {o.label}
                  </option>
                ))}
              </select>
            </label>
            <p className="field-hint">
              左手で押しやすい候補です。Shift + Alt + Z
              は、OS の入力言語�E替�E�Elt + Shift�E�と干渉することがあります。変更は再起動後に反映されます、E
            </p>
            <label>
              <span className="field-label">検索結果表示数</span>
              <span className="field-leader" aria-hidden="true" />
              <input
                type="number"
                min={1}
                max={50}
                value={settings.maxResults}
                onChange={(e) =>
                  setSettings({ ...settings, maxResults: Number(e.target.value) || 10 })
                }
              />
            </label>
            <label>
              <span className="field-label">フォントサイズ</span>
              <span className="field-leader" aria-hidden="true" />
              <input
                type="number"
                min={11}
                max={24}
                value={settings.fontSize}
                onChange={(e) =>
                  setSettings({ ...settings, fontSize: Number(e.target.value) || 14 })
                }
              />
            </label>
            <label>
              <span className="field-label">検索ポップアチE�Eの初期幁E��Ex�E�E/span>
              <span className="field-leader" aria-hidden="true" />
              <input
                type="number"
                min={320}
                max={1200}
                value={settings.popupWidth}
                onChange={(e) =>
                  setSettings({
                    ...settings,
                    popupWidth: Number(e.target.value) || 640,
                  })
                }
              />
            </label>
            <label>
              <span className="field-label">検索ポップアチE�Eの初期高さ�E�Ex�E�E/span>
              <span className="field-leader" aria-hidden="true" />
              <input
                type="number"
                min={280}
                max={1000}
                value={settings.popupHeight}
                onChange={(e) =>
                  setSettings({
                    ...settings,
                    popupHeight: Number(e.target.value) || 520,
                  })
                }
              />
            </label>
            <label>
              <span className="field-label">検索ポップアチE�Eの出現位置</span>
              <span className="field-leader" aria-hidden="true" />
              <select
                value={settings.popupPosition}
                onChange={(e) =>
                  setSettings({
                    ...settings,
                    popupPosition: e.target.value as SettingsData["popupPosition"],
                  })
                }
              >
                {POPUP_POSITION_OPTIONS.map((o) => (
                  <option key={o.value} value={o.value}>
                    {o.label}
                  </option>
                ))}
              </select>
            </label>
            <p className="field-hint">
              ポップアチE�Eを開き直すときにサイズ・位置が適用されます。表示中の手動移動�Eリサイズは、E��じるまで維持されます、E
            </p>
            <label>
              <span className="field-label">インチE��クス更新間隔�E�秒！E/span>
              <span className="field-leader" aria-hidden="true" />
              <input
                type="number"
                min={60}
                value={settings.indexIntervalSecs}
                onChange={(e) =>
                  setSettings({
                    ...settings,
                    indexIntervalSecs: Number(e.target.value) || 3600,
                  })
                }
              />
            </label>
            <label className="row-check">
              <input
                type="checkbox"
                checked={settings.autostart}
                onChange={(e) =>
                  setSettings({ ...settings, autostart: e.target.checked })
                }
              />
              Windows ログオン時に自動起勁E
            </label>
            <label className="row-check">
              <input
                type="checkbox"
                checked={settings.posFilterEnabled}
                onChange={(e) =>
                  setSettings({ ...settings, posFilterEnabled: e.target.checked })
                }
              />
              助詞�E助動詞を検索から除外する（品詞フィルタ�E�E
            </label>
            <p className="field-hint">
              自然斁E��エリのノイズを減らします。ユーザ辞書めE&quot;フレーズ&quot; 冁E�E助詞�E除外しません、E
            </p>
            <button type="button" className="primary" onClick={() => void saveSettings()}>
              設定を保孁E
            </button>
            {message ? <p className="msg">{message}</p> : null}
          </section>
        </div>
      ) : null}

      {tab === "remote" ? (
        <div
          className="tab-panel"
          role="tabpanel"
          id="panel-remote"
          aria-labelledby="tab-remote"
        >
          <section>
            <h2>こ�E PC を検索ホストにする</h2>
            <p className="muted">
              有効にすると、ローカル索引を LAN 上�E他�E Argos から検索できるようになります（既定�EーチE
              17890�E�。Windows ファイアウォールで当該ポ�Eト�E受信を許可してください、E
            </p>
            <label className="row-check">
              <input
                type="checkbox"
                checked={settings.remoteServerEnabled}
                onChange={(e) => {
                  const enabled = e.target.checked;
                  setSettings({
                    ...settings,
                    remoteServerEnabled: enabled,
                    remoteServerToken:
                      enabled && !settings.remoteServerToken.trim()
                        ? newToken()
                        : settings.remoteServerToken,
                  });
                }}
              />
              リモート検索サーバを有効にする
            </label>
            <div className="options-form">
              <label>
                <span className="field-label">ポ�EチE/span>
                <span className="field-leader" aria-hidden="true" />
                <input
                  type="number"
                  min={1}
                  max={65535}
                  value={settings.remoteServerPort}
                  onChange={(e) =>
                    setSettings({
                      ...settings,
                      remoteServerPort: Number(e.target.value) || 17890,
                    })
                  }
                />
              </label>
              <label>
                <span className="field-label">共有トークン</span>
                <span className="field-leader" aria-hidden="true" />
                <input
                  type="text"
                  value={settings.remoteServerToken}
                  onChange={(e) =>
                    setSettings({ ...settings, remoteServerToken: e.target.value })
                  }
                />
              </label>
            </div>
            <div className="row">
              <button
                type="button"
                onClick={() =>
                  setSettings({ ...settings, remoteServerToken: newToken() })
                }
              >
                ト�Eクンを�E生�E
              </button>
            </div>
            <p className="field-hint">
              初回有効化時に UUID 乱数が�E動生成されます。任意�E斁E���Eへの変更めE��トークンを�E生�E」も可能です。トークンは{" "}
              <code>%APPDATA%\Argos\argos.db</code>{" "}
              に保存されます。漏洩時�E再生成�EぁE��、�Eクライアント�Eリモートトークンを更新してください、E
            </p>
            <p className="field-hint">
              クライアント用 URL の侁E <code>{clientUrlHint}</code>
              �E�トークンもクライアントに同じも�Eを設定！E
            </p>
          </section>

          <section>
            <h2>他�E Argos を検索する�E�クライアント！E/h2>
            <p className="muted">
              ホスト�Eでサーバを有効にしたあと、こちらで URL とト�Eクンを設定します、E
            </p>
            <div className="options-form">
              <label>
                <span className="field-label">検索モーチE/span>
                <span className="field-leader" aria-hidden="true" />
                <select
                  value={settings.searchMode}
                  onChange={(e) =>
                    setSettings({
                      ...settings,
                      searchMode: e.target.value as SettingsData["searchMode"],
                    })
                  }
                >
                  {SEARCH_MODE_OPTIONS.map((o) => (
                    <option key={o.value} value={o.value}>
                      {o.label}
                    </option>
                  ))}
                </select>
              </label>
              <label>
                <span className="field-label">リモーチEURL</span>
                <span className="field-leader" aria-hidden="true" />
                <input
                  type="text"
                  placeholder="http://192.168.x.x:17890"
                  value={settings.remoteUrl}
                  onChange={(e) =>
                    setSettings({ ...settings, remoteUrl: e.target.value })
                  }
                />
              </label>
              <label>
                <span className="field-label">リモートトークン</span>
                <span className="field-leader" aria-hidden="true" />
                <input
                  type="text"
                  value={settings.remoteToken}
                  onChange={(e) =>
                    setSettings({ ...settings, remoteToken: e.target.value })
                  }
                />
              </label>
              <label>
                <span className="field-label">タイムアウト！Es�E�E/span>
                <span className="field-leader" aria-hidden="true" />
                <input
                  type="number"
                  min={500}
                  max={60000}
                  value={settings.remoteTimeoutMs}
                  onChange={(e) =>
                    setSettings({
                      ...settings,
                      remoteTimeoutMs: Number(e.target.value) || 3000,
                    })
                  }
                />
              </label>
            </div>
            <div className="row">
              <button
                type="button"
                disabled={testing}
                onClick={() => void testRemote()}
              >
                {testing ? "チE��ト中…" : "接続テスチE}
              </button>
            </div>
            <p className="field-hint">
              リモート結果のファイルパスが�Eスト�Eローカルパス�E�侁E C:\…�E��E場合、この
              PC からは開けなぁE��とがあります。�Eスト�Eでフォルダの「�E開パス�E�ENC�E�」を設定し、�EインチE��クスしてください、E
            </p>
          </section>

          <section>
            <button type="button" className="primary" onClick={() => void saveSettings()}>
              設定を保孁E
            </button>
            {message ? <p className="msg">{message}</p> : null}
          </section>
        </div>
      ) : null}

      {tab === "credits" ? (
        <div
          className="tab-panel"
          role="tabpanel"
          id="panel-credits"
          aria-labelledby="tab-credits"
        >
          <section className="credits-block">
            <h2>開発老E/h2>
            <p className="credits-org">半蔵門総合法律事務所</p>
            <p className="credits-person">弁護士　吉田秀平</p>
            <p className="muted credits-meta">Argos v{APP_VERSION}</p>
          </section>

          <section className="credits-block">
            <h2>利用条件</h2>
            <p className="muted">
              本ソフトウェア�E�Ergos�E��E、Apache License 2.0 又�Eこれと同等�E条件に基づき提供されます、E
              ソースコード�Eバイナリの利用・褁E��・改変�E再�E币E�E、当該ライセンスの条件に従って行ってください、E
            </p>
            <ul className="credits-terms">
              <li>著作権表示およびライセンス表示を保持すること</li>
              <li>改変した場合�Eそ�E旨を�E示すること</li>
              <li>本ソフトウェアは「現状有姿」で提供され、�E示・黙示を問わず保証はありません</li>
              <li>詳細は Apache License, Version 2.0�E�Ettps://www.apache.org/licenses/LICENSE-2.0�E�を参�E</li>
            </ul>
          </section>

          <section className="credits-block">
            <h2>使用ライブラリ�E�クレジチE���E�E/h2>
            <p className="muted">
              本アプリは次のオープンソースライブラリ等を利用してぁE��す。各ライブラリの著作権はそれぞれの権利老E��帰属し、記載�Eライセンス条件に従います。間接依存も含め、完�Eな一覧ではなぁE��合があります、E
            </p>
            <ul className="credits-libs">
              {THIRD_PARTY_LICENSES.map((lib) => (
                <li key={lib.name}>
                  <span className="credits-lib-name">{lib.name}</span>
                  <span className="credits-lib-license">{lib.license}</span>
                  {lib.note ? <span className="credits-lib-note">{lib.note}</span> : null}
                </li>
              ))}
            </ul>
          </section>
        </div>
      ) : null}
    </div>
  );
}
