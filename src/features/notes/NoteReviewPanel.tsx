import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";

export type NoteDiffLine = { kind: string; text: string };

export type NoteDiffHunk = {
  gapBefore: number;
  lines: NoteDiffLine[];
};

export type NoteReview = {
  noteId: string;
  noteTitle: string;
  hasReview: boolean;
  base: string;
  memo: string;
  baseLen: number;
  memoLen: number;
  hunks: NoteDiffHunk[];
  heavyDelete: boolean;
};

type Props = {
  review: NoteReview;
  onChange: (review: NoteReview) => void;
  onError: (message: string) => void;
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

export default function NoteReviewPanel({ review, onChange, onError }: Props) {
  const [busy, setBusy] = useState(false);

  async function run(
    cmd: string,
    extra: Record<string, unknown> = {},
  ): Promise<void> {
    if (busy) return;
    setBusy(true);
    try {
      const args =
        cmd === "ack_note_review" || cmd === "revert_note_review"
          ? { noteId: review.noteId, memoLen: review.memoLen }
          : {
              noteId: review.noteId,
              hunkIndex: extra.hunkIndex,
              baseLen: review.baseLen,
              memoLen: review.memoLen,
            };
      const next = await invoke<NoteReview>(cmd, args);
      onChange(next);
    } catch (e) {
      onError(formatInvokeError(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="notes-review-panel">
      <div className="notes-review-head">
        <strong>チャットがメモを更新しました</strong>
        <div className="notes-review-actions">
          <button
            type="button"
            className="notes-review-ack"
            disabled={busy}
            onClick={() => void run("ack_note_review")}
          >
            すべて確定
          </button>
          <button
            type="button"
            disabled={busy}
            onClick={() => void run("revert_note_review")}
          >
            すべて戻す
          </button>
        </div>
      </div>
      {review.heavyDelete ? (
        <p className="notes-review-warn">大きく削られています</p>
      ) : null}
      <div className="notes-review-hunks">
        {review.hunks.map((hunk, i) => (
          <div key={i} className="notes-review-hunk">
            {hunk.gapBefore > 2 ? (
              <p className="notes-review-gap">… {hunk.gapBefore}行 …</p>
            ) : hunk.gapBefore > 0 ? (
              <p className="notes-review-gap muted">（{hunk.gapBefore}行）</p>
            ) : null}
            <pre className="notes-review-diff">
              {hunk.lines.map((line, j) => (
                <span
                  key={j}
                  className={
                    line.kind === "add"
                      ? "notes-diff-add"
                      : line.kind === "del"
                        ? "notes-diff-del"
                        : "notes-diff-eq"
                  }
                >
                  {line.kind === "add" ? "+" : line.kind === "del" ? "−" : " "}
                  {line.text}
                  {"\n"}
                </span>
              ))}
            </pre>
            <div className="notes-review-hunk-actions">
              <button
                type="button"
                disabled={busy}
                onClick={() => void run("keep_note_hunk", { hunkIndex: i })}
              >
                確定
              </button>
              <button
                type="button"
                disabled={busy}
                onClick={() => void run("revert_note_hunk", { hunkIndex: i })}
              >
                戻す
              </button>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}
