import { describe, expect, it } from "vitest";
import { applyChatDelta, isNearBottom, type LiveGeneration } from "./chatLive";

const live = (threadId: string, stream = ""): LiveGeneration => ({
  threadId,
  stream,
  thinking: "",
  toolHint: "",
});

describe("stream routing", () => {
  it("keeps deltas on the originating thread after the view switches", () => {
    const origin = live("thread-a", "途中");
    const ignored = applyChatDelta(origin, {
      threadId: "thread-b",
      text: "混線",
    });
    expect(ignored).toBe(origin);
    const next = applyChatDelta(origin, {
      threadId: "thread-a",
      text: "の続き",
    });
    expect(next.threadId).toBe("thread-a");
    expect(next.stream).toBe("途中の続き");
  });

  it("resets and records reasoning or tool hints only for that thread", () => {
    const withThought = applyChatDelta(live("thread-a"), {
      threadId: "thread-a",
      kind: "reasoning",
      text: "考え",
    });
    expect(withThought.thinking).toBe("考え");
    const withTool = applyChatDelta(withThought, {
      threadId: "thread-a",
      kind: "tool",
      text: "search_index…",
    });
    expect(withTool.toolHint).toBe("search_index…");
    expect(withTool.thinking).toBe("考え");
    const reset = applyChatDelta(withTool, {
      threadId: "thread-a",
      kind: "reset",
      text: "",
    });
    expect(reset).toEqual(live("thread-a"));
  });
});

describe("scroll follow", () => {
  it("treats the bottom slack as still following", () => {
    expect(isNearBottom(0, 800, 120)).toBe(false);
    expect(isNearBottom(632, 800, 120)).toBe(true);
    expect(isNearBottom(800, 800, 120)).toBe(true);
  });
});
