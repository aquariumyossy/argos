import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type MouseEvent as ReactMouseEvent,
  type ReactNode,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import "./settings.css";

type SettingsData = {
  shortcut: string;
  notesShortcut: string;
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
  mailEnabled: boolean;
  mailDaysBack: number;
  mailSyncIntervalSecs: number;
  mailLatestOnly: boolean;
  mailThreadCollapse: boolean;
  mailLastSyncAt: string;
  llmBaseUrl: string;
  llmApiKey: string;
  llmModel: string;
  llmTimeoutMs: number;
  llmMaxContextChars: number;
  llmSystemPrompt: string;
  llmThinking: "auto" | "brief" | "off";
  llmThinkingBudget: number;
  llmSearchTopK: number;
  searxngUrl: string;
  searxngTimeoutMs: number;
  llmWebSearchTopK: number;
};

type FolderRow = {
  id: number;
  path: string;
  publicPath: string;
  enabled: boolean;
  indexedCount: number;
  exists?: boolean;
  shareRemote?: boolean;
};
type ExcludePathRow = { id: number; path: string };
type SearchWordRow = {
  id: number;
  word: string;
  reading?: string;
  posLabel?: string;
};
type EmailFolderRow = {
  id: number;
  storeId: string;
  entryId: string;
  name: string;
  pathLabel: string;
  selected: boolean;
  itemCount?: number;
  indexedCount?: number;
};

type TabId = "howto" | "folders" | "mail" | "words" | "options" | "llm" | "remote" | "credits";

type IndexProgressPayload = {
  folderId: number;
  current: number;
  total: number;
  phase: "counting" | "indexing";
};

type MailSyncProgressPayload = {
  phase: string;
  folderLabel: string;
  current: number;
  total: number;
  message: string;
  indexedTotal: number;
  folderIndexed: number;
};

function llmUrlLooksRemote(url: string): boolean {
  const u = url.trim();
  if (!u) return false;
  return !/127\.0\.0\.1|localhost|\[::1\]/i.test(u);
}

function withLlmPreset(currentUrl: string, port: number): string {
  try {
    const raw = currentUrl.trim();
    const u = new URL(raw.includes("://") ? raw : `http://${raw}`);
    u.port = String(port);
    u.pathname = "/v1";
    u.search = "";
    u.hash = "";
    return u.toString().replace(/\/$/, "");
  } catch {
    return `http://127.0.0.1:${port}/v1`;
  }
}

function formatIndexProgress(p: IndexProgressPayload | null): string {
  if (!p) return "処理中…";
  if (p.phase === "counting") return "ファイル数を確認中…";
  return `${p.current.toLocaleString()} / ${p.total.toLocaleString()}`;
}

/** Left-hand-friendly shortcuts that avoid common Windows / IME reserved combos. */
const SHORTCUT_OPTIONS = [
  { value: "Ctrl+Alt+A", label: "Ctrl + Alt + A（推奨・検索）" },
  { value: "Ctrl+Alt+N", label: "Ctrl + Alt + N（推奨・ノート）" },
  { value: "Ctrl+Alt+B", label: "Ctrl + Alt + B" },
  { value: "Ctrl+Alt+Space", label: "Ctrl + Alt + Space" },
  { value: "Alt+Shift+Z", label: "Shift + Alt + Z" },
  { value: "Ctrl+Alt+H", label: "Ctrl + Alt + H" },
  { value: "Ctrl+Alt+J", label: "Ctrl + Alt + J" },
] as const;

const POPUP_POSITION_OPTIONS = [
  { value: "left", label: "左" },
  { value: "center", label: "中央" },
  { value: "right", label: "右" },
] as const;

const SEARCH_MODE_OPTIONS = [
  { value: "local", label: "ローカルのみ" },
  { value: "remote", label: "ホストのみ" },
  { value: "hybrid", label: "ハイブリッド（ローカル＋ホスト）" },
] as const;

const TABS: { id: TabId; label: string }[] = [
  { id: "howto", label: "操作方法" },
  { id: "folders", label: "フォルダ設定" },
  { id: "mail", label: "メール設定" },
  { id: "words", label: "辞書登録" },
  { id: "options", label: "各種設定" },
  { id: "llm", label: "ローカルLLM" },
  { id: "remote", label: "インデックス共有" },
  { id: "credits", label: "クレジット" },
];

const APP_VERSION = "1.11.1";

/** Direct runtime dependencies shown for attribution (not an exhaustive transitive list). */
const THIRD_PARTY_LICENSES: { name: string; license: string; note?: string }[] = [
  { name: "Tauri", license: "Apache-2.0 OR MIT" },
  { name: "@tauri-apps/api / plugins", license: "Apache-2.0 OR MIT" },
  { name: "React / React DOM", license: "MIT" },
  { name: "marked", license: "MIT", note: "Markdown 表示" },
  { name: "mermaid", license: "MIT", note: "チャット回答の図" },
  { name: "Tantivy", license: "MIT" },
  { name: "Lindera / lindera-tantivy", license: "MIT", note: "形態素解析辞書 IPADIC を埋め込み" },
  { name: "IPADIC（Lindera 経由）", license: "IPADIC 独自ライセンス" },
  { name: "rusqlite / SQLite", license: "MIT / Public Domain" },
  { name: "axum / tower-http", license: "MIT" },
  { name: "tokio", license: "MIT" },
  { name: "reqwest", license: "Apache-2.0 OR MIT" },
  { name: "SearXNG", license: "AGPL-3.0", note: "接続先（任意）。アプリには同梱しない" },
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

function sleep(ms: number) {
  return new Promise<void>((resolve) => {
    setTimeout(resolve, ms);
  });
}

function formatInvokeError(e: unknown): string {
  if (e instanceof Error) return e.message;
  if (typeof e === "string") return e;
  if (e && typeof e === "object" && "message" in e) {
    return String((e as { message: unknown }).message);
  }
  try {
    return JSON.stringify(e);
  } catch {
    return String(e);
  }
}

const SIDEBAR_MIN = 160;
const SIDEBAR_MAX = 420;
const SIDEBAR_DEFAULT = 220;
const SIDEBAR_WIDTH_KEY = "argos.settings.sidebarWidth";

function clampSidebarWidth(w: number, containerWidth?: number): number {
  const maxByWindow =
    containerWidth && containerWidth > 0
      ? Math.max(SIDEBAR_MIN, Math.floor(containerWidth * 0.5))
      : SIDEBAR_MAX;
  const max = Math.min(SIDEBAR_MAX, maxByWindow);
  return Math.min(max, Math.max(SIDEBAR_MIN, Math.round(w)));
}

function loadSidebarWidth(): number {
  try {
    const raw = localStorage.getItem(SIDEBAR_WIDTH_KEY);
    if (!raw) return SIDEBAR_DEFAULT;
    const n = Number(raw);
    if (!Number.isFinite(n)) return SIDEBAR_DEFAULT;
    return clampSidebarWidth(n);
  } catch {
    return SIDEBAR_DEFAULT;
  }
}

function NavIcon({ children }: { children: ReactNode }) {
  return (
    <svg
      viewBox="0 0 16 16"
      width="16"
      height="16"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      {children}
    </svg>
  );
}

function IconSearch() {
  return (
    <NavIcon>
      <circle cx="7" cy="7" r="4" />
      <path d="m13 13-2.5-2.5" />
    </NavIcon>
  );
}

function IconChat() {
  return (
    <NavIcon>
      <path d="M3.25 3.5h9.5A1.25 1.25 0 0 1 14 4.75v5.25A1.25 1.25 0 0 1 12.75 11.25H8.1L4.75 13.5v-2.25H3.25A1.25 1.25 0 0 1 2 10V4.75A1.25 1.25 0 0 1 3.25 3.5Z" />
    </NavIcon>
  );
}

function IconNotes() {
  return (
    <NavIcon>
      <path d="M3.5 3.25h9A1.25 1.25 0 0 1 13.75 4.5v7A1.25 1.25 0 0 1 12.5 12.75h-9A1.25 1.25 0 0 1 2.25 11.5v-7A1.25 1.25 0 0 1 3.5 3.25Z" />
      <path d="M5.25 6.25h5.5" />
      <path d="M5.25 8.75h4" />
    </NavIcon>
  );
}

function normalizeSettings(s: SettingsData): SettingsData {
  if (!SHORTCUT_OPTIONS.some((o) => o.value === s.shortcut)) {
    s.shortcut = SHORTCUT_OPTIONS[0].value;
  }
  if (!s.notesShortcut) s.notesShortcut = "Ctrl+Alt+N";
  if (!SHORTCUT_OPTIONS.some((o) => o.value === s.notesShortcut)) {
    s.notesShortcut = "Ctrl+Alt+N";
  }
  if (s.notesShortcut === s.shortcut) {
    s.notesShortcut =
      SHORTCUT_OPTIONS.find((o) => o.value !== s.shortcut)?.value ??
      "Ctrl+Alt+N";
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
  if (typeof s.mailEnabled !== "boolean") s.mailEnabled = false;
  if (typeof s.mailDaysBack !== "number") s.mailDaysBack = 730;
  if (typeof s.mailSyncIntervalSecs !== "number") s.mailSyncIntervalSecs = 3600;
  if (typeof s.mailLatestOnly !== "boolean") s.mailLatestOnly = false;
  if (typeof s.mailThreadCollapse !== "boolean") s.mailThreadCollapse = true;
  if (typeof s.mailLastSyncAt !== "string") s.mailLastSyncAt = "";
  if (typeof s.llmBaseUrl !== "string" || !s.llmBaseUrl.trim()) {
    s.llmBaseUrl = "http://127.0.0.1:11434/v1";
  }
  if (typeof s.llmApiKey !== "string") s.llmApiKey = "";
  if (typeof s.llmModel !== "string") s.llmModel = "";
  if (typeof s.llmTimeoutMs !== "number" || !Number.isFinite(s.llmTimeoutMs)) {
    s.llmTimeoutMs = 120000;
  }
  if (typeof s.llmMaxContextChars !== "number" || !Number.isFinite(s.llmMaxContextChars)) {
    s.llmMaxContextChars = 80000;
  }
  if (typeof s.llmSystemPrompt !== "string" || !s.llmSystemPrompt.trim()) {
    s.llmSystemPrompt =
      "あなたは法律事務所の調査補助です。日本語で簡潔に答えてください。出典ブロックがあるときはその本文だけを根拠にし、根拠箇所には [n] を付けてください。根拠がないことは推測だと明示し、分からないことは分からないと言ってください。インデックスを検索するツールがあります。添付出典で足りるときは検索しないでください。検索したら結果を [n] で引用してください。\n回答はMarkdownで書いてください。見出し・箇条書き・表を使ってよいです。生のHTMLは書かないでください。ユーザーに選ばせるときは ```choices フェンスに選択肢を1行ずつ書いてください。";
  }
  if (s.llmThinking !== "auto" && s.llmThinking !== "brief" && s.llmThinking !== "off") {
    s.llmThinking = "brief";
  }
  if (typeof s.llmThinkingBudget !== "number" || !Number.isFinite(s.llmThinkingBudget)) {
    s.llmThinkingBudget = 2048;
  }
  if (typeof s.llmSearchTopK !== "number" || !Number.isFinite(s.llmSearchTopK)) {
    s.llmSearchTopK = 4;
  } else {
    s.llmSearchTopK = Math.min(16, Math.max(1, Math.round(s.llmSearchTopK)));
  }
  if (typeof s.searxngUrl !== "string") s.searxngUrl = "";
  if (typeof s.searxngTimeoutMs !== "number" || !Number.isFinite(s.searxngTimeoutMs)) {
    s.searxngTimeoutMs = 8000;
  } else {
    s.searxngTimeoutMs = Math.min(30000, Math.max(5000, Math.round(s.searxngTimeoutMs)));
  }
  if (typeof s.llmWebSearchTopK !== "number" || !Number.isFinite(s.llmWebSearchTopK)) {
    s.llmWebSearchTopK = 5;
  } else {
    s.llmWebSearchTopK = Math.min(8, Math.max(1, Math.round(s.llmWebSearchTopK)));
  }
  return s;
}

/** Wait for AppState to be managed; startup can race the hidden main WebView. */
async function invokeGetSettingsWithRetry(
  maxAttempts = 50,
): Promise<SettingsData> {
  let lastError: unknown;
  for (let attempt = 0; attempt < maxAttempts; attempt++) {
    if (attempt > 0) {
      await sleep(Math.min(50 * 2 ** Math.min(attempt - 1, 4), 1000));
    }
    try {
      return await invoke<SettingsData>("get_settings");
    } catch (e) {
      lastError = e;
    }
  }
  throw lastError instanceof Error
    ? lastError
    : new Error(formatInvokeError(lastError));
}

export default function Settings() {
  const [settings, setSettings] = useState<SettingsData | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
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
  const [folderMenuId, setFolderMenuId] = useState<number | null>(null);
  const [folderMenuStyle, setFolderMenuStyle] = useState<CSSProperties>({});
  const folderMenuBtnRef = useRef<HTMLButtonElement | null>(null);
  const folderMenuRef = useRef<HTMLDivElement | null>(null);
  const [message, setMessage] = useState("");
  const [indexing, setIndexing] = useState(false);
  const [busyFolderId, setBusyFolderId] = useState<number | null>(null);
  const [indexProgress, setIndexProgress] = useState<IndexProgressPayload | null>(
    null,
  );
  const [tab, setTab] = useState<TabId>("howto");
  const [sidebarWidth, setSidebarWidth] = useState(loadSidebarWidth);
  const [resizingSidebar, setResizingSidebar] = useState(false);
  const rootRef = useRef<HTMLDivElement | null>(null);
  const [lanIp, setLanIp] = useState<string | null>(null);
  const [testing, setTesting] = useState(false);
  const [testingLlm, setTestingLlm] = useState(false);
  const [testingSearxng, setTestingSearxng] = useState(false);
  const [llmModels, setLlmModels] = useState<string[]>([]);
  const [mailFolders, setMailFolders] = useState<EmailFolderRow[]>([]);
  const [mailDetect, setMailDetect] = useState<string>("");
  const [mailBusy, setMailBusy] = useState(false);
  const [mailProgress, setMailProgress] = useState<MailSyncProgressPayload | null>(
    null,
  );
  const [mailIndexedCount, setMailIndexedCount] = useState(0);
  const settingsRef = useRef<SettingsData | null>(null);
  const bootstrappingRef = useRef(false);

  useEffect(() => {
    settingsRef.current = settings;
  }, [settings]);

  useEffect(() => {
    if (folderMenuId == null) return;
    const onDoc = (e: MouseEvent) => {
      const t = e.target;
      if (!(t instanceof Node)) return;
      if (folderMenuRef.current?.contains(t)) return;
      if (folderMenuBtnRef.current?.contains(t)) return;
      setFolderMenuId(null);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setFolderMenuId(null);
    };
    document.addEventListener("mousedown", onDoc);
    window.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDoc);
      window.removeEventListener("keydown", onKey);
    };
  }, [folderMenuId]);

  useEffect(() => {
    if (publicPathOpenId == null) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setPublicPathOpenId(null);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [publicPathOpenId]);

  useLayoutEffect(() => {
    if (folderMenuId == null || !folderMenuBtnRef.current) return;
    const rect = folderMenuBtnRef.current.getBoundingClientRect();
    const menuW = folderMenuRef.current?.offsetWidth || 168;
    const menuH = folderMenuRef.current?.offsetHeight || 140;
    const margin = 8;
    let left = rect.right - menuW;
    if (left < margin) left = margin;
    if (left + menuW > window.innerWidth - margin) {
      left = Math.max(margin, window.innerWidth - margin - menuW);
    }
    const dropUp = rect.bottom + menuH + 6 > window.innerHeight && rect.top > menuH + 6;
    setFolderMenuStyle({
      position: "fixed",
      left,
      top: dropUp ? undefined : rect.bottom + 6,
      bottom: dropUp ? window.innerHeight - rect.top + 6 : undefined,
    });
  }, [folderMenuId]);

  useEffect(() => {
    setFolderMenuId(null);
  }, [tab]);

  async function reload(opts?: { retry?: boolean }) {
    const raw = opts?.retry
      ? await invokeGetSettingsWithRetry()
      : await invoke<SettingsData>("get_settings");
    const s = normalizeSettings(raw);
    setSettings(s);
    settingsRef.current = s;
    setLoadError(null);
    const nextFolders = await invoke<FolderRow[]>("list_folders");
    setFolders(nextFolders);
    setPublicPathDrafts(
      Object.fromEntries(nextFolders.map((f) => [f.id, f.publicPath ?? ""])),
    );
    setExcludes(await invoke<ExcludePathRow[]>("list_exclude_paths"));
    setSearchWords(await invoke<SearchWordRow[]>("list_search_words"));
    try {
      setMailFolders(await invoke<EmailFolderRow[]>("mail_list_folders"));
    } catch {
      setMailFolders([]);
    }
    try {
      setMailIndexedCount(await invoke<number>("mail_indexed_count"));
    } catch {
      setMailIndexedCount(0);
    }
    try {
      const ip = await invoke<string | null>("get_lan_ip_hint");
      setLanIp(ip);
    } catch {
      setLanIp(null);
    }
  }

  async function bootstrapSettings() {
    if (bootstrappingRef.current || settingsRef.current) return;
    bootstrappingRef.current = true;
    setLoadError(null);
    try {
      await reload({ retry: true });
    } catch (e) {
      console.error(e);
      if (!settingsRef.current) {
        setLoadError(formatInvokeError(e));
      }
    } finally {
      bootstrappingRef.current = false;
    }
  }

  useEffect(() => {
    void bootstrapSettings();
  }, []);

  // Window is hide/show (not remounted). Retry if the startup race left settings null.
  useEffect(() => {
    const win = getCurrentWindow();
    let cancelled = false;
    let unlistenFocus: (() => void) | undefined;
    let unlistenReady: (() => void) | undefined;
    void win
      .onFocusChanged((event) => {
        if (event.payload && !settingsRef.current) {
          void bootstrapSettings();
        }
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlistenFocus = fn;
      });
    void listen("argos-ready", () => {
      if (!settingsRef.current) {
        void bootstrapSettings();
      }
    }).then((fn) => {
      if (cancelled) fn();
      else unlistenReady = fn;
    });
    return () => {
      cancelled = true;
      unlistenFocus?.();
      unlistenReady?.();
    };
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

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    listen<MailSyncProgressPayload>("mail-sync-progress", (event) => {
      const p = event.payload;
      setMailProgress(p);
      if (typeof p.indexedTotal === "number") {
        setMailIndexedCount(p.indexedTotal);
      }
      if (p.phase === "indexing" && p.folderLabel) {
        setMailFolders((prev) =>
          prev.map((f) =>
            f.pathLabel === p.folderLabel || f.name === p.folderLabel
              ? { ...f, indexedCount: p.folderIndexed }
              : f,
          ),
        );
      }
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    if (tab !== "mail") return;
    void invoke<EmailFolderRow[]>("mail_list_folders")
      .then(setMailFolders)
      .catch(console.error);
    void invoke<number>("mail_indexed_count")
      .then(setMailIndexedCount)
      .catch(console.error);
  }, [tab]);

  // Settings window stays mounted; refresh when opening this tab.
  useEffect(() => {
    if (tab !== "words") return;
    void invoke<SearchWordRow[]>("list_search_words")
      .then(setSearchWords)
      .catch(console.error);
  }, [tab]);

  async function detectOutlook() {
    try {
      const running = await invoke<boolean>("mail_outlook_running");
      if (!running) {
        setMessage("Outlook を起動して接続します…");
      }
      const v = await invoke<string>("mail_detect_outlook");
      setMailDetect(v);
      setMessage(`検出: ${v}`);
    } catch (err) {
      setMailDetect("");
      setMessage(`Outlook 検出失敗: ${String(err)}`);
    }
  }

  async function refreshMailFolders() {
    setMailBusy(true);
    try {
      const running = await invoke<boolean>("mail_outlook_running");
      if (!running) {
        setMessage("Outlook を起動してフォルダ一覧を取得します…");
      }
      const rows = await invoke<EmailFolderRow[]>("mail_refresh_folder_catalog");
      setMailFolders(rows);
      setMessage(`Outlook フォルダ ${rows.length} 件を取得しました`);
    } catch (err) {
      setMessage(`フォルダ取得失敗: ${String(err)}`);
    } finally {
      setMailBusy(false);
    }
  }

  async function toggleMailFolder(folder: EmailFolderRow, selected: boolean) {
    const next = mailFolders.map((f) =>
      f.storeId === folder.storeId && f.entryId === folder.entryId
        ? { ...f, selected }
        : f,
    );
    setMailFolders(next);
    const keys = next
      .filter((f) => f.selected)
      .map((f) => ({ storeId: f.storeId, entryId: f.entryId }));
    try {
      await invoke("mail_set_selected_folders", { folders: keys });
    } catch (err) {
      setMessage(`選択の保存に失敗: ${String(err)}`);
      await reload();
    }
  }

  async function runMailSync() {
    if (!settings) return;
    setMailBusy(true);
    setMailProgress(null);
    try {
      if (!settings.mailEnabled) {
        const saved = await invoke<SettingsData>("update_settings", {
          settings: { ...settings, mailEnabled: true },
        });
        setSettings(saved);
      }
      const running = await invoke<boolean>("mail_outlook_running");
      if (!running) {
        const launchMsg = "Outlook を起動して同期します…";
        setMessage(launchMsg);
        // Sync button shows mailProgress.message while busy — set it before the long wait.
        setMailProgress({
          phase: "starting",
          folderLabel: "",
          current: 0,
          total: 0,
          message: launchMsg,
          indexedTotal: mailIndexedCount,
          folderIndexed: 0,
        });
        // Let React paint the notice before the blocking sync invoke.
        await new Promise<void>((resolve) => {
          requestAnimationFrame(() => requestAnimationFrame(() => resolve()));
        });
      }
      const stats = await invoke<{
        indexed: number;
        skipped: number;
        superseded: number;
        errors: number;
        folders: number;
      }>("mail_run_sync");
      setMessage(
        `メール同期完了: インデックス登録 ${stats.indexed} / スキップ ${stats.skipped} / 統合除外 ${stats.superseded} / エラー ${stats.errors}`,
      );
      setMailIndexedCount(await invoke<number>("mail_indexed_count"));
      await reload();
    } catch (err) {
      setMessage(`メール同期失敗: ${String(err)}`);
    } finally {
      setMailBusy(false);
      setMailProgress(null);
    }
  }

  async function saveSettings() {
    if (!settings) return;
    const saved = await invoke<SettingsData>("update_settings", { settings });
    setSettings(saved);
    setMessage("設定を保存しました（ショートカット変更はすぐに反映されます）");
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
        `フォルダを追加しました（このフォルダのみをインデックス: 登録 ${stats.indexed} / スキップ ${stats.skipped} / エラー ${stats.errors}）。以降の変更は自動監視されます。`,
      );
      await reload();
    } catch (e) {
      setMessage(`失敗: ${String(e)}`);
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
    setIndexProgress(null);
    try {
      await invoke("update_folder_public_path", { id, publicPath });
      const stats = await invoke<{
        indexed: number;
        skipped: number;
        errors: number;
      }>("run_reindex_folder", { id });
      setMessage(
        `公開パスを更新しました（このフォルダのみ再インデックス: 登録 ${stats.indexed} / スキップ ${stats.skipped} / エラー ${stats.errors}）`,
      );
      await reload();
    } catch (e) {
      setMessage(`失敗: ${String(e)}`);
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

  async function rebindFolder(id: number) {
    const selected = await openDialog({
      directory: true,
      multiple: false,
      title: "変更後のフォルダ場所を選択",
    });
    if (typeof selected !== "string" || !selected) return;
    setIndexing(true);
    setBusyFolderId(id);
    setIndexProgress(null);
    try {
      await invoke<FolderRow>("update_folder_path", { id, path: selected });
      setMessage(
        "フォルダの場所を更新しました（既存インデックスを紐づけ直しました。本文の再読み込みはしていません）。",
      );
      await reload();
    } catch (e) {
      setMessage(`失敗: ${String(e)}`);
      await reload().catch(() => undefined);
    } finally {
      setBusyFolderId(null);
      setIndexProgress(null);
      setIndexing(false);
    }
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
        `完了（このフォルダのみ）: 登録 ${stats.indexed} / スキップ ${stats.skipped} / エラー ${stats.errors}`,
      );
      await reload();
    } catch (e) {
      setMessage(`失敗: ${String(e)}`);
    } finally {
      setBusyFolderId(null);
      setIndexProgress(null);
      setIndexing(false);
    }
  }

  async function setFolderShareRemote(id: number, shareRemote: boolean) {
    try {
      const row = await invoke<FolderRow>("set_folder_share_remote", {
        id,
        shareRemote,
      });
      setFolders((prev) =>
        prev.map((f) => (f.id === row.id ? { ...f, ...row } : f)),
      );
    } catch (e) {
      setMessage(`失敗: ${String(e)}`);
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

  async function clearAllSearchWords() {
    if (searchWords.length === 0) return;
    const ok = window.confirm(
      `登録済みの検索ワード ${searchWords.length} 件をすべて削除しますか？\nこの操作は取り消せません。`,
    );
    if (!ok) return;
    try {
      const n = await invoke<number>("clear_search_words");
      setEditingWordId(null);
      setEditingWordDraft("");
      setMessage(`検索ワードをすべて削除しました（${n} 件）`);
      await reload();
    } catch (e) {
      setMessage(`一括削除失敗: ${String(e)}`);
    }
  }

  async function clearSearchHistory() {
    const ok = window.confirm(
      "検索履歴をすべて削除しますか？\nこの操作は取り消せません。",
    );
    if (!ok) return;
    try {
      await invoke("clear_search_term_history");
      setMessage("検索履歴を削除しました");
    } catch (e) {
      setMessage(`履歴の削除に失敗: ${String(e)}`);
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
    setIndexProgress(null);
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
      setBusyFolderId(null);
      setIndexProgress(null);
      setIndexing(false);
    }
  }

  async function testLlm() {
    if (!settings) return;
    setTestingLlm(true);
    setMessage("LLM 接続テスト中…");
    try {
      const saved = await invoke<SettingsData>("update_settings", { settings });
      setSettings(saved);
      const result = await invoke<{
        message: string;
        loopback: boolean;
        models: { id: string }[];
      }>("llm_test_connection");
      const ids = result.models.map((m) => m.id);
      setLlmModels(ids);
      if (!saved.llmModel.trim() && ids.length > 0) {
        const next = { ...saved, llmModel: ids[0] };
        setSettings(next);
        await invoke("update_settings", { settings: next });
      }
      setMessage(result.message);
    } catch (e) {
      setMessage(`LLM 接続テスト失敗: ${formatInvokeError(e)}`);
    } finally {
      setTestingLlm(false);
    }
  }

  async function testSearxng() {
    if (!settings) return;
    setTestingSearxng(true);
    setMessage("SearXNG 接続テスト中…");
    try {
      const saved = await invoke<SettingsData>("update_settings", { settings });
      setSettings(saved);
      const msg = await invoke<string>("test_searxng_connection");
      setMessage(msg);
    } catch (e) {
      setMessage(`SearXNG 接続テスト失敗: ${formatInvokeError(e)}`);
    } finally {
      setTestingSearxng(false);
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

  useEffect(() => {
    try {
      localStorage.setItem(SIDEBAR_WIDTH_KEY, String(sidebarWidth));
    } catch {
      /* ignore */
    }
  }, [sidebarWidth]);

  const onSidebarResizeStart = useCallback((e: ReactMouseEvent) => {
    if (e.button !== 0) return;
    e.preventDefault();
    setResizingSidebar(true);
    const startX = e.clientX;
    const startW = sidebarWidth;
    const containerW = rootRef.current?.clientWidth ?? 0;

    const onMove = (ev: MouseEvent) => {
      const next = clampSidebarWidth(startW + (ev.clientX - startX), containerW);
      setSidebarWidth(next);
    };
    const onUp = () => {
      setResizingSidebar(false);
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  }, [sidebarWidth]);

  async function openWindow(cmd: string) {
    try {
      await invoke(cmd);
    } catch (e) {
      setMessage(formatInvokeError(e));
    }
  }

  const clientUrlHint =
    lanIp && settings
      ? `http://${lanIp}:${settings.remoteServerPort}`
      : settings
        ? `http://<このPCのLAN IP>:${settings.remoteServerPort}`
        : "";

  if (!settings) {
    return (
      <div className="settings loading">
        {loadError ? (
          <div className="loading-error">
            <p>設定の読み込みに失敗しました。</p>
            <p className="msg">{loadError}</p>
            <button
              type="button"
              className="primary"
              onClick={() => {
                void bootstrapSettings();
              }}
            >
              再試行
            </button>
          </div>
        ) : (
          "読み込み中…"
        )}
      </div>
    );
  }

  return (
    <div
      ref={rootRef}
      className={["settings", resizingSidebar ? "settings--resizing" : ""]
        .filter(Boolean)
        .join(" ")}
      style={{
        gridTemplateColumns: `${sidebarWidth}px 5px minmax(0, 1fr)`,
      }}
    >
      <aside className="settings-sidebar">
        <header className="settings-sidebar-head">
          <h1>設定</h1>
        </header>
        <nav className="settings-sidebar-body" aria-label="設定グループ">
          <ul className="settings-nav-list">
            {TABS.map((t) => (
              <li key={t.id}>
                <button
                  type="button"
                  id={`tab-${t.id}`}
                  className={
                    tab === t.id ? "settings-nav-item active" : "settings-nav-item"
                  }
                  aria-current={tab === t.id ? "page" : undefined}
                  onClick={() => {
                    setTab(t.id);
                    setMessage("");
                    setEditingWordId(null);
                    setEditingWordDraft("");
                  }}
                >
                  {t.label}
                </button>
              </li>
            ))}
          </ul>
        </nav>
        <footer className="settings-sidebar-foot">
          <button
            type="button"
            className="settings-nav-link"
            title="検索"
            aria-label="検索"
            onClick={() => void openWindow("show_popup_window")}
          >
            <IconSearch />
          </button>
          <button
            type="button"
            className="settings-nav-link"
            title="チャット"
            aria-label="チャット"
            onClick={() => void openWindow("show_chat_window")}
          >
            <IconChat />
          </button>
          <button
            type="button"
            className="settings-nav-link"
            title="ノート"
            aria-label="ノート"
            onClick={() => void openWindow("show_notes_window")}
          >
            <IconNotes />
          </button>
        </footer>
      </aside>

      <div
        className="settings-splitter"
        role="separator"
        aria-orientation="vertical"
        aria-label="設定ナビの幅を変更"
        aria-valuemin={SIDEBAR_MIN}
        aria-valuemax={SIDEBAR_MAX}
        aria-valuenow={sidebarWidth}
        onMouseDown={onSidebarResizeStart}
      />

      <main className="settings-main">
      {tab === "howto" ? (
        <div
          className="tab-panel"
          role="region"
          id="panel-howto"
          aria-labelledby="tab-howto"
        >
          <section>
            <h2>Argos とは</h2>
            <p className="muted">
              任意のアプリで文字列を選択しショートカットを押すと、登録フォルダ内の PDF / DOCX / DOC /
              JTD / XLS / XLSX / TXT / Markdown / HTML / JSON、および Outlook
              クラシックのメールから全文検索し、結果をポップアップで表示します。ヒットはノートにキープでき、出典としてローカル
              LLM チャットにも送れます。
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
              <li>
                ヒットをノートに残し、出典としてチャットで質問できます
              </li>
            </ul>
          </section>

          <section>
            <h2>はじめに</h2>
            <ol className="howto-steps">
              <li>
                左の「フォルダ設定」で検索したいフォルダを追加します
              </li>
              <li>「今すぐインデックス」を実行して全文検索用のインデックスを作成します</li>
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
                <span>プレビューを表示（本文をスクロール）</span>
              </li>
              <li>
                <kbd>←</kbd> / <kbd>→</kbd>
                <span>プレビュー中、次／前のマッチへスクロール</span>
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
            <p className="muted">
              検索欄の語をドラッグすると「隣接にする」「辞書に登録」が出せます。入力欄が空のときは最近の検索語が候補になります。種別・フォルダ・時期（1週間／1か月など、または開始・終了日）で絞れます。ヒットの「キープ」でノートへ、「チャット」で会話へ送れます。
            </p>
          </section>

          <section>
            <h2>メール検索</h2>
            <p className="muted">
              同一 PC の Outlook クラシックのメールを、ファイルと同じポップアップから検索できます（新しい
              Outlook のみの環境では使えません。LAN リモート検索にはメールは公開されません）。
            </p>
            <ol className="howto-steps">
              <li>左の「メール設定」で Outlook を検出して設定を保存します</li>
              <li>フォルダを選んで同期します</li>
              <li>
                通常どおり <kbd>{settings.shortcut}</kbd> で検索します。時期ボタンで受信日の範囲を指定できます
              </li>
            </ol>
          </section>

          <section>
            <h2>ノート</h2>
            <p className="muted">
              検索ヒットの段落やメールをノートにキープし、案件の作業文書としてメモ（Markdown）にまとめます。ToDo と日程はメモ内のチェックと <code>@YYYY-MM-DD</code> から集約します。
            </p>
            <ol className="howto-steps">
              <li>ポップアップまたはプレビューの「キープ」から送り先のノートを選びます</li>
              <li>
                トレイまたは <kbd>{settings.notesShortcut}</kbd>{" "}
                でノートを開きます。メインはメモとキープの2ペインで、上下／左右を切り替えられます
              </li>
              <li>
                メモは表示（Markdown）と編集を切り替えます。「記法」から見出し・リスト・ToDo・今日の日付を入れられます。チェックを付けると本文の{" "}
                <code>- [ ]</code> が変わります。左の「未完了ToDo」で全ノートの宿題を一覧できます
              </li>
              <li>
                「チャット」から出典として送れます（長いメモは要約）。送った会話はそのノートを対象にします。回答の「ノートへ」でメモ末尾へすぐ追記できます。モデルの変更は提案カードの採用後に反映されます
              </li>
            </ol>
          </section>

          <section>
            <h2>インデックスの共有（LAN）</h2>
            <p className="muted">
              同じ LAN 上の別 PC から、この PC のインデックスを検索できます。ファイル自体をコピーする必要はありません。
            </p>
            <h3 className="howto-subhead">インデックスがある PC（ホスト）</h3>
            <ol className="howto-steps">
              <li>左の「フォルダ設定」でフォルダを追加し、必要なフォルダの「LAN共有」をオンにします（未選択ならサーバをオンにしてもファイルは出ません。入れ子なら内側のトグルが勝ちます）</li>
              <li>
                クライアントからもファイルを開けるようにする場合は、フォルダの「公開パス（UNC）」に共有パス（例:{" "}
                <code>\\192.168.0.8\共有名</code>
                ）を設定してから再インデックスします。すでに UNC で登録している場合は空のままで構いません
              </li>
              <li>
                左の「インデックス共有」で「このPCのインデックスを共有する」をオンにします
              </li>
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
                左の「インデックス共有」で検索モードを「ホストのみ」または「ハイブリッド」にします
              </li>
              <li>
                ホスト URL（例: <code>http://192.168.0.8:17890</code>
                ）と接続トークンを入力します
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
                などローカルパスだけをインデックスしていると、クライアントではプレビューはできてもファイルを開けないことがあります
              </li>
              <li>詳細な項目は左の「インデックス共有」でも設定・確認できます</li>
            </ul>
          </section>

          <section>
            <h2>ローカルLLM</h2>
            <p className="muted">
              トレイの「チャットを開く」から、OpenAI 互換のローカルサーバ（MTPLX / Ollama / LM Studio / llama.cpp など）と会話できます。本文は、あなたが送ったメッセージと添付した出典がサーバに渡ります。モデルが対応していれば、会話中にインデックスを検索することもあります（「Aさんの直近メール」のように時期や送信者を指定できます）。回答は Markdown で表示し、図や表（列幅はドラッグで調整）、選択肢も出せます。
            </p>
            <ol className="howto-steps">
              <li>MTPLX 等を起動し、使いたいモデルを読み込みます</li>
              <li>
                ベース URL は <code>/v1</code> まで含めます。例:{" "}
                <code>http://127.0.0.1:8000/v1</code>
                。ホストとポートだけ入力した場合は保存時に自動で付きます
              </li>
              <li>
                別の PC から Tailscale / LAN で使うときは、Mac 側が{" "}
                <code>127.0.0.1</code> だけではなく外向きに待っている必要があります（
                <code>mtplx serve --host 0.0.0.0 --port 8000 --api-key 任意</code>
                、または <code>tailscale serve 8000</code>）
              </li>
              <li>
                サーバ側のコンテキスト長（Ollama なら <code>num_ctx</code>）を{" "}
                <strong>64k 以上</strong>にしてください。モデル最大が 262k
                でも、サーバ既定が 4k のままだと長い会話は失敗します
              </li>
              <li>
                左の「ローカルLLM」で URL を入れ、「接続テスト」するとモデル一覧を取得します
              </li>
              <li>トレイから「チャットを開く」。左下のアイコンから検索・ノート・設定へ移れます</li>
              <li>
                検索ヒットやノートの「チャット」から、送り先の会話を選べます。ノートから送るとそのノートが対象になり、回答は「ノートへ」でメモに戻せます。モデルが出した見出しの追記・置換は、チャット上の提案カードで採用または却下します。
              </li>
              <li>
                根拠には出典番号 <code>[n]</code>{" "}
                が付きます。図は入力上の「要件事実」など。Qwen の思考が長いときは左の「ローカルLLM」の思考を「短くする」か「オフ」にしてください
              </li>
              <li>
                入力欄上の「ウェブ検索オフ／オン」でウェブ検索を足せます。オンのとき、モデルがインデックスを検索すると同じ語で
                SearXNG も検索します（スニペット）。本文が必要なヒットはモデルが read_url します。メッセージに貼った
                http(s) の URL は、トグルがオフでも送信時に本文を取ります（最大3件）。JavaScript
                だけで描画されるページやログインが必要なページは読めないことがあります。検索語は接続先の検索エンジンへ送られるので、依頼者名などはオンにしないでください。左の「ローカルLLM」で
                SearXNG の URL を入れてからウェブ検索を使います
              </li>
            </ol>
          </section>

          <section>
            <h2>ヒント</h2>
            <ul className="howto-tips">
              <li>登録フォルダ内のファイル変更は自動で監視され、インデックスに反映されます</li>
              <li>ショートカットキーの変更は保存後すぐに反映されます</li>
              <li>
                チャット・ノート・設定の左下アイコンで、検索と各窓を行き来できます
              </li>
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
          role="region"
          id="panel-folders"
          aria-labelledby="tab-folders"
        >
          <section>
            <h2>検索対象フォルダ</h2>
            <p className="muted">
              ここに追加したフォルダ内の文書がこの PC
              の検索対象になります。
              {settings.remoteServerEnabled
                ? " LAN 上の他の Argos に出すフォルダだけ「共有」にしてください（再インデックスは不要）。入れ子の登録フォルダがあるときは、内側の切替が勝ちます。"
                : ""}
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
                  const isMissing = f.exists === false;
                  const shareHint = f.shareRemote
                    ? "このフォルダは共有中です"
                    : "このフォルダは共有オフです。";
                  return (
                    <li
                      key={f.id}
                      className={
                        [
                          "folder-item",
                          isBusy ? "is-busy" : "",
                          isMissing ? "is-missing" : "",
                        ]
                          .filter(Boolean)
                          .join(" ")
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
                            title="このフォルダのインデックスを処理中です"
                          >
                            {indexProgress &&
                            indexProgress.folderId === f.id
                              ? formatIndexProgress(indexProgress)
                              : "処理中…"}
                          </span>
                        ) : (
                          <span className="folder-actions">
                            {settings.remoteServerEnabled ? (
                            <div
                              className="folder-share-toggle"
                              role="group"
                              aria-label="LAN共有"
                            >
                              <button
                                type="button"
                                className={f.shareRemote ? "active" : ""}
                                disabled={indexing}
                                aria-pressed={!!f.shareRemote}
                                title={shareHint}
                                onClick={() => {
                                  if (!f.shareRemote) {
                                    void setFolderShareRemote(f.id, true);
                                  }
                                }}
                              >
                                共有
                              </button>
                              <button
                                type="button"
                                className={f.shareRemote ? "" : "active"}
                                disabled={indexing}
                                aria-pressed={!f.shareRemote}
                                title={shareHint}
                                onClick={() => {
                                  if (f.shareRemote) {
                                    void setFolderShareRemote(f.id, false);
                                  }
                                }}
                              >
                                オフ
                              </button>
                            </div>
                            ) : null}
                            {isMissing ? (
                              <button
                                type="button"
                                className="folder-rebind"
                                disabled={indexing}
                                title="リネーム／移動後の新しい場所を指定して既存インデックスを紐づけ直す"
                                onClick={() => void rebindFolder(f.id)}
                              >
                                場所を指定
                              </button>
                            ) : null}
                            <button
                              type="button"
                              disabled={indexing}
                              title="このフォルダを読み込み直す（未変更はスキップ・中断後も続きから可能）"
                              onClick={() => void runReindexFolder(f.id)}
                            >
                              読込
                            </button>
                            <button
                              type="button"
                              className={
                                folderMenuId === f.id
                                  ? "folder-more is-open"
                                  : "folder-more"
                              }
                              aria-label="その他の操作"
                              aria-haspopup="menu"
                              aria-expanded={folderMenuId === f.id}
                              disabled={indexing}
                              ref={
                                folderMenuId === f.id
                                  ? folderMenuBtnRef
                                  : undefined
                              }
                              onClick={() =>
                                setFolderMenuId((cur) =>
                                  cur === f.id ? null : f.id,
                                )
                              }
                            >
                              ⋯
                            </button>
                            {folderMenuId === f.id ? (
                              <div
                                ref={folderMenuRef}
                                className="folder-more-menu"
                                role="menu"
                                style={folderMenuStyle}
                              >
                                {isMissing ? null : (
                                  <button
                                    type="button"
                                    role="menuitem"
                                    disabled={indexing}
                                    title="エクスプローラーなどでフォルダ名を変更・移動したときに使います。新しい場所を選び、既存の検索インデックスを紐づけ直します（本文の再読み込みはしません）"
                                    onClick={() => {
                                      setFolderMenuId(null);
                                      void rebindFolder(f.id);
                                    }}
                                  >
                                    パス変更
                                  </button>
                                )}
                                <button
                                  type="button"
                                  role="menuitem"
                                  className={
                                    hasPublicPath ? "is-active" : undefined
                                  }
                                  aria-expanded={publicOpen}
                                  title={
                                    hasPublicPath
                                      ? `LAN公開用のUNCパスを表示・編集（設定済: ${publicDraft}）`
                                      : "LAN公開用のUNCパスを表示・編集"
                                  }
                                  onClick={() => {
                                    setFolderMenuId(null);
                                    setPublicPathOpenId(
                                      publicOpen ? null : f.id,
                                    );
                                  }}
                                >
                                  UNC{hasPublicPath ? " ✓" : ""}
                                </button>
                                <button
                                  type="button"
                                  role="menuitem"
                                  className="danger"
                                  disabled={indexing}
                                  title="このフォルダを検索対象から削除する"
                                  onClick={() => {
                                    setFolderMenuId(null);
                                    void removeFolder(f.id);
                                  }}
                                >
                                  削除
                                </button>
                              </div>
                            ) : null}
                          </span>
                        )}
                      </div>
                      {isMissing ? (
                        <p className="folder-missing-hint">
                          場所が見つかりません。フォルダをリネーム／移動した場合は「場所を指定」で新しいパスを選んでください。
                        </p>
                      ) : null}
                      {publicOpen && !isBusy ? (
                        <div className="folder-public">
                          <div className="folder-public-head">
                            <span className="field-label">
                              公開パス（UNC）— LAN 上の別 PC から開く場合に設定
                            </span>
                            <button
                              type="button"
                              className="folder-public-close"
                              title="閉じる"
                              aria-label="閉じる"
                              onClick={() => setPublicPathOpenId(null)}
                            >
                              ×
                            </button>
                          </div>
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
                              onKeyDown={(e) => {
                                if (e.key === "Escape") {
                                  e.preventDefault();
                                  setPublicPathOpenId(null);
                                }
                              }}
                            />
                            <button
                              type="button"
                              disabled={indexing}
                              title="UNCパスを保存し、このフォルダだけ再インデックスする"
                              onClick={() => void savePublicPath(f.id)}
                            >
                              保存
                            </button>
                          </span>
                        </div>
                      ) : null}
                    </li>
                  );
                })
              )}
            </ul>
            <p className="field-hint">
              フォルダを追加すると、そのフォルダだけが自動でインデックスされます。ほかの登録フォルダは対象外です。
              {settings.remoteServerEnabled
                ? " 追加したフォルダの共有はオフです。"
                : ""}{" "}
              LAN 上の別 PC からファイルを開きたいときだけ、各フォルダ右端の ⋯
              →「UNC」で公開パス（例: \\PC名\共有名）を設定してください。この PC
              だけで使うなら不要です。
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
              登録フォルダ内の PDF / DOCX / DOC / JTD / XLS / XLSX / TXT / Markdown / HTML / JSON
              を検索用に登録します。既存フォルダのファイル変更は自動監視されるため、通常はフォルダ追加時の自動インデックスだけで十分です。全フォルダ再構築は、インデックスの不整合を直すときだけ使ってください。
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
                  : "処理中…"
                : "全フォルダを再構築"}
            </button>
            {message ? <p className="msg">{message}</p> : null}
          </section>
        </div>
      ) : null}

      {tab === "mail" ? (
        <div
          className="tab-panel"
          role="region"
          id="panel-mail"
          aria-labelledby="tab-mail"
        >
          <section>
            <h2>Outlookメール検索</h2>
            <p className="muted">
              同一 PC の Outlook クラシックのメールを全文検索できます（新しい Outlook
              のみの環境では利用できません）。LAN
              リモート検索にはメールは公開されません。下の手順どおりに進めてください。
            </p>
            <ol className="mail-flow">
              <li>Outlook を検出</li>
              <li>設定を保存</li>
              <li>フォルダを選んで同期</li>
            </ol>
          </section>

          <section className="mail-step">
            <h3 className="mail-step-title">
              <span className="mail-step-num" aria-hidden="true">
                1
              </span>
              Outlook を検出
            </h3>
            <p className="muted mail-step-desc">
              この PC に Outlook クラシックが入っているか確認します。初回操作時にセキュリティ許可のダイアログが出ることがあります。
            </p>
            <div className="row">
              <button
                type="button"
                onClick={() => void detectOutlook()}
                disabled={mailBusy}
              >
                Outlook を検出
              </button>
              {mailDetect ? (
                <span className="mail-detect-ok">{mailDetect}</span>
              ) : (
                <span className="muted">未検出</span>
              )}
            </div>
          </section>

          <section className="mail-step options-form">
            <h3 className="mail-step-title">
              <span className="mail-step-num" aria-hidden="true">
                2
              </span>
              設定
            </h3>
            <p className="muted mail-step-desc">
              インデックスのオン／オフと同期の範囲を決め、「設定を保存」を押してください。
            </p>
            <label className="check">
              <input
                type="checkbox"
                checked={!!settings?.mailEnabled}
                onChange={(e) =>
                  settings &&
                  setSettings({ ...settings, mailEnabled: e.target.checked })
                }
              />
              Outlook メールをインデックスする
            </label>
            <label>
              同期対象の期間（日）
              <input
                type="number"
                min={1}
                max={3650}
                value={settings?.mailDaysBack ?? 730}
                onChange={(e) =>
                  settings &&
                  setSettings({
                    ...settings,
                    mailDaysBack: Number(e.target.value) || 730,
                  })
                }
              />
            </label>
            <label>
              自動同期間隔（秒・0 で手動のみ）
              <input
                type="number"
                min={0}
                step={60}
                value={settings?.mailSyncIntervalSecs ?? 3600}
                onChange={(e) =>
                  settings &&
                  setSettings({
                    ...settings,
                    mailSyncIntervalSecs: Number(e.target.value) || 0,
                  })
                }
              />
            </label>
            <label className="check">
              <input
                type="checkbox"
                checked={!!settings?.mailThreadCollapse}
                onChange={(e) =>
                  settings &&
                  setSettings({
                    ...settings,
                    mailThreadCollapse: e.target.checked,
                  })
                }
              />
              検索結果で同一スレッドを1行にまとめる（推奨）
            </label>
            <label className="check">
              <input
                type="checkbox"
                checked={!!settings?.mailLatestOnly}
                onChange={(e) =>
                  settings &&
                  setSettings({ ...settings, mailLatestOnly: e.target.checked })
                }
              />
              インデックスはスレッド最新通のみ残す（容量節約・古い固有文が欠ける可能性）
            </label>
            <div className="row">
              <button
                type="button"
                className="primary"
                onClick={() => void saveSettings()}
              >
                設定を保存
              </button>
            </div>
          </section>

          <section className="mail-step">
            <h3 className="mail-step-title">
              <span className="mail-step-num" aria-hidden="true">
                3
              </span>
              フォルダの選択・同期
            </h3>
            <p className="muted mail-step-desc">
              まずフォルダ一覧を取得し、対象にチェックを入れてから「今すぐ同期」を実行します。チェックしたフォルダだけが検索対象になります。
            </p>
            <div className="row">
              <button
                type="button"
                onClick={() => void refreshMailFolders()}
                disabled={mailBusy}
              >
                フォルダ一覧を取得
              </button>
              <button
                type="button"
                className="primary"
                onClick={() => void runMailSync()}
                disabled={mailBusy}
              >
                {mailBusy
                  ? mailProgress
                    ? mailProgress.message || "同期中…"
                    : "同期中…"
                  : "今すぐ同期"}
              </button>
            </div>
            <p className="field-hint">
              最終同期: {settings?.mailLastSyncAt || "未実行"} / インデックス済み{" "}
              {mailIndexedCount.toLocaleString()} 通
              {mailFolders.length > 0
                ? ` / 選択中 ${mailFolders.filter((f) => f.selected).length.toLocaleString()} / ${mailFolders.length.toLocaleString()} フォルダ`
                : null}
            </p>
            <ul className="folder-list">
              {mailFolders.length === 0 ? (
                <li className="empty">
                  まだフォルダがありません。上の「フォルダ一覧を取得」を実行してください。
                </li>
              ) : (
                mailFolders.map((f) => {
                  const count = f.indexedCount ?? 0;
                  return (
                    <li
                      key={`${f.storeId}/${f.entryId}`}
                      className={
                        f.selected
                          ? "folder-item mail-folder-item is-selected"
                          : "folder-item mail-folder-item"
                      }
                    >
                      <div className="folder-item-top">
                        <label className="mail-folder-select">
                          <input
                            type="checkbox"
                            checked={f.selected}
                            onChange={(e) =>
                              void toggleMailFolder(f, e.target.checked)
                            }
                          />
                          <span className="folder-main">
                            <span className="folder-path" title={f.pathLabel}>
                              {f.pathLabel || f.name}
                            </span>
                            <span
                              className="folder-count"
                              title="Argos にインデックス済みの通数"
                            >
                              {count.toLocaleString()} 通
                            </span>
                          </span>
                        </label>
                      </div>
                    </li>
                  );
                })
              )}
            </ul>
          </section>
          {message ? <p className="msg">{message}</p> : null}
        </div>
      ) : null}

      {tab === "words" ? (
        <div
          className="tab-panel"
          role="region"
          id="panel-words"
          aria-labelledby="tab-words"
        >
          <section>
            <h2>辞書登録</h2>
            <p className="muted">
              法律用語などの複合語を登録すると、検索時に隣接フレーズとして扱われます（インデックスの分解は変えないため、部分語でもヒットします）。
              各種設定の品詞フィルタと連携し、登録語内の助詞は除外されません。検索ポップアップでは語をドラッグして「辞書に登録」できます。入力中の候補からも履歴・登録語を選べます。
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
              <button
                type="button"
                className="danger"
                onClick={() => void clearAllSearchWords()}
                disabled={searchWords.length === 0}
              >
                すべて削除
              </button>
              <button type="button" onClick={() => void clearSearchHistory()}>
                検索履歴をクリア
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
          role="region"
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
                onChange={(e) => {
                  const shortcut = e.target.value;
                  let notesShortcut = settings.notesShortcut;
                  if (notesShortcut === shortcut) {
                    notesShortcut =
                      SHORTCUT_OPTIONS.find((o) => o.value !== shortcut)?.value ??
                      "Ctrl+Alt+N";
                  }
                  setSettings({ ...settings, shortcut, notesShortcut });
                }}
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
              は、OS の入力言語切替（Alt + Shift）と干渉することがあります。変更は保存時に反映されます。
            </p>
            <label>
              <span className="field-label">ノート用ショートカット</span>
              <span className="field-leader" aria-hidden="true" />
              <select
                value={settings.notesShortcut}
                onChange={(e) =>
                  setSettings({ ...settings, notesShortcut: e.target.value })
                }
              >
                {SHORTCUT_OPTIONS.filter((o) => o.value !== settings.shortcut).map(
                  (o) => (
                    <option key={o.value} value={o.value}>
                      {o.label}
                    </option>
                  ),
                )}
              </select>
            </label>
            <p className="field-hint">
              ノートウィンドウを開きます。検索用ショートカットと同じ値は選べません。
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
            <p className="field-hint">
              ファイルインデックスは登録フォルダの変更監視と、設定画面からの手動再構築で更新されます（定期フル再インデックスの間隔設定はありません）。
            </p>
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

      {tab === "llm" ? (
        <div
          className="tab-panel"
          role="region"
          id="panel-llm"
          aria-labelledby="tab-llm"
        >
          <section>
            <h2>ローカルLLM</h2>
            <p className="muted">
              OpenAI 互換の <code>/v1/chat/completions</code> に接続します。ホストとポートだけ書いた場合は保存時に{" "}
              <code>/v1</code> を付けます。
            </p>
            <p className="muted">
              サーバ側のコンテキスト長（Ollama の <code>num_ctx</code> など）を 64k
              以上にしてください。
            </p>
            {llmUrlLooksRemote(settings.llmBaseUrl) ? (
              <p className="msg">
                URL がこの PC 以外を指しています。インデックスの本文が外部へ送られる可能性があります。
              </p>
            ) : null}
            <div className="row" style={{ marginBottom: 12 }}>
              <button
                type="button"
                onClick={() =>
                  setSettings({
                    ...settings,
                    llmBaseUrl: "http://127.0.0.1:11434/v1",
                  })
                }
              >
                Ollama
              </button>
              <button
                type="button"
                onClick={() =>
                  setSettings({
                    ...settings,
                    llmBaseUrl: "http://127.0.0.1:1234/v1",
                  })
                }
              >
                LM Studio
              </button>
              <button
                type="button"
                onClick={() =>
                  setSettings({
                    ...settings,
                    llmBaseUrl: "http://127.0.0.1:8080/v1",
                  })
                }
              >
                llama.cpp
              </button>
              <button
                type="button"
                onClick={() =>
                  setSettings({
                    ...settings,
                    llmBaseUrl: withLlmPreset(settings.llmBaseUrl, 8000),
                  })
                }
              >
                MTPLX
              </button>
            </div>
            <div className="options-form">
              <label>
                <span className="field-label">ベース URL</span>
                <span className="field-leader" aria-hidden="true" />
                <input
                  value={settings.llmBaseUrl}
                  onChange={(e) =>
                    setSettings({ ...settings, llmBaseUrl: e.target.value })
                  }
                  placeholder="http://127.0.0.1:8000/v1"
                />
              </label>
              <label>
                <span className="field-label">API キー（任意）</span>
                <span className="field-leader" aria-hidden="true" />
                <input
                  type="password"
                  autoComplete="off"
                  value={settings.llmApiKey}
                  onChange={(e) =>
                    setSettings({ ...settings, llmApiKey: e.target.value })
                  }
                />
              </label>
              <label>
                <span className="field-label">モデル</span>
                <span className="field-leader" aria-hidden="true" />
                <input
                  list="llm-model-list"
                  value={settings.llmModel}
                  onChange={(e) =>
                    setSettings({ ...settings, llmModel: e.target.value })
                  }
                  placeholder="接続テストで一覧を取得"
                />
                {llmModels.length > 0 ? (
                  <datalist id="llm-model-list">
                    {llmModels.map((id) => (
                      <option key={id} value={id} />
                    ))}
                  </datalist>
                ) : null}
              </label>
              <label>
                <span className="field-label">タイムアウト（ミリ秒）</span>
                <span className="field-leader" aria-hidden="true" />
                <input
                  type="number"
                  min={5000}
                  max={600000}
                  value={settings.llmTimeoutMs}
                  onChange={(e) =>
                    setSettings({
                      ...settings,
                      llmTimeoutMs: Number(e.target.value) || 120000,
                    })
                  }
                />
              </label>
              <p className="field-hint">
                トークンが止まってから切る待ち時間です。思考中でも流れている限り待ちます。
              </p>
              <label>
                <span className="field-label">思考</span>
                <span className="field-leader" aria-hidden="true" />
                <select
                  value={settings.llmThinking}
                  onChange={(e) =>
                    setSettings({
                      ...settings,
                      llmThinking: e.target.value as SettingsData["llmThinking"],
                    })
                  }
                >
                  <option value="brief">短くする</option>
                  <option value="off">オフ</option>
                  <option value="auto">サーバ任せ</option>
                </select>
              </label>
              {settings.llmThinking === "brief" ? (
                <label>
                  <span className="field-label">思考の上限（トークン）</span>
                  <span className="field-leader" aria-hidden="true" />
                  <input
                    type="number"
                    min={64}
                    max={32000}
                    value={settings.llmThinkingBudget}
                    onChange={(e) =>
                      setSettings({
                        ...settings,
                        llmThinkingBudget: Number(e.target.value) || 2048,
                      })
                    }
                  />
                </label>
              ) : null}
              <p className="field-hint">
                Qwen の長い思考はプロンプトだけではあまり短くなりません。「短くする」は上限トークンをサーバに渡し、「オフ」は思考そのものを止めます。
              </p>
              <label>
                <span className="field-label">コンテキスト目安（文字）</span>
                <span className="field-leader" aria-hidden="true" />
                <input
                  type="number"
                  min={4000}
                  max={200000}
                  value={settings.llmMaxContextChars}
                  onChange={(e) =>
                    setSettings({
                      ...settings,
                      llmMaxContextChars: Number(e.target.value) || 80000,
                    })
                  }
                />
              </label>
              <p className="field-hint">
                出典と会話履歴を合わせた送信量の目安です。サーバのコンテキスト長より小さくしてください。
              </p>
              <label>
                <span className="field-label">インデックス検索の件数</span>
                <span className="field-leader" aria-hidden="true" />
                <input
                  type="number"
                  min={1}
                  max={16}
                  value={settings.llmSearchTopK}
                  onChange={(e) =>
                    setSettings({
                      ...settings,
                      llmSearchTopK: Math.min(
                        16,
                        Math.max(1, Number(e.target.value) || 4),
                      ),
                    })
                  }
                />
              </label>
              <p className="field-hint">
                モデルがインデックス検索ツールを使うときのファイル件数です（1〜16）。1ファイルから最大3段落まで返すため、実際の段落数はこれより多くなります。検索窓から手動で送るときは関係ありません。
              </p>
              <label>
                <span className="field-label">SearXNG URL</span>
                <span className="field-leader" aria-hidden="true" />
                <input
                  value={settings.searxngUrl}
                  onChange={(e) =>
                    setSettings({ ...settings, searxngUrl: e.target.value })
                  }
                  placeholder="http://192.168.0.1:8080"
                  autoComplete="off"
                />
              </label>
              <p className="field-hint">
                別 PC の SearXNG のベース URL です（<code>/search</code> は不要）。空ならチャットのウェブ検索は使えません。検索語は接続先へ送られます。プリセットはありません。
              </p>
              <label>
                <span className="field-label">SearXNG タイムアウト（ミリ秒）</span>
                <span className="field-leader" aria-hidden="true" />
                <input
                  type="number"
                  min={5000}
                  max={30000}
                  value={settings.searxngTimeoutMs}
                  onChange={(e) =>
                    setSettings({
                      ...settings,
                      searxngTimeoutMs: Number(e.target.value) || 8000,
                    })
                  }
                />
              </label>
              <p className="field-hint">
                SearXNG と、メッセージに貼った URL や read_url のページ取得の待ち時間です。
              </p>
              <label>
                <span className="field-label">ウェブ検索の件数</span>
                <span className="field-leader" aria-hidden="true" />
                <input
                  type="number"
                  min={1}
                  max={8}
                  value={settings.llmWebSearchTopK}
                  onChange={(e) =>
                    setSettings({
                      ...settings,
                      llmWebSearchTopK: Math.min(
                        8,
                        Math.max(1, Number(e.target.value) || 5),
                      ),
                    })
                  }
                />
              </label>
              <p className="field-hint">
                チャットでウェブ検索がオンのとき、インデックス検索に添える公開ウェブの件数です（1〜8）。ヒットはスニペットです。本文が必要ならモデルが read_url します。メッセージに貼った URL はトグルなしで本文を取ります。
              </p>
              <label className="llm-prompt-label">
                <span className="field-label">システムプロンプト</span>
                <textarea
                  rows={5}
                  value={settings.llmSystemPrompt}
                  onChange={(e) =>
                    setSettings({ ...settings, llmSystemPrompt: e.target.value })
                  }
                />
              </label>
            </div>
            <div className="row" style={{ marginTop: 12 }}>
              <button
                type="button"
                disabled={testingLlm}
                onClick={() => void testLlm()}
              >
                {testingLlm ? "テスト中…" : "接続テスト"}
              </button>
              <button
                type="button"
                disabled={testingSearxng}
                onClick={() => void testSearxng()}
              >
                {testingSearxng ? "テスト中…" : "SearXNG 接続テスト"}
              </button>
              <button
                type="button"
                onClick={() => void invoke("show_chat_window")}
              >
                チャットを開く
              </button>
            </div>
          </section>
          <section>
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
          role="region"
          id="panel-remote"
          aria-labelledby="tab-remote"
        >
          <section>
            <h2>この PC を検索ホストにする</h2>
            <p className="muted">
              有効にすると、フォルダ設定で「LAN共有」を付けた登録フォルダのインデックスだけを、LAN
              上の他の Argos から検索できるようになります（既定ポート
              17890）。共有フォルダがゼロ件なら、サーバをオンにしてもファイルは出ません。Windows
              ファイアウォールで当該ポートの受信を許可してください。
            </p>
            <div className="options-form">
              <label
                className={
                  settings.remoteServerEnabled ? "ui-switch is-on" : "ui-switch"
                }
              >
                <span className="field-label">このPCのインデックスを共有する</span>
                <span className="field-leader" aria-hidden="true" />
                <input
                  className="ui-switch-input"
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
                <span className="ui-switch-track" aria-hidden="true" />
              </label>
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
              に保存されます。漏洩時は再生成のうえ、全クライアントの接続トークンを更新してください。
            </p>
            <p className="field-hint">
              クライアント用 URL の例: <code>{clientUrlHint}</code>
              （トークンもクライアントに同じものを設定）
            </p>
            <p className="field-hint">
              いま LAN 共有している登録フォルダ:{" "}
              {folders.filter((f) => f.shareRemote).length.toLocaleString()} /{" "}
              {folders.length.toLocaleString()}{" "}
              （変更は「フォルダ設定」）
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
                <span className="field-label">ホスト URL</span>
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
                <span className="field-label">接続トークン</span>
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
          role="region"
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
      </main>
    </div>
  );
}
