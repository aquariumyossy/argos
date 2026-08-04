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

/** Left-hand-friendly shortcuts that avoid common Windows / IME reserved combos. */
const SHORTCUT_OPTIONS = [
  { value: "Ctrl+Alt+A", label: "Ctrl + Alt + A（推奨）" },
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
  { value: "remote", label: "リモートのみ" },
  { value: "hybrid", label: "ハイブリッド（ローカル＋リモート）" },
] as const;

const TABS: { id: TabId; label: string }[] = [
  { id: "howto", label: "操作方法" },
  { id: "folders", label: "検索対象フォルダ" },
  { id: "words", label: "検索ワード登録" },
  { id: "options", label: "各種設定" },
  { id: "remote", label: "リモート" },
  { id: "credits", label: "クレジット" },
];

const APP_VERSION = "1.2.0";

/** Direct runtime dependencies shown for attribution (not an exhaustive transitive list). */
const THIRD_PARTY_LICENSES: { name: string; license: string; note?: string }[] = [
  { name: "Tauri", license: "Apache-2.0 OR MIT" },
  { name: "@tauri-apps/api / plugins", license: "Apache-2.0 OR MIT" },
  { name: "React / React DOM", license: "MIT" },
  { name: "Tantivy", license: "MIT" },
  { name: "Lindera / lindera-tantivy", license: "MIT", note: "形態素解析辞書 IPADIC を埋め込み" },
  { name: "IPADIC（Lindera 経由）", license: "IPADIC 独自ライセンス" },
  { name: "rusqlite / SQLite", license: "MIT / Public Domain" },
  { name: "axum / tower-http", license: "MIT" },
  { name: "tokio", license: "MIT" },
  { name: "reqwest", license: "Apache-2.0 OR MIT" },
  { name: "serde / serde_json", license: "Apache-2.0 OR MIT" },
  { name: "notify", license: "CC0-1.0" },
  { name: "walkdir", license: "MIT OR Unlicense" },
  { name: "pdf-extract", license: "MIT" },
  { name: "calamine", license: "MIT" },
  { name: "encoding_rs", license: "Apache-2.0 OR MIT", note: "HTML の文字コード判定・デコード" },
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
    setMessage("設定を保存しました（ショートカット変更は再起動後に反映）");
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
      const stats = await invoke<{
        indexed: number;
        skipped: number;
        errors: number;
      }>("run_reindex_folder", { id: row.id });
      setMessage(
        `フォルダを追加しました（このフォルダのみ索引: 登録 ${stats.indexed} / スキップ ${stats.skipped} / エラー ${stats.errors}）。以降の変更は自動監視されます。`,
      );
      await reload();
    } catch (e) {
      setMessage(`失敗: ${String(e)}`);
      await reload().catch(() => undefined);
    } finally {
      setBusyFolderId(null);
      setIndexing(false);
    }
  }

  async function browseFolder() {
    const selected = await openDialog({
      directory: true,
      multiple: false,
      title: "検索対象フォルダを選択",
    });
    if (typeof selected === "string" && selected) {
      setFolderInput(selected);
    }
  }

  async function browseExcludeFolder() {
    const selected = await openDialog({
      directory: true,
      multiple: false,
      title: "除外フォルダを選択",
    });
    if (typeof selected === "string" && selected) {
      setExcludeInput(selected);
    }
  }

  async function savePublicPath(id: number) {
    const publicPath = (publicPathDrafts[id] ?? "").trim();
    setIndexing(true);
    setBusyFolderId(id);
    try {
      await invoke("update_folder_public_path", { id, publicPath });
      const stats = await invoke<{
        indexed: number;
        skipped: number;
        errors: number;
      }>("run_reindex_folder", { id });
      setMessage(
        `公開パスを更新しました（このフォルダのみ再索引: 登録 ${stats.indexed} / スキップ ${stats.skipped} / エラー ${stats.errors}）`,
      );
      await reload();
    } catch (e) {
      setMessage(`失敗: ${String(e)}`);
    } finally {
      setBusyFolderId(null);
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
    try {
      const stats = await invoke<{
        indexed: number;
        skipped: number;
        errors: number;
      }>("run_reindex_folder", { id });
      setMessage(
        `完了（このフォルダのみ）: 登録 ${stats.indexed} / スキップ ${stats.skipped} / エラー ${stats.errors}`,
      );
      await reload();
    } catch (e) {
      setMessage(`失敗: ${String(e)}`);
    } finally {
      setBusyFolderId(null);
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
      setMessage(`追加失敗: ${String(e)}`);
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
      setMessage(`更新失敗: ${String(e)}`);
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
          `CSV 取り込み: 追加 ${result.added} / 更新 ${result.updated} / スキップ ${result.skipped}`,
        );
        await reload();
      } catch (err) {
        setMessage(`CSV 取り込み失敗: ${String(err)}`);
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
    setMessage("CSV を書き出しました");
  }

  async function runReindex() {
    setIndexing(true);
    setMessage("全フォルダを再構築中…");
    try {
      const stats = await invoke<{
        indexed: number;
        skipped: number;
        errors: number;
      }>("run_reindex");
      setMessage(
        `完了（全フォルダ再構築）: 登録 ${stats.indexed} / スキップ ${stats.skipped} / エラー ${stats.errors}`,
      );
      await reload();
    } catch (e) {
      setMessage(`失敗: ${String(e)}`);
    } finally {
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
      setMessage(`接続テスト失敗: ${String(e)}`);
    } finally {
      setTesting(false);
    }
  }

  const clientUrlHint =
    lanIp && settings
      ? `http://${lanIp}:${settings.remoteServerPort}`
      : settings
        ? `http://<このPCのLAN IP>:${settings.remoteServerPort}`
        : "";

  if (!settings) {
    return <div className="settings loading">読み込み中…</div>;
  }

  return (
    <div className="settings">
      <nav className="settings-tabs" role="tablist" aria-label="設定グループ">
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
              任意のアプリで文字列を選択しショートカットを押すと、登録フォルダ内の PDF / DOCX / DOC /
              JTD / XLS / XLSX / TXT / Markdown / HTML から全文検索し、結果をポップアップで表示します。
            </p>
          </section>

          <section>
            <h2>便利な使い方</h2>
            <ul className="howto-tips">
              <li>
                メールや文書の閲覧・編集中に、相手名・件名・キーワードを選択して検索し、関連資料をすぐ参照できます
              </li>
              <li>
                ブラウザやチャットで気になった語句を選び、手元のフォルダから根拠や過去資料を探せます
              </li>
              <li>
                アプリを切り替えずポップアップで結果を確認し、必要ならそのままファイルを開けます
              </li>
            </ul>
          </section>

          <section>
            <h2>はじめに</h2>
            <ol className="howto-steps">
              <li>
                「検索対象フォルダ」タブで検索したいフォルダを追加します
              </li>
              <li>「今すぐインデックス」を実行して全文検索用の索引を作成します</li>
              <li>
                任意のアプリで文字列を選択し <kbd>{settings.shortcut}</kbd> を押します
              </li>
              <li>ポップアップで結果を確認します</li>
            </ol>
          </section>

          <section>
            <h2>ポップアップの操作</h2>
            <ul className="howto-keys">
              <li>
                <kbd>Enter</kbd>
                <span>選択中の結果を開く</span>
              </li>
              <li>
                <kbd>Ctrl</kbd>+<kbd>Enter</kbd>
                <span>プレビューを表示</span>
              </li>
              <li>
                <kbd>Shift</kbd>+<kbd>Enter</kbd>
                <span>ファイルのフォルダを開く</span>
              </li>
              <li>
                <kbd>↑</kbd> / <kbd>↓</kbd>
                <span>結果を移動</span>
              </li>
              <li>
                <kbd>Esc</kbd>
                <span>ポップアップを閉じる</span>
              </li>
            </ul>
          </section>

          <section>
            <h2>インデックスの共有（LAN）</h2>
            <p className="muted">
              同じ LAN 上の別 PC から、この PC の索引を検索できます。ファイル自体をコピーする必要はありません。
            </p>
            <h3 className="howto-subhead">索引がある PC（ホスト）</h3>
            <ol className="howto-steps">
              <li>「検索対象フォルダ」でフォルダを追加し、「今すぐインデックス」を実行します</li>
              <li>
                クライアントからもファイルを開けるようにする場合は、フォルダの「公開パス（UNC）」に共有パス（例:{" "}
                <code>\\192.168.0.8\共有名</code>
                ）を設定してから再インデックスします。すでに UNC で登録している場合は空のままで構いません
              </li>
              <li>「リモート」タブで「リモート検索サーバを有効にする」をオンにします</li>
              <li>
                共有トークンを控えます（初回有効化時に乱数が自動生成されます。クライアントに同じ値を入れます）
              </li>
              <li>
                Windows ファイアウォールでポート（既定 <code>17890</code>）の受信を許可します
              </li>
            </ol>
            <h3 className="howto-subhead">検索する側の PC（クライアント）</h3>
            <ol className="howto-steps">
              <li>
                「リモート」タブで検索モードを「リモートのみ」または「ハイブリッド」にします
              </li>
              <li>
                リモート URL（例: <code>http://192.168.0.8:17890</code>
                ）とホストと同じトークンを入力します
              </li>
              <li>「接続テスト」で確認してから設定を保存します</li>
            </ol>
            <ul className="howto-tips">
              <li>
                トークンは <code>%APPDATA%\Argos\argos.db</code>{" "}
                に保存されます。漏洩が疑われる場合はホストで「トークンを再生成」し、全クライアントを更新してください
              </li>
              <li>
                ホストが <code>C:\...</code>{" "}
                などローカルパスだけを索引していると、クライアントではプレビューはできてもファイルを開けないことがあります
              </li>
              <li>詳細な項目は「リモート」タブでも設定・確認できます</li>
            </ul>
          </section>

          <section>
            <h2>ヒント</h2>
            <ul className="howto-tips">
              <li>登録フォルダ内のファイル変更は自動で監視され、索引に反映されます</li>
              <li>ショートカットキーの変更はアプリ再起動後に反映されます</li>
              <li>
                データは <code>%APPDATA%\Argos\</code> に保存されます
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
              ここに追加したフォルダ内の文書が検索対象になります。LAN
              公開が必要な場合のみ、各フォルダの「UNC」から共有パスを設定できます。
            </p>
            <div className="row">
              <input
                placeholder="例: D:\事件 または \\他PC\共有"
                value={folderInput}
                onChange={(e) => setFolderInput(e.target.value)}
              />
              <button type="button" onClick={() => void browseFolder()}>
                参照…
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
                <li className="empty">フォルダがまだ登録されていません</li>
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
                            title="このフォルダの索引を処理中です"
                          >
                            処理中…
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
                                  ? `LAN公開用のUNCパスを表示・編集（設定済: ${publicDraft}）`
                                  : "LAN公開用のUNCパスを表示・編集"
                              }
                              onClick={() =>
                                setPublicPathOpenId(publicOpen ? null : f.id)
                              }
                            >
                              UNC
                              {hasPublicPath && !publicOpen ? " ✓" : ""}
                            </button>
                            <button
                              type="button"
                              disabled={indexing}
                              title="このフォルダだけ索引を読み込み直す"
                              onClick={() => void runReindexFolder(f.id)}
                            >
                              読込
                            </button>
                            <button
                              type="button"
                              className="danger"
                              disabled={indexing}
                              title="このフォルダを検索対象から削除する"
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
                            公開パス（UNC）— LAN 上の別 PC から開く場合に設定
                          </span>
                          <span className="folder-public-row">
                            <input
                              placeholder="例: \\このPC名\共有名（空なら上記パスをそのまま使用）"
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
                              title="UNCパスを保存し、このフォルダだけ再索引する"
                              onClick={() => void savePublicPath(f.id)}
                            >
                              保存
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
              フォルダ追加時はこのフォルダだけ自動で索引されます。UNC
              は必要なときだけ「UNC」から設定してください。
            </p>
          </section>

          <section>
            <h2>除外フォルダ</h2>
            <p className="muted">検索・インデックスから除外するパスを指定します。</p>
            <div className="row">
              <input
                placeholder="除外するフォルダパス"
                value={excludeInput}
                onChange={(e) => setExcludeInput(e.target.value)}
              />
              <button type="button" onClick={() => void browseExcludeFolder()}>
                参照…
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
            <h2>インデックス</h2>
            <p className="muted">
              登録フォルダ内の PDF / DOCX / DOC / JTD / XLS / XLSX / TXT / Markdown / HTML
              を検索用に登録します。既存フォルダのファイル変更は自動監視されるため、通常はフォルダ追加時の自動索引だけで十分です。全フォルダ再構築は、索引の不整合を直すときだけ使ってください。
            </p>
            <button
              type="button"
              className="primary"
              disabled={indexing}
              onClick={() => void runReindex()}
            >
              {indexing ? "処理中…" : "全フォルダを再構築"}
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
              法律用語などの複合語を登録すると、検索時に隣接フレーズとして扱われます（索引の分解は変えないため、部分語でもヒットします）。
              各種設定の品詞フィルタと連携し、登録語内の助詞は除外されません。検索ポップアップの「＋」から挿入・その場登録もできます。
            </p>
            <div className="row">
              <input
                placeholder="例: 弁済による代位 / 損害賠償"
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
                CSV 書き出し
              </button>
            </div>
            <p className="field-hint">
              CSV 形式: 1列（表層）または 表層,品詞,読み。法令から抽出した用語リストの一括登録向けです。
            </p>
            <ul>
              {searchWords.length === 0 ? (
                <li className="empty">検索ワードはまだ登録されていません</li>
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
                            保存
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
                            編集
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
            <h2>各種設定</h2>
            <label>
              <span className="field-label">ショートカットキー</span>
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
              は、OS の入力言語切替（Alt + Shift）と干渉することがあります。変更は再起動後に反映されます。
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
              <span className="field-label">検索ポップアップの初期幅（px）</span>
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
              <span className="field-label">検索ポップアップの初期高さ（px）</span>
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
              <span className="field-label">検索ポップアップの出現位置</span>
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
              ポップアップを開き直すときにサイズ・位置が適用されます。表示中の手動移動・リサイズは、閉じるまで維持されます。
            </p>
            <label>
              <span className="field-label">インデックス更新間隔（秒）</span>
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
              Windows ログオン時に自動起動
            </label>
            <label className="row-check">
              <input
                type="checkbox"
                checked={settings.posFilterEnabled}
                onChange={(e) =>
                  setSettings({ ...settings, posFilterEnabled: e.target.checked })
                }
              />
              助詞・助動詞を検索から除外する（品詞フィルタ）
            </label>
            <p className="field-hint">
              自然文クエリのノイズを減らします。ユーザ辞書や &quot;フレーズ&quot; 内の助詞は除外しません。
            </p>
            <button type="button" className="primary" onClick={() => void saveSettings()}>
              設定を保存
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
            <h2>この PC を検索ホストにする</h2>
            <p className="muted">
              有効にすると、ローカル索引を LAN 上の他の Argos から検索できるようになります（既定ポート
              17890）。Windows ファイアウォールで当該ポートの受信を許可してください。
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
                <span className="field-label">ポート</span>
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
                トークンを再生成
              </button>
            </div>
            <p className="field-hint">
              初回有効化時に UUID 乱数が自動生成されます。任意の文字列への変更や「トークンを再生成」も可能です。トークンは{" "}
              <code>%APPDATA%\Argos\argos.db</code>{" "}
              に保存されます。漏洩時は再生成のうえ、全クライアントのリモートトークンを更新してください。
            </p>
            <p className="field-hint">
              クライアント用 URL の例: <code>{clientUrlHint}</code>
              （トークンもクライアントに同じものを設定）
            </p>
          </section>

          <section>
            <h2>他の Argos を検索する（クライアント）</h2>
            <p className="muted">
              ホスト側でサーバを有効にしたあと、こちらで URL とトークンを設定します。
            </p>
            <div className="options-form">
              <label>
                <span className="field-label">検索モード</span>
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
                <span className="field-label">リモート URL</span>
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
                <span className="field-label">タイムアウト（ms）</span>
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
                {testing ? "テスト中…" : "接続テスト"}
              </button>
            </div>
            <p className="field-hint">
              リモート結果のファイルパスがホストのローカルパス（例: C:\…）の場合、この
              PC からは開けないことがあります。ホスト側でフォルダの「公開パス（UNC）」を設定し、再インデックスしてください。
            </p>
          </section>

          <section>
            <button type="button" className="primary" onClick={() => void saveSettings()}>
              設定を保存
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
            <h2>開発者</h2>
            <p className="credits-org">半蔵門総合法律事務所</p>
            <p className="credits-person">弁護士　吉田秀平</p>
            <p className="muted credits-meta">Argos v{APP_VERSION}</p>
          </section>

          <section className="credits-block">
            <h2>利用条件</h2>
            <p className="muted">
              本ソフトウェア（Argos）は、Apache License 2.0 又はこれと同等の条件に基づき提供されます。
              ソースコード・バイナリの利用・複製・改変・再配布は、当該ライセンスの条件に従って行ってください。
            </p>
            <ul className="credits-terms">
              <li>著作権表示およびライセンス表示を保持すること</li>
              <li>改変した場合はその旨を明示すること</li>
              <li>本ソフトウェアは「現状有姿」で提供され、明示・黙示を問わず保証はありません</li>
              <li>詳細は Apache License, Version 2.0（https://www.apache.org/licenses/LICENSE-2.0）を参照</li>
            </ul>
          </section>

          <section className="credits-block">
            <h2>使用ライブラリ（クレジット）</h2>
            <p className="muted">
              本アプリは次のオープンソースライブラリ等を利用しています。各ライブラリの著作権はそれぞれの権利者に帰属し、記載のライセンス条件に従います。間接依存も含め、完全な一覧ではない場合があります。
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
