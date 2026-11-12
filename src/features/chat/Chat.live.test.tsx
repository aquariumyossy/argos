import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import Chat from "./Chat";
import { contextMeterLabel, contextMeterLimit } from "./contextEstimate";

type Handler = (event: { payload: unknown }) => void;

const { mockInvoke, mockWriteText, listeners, focusHandlers } = vi.hoisted(() => ({
  mockInvoke: vi.fn(),
  mockWriteText: vi.fn(async (_text: string) => {}),
  listeners: new Map<string, Handler>(),
  focusHandlers: [] as Array<(event: { payload: boolean }) => void>,
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
    onFocusChanged: async (cb: (event: { payload: boolean }) => void) => {
      focusHandlers.push(cb);
      return () => {
        const index = focusHandlers.indexOf(cb);
        if (index >= 0) focusHandlers.splice(index, 1);
      };
    },
  }),
}));

vi.mock("mermaid", () => ({
  default: {
    initialize: () => {},
    render: async () => ({ svg: "<svg></svg>" }),
  },
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
let sourcesA: Array<Record<string, unknown>> = [];
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

type ObservedResize = {
  callback: ResizeObserverCallback;
  target: Element;
};

const observedResizes: ObservedResize[] = [];

class ContentResizeObserver {
  constructor(private readonly callback: ResizeObserverCallback) {}
  observe(target: Element) {
    observedResizes.push({ callback: this.callback, target });
  }
  unobserve(target: Element) {
    const index = observedResizes.findIndex(
      (entry) => entry.callback === this.callback && entry.target === target,
    );
    if (index >= 0) observedResizes.splice(index, 1);
  }
  disconnect() {
    for (let index = observedResizes.length - 1; index >= 0; index -= 1) {
      if (observedResizes[index].callback === this.callback) observedResizes.splice(index, 1);
    }
  }
}

function notifyResizeObservers(target?: Element) {
  const callbacks: ResizeObserverCallback[] = [];
  for (const entry of observedResizes) {
    if (target && entry.target !== target) continue;
    if (!callbacks.includes(entry.callback)) callbacks.push(entry.callback);
  }
  if (callbacks.length === 0) {
    throw new Error(
      target
        ? "ResizeObserver is not observing this element"
        : "content ResizeObserver is not attached",
    );
  }
  for (const callback of callbacks) callback([], {} as ResizeObserver);
}

function notifyLogResize() {
  notifyResizeObservers();
}

function notifyElementResize(target: Element) {
  notifyResizeObservers(target);
}

function notifyContentResize(content: HTMLElement, height: number) {
  Object.defineProperty(content, "offsetHeight", {
    configurable: true,
    get: () => height,
  });
  notifyLogResize();
}

/** jsdom does not clamp scrollTop. A real log cannot scroll past scrollHeight - clientHeight. */
function installLogGeometry(
  log: HTMLElement,
  initial: { scrollHeight: number; clientHeight: number; clientWidth?: number },
) {
  let scrollHeight = initial.scrollHeight;
  let clientHeight = initial.clientHeight;
  let clientWidth = initial.clientWidth ?? 0;
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
  Object.defineProperty(log, "clientWidth", {
    configurable: true,
    get: () => clientWidth,
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
    setClientWidth(next: number) {
      clientWidth = next;
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
  observedResizes.length = 0;
  focusHandlers.length = 0;
  vi.stubGlobal("ResizeObserver", ContentResizeObserver);
  messagesA = [originalUser];
  sourcesA = [];
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
        return (args?.threadId === threadB.id ? messagesB : messagesA).map((row) => ({
          ...row,
        }));
      case "llm_list_sources":
        return args?.threadId === threadB.id ? [] : sourcesA;
      case "llm_preview_source_file":
        return { chars: 20, units: 1, hardCap: 100 };
      case "llm_file_grain_meter":
        return { tokens: 40, bodyTrimmed: false };
      case "llm_set_source_grain": {
        const onSources = listeners.get("llm-sources-updated");
        queueMicrotask(() => {
          onSources?.({ payload: { threadId: threadA.id } });
        });
        return {
          source: sourcesA[0] ?? null,
          sources: sourcesA,
          removed: 0,
        };
      }
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
    const live = log.querySelector(".chat-msg.assistant.streaming");
    if (!(live instanceof HTMLElement)) throw new Error("streaming message missing");
    Object.defineProperty(live, "offsetHeight", {
      configurable: true,
      get: () => box.scrollHeight(),
    });
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
    notifyLogResize();
    const parked = log.scrollTop;
    fireEvent.click(summary);
    box.setScrollHeight(box.scrollHeight() + 180);
    const content = log.querySelector(".chat-log-content");
    if (!(content instanceof HTMLElement)) throw new Error("log content missing");
    notifyContentResize(content, box.scrollHeight());
    expect(log.scrollTop).toBe(parked);
    expect(screen.queryByRole("button", { name: "↓最新へ" })).toBeNull();
  });

  it("does not jump after thinking is open when the viewport shrinks or grows", async () => {
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
    const box = installLogGeometry(log, { scrollHeight: 1200, clientHeight: 405 });
    log.scrollTop = box.maxTop();
    fireEvent.scroll(log);
    notifyLogResize();
    fireEvent.click(summary);
    box.setScrollHeight(box.scrollHeight() + 736);
    const content = log.querySelector(".chat-log-content");
    if (!(content instanceof HTMLElement)) throw new Error("log content missing");
    notifyContentResize(content, box.scrollHeight());
    const parked = log.scrollTop;
    expect(1200 + 736 - parked - 405).toBe(736);
    box.resize({ clientHeight: 377 });
    notifyLogResize();
    expect(log.scrollTop).toBe(parked);
    box.resize({ clientHeight: 700 });
    notifyLogResize();
    expect(log.scrollTop).toBe(parked);
  });

  it("pins when the composer grows and shrinks a finished thread's log", async () => {
    messagesA = [
      originalUser,
      {
        id: "m-asst",
        threadId: "thread-a",
        role: "assistant",
        content: "回答です",
        createdAt: 2,
      },
    ];
    render(<Chat />);
    await screen.findByText("回答です");
    const log = document.querySelector(".chat-log");
    if (!(log instanceof HTMLElement)) throw new Error("chat log missing");
    const box = installLogGeometry(log, { scrollHeight: 1186, clientHeight: 610 });
    log.scrollTop = box.maxTop();
    fireEvent.scroll(log);
    notifyLogResize();
    expect(log.scrollTop).toBe(576);
    box.resize({ clientHeight: 576 });
    expect(1186 - log.scrollTop - 576).toBe(34);
    notifyLogResize();
    expect(log.scrollTop).toBe(box.maxTop());
    expect(1186 - log.scrollTop - 576).toBe(0);
    expect(screen.queryByRole("button", { name: "↓最新へ" })).toBeNull();
  });

  it("pins when the window shrinks while following", async () => {
    const { log, box } = await startPinnedLog();
    box.resize({ scrollHeight: 1200, clientHeight: 400 });
    log.scrollTop = box.maxTop();
    fireEvent.scroll(log);
    notifyLogResize();
    expect(log.scrollTop).toBe(800);
    box.resize({ clientHeight: 240 });
    expect(1200 - log.scrollTop - 240).toBe(160);
    notifyLogResize();
    expect(log.scrollTop).toBe(box.maxTop());
    expect(1200 - log.scrollTop - 240).toBe(0);
  });

  it("does not jump when earlier thinking opens on a short stretched log", async () => {
    messagesA = [
      originalUser,
      {
        id: "m-old",
        threadId: "thread-a",
        role: "assistant",
        content: "前の回答",
        reasoning: "前の思考",
        createdAt: 2,
      },
    ];
    await renderChat();
    fireEvent.click(screen.getByRole("button", { name: "再送" }));
    await screen.findByText("生成中…");
    const log = document.querySelector(".chat-log");
    if (!(log instanceof HTMLElement)) throw new Error("chat log missing");
    const box = installLogGeometry(log, { scrollHeight: 400, clientHeight: 400 });
    log.scrollTop = 0;
    fireEvent.scroll(log);
    await emitDelta("thread-a", "底");
    expect(log.scrollTop).toBe(0);
    const content = log.querySelector(".chat-log-content");
    const live = log.querySelector(".chat-msg.assistant.streaming");
    if (!(content instanceof HTMLElement) || !(live instanceof HTMLElement)) {
      throw new Error("stretched log is missing");
    }
    let contentHeight = 400;
    let liveHeight = 48;
    Object.defineProperty(content, "offsetHeight", {
      configurable: true,
      get: () => contentHeight,
    });
    Object.defineProperty(live, "offsetHeight", {
      configurable: true,
      get: () => liveHeight,
    });
    notifyElementResize(live);
    liveHeight = 160;
    notifyElementResize(live);
    expect(log.scrollTop).toBe(0);
    expect(jump()).toBeNull();

    const parked = log.scrollTop;
    fireEvent.click(screen.getByText("思考"));
    contentHeight = 560;
    box.setScrollHeight(560);
    notifyElementResize(content);
    await act(async () => {});
    expect(log.scrollTop).toBe(parked);
    expect(jump()).toBeTruthy();
    box.setScrollHeight(640);
    await emitDelta("thread-a", "次のトークン");
    expect(log.scrollTop).toBe(parked);
    expect(jump()).toBeTruthy();
  });

  it("keeps following when an earlier diagram lays out during generation", async () => {
    messagesA = [
      originalUser,
      {
        id: "m-diagram",
        threadId: "thread-a",
        role: "assistant",
        content: "回答です\n\n```mermaid\nflowchart LR\nA-->B\n```",
        createdAt: 2,
      },
    ];
    const { log, box } = await startPinnedLog();
    await screen.findByRole("button", { name: "図" });
    const content = log.querySelector(".chat-log-content");
    const live = log.querySelector(".chat-msg.assistant.streaming");
    if (!(content instanceof HTMLElement) || !(live instanceof HTMLElement)) {
      throw new Error("diagram log is missing");
    }
    let liveHeight = 80;
    Object.defineProperty(live, "offsetHeight", {
      configurable: true,
      get: () => liveHeight,
    });
    notifyElementResize(live);
    box.setScrollHeight(box.scrollHeight() + 197);
    fireEvent.click(screen.getByRole("button", { name: "ソース" }));
    fireEvent.click(screen.getByRole("button", { name: "図" }));
    await act(async () => {
      await Promise.resolve();
    });
    notifyContentResize(content, box.scrollHeight());
    expect(box.scrollHeight() - log.scrollTop - 120).toBe(0);
    expect(jump()).toBeNull();
    box.setScrollHeight(box.scrollHeight() + 40);
    await emitDelta("thread-a", "次のトークン");
    expect(log.scrollTop).toBe(box.maxTop());
    expect(jump()).toBeNull();
  });

  it("keeps following when the log gets narrower during generation", async () => {
    const { log, box } = await startPinnedLog();
    box.setClientWidth(800);
    notifyLogResize();
    expect(log.scrollTop).toBe(box.maxTop());
    box.setClientWidth(560);
    box.setScrollHeight(box.scrollHeight() + 200);
    notifyLogResize();
    expect(box.scrollHeight() - log.scrollTop - log.clientHeight).toBe(0);
    expect(jump()).toBeNull();
    box.setScrollHeight(box.scrollHeight() + 30);
    await emitDelta("thread-a", "狭い");
    expect(log.scrollTop).toBe(box.maxTop());
    expect(jump()).toBeNull();
  });

  it("keeps an earlier heading in place when its thinking is opened during generation", async () => {
    messagesA = [
      originalUser,
      {
        id: "m-old",
        threadId: "thread-a",
        role: "assistant",
        content: "前の回答",
        reasoning: "前の思考",
        createdAt: 2,
      },
    ];
    const { log, box } = await startPinnedLog();
    const parked = log.scrollTop;
    notifyLogResize();
    fireEvent.click(screen.getByText("思考"));
    box.setScrollHeight(box.scrollHeight() + 426);
    const content = log.querySelector(".chat-log-content");
    if (!(content instanceof HTMLElement)) throw new Error("log content missing");
    notifyContentResize(content, box.scrollHeight());
    await act(async () => {});
    expect(log.scrollTop).toBe(parked);
    expect(jump()).toBeTruthy();
    await emitDelta("thread-a", "次のトークン");
    expect(log.scrollTop).toBe(parked);
  });

  it("does not resume when closing earlier thinking clamps onto the bottom", async () => {
    messagesA = [
      originalUser,
      {
        id: "m-old",
        threadId: "thread-a",
        role: "assistant",
        content: "前の回答",
        reasoning: "前の思考",
        createdAt: 2,
      },
    ];
    const { log, box } = await startPinnedLog();
    expect(log.classList.contains("chat-log-no-anchor")).toBe(false);
    fireEvent.click(screen.getByText("思考"));
    box.resize({ scrollHeight: 1200 });
    await act(async () => {});
    expect(jump()).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "↓最新へ" }));
    await act(async () => {});
    expect(log.scrollTop).toBe(box.maxTop());
    expect(jump()).toBeNull();
    expect(log.classList.contains("chat-log-no-anchor")).toBe(false);

    fireEvent.click(screen.getByText("思考"));
    box.resize({ scrollHeight: 800 });
    expect(800 - log.scrollTop - 120).toBe(0);
    expect(jump()).toBeTruthy();
    expect(log.classList.contains("chat-log-no-anchor")).toBe(true);
    const parked = log.scrollTop;
    box.setScrollHeight(box.scrollHeight() + 180);
    await emitDelta("thread-a", "次の段落");
    expect(log.scrollTop).toBe(parked);
    expect(jump()).toBeTruthy();
    log.scrollTop = box.maxTop();
    fireEvent.scroll(log);
    expect(jump()).toBeTruthy();
    expect(log.classList.contains("chat-log-no-anchor")).toBe(true);

    fireEvent.wheel(log, { deltaY: 40 });
    log.scrollTop = box.maxTop();
    fireEvent.scroll(log);
    await act(async () => {});
    expect(jump()).toBeNull();
    expect(log.classList.contains("chat-log-no-anchor")).toBe(false);
    box.setScrollHeight(box.scrollHeight() + 40);
    await emitDelta("thread-a", "再開");
    expect(log.scrollTop).toBe(box.maxTop());
    expect(jump()).toBeNull();
  });

  it("drops the no-anchor class when generation ends and keeps the jump button", async () => {
    messagesA = [
      originalUser,
      {
        id: "m-old",
        threadId: "thread-a",
        role: "assistant",
        content: "前の回答",
        reasoning: "前の思考",
        createdAt: 2,
      },
    ];
    const { log } = await startPinnedLog();
    fireEvent.click(screen.getByText("思考"));
    await act(async () => {});
    expect(log.classList.contains("chat-log-no-anchor")).toBe(true);
    expect(jump()).toBeTruthy();
    const parked = log.scrollTop;
    await act(async () => {
      releaseSend();
    });
    await screen.findByText("完成した回答");
    expect(log.classList.contains("chat-log-no-anchor")).toBe(false);
    expect(jump()).toBeTruthy();
    expect(log.scrollTop).toBe(parked);
    log.scrollTop = Math.max(0, parked - 200);
    fireEvent.scroll(log);
    expect(log.classList.contains("chat-log-no-anchor")).toBe(false);
    expect(jump()).toBeTruthy();
    expect(log.scrollTop).toBe(Math.max(0, parked - 200));
  });

  it("resumes following when enlarging the window clamps onto the bottom", async () => {
    const { log, box } = await startPinnedLog();
    log.scrollTop = 500;
    fireEvent.scroll(log);
    expect(jump()).toBeTruthy();
    box.resize({ clientHeight: 400 });
    expect(log.scrollTop).toBe(400);
    expect(800 - log.scrollTop - 400).toBe(0);
    expect(jump()).toBeNull();
    await emitDelta("thread-a", "続く");
    expect(log.scrollTop).toBe(box.maxTop());
  });

  it("resumes following when a reset shrinks content onto the bottom", async () => {
    const { log, box } = await startPinnedLog();
    log.scrollTop = 500;
    fireEvent.scroll(log);
    expect(jump()).toBeTruthy();
    box.resize({ scrollHeight: 520 });
    expect(log.scrollTop).toBe(400);
    expect(520 - log.scrollTop - 120).toBe(0);
    expect(jump()).toBeNull();
    await emitDelta("thread-a", "再開");
    expect(log.scrollTop).toBe(box.maxTop());
  });

  it("does not pin when the viewport shrinks while follow is off", async () => {
    const { log, box } = await startPinnedLog();
    box.resize({ scrollHeight: 1186, clientHeight: 610 });
    log.scrollTop = box.maxTop();
    fireEvent.scroll(log);
    notifyLogResize();
    await act(async () => {});
    log.scrollTop = box.maxTop() - 80;
    fireEvent.scroll(log);
    const parked = log.scrollTop;
    box.resize({ clientHeight: 576 });
    notifyLogResize();
    expect(log.scrollTop).toBe(parked);
  });
});

const finishedAnswer = {
  id: "m-asst",
  threadId: "thread-a",
  role: "assistant",
  content: "回答です\n\n```mermaid\nflowchart LR\nA-->B\n```",
  reasoning: "隠れた思考です",
  createdAt: 2,
};

describe("finished answer stays put", () => {
  async function openThinkingAwayFromBottom() {
    messagesA = [originalUser, finishedAnswer];
    render(<Chat />);
    const summary = await screen.findByText("思考");
    await screen.findByRole("button", { name: "図" });
    const log = document.querySelector(".chat-log");
    if (!(log instanceof HTMLElement)) throw new Error("chat log missing");
    const box = installLogGeometry(log, { scrollHeight: 1200, clientHeight: 405 });
    log.scrollTop = box.maxTop();
    fireEvent.scroll(log);
    notifyLogResize();
    fireEvent.click(summary);
    box.setScrollHeight(box.scrollHeight() + 736);
    const content = log.querySelector(".chat-log-content");
    if (!(content instanceof HTMLElement)) throw new Error("log content missing");
    notifyContentResize(content, box.scrollHeight());
    const parked = log.scrollTop;
    expect(box.scrollHeight() - parked - log.clientHeight).toBe(736);
    return { log, box, parked };
  }

  async function reloadMessages() {
    const before = mockInvoke.mock.calls.filter((call) => call[0] === "llm_list_messages").length;
    await waitFor(() => {
      const after = mockInvoke.mock.calls.filter((call) => call[0] === "llm_list_messages").length;
      expect(after).toBeGreaterThan(before);
    });
    await act(async () => {});
  }

  it("does not jump when focus reloads the same messages", async () => {
    const { log, parked } = await openThinkingAwayFromBottom();
    const onFocus = focusHandlers[focusHandlers.length - 1];
    if (!onFocus) throw new Error("focus handler missing");
    const pending = reloadMessages();
    await act(async () => {
      onFocus({ payload: true });
    });
    await pending;
    expect(log.scrollTop).toBe(parked);
  });

  it("does not jump when sources update reloads the same messages", async () => {
    const { log, parked } = await openThinkingAwayFromBottom();
    const onSources = listeners.get("llm-sources-updated");
    if (!onSources) throw new Error("sources listener missing");
    const pending = reloadMessages();
    await act(async () => {
      onSources({ payload: { threadId: "thread-a" } });
    });
    await pending;
    expect(log.scrollTop).toBe(parked);
  });

  it("pins when a diagram grows after the log was at the bottom", async () => {
    messagesA = [originalUser, finishedAnswer];
    render(<Chat />);
    await screen.findByRole("button", { name: "図" });
    const log = document.querySelector(".chat-log");
    if (!(log instanceof HTMLElement)) throw new Error("chat log missing");
    const box = installLogGeometry(log, { scrollHeight: 800, clientHeight: 120 });
    log.scrollTop = box.maxTop();
    fireEvent.scroll(log);
    notifyLogResize();
    box.setScrollHeight(box.scrollHeight() + 58);
    fireEvent.click(screen.getByRole("button", { name: "ソース" }));
    fireEvent.click(screen.getByRole("button", { name: "図" }));
    await act(async () => {
      await Promise.resolve();
    });
    expect(800 + 58 - log.scrollTop - 120).toBe(0);
    expect(screen.queryByRole("button", { name: "↓最新へ" })).toBeNull();
  });

  it("pins when a narrower log reflows a finished thread that was at the bottom", async () => {
    messagesA = [originalUser, finishedAnswer];
    render(<Chat />);
    await screen.findByText("回答です");
    const log = document.querySelector(".chat-log");
    if (!(log instanceof HTMLElement)) throw new Error("chat log missing");
    const box = installLogGeometry(log, {
      scrollHeight: 914,
      clientHeight: 616,
      clientWidth: 800,
    });
    log.scrollTop = box.maxTop();
    fireEvent.scroll(log);
    notifyLogResize();
    expect(914 - log.scrollTop - 616).toBe(0);
    box.setClientWidth(560);
    box.setScrollHeight(1277);
    notifyLogResize();
    expect(1277 - log.scrollTop - 616).toBe(0);
    expect(screen.queryByRole("button", { name: "↓最新へ" })).toBeNull();
  });

  it("does not jump when a diagram lays out away from the bottom", async () => {
    const { log, parked } = await openThinkingAwayFromBottom();
    fireEvent.click(screen.getByRole("button", { name: "ソース" }));
    fireEvent.click(screen.getByRole("button", { name: "図" }));
    await act(async () => {
      await Promise.resolve();
    });
    expect(log.scrollTop).toBe(parked);
  });

  it("does not jump when a full-text chip reloads the same messages", async () => {
    sourcesA = [
      {
        id: "src-1",
        threadId: "thread-a",
        sortOrder: 0,
        origin: "attach",
        path: "C:\\case\\準備書面.pdf",
        title: "準備書面.pdf",
        paragraphId: "p1",
        body: "本文",
        query: "",
        createdAt: 1,
        grain: "unit",
      },
    ];
    const { log, parked } = await openThinkingAwayFromBottom();
    const pending = reloadMessages();
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "準備書面.pdf" }));
    });
    await pending;
    expect(mockInvoke).toHaveBeenCalledWith(
      "llm_set_source_grain",
      expect.objectContaining({ id: "src-1", grain: "file" }),
    );
    expect(log.scrollTop).toBe(parked);
  });
});

const COMPOSER_PLACEHOLDER =
  "メッセージを入力（Enter で送信、Shift+Enter で改行）";

describe("Ctrl+Alt+C", () => {
  async function shortcutHandler() {
    await vi.waitFor(() => {
      if (!listeners.get("chat-shortcut-open")) {
        throw new Error("chat-shortcut-open is not listening");
      }
    });
    const handler = listeners.get("chat-shortcut-open");
    if (!handler) throw new Error("chat-shortcut-open is not listening");
    return handler;
  }

  it("focuses the composer of the conversation already open", async () => {
    await renderChat();
    const box = screen.getByPlaceholderText(COMPOSER_PLACEHOLDER);
    box.blur();
    const before = mockInvoke.mock.calls.filter(
      (call) => call[0] === "llm_set_active_thread" || call[0] === "llm_create_thread",
    );
    const handler = await shortcutHandler();
    await act(async () => {
      handler({ payload: { threadId: "thread-a" } });
    });
    await vi.waitFor(() => {
      expect(document.activeElement).toBe(box);
    });
    const after = mockInvoke.mock.calls.filter(
      (call) => call[0] === "llm_set_active_thread" || call[0] === "llm_create_thread",
    );
    expect(after).toEqual(before);
  });

  it("opens another existing conversation and focuses the composer", async () => {
    await renderChat();
    const handler = await shortcutHandler();
    await act(async () => {
      handler({ payload: { threadId: "thread-b" } });
    });
    await screen.findByText("別スレッドの発言");
    await vi.waitFor(() => {
      expect(document.activeElement).toBe(
        screen.getByPlaceholderText(COMPOSER_PLACEHOLDER),
      );
    });
    expect(mockInvoke).toHaveBeenCalledWith("llm_set_active_thread", {
      id: "thread-b",
    });
  });

  it("shows the new empty conversation from the shortcut without creating a second one", async () => {
    const created = {
      id: "thread-new",
      title: "",
      pathPrefix: "",
      noteId: "",
      sortOrder: -1,
      createdAt: 3,
      updatedAt: 3,
    };
    let opened = false;
    const previous = mockInvoke.getMockImplementation();
    mockInvoke.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "llm_list_threads") return opened ? [created] : [];
      if (cmd === "llm_get_active_thread") return opened ? created : null;
      if (cmd === "llm_list_messages" || cmd === "llm_list_sources") return [];
      if (cmd === "llm_create_thread") {
        throw new Error("the shortcut already created the conversation");
      }
      return previous?.(cmd, args);
    });
    render(<Chat />);
    await screen.findByText("会話はまだありません。");
    opened = true;
    const handler = await shortcutHandler();
    await act(async () => {
      handler({ payload: { threadId: created.id } });
    });
    await screen.findByRole("button", { name: "新しい会話" });
    await vi.waitFor(() => {
      expect(document.activeElement).toBe(
        screen.getByPlaceholderText(COMPOSER_PLACEHOLDER),
      );
    });
    expect(mockInvoke.mock.calls.some((call) => call[0] === "llm_create_thread")).toBe(
      false,
    );
  });

  it("starts an empty conversation when the shortcut has no conversation to open", async () => {
    const created = {
      id: "thread-new",
      title: "",
      pathPrefix: "",
      noteId: "",
      sortOrder: -1,
      createdAt: 4,
      updatedAt: 4,
    };
    let threads: Array<typeof created> = [];
    let active: typeof created | null = null;
    const previous = mockInvoke.getMockImplementation();
    mockInvoke.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "llm_list_threads") return threads;
      if (cmd === "llm_get_active_thread") return active;
      if (cmd === "llm_create_thread") {
        threads = [created];
        active = created;
        return created;
      }
      if (cmd === "llm_list_messages" || cmd === "llm_list_sources") return [];
      return previous?.(cmd, args);
    });
    render(<Chat />);
    await screen.findByText("会話はまだありません。");
    const handler = await shortcutHandler();
    await act(async () => {
      handler({ payload: { threadId: "" } });
    });
    await screen.findByRole("button", { name: "新しい会話" });
    expect(mockInvoke).toHaveBeenCalledWith("llm_create_thread", { title: "" });
    await vi.waitFor(() => {
      expect(document.activeElement).toBe(
        screen.getByPlaceholderText(COMPOSER_PLACEHOLDER),
      );
    });
  });

  it("places the caret when the input becomes usable after a reply finishes", async () => {
    const hasFocus = vi.spyOn(document, "hasFocus").mockReturnValue(true);
    try {
      await renderChat();
      fireEvent.click(screen.getByRole("button", { name: "再送" }));
      await screen.findByText("生成中…");
      const during = screen.getByPlaceholderText(
        COMPOSER_PLACEHOLDER,
      ) as HTMLTextAreaElement;
      expect(during.disabled).toBe(true);
      const handler = await shortcutHandler();
      await act(async () => {
        handler({ payload: { threadId: "thread-a" } });
      });
      expect(document.activeElement).not.toBe(during);
      await act(async () => {
        releaseSend();
      });
      await vi.waitFor(() => {
        const box = screen.getByPlaceholderText(COMPOSER_PLACEHOLDER);
        expect((box as HTMLTextAreaElement).disabled).toBe(false);
        expect(document.activeElement).toBe(box);
      });
    } finally {
      hasFocus.mockRestore();
    }
  });

  it("does not pull the caret back after the shortcut focus has landed", async () => {
    const hasFocus = vi.spyOn(document, "hasFocus").mockReturnValue(true);
    try {
      await renderChat();
      const handler = await shortcutHandler();
      await act(async () => {
        handler({ payload: { threadId: "thread-a" } });
      });
      const box = screen.getByPlaceholderText(COMPOSER_PLACEHOLDER);
      await vi.waitFor(() => expect(document.activeElement).toBe(box));
      const search = screen.getByRole("searchbox");
      await act(async () => {
        search.focus();
      });
      expect(document.activeElement).toBe(search);
      const onFocus = focusHandlers[focusHandlers.length - 1];
      if (!onFocus) throw new Error("focus handler missing");
      await act(async () => {
        onFocus({ payload: true });
      });
      await vi.waitFor(() => {
        expect(document.activeElement).not.toBe(box);
      });
    } finally {
      hasFocus.mockRestore();
    }
  });
});

describe("IME composition Enter", () => {
  it("does not send while IME composition is confirming and still sends on a plain Enter", async () => {
    await renderChat();
    const box = screen.getByPlaceholderText(COMPOSER_PLACEHOLDER) as HTMLTextAreaElement;
    fireEvent.change(box, { target: { value: "変換中の文章" } });

    const composing = fireEvent.keyDown(box, {
      key: "Enter",
      isComposing: true,
      keyCode: 229,
    });
    expect(composing).toBe(true);
    expect(sendCalls()).toHaveLength(0);
    expect(box.value).toBe("変換中の文章");

    const processKey = fireEvent.keyDown(box, {
      key: "Enter",
      isComposing: false,
      keyCode: 229,
    });
    expect(processKey).toBe(true);
    expect(sendCalls()).toHaveLength(0);
    expect(box.value).toBe("変換中の文章");

    fireEvent.keyDown(box, { key: "Enter" });
    expect(sendCalls()).toEqual([
      [
        "llm_send",
        { threadId: "thread-a", content: "変換中の文章", webSearch: false },
      ],
    ]);
  });

  it("does not rename a conversation on IME composition Enter", async () => {
    const previous = mockInvoke.getMockImplementation();
    mockInvoke.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "llm_rename_thread") {
        return { ...threadA, title: String(args?.title ?? "") };
      }
      return previous?.(cmd, args);
    });

    await renderChat();
    fireEvent.doubleClick(screen.getByRole("button", { name: "契約の相談" }));
    const input = document.querySelector(".chat-rename");
    if (!(input instanceof HTMLInputElement)) throw new Error("rename field missing");
    fireEvent.change(input, { target: { value: "新しい相談" } });

    const composing = fireEvent.keyDown(input, {
      key: "Enter",
      isComposing: true,
      keyCode: 229,
    });
    expect(composing).toBe(true);
    expect(mockInvoke.mock.calls.some((call) => call[0] === "llm_rename_thread")).toBe(false);
    expect(input.value).toBe("新しい相談");
    expect(document.querySelector(".chat-rename")).toBe(input);

    fireEvent.keyDown(input, { key: "Enter" });
    await act(async () => {
      await Promise.resolve();
    });
    expect(mockInvoke).toHaveBeenCalledWith("llm_rename_thread", {
      id: "thread-a",
      title: "新しい相談",
    });
  });
});
