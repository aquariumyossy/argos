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
  noteId: "",
  sortOrder: 0,
  createdAt: 1,
  updatedAt: 2,
};

const assistant = {
  id: "a1",
  threadId: thread.id,
  role: "assistant",
  content: "解除できるのは重大な不履行のときです。",
  createdAt: 3,
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
            content: "解除の要件は？",
            createdAt: 1,
          },
          assistant,
        ];
      case "llm_list_sources":
        return [
          {
            id: "s1",
            threadId: thread.id,
            sortOrder: 0,
            origin: "tool",
            path: "https://example.com/law",
            title: "解説",
            paragraphId: "",
            body: "不履行の説明",
            query: "解除",
            createdAt: 2,
            kind: "web",
            citeNo: 1,
            citedAssistantMessageId: "a1",
            injectedUserMessageId: "m1",
          },
        ];
      case "llm_context_meter":
        return 10;
      case "consume_chat_shortcut_focus":
        return false;
      case "get_note_review":
        return null;
      case "list_notes":
        return [{ id: "note-1", title: "事件メモ" }];
      case "get_active_note":
        return null;
      case "keep_to_note":
        return {
          created: true,
          createdNote: true,
          note: { id: "note-new", title: "無題のノート" },
        };
      case "summarize_chat_to_note":
        return { note: { id: "note-1", title: "事件メモ" } };
      default:
        return null;
    }
  });
});

afterEach(() => {
  cleanup();
});

describe("chat note memory", () => {
  it("keeps a web citation as a badge and summarizes the conversation into a note", async () => {
    render(<Chat />);
    const keep = await screen.findByRole("button", { name: "この出典をノートにキープ" });
    fireEvent.click(keep);
    fireEvent.click(await screen.findByRole("menuitem", { name: "新しいノート" }));
    await vi.waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith(
        "keep_to_note",
        expect.objectContaining({
          payload: expect.objectContaining({
            badge: true,
            silent: true,
            noteId: "new",
            path: "https://example.com/law",
            title: "解説",
            source: "web",
          }),
        }),
      );
    });

    fireEvent.click(
      screen.getByRole("button", { name: "ここまでの会話を要約してノートへ" }),
    );
    const menus = await screen.findAllByRole("menuitem", { name: "新しいノート" });
    fireEvent.click(menus[menus.length - 1]);
    await vi.waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith("summarize_chat_to_note", {
        threadId: "thread-a",
        noteId: "new",
        throughMessageId: "a1",
      });
    });
    expect(mockInvoke).not.toHaveBeenCalledWith(
      "append_note_memo",
      expect.anything(),
    );
  });
});
