import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { SearchHit } from "../preview/types";

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
    isMaximized: async () => false,
    onResized: async () => () => {},
    minimize: async () => {},
    toggleMaximize: async () => {},
    startDragging: async () => {},
    startResizeDragging: async () => {},
  }),
}));

const FILE =
  "\\\\FILES\\cases\\訴訟案件\\4. 訴え提起〜結審前\\甲野商事 売掛金請求事件\\提出書面\\準備書面（2）.docx";

const fileHit: SearchHit = {
  id: "file-1",
  title: "準備書面（2）.docx",
  snippet: "被告は本件売掛金債権の発生原因を争う",
  path: FILE,
  score: 3,
  source: "local",
  previewText: "",
  highlightTerms: ["売掛金"],
  docKind: "file",
};

const mailHit: SearchHit = {
  id: "outlook:0000ABCDEF",
  title: "売掛金の件",
  snippet: "売掛金残高をご確認ください",
  path: "outlook:0000ABCDEF",
  score: 1,
  source: "outlook",
  previewText: "",
  highlightTerms: ["売掛金"],
  mailFrom: "甲野",
  mailFolder: "受信トレイ",
  docKind: "email",
};

function suggestion(term: string) {
  return {
    term,
    displayPrefix: "",
    displayRest: "",
    kind: "history",
    score: 1,
    fromHistory: true,
  };
}

beforeEach(() => {
  mockInvoke.mockImplementation(async (cmd: string) => {
    if (cmd === "suggest_search_terms") return [suggestion("売掛金残高"), suggestion("AC")];
    if (cmd === "search_query") return [fileHit, mailHit];
    if (cmd === "list_folders") {
      return [
        {
          id: 1,
          path: "\\\\FILES\\cases\\訴訟案件",
          publicPath: "",
          enabled: true,
          indexedCount: 4,
        },
      ];
    }
    if (cmd === "list_search_words") return [];
    if (cmd === "get_settings") return { calendarEnabled: false };
    if (cmd === "record_search_query") return null;
    if (cmd === "open_hit") return null;
    if (cmd === "open_containing_folder") return null;
    return null;
  });
});

afterEach(() => {
  vi.useRealTimers();
  cleanup();
  mockInvoke.mockReset();
});

async function typeQuery(value: string) {
  const input = screen.getByRole("textbox");
  input.focus();
  fireEvent.change(input, { target: { value } });
  await act(async () => {
    await Promise.resolve();
    await vi.advanceTimersByTimeAsync(500);
  });
  return input;
}

describe("Popup keyboard", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  it("does not auto-open suggestions, and Down opens them", async () => {
    const { default: Popup } = await import("./Popup");
    render(<Popup />);
    await typeQuery("売掛金");
    expect(screen.queryByRole("listbox", { name: "検索候補" })).toBeNull();
    expect(screen.getByText("↓で履歴候補")).toBeTruthy();
    fireEvent.keyDown(window, { key: "ArrowDown" });
    expect(screen.getByRole("listbox", { name: "検索候補" })).toBeTruthy();
    expect(screen.getByText("↑↓ で選択 · Enter で閉じる · Tab で確定")).toBeTruthy();
    expect(screen.queryByText("↓で履歴候補")).toBeNull();
  });

  it("Enter with the overlay open never opens a result, and the next Enter does", async () => {
    const { default: Popup } = await import("./Popup");
    render(<Popup />);
    const input = await typeQuery("売掛金");
    input.focus();
    fireEvent.keyDown(window, { key: "ArrowDown" });
    expect(screen.getByRole("listbox", { name: "検索候補" })).toBeTruthy();
    expect(document.querySelector(".hit.active")).toBeNull();
    expect(document.querySelector(".hit-list.is-behind-overlay")).toBeTruthy();

    fireEvent.keyDown(window, { key: "Enter" });
    expect(screen.queryByRole("listbox", { name: "検索候補" })).toBeNull();
    expect(mockInvoke.mock.calls.filter((c) => c[0] === "open_hit")).toHaveLength(0);
    expect(mockInvoke.mock.calls.filter((c) => c[0] === "open_containing_folder")).toHaveLength(
      0,
    );

    fireEvent.keyDown(window, { key: "Enter" });
    const opened = mockInvoke.mock.calls.filter((c) => c[0] === "open_hit");
    expect(opened).toHaveLength(1);
    expect(opened[0]?.[1]).toMatchObject({ path: FILE });
  });

  it("Down after Enter-close moves to result 2", async () => {
    const { default: Popup } = await import("./Popup");
    render(<Popup />);
    const input = await typeQuery("売掛金");
    input.focus();
    fireEvent.keyDown(window, { key: "ArrowDown" });
    expect(screen.getByRole("listbox", { name: "検索候補" })).toBeTruthy();
    fireEvent.keyDown(window, { key: "Enter" });
    expect(screen.queryByRole("listbox", { name: "検索候補" })).toBeNull();
    input.focus();
    fireEvent.keyDown(window, { key: "ArrowDown" });
    expect(screen.queryByRole("listbox", { name: "検索候補" })).toBeNull();
    expect(document.querySelector(".hit.active")?.textContent).toContain("売掛金の件");
    expect(mockInvoke.mock.calls.filter((c) => c[0] === "open_hit")).toHaveLength(0);
  });

  it("Down after Esc-close moves to result 2", async () => {
    const { default: Popup } = await import("./Popup");
    render(<Popup />);
    const input = await typeQuery("売掛金");
    input.focus();
    fireEvent.keyDown(window, { key: "ArrowDown" });
    expect(screen.getByRole("listbox", { name: "検索候補" })).toBeTruthy();
    fireEvent.keyDown(window, { key: "Escape" });
    expect(screen.queryByRole("listbox", { name: "検索候補" })).toBeNull();
    input.focus();
    fireEvent.keyDown(window, { key: "ArrowDown" });
    expect(screen.queryByRole("listbox", { name: "検索候補" })).toBeNull();
    expect(document.querySelector(".hit.active")?.textContent).toContain("売掛金の件");
  });

  it("ignores Enter during IME composition", async () => {
    const { default: Popup } = await import("./Popup");
    render(<Popup />);
    const input = await typeQuery("売掛金");
    input.focus();
    fireEvent.keyDown(window, { key: "ArrowDown" });
    expect(screen.getByRole("listbox", { name: "検索候補" })).toBeTruthy();

    fireEvent.keyDown(window, { key: "Enter", isComposing: true, keyCode: 229 });
    expect(screen.getByRole("listbox", { name: "検索候補" })).toBeTruthy();
    expect(mockInvoke.mock.calls.filter((c) => c[0] === "open_hit")).toHaveLength(0);

    fireEvent.compositionStart(input);
    fireEvent.keyDown(window, { key: "Enter" });
    expect(mockInvoke.mock.calls.filter((c) => c[0] === "open_hit")).toHaveLength(0);
  });

  it("does not render outlook ids", async () => {
    const { default: Popup } = await import("./Popup");
    render(<Popup />);
    await typeQuery("売掛金");
    expect(document.body.textContent).not.toContain("outlook:");
    expect(screen.getByText("メール")).toBeTruthy();
    expect(
      screen.getByText("訴訟案件 › 4. 訴え提起〜結審前 › 甲野商事 売掛金請求事件"),
    ).toBeTruthy();
  });

  it("empty-query Enter closes until a history item is highlighted, then searches it", async () => {
    const { default: Popup } = await import("./Popup");
    render(<Popup />);
    const input = await typeQuery("売掛金");
    const searchesBefore = mockInvoke.mock.calls.filter((c) => c[0] === "search_query").length;
    fireEvent.change(input, { target: { value: "" } });
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
    input.focus();
    fireEvent.keyDown(window, { key: "ArrowDown" });
    expect(screen.getByRole("listbox", { name: "検索候補" })).toBeTruthy();
    expect(document.querySelector(".popup-suggest .is-active")).toBeNull();

    fireEvent.keyDown(window, { key: "Enter" });
    expect(screen.queryByRole("listbox", { name: "検索候補" })).toBeNull();
    expect((input as HTMLInputElement).value).toBe("");
    expect(mockInvoke.mock.calls.filter((c) => c[0] === "open_hit")).toHaveLength(0);
    expect(mockInvoke.mock.calls.filter((c) => c[0] === "search_query")).toHaveLength(
      searchesBefore,
    );

    fireEvent.change(input, { target: { value: " " } });
    fireEvent.change(input, { target: { value: "" } });
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
    input.focus();
    fireEvent.keyDown(window, { key: "ArrowDown" });
    fireEvent.keyDown(window, { key: "ArrowDown" });
    expect(document.querySelector(".popup-suggest .is-active")?.textContent).toContain(
      "売掛金残高",
    );
    expect(screen.getByText("↑↓ で選択 · Enter で検索 · Tab で確定")).toBeTruthy();

    fireEvent.keyDown(window, { key: "Enter", ctrlKey: true });
    expect(screen.getByRole("listbox", { name: "検索候補" })).toBeTruthy();
    expect(mockInvoke.mock.calls.filter((c) => c[0] === "open_hit")).toHaveLength(0);

    fireEvent.keyDown(window, { key: "Enter" });
    expect(screen.queryByRole("listbox", { name: "検索候補" })).toBeNull();
    expect(mockInvoke.mock.calls.filter((c) => c[0] === "open_hit")).toHaveLength(0);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(500);
    });
    const searches = mockInvoke.mock.calls.filter((c) => c[0] === "search_query");
    expect(searches[searches.length - 1]?.[1]).toMatchObject({ query: "売掛金残高" });
    expect(mockInvoke.mock.calls.filter((c) => c[0] === "open_hit")).toHaveLength(0);
    expect((input as HTMLInputElement).value).toBe("売掛金残高");
  });

  it("highlighted Enter on a typed query closes the overlay and does not open the result", async () => {
    const { default: Popup } = await import("./Popup");
    render(<Popup />);
    const input = await typeQuery("売掛金");
    input.focus();
    fireEvent.keyDown(window, { key: "ArrowDown" });
    fireEvent.keyDown(window, { key: "ArrowDown" });
    expect(document.querySelector(".popup-suggest .is-active")?.textContent).toContain(
      "売掛金残高",
    );
    fireEvent.keyDown(window, { key: "Enter" });
    expect(screen.queryByRole("listbox", { name: "検索候補" })).toBeNull();
    expect((input as HTMLInputElement).value).toBe("売掛金");
    expect(mockInvoke.mock.calls.filter((c) => c[0] === "open_hit")).toHaveLength(0);
  });

  it("Tab inserts the current suggestion into the input", async () => {
    const { default: Popup } = await import("./Popup");
    render(<Popup />);
    const input = await typeQuery("売掛金");
    input.focus();
    fireEvent.keyDown(window, { key: "ArrowDown" });
    fireEvent.keyDown(window, { key: "Tab" });
    expect((input as HTMLInputElement).value).toBe("売掛金残高");
    expect(screen.queryByRole("listbox", { name: "検索候補" })).toBeNull();
    expect(mockInvoke.mock.calls.filter((c) => c[0] === "open_hit")).toHaveLength(0);
  });

  it("hides the history hint after result navigation until the query changes", async () => {
    const { default: Popup } = await import("./Popup");
    render(<Popup />);
    const input = await typeQuery("売掛金");
    expect(screen.getByText("↓で履歴候補")).toBeTruthy();
    input.focus();
    fireEvent.keyDown(window, { key: "ArrowDown" });
    fireEvent.keyDown(window, { key: "Escape" });
    expect(screen.queryByRole("listbox", { name: "検索候補" })).toBeNull();
    expect(screen.queryByText("↓で履歴候補")).toBeNull();
    fireEvent.change(input, { target: { value: "売掛金請求" } });
    expect(screen.getByText("↓で履歴候補")).toBeTruthy();
  });
});
