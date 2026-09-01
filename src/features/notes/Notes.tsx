import {
  useCallback,
  useEffect,
  useLayoutEffect,
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
import { highlightText } from "../search/highlightText";
import {
  applyLegalMdHighlights,
  clearLegalMdHighlights,
  formatLegalDisplayHtml,
  legalMdHighlightTerms,
  type LegalDisplayKind,
} from "./legalMdFormat";
import {
  buildNoteExportMarkdown,
  formatExportBody,
  noteExportFilename,
  saveNoteMarkdown,
} from "./exportNoteText";
import ChatDestPicker, { attachToChat } from "../chat/ChatDestPicker";
import { openPreview } from "../preview/openPreview";
import MemoMdHelper from "./MemoMdHelper";
import { applyMemoMdInsert, type MemoMdKind } from "./memoMdInsert";
import NoteMemoView from "./NoteMemoView";
import NoteReviewPanel, { type NoteReview } from "./NoteReviewPanel";
import {
  memoHasOpenWork,
  parseNoteTasks,
  toggleTaskLine,
  type NoteTask,
} from "./noteTasks";
import "./notes.css";

const SIDEBAR_MIN = 160;
const SIDEBAR_MAX = 420;
const SIDEBAR_DEFAULT = 220;
const SIDEBAR_WIDTH_KEY = "argos.notes.sidebarWidth";

const BODY_HEIGHT_MIN = 72;
const BODY_HEIGHT_MAX = 800;
const BODY_HEIGHTS_KEY = "argos.notes.bodyHeights";

const LEGAL_MD_FORMAT_KEY = "argos.notes.legalMdFormat";
const PRINT_OPTS_KEY = "argos.notes.printOpts";
const SPLIT_DIR_KEY = "argos.notes.splitDir";
const SPLIT_RATIO_KEY = "argos.notes.splitRatio";
const MEMO_EDIT_KEY = "argos.notes.memoEdit";
const SIDEBAR_FILTER_KEY = "argos.notes.sidebarFilter";
const SPLIT_RATIO_MIN = 0.22;
const SPLIT_RATIO_MAX = 0.78;
const SPLIT_FORCE_COL_PX = 560;
const MEMO_ATTACH_FULL_MAX = 4000;

type SplitDir = "col" | "row";
type SidebarFilter = "notes" | "tasks";
type NoteUpdatedPayload = { noteId?: string; kind?: string; source?: string };

function loadSplitDir(): SplitDir {
  try {
    return localStorage.getItem(SPLIT_DIR_KEY) === "row" ? "row" : "col";
  } catch {
    return "col";
  }
}

function loadSplitRatio(): number {
  try {
    const n = Number(localStorage.getItem(SPLIT_RATIO_KEY));
    if (!Number.isFinite(n)) return 0.4;
    return Math.min(SPLIT_RATIO_MAX, Math.max(SPLIT_RATIO_MIN, n));
  } catch {
    return 0.4;
  }
}

function loadMemoEdit(): boolean {
  try {
    return localStorage.getItem(MEMO_EDIT_KEY) === "1";
  } catch {
    return false;
  }
}

function loadSidebarFilter(): SidebarFilter {
  try {
    return localStorage.getItem(SIDEBAR_FILTER_KEY) === "tasks" ? "tasks" : "notes";
  } catch {
    return "notes";
  }
}

function memoAttachBody(title: string, memo: string): string {
  const text = memo.trim();
  if (!text) return "";
  if ([...text].length <= MEMO_ATTACH_FULL_MAX) return text;
  const heads = text
    .split("\n")
    .filter((ln) => /^#{1,6}\s+\S/.test(ln))
    .map((ln) => ln.trim());
  const head = [...text].slice(0, 1200).join("");
  const outline = heads.length > 0 ? heads.join("\n") : "（見出しなし）";
  return `ノート『${title}』のメモ（長いため要約）\n見出し:\n${outline}\n\n先頭:\n${head}\n…`;
}

type PrintOpts = {
  path: boolean;
  query: boolean;
  itemMemo: boolean;
  noteMemo: boolean;
};

const DEFAULT_PRINT_OPTS: PrintOpts = {
  path: true,
  query: true,
  itemMemo: true,
  noteMemo: true,
};

function loadPrintOpts(): PrintOpts {
  try {
    const raw = localStorage.getItem(PRINT_OPTS_KEY);
    if (!raw) return { ...DEFAULT_PRINT_OPTS };
    const parsed = JSON.parse(raw) as Partial<PrintOpts>;
    return {
      path: parsed.path !== false,
      query: parsed.query !== false,
      itemMemo: parsed.itemMemo !== false,
      noteMemo: parsed.noteMemo !== false,
    };
  } catch {
    return { ...DEFAULT_PRINT_OPTS };
  }
}

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

function clampBodyHeight(h: number): number {
  return Math.min(BODY_HEIGHT_MAX, Math.max(BODY_HEIGHT_MIN, Math.round(h)));
}

function loadBodyHeights(): Record<string, number> {
  try {
    const raw = localStorage.getItem(BODY_HEIGHTS_KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw) as Record<string, unknown>;
    if (!parsed || typeof parsed !== "object") return {};
    const out: Record<string, number> = {};
    for (const [k, v] of Object.entries(parsed)) {
      if (typeof v === "number" && Number.isFinite(v)) {
        out[k] = clampBodyHeight(v);
      }
    }
    return out;
  } catch {
    return {};
  }
}

function loadLegalMdFormat(): boolean {
  try {
    return localStorage.getItem(LEGAL_MD_FORMAT_KEY) === "1";
  } catch {
    return false;
  }
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

function matchesListQuery(text: string, query: string): boolean {
  const q = query.trim().normalize("NFKC").toLowerCase();
  if (!q) return true;
  return text.normalize("NFKC").toLowerCase().includes(q);
}

function noteListTitle(n: { title: string }): string {
  return n.title.trim() || "無題のノート";
}

type NoteRow = {
  id: string;
  title: string;
  memo: string;
  viewMode: string;
  sortOrder: number;
  createdAt: number;
  updatedAt: number;
};

type NoteItemRow = {
  id: string;
  noteId: string;
  sortOrder: number;
  query: string;
  paragraphId: string;
  itemJson: string;
  memo: string;
  createdAt: number;
};

type NoteItemSnapshot = {
  path: string;
  title: string;
  source: string;
  docKind: string;
  paragraphId: string;
  label: string;
  page?: number | null;
  body: string;
  highlightTerms?: string[];
  mailFrom?: string;
  mailDate?: string;
  mailFolder?: string;
};

function parseSnapshot(raw: string): NoteItemSnapshot {
  try {
    const v = JSON.parse(raw) as NoteItemSnapshot;
    return {
      path: v.path ?? "",
      title: v.title ?? "",
      source: v.source ?? "",
      docKind: v.docKind ?? "",
      paragraphId: v.paragraphId ?? "",
      label: v.label ?? "",
      page: v.page ?? null,
      body: v.body ?? "",
      highlightTerms: Array.isArray(v.highlightTerms) ? v.highlightTerms : [],
      mailFrom: v.mailFrom ?? "",
      mailDate: v.mailDate ?? "",
      mailFolder: v.mailFolder ?? "",
    };
  } catch {
    return {
      path: "",
      title: "",
      source: "",
      docKind: "",
      paragraphId: "",
      label: "",
      body: raw,
      highlightTerms: [],
    };
  }
}

function isOutlookPath(path: string): boolean {
  return path.startsWith("outlook:") || path.includes("outlook:");
}

function isOutlookSnapshot(snap: NoteItemSnapshot): boolean {
  return (
    snap.source === "outlook" ||
    snap.docKind === "email" ||
    isOutlookPath(snap.path)
  );
}

/** Compact folder meta: `受信トレイ（半蔵門総合法律事務所）`. */
function formatMailFolderMeta(pathLabel: string): string {
  const parts = pathLabel
    .split("/")
    .map((s) => s.trim())
    .filter(Boolean);
  if (parts.length === 0) return pathLabel.trim();
  const store = parts[0];
  let i = 1;
  while (
    i < parts.length &&
    parts[i].localeCompare(store, undefined, { sensitivity: "accent" }) === 0
  ) {
    i += 1;
  }
  const folderParts = parts.slice(i);
  if (folderParts.length === 0) return store;
  return `${folderParts.join("／")}（${store}）`;
}

function noteItemSourceLabel(snap: NoteItemSnapshot): string {
  if (!isOutlookSnapshot(snap)) return snap.path;
  const from = (snap.mailFrom ?? "").trim();
  const folder = (snap.mailFolder ?? "").trim();
  const folderLabel = folder ? formatMailFolderMeta(folder) : "";
  return [from, folderLabel].filter(Boolean).join(" · ") || "Outlook メール";
}

function ActionIcon({ children }: { children: ReactNode }) {
  return (
    <svg
      className="notes-action-icon"
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

function IconOpenFile() {
  return (
    <ActionIcon>
      <path d="M9 2.5h4.5V7" />
      <path d="M13.5 2.5 7 9" />
      <path d="M7.5 3.5H3.75A1.25 1.25 0 0 0 2.5 4.75v7.5A1.25 1.25 0 0 0 3.75 13.5h7.5a1.25 1.25 0 0 0 1.25-1.25V8.5" />
    </ActionIcon>
  );
}

function IconPreview() {
  return (
    <ActionIcon>
      <path d="M1.75 8s2.25-4 6.25-4 6.25 4 6.25 4-2.25 4-6.25 4-6.25-4-6.25-4Z" />
      <circle cx="8" cy="8" r="1.75" />
    </ActionIcon>
  );
}

function IconFolder() {
  return (
    <ActionIcon>
      <path d="M2.5 5.25A1.25 1.25 0 0 1 3.75 4h2.3l1.2 1.5h5A1.25 1.25 0 0 1 13.5 6.75v4.5A1.25 1.25 0 0 1 12.25 12.5H3.75A1.25 1.25 0 0 1 2.5 11.25v-6Z" />
    </ActionIcon>
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

export default function Notes() {
  const [notes, setNotes] = useState<NoteRow[]>([]);
  const [active, setActive] = useState<NoteRow | null>(null);
  const [items, setItems] = useState<NoteItemRow[]>([]);
  const [error, setError] = useState("");
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState("");
  const [dragId, setDragId] = useState<string | null>(null);
  const [draggableId, setDraggableId] = useState<string | null>(null);
  const [noteDragId, setNoteDragId] = useState<string | null>(null);
  const [noteDraggableId, setNoteDraggableId] = useState<string | null>(null);
  const [sidebarWidth, setSidebarWidth] = useState(loadSidebarWidth);
  const [resizingSidebar, setResizingSidebar] = useState(false);
  const [listQuery, setListQuery] = useState("");
  const [contentHitIds, setContentHitIds] = useState<string[] | null>(null);
  const [bodyHeights, setBodyHeights] = useState<Record<string, number>>(loadBodyHeights);
  const [resizingBodyId, setResizingBodyId] = useState<string | null>(null);
  const [legalMdFormat, setLegalMdFormat] = useState(loadLegalMdFormat);
  const [printOpts, setPrintOpts] = useState(loadPrintOpts);
  const [printMenuOpen, setPrintMenuOpen] = useState(false);
  const [chatNotice, setChatNotice] = useState("");
  const [splitDirPref, setSplitDirPref] = useState<SplitDir>(loadSplitDir);
  const [splitRatio, setSplitRatio] = useState(loadSplitRatio);
  const [forceCol, setForceCol] = useState(false);
  const [resizingSplit, setResizingSplit] = useState(false);
  const [memoEdit, setMemoEdit] = useState(loadMemoEdit);
  const [sidebarFilter, setSidebarFilter] = useState<SidebarFilter>(loadSidebarFilter);
  const [noteReview, setNoteReview] = useState<NoteReview | null>(null);
  const printMenuRef = useRef<HTMLDivElement | null>(null);
  const chatNoticeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const rootRef = useRef<HTMLDivElement | null>(null);
  const bodyRefs = useRef<Map<string, HTMLDivElement>>(new Map());
  const noteMemoTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const itemMemoTimers = useRef<Map<string, ReturnType<typeof setTimeout>>>(
    new Map(),
  );
  const memoDirtyRef = useRef(false);
  const pendingMemoRef = useRef<{ id: string; memo: string } | null>(null);
  const skipMemoReloadRef = useRef<{ id: string; at: number } | null>(null);
  const loadReviewRef = useRef<(id: string | undefined) => Promise<void>>(
    async () => {},
  );
  const activeRef = useRef<NoteRow | null>(null);
  const mainRef = useRef<HTMLElement | null>(null);
  const memoTextareaRef = useRef<HTMLTextAreaElement | null>(null);
  const memoViewRef = useRef<{
    start: number;
    end: number;
    scrollTop: number;
    scrollLeft: number;
  } | null>(null);
  const pendingMemoCaretRef = useRef<number | null>(null);
  const pendingMemoScrollRef = useRef<{
    top: number;
    left: number;
  } | null>(null);

  const viewMode = active?.viewMode === "grid" ? "grid" : "list";
  const splitDir: SplitDir = forceCol ? "col" : splitDirPref;
  const notesReadyRef = useRef(false);
  const bootstrappingRef = useRef(false);

  const showChatNotice = useCallback((text: string) => {
    setChatNotice(text);
    if (chatNoticeTimer.current) clearTimeout(chatNoticeTimer.current);
    chatNoticeTimer.current = setTimeout(() => setChatNotice(""), 2200);
  }, []);

  const flushNoteMemo = useCallback(async () => {
    if (noteMemoTimer.current) {
      clearTimeout(noteMemoTimer.current);
      noteMemoTimer.current = null;
    }
    const pending = pendingMemoRef.current;
    if (!pending) return;
    pendingMemoRef.current = null;
    memoDirtyRef.current = false;
    skipMemoReloadRef.current = { id: pending.id, at: Date.now() };
    try {
      const row = await invoke<NoteRow>("update_note_memo", {
        id: pending.id,
        memo: pending.memo,
      });
      setActive((prev) =>
        prev && prev.id === row.id
          ? { ...prev, memo: row.memo, updatedAt: row.updatedAt }
          : prev,
      );
      setNotes((prev) =>
        prev.map((n) =>
          n.id === row.id ? { ...n, memo: row.memo, updatedAt: row.updatedAt } : n,
        ),
      );
      void loadReviewRef.current(pending.id);
    } catch (e) {
      memoDirtyRef.current = true;
      pendingMemoRef.current = pending;
      setError(formatInvokeError(e));
    }
  }, []);

  const loadNotes = useCallback(async () => {
    const list = await invoke<NoteRow[]>("list_notes");
    let current = await invoke<NoteRow | null>("get_active_note");
    if (!current && list.length > 0) {
      current = await invoke<NoteRow>("set_active_note", { id: list[0].id });
    }
    const dirty = pendingMemoRef.current;
    if (dirty) {
      setNotes(
        list.map((n) => (n.id === dirty.id ? { ...n, memo: dirty.memo } : n)),
      );
      if (current && current.id === dirty.id) {
        current = { ...current, memo: dirty.memo };
      }
    } else {
      setNotes(list);
    }
    setActive(current);
    activeRef.current = current;
    if (current) {
      const nextItems = await invoke<NoteItemRow[]>("list_note_items", {
        noteId: current.id,
      });
      setItems(nextItems);
    } else {
      setItems([]);
    }
  }, []);

  const loadReview = useCallback(async (noteId: string | undefined) => {
    if (!noteId) {
      setNoteReview(null);
      return;
    }
    try {
      const row = await invoke<NoteReview | null>("get_note_review", { noteId });
      setNoteReview(row && row.hasReview ? row : null);
    } catch {
      setNoteReview(null);
    }
  }, []);
  loadReviewRef.current = loadReview;

  const refresh = useCallback(async () => {
    try {
      await loadNotes();
      notesReadyRef.current = true;
      setError("");
      await loadReview(activeRef.current?.id);
    } catch (e) {
      setError(formatInvokeError(e));
    }
  }, [loadNotes, loadReview]);

  const bootstrapNotes = useCallback(async () => {
    if (bootstrappingRef.current || notesReadyRef.current) return;
    bootstrappingRef.current = true;
    setError("");
    try {
      let lastError: unknown;
      for (let attempt = 0; attempt < 50; attempt++) {
        if (attempt > 0) {
          await sleep(Math.min(50 * 2 ** Math.min(attempt - 1, 4), 1000));
        }
        try {
          await loadNotes();
          notesReadyRef.current = true;
          setError("");
          await loadReview(activeRef.current?.id);
          return;
        } catch (e) {
          lastError = e;
        }
      }
      setError(formatInvokeError(lastError));
    } finally {
      bootstrappingRef.current = false;
    }
  }, [loadNotes, loadReview]);

  useEffect(() => {
    void bootstrapNotes();
  }, [bootstrapNotes]);

  useEffect(() => {
    const q = listQuery.trim();
    if (!q) {
      setContentHitIds(null);
      return;
    }
    let cancelled = false;
    const timer = window.setTimeout(() => {
      void invoke<string[]>("search_notes", { query: q })
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

  const visibleNotes = useMemo(() => {
    let list = notes;
    if (sidebarFilter === "tasks") {
      list = list.filter((n) => memoHasOpenWork(n.memo));
    }
    const q = listQuery.trim();
    if (!q) return list;
    const content = new Set(contentHitIds ?? []);
    return list.filter(
      (n) =>
        matchesListQuery(noteListTitle(n), q) ||
        matchesListQuery(n.memo, q) ||
        content.has(n.id),
    );
  }, [notes, listQuery, contentHitIds, sidebarFilter]);

  const openTasks = useMemo(() => {
    const all: NoteTask[] = [];
    for (const n of notes) {
      for (const t of parseNoteTasks(n.id, noteListTitle(n), n.memo)) {
        if (!t.done) all.push(t);
      }
    }
    all.sort((a, b) => {
      if (a.due && b.due) return a.due.localeCompare(b.due);
      if (a.due) return -1;
      if (b.due) return 1;
      return 0;
    });
    return all;
  }, [notes]);

  const listSearching = listQuery.trim().length > 0 && contentHitIds === null;

  useEffect(() => {
    const win = getCurrentWindow();
    let cancelled = false;
    let unlistenFocus: (() => void) | undefined;
    let unlistenReady: (() => void) | undefined;
    void win
      .onFocusChanged((event) => {
        if (event.payload && !notesReadyRef.current) {
          void bootstrapNotes();
        }
        if (!event.payload) {
          void flushNoteMemo();
        }
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlistenFocus = fn;
      });
    void listen("argos-ready", () => {
      if (!notesReadyRef.current) {
        void bootstrapNotes();
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
  }, [bootstrapNotes, flushNoteMemo]);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    void listen<NoteUpdatedPayload>("note-updated", (event) => {
      const payload = event.payload ?? {};
      const skip = skipMemoReloadRef.current;
      const kind = payload.kind ?? "";
      const noteId = payload.noteId ?? "";
      const pending = pendingMemoRef.current;
      const llm = payload.source === "llm";
      if (llm && (!noteId || noteId === pending?.id || noteId === activeRef.current?.id)) {
        if (noteMemoTimer.current) {
          clearTimeout(noteMemoTimer.current);
          noteMemoTimer.current = null;
        }
        pendingMemoRef.current = null;
        memoDirtyRef.current = false;
        skipMemoReloadRef.current = null;
        void (async () => {
          await loadNotes();
          if (cancelled) return;
          await loadReview(activeRef.current?.id);
        })();
        return;
      }
      if (
        skip &&
        kind === "memo" &&
        (!noteId || noteId === skip.id) &&
        Date.now() - skip.at < 1500
      ) {
        return;
      }
      if (
        memoDirtyRef.current &&
        kind === "memo" &&
        (!noteId || noteId === pending?.id)
      ) {
        return;
      }
      if (
        noteId &&
        activeRef.current &&
        noteId !== activeRef.current.id &&
        kind === "items"
      ) {
        return;
      }
      void (async () => {
        await flushNoteMemo();
        if (cancelled) return;
        await refresh();
      })();
    }).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [flushNoteMemo, refresh, loadNotes, loadReview]);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    void listen<{ noteId?: string }>("llm-note-review", (event) => {
      const noteId = event.payload?.noteId ?? "";
      if (noteId && activeRef.current && noteId !== activeRef.current.id) {
        return;
      }
      void loadReview(activeRef.current?.id);
    }).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [loadReview]);

  useEffect(() => {
    return () => {
      if (noteMemoTimer.current) clearTimeout(noteMemoTimer.current);
      for (const t of itemMemoTimers.current.values()) clearTimeout(t);
    };
  }, []);

  useEffect(() => {
    try {
      localStorage.setItem(SIDEBAR_WIDTH_KEY, String(sidebarWidth));
    } catch {
      /* ignore */
    }
  }, [sidebarWidth]);

  useEffect(() => {
    try {
      localStorage.setItem(BODY_HEIGHTS_KEY, JSON.stringify(bodyHeights));
    } catch {
      /* ignore */
    }
  }, [bodyHeights]);

  useEffect(() => {
    try {
      localStorage.setItem(LEGAL_MD_FORMAT_KEY, legalMdFormat ? "1" : "0");
    } catch {
      /* ignore */
    }
  }, [legalMdFormat]);

  useEffect(() => {
    try {
      localStorage.setItem(PRINT_OPTS_KEY, JSON.stringify(printOpts));
    } catch {
      /* ignore */
    }
  }, [printOpts]);

  useEffect(() => {
    try {
      localStorage.setItem(SPLIT_DIR_KEY, splitDirPref);
    } catch {
      /* ignore */
    }
  }, [splitDirPref]);

  useEffect(() => {
    try {
      localStorage.setItem(SPLIT_RATIO_KEY, String(splitRatio));
    } catch {
      /* ignore */
    }
  }, [splitRatio]);

  useEffect(() => {
    try {
      localStorage.setItem(MEMO_EDIT_KEY, memoEdit ? "1" : "0");
    } catch {
      /* ignore */
    }
  }, [memoEdit]);

  useEffect(() => {
    try {
      localStorage.setItem(SIDEBAR_FILTER_KEY, sidebarFilter);
    } catch {
      /* ignore */
    }
  }, [sidebarFilter]);

  useEffect(() => {
    activeRef.current = active;
  }, [active]);

  useEffect(() => {
    const el = mainRef.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver((entries) => {
      const w = entries[0]?.contentRect.width ?? 0;
      setForceCol(w > 0 && w < SPLIT_FORCE_COL_PX);
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, [active]);

  useEffect(() => {
    if (!printMenuOpen) return;
    const onDocMouseDown = (e: MouseEvent) => {
      const target = e.target as Node;
      if (printMenuRef.current && !printMenuRef.current.contains(target)) {
        setPrintMenuOpen(false);
      }
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        setPrintMenuOpen(false);
      }
    };
    document.addEventListener("mousedown", onDocMouseDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDocMouseDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [printMenuOpen]);

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

  const onBodyResizeStart = useCallback(
    (e: ReactMouseEvent, itemId: string) => {
      if (e.button !== 0) return;
      e.preventDefault();
      e.stopPropagation();
      setDraggableId(null);
      const el = bodyRefs.current.get(itemId);
      const startH = bodyHeights[itemId] ?? el?.offsetHeight ?? 160;
      const startY = e.clientY;
      setResizingBodyId(itemId);

      const onMove = (ev: MouseEvent) => {
        setBodyHeights((prev) => ({
          ...prev,
          [itemId]: clampBodyHeight(startH + (ev.clientY - startY)),
        }));
      };
      const onUp = () => {
        setResizingBodyId(null);
        window.removeEventListener("mousemove", onMove);
        window.removeEventListener("mouseup", onUp);
      };
      window.addEventListener("mousemove", onMove);
      window.addEventListener("mouseup", onUp);
    },
    [bodyHeights],
  );

  const onSplitResizeStart = useCallback(
    (e: ReactMouseEvent) => {
      if (e.button !== 0) return;
      e.preventDefault();
      const main = mainRef.current;
      if (!main) return;
      setResizingSplit(true);
      const rect = main.getBoundingClientRect();
      const col = splitDir === "col";
      const size = col ? rect.height : rect.width;
      const head = main.querySelector(".notes-main-head") as HTMLElement | null;
      const extra = (head?.offsetHeight ?? 0) + 24;

      const onMove = (ev: MouseEvent) => {
        const pos = col ? ev.clientY : ev.clientX;
        const origin = col ? rect.top + extra : rect.left;
        const usable = Math.max(80, size - extra);
        const next = (pos - origin) / usable;
        if (!Number.isFinite(next)) return;
        setSplitRatio(
          Math.min(SPLIT_RATIO_MAX, Math.max(SPLIT_RATIO_MIN, next)),
        );
      };
      const onUp = () => {
        setResizingSplit(false);
        window.removeEventListener("mousemove", onMove);
        window.removeEventListener("mouseup", onUp);
      };
      window.addEventListener("mousemove", onMove);
      window.addEventListener("mouseup", onUp);
    },
    [splitDir],
  );

  const createNote = useCallback(async () => {
    try {
      await flushNoteMemo();
      await invoke<NoteRow>("create_note", { title: "無題のノート" });
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  }, [flushNoteMemo, refresh]);

  const selectNote = useCallback(
    async (id: string) => {
      try {
        await flushNoteMemo();
        await invoke<NoteRow>("set_active_note", { id });
        await refresh();
      } catch (e) {
        setError(String(e));
      }
    },
    [flushNoteMemo, refresh],
  );

  const deleteNote = useCallback(
    async (id: string) => {
      if (!window.confirm("このノートを削除しますか？")) return;
      try {
        await flushNoteMemo();
        await invoke("delete_note", { id });
        await refresh();
      } catch (e) {
        setError(String(e));
      }
    },
    [flushNoteMemo, refresh],
  );

  const commitRename = useCallback(
    async (id: string) => {
      try {
        await invoke<NoteRow>("rename_note", { id, title: renameDraft });
        setRenamingId(null);
        await refresh();
      } catch (e) {
        setError(String(e));
      }
    },
    [refresh, renameDraft],
  );

  const onNoteMemoChange = useCallback(
    (memo: string) => {
      if (!active) return;
      setActive({ ...active, memo });
      setNotes((prev) =>
        prev.map((n) => (n.id === active.id ? { ...n, memo } : n)),
      );
      memoDirtyRef.current = true;
      pendingMemoRef.current = { id: active.id, memo };
      if (noteMemoTimer.current) clearTimeout(noteMemoTimer.current);
      noteMemoTimer.current = setTimeout(() => {
        void flushNoteMemo();
      }, 400);
    },
    [active, flushNoteMemo],
  );

  const captureMemoView = useCallback((el?: HTMLTextAreaElement | null) => {
    const t = el ?? memoTextareaRef.current;
    if (!t) return;
    memoViewRef.current = {
      start: t.selectionStart,
      end: t.selectionEnd,
      scrollTop: t.scrollTop,
      scrollLeft: t.scrollLeft,
    };
  }, []);

  useLayoutEffect(() => {
    const caret = pendingMemoCaretRef.current;
    const scroll = pendingMemoScrollRef.current;
    const el = memoTextareaRef.current;
    if (caret == null || !el) return;
    pendingMemoCaretRef.current = null;
    pendingMemoScrollRef.current = null;
    el.focus({ preventScroll: true });
    el.setSelectionRange(caret, caret);
    if (scroll) {
      el.scrollTop = scroll.top;
      el.scrollLeft = scroll.left;
      requestAnimationFrame(() => {
        el.scrollTop = scroll.top;
        el.scrollLeft = scroll.left;
        requestAnimationFrame(() => {
          el.scrollTop = scroll.top;
          el.scrollLeft = scroll.left;
        });
      });
    }
  }, [active?.memo, memoEdit]);

  const onInsertMemoMd = useCallback(
    (kind: MemoMdKind) => {
      if (!active) return;
      const el = memoTextareaRef.current;
      const snap = memoViewRef.current;
      const start =
        el && document.activeElement === el
          ? el.selectionStart
          : (snap?.start ?? el?.selectionStart ?? active.memo.length);
      const end =
        el && document.activeElement === el
          ? el.selectionEnd
          : (snap?.end ?? el?.selectionEnd ?? active.memo.length);
      const { next, caret } = applyMemoMdInsert(active.memo, start, end, kind);
      if (!memoEdit) setMemoEdit(true);
      pendingMemoCaretRef.current = caret;
      if (el || snap) {
        pendingMemoScrollRef.current = {
          top: snap?.scrollTop ?? el?.scrollTop ?? 0,
          left: snap?.scrollLeft ?? el?.scrollLeft ?? 0,
        };
      }
      onNoteMemoChange(next);
    },
    [active, memoEdit, onNoteMemoChange],
  );

  const onToggleTask = useCallback(
    async (task: NoteTask) => {
      const note = notes.find((n) => n.id === task.noteId);
      if (!note) return;
      const next = toggleTaskLine(note.memo, task.line);
      if (next == null) return;
      if (active?.id === note.id) {
        onNoteMemoChange(next);
        return;
      }
      try {
        await invoke("update_note_memo", { id: note.id, memo: next });
        setNotes((prev) =>
          prev.map((n) => (n.id === note.id ? { ...n, memo: next } : n)),
        );
      } catch (e) {
        setError(formatInvokeError(e));
      }
    },
    [active, notes, onNoteMemoChange],
  );

  const onItemMemoChange = useCallback((itemId: string, memo: string) => {
    setItems((prev) =>
      prev.map((it) => (it.id === itemId ? { ...it, memo } : it)),
    );
    const prev = itemMemoTimers.current.get(itemId);
    if (prev) clearTimeout(prev);
    itemMemoTimers.current.set(
      itemId,
      setTimeout(() => {
        void invoke("update_note_item_memo", { id: itemId, memo }).catch((e) =>
          setError(String(e)),
        );
      }, 400),
    );
  }, []);

  const setViewMode = useCallback(
    async (mode: "list" | "grid") => {
      if (!active) return;
      try {
        const next = await invoke<NoteRow>("set_note_view_mode", {
          id: active.id,
          viewMode: mode,
        });
        setActive(next);
      } catch (e) {
        setError(String(e));
      }
    },
    [active],
  );

  const openPath = useCallback(async (path: string) => {
    if (!path) return;
    try {
      await invoke("open_hit", { path });
      setError("");
    } catch (e) {
      setError(String(e));
    }
  }, []);

  const previewItem = useCallback(
    async (row: NoteItemRow, snap: NoteItemSnapshot) => {
      const path = snap.path.trim();
      if (!path) return;
      try {
        await openPreview({
          origin: "notes",
          path,
          paragraphId: snap.paragraphId || row.paragraphId,
          query: row.query,
          highlightTerms: snap.highlightTerms,
          title: snap.title,
          fallbackBody: snap.body,
          source: snap.source,
        });
        setError("");
      } catch (e) {
        setError(String(e));
      }
    },
    [],
  );

  const openFolder = useCallback(async (path: string) => {
    if (!path) return;
    try {
      await invoke("open_containing_folder", { path });
      setError("");
    } catch (e) {
      setError(String(e));
    }
  }, []);

  const removeItem = useCallback(
    async (id: string) => {
      try {
        await invoke("remove_note_item", { id });
        await refresh();
      } catch (e) {
        setError(String(e));
      }
    },
    [refresh],
  );

  /** Only arm dragging when the press starts outside text/inputs, so selection still works. */
  const onCardMouseDown = useCallback((e: ReactMouseEvent, id: string) => {
    const el = e.target as HTMLElement | null;
    const blocked = el?.closest(
      "textarea, input, .notes-item-body, .notes-item-path, .notes-body-resize",
    );
    const handle = el?.closest(".notes-drag-handle");
    setDraggableId(!handle && blocked ? null : id);
  }, []);

  const onDragStart = useCallback((e: DragEvent, id: string) => {
    if (draggableId !== id) {
      e.preventDefault();
      return;
    }
    setDragId(id);
    e.dataTransfer.effectAllowed = "move";
    e.dataTransfer.setData("text/plain", id);
  }, [draggableId]);

  const onDragOver = useCallback((e: DragEvent) => {
    e.preventDefault();
    e.dataTransfer.dropEffect = "move";
  }, []);

  const onDrop = useCallback(
    async (e: DragEvent, targetId: string) => {
      e.preventDefault();
      const sourceId = dragId || e.dataTransfer.getData("text/plain");
      setDragId(null);
      if (!active || !sourceId || sourceId === targetId) return;
      const ids = items.map((it) => it.id);
      const from = ids.indexOf(sourceId);
      const to = ids.indexOf(targetId);
      if (from < 0 || to < 0) return;
      const next = [...ids];
      next.splice(from, 1);
      next.splice(to, 0, sourceId);
      setItems((prev) => {
        const map = new Map(prev.map((it) => [it.id, it]));
        return next
          .map((id, i) => {
            const it = map.get(id);
            return it ? { ...it, sortOrder: i } : null;
          })
          .filter(Boolean) as NoteItemRow[];
      });
      try {
        await invoke("reorder_note_items", {
          noteId: active.id,
          orderedIds: next,
        });
      } catch (err) {
        setError(String(err));
        await refresh();
      }
    },
    [active, dragId, items, refresh],
  );

  const onNoteListMouseDown = useCallback((e: ReactMouseEvent, id: string) => {
    const el = e.target as HTMLElement | null;
    if (el?.closest(".notes-list-note-handle")) {
      setNoteDraggableId(id);
      return;
    }
    setNoteDraggableId(null);
  }, []);

  const onNoteDragStart = useCallback(
    (e: DragEvent, id: string) => {
      if (noteDraggableId !== id) {
        e.preventDefault();
        return;
      }
      setNoteDragId(id);
      e.dataTransfer.effectAllowed = "move";
      e.dataTransfer.setData("text/note-id", id);
      e.dataTransfer.setData("text/plain", id);
    },
    [noteDraggableId],
  );

  const onNoteDrop = useCallback(
    async (e: DragEvent, targetId: string) => {
      e.preventDefault();
      e.stopPropagation();
      const sourceId =
        noteDragId ||
        e.dataTransfer.getData("text/note-id") ||
        e.dataTransfer.getData("text/plain");
      setNoteDragId(null);
      setNoteDraggableId(null);
      if (!sourceId || sourceId === targetId) return;
      const ids = notes.map((n) => n.id);
      const from = ids.indexOf(sourceId);
      const to = ids.indexOf(targetId);
      if (from < 0 || to < 0) return;
      const next = [...ids];
      next.splice(from, 1);
      next.splice(to, 0, sourceId);
      setNotes((prev) => {
        const map = new Map(prev.map((n) => [n.id, n]));
        return next
          .map((id, i) => {
            const n = map.get(id);
            return n ? { ...n, sortOrder: i } : null;
          })
          .filter(Boolean) as NoteRow[];
      });
      try {
        await invoke("reorder_notes", { orderedIds: next });
      } catch (err) {
        setError(formatInvokeError(err));
        await refresh();
      }
    },
    [noteDragId, notes, refresh],
  );

  const parsedItems = useMemo(
    () =>
      items.map((it) => ({
        row: it,
        snap: parseSnapshot(it.itemJson),
      })),
    [items],
  );

  const noteItemToAttach = useCallback(
    (row: NoteItemRow, snap: NoteItemSnapshot) => ({
      path: snap.path,
      title: snap.title || snap.label || snap.path || "（無題）",
      paragraphId: snap.paragraphId || row.paragraphId,
      body: formatExportBody(snap.path, snap.body),
      query: row.query,
      origin: "attach",
    }),
    [],
  );

  const attachItemsToChat = useCallback(
    async (
      attachItems: ReturnType<typeof noteItemToAttach>[],
      title: string,
      threadId: "new" | string,
      bindNoteId?: string,
    ) => {
      const items = attachItems.filter((it) => it.body.trim());
      if (items.length === 0) {
        setError("添付できる本文がありません。");
        return;
      }
      setError("");
      try {
        const result = await attachToChat(items, title, threadId);
        try {
          await invoke("llm_set_thread_note", {
            id: result.thread.id,
            noteId: bindNoteId ?? "",
          });
        } catch {
          /* command may be missing until bound */
        }
        const dest = result.thread?.title?.trim() || "新しい会話";
        if (result.added > 0) {
          showChatNotice(
            result.createdThread
              ? result.added === 1
                ? "新しいチャットに送った"
                : `${result.added} 件を新しいチャットに送った`
              : `『${dest}』に追加した`,
          );
        } else if (result.skipped > 0) {
          showChatNotice("同じ出典がすでに読込前にあります");
        } else {
          showChatNotice("本文が空のため送れませんでした");
        }
      } catch (e) {
        setError(formatInvokeError(e));
      }
    },
    [showChatNotice],
  );

  const sendNoteToChat = useCallback(
    async (threadId: "new" | string) => {
      if (!active) return;
      const attachItems = parsedItems.map(({ row, snap }) =>
        noteItemToAttach(row, snap),
      );
      const memoBody = memoAttachBody(
        active.title.trim() || "無題のノート",
        active.memo,
      );
      if (memoBody) {
        attachItems.unshift({
          path: "",
          title: "ノートメモ",
          paragraphId: `note-memo:${active.id}`,
          body: memoBody,
          query: "",
          origin: "attach",
        });
      }
      if (attachItems.every((it) => !it.body.trim())) {
        setError("このノートには添付できる本文がありません。");
        return;
      }
      await attachItemsToChat(
        attachItems,
        active.title.trim() || "無題のノート",
        threadId,
        active.id,
      );
    },
    [active, attachItemsToChat, noteItemToAttach, parsedItems],
  );

  const legalFormattedItems = useMemo(() => {
    if (!legalMdFormat) return [];
    const out: {
      id: string;
      html: string;
      kind: LegalDisplayKind;
      terms: string[];
    }[] = [];
    for (const { row, snap } of parsedItems) {
      if (!snap.body) continue;
      const formatted = formatLegalDisplayHtml(snap.path, snap.body);
      if (!formatted) continue;
      out.push({
        id: row.id,
        html: formatted.html,
        kind: formatted.kind,
        terms: legalMdHighlightTerms(row.query, snap.highlightTerms),
      });
    }
    return out;
  }, [legalMdFormat, parsedItems]);

  const legalFormattedById = useMemo(() => {
    const map = new Map<
      string,
      { html: string; kind: LegalDisplayKind; terms: string[] }
    >();
    for (const item of legalFormattedItems) {
      map.set(item.id, {
        html: item.html,
        kind: item.kind,
        terms: item.terms,
      });
    }
    return map;
  }, [legalFormattedItems]);

  useLayoutEffect(() => {
    if (!legalMdFormat || legalFormattedItems.length === 0) {
      clearLegalMdHighlights();
      return;
    }
    const entries: { el: HTMLElement; terms: string[] }[] = [];
    for (const item of legalFormattedItems) {
      const el = bodyRefs.current.get(item.id);
      if (el) entries.push({ el, terms: item.terms });
    }
    applyLegalMdHighlights(entries);
    return () => clearLegalMdHighlights();
  }, [legalMdFormat, legalFormattedItems]);

  const exportActiveNoteText = useCallback(async () => {
    if (!active) return;
    const exportItems = parsedItems.map(({ row, snap }) => ({
      path: snap.path,
      title: snap.title,
      label: snap.label,
      body: snap.body,
      query: row.query,
      memo: row.memo,
      page: snap.page,
    }));
    const markdown = buildNoteExportMarkdown(
      { title: active.title, memo: active.memo },
      exportItems,
    );
    try {
      await saveNoteMarkdown(markdown, noteExportFilename(active.title));
      setError("");
    } catch (e) {
      setError(formatInvokeError(e));
    }
  }, [active, parsedItems]);

  const printActiveNote = useCallback(() => {
    setPrintMenuOpen(false);
    // Let the menu close before the print dialog paints.
    requestAnimationFrame(() => {
      window.print();
    });
  }, []);

  const togglePrintOpt = useCallback((key: keyof PrintOpts) => {
    setPrintOpts((prev) => ({ ...prev, [key]: !prev[key] }));
  }, []);

  async function openWindow(cmd: string) {
    try {
      await invoke(cmd);
    } catch (e) {
      setError(formatInvokeError(e));
    }
  }

  return (
    <div
      ref={rootRef}
      className={[
        "notes",
        resizingSidebar ? "notes--resizing" : "",
        resizingBodyId ? "notes--resizing-body" : "",
        resizingSplit ? `notes--resizing-split notes--split-${splitDir}` : "",
        printOpts.path ? "" : "print-omit-path",
        printOpts.query ? "" : "print-omit-query",
        printOpts.itemMemo ? "" : "print-omit-item-memo",
        printOpts.noteMemo ? "" : "print-omit-note-memo",
      ]
        .filter(Boolean)
        .join(" ")}
      style={{
        gridTemplateColumns: `${sidebarWidth}px 5px minmax(0, 1fr)`,
      }}
    >
      <aside className="notes-sidebar">
        <div className="notes-sidebar-head">
          <h1>ノート</h1>
          <button type="button" className="notes-btn" onClick={() => void createNote()}>
            新規
          </button>
        </div>
        <div className="notes-sidebar-search">
          <span className="notes-sidebar-search-icon" aria-hidden="true">
            <IconSearch />
          </span>
          <input
            className="notes-sidebar-search-input"
            type="text"
            role="searchbox"
            value={listQuery}
            placeholder="ノートを検索"
            aria-label="ノートを検索"
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
              className="notes-sidebar-search-clear"
              title="検索を消す"
              aria-label="検索を消す"
              onClick={() => setListQuery("")}
            >
              ×
            </button>
          ) : null}
        </div>
        <button
          type="button"
          className={
            sidebarFilter === "tasks"
              ? "notes-legal-toggle active notes-filter-toggle"
              : "notes-legal-toggle notes-filter-toggle"
          }
          aria-pressed={sidebarFilter === "tasks"}
          onClick={() =>
            setSidebarFilter((v) => (v === "tasks" ? "notes" : "tasks"))
          }
        >
          未完了ToDo：{openTasks.length}
        </button>
        <div className="notes-sidebar-body">
          {sidebarFilter === "tasks" ? (
            openTasks.length === 0 ? (
              <p className="notes-empty-side">未完了の ToDo はありません</p>
            ) : (
              <ul className="notes-task-list">
                {openTasks.map((t) => (
                  <li key={`${t.noteId}:${t.line}`} className="notes-task-item">
                    <label className="notes-task-row">
                      <input
                        type="checkbox"
                        checked={false}
                        onChange={() => void onToggleTask(t)}
                      />
                      <span>{t.text}</span>
                      {t.due ? (
                        <span className="notes-task-due">{t.due}</span>
                      ) : null}
                    </label>
                    <button
                      type="button"
                      className="notes-task-note"
                      onClick={() => void selectNote(t.noteId)}
                    >
                      {t.noteTitle}
                    </button>
                  </li>
                ))}
              </ul>
            )
          ) : notes.length === 0 ? (
            <p className="notes-empty-side">保存済みノートはありません</p>
          ) : visibleNotes.length === 0 ? (
            <p className="notes-empty-side">
              {listSearching ? "検索中…" : "一致するノートがありません"}
            </p>
          ) : (
            <ul className="notes-list">
              {visibleNotes.map((n) => {
                const title = noteListTitle(n);
                const q = listQuery.trim();
                const titleHit = q ? matchesListQuery(title, q) : true;
                return (
                <li
                  key={n.id}
                  className={[
                    active?.id === n.id ? "notes-list-item active" : "notes-list-item",
                    noteDragId === n.id ? "notes-list-item--dragging" : "",
                  ]
                    .filter(Boolean)
                    .join(" ")}
                  draggable={noteDraggableId === n.id}
                  onMouseDown={(e) => onNoteListMouseDown(e, n.id)}
                  onDragStart={(e) => onNoteDragStart(e, n.id)}
                  onDragOver={onDragOver}
                  onDrop={(e) => void onNoteDrop(e, n.id)}
                  onDragEnd={() => {
                    setNoteDragId(null);
                    setNoteDraggableId(null);
                  }}
                >
                  <span
                    className="notes-list-note-handle"
                    title="ドラッグで並べ替え"
                    aria-hidden="true"
                  >
                    <IconGrip />
                  </span>
                  {renamingId === n.id ? (
                    <form
                      className="notes-rename-form"
                      onSubmit={(e) => {
                        e.preventDefault();
                        void commitRename(n.id);
                      }}
                    >
                      <input
                        autoFocus
                        value={renameDraft}
                        onChange={(e) => setRenameDraft(e.target.value)}
                        onBlur={() => void commitRename(n.id)}
                      />
                    </form>
                  ) : (
                    <button
                      type="button"
                      className="notes-list-title"
                      onClick={() => void selectNote(n.id)}
                      onDoubleClick={() => {
                        setRenamingId(n.id);
                        setRenameDraft(n.title);
                      }}
                      title="ダブルクリックで名前変更"
                    >
                      {q && titleHit ? highlightText(title, q) : title}
                    </button>
                  )}
                  {q && !titleHit ? (
                    <span className="notes-list-hit">内容</span>
                  ) : null}
                  <button
                    type="button"
                    className="notes-icon-btn danger"
                    title="削除"
                    aria-label="削除"
                    onClick={() => void deleteNote(n.id)}
                  >
                    <IconTrash />
                  </button>
                </li>
                );
              })}
            </ul>
          )}
        </div>
        <footer className="notes-sidebar-foot">
          <button
            type="button"
            className="notes-nav-link"
            title="検索"
            aria-label="検索"
            onClick={() => void openWindow("show_popup_window")}
          >
            <IconSearch />
          </button>
          <button
            type="button"
            className="notes-nav-link"
            title="チャット"
            aria-label="チャット"
            onClick={() => void openWindow("show_chat_window")}
          >
            <IconChat />
          </button>
          <button
            type="button"
            className="notes-nav-link"
            title="設定"
            aria-label="設定"
            onClick={() => void openWindow("show_settings_window")}
          >
            <IconSettings />
          </button>
        </footer>
      </aside>

      <div
        className="notes-splitter"
        role="separator"
        aria-orientation="vertical"
        aria-label="ノートリストの幅を変更"
        aria-valuemin={SIDEBAR_MIN}
        aria-valuemax={SIDEBAR_MAX}
        aria-valuenow={sidebarWidth}
        onMouseDown={onSidebarResizeStart}
      />

      <main className="notes-main" ref={mainRef}>
        {!active ? (
          <div className="notes-empty-main">
            <p>ノートがありません。新規作成するか、検索結果からキープしてください。</p>
            <button type="button" className="notes-btn primary" onClick={() => void createNote()}>
              ノートを作成
            </button>
          </div>
        ) : (
          <>
            <header className="notes-main-head">
              <h2>{active.title || "無題のノート"}</h2>
              <div className="notes-main-head-actions">
                <ChatDestPicker
                  buttonClassName="notes-legal-toggle"
                  title="このノートをチャットに送る"
                  onPick={(id) => void sendNoteToChat(id)}
                >
                  チャット
                </ChatDestPicker>
                <button
                  type="button"
                  className="notes-legal-toggle"
                  title="見出し付き Markdown を保存"
                  onClick={() => void exportActiveNoteText()}
                >
                  MD
                </button>
                <div className="notes-print-menu" ref={printMenuRef}>
                  <button
                    type="button"
                    className={
                      printMenuOpen
                        ? "notes-legal-toggle active"
                        : "notes-legal-toggle"
                    }
                    title="印刷する項目を選んで印刷（PDF 可）"
                    aria-expanded={printMenuOpen}
                    aria-haspopup="menu"
                    onClick={() => {
                      setPrintMenuOpen((v) => !v);
                    }}
                  >
                    印刷
                  </button>
                  {printMenuOpen ? (
                    <div className="notes-print-dropdown" role="menu">
                      <p className="notes-print-dropdown-title">印刷に含める項目</p>
                      <label className="notes-print-opt">
                        <input
                          type="checkbox"
                          checked={printOpts.path}
                          onChange={() => togglePrintOpt("path")}
                        />
                        パス
                      </label>
                      <label className="notes-print-opt">
                        <input
                          type="checkbox"
                          checked={printOpts.query}
                          onChange={() => togglePrintOpt("query")}
                        />
                        検索クエリ
                      </label>
                      <label className="notes-print-opt">
                        <input
                          type="checkbox"
                          checked={printOpts.itemMemo}
                          onChange={() => togglePrintOpt("itemMemo")}
                        />
                        アイテムメモ
                      </label>
                      <label className="notes-print-opt">
                        <input
                          type="checkbox"
                          checked={printOpts.noteMemo}
                          onChange={() => togglePrintOpt("noteMemo")}
                        />
                        ノートメモ
                      </label>
                      <button
                        type="button"
                        className="notes-btn primary notes-print-go"
                        onClick={() => printActiveNote()}
                      >
                        印刷する
                      </button>
                    </div>
                  ) : null}
                </div>
                <div className="notes-view-toggle" role="group" aria-label="分割の向き">
                  <button
                    type="button"
                    className={splitDirPref === "col" ? "active" : ""}
                    disabled={forceCol}
                    title={forceCol ? "幅が狭いため上下のみ" : "上下に分割"}
                    onClick={() => setSplitDirPref("col")}
                  >
                    上下
                  </button>
                  <button
                    type="button"
                    className={splitDirPref === "row" && !forceCol ? "active" : ""}
                    disabled={forceCol}
                    title={forceCol ? "幅が狭いため上下のみ" : "左右に分割"}
                    onClick={() => setSplitDirPref("row")}
                  >
                    左右
                  </button>
                </div>
              </div>
            </header>

            {error ? <p className="notes-error">{error}</p> : null}
            {chatNotice ? <p className="notes-chat-notice">{chatNotice}</p> : null}
            {noteReview?.hasReview ? (
              <NoteReviewPanel
                review={noteReview}
                onChange={(row) =>
                  setNoteReview(row.hasReview ? row : null)
                }
                onError={setError}
              />
            ) : null}

            <div
              className={
                splitDir === "row" ? "notes-split notes-split--row" : "notes-split notes-split--col"
              }
            >
              <section
                className="notes-memo-pane notes-memo-block"
                style={
                  splitDir === "col"
                    ? { flex: `${splitRatio} 1 0` }
                    : { flex: `${splitRatio} 1 0`, width: 0 }
                }
              >
                <div className="notes-pane-head">
                  <span className="notes-pane-head-title">メモ</span>
                  <div className="notes-pane-head-actions">
                    <MemoMdHelper
                      key={active.id}
                      onPick={onInsertMemoMd}
                      onCapture={() => captureMemoView()}
                    />
                    <div className="notes-view-toggle" role="group" aria-label="メモの表示">
                      <button
                        type="button"
                        className={!memoEdit ? "active" : ""}
                        onClick={() => setMemoEdit(false)}
                      >
                        表示
                      </button>
                      <button
                        type="button"
                        className={memoEdit ? "active" : ""}
                        onClick={() => setMemoEdit(true)}
                      >
                        編集
                      </button>
                    </div>
                  </div>
                </div>
                <div className="notes-pane-body">
                  {memoEdit ? (
                    <textarea
                      ref={memoTextareaRef}
                      value={active.memo}
                      placeholder="このノートについてのメモ（Markdown）"
                      onChange={(e) => onNoteMemoChange(e.target.value)}
                      onSelect={(e) => captureMemoView(e.currentTarget)}
                      onKeyUp={(e) => captureMemoView(e.currentTarget)}
                      onMouseUp={(e) => captureMemoView(e.currentTarget)}
                      onScroll={(e) => captureMemoView(e.currentTarget)}
                      onBlur={(e) => {
                        captureMemoView(e.currentTarget);
                        void flushNoteMemo();
                      }}
                    />
                  ) : (
                    <NoteMemoView
                      memo={active.memo}
                      highlightQuery={listQuery}
                      onToggleCheckbox={onNoteMemoChange}
                    />
                  )}
                </div>
              </section>
              <div
                className="notes-pane-splitter"
                role="separator"
                aria-orientation={splitDir === "col" ? "horizontal" : "vertical"}
                aria-label="メモとキープの境界"
                onMouseDown={onSplitResizeStart}
              />
              <section
                className="notes-keep-pane"
                style={
                  splitDir === "col"
                    ? { flex: `${1 - splitRatio} 1 0` }
                    : { flex: `${1 - splitRatio} 1 0`, width: 0 }
                }
              >
                <div className="notes-pane-head">
                  <span className="notes-pane-head-title">
                    {items.length} 件キープ
                  </span>
                  <div className="notes-pane-head-actions">
                    <button
                      type="button"
                      className={
                        legalMdFormat
                          ? "notes-legal-toggle active"
                          : "notes-legal-toggle"
                      }
                      aria-pressed={legalMdFormat}
                      title="法令MD・裁判例を見やすく表示（表示のみ）"
                      onClick={() => setLegalMdFormat((v) => !v)}
                    >
                      整形
                    </button>
                    <div className="notes-view-toggle" role="group" aria-label="キープの表示">
                      <button
                        type="button"
                        className={viewMode === "list" ? "active" : ""}
                        onClick={() => void setViewMode("list")}
                      >
                        リスト
                      </button>
                      <button
                        type="button"
                        className={viewMode === "grid" ? "active" : ""}
                        onClick={() => void setViewMode("grid")}
                      >
                        グリッド
                      </button>
                    </div>
                  </div>
                </div>
                <div className="notes-pane-body">
            {parsedItems.length === 0 ? (
              <p className="notes-empty-items">
                まだキープがありません。検索ポップアップの段落からキープできます。
              </p>
            ) : (
              <ul
                className={
                  viewMode === "grid" ? "notes-items notes-items--grid" : "notes-items"
                }
              >
                {parsedItems.map(({ row, snap }) => {
                  const legal = legalFormattedById.get(row.id);
                  return (
                  <li
                    key={row.id}
                    className={
                      dragId === row.id
                        ? "notes-item notes-item--dragging"
                        : "notes-item"
                    }
                    draggable={draggableId === row.id}
                    onMouseDown={(e) => onCardMouseDown(e, row.id)}
                    onDragStart={(e) => onDragStart(e, row.id)}
                    onDragOver={onDragOver}
                    onDrop={(e) => void onDrop(e, row.id)}
                    onDragEnd={() => {
                      setDragId(null);
                      setDraggableId(null);
                    }}
                  >
                    <div className="notes-item-top">
                      <span
                        className="notes-drag-handle"
                        title="ドラッグで並べ替え"
                        aria-hidden="true"
                      >
                        <IconGrip />
                      </span>
                      <div className="notes-item-title">
                        <span className="notes-item-name">
                          {snap.title || "（無題）"}
                        </span>
                        {snap.page != null ? (
                          <span className="notes-item-page">p.{snap.page}</span>
                        ) : null}
                      </div>
                      <div className="notes-item-actions">
                        {snap.path.trim() ? (
                          <button
                            type="button"
                            className="notes-icon-btn"
                            title="プレビュー"
                            aria-label="プレビュー"
                            onClick={() => void previewItem(row, snap)}
                          >
                            <IconPreview />
                          </button>
                        ) : null}
                        <button
                          type="button"
                          className="notes-icon-btn"
                          title={isOutlookSnapshot(snap) ? "メールを開く" : "開く"}
                          aria-label={isOutlookSnapshot(snap) ? "メールを開く" : "開く"}
                          onClick={() => void openPath(snap.path)}
                        >
                          <IconOpenFile />
                        </button>
                        {!isOutlookSnapshot(snap) ? (
                          <button
                            type="button"
                            className="notes-icon-btn"
                            title="フォルダを開く"
                            aria-label="フォルダを開く"
                            onClick={() => void openFolder(snap.path)}
                          >
                            <IconFolder />
                          </button>
                        ) : null}
                        <button
                          type="button"
                          className="notes-icon-btn danger"
                          title="キープを削除"
                          aria-label="キープを削除"
                          onClick={() => void removeItem(row.id)}
                        >
                          <IconTrash />
                        </button>
                      </div>
                    </div>
                    <div
                      className={
                        isOutlookSnapshot(snap)
                          ? "notes-item-path notes-item-path--mail"
                          : "notes-item-path"
                      }
                      title={snap.path}
                    >
                      {noteItemSourceLabel(snap)}
                    </div>
                    <div
                      className={
                        resizingBodyId === row.id
                          ? "notes-item-body-wrap is-resizing"
                          : "notes-item-body-wrap"
                      }
                    >
                      {legal ? (
                        <div
                          className={
                            legal.kind === "court"
                              ? "notes-item-body notes-item-body--legal-md notes-item-body--court-case md-body md-body--compact"
                              : "notes-item-body notes-item-body--legal-md md-body md-body--compact"
                          }
                          ref={(node) => {
                            if (node) bodyRefs.current.set(row.id, node);
                            else bodyRefs.current.delete(row.id);
                          }}
                          style={
                            bodyHeights[row.id] != null
                              ? {
                                  height: bodyHeights[row.id],
                                  maxHeight: "none",
                                }
                              : undefined
                          }
                          dangerouslySetInnerHTML={{ __html: legal.html }}
                        />
                      ) : (
                        <div
                          className="notes-item-body"
                          ref={(node) => {
                            if (node) bodyRefs.current.set(row.id, node);
                            else bodyRefs.current.delete(row.id);
                          }}
                          style={
                            bodyHeights[row.id] != null
                              ? {
                                  height: bodyHeights[row.id],
                                  maxHeight: "none",
                                }
                              : undefined
                          }
                        >
                          {snap.body
                            ? highlightText(snap.body, row.query, snap.highlightTerms)
                            : "（本文なし）"}
                        </div>
                      )}
                      <div
                        className="notes-body-resize"
                        role="separator"
                        aria-orientation="horizontal"
                        aria-label="スニペットの高さを変更"
                        title="ドラッグで高さ変更"
                        onMouseDown={(e) => onBodyResizeStart(e, row.id)}
                      />
                    </div>
                    <div className="notes-item-meta">
                      {row.query ? (
                        <span className="notes-item-query">検索: {row.query}</span>
                      ) : null}
                    </div>
                    <textarea
                      className="notes-item-memo"
                      value={row.memo}
                      placeholder="メモ（任意）"
                      rows={1}
                      onChange={(e) => onItemMemoChange(row.id, e.target.value)}
                    />
                  </li>
                  );
                })}
              </ul>
            )}
                </div>
              </section>
            </div>
          </>
        )}
      </main>
    </div>
  );
}
