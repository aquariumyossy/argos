import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { useState } from "react";
import { afterEach, describe, expect, it } from "vitest";
import {
  NoteHistoryControls,
  NoteVersionDiffView,
  formatAiMark,
  type NoteHistory,
} from "./NoteHistory";
import type { NoteDiffHunk } from "./noteReview";

const createdAt = 1_700_000_000;

const history: NoteHistory = {
  aiUpdatedAt: createdAt + 60,
  versions: [
    {
      id: "v1",
      title: "事件",
      source: "manual",
      createdAt,
      memoLen: 2,
    },
    {
      id: "v2",
      title: "事件",
      source: "ai",
      createdAt: createdAt + 60,
      memoLen: 4,
    },
  ],
};

const hunks: NoteDiffHunk[] = [
  {
    gapBefore: 0,
    lines: [
      { kind: "del", text: "元" },
      { kind: "add", text: "新しい" },
    ],
  },
];

function Harness() {
  const [open, setOpen] = useState(false);
  const [selected, setSelected] = useState<string | null>(null);
  const [restored, setRestored] = useState("");
  const showing = selected === "v1";
  return (
    <div>
      <NoteHistoryControls
        history={history}
        open={open}
        selectedId={selected}
        onToggleOpen={() => setOpen((v) => !v)}
        onSelect={setSelected}
      />
      {showing ? (
        <NoteVersionDiffView
          memo={"新しい\n"}
          hunks={hunks}
          busy={false}
          onRestore={() => setRestored(selected ?? "")}
          onClose={() => setSelected(null)}
        />
      ) : (
        <p>最新の本文</p>
      )}
      <p>戻した:{restored}</p>
    </div>
  );
}

afterEach(() => {
  cleanup();
});

describe("note version history", () => {
  it("shows the AI mark and a colored diff until the version is restored", () => {
    render(<Harness />);
    expect(screen.getByRole("button", { name: formatAiMark(createdAt + 60) })).toBeTruthy();
    expect(screen.getByText("最新の本文")).toBeTruthy();
    expect(document.querySelector(".notes-inline-add")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: formatAiMark(createdAt + 60) }));
    expect(document.querySelector(".notes-inline-add")?.textContent).toBe("新しい");
    expect(document.querySelector(".notes-inline-del")?.textContent).toBe("元");
    expect(screen.queryByText("最新の本文")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "この版に戻す" }));
    expect(screen.getByText("戻した:v1")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "閉じる" }));
    expect(screen.getByText("最新の本文")).toBeTruthy();
    expect(document.querySelector(".notes-inline-add")).toBeNull();
  });

  it("selects an older version from the history without restoring it", () => {
    render(<Harness />);
    fireEvent.click(screen.getByRole("button", { name: "履歴" }));
    fireEvent.click(screen.getByRole("button", { name: /手入力/ }));
    expect(document.querySelector(".notes-inline-add")).toBeTruthy();
    expect(screen.getByText("戻した:")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: /最新/ }));
    expect(screen.getByText("最新の本文")).toBeTruthy();
  });
});
