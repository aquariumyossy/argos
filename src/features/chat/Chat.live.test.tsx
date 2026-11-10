import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import Chat from "./Chat";
import { contextMeterLabel, contextMeterLimit } from "./contextEstimate";

type Handler = (event: { payload: unknown }) => void;

const { mockInvoke, mockWriteText, listeners } = vi.hoisted(() => ({
  mockInvoke: vi.fn(),
  mockWriteText: vi.fn(async (_text: string) => {}),
  listeners: new Map<string, Handler>(),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => mockInvoke(...args),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: async (name: string, cb: Handler) => {
    listeners.set(name, cb);
    return () => {
      if (listeners.get(name) === cb) listeners.delete(name);
    };
  },
}));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    onFocusChanged: async () => () => {},
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
  writeText: (text: string) => mockWriteText(text),
}));

const threadA = {
  id: "thread-a",
  title: "契約の相談",
  pathPrefix: "",
  noteId: "",
  sortOrder: 0,
  createdAt: 1,
  updatedAt: 2,
};

const threadB = {
  id: "thread-b",
  title: "別の会話",
  pathPrefix: "",
  noteId: "",
  sortOrder: 1,
  createdAt: 1,
  updatedAt: 1,
};

const originalUser = {
  id: "m1",
  threadId: "thread-a",
  role: "user",
  content: "最初の質問",
  createdAt: 1,
};

const messagesB = [
  {
    id: "mb",
    threadId: "thread-b",
    role: "user",
    content: "別スレッドの発言",
    createdAt: 1,
  },
];

let messagesA = [originalUser];
let releaseSend: () => void = () => {};

const meterLimit = contextMeterLimit(131_072);
const meterA = contextMeterLabel(2222, meterLimit);
const meterB = contextMeterLabel(1111, meterLimit);

function sendCalls() {
  return mockInvoke.mock.calls.filter((call) => call[0] === "llm_send");
}

async function emitDelta(threadId: string, text: string) {
  const handler = listeners.get("llm-chat-delta");
  if (!handler) throw new Error("llm-chat-delta is not listening");
  await act(async () => {
    handler({
      payload: { requestId: "req-1", threadId, text, kind: "content" },
    });
  });
}

async function renderChat() {
  render(<Chat />);
  await screen.findByRole("button", { name: "コピー" });
}

beforeEach(() => {
  messagesA = [originalUser];
  releaseSend = () => {};
  listeners.clear();
  mockWriteText.mockReset();
  mockWriteText.mockResolvedValue(undefined);
  mockInvoke.mockReset();
  mockInvoke.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
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
        return [threadA, threadB];
      case "llm_get_active_thread":
        return threadA;
      case "llm_set_active_thread":
        return args?.id === threadB.id ? threadB : threadA;
      case "llm_list_messages":
        return args?.threadId === threadB.id ? messagesB : messagesA;
      case "llm_list_sources":
        return [];
      case "llm_context_meter":
        return args?.threadId === threadB.id ? 1111 : 2222;
      case "llm_send": {
        const content = String(args?.content ?? "");
        return new Promise((resolve) => {
          releaseSend = () => {
            messagesA = [
              originalUser,
              {
                id: "m-user-2",
                threadId: threadA.id,
                role: "user",
                content,
                createdAt: 2,
              },
              {
                id: "m-asst",
                threadId: threadA.id,
                role: "assistant",
                content: "完成した回答",
                createdAt: 3,
              },
            ];
            resolve({
              thread: threadA,
              userMessage: messagesA[1],
              assistantMessage: messagesA[2],
              cancelled: false,
              error: null,
            });
          };
        });
      }
      default:
        return null;
    }
  });
});

afterEach(() => {
  cleanup();
});

describe("user message actions", () => {
  it("copies that message's text", async () => {
    await renderChat();
    fireEvent.click(screen.getByRole("button", { name: "コピー" }));
    await vi.waitFor(() => {
      expect(mockWriteText).toHaveBeenCalledWith("最初の質問");
    });
  });

  it("resends the same text as a new message and disables resend while generating", async () => {
    await renderChat();
    const resend = screen.getByRole("button", { name: "再送" }) as HTMLButtonElement;
    expect(resend.disabled).toBe(false);
    fireEvent.click(resend);
    await screen.findByText("生成中…");
    expect(sendCalls()).toEqual([
      [
        "llm_send",
        { threadId: "thread-a", content: "最初の質問", webSearch: false },
      ],
    ]);
    expect(screen.getAllByText("最初の質問").length).toBeGreaterThanOrEqual(2);
    const during = screen.getAllByRole("button", { name: "再送" }) as HTMLButtonElement[];
    expect(during.length).toBeGreaterThanOrEqual(2);
    expect(during.every((button) => button.disabled)).toBe(true);
    const copies = screen.getAllByRole("button", { name: "コピー" }) as HTMLButtonElement[];
    expect(copies.length).toBeGreaterThanOrEqual(1);
    expect(copies.every((button) => button.disabled)).toBe(false);
  });
});

describe("background generation", () => {
  it("routes deltas to the originating thread and keeps send disabled elsewhere", async () => {
    await renderChat();
    await screen.findByText(meterA);
    fireEvent.click(screen.getByRole("button", { name: "再送" }));
    await screen.findByText("生成中…");
    await emitDelta("thread-a", "途中の回答");
    await screen.findByText("途中の回答");
    const origin = document.querySelector('[data-thread-id="thread-a"]');
    const other = document.querySelector('[data-thread-id="thread-b"]');
    expect(origin?.textContent).toContain("生成中");
    expect(other?.textContent ?? "").not.toContain("生成中");

    fireEvent.click(screen.getByRole("button", { name: "別の会話" }));
    await screen.findByText("別スレッドの発言");
    expect(screen.queryByText("途中の回答")).toBeNull();
    expect(screen.queryByText("生成中…")).toBeNull();
    expect(document.querySelector('[data-thread-id="thread-a"]')?.textContent).toContain("生成中");
    const box = screen.getByPlaceholderText(
      "メッセージを入力（Enter で送信、Shift+Enter で改行）",
    );
    fireEvent.change(box, { target: { value: "別の送信" } });
    expect((screen.getByRole("button", { name: "送信" }) as HTMLButtonElement).disabled).toBe(
      true,
    );
    fireEvent.keyDown(box, { key: "Enter" });
    expect(sendCalls()).toHaveLength(1);
    expect(mockInvoke.mock.calls.some((call) => call[0] === "llm_cancel")).toBe(false);
    expect((screen.getByRole("button", { name: "再送" }) as HTMLButtonElement).disabled).toBe(true);

    await emitDelta("thread-a", "の続き");
    await emitDelta("thread-b", "混線");
    expect(screen.queryByText("途中の回答")).toBeNull();
    expect(screen.queryByText("混線")).toBeNull();
    await screen.findByText(meterB);
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 350));
    });
    expect(screen.getByText(meterB)).toBeTruthy();
    expect(screen.queryByText(meterA)).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "契約の相談" }));
    await screen.findByText("途中の回答の続き");
    expect(screen.queryByText("別スレッドの発言")).toBeNull();
    expect(screen.queryByText("混線")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "別の会話" }));
    await screen.findByText("別スレッドの発言");
    await act(async () => {
      releaseSend();
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 50));
    });
    expect(screen.queryByText("完成した回答")).toBeNull();
    expect(screen.getByText("別スレッドの発言")).toBeTruthy();
    expect(screen.getByText(meterB)).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "契約の相談" }));
    await screen.findByText("完成した回答");
    expect(screen.getAllByText("最初の質問").length).toBeGreaterThanOrEqual(2);
    expect(screen.queryByText("別スレッドの発言")).toBeNull();
    expect(screen.queryByText("生成中…")).toBeNull();
  }, 15000);
});

describe("scroll follow during generation", () => {
  async function startPinnedLog(): Promise<HTMLElement> {
    await renderChat();
    fireEvent.click(screen.getByRole("button", { name: "再送" }));
    await screen.findByText("生成中…");
    const log = document.querySelector(".chat-log");
    if (!(log instanceof HTMLElement)) throw new Error("chat log missing");
    Object.defineProperty(log, "clientHeight", { configurable: true, value: 120 });
    Object.defineProperty(log, "scrollHeight", { configurable: true, value: 800 });
    await emitDelta("thread-a", "底");
    expect(log.scrollTop).toBe(800);
    expect(screen.queryByRole("button", { name: "↓最新へ" })).toBeNull();
    return log;
  }

  it("stops following when scrolled up, resumes at the bottom, and the button jumps back", async () => {
    const log = await startPinnedLog();

    log.scrollTop = 0;
    fireEvent.scroll(log);
    expect(screen.getByRole("button", { name: "↓最新へ" })).toBeTruthy();
    await emitDelta("thread-a", "一行");
    expect(log.scrollTop).toBe(0);

    log.scrollTop = 700;
    fireEvent.scroll(log);
    expect(screen.queryByRole("button", { name: "↓最新へ" })).toBeNull();
    await emitDelta("thread-a", "つづき");
    expect(log.scrollTop).toBe(800);

    log.scrollTop = 0;
    fireEvent.scroll(log);
    fireEvent.click(screen.getByRole("button", { name: "↓最新へ" }));
    expect(log.scrollTop).toBe(800);
    expect(screen.queryByRole("button", { name: "↓最新へ" })).toBeNull();
    await emitDelta("thread-a", "さらに");
    expect(log.scrollTop).toBe(800);
  });

  it("stops following on an upward step smaller than the bottom slack", async () => {
    const log = await startPinnedLog();
    log.scrollTop = 790;
    fireEvent.scroll(log);
    expect(screen.getByRole("button", { name: "↓最新へ" })).toBeTruthy();
    await emitDelta("thread-a", "少し上");
    expect(log.scrollTop).toBe(790);

    log.scrollTop = 800;
    fireEvent.scroll(log);
    expect(screen.queryByRole("button", { name: "↓最新へ" })).toBeNull();
    await emitDelta("thread-a", "戻った");
    expect(log.scrollTop).toBe(800);
  });

  it("stops following on wheel, arrow key, and touch before the next delta", async () => {
    const log = await startPinnedLog();

    fireEvent.wheel(log, { deltaY: -4 });
    log.scrollTop = 796;
    await emitDelta("thread-a", "ホイール");
    expect(log.scrollTop).toBe(796);
    fireEvent.click(screen.getByRole("button", { name: "↓最新へ" }));
    expect(log.scrollTop).toBe(800);

    fireEvent.keyDown(window, { key: "ArrowUp" });
    log.scrollTop = 794;
    await emitDelta("thread-a", "キー");
    expect(log.scrollTop).toBe(794);
    fireEvent.click(screen.getByRole("button", { name: "↓最新へ" }));

    fireEvent.touchStart(log, { touches: [{ clientY: 100 }] });
    fireEvent.touchMove(log, { touches: [{ clientY: 128 }] });
    log.scrollTop = 792;
    await emitDelta("thread-a", "タッチ");
    expect(log.scrollTop).toBe(792);
  });
});
