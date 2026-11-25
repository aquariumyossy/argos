import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import Notes from "./Notes";

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
  }),
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({
  save: vi.fn(),
}));

const note = {
  id: "n1",
  title: "事件",
  memo: "メモ",
  viewMode: "grid",
  sortOrder: 0,
  createdAt: 1,
  updatedAt: 2,
};

const paragraph = {
  id: "i1",
  noteId: "n1",
  sortOrder: 0,
  query: "解除",
  paragraphId: "C:\\docs\\a.txt#1",
  itemJson: JSON.stringify({
    path: "C:\\docs\\a.txt",
    title: "契約書",
    source: "index",
    paragraphId: "C:\\docs\\a.txt#1",
    body: "この本文は出さない",
    badge: false,
  }),
  memo: "",
  createdAt: 1,
};

const web = {
  id: "i2",
  noteId: "n1",
  sortOrder: 1,
  query: "",
  paragraphId: "https://example.com/law",
  itemJson: JSON.stringify({
    path: "https://example.com/law",
    title: "解説",
    source: "web",
    paragraphId: "https://example.com/law",
    body: "",
    badge: true,
  }),
  memo: "",
  createdAt: 2,
};

beforeEach(() => {
  localStorage.clear();
  mockInvoke.mockReset();
  mockInvoke.mockImplementation(async (cmd: string) => {
    switch (cmd) {
      case "list_notes":
        return [note];
      case "get_active_note":
        return note;
      case "list_note_items":
        return [paragraph, web];
      case "list_note_versions":
        return { versions: [], aiUpdatedAt: null };
      default:
        return null;
    }
  });
});

afterEach(() => {
  cleanup();
});

describe("note keeps", () => {
  it("shows every keep as a badge in a left-right layout", async () => {
    render(<Notes />);
    const file = await screen.findByRole("button", { name: "契約書" });
    expect(
      document.querySelector(".notes-memo-pane .notes-pane-head-title")?.textContent,
    ).toBe("ノート");
    expect(
      document.querySelector(".notes-keep-pane .notes-pane-head-title")?.textContent?.replace(/\s+/g, " ").trim(),
    ).toBe("出典 2 件");
    const page = screen.getByRole("button", { name: "解説" });
    const keepPane = document.querySelector(".notes-keep-pane");
    if (!keepPane) throw new Error("出典ペインがありません");
    const removes = within(keepPane as HTMLElement).getAllByRole("button", { name: "削除" });
    expect(removes).toHaveLength(2);
    fireEvent.click(removes[1]);
    await vi.waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith("remove_note_item", { id: "i2" });
    });
    expect(mockInvoke).not.toHaveBeenCalledWith(
      "open_web_url",
      expect.anything(),
    );
    expect(screen.queryByText("この本文は出さない")).toBeNull();
    expect(screen.queryByRole("button", { name: "上下" })).toBeNull();
    expect(screen.queryByRole("button", { name: "左右" })).toBeNull();
    expect(screen.queryByRole("button", { name: "リスト" })).toBeNull();
    expect(screen.queryByRole("button", { name: "グリッド" })).toBeNull();
    expect(screen.queryByRole("button", { name: "整形" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "印刷" }));
    expect(screen.queryByRole("checkbox", { name: "パス" })).toBeNull();
    expect(screen.queryByRole("checkbox", { name: "検索クエリ" })).toBeNull();
    expect(screen.queryByRole("checkbox", { name: "アイテムメモ" })).toBeNull();
    expect(screen.getByRole("checkbox", { name: "ノートメモ" })).toBeTruthy();
    expect(document.querySelector(".notes-split--row")).not.toBeNull();
    expect(document.querySelector(".notes-split--col")).toBeNull();

    fireEvent.click(page);
    await vi.waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith("open_web_url", {
        url: "https://example.com/law",
      });
    });

    fireEvent.click(file);
    await vi.waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith(
        "show_preview_window",
        expect.objectContaining({
          target: expect.objectContaining({
            path: "C:\\docs\\a.txt",
            paragraphId: "C:\\docs\\a.txt#1",
          }),
        }),
      );
    });
  });
});
