import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

type NoteRow = { id: string; title: string };

type ThreadRow = {
  id: string;
  title: string;
  noteId?: string;
  pathPrefix?: string;
  createdAt: number;
  updatedAt: number;
};

function formatInvokeError(e: unknown): string {
  if (e instanceof Error) return e.message;
  if (typeof e === "string") return e;
  try {
    return JSON.stringify(e);
  } catch {
    return String(e);
  }
}

type Props = {
  disabled: boolean;
  threadId: string | null;
  noteId: string;
  onApplied: (thread: ThreadRow) => void;
  onError: (message: string) => void;
};

export default function ChatNotePicker({
  disabled,
  threadId,
  noteId,
  onApplied,
  onError,
}: Props) {
  const [open, setOpen] = useState(false);
  const [loading, setLoading] = useState(false);
  const [notes, setNotes] = useState<NoteRow[]>([]);
  const [filter, setFilter] = useState("");
  const wrapRef = useRef<HTMLDivElement | null>(null);
  const filterRef = useRef<HTMLInputElement | null>(null);

  const bound = notes.find((n) => n.id === noteId);
  const label = bound
    ? `ノート: ${bound.title.trim() || "無題のノート"}`
    : noteId
      ? "ノート: （削除済み）"
      : "対象ノート";

  const close = useCallback(() => {
    setOpen(false);
    setLoading(false);
    setFilter("");
  }, []);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const list = await invoke<NoteRow[]>("list_notes");
      setNotes(list);
    } catch (e) {
      onError(formatInvokeError(e));
      close();
    } finally {
      setLoading(false);
    }
  }, [close, onError]);

  useEffect(() => {
    close();
  }, [threadId, close]);

  useEffect(() => {
    if (!open) return;
    const onPointerDown = (e: MouseEvent) => {
      const el = wrapRef.current;
      if (!el) return;
      if (e.target instanceof Node && el.contains(e.target)) return;
      close();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        close();
      }
    };
    document.addEventListener("mousedown", onPointerDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onPointerDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open, close]);

  const apply = useCallback(
    async (id: string) => {
      const tid = threadId;
      if (!tid) return;
      close();
      try {
        const t = await invoke<ThreadRow>("llm_set_thread_note", {
          id: tid,
          noteId: id,
        });
        onApplied(t);
      } catch (e) {
        onError(formatInvokeError(e));
      }
    },
    [close, onApplied, onError, threadId],
  );

  const q = filter.trim().toLowerCase();
  const filtered = q
    ? notes.filter((n) => (n.title || "無題のノート").toLowerCase().includes(q))
    : notes;

  return (
    <div className="chat-scope" ref={wrapRef}>
      <button
        type="button"
        className={`chat-tpl${noteId ? " is-active" : ""}`}
        disabled={disabled}
        title="この会話が読み書きするノート"
        aria-expanded={open}
        onClick={() => {
          if (open) close();
          else {
            setOpen(true);
            void load();
            requestAnimationFrame(() => filterRef.current?.focus());
          }
        }}
      >
        {label}
      </button>
      {noteId ? (
        <button
          type="button"
          className="chat-scope-clear"
          title="対象ノートを外す"
          aria-label="対象ノートを外す"
          disabled={disabled}
          onClick={() => void apply("")}
        >
          ×
        </button>
      ) : null}
      {open ? (
        <div className="chat-scope-picker" role="listbox" aria-label="対象ノート">
          <div className="chat-scope-picker-head">
            <span className="chat-scope-picker-head-label">ノート</span>
            <button
              type="button"
              className="chat-scope-picker-close"
              title="閉じる (Esc)"
              aria-label="閉じる"
              onClick={() => close()}
            >
              ×
            </button>
          </div>
          <input
            ref={filterRef}
            className="chat-scope-filter"
            value={filter}
            placeholder="題名で絞り込み…"
            spellCheck={false}
            onChange={(e) => setFilter(e.target.value)}
          />
          <div className="chat-scope-picker-list">
            {loading ? (
              <div className="chat-scope-empty">読み込み中…</div>
            ) : filtered.length === 0 ? (
              <div className="chat-scope-empty">
                {notes.length === 0
                  ? "ノートがありません。"
                  : "一致するノートがありません。"}
              </div>
            ) : (
              <ul>
                <li>
                  <button
                    type="button"
                    role="option"
                    aria-selected={!noteId}
                    onClick={() => void apply("")}
                  >
                    <span className="chat-scope-check" aria-hidden="true">
                      {!noteId ? "✓" : ""}
                    </span>
                    <span className="chat-scope-label">対象なし</span>
                  </button>
                </li>
                {filtered.map((n) => (
                  <li key={n.id}>
                    <button
                      type="button"
                      role="option"
                      aria-selected={n.id === noteId}
                      onClick={() => void apply(n.id)}
                    >
                      <span className="chat-scope-check" aria-hidden="true">
                        {n.id === noteId ? "✓" : ""}
                      </span>
                      <span className="chat-scope-label">
                        {n.title.trim() || "無題のノート"}
                      </span>
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </div>
        </div>
      ) : null}
    </div>
  );
}
