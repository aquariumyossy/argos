import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { METER_DEBOUNCE_MS, useChatContextMeter } from "./useChatContextMeter";

const { mockInvoke } = vi.hoisted(() => ({
  mockInvoke: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => mockInvoke(...args),
}));

const base = {
  threadId: "thread-a",
  noteId: "",
  pathPrefix: "",
  draft: "",
  webSearch: false,
  maxContextTokens: 120_000,
  searchMode: "local",
  messages: [],
  sources: [],
};

beforeEach(() => {
  vi.useFakeTimers();
  mockInvoke.mockReset();
  mockInvoke.mockResolvedValue(64_968);
});

afterEach(() => {
  vi.useRealTimers();
});

describe("chat context meter", () => {
  it("waits out a burst of keystrokes and then measures once", async () => {
    const { rerender } = renderHook(
      (props: typeof base) => useChatContextMeter(props),
      { initialProps: base },
    );
    rerender({ ...base, draft: "あ" });
    rerender({ ...base, draft: "あい" });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(METER_DEBOUNCE_MS - 1);
    });
    expect(mockInvoke).not.toHaveBeenCalled();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1);
    });
    expect(mockInvoke).toHaveBeenCalledTimes(1);
    expect(mockInvoke).toHaveBeenCalledWith("llm_context_meter", {
      threadId: "thread-a",
      draft: "あい",
      webSearch: false,
    });
  });

  it("measures again when the context limit changes", async () => {
    const { rerender } = renderHook(
      (props: typeof base) => useChatContextMeter(props),
      { initialProps: base },
    );
    await act(async () => {
      await vi.advanceTimersByTimeAsync(METER_DEBOUNCE_MS);
    });
    expect(mockInvoke).toHaveBeenCalledTimes(1);
    rerender({ ...base, maxContextTokens: 32_000 });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(METER_DEBOUNCE_MS);
    });
    expect(mockInvoke).toHaveBeenCalledTimes(2);
  });

  it("drops the previous thread's warning until the new reading arrives", async () => {
    const { result, rerender } = renderHook(
      (props: typeof base) => useChatContextMeter(props),
      { initialProps: base },
    );
    await act(async () => {
      await vi.advanceTimersByTimeAsync(METER_DEBOUNCE_MS);
    });
    expect(result.current.used).toBe(64_968);
    expect(result.current.warn).toBe(true);

    rerender({ ...base, threadId: "thread-b" });
    expect(result.current.used).toBeNull();
    expect(result.current.warn).toBe(false);
    expect(result.current.limit).toBe(78_000);
  });

  it("clears the reading when the meter command fails", async () => {
    const { result, rerender } = renderHook(
      (props: typeof base) => useChatContextMeter(props),
      { initialProps: base },
    );
    await act(async () => {
      await vi.advanceTimersByTimeAsync(METER_DEBOUNCE_MS);
    });
    expect(result.current.warn).toBe(true);

    mockInvoke.mockRejectedValueOnce(new Error("db"));
    rerender({ ...base, draft: "続き" });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(METER_DEBOUNCE_MS);
    });
    expect(result.current.used).toBeNull();
    expect(result.current.warn).toBe(false);
  });
});
