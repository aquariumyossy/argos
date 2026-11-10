import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
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

let messagesA: Array<typeof originalUser & { reasoning?: string }> = [originalUser];
let releaseSend: () => void = () => {};

const meterLimit = contextMeterLimit(131_072);
const meterA = contextMeterLabel(2222, meterLimit);
const meterB = contextMeterLabel(1111, meterLimit);

function sendCalls() {
  return mockInvoke.mock.calls.filter((call) => call[0] === "llm_send");
}

async function emitDelta(threadId: string, text: string, kind = "content") {
  const handler = listeners.get("llm-chat-delta");
  if (!handler) throw new Error("llm-chat-delta is not listening");
  await act(async () => {
    handler({
      payload: { requestId: "req-1", threadId, text, kind },
    });
  });
}

const contentObservers: ResizeObserverCallback[] = [];

class ContentResizeObserver {
  constructor(private readonly callback: ResizeObserverCallback) {}
  observe() {
    contentObservers.push(this.callback);
  }
  unobserve() {}
  disconnect() {
    const index = contentObservers.indexOf(this.callback);
    if (index >= 0) contentObservers.splice(index, 1);
  }
}

function notifyContentResize(content: HTMLElement, height: number) {
  Object.defineProperty(content, "offsetHeight", {
    configurable: true,
    get: () => height,
  });
  const callback = contentObservers[contentObservers.length - 1];
  if (!callback) throw new Error("content ResizeObserver is not attached");
  callback([], {} as ResizeObserver);
}

/** jsdom does not clamp scrollTop. A real log cannot scroll past scrollHeight - clientHeight. */
function installLogGeometry(
  log: HTMLElement,
  initial: { scrollHeight: number; clientHeight: number },
) {
  let scrollHeight = initial.scrollHeight;
  let clientHeight = initial.clientHeight;
  let scrollTop = 0;
  const maxTop = () => Math.max(0, scrollHeight - clientHeight);
  Object.defineProperty(log, "scrollHeight", {
    configurable: true,
    get: () => scrollHeight,
  });
  Object.defineProperty(log, "clientHeight", {
    configurable: true,
    get: () => clientHeight,
  });
  Object.defineProperty(log, "scrollTop", {
    configurable: true,
    get: () => scrollTop,
    set(value: number) {
      scrollTop = Math.min(Math.max(0, value), maxTop());
    },
  });
  return {
    maxTop,
    scrollHeight: () => scrollHeight,
    setScrollHeight(next: number) {
      scrollHeight = next;
    },
    resize(next: { scrollHeight?: number; clientHeight?: number }) {
      if (next.scrollHeight != null) scrollHeight = next.scrollHeight;
      if (next.clientHeight != null) clientHeight = next.clientHeight;
      const max = maxTop();
      if (scrollTop > max) {
        scrollTop = max;
        fireEvent.scroll(log);
      }
    },
  };
}

async function renderChat() {
  render(<Chat />);
  await screen.findByRole("button", { name: "コピー" });
}

beforeEach(() => {
  contentObservers.length = 0;
  vi.stubGlobal("ResizeObserver", ContentResizeObserver);
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
  vi.useRealTimers();
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
  const jump = () => screen.queryByRole("button", { name: "↓最新へ" });

  async function startPinnedLog() {
    await renderChat();
    fireEvent.click(screen.getByRole("button", { name: "再送" }));
    await screen.findByText("生成中…");
    const log = document.querySelector(".chat-log");
    if (!(log instanceof HTMLElement)) throw new Error("chat log missing");
    const box = installLogGeometry(log, { scrollHeight: 800, clientHeight: 120 });
    await emitDelta("thread-a", "底");
    expect(log.scrollTop).toBe(box.maxTop());
    expect(jump()).toBeNull();
    return { log, box };
  }

  it("stops following when scrolled up, resumes at the bottom, and the button jumps back", async () => {
    const { log, box } = await startPinnedLog();
    const bottom = box.maxTop();

    log.scrollTop = 0;
    fireEvent.scroll(log);
    expect(jump()).toBeTruthy();
    await emitDelta("thread-a", "一行");
    expect(log.scrollTop).toBe(0);

    log.scrollTop = bottom;
    fireEvent.scroll(log);
    expect(jump()).toBeNull();
    await emitDelta("thread-a", "つづき");
    expect(log.scrollTop).toBe(bottom);

    log.scrollTop = 0;
    fireEvent.scroll(log);
    fireEvent.click(screen.getByRole("button", { name: "↓最新へ" }));
    expect(log.scrollTop).toBe(bottom);
    expect(jump()).toBeNull();
    await emitDelta("thread-a", "さらに");
    expect(log.scrollTop).toBe(bottom);
  });

  it("keeps following when reasoning, a tool hint, or a reset shrinks the log", async () => {
    const { log, box } = await startPinnedLog();

    await emitDelta("thread-a", "考えている", "reasoning");
    await waitFor(() => expect(screen.queryByText("生成中…")).toBeNull());
    box.resize({ scrollHeight: 760 });
    expect(log.scrollTop).toBe(box.maxTop());
    expect(jump()).toBeNull();
    await emitDelta("thread-a", "続き", "reasoning");
    expect(log.scrollTop).toBe(box.maxTop());

    await emitDelta("thread-a", "search_index…", "tool");
    box.resize({ scrollHeight: 720 });
    expect(jump()).toBeNull();
    await emitDelta("thread-a", "本文");
    expect(log.scrollTop).toBe(box.maxTop());

    await emitDelta("thread-a", "", "reset");
    box.resize({ scrollHeight: 680 });
    expect(jump()).toBeNull();
    await emitDelta("thread-a", "再開");
    expect(log.scrollTop).toBe(box.maxTop());
  });

  it("keeps following a short log when the wheel cannot move it", async () => {
    const { log, box } = await startPinnedLog();
    box.resize({ scrollHeight: 80, clientHeight: 120 });
    expect(log.scrollTop).toBe(0);
    fireEvent.wheel(log, { deltaY: -40 });
    expect(jump()).toBeNull();
    box.resize({ scrollHeight: 400 });
    await emitDelta("thread-a", "伸びた");
    expect(log.scrollTop).toBe(box.maxTop());
    expect(jump()).toBeNull();
  });

  it("keeps following when ArrowUp does not move the log", async () => {
    const { log, box } = await startPinnedLog();
    fireEvent.keyDown(document.body, { key: "ArrowUp" });
    expect(log.scrollTop).toBe(box.maxTop());
    await emitDelta("thread-a", "続く");
    expect(log.scrollTop).toBe(box.maxTop());
    expect(jump()).toBeNull();
  });

  it("stays stopped after scrolling up 16px and then 1px down", async () => {
    const { log, box } = await startPinnedLog();
    const bottom = box.maxTop();
    log.scrollTop = bottom - 16;
    fireEvent.scroll(log);
    expect(jump()).toBeTruthy();
    log.scrollTop = bottom - 15;
    fireEvent.scroll(log);
    expect(jump()).toBeTruthy();
    await emitDelta("thread-a", "止まっている");
    expect(log.scrollTop).toBe(bottom - 15);
  });

  it("keeps following when the window grows while the log is at the bottom", async () => {
    const { log, box } = await startPinnedLog();
    box.resize({ clientHeight: 200 });
    expect(log.scrollTop).toBe(box.maxTop());
    expect(jump()).toBeNull();
    await emitDelta("thread-a", "広い");
    expect(log.scrollTop).toBe(box.maxTop());
  });

  it("stops on an 8px upward step and on an arrow key that moves the log", async () => {
    const { log, box } = await startPinnedLog();
    const bottom = box.maxTop();
    log.scrollTop = bottom - 8;
    fireEvent.scroll(log);
    expect(jump()).toBeTruthy();
    await emitDelta("thread-a", "タッチパッド");
    expect(log.scrollTop).toBe(bottom - 8);

    fireEvent.click(screen.getByRole("button", { name: "↓最新へ" }));
    expect(log.scrollTop).toBe(box.maxTop());
    await act(async () => {});
    log.scrollTop = box.maxTop() - 16;
    fireEvent.scroll(log);
    expect(jump()).toBeTruthy();
    await emitDelta("thread-a", "矢印");
    expect(log.scrollTop).toBe(box.maxTop() - 16);
  });

  it("pins after streamed text grows late, and stays put when follow is off", async () => {
    const { log, box } = await startPinnedLog();
    const content = log.querySelector(".chat-log-content");
    if (!(content instanceof HTMLElement)) throw new Error("log content missing");
    const oldBottom = log.scrollTop;
    vi.useFakeTimers();
    await emitDelta("thread-a", `${"新しい行\n".repeat(12)}末尾`);
    expect(log.scrollTop).toBe(oldBottom);
    await act(async () => {
      vi.advanceTimersByTime(50);
    });
    box.setScrollHeight(box.scrollHeight() + 285);
    notifyContentResize(content, box.scrollHeight());
    expect(log.scrollTop).toBe(box.maxTop());
    expect(jump()).toBeNull();
    await act(async () => {});

    log.scrollTop = box.maxTop() - 40;
    fireEvent.scroll(log);
    const parked = log.scrollTop;
    expect(jump()).toBeTruthy();
    await emitDelta("thread-a", `${"さらに\n".repeat(25)}下`);
    await act(async () => {
      vi.advanceTimersByTime(50);
    });
    box.setScrollHeight(box.scrollHeight() + 621);
    notifyContentResize(content, box.scrollHeight());
    expect(log.scrollTop).toBe(parked);
  });

  it("does not move when finished thinking is opened while following", async () => {
    messagesA = [
      originalUser,
      {
        id: "m-asst",
        threadId: "thread-a",
        role: "assistant",
        content: "回答です",
        reasoning: "隠れた思考です",
        createdAt: 2,
      },
    ];
    render(<Chat />);
    const summary = await screen.findByText("思考");
    const log = document.querySelector(".chat-log");
    if (!(log instanceof HTMLElement)) throw new Error("chat log missing");
    const box = installLogGeometry(log, { scrollHeight: 800, clientHeight: 120 });
    log.scrollTop = box.maxTop();
    fireEvent.scroll(log);
    const parked = log.scrollTop;
    fireEvent.click(summary);
    box.setScrollHeight(box.scrollHeight() + 180);
    const content = log.querySelector(".chat-log-content");
    if (!(content instanceof HTMLElement)) throw new Error("log content missing");
    notifyContentResize(content, box.scrollHeight());
    expect(log.scrollTop).toBe(parked);
    expect(screen.queryByRole("button", { name: "↓最新へ" })).toBeNull();
  });
});
