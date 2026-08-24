import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type DragEvent,
  type MouseEvent as ReactMouseEvent,
  type ReactNode,
} from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import ChatScopePicker from "./ChatScopePicker";
import { AssistantBody } from "./AssistantBody";
import { openPreview } from "../preview/openPreview";
import { highlightText } from "../search/highlightText";
import {
  fileLabel,
  groupOcrState,
  groupSources,
  imageGroupKey,
  isImageSource,
  isWebSource,
  ocrIncomplete,
  openableCitesFromGroups,
} from "./sourceGroups";
import "./chat.css";

type LlmThreadRow = {
  id: string;
  title: string;
  /** Folder scope for index searches in this thread. Empty means the whole index. */
  pathPrefix?: string;
  sortOrder?: number;
  createdAt: number;
  updatedAt: number;
};

type LlmMessageRow = {
  id: string;
  threadId: string;
  role: string;
  content: string;
  createdAt: number;
};

type LlmSourceRow = {
  id: string;
  threadId: string;
  sortOrder: number;
  origin: string;
  path: string;
  title: string;
  paragraphId: string;
  body: string;
  query: string;
  createdAt: number;
  grain?: string;
  injectedUserMessageId?: string;
  citedAssistantMessageId?: string;
  citeNo?: number;
  kind?: string;
  storedRelpath?: string;
  ocrStatus?: string;
};

type LlmFilePreview = {
  chars: number;
  units: number;
  hardCap: number;
};

type LlmGrainResult = {
  source: LlmSourceRow;
  sources: LlmSourceRow[];
  removed: number;
};

type LlmAttachFilesResult = {
  thread: LlmThreadRow;
  sources: LlmSourceRow[];
  added: number;
  skipped: number;
  createdThread: boolean;
  errors: { path: string; message: string }[];
  remoteOcr: boolean;
  ocrStarted: boolean;
  warnings?: string[];
};

type SettingsData = {
  fontSize: number;
  llmMaxContextChars?: number;
  searxngUrl?: string;
};

type LlmSendResult = {
  thread: LlmThreadRow;
  userMessage: LlmMessageRow;
  assistantMessage: LlmMessageRow | null;
  cancelled: boolean;
  error: string | null;
  truncated?: boolean;
  contextChars?: number;
  warning?: string | null;
};

type LlmChatDelta = {
  requestId: string;
  threadId: string;
  text: string;
  kind?: string;
};

const TPL_SUMMARY =
  "添付した出典を日本語で要約してください。重要な結論と根拠を短く。";
const TPL_POINTS =
  "争点、結論、根拠を箇条書きで整理してください。根拠には出典番号 [n] を付けてください。";
const TPL_FACTS =
  "添付出典だけを根拠に、要件事実の関係を mermaid の flowchart LR でフェンス1本にまとめてください。左から右へ、請求原因 → 抗弁 → 再抗弁。請求原因はノードを1つだけにし、複数の主要事実は同じノード内に番号で書いてください。抗弁は種類ごとに分け、その右に対応する再抗弁を置いてください。ノードは短く。ラベルは A[\"請求原因 [n]\"] のように二重引用符で囲んでください。矢印は --> または -.-> で、途中に空白を入れないでください。添付出典にない事実はノードにしないでください。";
const TPL_TIMELINE =
  "添付出典に書かれた出来事だけを時系列で整理してください。図にする場合は mermaid の flowchart LR または timeline をフェンス1本にしてください。ノードラベルは二重引用符で囲み、出典番号 [n] をラベル内に書いてください。出典にない出来事は入れないでください。";

const ATTACH_EXTENSIONS = [
  "pdf",
  "docx",
  "doc",
  "xlsx",
  "xls",
  "txt",
  "md",
  "markdown",
  "json",
  "html",
  "htm",
  "jtd",
  "png",
  "jpg",
  "jpeg",
  "gif",
  "webp",
];

function sleep(ms: number) {
  return new Promise<void>((resolve) => setTimeout(resolve, ms));
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

async function waitReady(maxAttempts = 50): Promise<void> {
  for (let attempt = 0; attempt < maxAttempts; attempt++) {
    try {
      const ready = await invoke<boolean>("is_app_ready");
      if (ready) return;
    } catch {
      /* still starting */
    }
    await sleep(Math.min(50 * 2 ** Math.min(attempt, 4), 1000));
  }
}

function sourceLabel(s: LlmSourceRow): string {
  const t = s.title.trim();
  if (t) return t;
  const p = s.path.trim();
  if (p) return p;
  return "出典";
}

function isFileGrain(s: LlmSourceRow): boolean {
  return (s.grain ?? "unit").toLowerCase() === "file";
}

function sourceCanExpand(s: LlmSourceRow): boolean {
  if (isImageSource(s)) return false;
  if (isWebSource(s)) return false;
  if (!s.path.trim()) return false;
  if (isFileGrain(s) && !(s.paragraphId ?? "").trim()) return false;
  return true;
}

function sourceOpenTitle(s: LlmSourceRow): string {
  return isWebSource(s) ? "ブラウザで開く" : "プレビュー";
}

function isPendingSource(s: LlmSourceRow): boolean {
  const injected = (s.injectedUserMessageId ?? "").trim();
  if (injected) return false;
  return (s.origin ?? "attach").toLowerCase() !== "tool";
}

function isUncitedToolSource(s: LlmSourceRow): boolean {
  const injected = (s.injectedUserMessageId ?? "").trim();
  return !injected && (s.origin ?? "").toLowerCase() === "tool";
}

function citedForMessage(
  sources: LlmSourceRow[],
  messageId: string,
): LlmSourceRow[] {
  return sources
    .filter((s) => (s.citedAssistantMessageId ?? "") === messageId)
    .sort((a, b) => (a.citeNo ?? 0) - (b.citeNo ?? 0) || a.sortOrder - b.sortOrder);
}

function chipLabel(s: LlmSourceRow): string {
  return imageGroupKey(s) ? fileLabel(s) : sourceLabel(s);
}

function IconOpenFile() {
  return (
    <svg width="14" height="14" viewBox="0 0 24 24" aria-hidden="true">
      <path
        fill="currentColor"
        d="M14 3h7v7h-2V6.41l-9.29 9.3-1.42-1.42 9.3-9.29H14V3zM5 5h6v2H7v10h10v-4h2v6H5V5z"
      />
    </svg>
  );
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

function IconNotes() {
  return (
    <NavIcon>
      <path d="M3.5 3.25h9A1.25 1.25 0 0 1 13.75 4.5v7A1.25 1.25 0 0 1 12.5 12.75h-9A1.25 1.25 0 0 1 2.25 11.5v-7A1.25 1.25 0 0 1 3.5 3.25Z" />
      <path d="M5.25 6.25h5.5" />
      <path d="M5.25 8.75h4" />
    </NavIcon>
  );
}

function IconSettings() {
  return (
    <svg
      viewBox="0 0 24 24"
      width="16"
      height="16"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d="M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6Z" />
      <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1Z" />
    </svg>
  );
}

function ActionIcon({ children }: { children: ReactNode }) {
  return (
    <svg
      viewBox="0 0 16 16"
      width="14"
      height="14"
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

function IconGrip() {
  return (
    <ActionIcon>
      <circle cx="6" cy="4" r="0.9" fill="currentColor" stroke="none" />
      <circle cx="10" cy="4" r="0.9" fill="currentColor" stroke="none" />
      <circle cx="6" cy="8" r="0.9" fill="currentColor" stroke="none" />
      <circle cx="10" cy="8" r="0.9" fill="currentColor" stroke="none" />
      <circle cx="6" cy="12" r="0.9" fill="currentColor" stroke="none" />
      <circle cx="10" cy="12" r="0.9" fill="currentColor" stroke="none" />
    </ActionIcon>
  );
}

function IconTrash() {
  return (
    <ActionIcon>
      <path d="M3.5 5.5h9" />
      <path d="M6 5.5V4.25A1.25 1.25 0 0 1 7.25 3h1.5A1.25 1.25 0 0 1 10 4.25V5.5" />
      <path d="M5 5.5l.5 7h5l.5-7" />
    </ActionIcon>
  );
}

function countChars(s: string): number {
  return [...s].length;
}

function matchesListQuery(text: string, query: string): boolean {
  const q = query.trim().normalize("NFKC").toLowerCase();
  if (!q) return true;
  return text.normalize("NFKC").toLowerCase().includes(q);
}

function threadListTitle(t: { title: string }): string {
  return t.title.trim() || "新しい会話";
}

const SIDEBAR_MIN = 160;
const SIDEBAR_MAX = 420;
const SIDEBAR_DEFAULT = 220;
const SIDEBAR_WIDTH_KEY = "argos.chat.sidebarWidth";

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

export default function Chat() {
  const [threads, setThreads] = useState<LlmThreadRow[]>([]);
  const [active, setActive] = useState<LlmThreadRow | null>(null);
  const [messages, setMessages] = useState<LlmMessageRow[]>([]);
  const [sources, setSources] = useState<LlmSourceRow[]>([]);
  const [draft, setDraft] = useState("");
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [busy, setBusy] = useState(false);
  const [ocrBusy, setOcrBusy] = useState(false);
  const [attaching, setAttaching] = useState(false);
  const [dropActive, setDropActive] = useState(false);
  const [stream, setStream] = useState("");
  const [thinking, setThinking] = useState("");
  const [toolHint, setToolHint] = useState("");
  const [fontSize, setFontSize] = useState(14);
  const [maxContextChars, setMaxContextChars] = useState(80_000);
  const [searxngUrl, setSearxngUrl] = useState("");
  const [webSearch, setWebSearch] = useState(false);
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState("");
  const [sidebarWidth, setSidebarWidth] = useState(loadSidebarWidth);
  const [resizingSidebar, setResizingSidebar] = useState(false);
  const [threadDragId, setThreadDragId] = useState<string | null>(null);
  const [threadDraggableId, setThreadDraggableId] = useState<string | null>(null);
  const [listQuery, setListQuery] = useState("");
  const [contentHitIds, setContentHitIds] = useState<string[] | null>(null);
  const listRef = useRef<HTMLDivElement | null>(null);
  const rootRef = useRef<HTMLDivElement | null>(null);
  const bootstrappingRef = useRef(false);
  const activeIdRef = useRef<string | null>(null);
  const busyRef = useRef(false);
  const ocrBusyRef = useRef(false);
  const attachingRef = useRef(false);
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);

  useEffect(() => {
    activeIdRef.current = active?.id ?? null;
  }, [active]);

  useEffect(() => {
    busyRef.current = busy;
  }, [busy]);

  useEffect(() => {
    ocrBusyRef.current = ocrBusy;
  }, [ocrBusy]);

  useEffect(() => {
    attachingRef.current = attaching;
  }, [attaching]);

  const loadThreadContent = useCallback(async (thread: LlmThreadRow | null) => {
    if (!thread) {
      setMessages([]);
      setSources([]);
      return;
    }
    const [msgs, srcs] = await Promise.all([
      invoke<LlmMessageRow[]>("llm_list_messages", { threadId: thread.id }),
      invoke<LlmSourceRow[]>("llm_list_sources", { threadId: thread.id }),
    ]);
    setMessages(msgs);
    setSources(srcs);
  }, []);

  const loadThreads = useCallback(async () => {
    const list = await invoke<LlmThreadRow[]>("llm_list_threads");
    setThreads(list);
    let current = await invoke<LlmThreadRow | null>("llm_get_active_thread");
    if (!current && list.length > 0) {
      current = await invoke<LlmThreadRow>("llm_set_active_thread", {
        id: list[0].id,
      });
    }
    setActive(current);
    await loadThreadContent(current);
  }, [loadThreadContent]);

  useEffect(() => {
    const q = listQuery.trim();
    if (!q) {
      setContentHitIds(null);
      return;
    }
    let cancelled = false;
    const timer = window.setTimeout(() => {
      void invoke<string[]>("llm_search_threads", { query: q })
        .then((ids) => {
          if (!cancelled) setContentHitIds(ids);
        })
        .catch(() => {
          if (!cancelled) setContentHitIds([]);
        });
    }, 180);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [listQuery]);

  const visibleThreads = useMemo(() => {
    const q = listQuery.trim();
    if (!q) return threads;
    const content = new Set(contentHitIds ?? []);
    return threads.filter(
      (t) => matchesListQuery(threadListTitle(t), q) || content.has(t.id),
    );
  }, [threads, listQuery, contentHitIds]);

  const listSearching = listQuery.trim().length > 0 && contentHitIds === null;

  const applySettings = useCallback((s: SettingsData) => {
    if (typeof s.fontSize === "number" && s.fontSize > 0) {
      setFontSize(s.fontSize);
    }
    if (typeof s.llmMaxContextChars === "number" && s.llmMaxContextChars > 0) {
      setMaxContextChars(s.llmMaxContextChars);
    }
    const url = typeof s.searxngUrl === "string" ? s.searxngUrl.trim() : "";
    setSearxngUrl(url);
    if (!url) setWebSearch(false);
  }, []);

  const bootstrap = useCallback(async () => {
    if (bootstrappingRef.current) return;
    bootstrappingRef.current = true;
    try {
      await waitReady();
      try {
        const s = await invoke<SettingsData>("get_settings");
        applySettings(s);
      } catch {
        /* keep default */
      }
      await loadThreads();
      setError("");
    } catch (e) {
      setError(formatInvokeError(e));
    } finally {
      bootstrappingRef.current = false;
    }
  }, [applySettings, loadThreads]);

  useEffect(() => {
    void bootstrap();
  }, [bootstrap]);

  useEffect(() => {
    const win = getCurrentWindow();
    let cancelled = false;
    let unlistenFocus: (() => void) | undefined;
    let unlistenReady: (() => void) | undefined;
    let unlistenSettings: (() => void) | undefined;
    void win
      .onFocusChanged((event) => {
        if (event.payload && !busyRef.current && !ocrBusyRef.current && !attachingRef.current) void bootstrap();
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlistenFocus = fn;
      });
    void listen("argos-ready", () => {
      void bootstrap();
    }).then((fn) => {
      if (cancelled) fn();
      else unlistenReady = fn;
    });
    void listen("settings-updated", () => {
      void invoke<SettingsData>("get_settings")
        .then(applySettings)
        .catch(() => {
          /* keep current */
        });
    }).then((fn) => {
      if (cancelled) fn();
      else unlistenSettings = fn;
    });
    return () => {
      cancelled = true;
      unlistenFocus?.();
      unlistenReady?.();
      unlistenSettings?.();
    };
  }, [applySettings, bootstrap]);

  useEffect(() => {
    let unlistenDelta: (() => void) | undefined;
    let unlistenError: (() => void) | undefined;
    let unlistenSources: (() => void) | undefined;
    let unlistenOcr: (() => void) | undefined;
    void listen<LlmChatDelta>("llm-chat-delta", (event) => {
      if (event.payload.threadId !== activeIdRef.current) return;
      const kind = event.payload.kind ?? "content";
      if (kind === "reasoning") {
        setThinking((prev) => prev + event.payload.text);
      } else if (kind === "tool") {
        setToolHint(event.payload.text || "インデックスを検索しています…");
      } else {
        setStream((prev) => prev + event.payload.text);
      }
    }).then((fn) => {
      unlistenDelta = fn;
    });
    void listen<{ threadId: string; message: string }>("llm-chat-error", (event) => {
      if (event.payload.threadId !== activeIdRef.current) return;
      setError(event.payload.message);
    }).then((fn) => {
      unlistenError = fn;
    });
    void listen<{ threadId: string }>("llm-sources-updated", (event) => {
      const tid = event.payload.threadId;
      if (busyRef.current || ocrBusyRef.current || attachingRef.current) {
        if (tid === activeIdRef.current) {
          void invoke<LlmSourceRow[]>("llm_list_sources", { threadId: tid })
            .then(setSources)
            .catch((e) => setError(formatInvokeError(e)));
        }
        return;
      }
      const prev = activeIdRef.current;
      void loadThreads()
        .then(() => {
          if (tid !== prev) {
            setDraft("");
            setStream("");
            setThinking("");
            setToolHint("");
            setError("");
          }
        })
        .catch((e) => setError(formatInvokeError(e)));
    }).then((fn) => {
      unlistenSources = fn;
    });
    void listen<{ threadId: string; active: boolean }>("llm-ocr-status", (event) => {
      setOcrBusy(event.payload.active);
    }).then((fn) => {
      unlistenOcr = fn;
    });
    return () => {
      unlistenDelta?.();
      unlistenError?.();
      unlistenSources?.();
      unlistenOcr?.();
    };
  }, [loadThreads]);

  const scrollLog = useCallback(() => {
    const el = listRef.current;
    if (!el) return;
    el.scrollTop = el.scrollHeight;
  }, []);

  useEffect(() => {
    scrollLog();
  }, [messages, stream, thinking, toolHint, busy, scrollLog]);

  const estimatedChars = useMemo(() => {
    const src = sources.reduce(
      (n, s) => n + countChars(s.body) + countChars(s.title) + 24,
      0,
    );
    const hist = messages.reduce((n, m) => n + countChars(m.content), 0);
    return src + hist + countChars(draft);
  }, [sources, messages, draft]);

  const pendingSources = useMemo(
    () => sources.filter(isPendingSource),
    [sources],
  );

  const liveCites = useMemo(() => {
    const ready = pendingSources.filter((s) => !ocrIncomplete(s));
    const tools = sources.filter(isUncitedToolSource);
    return [...ready, ...tools];
  }, [pendingSources, sources]);

  const maxCitedNo = useMemo(
    () =>
      sources
        .filter((s) => (s.injectedUserMessageId ?? "").trim())
        .reduce((n, s) => Math.max(n, s.citeNo ?? 0), 0),
    [sources],
  );

  const pendingGroups = useMemo(
    () => groupSources(pendingSources, maxCitedNo + 1),
    [pendingSources, maxCitedNo],
  );

  const liveCiteGroups = useMemo(
    () => groupSources(liveCites, maxCitedNo + 1),
    [liveCites, maxCitedNo],
  );

  const liveCiteInfo = useMemo(
    () => openableCitesFromGroups(liveCiteGroups),
    [liveCiteGroups],
  );

  const overBudget = estimatedChars > maxContextChars;
  const blocked = busy || ocrBusy || attaching;

  async function selectThread(id: string) {
    if (blocked) return;
    try {
      const t = await invoke<LlmThreadRow>("llm_set_active_thread", { id });
      setActive(t);
      await loadThreadContent(t);
      setStream("");
      setThinking("");
      setToolHint("");
      setError("");
      setNotice("");
    } catch (e) {
      setError(formatInvokeError(e));
    }
  }

  async function createThread() {
    if (blocked) return;
    try {
      const t = await invoke<LlmThreadRow>("llm_create_thread", { title: "" });
      setThreads((prev) => [t, ...prev.filter((x) => x.id !== t.id)]);
      setActive(t);
      setMessages([]);
      setSources([]);
      setStream("");
      setThinking("");
      setToolHint("");
      setError("");
      setNotice("");
      textareaRef.current?.focus();
    } catch (e) {
      setError(formatInvokeError(e));
    }
  }

  async function deleteThread(id: string) {
    if (blocked) return;
    try {
      await invoke("llm_delete_thread", { id });
      await loadThreads();
      setStream("");
      setThinking("");
    } catch (e) {
      setError(formatInvokeError(e));
    }
  }

  async function commitRename(id: string) {
    const title = renameDraft.trim();
    setRenamingId(null);
    try {
      const t = await invoke<LlmThreadRow>("llm_rename_thread", { id, title });
      setThreads((prev) => prev.map((x) => (x.id === id ? t : x)));
      if (active?.id === id) setActive(t);
    } catch (e) {
      setError(formatInvokeError(e));
    }
  }

  const applyThreadScope = useCallback((t: LlmThreadRow) => {
    setThreads((prev) => prev.map((x) => (x.id === t.id ? t : x)));
    setActive((prev) => (prev?.id === t.id ? t : prev));
  }, []);

  async function openSource(s: LlmSourceRow) {
    const path = s.path.trim();
    if (!path) return;
    try {
      if (isWebSource(s)) {
        await invoke("open_web_url", { url: path });
        return;
      }
      await openPreview({
        origin: "chat",
        path,
        paragraphId: s.paragraphId,
        query: s.query,
        title: imageGroupKey(s) ? fileLabel(s) : s.title,
        fallbackBody: s.body,
        kind: s.kind,
        sourceId: s.id,
      });
    } catch (e) {
      setError(formatInvokeError(e));
    }
  }

  async function removeSource(id: string) {
    const seed = sources.find((s) => s.id === id);
    const key = seed ? imageGroupKey(seed) : null;
    try {
      await invoke("llm_remove_source", { id });
      setSources((prev) =>
        key
          ? prev.filter((s) => imageGroupKey(s) !== key)
          : prev.filter((s) => s.id !== id),
      );
    } catch (e) {
      setError(formatInvokeError(e));
    }
  }

  async function toggleSourceGrain(s: LlmSourceRow) {
    if (blocked || !sourceCanExpand(s)) return;
    setError("");
    setNotice("");
    try {
      if (isFileGrain(s)) {
        const result = await invoke<LlmGrainResult>("llm_set_source_grain", {
          id: s.id,
          grain: "unit",
        });
        setSources(result.sources);
        return;
      }
      const preview = await invoke<LlmFilePreview>("llm_preview_source_file", {
        id: s.id,
      });
      if (preview.chars > maxContextChars) {
        const ok = window.confirm(
          `このファイルは約 ${preview.chars.toLocaleString()} 文字です。送信の上限は ${maxContextChars.toLocaleString()} 文字です。全文にしますか？`,
        );
        if (!ok) return;
      }
      const result = await invoke<LlmGrainResult>("llm_set_source_grain", {
        id: s.id,
        grain: "file",
      });
      setSources(result.sources);
      if (result.removed > 0) {
        setNotice(
          result.removed === 1
            ? "同じファイルの他の段落を外して全文にしました。"
            : `同じファイルの段落 ${result.removed} 件を外して全文にしました。`,
        );
      }
    } catch (e) {
      setError(formatInvokeError(e));
    }
  }

  function applyTemplate(text: string) {
    setDraft(text);
    requestAnimationFrame(() => {
      const el = textareaRef.current;
      if (!el) return;
      el.focus();
      el.selectionStart = el.selectionEnd = text.length;
    });
  }

  const attachFiles = useCallback(async (paths: string[]) => {
    const cleaned = paths.map((p) => p.trim()).filter(Boolean);
    if (
      !cleaned.length ||
      busyRef.current ||
      ocrBusyRef.current ||
      attachingRef.current
    ) {
      return;
    }
    setError("");
    setNotice("");
    setAttaching(true);
    try {
      const result = await invoke<LlmAttachFilesResult>("llm_attach_files", {
        paths: cleaned,
        threadId: activeIdRef.current ?? undefined,
      });
      setActive(result.thread);
      activeIdRef.current = result.thread.id;
      setThreads((prev) => {
        const rest = prev.filter((x) => x.id !== result.thread.id);
        return [result.thread, ...rest];
      });
      setSources(result.sources);
      if (result.errors.length) {
        setError(result.errors.map((e) => `${e.message}`).join("\n"));
      }
      const bits: string[] = [];
      if (result.skipped > 0) {
        bits.push("同じ出典がすでに読込前にあります");
      }
      if (result.remoteOcr) {
        bits.push("画像は設定中の LLM サーバへ送られます");
      }
      if (result.warnings?.length) {
        bits.push(...result.warnings);
      }
      if (bits.length) setNotice(bits.join(" · "));
    } catch (e) {
      setError(formatInvokeError(e));
    } finally {
      setAttaching(false);
    }
  }, []);

  async function pickFiles() {
    if (blocked) return;
    const selected = await openDialog({
      multiple: true,
      title: "会話に添付するファイル",
      filters: [{ name: "文書と画像", extensions: ATTACH_EXTENSIONS }],
    });
    if (!selected) return;
    const paths = Array.isArray(selected) ? selected : [selected];
    await attachFiles(paths);
  }

  async function retryOcr(id: string) {
    setError("");
    try {
      const srcs = await invoke<LlmSourceRow[]>("llm_retry_ocr", { id });
      setSources(srcs);
    } catch (e) {
      setError(formatInvokeError(e));
    }
  }

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    void getCurrentWebview()
      .onDragDropEvent((event) => {
        if (event.payload.type === "enter" || event.payload.type === "over") {
          if (!busyRef.current && !ocrBusyRef.current && !attachingRef.current) setDropActive(true);
          return;
        }
        setDropActive(false);
        if (event.payload.type !== "drop") return;
        if (busyRef.current || ocrBusyRef.current || attachingRef.current) return;
        const paths = event.payload.paths ?? [];
        if (paths.length) void attachFiles(paths);
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [attachFiles]);

  async function send() {
    const text = draft.trim();
    if (!text || blocked) return;
    setBusy(true);
    setError("");
    setNotice("");
    setDraft("");
    setStream("");
    setThinking("");
    setToolHint("");
    let thread: LlmThreadRow;
    try {
      thread =
        active ?? (await invoke<LlmThreadRow>("llm_create_thread", { title: "" }));
      if (!active) {
        setActive(thread);
        activeIdRef.current = thread.id;
        const created = thread;
        setThreads((prev) => [created, ...prev.filter((x) => x.id !== created.id)]);
      }
    } catch (e) {
      setBusy(false);
      setDraft(text);
      setError(formatInvokeError(e));
      return;
    }
    const optimistic: LlmMessageRow = {
      id: `tmp-${Date.now()}`,
      threadId: thread.id,
      role: "user",
      content: text,
      createdAt: Math.floor(Date.now() / 1000),
    };
    setMessages((prev) => [...prev, optimistic]);
    try {
      const result = await invoke<LlmSendResult>("llm_send", {
        threadId: thread.id,
        content: text,
        webSearch: webSearch && !!searxngUrl,
      });
      setActive(result.thread);
      setThreads((prev) =>
        prev.map((x) => (x.id === result.thread.id ? result.thread : x)),
      );
      const msgs = await invoke<LlmMessageRow[]>("llm_list_messages", {
        threadId: result.thread.id,
      });
      const srcs = await invoke<LlmSourceRow[]>("llm_list_sources", {
        threadId: result.thread.id,
      });
      setMessages(msgs);
      setSources(srcs);
      setStream("");
      setThinking("");
      setToolHint("");
      if (result.cancelled) {
        setError("生成を停止しました。");
      } else if (result.error) {
        setError(result.error);
      } else if (result.warning) {
        setNotice(result.warning);
      } else if (result.truncated) {
        setNotice("文字数上限のため、古い出典から切り詰めて送りました。");
      }
    } catch (e) {
      setStream("");
      setThinking("");
      setToolHint("");
      const msg = formatInvokeError(e);
      if (msg.includes("生成中です")) {
        try {
          await invoke("llm_cancel");
        } catch {
          /* ignore */
        }
        setDraft(text);
        setError(
          "前の生成が途中で止まっていました。停止したので、もう一度送信してください。",
        );
      } else {
        setError(msg);
      }
      try {
        const msgs = await invoke<LlmMessageRow[]>("llm_list_messages", {
          threadId: thread.id,
        });
        setMessages(msgs);
      } catch {
        /* keep optimistic */
      }
    } finally {
      setBusy(false);
    }
  }

  async function stop() {
    try {
      await invoke("llm_cancel");
    } catch (e) {
      setError(formatInvokeError(e));
    }
  }

  async function openWindow(cmd: string) {
    try {
      await invoke(cmd);
    } catch (e) {
      setError(formatInvokeError(e));
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

  const onThreadDragOver = useCallback((e: DragEvent) => {
    e.preventDefault();
    e.dataTransfer.dropEffect = "move";
  }, []);

  const onThreadListMouseDown = useCallback((e: ReactMouseEvent, id: string) => {
    const el = e.target as HTMLElement | null;
    if (el?.closest(".chat-thread-handle")) {
      setThreadDraggableId(id);
      return;
    }
    setThreadDraggableId(null);
  }, []);

  const onThreadDragStart = useCallback(
    (e: DragEvent, id: string) => {
      if (threadDraggableId !== id) {
        e.preventDefault();
        return;
      }
      setThreadDragId(id);
      e.dataTransfer.effectAllowed = "move";
      e.dataTransfer.setData("text/thread-id", id);
      e.dataTransfer.setData("text/plain", id);
    },
    [threadDraggableId],
  );

  const onThreadDrop = useCallback(
    async (e: DragEvent, targetId: string) => {
      e.preventDefault();
      e.stopPropagation();
      const sourceId =
        threadDragId ||
        e.dataTransfer.getData("text/thread-id") ||
        e.dataTransfer.getData("text/plain");
      setThreadDragId(null);
      setThreadDraggableId(null);
      if (!sourceId || sourceId === targetId) return;
      const ids = threads.map((t) => t.id);
      const from = ids.indexOf(sourceId);
      const to = ids.indexOf(targetId);
      if (from < 0 || to < 0) return;
      const next = [...ids];
      next.splice(from, 1);
      next.splice(to, 0, sourceId);
      setThreads((prev) => {
        const map = new Map(prev.map((t) => [t.id, t]));
        return next
          .map((id, i) => {
            const t = map.get(id);
            return t ? { ...t, sortOrder: i } : null;
          })
          .filter(Boolean) as LlmThreadRow[];
      });
      try {
        await invoke("llm_reorder_threads", { orderedIds: next });
      } catch (err) {
        setError(formatInvokeError(err));
        await loadThreads();
      }
    },
    [threadDragId, threads, loadThreads],
  );

  return (
    <div
      ref={rootRef}
      className={["chat", resizingSidebar ? "chat--resizing" : ""]
        .filter(Boolean)
        .join(" ")}
      style={{
        fontSize: `${fontSize}px`,
        gridTemplateColumns: `${sidebarWidth}px 5px minmax(0, 1fr)`,
      }}
    >
      <aside className="chat-sidebar">
        <header className="chat-sidebar-head">
          <h1>チャット</h1>
          <button
            type="button"
            className="chat-btn primary"
            onClick={() => void createThread()}
            disabled={blocked}
          >
            新規
          </button>
        </header>
        <div className="chat-sidebar-search">
          <span className="chat-sidebar-search-icon" aria-hidden="true">
            <IconSearch />
          </span>
          <input
            className="chat-sidebar-search-input"
            type="text"
            role="searchbox"
            value={listQuery}
            placeholder="会話を検索"
            aria-label="会話を検索"
            spellCheck={false}
            autoComplete="off"
            onChange={(e) => {
              setListQuery(e.target.value);
              setContentHitIds(null);
            }}
            onKeyDown={(e) => {
              if (e.key === "Escape" && listQuery) {
                e.preventDefault();
                setListQuery("");
              }
            }}
          />
          {listQuery ? (
            <button
              type="button"
              className="chat-sidebar-search-clear"
              title="検索を消す"
              aria-label="検索を消す"
              onClick={() => setListQuery("")}
            >
              ×
            </button>
          ) : null}
        </div>
        <div className="chat-sidebar-body">
          {threads.length === 0 ? (
            <p className="chat-muted chat-sidebar-empty">会話はまだありません。</p>
          ) : visibleThreads.length === 0 ? (
            <p className="chat-muted chat-sidebar-empty">
              {listSearching ? "検索中…" : "一致する会話がありません。"}
            </p>
          ) : (
            <ul className="chat-thread-list">
              {visibleThreads.map((t) => {
                const title = threadListTitle(t);
                const q = listQuery.trim();
                const titleHit = q ? matchesListQuery(title, q) : true;
                return (
                <li
                  key={t.id}
                  className={[
                    t.id === active?.id ? "chat-thread-item active" : "chat-thread-item",
                    threadDragId === t.id ? "chat-thread-item--dragging" : "",
                  ]
                    .filter(Boolean)
                    .join(" ")}
                  draggable={threadDraggableId === t.id}
                  onMouseDown={(e) => onThreadListMouseDown(e, t.id)}
                  onDragStart={(e) => onThreadDragStart(e, t.id)}
                  onDragOver={onThreadDragOver}
                  onDrop={(e) => void onThreadDrop(e, t.id)}
                  onDragEnd={() => {
                    setThreadDragId(null);
                    setThreadDraggableId(null);
                  }}
                >
                  <span
                    className="chat-thread-handle"
                    title="ドラッグで並べ替え"
                    aria-hidden="true"
                  >
                    <IconGrip />
                  </span>
                  {renamingId === t.id ? (
                    <input
                      className="chat-rename"
                      value={renameDraft}
                      autoFocus
                      onChange={(e) => setRenameDraft(e.target.value)}
                      onBlur={() => void commitRename(t.id)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") {
                          e.preventDefault();
                          void commitRename(t.id);
                        }
                        if (e.key === "Escape") setRenamingId(null);
                      }}
                    />
                  ) : (
                    <button
                      type="button"
                      className="chat-thread-title"
                      onClick={() => void selectThread(t.id)}
                      onDoubleClick={() => {
                        setRenamingId(t.id);
                        setRenameDraft(t.title);
                      }}
                    >
                      {q && titleHit ? highlightText(title, q) : title}
                    </button>
                  )}
                  {q && !titleHit ? (
                    <span className="chat-thread-hit">内容</span>
                  ) : null}
                  <button
                    type="button"
                    className="chat-icon-btn danger"
                    title="削除"
                    aria-label="会話を削除"
                    disabled={blocked}
                    onClick={() => void deleteThread(t.id)}
                  >
                    <IconTrash />
                  </button>
                </li>
                );
              })}
            </ul>
          )}
        </div>
        <footer className="chat-sidebar-foot">
          <button
            type="button"
            className="chat-nav-link"
            title="検索"
            aria-label="検索"
            onClick={() => void openWindow("show_popup_window")}
          >
            <IconSearch />
          </button>
          <button
            type="button"
            className="chat-nav-link"
            title="ノート"
            aria-label="ノート"
            onClick={() => void openWindow("show_notes_window")}
          >
            <IconNotes />
          </button>
          <button
            type="button"
            className="chat-nav-link"
            title="設定"
            aria-label="設定"
            onClick={() => void openWindow("show_settings_window")}
          >
            <IconSettings />
          </button>
        </footer>
      </aside>

      <div
        className="chat-splitter"
        role="separator"
        aria-orientation="vertical"
        aria-label="会話リストの幅を変更"
        aria-valuemin={SIDEBAR_MIN}
        aria-valuemax={SIDEBAR_MAX}
        aria-valuenow={sidebarWidth}
        onMouseDown={onSidebarResizeStart}
      />

      <main className="chat-main">
        <div className="chat-log" ref={listRef}>
          {messages.length === 0 && !stream && !thinking && !toolHint && !busy ? (
            <p className="chat-muted chat-empty">
              検索やノートの「チャット」から出典を送るか、ファイルを添付して会話できます。出典なしでも、設定の「ローカルLLM」に接続して会話できます。足りない資料はモデルがインデックスを検索することもあります。
            </p>
          ) : (
            messages.map((m) => {
              const cites =
                m.role === "assistant" ? citedForMessage(sources, m.id) : [];
              const citeGroups = groupSources(cites, 1);
              const citeInfo = openableCitesFromGroups(citeGroups);
              return (
                <article
                  key={m.id}
                  className={m.role === "user" ? "chat-msg user" : "chat-msg assistant"}
                >
                  {m.role === "assistant" ? (
                    <AssistantBody
                      text={m.content}
                      citeNos={citeInfo.nos}
                      onCite={(n) => {
                        const s = citeInfo.byNo.get(n);
                        if (s) void openSource(s);
                      }}
                      onChoice={applyTemplate}
                      choicesDisabled={blocked}
                      onLayout={scrollLog}
                    />
                  ) : (
                    <div className="chat-msg-body">{m.content}</div>
                  )}
                  {citeGroups.length > 0 ? (
                    <div className="chat-msg-cites" aria-label="出典">
                      {citeGroups.map((g) => {
                        const s = g.representative;
                        const path = s.path.trim();
                        if (!path) {
                          return (
                            <span key={g.key} className="chat-cite-badge">
                              [{g.citeNo}] {chipLabel(s)}
                            </span>
                          );
                        }
                        return (
                          <button
                            key={g.key}
                            type="button"
                            className="chat-cite-badge clickable"
                            title={sourceOpenTitle(s)}
                            onClick={() => void openSource(s)}
                          >
                            [{g.citeNo}] {chipLabel(s)}
                          </button>
                        );
                      })}
                    </div>
                  ) : null}
                </article>
              );
            })
          )}
          {busy || stream || thinking || toolHint ? (
            <article className="chat-msg assistant streaming">
              {toolHint ? <p className="chat-tool-hint">{toolHint}</p> : null}
              {thinking ? (
                <details className="chat-thinking">
                  <summary>{busy ? "思考中（表示する）" : "思考"}</summary>
                  <div className="chat-thinking-body">{thinking}</div>
                </details>
              ) : null}
              <AssistantBody
                text={stream || (!thinking && !toolHint ? "生成中…" : "")}
                citeNos={liveCiteInfo.nos}
                onCite={(n) => {
                  const s = liveCiteInfo.byNo.get(n);
                  if (s) void openSource(s);
                }}
                onChoice={applyTemplate}
                choicesDisabled
                streaming={!!stream}
                showCaret={!!stream || (!thinking && !toolHint)}
                onLayout={scrollLog}
              />
              {liveCiteGroups.length > 0 ? (
                <div className="chat-msg-cites" aria-label="今回の出典">
                  {liveCiteGroups.map((g) => {
                    const s = g.representative;
                    const path = s.path.trim();
                    if (!path) {
                      return (
                        <span key={g.key} className="chat-cite-badge">
                          [{g.citeNo}] {chipLabel(s)}
                        </span>
                      );
                    }
                    return (
                      <button
                        key={g.key}
                        type="button"
                        className="chat-cite-badge clickable"
                        title={sourceOpenTitle(s)}
                        onClick={() => void openSource(s)}
                      >
                        [{g.citeNo}] {chipLabel(s)}
                      </button>
                    );
                  })}
                </div>
              ) : null}
            </article>
          ) : null}
          {ocrBusy && !busy ? (
            <p className="chat-tool-hint">画像を読み取っています…</p>
          ) : null}
        </div>
        {error ? <p className="chat-error">{error}</p> : null}
        {notice ? <p className="chat-notice">{notice}</p> : null}
        <form
          className={dropActive ? "chat-composer drop-active" : "chat-composer"}
          onSubmit={(e) => {
            e.preventDefault();
            void send();
          }}
        >
          {pendingGroups.length > 0 ? (
            <div className="chat-sources" aria-label="これから読む出典">
              {pendingGroups.map((g) => {
                const s = g.representative;
                const expandable = sourceCanExpand(s);
                const fileGrain = isFileGrain(s);
                const image = isImageSource(s);
                const ocr = groupOcrState(g, ocrBusy);
                const n = g.citeNo;
                const web = isWebSource(s);
                const chipClass = [
                  "chat-source-chip",
                  fileGrain || image ? "file" : "",
                  ocr.interrupted ? "ocr-error" : "",
                ]
                  .filter(Boolean)
                  .join(" ");
                const badge = ocr.badge
                  ? ocr.badge
                  : web
                    ? "ウェブ"
                    : fileGrain && !image
                      ? "全文"
                      : null;
                const label = chipLabel(s);
                return (
                  <span key={g.key} className={chipClass}>
                    {expandable ? (
                      <button
                        type="button"
                        className="chat-source-grain"
                        disabled={blocked}
                        title={
                          fileGrain
                            ? "クリックで段落に戻す"
                            : "クリックでファイル全体を読ませる"
                        }
                        onClick={() => void toggleSourceGrain(s)}
                      >
                        [{n}] {label}
                        {badge ? (
                          <span className="chat-source-badge">{badge}</span>
                        ) : null}
                      </button>
                    ) : (
                      <button
                        type="button"
                        className="chat-source-grain"
                        title={sourceOpenTitle(s)}
                        onClick={() => void openSource(s)}
                      >
                        [{n}] {label}
                        {badge ? (
                          <span className="chat-source-badge">{badge}</span>
                        ) : null}
                      </button>
                    )}
                    {ocr.interrupted ? (
                      <button
                        type="button"
                        className="chat-source-retry"
                        title="再読み取り"
                        aria-label="再読み取り"
                        disabled={blocked}
                        onClick={() => void retryOcr(s.id)}
                      >
                        再試行
                      </button>
                    ) : null}
                    {s.path.trim() ? (
                      <button
                        type="button"
                        className="chat-source-openfile"
                        title={sourceOpenTitle(s)}
                        aria-label={sourceOpenTitle(s)}
                        onClick={() => void openSource(s)}
                      >
                        <IconOpenFile />
                      </button>
                    ) : null}
                    <button
                      type="button"
                      className="chat-source-remove"
                      title="出典を外す"
                      aria-label={`${label}を外す`}
                      disabled={blocked}
                      onClick={() => void removeSource(s.id)}
                    >
                      ×
                    </button>
                  </span>
                );
              })}
            </div>
          ) : null}
          <div className="chat-composer-tools">
            <ChatScopePicker
              disabled={blocked || !active}
              threadId={active?.id ?? null}
              pathPrefix={active?.pathPrefix ?? ""}
              onApplied={applyThreadScope}
              onError={setError}
            />
            <button
              type="button"
              className="chat-tpl"
              title="ファイルを会話に添付"
              disabled={blocked}
              onClick={() => void pickFiles()}
            >
              ファイル添付
            </button>
            <button
              type="button"
              className={`chat-tpl chat-web-toggle${webSearch ? " is-active" : ""}`}
              disabled={busy || !searxngUrl}
              title={
                searxngUrl
                  ? webSearch
                    ? "インデックス検索時に公開ウェブも使う"
                    : "オフ。クリックでウェブ検索を足す"
                  : "設定のローカルLLMで SearXNG の URL を入れてください"
              }
              aria-pressed={webSearch}
              onClick={() => {
                if (!searxngUrl) return;
                setWebSearch((v) => !v);
              }}
            >
              {webSearch ? "ウェブ検索オン" : "ウェブ検索オフ"}
            </button>
            <button
              type="button"
              className="chat-tpl"
              disabled={busy}
              onClick={() => applyTemplate(TPL_SUMMARY)}
            >
              要約
            </button>
            <button
              type="button"
              className="chat-tpl"
              disabled={busy}
              onClick={() => applyTemplate(TPL_POINTS)}
            >
              要点
            </button>
            <button
              type="button"
              className="chat-tpl"
              disabled={busy}
              onClick={() => applyTemplate(TPL_FACTS)}
            >
              要件事実
            </button>
            <button
              type="button"
              className="chat-tpl"
              disabled={busy}
              onClick={() => applyTemplate(TPL_TIMELINE)}
            >
              時系列
            </button>
            <span
              className={overBudget ? "chat-char-meter warn" : "chat-char-meter"}
              title="読込済みの出典本文も履歴の一部として数えます"
            >
              約 {estimatedChars.toLocaleString()} / {maxContextChars.toLocaleString()}{" "}
              文字
              {overBudget ? " · 古い出典から切り詰めます" : ""}
            </span>
          </div>
          <div className="chat-composer-row">
            <textarea
              ref={textareaRef}
              rows={3}
              value={draft}
              placeholder="メッセージを入力（Enter で送信、Shift+Enter で改行）"
              disabled={busy || attaching}
              onChange={(e) => setDraft(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  void send();
                }
              }}
            />
            <div className="chat-composer-actions">
              {busy || ocrBusy ? (
                <button type="button" className="chat-btn" onClick={() => void stop()}>
                  停止
                </button>
              ) : (
                <button
                  type="submit"
                  className="chat-btn primary"
                  disabled={!draft.trim() || attaching}
                >
                  送信
                </button>
              )}
            </div>
          </div>
        </form>
      </main>
    </div>
  );
}
