import {
  useRef,
  useState,
  type ClipboardEvent,
  type CompositionEvent,
  type KeyboardEvent,
} from "react";
import { flushSync } from "react-dom";
import {
  ackNoteReview,
  buildInlineLines,
  revertNoteHunk,
  revertNoteReview,
  type NoteReview,
} from "./noteReview";

type ChangeProps = {
  review: NoteReview;
  onChange: (review: NoteReview) => void;
  onError: (message: string) => void;
};

type DiffProps = ChangeProps & {
  /**
   * Open the note editor on `baseMemo` immediately.
   * `extra` is pasted text only. Keystrokes and IME stay with the editor.
   */
  onBeginEdit: (extra: string, baseMemo: string) => void;
};

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

function lineText(text: string): string {
  return text.length > 0 ? text : "\u00a0";
}

export function NoteReviewBar({ review, onChange, onError }: ChangeProps) {
  const [busy, setBusy] = useState(false);
  const busyRef = useRef(false);

  async function run(kind: "ack" | "revert"): Promise<void> {
    if (busyRef.current) return;
    busyRef.current = true;
    setBusy(true);
    try {
      const next =
        kind === "ack" ? await ackNoteReview(review) : await revertNoteReview(review);
      onChange(next);
    } catch (e) {
      onError(formatInvokeError(e));
    } finally {
      busyRef.current = false;
      setBusy(false);
    }
  }

  return (
    <div className="notes-review-bar" role="status">
      <span className="notes-review-bar-label">チャットが更新</span>
      <div className="notes-review-bar-actions">
        <button
          type="button"
          className="notes-review-ack"
          disabled={busy}
          onClick={() => void run("ack")}
        >
          すべて確定
        </button>
        <button type="button" disabled={busy} onClick={() => void run("revert")}>
          すべて戻す
        </button>
      </div>
    </div>
  );
}

function isEditKey(event: KeyboardEvent<HTMLDivElement>): boolean {
  const native = event.nativeEvent;
  if (native.isComposing || event.key === "Process" || event.keyCode === 229) return true;
  if (event.metaKey || event.ctrlKey || event.altKey) return false;
  if (event.key === "Enter" || event.key === "Backspace" || event.key === "Delete") return true;
  return event.key.length === 1;
}

export function NoteInlineDiff({
  review,
  onChange,
  onError,
  onBeginEdit,
}: DiffProps) {
  const [busy, setBusy] = useState(false);
  const busyRef = useRef(false);
  const startedEditRef = useRef(false);
  const lines = buildInlineLines(review);

  async function occupy<T>(action: () => Promise<T>): Promise<T | undefined> {
    if (busyRef.current) return undefined;
    busyRef.current = true;
    setBusy(true);
    try {
      return await action();
    } catch (e) {
      onError(formatInvokeError(e));
      return undefined;
    } finally {
      busyRef.current = false;
      setBusy(false);
    }
  }

  function revertHunk(hunkIndex: number) {
    void occupy(async () => {
      onChange(await revertNoteHunk(review, hunkIndex));
    });
  }

  function startEditing(extra: string) {
    if (startedEditRef.current) return;
    startedEditRef.current = true;
    const snapshot = review;
    // Mount and focus the textarea before this key or composition continues.
    flushSync(() => {
      onBeginEdit(extra, snapshot.memo);
    });
    void ackNoteReview(snapshot).then(
      (next) => onChange(next),
      (error) => onError(formatInvokeError(error)),
    );
  }

  function onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (event.target !== event.currentTarget) return;
    if (!isEditKey(event)) return;
    startEditing("");
  }

  function onCompositionStart(event: CompositionEvent<HTMLDivElement>) {
    if (event.target !== event.currentTarget) return;
    startEditing("");
  }

  function onPaste(event: ClipboardEvent<HTMLDivElement>) {
    if (event.target !== event.currentTarget) return;
    const text = event.clipboardData.getData("text");
    if (!text) return;
    event.preventDefault();
    startEditing(text);
  }

  return (
    <div
      className="notes-inline-diff"
      tabIndex={0}
      aria-label="チャットが更新した本文"
      onKeyDown={onKeyDown}
      onCompositionStart={onCompositionStart}
      onPaste={onPaste}
    >
      {lines.map((line, index) => {
        const hunkIndex = line.revertHunk;
        return (
          <div key={index} className="notes-inline-line">
            <span
              className={
                line.kind === "add"
                  ? "notes-inline-text notes-inline-add"
                  : line.kind === "del"
                    ? "notes-inline-text notes-inline-del"
                    : "notes-inline-text"
              }
            >
              {lineText(line.text)}
            </span>
            {hunkIndex != null ? (
              <button
                type="button"
                className="notes-inline-revert"
                disabled={busy}
                onClick={() => revertHunk(hunkIndex)}
              >
                戻す
              </button>
            ) : null}
          </div>
        );
      })}
    </div>
  );
}
