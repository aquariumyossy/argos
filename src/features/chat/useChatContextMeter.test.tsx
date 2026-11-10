import { useLayoutEffect } from "react";
import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { contextMeterLabel } from "./contextEstimate";
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

  it("hides the previous numerator until a reading for the new limit arrives", async () => {
    const seen: { limit: number; used: number | null; warn: boolean }[] = [];
    const { result, rerender } = renderHook(
      (props: typeof base) => {
        const meter = useChatContextMeter(props);
        useLayoutEffect(() => {
          seen.push({ limit: meter.limit, used: meter.used, warn: meter.warn });
        });
        return meter;
      },
      { initialProps: base },
    );
    await act(async () => {
      await vi.advanceTimersByTimeAsync(METER_DEBOUNCE_MS);
    });
    expect(result.current.used).toBe(64_968);
    expect(result.current.limit).toBe(78_000);
    expect(result.current.warn).toBe(true);

    mockInvoke.mockResolvedValue(10);
    rerender({ ...base, maxContextTokens: 32_000 });
    expect(result.current.used).toBeNull();
    expect(result.current.warn).toBe(false);
    expect(result.current.limit).toBe(20_800);
    expect(contextMeterLabel(result.current.used, result.current.limit)).toBe(
      "— / 20,800 トークン",
    );
    const forNewLimit = seen.filter((row) => row.limit === 20_800);
    expect(forNewLimit.length).toBeGreaterThan(0);
    expect(forNewLimit.every((row) => row.used == null && row.warn === false)).toBe(true);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(METER_DEBOUNCE_MS);
    });
    expect(result.current.used).toBe(10);
    expect(result.current.warn).toBe(false);
    expect(result.current.limit).toBe(20_800);
  });

  it("hides the numerator when the setting changes but the denominator does not", async () => {
    const first = { ...base, maxContextTokens: 16_387 };
    const { result, rerender } = renderHook(
      (props: typeof base) => useChatContextMeter(props),
      { initialProps: first },
    );
    await act(async () => {
      await vi.advanceTimersByTimeAsync(METER_DEBOUNCE_MS);
    });
    expect(result.current.used).toBe(64_968);
    const denom = result.current.limit;
    rerender({ ...base, maxContextTokens: 16_388 });
    expect(result.current.limit).toBe(denom);
    expect(result.current.used).toBeNull();
    expect(result.current.warn).toBe(false);
    expect(contextMeterLabel(result.current.used, result.current.limit)).toBe(
      `— / ${denom.toLocaleString("ja-JP")} トークン`,
    );
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

  it("does not commit the previous thread's reading on the new thread", async () => {
    const seen: { thread: string; used: number | null; warn: boolean }[] = [];
    const { rerender } = renderHook(
      (props: typeof base) => {
        const meter = useChatContextMeter(props);
        useLayoutEffect(() => {
          seen.push({ thread: props.threadId, used: meter.used, warn: meter.warn });
        });
        return meter;
      },
      { initialProps: base },
    );
    await act(async () => {
      await vi.advanceTimersByTimeAsync(METER_DEBOUNCE_MS);
    });
    rerender({ ...base, threadId: "thread-b" });
    const forB = seen.filter((row) => row.thread === "thread-b");
    expect(seen.some((row) => row.thread === "thread-a" && row.used === 64_968 && row.warn)).toBe(
      true,
    );
    expect(forB.length).toBeGreaterThan(0);
    expect(forB.every((row) => row.used == null && row.warn === false)).toBe(true);
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
