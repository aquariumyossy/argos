import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useDeferredValue,
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
import {
  NoteHistoryControls,
  NoteVersionDiffView,
  type NoteHistory,
} from "./NoteHistory";
import type { NoteDiffHunk } from "./noteReview";
import { finishMemoWrite, type PendingMemo } from "./memoSave";
import {
  memoHasOpenWork,
  parseNoteTasks,
  toggleTaskLine,
  type NoteTask,
} from "./noteTasks";
import { noteHeadings, scrollTextareaToOffset, type NoteHeading } from "./noteToc";
import "./notes.css";

const SIDEBAR_MIN = 160;
const SIDEBAR_MAX = 420;
const SIDEBAR_DEFAULT = 220;
const SIDEBAR_WIDTH_KEY = "argos.notes.sidebarWidth";

const PRINT_OPTS_KEY = "argos.notes.printOpts";
const SPLIT_RATIO_KEY = "argos.notes.splitRatio";
const MEMO_EDIT_KEY = "argos.notes.memoEdit";
const SIDEBAR_FILTER_KEY = "argos.notes.sidebarFilter";
const SPLIT_RATIO_MIN = 0.22;
const SPLIT_RATIO_MAX = 0.78;
const MEMO_ATTACH_FULL_MAX = 4000;

type SidebarFilter = "notes" | "tasks";
type NoteUpdatedPayload = { noteId?: string; kind?: string; source?: string };

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
  noteMemo: boolean;
};

const DEFAULT_PRINT_OPTS: PrintOpts = {
  noteMemo: true,
};

function loadPrintOpts(): PrintOpts {
  try {
    const raw = localStorage.getItem(PRINT_OPTS_KEY);
    if (!raw) return { ...DEFAULT_PRINT_OPTS };
    const parsed = JSON.parse(raw) as Partial<PrintOpts>;
    return {
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
  badge?: boolean;
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
      badge: v.badge === true,
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
  const [noteDragId, setNoteDragId] = useState<string | null>(null);
  const [noteDraggableId, setNoteDraggableId] = useState<string | null>(null);
  const [sidebarWidth, setSidebarWidth] = useState(loadSidebarWidth);
  const [resizingSidebar, setResizingSidebar] = useState(false);
  const [listQuery, setListQuery] = useState("");
  const [contentHitIds, setContentHitIds] = useState<string[] | null>(null);
  const [printOpts, setPrintOpts] = useState(loadPrintOpts);
  const [printMenuOpen, setPrintMenuOpen] = useState(false);
  const [chatNotice, setChatNotice] = useState("");
  const [splitRatio, setSplitRatio] = useState(loadSplitRatio);
  const [resizingSplit, setResizingSplit] = useState(false);
  const [memoEdit, setMemoEdit] = useState(loadMemoEdit);
  const [sidebarFilter, setSidebarFilter] = useState<SidebarFilter>(loadSidebarFilter);
  const [history, setHistory] = useState<NoteHistory | null>(null);
  const [historyOpen, setHistoryOpen] = useState(false);
  const [rightTab, setRightTab] = useState<"sources" | "toc">("sources");
  const deferredMemo = useDeferredValue(active?.memo ?? "");
  const headings = useMemo(
    () => (rightTab === "toc" ? noteHeadings(deferredMemo) : []),
    [rightTab, deferredMemo],
  );
  const [diffVersionId, setDiffVersionId] = useState<string | null>(null);
  const [versionDiff, setVersionDiff] = useState<{
    memo: string;
    hunks: NoteDiffHunk[];
  } | null>(null);
  const [historyBusy, setHistoryBusy] = useState(false);
  const printMenuRef = useRef<HTMLDivElement | null>(null);
  const chatNoticeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const rootRef = useRef<HTMLDivElement | null>(null);
  const noteMemoTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const memoDirtyRef = useRef(false);
  const pendingMemoRef = useRef<PendingMemo | null>(null);
  const flushingMemoRef = useRef<PendingMemo | null>(null);
  const flushChainRef = useRef<Promise<void>>(Promise.resolve());
  const memoEpochRef = useRef(0);
  const restoredMemoRef = useRef<PendingMemo | null>(null);
  const aiHoldRef = useRef(false);
  const skipMemoReloadRef = useRef<{ id: string; at: number } | null>(null);
  const loadHistoryRef = useRef<(id: string | undefined) => Promise<void>>(
    async () => {},
  );
  const activeRef = useRef<NoteRow | null>(null);
  const mainRef = useRef<HTMLElement | null>(null);
  const memoTextareaRef = useRef<HTMLTextAreaElement | null>(null);
  const memoPaneRef = useRef<HTMLElement | null>(null);
  const scrollNoteToHeading = useCallback((heading: NoteHeading, index: number) => {
    const pane = memoPaneRef.current;
    const ta = memoTextareaRef.current;
    if (ta && pane?.contains(ta)) {
      scrollTextareaToOffset(ta, heading.offset);
      return;
    }
    const preview = pane?.querySelector(".notes-memo-preview");
    const el = preview?.querySelectorAll("h1, h2, h3, h4, h5, h6").item(index);
    el?.scrollIntoView({ block: "start" });
  }, []);
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

  const notesReadyRef = useRef(false);
  const bootstrappingRef = useRef(false);

  const showChatNotice = useCallback((text: string) => {
    setChatNotice(text);
    if (chatNoticeTimer.current) clearTimeout(chatNoticeTimer.current);
    chatNoticeTimer.current = setTimeout(() => setChatNotice(""), 2200);
  }, []);

  const applySavedMemo = useCallback((row: NoteRow) => {
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
  }, []);

  const flushNoteMemo = useCallback(async () => {
    if (noteMemoTimer.current) {
      clearTimeout(noteMemoTimer.current);
      noteMemoTimer.current = null;
    }
    const pending = pendingMemoRef.current;
    if (pending) {
      pendingMemoRef.current = null;
      memoDirtyRef.current = false;
      flushingMemoRef.current = pending;
      const epoch = memoEpochRef.current;
      skipMemoReloadRef.current = { id: pending.id, at: Date.now() };
      const run = async () => {
        if (aiHoldRef.current) {
          flushingMemoRef.current = null;
          return;
        }
        try {
          const row = await invoke<NoteRow>("update_note_memo", {
            id: pending.id,
            memo: pending.memo,
          });
          const outcome = await finishMemoWrite(
            epoch,
            memoEpochRef.current,
            pending,
            restoredMemoRef.current,
            async (memo) => {
              const fixed = await invoke<NoteRow>("update_note_memo", {
                id: memo.id,
                memo: memo.memo,
              });
              const guard = restoredMemoRef.current;
              if (guard && guard.id === fixed.id && guard.memo === fixed.memo) {
                applySavedMemo(fixed);
              }
            },
          );
          flushingMemoRef.current = null;
          if (outcome !== "apply" || aiHoldRef.current) return;
          applySavedMemo(row);
          void loadHistoryRef.current(pending.id);
        } catch (e) {
          flushingMemoRef.current = null;
          if (
            memoEpochRef.current !== epoch ||
            aiHoldRef.current ||
            restoredMemoRef.current
          ) {
            return;
          }
          memoDirtyRef.current = true;
          pendingMemoRef.current = pending;
          setError(formatInvokeError(e));
        }
      };
      const job = flushChainRef.current.then(run, run);
      flushChainRef.current = job.then(
        () => undefined,
        () => undefined,
      );
    }
    await flushChainRef.current;
  }, [applySavedMemo]);

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

  const loadHistory = useCallback(async (noteId: string | undefined) => {
    if (!noteId) {
      setHistory(null);
      setDiffVersionId(null);
      setVersionDiff(null);
      return;
    }
    try {
      const row = await invoke<NoteHistory>("list_note_versions", { noteId });
      setHistory(row);
      setDiffVersionId((cur) =>
        cur && row.versions.some((v) => v.id === cur) ? cur : null,
      );
    } catch {
      setHistory(null);
    }
  }, []);
  loadHistoryRef.current = loadHistory;

  const refresh = useCallback(async () => {
    try {
      await loadNotes();
      notesReadyRef.current = true;
      setError("");
      await loadHistory(activeRef.current?.id);
    } catch (e) {
      setError(formatInvokeError(e));
    }
  }, [loadNotes, loadHistory]);

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
          await loadHistory(activeRef.current?.id);
          return;
        } catch (e) {
          lastError = e;
        }
      }
      setError(formatInvokeError(lastError));
    } finally {
      bootstrappingRef.current = false;
    }
  }, [loadNotes, loadHistory]);

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
        const draft = pending ?? flushingMemoRef.current;
        if (noteMemoTimer.current) {
          clearTimeout(noteMemoTimer.current);
          noteMemoTimer.current = null;
        }
        restoredMemoRef.current = null;
        aiHoldRef.current = true;
        memoEpochRef.current += 1;
        void (async () => {
          try {
            await flushNoteMemo();
            if (draft) {
              try {
                await invoke("keep_unsaved_note_memo", {
                  id: draft.id,
                  memo: draft.memo,
                });
              } catch (e) {
                if (!pendingMemoRef.current) {
                  pendingMemoRef.current = draft;
                  memoDirtyRef.current = true;
                }
                setError(formatInvokeError(e));
              }
            }
          } finally {
            aiHoldRef.current = false;
          }
          if (cancelled) return;
          await loadNotes();
          if (cancelled) return;
          setDiffVersionId(null);
          await loadHistory(activeRef.current?.id);
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
  }, [flushNoteMemo, refresh, loadNotes, loadHistory]);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    void listen<{ noteId?: string }>("llm-note-review", (event) => {
      const noteId = event.payload?.noteId ?? "";
      if (noteId && activeRef.current && noteId !== activeRef.current.id) {
        return;
      }
      setDiffVersionId(null);
      void loadHistory(activeRef.current?.id);
    }).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [loadHistory]);

  useEffect(() => {
    const noteId = active?.id;
    const versionId = diffVersionId;
    if (!noteId || !versionId) {
      setVersionDiff(null);
      return;
    }
    let cancelled = false;
    void invoke<{ memo: string; hunks: NoteDiffHunk[] }>("note_version_diff", {
      noteId,
      versionId,
    })
      .then((row) => {
        if (!cancelled) setVersionDiff({ memo: row.memo, hunks: row.hunks ?? [] });
      })
      .catch((e) => {
        if (!cancelled) setError(formatInvokeError(e));
      });
    return () => {
      cancelled = true;
    };
  }, [diffVersionId, active?.id]);

  useEffect(() => {
    return () => {
      if (noteMemoTimer.current) clearTimeout(noteMemoTimer.current);
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
      localStorage.setItem(PRINT_OPTS_KEY, JSON.stringify(printOpts));
    } catch {
      /* ignore */
    }
  }, [printOpts]);

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

  const onSplitResizeStart = useCallback((e: ReactMouseEvent) => {
    if (e.button !== 0) return;
    e.preventDefault();
    const main = mainRef.current;
    if (!main) return;
    setResizingSplit(true);
    const rect = main.getBoundingClientRect();
    const size = rect.width;
    const head = main.querySelector(".notes-main-head") as HTMLElement | null;
    const extra = (head?.offsetHeight ?? 0) + 24;

    const onMove = (ev: MouseEvent) => {
      const origin = rect.left;
      const usable = Math.max(80, size - extra);
      const next = (ev.clientX - origin) / usable;
      if (!Number.isFinite(next)) return;
      setSplitRatio(Math.min(SPLIT_RATIO_MAX, Math.max(SPLIT_RATIO_MIN, next)));
    };
    const onUp = () => {
      setResizingSplit(false);
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  }, []);

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

  const applyMemoLocal = useCallback(
    (memo: string) => {
      const id = activeRef.current?.id;
      if (!id) return;
      setActive((prev) => (prev && prev.id === id ? { ...prev, memo } : prev));
      setNotes((prev) => prev.map((n) => (n.id === id ? { ...n, memo } : n)));
      memoDirtyRef.current = true;
      pendingMemoRef.current = { id, memo };
      restoredMemoRef.current = null;
      if (noteMemoTimer.current) clearTimeout(noteMemoTimer.current);
      noteMemoTimer.current = setTimeout(() => {
        void flushNoteMemo();
      }, 400);
    },
    [flushNoteMemo],
  );

  const onNoteMemoChange = useCallback(
    (memo: string) => {
      setDiffVersionId(null);
      applyMemoLocal(memo);
    },
    [applyMemoLocal],
  );

  const restoreVersion = useCallback(async () => {
    const noteId = activeRef.current?.id;
    const versionId = diffVersionId;
    if (!noteId || !versionId || historyBusy) return;
    setHistoryBusy(true);
    try {
      // Finish the blur save first. A save still on the wire can land after
      // the revert and replace the restored memo.
      await flushNoteMemo();
      const epoch = memoEpochRef.current + 1;
      memoEpochRef.current = epoch;
      restoredMemoRef.current = null;
      const row = await invoke<NoteRow>("revert_note_to_version", {
        noteId,
        versionId,
      });
      restoredMemoRef.current = { id: row.id, memo: row.memo };
      if (memoEpochRef.current !== epoch) return;
      setDiffVersionId(null);
      setVersionDiff(null);
      setActive((prev) => (prev && prev.id === row.id ? row : prev));
      setNotes((prev) => prev.map((n) => (n.id === row.id ? row : n)));
      await loadHistory(row.id);
      setError("");
    } catch (e) {
      setError(formatInvokeError(e));
    } finally {
      setHistoryBusy(false);
    }
  }, [diffVersionId, historyBusy, loadHistory, flushNoteMemo]);

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
  }, [active?.memo, memoEdit, diffVersionId]);

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

  const openKept = useCallback(
    async (row: NoteItemRow, snap: NoteItemSnapshot) => {
      const path = snap.path.trim();
      if (!path) return;
      const lower = path.toLowerCase();
      if (lower.startsWith("http://") || lower.startsWith("https://")) {
        try {
          await invoke("open_web_url", { url: path });
          setError("");
        } catch (e) {
          setError(String(e));
        }
        return;
      }
      await previewItem(row, snap);
    },
    [previewItem],
  );

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

  const onNoteListMouseDown = useCallback((e: ReactMouseEvent, id: string) => {
    const el = e.target as HTMLElement | null;
    if (el?.closest(".notes-list-note-handle")) {
      setNoteDraggableId(id);
      return;
    }
    setNoteDraggableId(null);
  }, []);

  const onNoteDragOver = useCallback((e: DragEvent) => {
    e.preventDefault();
    e.dataTransfer.dropEffect = "move";
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
        resizingSplit ? "notes--resizing-split notes--split-row" : "",
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
                  onDragOver={onNoteDragOver}
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
              </div>
            </header>

            {error ? <p className="notes-error">{error}</p> : null}
            {chatNotice ? <p className="notes-chat-notice">{chatNotice}</p> : null}

            <div className="notes-split notes-split--row">
              <section
                ref={memoPaneRef}
                className="notes-memo-pane notes-memo-block"
                style={{ flex: `${splitRatio} 1 0`, width: 0 }}
              >
                <div className="notes-pane-head">
                  <span className="notes-pane-head-title">ノート</span>
                  <div className="notes-pane-head-actions">
                    <NoteHistoryControls
                      history={history}
                      open={historyOpen}
                      selectedId={diffVersionId}
                      onToggleOpen={() => setHistoryOpen((v) => !v)}
                      onSelect={(id) => {
                        setDiffVersionId(id);
                        if (id) setHistoryOpen(true);
                      }}
                    />
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
                        onClick={() => {
                          setDiffVersionId(null);
                          setMemoEdit(true);
                        }}
                      >
                        編集
                      </button>
                    </div>
                  </div>
                </div>
                <div className="notes-pane-body">
                  {diffVersionId && versionDiff ? (
                    <NoteVersionDiffView
                      memo={versionDiff.memo}
                      hunks={versionDiff.hunks}
                      busy={historyBusy}
                      onRestore={() => void restoreVersion()}
                      onClose={() => setDiffVersionId(null)}
                    />
                  ) : memoEdit ? (
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
                aria-orientation="vertical"
                aria-label="ノートと出典の境界"
                onMouseDown={onSplitResizeStart}
              />
              <section
                className="notes-keep-pane"
                style={{ flex: `${1 - splitRatio} 1 0`, width: 0 }}
              >
                <div className="notes-pane-head">
                  <div className="notes-right-tabs" role="tablist" aria-label="出典と目次">
                    <button
                      type="button"
                      role="tab"
                      className="notes-right-tab"
                      id="notes-tab-sources"
                      aria-selected={rightTab === "sources"}
                      aria-controls="notes-panel-sources"
                      onClick={() => setRightTab("sources")}
                    >
                      出典 {items.length} 件
                    </button>
                    <button
                      type="button"
                      role="tab"
                      className="notes-right-tab"
                      id="notes-tab-toc"
                      aria-selected={rightTab === "toc"}
                      aria-controls="notes-panel-toc"
                      onClick={() => setRightTab("toc")}
                    >
                      目次
                    </button>
                  </div>
                </div>
                <div className="notes-pane-body">
            {rightTab === "toc" ? (
              <div
                role="tabpanel"
                id="notes-panel-toc"
                aria-labelledby="notes-tab-toc"
              >
                {headings.length === 0 ? (
                  <p className="notes-toc-empty">見出しがありません</p>
                ) : (
                  <ul className="notes-toc" aria-label="目次">
                    {headings.map((heading, index) => (
                      <li key={`${heading.offset}-${heading.level}-${heading.text}`}>
                        <button
                          type="button"
                          style={{ paddingLeft: `${8 + (heading.level - 1) * 14}px` }}
                          onClick={() => scrollNoteToHeading(heading, index)}
                        >
                          {heading.text}
                        </button>
                      </li>
                    ))}
                  </ul>
                )}
              </div>
            ) : (
              <div
                role="tabpanel"
                id="notes-panel-sources"
                aria-labelledby="notes-tab-sources"
              >
            {parsedItems.length === 0 ? (
              <p className="notes-empty-items">
                まだ出典がありません。検索結果から追加するか、チャットでノートを書くと引用した出典が入ります。
              </p>
            ) : (
              <ul className="notes-items" aria-label="出典">
                {parsedItems.map(({ row, snap }) => {
                  const label = snap.title.trim() || snap.path.trim() || "出典";
                  const web = snap.path.trim().toLowerCase().startsWith("http://")
                    || snap.path.trim().toLowerCase().startsWith("https://");
                  return (
                    <li key={row.id} className="notes-item notes-item--badge">
                      <span className="notes-keep-badge">
                        <button
                          type="button"
                          className="notes-keep-badge-open"
                          title={web ? "ページを開く" : "プレビュー"}
                          onClick={() => void openKept(row, snap)}
                        >
                          {label}
                        </button>
                        <button
                          type="button"
                          className="notes-keep-badge-remove"
                          title="削除"
                          aria-label="削除"
                          onClick={(e) => {
                            e.preventDefault();
                            e.stopPropagation();
                            void removeItem(row.id);
                          }}
                        >
                          ×
                        </button>
                      </span>
                    </li>
                  );
                })}
              </ul>
            )}
              </div>
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
