import {
  Fragment,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ClipboardEvent as ReactClipboardEvent,
  type KeyboardEvent as ReactKeyboardEvent,
  type MouseEvent as ReactMouseEvent,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
  type SyntheticEvent as ReactSyntheticEvent,
} from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import ChatScopePicker from "./ChatScopePicker";
import ChatNotePicker from "./ChatNotePicker";
import PromptLibrary from "./PromptLibrary";
import { isImeEnter } from "./imeEnter";
import { AssistantBody } from "./AssistantBody";
import { OfficeFileCards } from "./OfficeFileCard";
import { HtmlPreviews } from "./HtmlPreview";
import { officeCardsByMessage } from "./officeCards";
import { parseOfficeFileNotice } from "./officeFile";
import { contextMeterLabel, fileGrainConfirmMessage } from "./contextEstimate";
import {
  applyChatDelta,
  followAfterUserScroll,
  isAtBottom,
  messageListKey,
  shouldFollowScrollToBottom,
  type LiveGeneration,
} from "./chatLive";
import { noticeFromSend, SummaryAfterMessage } from "./conversationSummary";
import {
  composerFocusLanded,
  planChatShortcut,
  shortcutInputLocked,
} from "./chatShortcut";
import { useChatContextMeter } from "./useChatContextMeter";
import { editOfficeFileLink } from "./officeOutput";
import { shouldRestoreDraft } from "./refusedSend";
import {
  bytesToBase64,
  chooseClipboardPaste,
  pasteBlockedMessage,
  pastedImageSizeError,
  pastedImageTypeError,
} from "./pasteImage";
import NoteDestPicker from "../notes/NoteDestPicker";
import type { NoteReview } from "../notes/noteReview";
import { openPreview } from "../preview/openPreview";
import { highlightText } from "../search/highlightText";
import {
  fileLabel,
  groupOcrState,
  groupSources,
  imageGroupKey,
  isImageSource,
  isRemoteSource,
  isWebSource,
  ocrIncomplete,
  openableCitesFromGroups,
} from "./sourceGroups";
import "./chat.css";

const USER_SCROLL_TAIL_MS = 300;
const SCROLL_KEYS = new Set([
  "ArrowUp",
  "ArrowDown",
  "ArrowLeft",
  "ArrowRight",
  "PageUp",
  "PageDown",
  "Home",
  "End",
  " ",
]);

type LlmThreadRow = {
  id: string;
  title: string;
  /** Folder scope for index searches in this thread. Empty means the whole index. */
  pathPrefix?: string;
  noteId?: string;
  sortOrder?: number;
  createdAt: number;
  updatedAt: number;
  summary?: string;
  summaryThroughId?: string;
};

type LlmMessageRow = {
  id: string;
  threadId: string;
  role: string;
  content: string;
  createdAt: number;
  reasoning?: string;
  toolCalls?: string;
  toolCallId?: string;
  toolName?: string;
  /** "notice" is shown in the chat and left out of the token estimate. */
  kind?: string;
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

type LlmFileGrainMeter = {
  tokens: number;
  bodyTrimmed: boolean;
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
  llmMaxContextTokens?: number;
  searxngUrl?: string;
  searchMode?: string;
};

type ThinkingBudgetStatus = {
  state: string;
  tokens?: number | null;
};

type LlmSendResult = {
  thread: LlmThreadRow;
  userMessage: LlmMessageRow;
  assistantMessage: LlmMessageRow | null;
  cancelled: boolean;
  error: string | null;
  truncated?: boolean;
  contextTokens?: number;
  thinkingBudgetState?: string;
  thinkingBudgetTokens?: number | null;
  warning?: string | null;
  noteWrites?: { noteId: string; title: string; hasReview: boolean }[];
};

type LlmChatDelta = {
  requestId: string;
  threadId: string;
  text: string;
  kind?: string;
};

type ThreadBanner = {
  error: string;
  notice: string;
  restoreDraft: string | null;
};

function bannerFromResult(
  text: string,
  source: "draft" | "resend",
  result: LlmSendResult,
): ThreadBanner {
  if (result.cancelled) {
    return { error: "生成を停止しました。", notice: "", restoreDraft: null };
  }
  if (result.error) {
    return {
      error: result.error,
      notice: "",
      restoreDraft: source === "draft" && shouldRestoreDraft(result.error) ? text : null,
    };
  }
  const note = noticeFromSend(result.warning, !!result.truncated);
  if (note) return { error: "", notice: note, restoreDraft: null };
  return { error: "", notice: "", restoreDraft: null };
}

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

function attachFollowup(result: LlmAttachFilesResult): { error: string; notice: string } {
  const error = result.errors
    .map((item) => item.message)
    .filter((message) => message.trim())
    .join("\n");
  const notice = [
    result.skipped > 0 ? "同じ出典がすでに読込前にあります" : "",
    result.remoteOcr ? "画像は設定中の LLM サーバへ送られます" : "",
    ...(result.warnings ?? []),
  ]
    .filter(Boolean)
    .join(" · ");
  return { error, notice };
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
  if (isRemoteSource(s)) return false;
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

function citePrefix(n: number): string {
  return n > 0 ? `[${n}] ` : "";
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

function toolCallNames(raw: string | undefined): string[] {
  const text = (raw ?? "").trim();
  if (!text) return [];
  try {
    const parsed = JSON.parse(text) as unknown;
    if (!Array.isArray(parsed)) return [];
    return parsed
      .map((item) => {
        if (!item || typeof item !== "object") return "";
        const row = item as { name?: unknown; function?: { name?: unknown } };
        if (typeof row.function?.name === "string") return row.function.name;
        if (typeof row.name === "string") return row.name;
        return "";
      })
      .filter((name) => name.trim().length > 0);
  } catch {
    return [];
  }
}

function thinkingBudgetLabel(status: ThinkingBudgetStatus): string {
  if (status.state === "set" && typeof status.tokens === "number" && status.tokens > 0) {
    return `思考の上限 ${status.tokens.toLocaleString()} トークン`;
  }
  if (status.state === "unset") return "思考の上限は未設定";
  return "思考の上限は未確認";
}

function matchesListQuery(text: string, query: string): boolean {
  const q = query.trim().normalize("NFKC").toLowerCase();
  if (!q) return true;
  return text.normalize("NFKC").toLowerCase().includes(q);
}

function threadListTitle(t: { title: string }): string {
  return t.title.trim() || "新しい会話";
}

/** HTML5 DnD is blocked while the chat window has Tauri file-drop enabled. */
function reorderIds(
  ids: string[],
  sourceId: string,
  targetId: string,
): string[] | null {
  if (sourceId === targetId) return null;
  const from = ids.indexOf(sourceId);
  const to = ids.indexOf(targetId);
  if (from < 0 || to < 0) return null;
  const next = [...ids];
  next.splice(from, 1);
  next.splice(to, 0, sourceId);
  return next;
}

function threadIdFromPoint(x: number, y: number): string | null {
  const node = document.elementFromPoint(x, y);
  if (!(node instanceof Element)) return null;
  const item = node.closest(".chat-thread-item");
  return item instanceof HTMLElement ? (item.dataset.threadId ?? null) : null;
}

const THREAD_DRAG_THRESHOLD = 4;

type ThreadDragSession = {
  pointerId: number;
  sourceId: string;
  overId: string | null;
  started: boolean;
  startX: number;
  startY: number;
  offsetX: number;
  offsetY: number;
  ghost: HTMLElement | null;
};

function moveThreadDragGhost(ghost: HTMLElement, x: number, y: number) {
  ghost.style.transform = `translate(${Math.round(x)}px, ${Math.round(y)}px)`;
}

function makeThreadDragGhost(
  item: HTMLElement,
  x: number,
  y: number,
): HTMLElement {
  const rect = item.getBoundingClientRect();
  const cs = getComputedStyle(item);
  const ghost = item.cloneNode(true) as HTMLElement;
  ghost.removeAttribute("data-thread-id");
  ghost.classList.add("chat-thread-item--ghost");
  ghost.setAttribute("aria-hidden", "true");
  ghost.style.width = `${rect.width}px`;
  ghost.style.fontSize = cs.fontSize;
  ghost.style.color = cs.color;
  ghost.querySelectorAll("button, input").forEach((el) => {
    el.setAttribute("tabindex", "-1");
  });
  moveThreadDragGhost(ghost, x, y);
  document.body.appendChild(ghost);
  return ghost;
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
  const [live, setLive] = useState<LiveGeneration | null>(null);
  const [followBottom, setFollowBottom] = useState(true);
  const [detailsHold, setDetailsHold] = useState(false);
  const [noteReviews, setNoteReviews] = useState<NoteReview[]>([]);
  const [appendUndo, setAppendUndo] = useState<{
    noteId: string;
    chunk: string;
  } | null>(null);
  const [fontSize, setFontSize] = useState(14);
  const [maxContextTokens, setMaxContextTokens] = useState(131_072);
  const [thinkingBudget, setThinkingBudget] = useState<ThinkingBudgetStatus>({
    state: "unknown",
  });
  const [searxngUrl, setSearxngUrl] = useState("");
  const [searchMode, setSearchMode] = useState("local");
  const [webSearch, setWebSearch] = useState(false);
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState("");
  const [sidebarWidth, setSidebarWidth] = useState(loadSidebarWidth);
  const [resizingSidebar, setResizingSidebar] = useState(false);
  const [threadDragId, setThreadDragId] = useState<string | null>(null);
  const [listQuery, setListQuery] = useState("");
  const [contentHitIds, setContentHitIds] = useState<string[] | null>(null);
  const listRef = useRef<HTMLDivElement | null>(null);
  const logContentRef = useRef<HTMLDivElement | null>(null);
  const rootRef = useRef<HTMLDivElement | null>(null);
  const bootstrappingRef = useRef(false);
  const activeIdRef = useRef<string | null>(null);
  const busyRef = useRef(false);
  const ocrBusyRef = useRef(false);
  const attachingRef = useRef(false);
  const liveRef = useRef<LiveGeneration | null>(null);
  const pendingThreadId = useRef<string | null>(null);
  const contentEpoch = useRef(0);
  const heldBanners = useRef(new Map<string, ThreadBanner>());
  const followRef = useRef(true);
  const showLiveRef = useRef(false);
  const programmaticScroll = useRef(false);
  const scrollAnchor = useRef(0);
  const wasAtBottomRef = useRef(true);
  const detailsHoldRef = useRef(false);
  const userScrollUntilRef = useRef(0);
  const scrollbarPointerRef = useRef(false);
  const liveMessageRef = useRef<HTMLElement | null>(null);
  const liveHeightRef = useRef(0);
  const logObserverRef = useRef<ResizeObserver | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);
  const shortcutFocusPending = useRef(false);
  const openFromShortcutRef = useRef<(threadId: string) => void>(() => {});
  const threadsRef = useRef(threads);
  const messagesRef = useRef(messages);
  const seenMessageKeyRef = useRef("");
  const threadDragRef = useRef<ThreadDragSession | null>(null);
  threadsRef.current = threads;
  messagesRef.current = messages;

  useEffect(() => {
    const id = active?.id ?? null;
    const pending = pendingThreadId.current;
    if (pending && pending !== id) return;
    activeIdRef.current = id;
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

  const focusComposer = useCallback(() => {
    const el = textareaRef.current;
    if (!el || el.disabled) return false;
    el.focus();
    const landed = composerFocusLanded(
      document.hasFocus(),
      document.activeElement,
      el,
    );
    if (landed) shortcutFocusPending.current = false;
    return landed;
  }, []);

  const loadNoteReview = useCallback(async (noteId: string | undefined) => {
    const id = (noteId ?? "").trim();
    if (!id) return;
    try {
      const row = await invoke<NoteReview | null>("get_note_review", {
        noteId: id,
      });
      setNoteReviews((prev) => {
        const rest = prev.filter((r) => r.noteId !== id);
        if (row && row.hasReview) return [...rest, row];
        return rest;
      });
    } catch {
      setNoteReviews((prev) => prev.filter((r) => r.noteId !== id));
    }
  }, []);

  const loadThreadContent = useCallback(async (thread: LlmThreadRow | null): Promise<boolean> => {
    const epoch = ++contentEpoch.current;
    if (!thread) {
      if (contentEpoch.current !== epoch) return false;
      setMessages([]);
      setSources([]);
      setNoteReviews([]);
      return true;
    }
    const [msgs, srcs] = await Promise.all([
      invoke<LlmMessageRow[]>("llm_list_messages", { threadId: thread.id }),
      invoke<LlmSourceRow[]>("llm_list_sources", { threadId: thread.id }),
    ]);
    if (contentEpoch.current !== epoch) return false;
    setMessages(msgs);
    setSources(srcs);
    setNoteReviews([]);
    if (thread.noteId) {
      await loadNoteReview(thread.noteId);
      if (contentEpoch.current !== epoch) return false;
    }
    return true;
  }, [loadNoteReview]);

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
    if (typeof s.llmMaxContextTokens === "number" && s.llmMaxContextTokens > 0) {
      setMaxContextTokens(s.llmMaxContextTokens);
    }
    const url = typeof s.searxngUrl === "string" ? s.searxngUrl.trim() : "";
    setSearxngUrl(url);
    if (!url) setWebSearch(false);
    if (typeof s.searchMode === "string" && s.searchMode.trim()) {
      setSearchMode(s.searchMode);
    }
  }, []);

  const refreshThinkingBudget = useCallback(() => {
    void invoke<ThinkingBudgetStatus>("llm_thinking_budget")
      .then(setThinkingBudget)
      .catch(() => {
        /* keep the last reading */
      });
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
      refreshThinkingBudget();
      await loadThreads();
      setError("");
      try {
        const focus = await invoke<boolean>("consume_chat_shortcut_focus");
        if (focus) focusComposer();
      } catch {
        /* the window is already open; the shortcut event focuses too */
      }
    } catch (e) {
      setError(formatInvokeError(e));
    } finally {
      bootstrappingRef.current = false;
    }
  }, [applySettings, focusComposer, loadThreads, refreshThinkingBudget]);

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
        if (!event.payload) return;
        const idle =
          !busyRef.current && !ocrBusyRef.current && !attachingRef.current;
        if (!idle) {
          if (shortcutFocusPending.current) focusComposer();
          return;
        }
        void bootstrap().finally(() => {
          if (shortcutFocusPending.current) focusComposer();
        });
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
      refreshThinkingBudget();
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
  }, [applySettings, bootstrap, focusComposer, refreshThinkingBudget]);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    void listen<{ threadId?: string }>("chat-shortcut-open", (event) => {
      openFromShortcutRef.current(event.payload?.threadId ?? "");
    }).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    let unlistenDelta: (() => void) | undefined;
    let unlistenError: (() => void) | undefined;
    let unlistenSources: (() => void) | undefined;
    let unlistenOcr: (() => void) | undefined;
    let unlistenReview: (() => void) | undefined;
    void listen<LlmChatDelta>("llm-chat-delta", (event) => {
      const current = liveRef.current;
      if (!current || event.payload.threadId !== current.threadId) return;
      const next = applyChatDelta(current, event.payload);
      if (next === current) return;
      liveRef.current = next;
      setLive(next);
    }).then((fn) => {
      unlistenDelta = fn;
    });
    void listen<{ threadId: string; message: string }>("llm-chat-error", (event) => {
      const tid = event.payload.threadId;
      const generating = liveRef.current?.threadId;
      if (generating && tid !== generating) return;
      if (!generating && tid !== activeIdRef.current) return;
      if (activeIdRef.current === tid) {
        setError(event.payload.message);
        return;
      }
      const prev = heldBanners.current.get(tid);
      heldBanners.current.set(tid, {
        error: event.payload.message,
        notice: prev?.notice ?? "",
        restoreDraft: prev?.restoreDraft ?? null,
      });
    }).then((fn) => {
      unlistenError = fn;
    });
    void listen<{ threadId: string }>("llm-sources-updated", (event) => {
      const tid = event.payload.threadId;
      if (busyRef.current || ocrBusyRef.current || attachingRef.current) {
        if (tid === activeIdRef.current) {
          void invoke<LlmSourceRow[]>("llm_list_sources", { threadId: tid })
            .then((rows) => {
              if (activeIdRef.current !== tid) return;
              setSources(rows);
            })
            .catch((e) => {
              if (activeIdRef.current === tid) setError(formatInvokeError(e));
            });
        }
        return;
      }
      const prev = activeIdRef.current;
      void loadThreads()
        .then(() => {
          if (tid !== prev) {
            setDraft("");
            liveRef.current = null;
            setLive(null);
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
    void listen<{ noteId?: string }>("llm-note-review", (event) => {
      const nid = event.payload?.noteId ?? "";
      if (nid) void loadNoteReview(nid);
    }).then((fn) => {
      unlistenReview = fn;
    });
    return () => {
      unlistenDelta?.();
      unlistenError?.();
      unlistenSources?.();
      unlistenOcr?.();
      unlistenReview?.();
    };
  }, [loadThreads, loadNoteReview]);

  const viewingLive = live && active?.id === live.threadId ? live : null;
  const stream = viewingLive?.stream ?? "";
  const thinking = viewingLive?.thinking ?? "";
  const toolHint = viewingLive?.toolHint ?? "";
  const status = viewingLive?.status ?? "";
  const showLive =
    !!viewingLive &&
    (busy ||
      stream.length > 0 ||
      thinking.length > 0 ||
      toolHint.length > 0 ||
      status.length > 0);
  showLiveRef.current = showLive;
  useEffect(() => {
    if (showLive) return;
    listRef.current?.classList.remove("chat-log-no-anchor");
  }, [showLive]);
  const officeCards = useMemo(() => officeCardsByMessage(messages), [messages]);
  const generatingThreadId = busy ? (live?.threadId ?? null) : null;
  const onGeneratingThread = !!generatingThreadId && active?.id === generatingThreadId;
  const inputLocked = shortcutInputLocked(attaching, onGeneratingThread);
  useEffect(() => {
    if (inputLocked || !shortcutFocusPending.current) return;
    focusComposer();
  }, [inputLocked, focusComposer]);

  const noteProgrammaticScroll = useCallback(() => {
    programmaticScroll.current = true;
    queueMicrotask(() => {
      programmaticScroll.current = false;
    });
  }, []);

  const pinLogToBottom = useCallback(() => {
    const el = listRef.current;
    if (!el || !followRef.current) return;
    noteProgrammaticScroll();
    el.scrollTop = el.scrollHeight;
    scrollAnchor.current = el.scrollTop;
    wasAtBottomRef.current = true;
  }, [noteProgrammaticScroll]);

  const releaseDetailsHold = useCallback(() => {
    if (!detailsHoldRef.current) return;
    detailsHoldRef.current = false;
    listRef.current?.classList.remove("chat-log-no-anchor");
    setDetailsHold(false);
  }, []);

  const pauseFollow = useCallback(() => {
    if (!followRef.current) return;
    followRef.current = false;
    wasAtBottomRef.current = false;
    // Class is applied before the browser clamps or anchors this frame.
    detailsHoldRef.current = true;
    if (showLiveRef.current) listRef.current?.classList.add("chat-log-no-anchor");
    setDetailsHold(true);
    setFollowBottom(false);
  }, []);

  const scrollLog = useCallback((layout = false) => {
    const el = listRef.current;
    const key = messageListKey(messagesRef.current.map((m) => m.id));
    const listChanged = key !== seenMessageKeyRef.current;
    seenMessageKeyRef.current = key;
    if (!el || !followRef.current) return;
    // Layout runs after the DOM has already grown, so the pre-growth flag is the one that matters.
    const atBottom = layout
      ? wasAtBottomRef.current
      : isAtBottom(el.scrollTop, el.scrollHeight, el.clientHeight);
    if (
      !shouldFollowScrollToBottom({
        live: showLiveRef.current && !layout,
        listChanged,
        atBottom,
      })
    ) {
      return;
    }
    pinLogToBottom();
  }, [pinLogToBottom]);

  const scrollAfterLayout = useCallback(() => {
    scrollLog(true);
  }, [scrollLog]);

  const setLiveMessageEl = useCallback((node: HTMLElement | null) => {
    const observer = logObserverRef.current;
    const prev = liveMessageRef.current;
    if (prev && prev !== node && observer) observer.unobserve(prev);
    liveMessageRef.current = node;
    // The article often mounts after the observer. Snapshot its height now, then
    // keep watching it: a short log stretches `.chat-log-content`, so token growth
    // does not resize that box and would otherwise leave this height stale.
    liveHeightRef.current = node?.offsetHeight ?? 0;
    if (node && observer) observer.observe(node);
  }, []);

  useEffect(() => {
    const log = listRef.current;
    if (!log) return;
    const outsideLiveDetails = (details: Element | null) => {
      if (!(details instanceof HTMLDetailsElement)) return false;
      const live = liveMessageRef.current;
      return !live || !live.contains(details);
    };
    // Click is synchronous, before the details box grows. Toggle is the same
    // open/close for keyboard; both must pause before ResizeObserver can pin.
    const onClick = (event: MouseEvent) => {
      if (!showLiveRef.current || !followRef.current) return;
      const target = event.target;
      if (!(target instanceof Element)) return;
      const summary = target.closest("summary");
      if (!summary || !outsideLiveDetails(summary.closest("details"))) return;
      pauseFollow();
    };
    const onToggle = (event: Event) => {
      if (!showLiveRef.current || !followRef.current) return;
      const details = event.target;
      if (!(details instanceof Element) || !outsideLiveDetails(details)) return;
      pauseFollow();
    };
    log.addEventListener("click", onClick);
    log.addEventListener("toggle", onToggle, true);
    return () => {
      log.removeEventListener("click", onClick);
      log.removeEventListener("toggle", onToggle, true);
    };
  }, [pauseFollow]);

  useEffect(() => {
    const content = logContentRef.current;
    const log = listRef.current;
    if (!content || !log || typeof ResizeObserver === "undefined") return;
    let lastContentHeight = content.offsetHeight;
    let lastClientHeight = log.clientHeight;
    let lastClientWidth = log.clientWidth;
    let lastScrollHeight = log.scrollHeight;
    const observer = new ResizeObserver(() => {
      const contentHeight = content.offsetHeight;
      const clientHeight = log.clientHeight;
      const clientWidth = log.clientWidth;
      const scrollHeight = log.scrollHeight;
      const contentGrew = contentHeight > lastContentHeight;
      const scrollGrew = scrollHeight > lastScrollHeight;
      const widthChanged = clientWidth !== lastClientWidth;
      const viewportChanged = clientHeight !== lastClientHeight;
      const viewportShrank = clientHeight < lastClientHeight;
      const wasGeoBottom = isAtBottom(log.scrollTop, lastScrollHeight, lastClientHeight);
      const liveEl = liveMessageRef.current;
      const liveHeight = liveEl?.offsetHeight ?? 0;
      const liveGrew = liveEl != null && liveHeight > liveHeightRef.current;
      lastContentHeight = contentHeight;
      lastClientHeight = clientHeight;
      lastClientWidth = clientWidth;
      lastScrollHeight = scrollHeight;
      liveHeightRef.current = liveHeight;
      if (showLiveRef.current) {
        if (!followRef.current) return;
        // Details outside the live message pause in the click/toggle handler.
        // Diagram layout and width reflow stay pinned while the user is following.
        if (
          wasAtBottomRef.current &&
          (liveGrew || contentGrew || scrollGrew || widthChanged)
        ) {
          pinLogToBottom();
          return;
        }
        if (viewportShrank && wasGeoBottom) pinLogToBottom();
        return;
      }
      if (!followRef.current) return;
      if (wasGeoBottom && (widthChanged || viewportChanged)) {
        pinLogToBottom();
        return;
      }
      if (contentGrew || scrollGrew) {
        wasAtBottomRef.current = isAtBottom(log.scrollTop, scrollHeight, clientHeight);
      }
    });
    logObserverRef.current = observer;
    observer.observe(content);
    observer.observe(log);
    if (liveMessageRef.current) observer.observe(liveMessageRef.current);
    return () => {
      observer.disconnect();
      if (logObserverRef.current === observer) logObserverRef.current = null;
    };
  }, [pauseFollow, pinLogToBottom, scrollLog]);

  const userScrollActive = useCallback(() => {
    return scrollbarPointerRef.current || performance.now() < userScrollUntilRef.current;
  }, []);

  const noteUserScroll = useCallback((event: ReactSyntheticEvent) => {
    if (event.type === "pointerdown") {
      if (event.target !== event.currentTarget) return;
      scrollbarPointerRef.current = true;
    } else if (event.type === "pointerup" || event.type === "pointercancel") {
      const wasScrollbar = scrollbarPointerRef.current;
      scrollbarPointerRef.current = false;
      if (!wasScrollbar) return;
    } else if (event.type === "pointermove") {
      if (!scrollbarPointerRef.current) return;
    } else if (event.type === "touchmove") {
      const target = event.target;
      if (target instanceof Element && target.closest("summary")) return;
    } else if (event.type === "keydown") {
      const key = (event as ReactKeyboardEvent).key;
      if (!SCROLL_KEYS.has(key)) return;
    }
    userScrollUntilRef.current = performance.now() + USER_SCROLL_TAIL_MS;
  }, []);

  const onLogScroll = useCallback(() => {
    const el = listRef.current;
    if (!el || programmaticScroll.current) return;
    const atBottom = isAtBottom(el.scrollTop, el.scrollHeight, el.clientHeight);
    // A details close clamps onto the bottom, and later growth can anchor there.
    // Neither is a user scroll, so they must not resume following.
    if (detailsHoldRef.current && !(userScrollActive() && atBottom)) {
      scrollAnchor.current = el.scrollTop;
      wasAtBottomRef.current = false;
      return;
    }
    const next = followAfterUserScroll({
      follow: followRef.current,
      scrollTop: el.scrollTop,
      anchor: scrollAnchor.current,
      scrollHeight: el.scrollHeight,
      clientHeight: el.clientHeight,
    });
    scrollAnchor.current = next.anchor;
    wasAtBottomRef.current = atBottom;
    if (next.follow) releaseDetailsHold();
    if (followRef.current === next.follow) return;
    followRef.current = next.follow;
    setFollowBottom(next.follow);
  }, [releaseDetailsHold, userScrollActive]);

  function pinFollow() {
    followRef.current = true;
    releaseDetailsHold();
    setFollowBottom(true);
  }

  function jumpToLatest() {
    pinFollow();
    const el = listRef.current;
    if (!el) return;
    noteProgrammaticScroll();
    el.scrollTop = el.scrollHeight;
    scrollAnchor.current = el.scrollTop;
    wasAtBottomRef.current = true;
  }

  useEffect(() => {
    scrollLog();
  }, [messages, stream, thinking, toolHint, showLive, scrollLog]);

  const meter = useChatContextMeter({
    threadId: active?.id ?? "",
    noteId: active?.noteId ?? "",
    pathPrefix: active?.pathPrefix ?? "",
    draft,
    webSearch: webSearch && !!searxngUrl,
    maxContextTokens,
    searchMode,
    messages,
    sources,
  });

  const pendingSources = useMemo(
    () => sources.filter(isPendingSource),
    [sources],
  );

  const liveCites = useMemo(() => {
    const ready = pendingSources.filter((s) => !ocrIncomplete(s));
    const tools = sources.filter(isUncitedToolSource);
    return [...ready, ...tools];
  }, [pendingSources, sources]);

  const pendingGroups = useMemo(
    () => groupSources(pendingSources),
    [pendingSources],
  );

  const liveCiteGroups = useMemo(
    () => groupSources(liveCites),
    [liveCites],
  );

  const liveCiteInfo = useMemo(
    () => openableCitesFromGroups(liveCiteGroups),
    [liveCiteGroups],
  );

  const nearBudget = meter.warn;
  const blocked = busy || ocrBusy || attaching;

  async function selectThread(id: string) {
    // Generation keeps running. OCR and attach still belong to the open thread.
    if (ocrBusy || attaching || !id || id === activeIdRef.current) return;
    const prev = activeIdRef.current;
    pendingThreadId.current = id;
    activeIdRef.current = id;
    pinFollow();
    try {
      const t = await invoke<LlmThreadRow>("llm_set_active_thread", { id });
      if (activeIdRef.current !== id) {
        if (pendingThreadId.current === id) pendingThreadId.current = null;
        return;
      }
      setActive(t);
      pendingThreadId.current = null;
      const applied = await loadThreadContent(t);
      if (activeIdRef.current !== id) return;
      const held = heldBanners.current.get(id) ?? null;
      if (held) heldBanners.current.delete(id);
      if (!applied && !held) return;
      if (held) {
        setError(held.error);
        setNotice(held.notice);
        if (held.restoreDraft) setDraft(held.restoreDraft);
        return;
      }
      setError("");
      setNotice("");
    } catch (e) {
      if (activeIdRef.current === id) {
        activeIdRef.current = prev;
        pendingThreadId.current = null;
        setError(formatInvokeError(e));
      } else if (pendingThreadId.current === id) {
        pendingThreadId.current = null;
      }
    }
  }

  async function createThread() {
    if (blocked) return;
    try {
      const t = await invoke<LlmThreadRow>("llm_create_thread", { title: "" });
      setThreads((prev) => [t, ...prev.filter((x) => x.id !== t.id)]);
      contentEpoch.current += 1;
      setActive(t);
      activeIdRef.current = t.id;
      setMessages([]);
      setSources([]);
      liveRef.current = null;
      setLive(null);
      pinFollow();
      setError("");
      setNotice("");
      textareaRef.current?.focus();
    } catch (e) {
      setError(formatInvokeError(e));
    }
  }

  openFromShortcutRef.current = (threadId: string) => {
    shortcutFocusPending.current = true;
    void invoke("consume_chat_shortcut_focus").catch(() => {
      /* a later focus of this window can still place the caret */
    });
    void (async () => {
      const plan = planChatShortcut({
        threadId,
        activeId: activeIdRef.current,
        knownThreadIds: threadsRef.current.map((t) => t.id),
      });
      try {
        if (plan.action === "select") await selectThread(plan.threadId);
        else if (plan.action === "reload") await loadThreads();
        else if (plan.action === "create") await createThread();
      } catch (e) {
        setError(formatInvokeError(e));
      }
      if (!focusComposer()) {
        requestAnimationFrame(() => {
          if (!focusComposer()) window.setTimeout(() => focusComposer(), 50);
        });
      }
    })();
  };

  async function deleteThread(id: string) {
    if (blocked) return;
    try {
      await invoke("llm_delete_thread", { id });
      if (liveRef.current?.threadId === id) {
        liveRef.current = null;
        setLive(null);
      }
      heldBanners.current.delete(id);
      await loadThreads();
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

  const applyThreadNote = useCallback((t: LlmThreadRow) => {
    setThreads((prev) => prev.map((x) => (x.id === t.id ? { ...x, ...t } : x)));
    setActive((prev) => (prev?.id === t.id ? { ...prev, ...t } : prev));
    if (t.noteId) void loadNoteReview(t.noteId);
  }, [loadNoteReview]);

  async function appendAnswerToNote(content: string, noteId: "new" | string) {
    const text = content.trim();
    if (!text) {
      setError("追記する本文が空です。");
      return;
    }
    try {
      const result = await invoke<{
        note: { id: string; title: string };
        chunk: string;
      }>("append_note_memo", {
        id: noteId === "new" ? "new" : noteId,
        chunk: text,
      });
      setAppendUndo({ noteId: result.note.id, chunk: result.chunk });
      setNotice(
        `ノート『${result.note.title.trim() || "無題のノート"}』に追記しました`,
      );
    } catch (e) {
      setError(formatInvokeError(e));
    }
  }

  async function undoAppendToNote() {
    if (!appendUndo) return;
    try {
      await invoke("undo_append_note_memo", {
        id: appendUndo.noteId,
        chunk: appendUndo.chunk,
      });
      setAppendUndo(null);
      setNotice("追記を取り消しました");
    } catch (e) {
      setError(formatInvokeError(e));
    }
  }

  async function openReviewedNote(noteId: string) {
    try {
      await invoke("set_active_note", { id: noteId });
      await invoke("show_notes_window");
    } catch (e) {
      setError(formatInvokeError(e));
    }
  }

  async function openSavedFile(name: string) {
    try {
      await invoke("open_output_file", { name });
    } catch (e) {
      setError(formatInvokeError(e));
    }
  }

  async function openHtmlLarge(name: string) {
    try {
      await invoke("show_html_preview", { name });
    } catch (e) {
      setError(formatInvokeError(e));
    }
  }

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
        source: isRemoteSource(s) ? "remote" : undefined,
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
      // Reject a missing or oversized file before asking.
      await invoke<LlmFilePreview>("llm_preview_source_file", {
        id: s.id,
      });
      const measured = await invoke<LlmFileGrainMeter>("llm_file_grain_meter", {
        threadId: active?.id || s.threadId,
        sourceId: s.id,
        draft,
        webSearch: webSearch && !!searxngUrl,
      });
      if (measured.bodyTrimmed) {
        const ok = window.confirm(
          fileGrainConfirmMessage(measured.tokens, maxContextTokens),
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
      const follow = attachFollowup(result);
      if (follow.error) setError(follow.error);
      if (follow.notice) setNotice(follow.notice);
    } catch (e) {
      setError(formatInvokeError(e));
    } finally {
      setAttaching(false);
    }
  }, []);

  const pasteClipboardImage = useCallback(
    async (event: ReactClipboardEvent<HTMLTextAreaElement>) => {
      const decision = chooseClipboardPaste(event.clipboardData);
      if (decision.kind !== "image") return;
      event.preventDefault();
      const blocked = pasteBlockedMessage(
        busyRef.current,
        ocrBusyRef.current || attachingRef.current,
      );
      if (blocked) {
        setNotice("");
        setError(blocked);
        return;
      }
      setError("");
      setNotice("");
      setAttaching(true);
      const problems: string[] = [];
      let last: LlmAttachFilesResult | null = null;
      let threadId = activeIdRef.current ?? undefined;
      try {
        for (const image of decision.files) {
          const typeError = pastedImageTypeError(image.type);
          if (typeError) {
            problems.push(typeError);
            continue;
          }
          const sizeError = pastedImageSizeError(image.size);
          if (sizeError) {
            problems.push(sizeError);
            continue;
          }
          const bytes = new Uint8Array(await image.arrayBuffer());
          const readError = pastedImageSizeError(bytes.byteLength);
          if (readError) {
            problems.push(readError);
            continue;
          }
          const result = await invoke<LlmAttachFilesResult>("llm_attach_pasted_image", {
            dataBase64: bytesToBase64(bytes),
            mime: image.type,
            threadId,
          });
          last = result;
          threadId = result.thread.id;
          for (const item of result.errors) {
            if (item.message.trim()) problems.push(item.message);
          }
        }
      } catch (e) {
        problems.push(formatInvokeError(e));
      } finally {
        const attached = last;
        if (attached) {
          setActive(attached.thread);
          activeIdRef.current = attached.thread.id;
          setThreads((prev) => {
            const rest = prev.filter((item) => item.id !== attached.thread.id);
            return [attached.thread, ...rest];
          });
          setSources(attached.sources);
          const follow = attachFollowup({ ...attached, errors: [] });
          if (follow.notice) setNotice(follow.notice);
        }
        if (problems.length) setError(problems.join("\n"));
        setAttaching(false);
      }
    },
    [],
  );

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

  function publishBanner(threadId: string, banner: ThreadBanner) {
    if (activeIdRef.current === threadId) {
      heldBanners.current.delete(threadId);
      setError(banner.error);
      setNotice(banner.notice);
      if (banner.restoreDraft) setDraft(banner.restoreDraft);
      return;
    }
    if (banner.error || banner.notice || banner.restoreDraft) {
      heldBanners.current.set(threadId, banner);
    } else {
      heldBanners.current.delete(threadId);
    }
  }

  async function copyMessage(text: string) {
    try {
      await writeText(text);
    } catch (e) {
      setError(formatInvokeError(e));
    }
  }

  async function send(raw?: string, source: "draft" | "resend" = "draft") {
    const text = (source === "draft" ? draft : (raw ?? "")).trim();
    if (!text || blocked) return;
    busyRef.current = true;
    setBusy(true);
    setError("");
    setNotice("");
    if (source === "draft") setDraft("");
    pinFollow();
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
      busyRef.current = false;
      setBusy(false);
      if (source === "draft") setDraft(text);
      setError(formatInvokeError(e));
      return;
    }
    const originId = thread.id;
    const started: LiveGeneration = {
      threadId: originId,
      stream: "",
      thinking: "",
      toolHint: "",
      status: "",
    };
    liveRef.current = started;
    setLive(started);
    const optimistic: LlmMessageRow = {
      id: `tmp-${Date.now()}`,
      threadId: originId,
      role: "user",
      content: text,
      createdAt: Math.floor(Date.now() / 1000),
    };
    // The ref is written in an effect, so a click on the first painted frame can
    // still see null while `active` already names this thread.
    const viewingId = activeIdRef.current ?? active?.id ?? null;
    if (viewingId === originId) {
      if (activeIdRef.current == null) activeIdRef.current = originId;
      // Drop a thread load that is still in flight so it cannot erase this row.
      contentEpoch.current += 1;
      setMessages((prev) => [...prev, optimistic]);
    }
    try {
      const result = await invoke<LlmSendResult>("llm_send", {
        threadId: originId,
        content: text,
        webSearch: webSearch && !!searxngUrl,
      });
      const finishedId = result.thread.id;
      setThreads((prev) => prev.map((x) => (x.id === finishedId ? result.thread : x)));
      if (result.thinkingBudgetState) {
        setThinkingBudget({
          state: result.thinkingBudgetState,
          tokens: result.thinkingBudgetTokens ?? null,
        });
      }
      const [msgs, srcs] = await Promise.all([
        invoke<LlmMessageRow[]>("llm_list_messages", { threadId: finishedId }),
        invoke<LlmSourceRow[]>("llm_list_sources", { threadId: finishedId }),
      ]);
      for (const w of result.noteWrites ?? []) {
        if (w.noteId) await loadNoteReview(w.noteId);
      }
      if (result.thread.noteId) await loadNoteReview(result.thread.noteId);
      if (activeIdRef.current === finishedId) {
        contentEpoch.current += 1;
        setActive(result.thread);
        setMessages(msgs);
        setSources(srcs);
      }
      publishBanner(finishedId, bannerFromResult(text, source, result));
    } catch (e) {
      const msg = formatInvokeError(e);
      if (msg.includes("生成中です")) {
        try {
          await invoke("llm_cancel");
        } catch {
          /* ignore */
        }
        publishBanner(originId, {
          error: "前の生成が途中で止まっていました。停止したので、もう一度送信してください。",
          notice: "",
          restoreDraft: source === "draft" ? text : null,
        });
      } else {
        publishBanner(originId, { error: msg, notice: "", restoreDraft: null });
      }
      if (activeIdRef.current === originId) {
        try {
          const msgs = await invoke<LlmMessageRow[]>("llm_list_messages", {
            threadId: originId,
          });
          if (activeIdRef.current === originId) {
            contentEpoch.current += 1;
            setMessages(msgs);
          }
        } catch {
          /* keep optimistic */
        }
      }
    } finally {
      if (liveRef.current?.threadId === originId) {
        liveRef.current = null;
        setLive(null);
      }
      busyRef.current = false;
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

  const clearThreadDrag = useCallback(() => {
    const drag = threadDragRef.current;
    threadDragRef.current = null;
    drag?.ghost?.remove();
    setThreadDragId(null);
  }, []);

  const commitThreadReorder = useCallback(
    async (sourceId: string, targetId: string) => {
      const next = reorderIds(
        threadsRef.current.map((t) => t.id),
        sourceId,
        targetId,
      );
      if (!next) return;
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
    [loadThreads],
  );

  const onThreadHandlePointerDown = useCallback(
    (e: ReactPointerEvent<HTMLSpanElement>, id: string) => {
      if (e.button !== 0) return;
      e.preventDefault();
      e.stopPropagation();
      const item = e.currentTarget.closest(".chat-thread-item");
      if (!(item instanceof HTMLElement)) return;
      const rect = item.getBoundingClientRect();
      e.currentTarget.setPointerCapture(e.pointerId);
      threadDragRef.current = {
        pointerId: e.pointerId,
        sourceId: id,
        overId: id,
        started: false,
        startX: e.clientX,
        startY: e.clientY,
        offsetX: e.clientX - rect.left,
        offsetY: e.clientY - rect.top,
        ghost: null,
      };
    },
    [],
  );

  const onThreadHandlePointerMove = useCallback(
    (e: ReactPointerEvent<HTMLSpanElement>) => {
      const drag = threadDragRef.current;
      if (!drag || drag.pointerId !== e.pointerId) return;
      if (!drag.started) {
        if (
          Math.hypot(e.clientX - drag.startX, e.clientY - drag.startY) <
          THREAD_DRAG_THRESHOLD
        ) {
          return;
        }
        const item = e.currentTarget.closest(".chat-thread-item");
        if (!(item instanceof HTMLElement)) return;
        drag.started = true;
        drag.ghost = makeThreadDragGhost(
          item,
          e.clientX - drag.offsetX,
          e.clientY - drag.offsetY,
        );
        setThreadDragId(drag.sourceId);
      }
      if (drag.ghost) {
        moveThreadDragGhost(
          drag.ghost,
          e.clientX - drag.offsetX,
          e.clientY - drag.offsetY,
        );
      }
      const body = e.currentTarget.closest(".chat-sidebar-body");
      if (body instanceof HTMLElement) {
        const rect = body.getBoundingClientRect();
        const margin = 36;
        if (e.clientY < rect.top + margin) body.scrollBy(0, -14);
        else if (e.clientY > rect.bottom - margin) body.scrollBy(0, 14);
      }
      drag.overId = threadIdFromPoint(e.clientX, e.clientY);
    },
    [],
  );

  const onThreadHandlePointerUp = useCallback(
    (e: ReactPointerEvent<HTMLSpanElement>) => {
      const drag = threadDragRef.current;
      if (!drag || drag.pointerId !== e.pointerId) return;
      if (e.currentTarget.hasPointerCapture(e.pointerId)) {
        e.currentTarget.releasePointerCapture(e.pointerId);
      }
      const started = drag.started;
      const sourceId = drag.sourceId;
      const targetId = threadIdFromPoint(e.clientX, e.clientY) ?? drag.overId;
      clearThreadDrag();
      if (started && sourceId && targetId) {
        void commitThreadReorder(sourceId, targetId);
      }
    },
    [clearThreadDrag, commitThreadReorder],
  );

  const onThreadHandlePointerCancel = useCallback(
    (e: ReactPointerEvent<HTMLSpanElement>) => {
      const drag = threadDragRef.current;
      if (!drag || drag.pointerId !== e.pointerId) return;
      if (e.currentTarget.hasPointerCapture(e.pointerId)) {
        e.currentTarget.releasePointerCapture(e.pointerId);
      }
      clearThreadDrag();
    },
    [clearThreadDrag],
  );

  useEffect(() => {
    if (!threadDragId) return;
    const onKey = (ev: KeyboardEvent) => {
      if (ev.key !== "Escape") return;
      ev.preventDefault();
      clearThreadDrag();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [threadDragId, clearThreadDrag]);

  useEffect(() => () => threadDragRef.current?.ghost?.remove(), []);

  return (
    <div
      ref={rootRef}
      className={[
        "chat",
        resizingSidebar ? "chat--resizing" : "",
        threadDragId ? "chat--reordering" : "",
      ]
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
                  data-thread-id={t.id}
                  className={[
                    t.id === active?.id ? "chat-thread-item active" : "chat-thread-item",
                    threadDragId === t.id ? "chat-thread-item--dragging" : "",
                    generatingThreadId === t.id ? "chat-thread-item--generating" : "",
                  ]
                    .filter(Boolean)
                    .join(" ")}
                >
                  <span
                    className="chat-thread-handle"
                    title="ドラッグで並べ替え"
                    aria-hidden="true"
                    onPointerDown={(e) => onThreadHandlePointerDown(e, t.id)}
                    onPointerMove={onThreadHandlePointerMove}
                    onPointerUp={onThreadHandlePointerUp}
                    onPointerCancel={onThreadHandlePointerCancel}
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
                          if (isImeEnter(e)) return;
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
                  {generatingThreadId === t.id ? (
                    <span className="chat-thread-generating">生成中</span>
                  ) : null}
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
        <div className="chat-log-wrap">
        <div
          className={detailsHold && showLive ? "chat-log chat-log-no-anchor" : "chat-log"}
          ref={listRef}
          onScroll={onLogScroll}
          onWheel={noteUserScroll}
          onPointerDown={noteUserScroll}
          onPointerMove={noteUserScroll}
          onPointerUp={noteUserScroll}
          onPointerCancel={noteUserScroll}
          onTouchMove={noteUserScroll}
          onKeyDown={noteUserScroll}
        >
          <div className="chat-log-content" ref={logContentRef}>
          {messages.length === 0 && !showLive ? (
            <p className="chat-muted chat-empty">
              検索やノートの「チャット」から出典を送るか、ファイルを添付して会話できます。出典なしでも、設定の「ローカルLLM」に接続して会話できます。足りない資料はモデルがインデックスを検索することもあります。
            </p>
          ) : (
            messages.map((m) => {
              const cites =
                m.role === "assistant" ? citedForMessage(sources, m.id) : [];
              const citeGroups = groupSources(cites);
              const citeInfo = openableCitesFromGroups(citeGroups);
              const calls = toolCallNames(m.toolCalls);
              const office =
                m.role === "tool" ? editOfficeFileLink(m.toolName, m.content) : null;
              const isNotice = m.kind === "notice";
              const officeFile = isNotice ? parseOfficeFileNotice(m.content) : null;
              const cards = officeCards.get(m.id) ?? [];
              const roleClass =
                m.role === "user"
                  ? "chat-msg user"
                  : m.role === "tool"
                    ? "chat-msg tool"
                    : isNotice
                      ? "chat-msg assistant notice"
                      : "chat-msg assistant";
              return (
                <Fragment key={m.id}>
                <article className={roleClass}>
                  {m.role === "assistant" && (m.reasoning ?? "").trim() ? (
                    <details className="chat-thinking">
                      <summary>思考</summary>
                      <div className="chat-thinking-body">{m.reasoning}</div>
                    </details>
                  ) : null}
                  {m.role === "tool" ? (
                    <>
                      <details className="chat-thinking">
                        <summary>{(m.toolName ?? "").trim() || "操作"} の結果</summary>
                        <div className="chat-thinking-body">{m.content}</div>
                      </details>
                      {office && (cards.length > 0 || office.leftovers.length > 0) ? (
                        <div className="chat-office-out">
                          {office.leftovers.length > 0 ? (
                            <div className="chat-office-left">
                              {office.leftovers.map((line, index) => (
                                <div key={`${index}:${line}`}>{line}</div>
                              ))}
                            </div>
                          ) : null}
                          {cards.length > 0 ? (
                            <OfficeFileCards files={cards} onOpen={openSavedFile} />
                          ) : null}
                          <HtmlPreviews files={cards} onExpand={(name) => void openHtmlLarge(name)} />
                        </div>
                      ) : null}
                    </>
                  ) : m.role === "assistant" ? (
                    <>
                      {calls.length > 0 ? (
                        <p className="chat-tool-hint">{calls.join("、")}</p>
                      ) : null}
                      {officeFile ? (
                        <div className="chat-office-file">
                          <span>{officeFile.label}</span>
                        </div>
                      ) : m.content.trim() ? (
                        <AssistantBody
                          text={m.content}
                          citeNos={citeInfo.nos}
                          onCite={(n) => {
                            const s = citeInfo.byNo.get(n);
                            if (s) void openSource(s);
                          }}
                          onChoice={applyTemplate}
                          choicesDisabled={blocked}
                          onLayout={scrollAfterLayout}
                        />
                      ) : null}
                      {cards.length > 0 ? (
                        <OfficeFileCards files={cards} onOpen={openSavedFile} />
                      ) : null}
                      <HtmlPreviews files={cards} onExpand={(name) => void openHtmlLarge(name)} />
                    </>
                  ) : (
                    <div className="chat-msg-body">{m.content}</div>
                  )}
                  {m.role === "user" ? (
                    <div className="chat-msg-actions">
                      <button
                        type="button"
                        className="chat-tpl"
                        disabled={blocked}
                        onClick={() => void send(m.content, "resend")}
                      >
                        再送
                      </button>
                      <button
                        type="button"
                        className="chat-tpl"
                        onClick={() => void copyMessage(m.content)}
                      >
                        コピー
                      </button>
                    </div>
                  ) : null}
                  {citeGroups.length > 0 ? (
                    <div className="chat-msg-cites" aria-label="出典">
                      {citeGroups.map((g) => {
                        const s = g.representative;
                        const path = s.path.trim();
                        if (!path) {
                          return (
                            <span key={g.key} className="chat-cite-badge">
                              {citePrefix(g.citeNo)}{chipLabel(s)}
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
                            {citePrefix(g.citeNo)}{chipLabel(s)}
                          </button>
                        );
                      })}
                    </div>
                  ) : null}
                  {m.role === "assistant" && !isNotice && m.content.trim() ? (
                    <div className="chat-msg-actions">
                      <NoteDestPicker
                        buttonClassName="chat-tpl"
                        title="この回答をノートのメモ末尾へ追記"
                        disabled={blocked}
                        onPick={(id) => void appendAnswerToNote(m.content, id)}
                      >
                        ノートへ
                      </NoteDestPicker>
                    </div>
                  ) : null}
                </article>
                <SummaryAfterMessage
                  messageId={m.id}
                  throughId={active?.summaryThroughId ?? ""}
                  summary={active?.summary ?? ""}
                />
                </Fragment>
              );
            })
          )}
          {showLive ? (
            <article className="chat-msg assistant streaming" ref={setLiveMessageEl}>
              {status ? <p className="chat-tool-hint">{status}</p> : null}
              {toolHint ? <p className="chat-tool-hint">{toolHint}</p> : null}
              {thinking ? (
                <details className="chat-thinking">
                  <summary>{busy ? "思考中（表示する）" : "思考"}</summary>
                  <div className="chat-thinking-body">{thinking}</div>
                </details>
              ) : null}
              <AssistantBody
                text={stream || (!thinking && !toolHint && !status ? "生成中…" : "")}
                citeNos={liveCiteInfo.nos}
                onCite={(n) => {
                  const s = liveCiteInfo.byNo.get(n);
                  if (s) void openSource(s);
                }}
                onChoice={applyTemplate}
                choicesDisabled
                streaming={!!stream}
                showCaret={!!stream || (!thinking && !toolHint && !status)}
                onLayout={scrollAfterLayout}
              />
              {liveCiteGroups.length > 0 ? (
                <div className="chat-msg-cites" aria-label="今回の出典">
                  {liveCiteGroups.map((g) => {
                    const s = g.representative;
                    const path = s.path.trim();
                    if (!path) {
                      return (
                        <span key={g.key} className="chat-cite-badge">
                          {citePrefix(g.citeNo)}{chipLabel(s)}
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
                        {citePrefix(g.citeNo)}{chipLabel(s)}
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
        </div>
        {!followBottom ? (
          <button type="button" className="chat-jump-latest" onClick={jumpToLatest}>
            ↓最新へ
          </button>
        ) : null}
        </div>
        {error ? <p className="chat-error">{error}</p> : null}
        {noteReviews.map((r) => (
          <p key={r.noteId} className="chat-notice">
            ノート『{r.noteTitle.trim() || "無題"}』を更新しました
            {" "}
            <button
              type="button"
              className="chat-notice-undo"
              onClick={() => void openReviewedNote(r.noteId)}
            >
              ノートで開く
            </button>
          </p>
        ))}
        {notice ? (
          <p className="chat-notice">
            {notice}
            {appendUndo ? (
              <>
                {" "}
                <button
                  type="button"
                  className="chat-notice-undo"
                  onClick={() => void undoAppendToNote()}
                >
                  取り消し
                </button>
              </>
            ) : null}
          </p>
        ) : null}
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
                const remote = isRemoteSource(s);
                const chipClass = [
                  "chat-source-chip",
                  fileGrain || image ? "file" : "",
                  ocr.interrupted ? "ocr-error" : "",
                ]
                  .filter(Boolean)
                  .join(" ");
                const badge = ocr.badge
                  ? ocr.badge
                  : remote
                    ? "リモート"
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
                        {citePrefix(n)}{label}
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
                        {citePrefix(n)}{label}
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
            <ChatNotePicker
              disabled={blocked || !active}
              threadId={active?.id ?? null}
              noteId={active?.noteId ?? ""}
              onApplied={applyThreadNote}
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
            <PromptLibrary text={draft} disabled={busy} onRecall={applyTemplate} />
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
            <span className="chat-context-meter">
              <span
                className={nearBudget ? "chat-char-meter warn" : "chat-char-meter"}
                title="この会話を送るときの目安です。上限を超えると古いやり取りから落ちます。"
              >
                {contextMeterLabel(meter.used, meter.limit)}
              </span>
              {nearBudget ? (
                <span className="chat-char-meter warn">
                  上限に近いので、新しい会話に切り替えてください。
                </span>
              ) : null}
              <span className="chat-thinking-budget" title={thinkingBudgetLabel(thinkingBudget)}>
                {thinkingBudgetLabel(thinkingBudget)}
              </span>
            </span>
          </div>
          <div className="chat-composer-row">
            <textarea
              ref={textareaRef}
              rows={3}
              value={draft}
              placeholder="メッセージを入力（Enter で送信、Shift+Enter で改行）"
              onChange={(e) => setDraft(e.target.value)}
              onPaste={(e) => {
                void pasteClipboardImage(e);
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey) {
                  if (isImeEnter(e)) return;
                  e.preventDefault();
                  void send();
                }
              }}
            />
            <div className="chat-composer-actions">
              {ocrBusy || onGeneratingThread ? (
                <button type="button" className="chat-btn" onClick={() => void stop()}>
                  停止
                </button>
              ) : (
                <button
                  type="submit"
                  className="chat-btn primary"
                  disabled={busy || !draft.trim() || attaching}
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
