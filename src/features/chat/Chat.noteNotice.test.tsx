import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import Chat from "./Chat";

const { mockInvoke } = vi.hoisted(() => ({
  mockInvoke: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => mockInvoke(...args),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: async () => () => {},
}));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    onFocusChanged: async () => () => {},
    onCloseRequested: async () => () => {},
  }),
}));

vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({
    onDragDropEvent: async () => () => {},
  }),
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn(),
}));

vi.mock("@tauri-apps/plugin-clipboard-manager", () => ({
  writeText: vi.fn(),
}));

vi.mock("mermaid", () => ({
  default: {
    initialize: () => {},
    render: async () => ({ svg: "<svg></svg>" }),
  },
}));

const thread = {
  id: "thread-a",
  title: "契約の相談",
  pathPrefix: "",
  noteId: "note-1",
  sortOrder: 0,
  createdAt: 1,
  updatedAt: 2,
};

const review = {
  noteId: "note-1",
  noteTitle: "事件メモ",
  hasReview: true,
  base: "元\n",
  memo: "新しい\n",
  baseLen: 2,
  memoLen: 4,
  hunks: [{ gapBefore: 0, lines: [{ kind: "del", text: "元" }, { kind: "add", text: "新しい" }] }],
  heavyDelete: false,
};

beforeEach(() => {
  mockInvoke.mockReset();
  mockInvoke.mockImplementation(async (cmd: string) => {
    switch (cmd) {
      case "is_app_ready":
        return true;
      case "get_settings":
        return {
          fontSize: 14,
          llmMaxContextTokens: 131_072,
          searxngUrl: "",
          searchMode: "local",
        };
      case "llm_thinking_budget":
        return { state: "unset" };
      case "llm_list_threads":
        return [thread];
      case "llm_get_active_thread":
        return thread;
      case "llm_list_messages":
        return [
          {
            id: "m1",
            threadId: thread.id,
            role: "user",
            content: "最初の質問",
            createdAt: 1,
          },
        ];
      case "llm_list_sources":
        return [];
      case "llm_context_meter":
        return 10;
      case "consume_chat_shortcut_focus":
        return false;
      case "get_note_review":
        return review;
      default:
        return null;
    }
  });
});

afterEach(() => {
  cleanup();
});

describe("chat note update notice", () => {
  it("shows only the update notice and the open button", async () => {
    render(<Chat />);
    const notice = await screen.findByText(/ノート『事件メモ』を更新しました/);
    const row = notice.closest("p");
    const labels = [...(row?.querySelectorAll("button") ?? [])].map((button) => button.textContent);
    expect(labels).toEqual(["ノートで開く"]);
    expect(screen.queryByRole("button", { name: "すべて戻す" })).toBeNull();
    expect(screen.queryByRole("button", { name: "すべて確定" })).toBeNull();
    expect(screen.queryByText("チャットがメモを更新しました")).toBeNull();
    expect(document.querySelector(".notes-inline-add, .notes-diff-add")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "ノートで開く" }));
    await vi.waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith("set_active_note", { id: "note-1" });
      expect(mockInvoke).toHaveBeenCalledWith("show_notes_window");
    });
  });
});
