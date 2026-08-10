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
import { highlightText } from "../search/highlightText";
import "./notes.css";

const SIDEBAR_MIN = 160;
const SIDEBAR_MAX = 420;
const SIDEBAR_DEFAULT = 220;
const SIDEBAR_WIDTH_KEY = "argos.notes.sidebarWidth";

const BODY_HEIGHT_MIN = 72;
const BODY_HEIGHT_MAX = 800;
const BODY_HEIGHTS_KEY = "argos.notes.bodyHeights";

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

type NoteRow = {
  id: string;
  title: string;
  memo: string;
  viewMode: string;
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

export default function Notes() {
  const [notes, setNotes] = useState<NoteRow[]>([]);
  const [active, setActive] = useState<NoteRow | null>(null);
  const [items, setItems] = useState<NoteItemRow[]>([]);
  const [error, setError] = useState("");
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState("");
  const [dragId, setDragId] = useState<string | null>(null);
  const [draggableId, setDraggableId] = useState<string | null>(null);
  const [sidebarWidth, setSidebarWidth] = useState(loadSidebarWidth);
  const [resizingSidebar, setResizingSidebar] = useState(false);
  const [bodyHeights, setBodyHeights] = useState<Record<string, number>>(loadBodyHeights);
  const [resizingBodyId, setResizingBodyId] = useState<string | null>(null);
  const rootRef = useRef<HTMLDivElement | null>(null);
  const bodyRefs = useRef<Map<string, HTMLDivElement>>(new Map());
  const noteMemoTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const itemMemoTimers = useRef<Map<string, ReturnType<typeof setTimeout>>>(
    new Map(),
  );

  const viewMode = active?.viewMode === "grid" ? "grid" : "list";
  const notesReadyRef = useRef(false);
  const bootstrappingRef = useRef(false);

  const loadNotes = useCallback(async () => {
    const list = await invoke<NoteRow[]>("list_notes");
    setNotes(list);
    let current = await invoke<NoteRow | null>("get_active_note");
    if (!current && list.length > 0) {
      current = await invoke<NoteRow>("set_active_note", { id: list[0].id });
    }
    setActive(current);
    if (current) {
      const nextItems = await invoke<NoteItemRow[]>("list_note_items", {
        noteId: current.id,
      });
      setItems(nextItems);
    } else {
      setItems([]);
    }
  }, []);

  const refresh = useCallback(async () => {
    try {
      await loadNotes();
      notesReadyRef.current = true;
      setError("");
    } catch (e) {
      setError(formatInvokeError(e));
    }
  }, [loadNotes]);

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
          return;
        } catch (e) {
          lastError = e;
        }
      }
      setError(formatInvokeError(lastError));
    } finally {
      bootstrappingRef.current = false;
    }
  }, [loadNotes]);

  useEffect(() => {
    void bootstrapNotes();
  }, [bootstrapNotes]);

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
  }, [bootstrapNotes]);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    void listen("note-updated", () => {
      void refresh();
    }).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [refresh]);

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

  const createNote = useCallback(async () => {
    try {
      await invoke<NoteRow>("create_note", { title: "無題のノート" });
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  }, [refresh]);

  const selectNote = useCallback(
    async (id: string) => {
      try {
        await invoke<NoteRow>("set_active_note", { id });
        await refresh();
      } catch (e) {
        setError(String(e));
      }
    },
    [refresh],
  );

  const deleteNote = useCallback(
    async (id: string) => {
      if (!window.confirm("このノートを削除しますか？")) return;
      try {
        await invoke("delete_note", { id });
        await refresh();
      } catch (e) {
        setError(String(e));
      }
    },
    [refresh],
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
      if (noteMemoTimer.current) clearTimeout(noteMemoTimer.current);
      const id = active.id;
      noteMemoTimer.current = setTimeout(() => {
        void invoke("update_note_memo", { id, memo }).catch((e) =>
          setError(String(e)),
        );
      }, 400);
    },
    [active],
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

  const parsedItems = useMemo(
    () =>
      items.map((it) => ({
        row: it,
        snap: parseSnapshot(it.itemJson),
      })),
    [items],
  );

  return (
    <div
      ref={rootRef}
      className={[
        "notes",
        resizingSidebar ? "notes--resizing" : "",
        resizingBodyId ? "notes--resizing-body" : "",
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
        {notes.length === 0 ? (
          <p className="notes-empty-side">保存済みノートはありません</p>
        ) : (
          <ul className="notes-list">
            {notes.map((n) => (
              <li
                key={n.id}
                className={
                  active?.id === n.id ? "notes-list-item active" : "notes-list-item"
                }
              >
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
                    {n.title || "無題のノート"}
                  </button>
                )}
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
            ))}
          </ul>
        )}
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

      <main className="notes-main">
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
              <div>
                <h2>{active.title || "無題のノート"}</h2>
                <p className="notes-muted">
                  {items.length} 件キープ · 左のハンドルをドラッグで並べ替え
                </p>
              </div>
              <div className="notes-view-toggle" role="group" aria-label="表示切替">
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
            </header>

            <label className="notes-memo-block">
              <span className="notes-memo-label">ノートメモ</span>
              <textarea
                value={active.memo}
                placeholder="このノートについてのメモ"
                rows={2}
                onChange={(e) => onNoteMemoChange(e.target.value)}
              />
            </label>

            {error ? <p className="notes-error">{error}</p> : null}

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
                {parsedItems.map(({ row, snap }) => (
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
                        <button
                          type="button"
                          className="notes-icon-btn"
                          title="開く"
                          aria-label="開く"
                          onClick={() => void openPath(snap.path)}
                        >
                          <IconOpenFile />
                        </button>
                        {!isOutlookPath(snap.path) ? (
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
                    <div className="notes-item-path" title={snap.path}>
                      {snap.path}
                    </div>
                    <div
                      className={
                        resizingBodyId === row.id
                          ? "notes-item-body-wrap is-resizing"
                          : "notes-item-body-wrap"
                      }
                    >
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
                ))}
              </ul>
            )}
          </>
        )}
      </main>
    </div>
  );
}
