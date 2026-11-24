import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import Notes from "./Notes";

const { mockInvoke, listeners } = vi.hoisted(() => ({
  mockInvoke: vi.fn(),
  listeners: new Map<string, (event: { payload?: unknown }) => void>(),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => mockInvoke(...args),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: async (name: string, cb: (event: { payload?: unknown }) => void) => {
    listeners.set(name, cb);
    return () => listeners.delete(name);
  },
}));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    onFocusChanged: async () => () => {},
  }),
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({
  save: vi.fn(),
}));

vi.mock("mermaid", () => ({
  default: {
    initialize: () => {},
    render: async () => ({ svg: "<svg></svg>" }),
  },
}));

const note = {
  id: "n1",
  title: "事件",
  memo: "AIの本文",
  viewMode: "list",
  sortOrder: 0,
  createdAt: 1,
  updatedAt: 2,
};

const history = {
  versions: [
    { id: "v1", title: "事件", source: "manual", createdAt: 1, memoLen: 3 },
    { id: "v2", title: "事件", source: "ai", createdAt: 2, memoLen: 6 },
  ],
  aiUpdatedAt: 2,
};

function calls(): { cmd: string; args: Record<string, unknown> | undefined }[] {
  return mockInvoke.mock.calls.map((call) => ({
    cmd: String(call[0]),
    args: call[1] as Record<string, unknown> | undefined,
  }));
}

beforeEach(() => {
  listeners.clear();
  localStorage.clear();
  mockInvoke.mockReset();
});

afterEach(() => {
  cleanup();
});

async function editMemo(text: string) {
  render(<Notes />);
  fireEvent.click(await screen.findByRole("button", { name: "編集" }));
  const box = await screen.findByPlaceholderText("このノートについてのメモ（Markdown）");
  fireEvent.change(box, { target: { value: text } });
  return box;
}

describe("Notes unsaved memo", () => {
  it("archives the open draft before reloading an AI write", async () => {
    mockInvoke.mockImplementation(async (cmd: string) => {
      switch (cmd) {
        case "list_notes":
          return [note];
        case "get_active_note":
          return note;
        case "list_note_items":
          return [];
        case "list_note_versions":
          return history;
        case "keep_unsaved_note_memo":
          return true;
        default:
          return null;
      }
    });
    const box = await editMemo("未保存の下書き");
    expect(calls().some((call) => call.cmd === "update_note_memo")).toBe(false);
    await act(async () => {
      listeners.get("note-updated")?.({
        payload: { source: "llm", noteId: "n1", kind: "memo" },
      });
    });
    await waitFor(() => {
      expect(calls().some((call) => call.cmd === "keep_unsaved_note_memo")).toBe(true);
    });
    const archived = calls().find((call) => call.cmd === "keep_unsaved_note_memo");
    expect(archived?.args).toMatchObject({ id: "n1", memo: "未保存の下書き" });
    expect(calls().some((call) => call.cmd === "update_note_memo")).toBe(false);
    await waitFor(() => {
      expect((box as HTMLTextAreaElement).value).toBe("AIの本文");
    });
    const keepAt = calls().findIndex((call) => call.cmd === "keep_unsaved_note_memo");
    const reloadAt = calls().findIndex(
      (call, index) => index > keepAt && call.cmd === "list_notes",
    );
    expect(reloadAt).toBeGreaterThan(keepAt);
  });

  it("flushes a blur save before restore and ignores a late overwrite", async () => {
    let releaseUpdate: () => void = () => {};
    const gate = new Promise<void>((resolve) => {
      releaseUpdate = resolve;
    });
    const memoWrites: string[] = [];
    mockInvoke.mockImplementation(async (cmd: string, args?: { memo?: string }) => {
      switch (cmd) {
        case "list_notes":
          return [note];
        case "get_active_note":
          return note;
        case "list_note_items":
          return [];
        case "list_note_versions":
          return history;
        case "note_version_diff":
          return { memo: "元", hunks: [] };
        case "update_note_memo": {
          const memo = String(args?.memo ?? "");
          memoWrites.push(memo);
          if (memoWrites.length === 1) await gate;
          return { ...note, memo, updatedAt: 30 };
        }
        case "revert_note_to_version":
          return { ...note, memo: "元", updatedAt: 40 };
        default:
          return null;
      }
    });
    const box = await editMemo("未保存の下書き");
    fireEvent.blur(box);
    await waitFor(() => {
      expect(memoWrites).toEqual(["未保存の下書き"]);
    });
    fireEvent.click(await screen.findByRole("button", { name: /AIが更新/ }));
    fireEvent.click(await screen.findByRole("button", { name: "この版に戻す" }));
    await act(async () => {
      await Promise.resolve();
    });
    expect(calls().some((call) => call.cmd === "revert_note_to_version")).toBe(false);
    releaseUpdate();
    await waitFor(() => {
      expect(calls().some((call) => call.cmd === "revert_note_to_version")).toBe(true);
    });
    const updateAt = calls().findIndex((call) => call.cmd === "update_note_memo");
    const revertAt = calls().findIndex((call) => call.cmd === "revert_note_to_version");
    expect(updateAt).toBeGreaterThanOrEqual(0);
    expect(revertAt).toBeGreaterThan(updateAt);
    expect(memoWrites.filter((memo) => memo === "未保存の下書き")).toEqual(["未保存の下書き"]);
    const afterRevert = memoWrites.slice(1);
    expect(afterRevert.every((memo) => memo !== "未保存の下書き")).toBe(true);
  });
});
