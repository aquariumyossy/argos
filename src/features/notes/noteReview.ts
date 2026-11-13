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

export type InlineLine = {
  kind: "eq" | "add" | "del";
  text: string;
  /** Set on the last added or deleted line of a hunk. */
  revertHunk: number | null;
};

/** Same split as the Rust review diff: an empty memo has no lines. */
export function splitNoteLines(text: string): string[] {
  if (text.length === 0) return [];
  return text.split("\n");
}

/**
 * Full note body for an inline review. Unchanged lines between hunks come
 * from the current memo. `gapBefore` counts those equal lines, matching
 * `diff_hunks` on the server. The revert button sits on the last changed
 * line of each hunk, which is the unit `revert_note_hunk` uses.
 */
export function buildInlineLines(
  review: Pick<NoteReview, "memo" | "hunks">,
): InlineLine[] {
  const memoLines = splitNoteLines(review.memo);
  const out: InlineLine[] = [];
  let memoIndex = 0;
  review.hunks.forEach((hunk, hunkIndex) => {
    const gap = Number.isFinite(hunk.gapBefore) ? Math.max(0, hunk.gapBefore) : 0;
    for (let i = 0; i < gap; i++) {
      out.push({
        kind: "eq",
        text: memoLines[memoIndex] ?? "",
        revertHunk: null,
      });
      memoIndex += 1;
    }
    let lastChange = -1;
    for (const line of hunk.lines) {
      if (line.kind === "add") {
        out.push({ kind: "add", text: line.text, revertHunk: null });
        lastChange = out.length - 1;
        memoIndex += 1;
      } else if (line.kind === "del") {
        out.push({ kind: "del", text: line.text, revertHunk: null });
        lastChange = out.length - 1;
      } else if (line.kind === "eq") {
        out.push({ kind: "eq", text: line.text, revertHunk: null });
        memoIndex += 1;
      }
    }
    if (lastChange >= 0) {
      out[lastChange] = { ...out[lastChange], revertHunk: hunkIndex };
    }
  });
  while (memoIndex < memoLines.length) {
    out.push({
      kind: "eq",
      text: memoLines[memoIndex] ?? "",
      revertHunk: null,
    });
    memoIndex += 1;
  }
  return out;
}

const inflight = new Map<string, Promise<NoteReview>>();

function invokeOnce(
  key: string,
  run: () => Promise<NoteReview>,
): Promise<NoteReview> {
  const existing = inflight.get(key);
  if (existing) return existing;
  const pending = run().finally(() => {
    if (inflight.get(key) === pending) inflight.delete(key);
  });
  inflight.set(key, pending);
  return pending;
}

export function ackNoteReview(review: NoteReview): Promise<NoteReview> {
  return invokeOnce(`ack:${review.noteId}:${review.memoLen}`, () =>
    invoke<NoteReview>("ack_note_review", {
      noteId: review.noteId,
      memoLen: review.memoLen,
    }),
  );
}

export function revertNoteReview(review: NoteReview): Promise<NoteReview> {
  return invoke<NoteReview>("revert_note_review", {
    noteId: review.noteId,
    memoLen: review.memoLen,
  });
}

export function revertNoteHunk(
  review: NoteReview,
  hunkIndex: number,
): Promise<NoteReview> {
  return invoke<NoteReview>("revert_note_hunk", {
    noteId: review.noteId,
    hunkIndex,
    baseLen: review.baseLen,
    memoLen: review.memoLen,
  });
}
