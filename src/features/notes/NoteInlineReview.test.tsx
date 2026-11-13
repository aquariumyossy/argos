import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { NoteInlineDiff, NoteReviewBar } from "./NoteInlineReview";
import { buildInlineLines, type NoteDiffHunk, type NoteReview } from "./noteReview";
import "./notes.css";

const { mockInvoke } = vi.hoisted(() => ({
  mockInvoke: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => mockInvoke(...args),
}));

const base = "甲\n一\n二\n三\n乙\n";
const memo = "甲A\n一\n二\n三\n乙B\n";

const initial: NoteReview = {
  noteId: "n1",
  noteTitle: "事件メモ",
  hasReview: true,
  base,
  memo,
  baseLen: [...base].length,
  memoLen: [...memo].length,
  heavyDelete: false,
  hunks: [
    {
      gapBefore: 0,
      lines: [
        { kind: "del", text: "甲" },
        { kind: "add", text: "甲A" },
      ],
    },
    {
      gapBefore: 3,
      lines: [
        { kind: "del", text: "乙" },
        { kind: "add", text: "乙B" },
      ],
    },
  ],
};

const afterFirst: NoteReview = {
  ...initial,
  memo: "甲\n一\n二\n三\n乙B\n",
  memoLen: [..."甲\n一\n二\n三\n乙B\n"].length,
  hunks: [
    {
      gapBefore: 4,
      lines: [
        { kind: "del", text: "乙" },
        { kind: "add", text: "乙B" },
      ],
    },
  ],
};

function cleared(text: string): NoteReview {
  return {
    ...initial,
    hasReview: false,
    base: "",
    memo: text,
    baseLen: 0,
    memoLen: [...text].length,
    hunks: [],
    heavyDelete: false,
  };
}

function Host({
  start = initial,
  onEdit,
}: {
  start?: NoteReview;
  onEdit?: (extra: string, baseMemo: string) => void;
}) {
  const [review, setReview] = useState(start);
  const [body, setBody] = useState(start.memo);
  if (!review.hasReview) {
    return <textarea aria-label="メモ" value={body} onChange={(e) => setBody(e.target.value)} />;
  }
  const apply = (row: NoteReview) => {
    setReview(row);
    setBody(row.memo);
  };
  return (
    <>
      <NoteReviewBar review={review} onChange={apply} onError={() => {}} />
      <NoteInlineDiff
        review={review}
        onChange={apply}
        onError={() => {}}
        onBeginEdit={(extra, baseMemo) => {
          onEdit?.(extra, baseMemo);
          setBody(extra ? baseMemo + extra : baseMemo);
        }}
      />
    </>
  );
}

function textSpan(text: string): HTMLElement {
  const found = [...document.querySelectorAll(".notes-inline-text")].find(
    (el) => el.textContent === text,
  );
  if (!found) throw new Error(`line missing: ${text}`);
  return found as HTMLElement;
}

function revertButton(text: string): HTMLButtonElement {
  const button = textSpan(text).closest(".notes-inline-line")?.querySelector("button");
  if (!button) throw new Error(`戻す button missing for ${text}`);
  return button as HTMLButtonElement;
}

afterEach(() => {
  cleanup();
  mockInvoke.mockReset();
});

describe("inline note review", () => {
  it("renders additions in green and deletions struck through, with the unchanged lines", () => {
    render(<Host />);
    const added = textSpan("甲A");
    const deleted = textSpan("甲");
    expect(added.classList.contains("notes-inline-add")).toBe(true);
    expect(deleted.classList.contains("notes-inline-del")).toBe(true);
    expect(getComputedStyle(added).color).toBe("rgb(20, 108, 46)");
    expect(getComputedStyle(deleted).textDecorationLine).toContain("line-through");
    expect(textSpan("一").className).not.toContain("notes-inline-add");
    expect(textSpan("二").className).not.toContain("notes-inline-del");
    expect(textSpan("三")).toBeTruthy();
    expect(screen.queryByText(/行/)).toBeNull();
    expect(screen.getByText("チャットが更新")).toBeTruthy();
    expect(screen.getAllByRole("button", { name: "戻す" })).toHaveLength(2);
  });

  it("puts 戻す on the last changed line of a merged hunk", () => {
    const hunks: NoteDiffHunk[] = [
      {
        gapBefore: 1,
        lines: [
          { kind: "del", text: "x" },
          { kind: "add", text: "X" },
          { kind: "eq", text: "b" },
          { kind: "del", text: "y" },
          { kind: "add", text: "Y" },
        ],
      },
    ];
    const lines = buildInlineLines({ memo: "a\nX\nb\nY\n", hunks });
    expect(lines.map((line) => [line.kind, line.text, line.revertHunk])).toEqual([
      ["eq", "a", null],
      ["del", "x", null],
      ["add", "X", null],
      ["eq", "b", null],
      ["del", "y", null],
      ["add", "Y", 0],
      ["eq", "", null],
    ]);
  });

  it("reverts one change and clears the highlights when every change is reverted", async () => {
    let reverted = 0;
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd !== "revert_note_hunk") throw new Error(cmd);
      reverted += 1;
      return reverted === 1 ? afterFirst : cleared(base);
    });
    render(<Host />);
    fireEvent.click(revertButton("甲A"));
    await waitFor(() => {
      expect(screen.queryByText("甲A")).toBeNull();
    });
    expect(mockInvoke).toHaveBeenCalledWith("revert_note_hunk", {
      noteId: "n1",
      hunkIndex: 0,
      baseLen: initial.baseLen,
      memoLen: initial.memoLen,
    });
    expect(textSpan("甲").classList.contains("notes-inline-del")).toBe(false);
    expect(textSpan("乙B").classList.contains("notes-inline-add")).toBe(true);
    expect(screen.getAllByRole("button", { name: "戻す" })).toHaveLength(1);

    fireEvent.click(revertButton("乙B"));
    await waitFor(() => {
      expect((screen.getByRole("textbox", { name: "メモ" }) as HTMLTextAreaElement).value).toBe(
        base,
      );
    });
    expect(document.querySelector(".notes-inline-add")).toBeNull();
    expect(document.querySelector(".notes-inline-del")).toBeNull();
    expect(screen.queryByRole("button", { name: "すべて確定" })).toBeNull();
    expect(mockInvoke).toHaveBeenLastCalledWith("revert_note_hunk", {
      noteId: "n1",
      hunkIndex: 0,
      baseLen: afterFirst.baseLen,
      memoLen: afterFirst.memoLen,
    });
  });

  it("reverts the whole note", async () => {
    mockInvoke.mockResolvedValue(cleared(base));
    render(<Host />);
    fireEvent.click(screen.getByRole("button", { name: "すべて戻す" }));
    await waitFor(() => {
      expect((screen.getByRole("textbox", { name: "メモ" }) as HTMLTextAreaElement).value).toBe(
        base,
      );
    });
    expect(mockInvoke).toHaveBeenCalledWith("revert_note_review", {
      noteId: "n1",
      memoLen: initial.memoLen,
    });
    expect(screen.queryByText("甲A")).toBeNull();
    expect(document.querySelector(".notes-inline-del")).toBeNull();
    expect(screen.queryByText("チャットが更新")).toBeNull();
  });

  it("confirms every change and returns the note to the current text", async () => {
    mockInvoke.mockResolvedValue(cleared(memo));
    render(<Host />);
    fireEvent.click(screen.getByRole("button", { name: "すべて確定" }));
    await waitFor(() => {
      expect((screen.getByRole("textbox", { name: "メモ" }) as HTMLTextAreaElement).value).toBe(
        memo,
      );
    });
    expect(mockInvoke).toHaveBeenCalledWith("ack_note_review", {
      noteId: "n1",
      memoLen: initial.memoLen,
    });
    expect(document.querySelector(".notes-inline-add")).toBeNull();
    expect(document.querySelector(".notes-inline-del")).toBeNull();
    expect(screen.queryByText("チャットが更新")).toBeNull();
  });

  it("clears highlights when the user starts typing and keeps the confirmed text", async () => {
    mockInvoke.mockResolvedValue(cleared(memo));
    const edits: string[] = [];
    render(
      <Host
        onEdit={(extra) => {
          edits.push(extra);
        }}
      />,
    );
    const body = document.querySelector(".notes-inline-diff");
    if (!body) throw new Error("diff missing");
    fireEvent.keyDown(body, { key: "ArrowLeft" });
    fireEvent.keyDown(body, { key: "x", ctrlKey: true });
    expect(mockInvoke).not.toHaveBeenCalled();
    fireEvent.keyDown(body, { key: "x" });
    await waitFor(() => {
      expect((screen.getByRole("textbox", { name: "メモ" }) as HTMLTextAreaElement).value).toBe(
        `${memo}x`,
      );
    });
    expect(edits).toEqual(["x"]);
    expect(mockInvoke).toHaveBeenCalledWith("ack_note_review", {
      noteId: "n1",
      memoLen: initial.memoLen,
    });
    expect(document.querySelector(".notes-inline-add")).toBeNull();
    expect(document.querySelector(".notes-inline-del")).toBeNull();
    expect(screen.queryByRole("button", { name: "チャットが更新" })).toBeNull();
    expect(screen.queryByText("チャットが更新")).toBeNull();
  });
});
