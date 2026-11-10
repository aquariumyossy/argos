import { describe, expect, it } from "vitest";
import {
  applyChatDelta,
  distanceFromBottom,
  followAfterUserScroll,
  messageListKey,
  shouldFollowScrollToBottom,
  type LiveGeneration,
} from "./chatLive";

const live = (threadId: string, stream = ""): LiveGeneration => ({
  threadId,
  stream,
  thinking: "",
  toolHint: "",
  status: "",
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

  it("keeps a summary status out of the answer stream", () => {
    const status = applyChatDelta(live("thread-a", "途中"), {
      threadId: "thread-a",
      kind: "status",
      text: "会話が長くなったため、古いやり取りを要約しています…",
    });
    expect(status.stream).toBe("途中");
    expect(status.status).toContain("要約しています");
    const answer = applyChatDelta(status, {
      threadId: "thread-a",
      text: "続き",
    });
    expect(answer.status).toBe("");
    expect(answer.stream).toBe("途中続き");
  });
});

const VIEW_H = 120;
const LONG_H = 800;
const BOTTOM = LONG_H - VIEW_H;

describe("scroll follow", () => {
  function step(
    follow: boolean,
    anchor: number,
    scrollTop: number,
    scrollHeight = LONG_H,
    clientHeight = VIEW_H,
  ) {
    return followAfterUserScroll({
      follow,
      anchor,
      scrollTop,
      scrollHeight,
      clientHeight,
    });
  }

  it("keeps following when a shrink or resize leaves the log at the bottom", () => {
    const shrunk = step(true, BOTTOM, BOTTOM - 40, LONG_H - 40, VIEW_H);
    expect(distanceFromBottom(BOTTOM - 40, LONG_H - 40, VIEW_H)).toBe(0);
    expect(shrunk).toEqual({ follow: true, anchor: BOTTOM - 40 });

    const taller = step(true, BOTTOM, LONG_H - 200, LONG_H, 200);
    expect(distanceFromBottom(LONG_H - 200, LONG_H, 200)).toBe(0);
    expect(taller.follow).toBe(true);
  });

  it("does not stop when a wheel or key leaves the scroll position unchanged", () => {
    expect(step(true, 0, 0, 80, VIEW_H)).toEqual({ follow: true, anchor: 0 });
    expect(step(true, BOTTOM, BOTTOM)).toEqual({ follow: true, anchor: BOTTOM });
  });

  it("stays stopped after scrolling up 16px and then 1px down", () => {
    const up = step(true, BOTTOM, BOTTOM - 16);
    expect(up).toEqual({ follow: false, anchor: BOTTOM - 16 });
    const down = step(false, up.anchor, BOTTOM - 15);
    expect(distanceFromBottom(BOTTOM - 15, LONG_H, VIEW_H)).toBe(15);
    expect(down).toEqual({ follow: false, anchor: BOTTOM - 15 });
  });

  it("does not jump for a same-id reload or a diagram layout away from the bottom", () => {
    const ids = messageListKey(["u1", "a1"]);
    expect(messageListKey(["u1", "a1"])).toBe(ids);
    expect(messageListKey(["u1", "a1", "a2"])).not.toBe(ids);
    expect(
      shouldFollowScrollToBottom({ live: false, listChanged: false, atBottom: false }),
    ).toBe(false);
    expect(
      shouldFollowScrollToBottom({ live: false, listChanged: true, atBottom: false }),
    ).toBe(true);
    expect(
      shouldFollowScrollToBottom({ live: false, listChanged: false, atBottom: true }),
    ).toBe(true);
    expect(
      shouldFollowScrollToBottom({ live: true, listChanged: false, atBottom: false }),
    ).toBe(true);
  });

  it("resumes when a clamp moves up onto the bottom", () => {
    const up = step(true, BOTTOM, 500);
    expect(up.follow).toBe(false);
    const enlarged = step(false, 500, 400, LONG_H, 400);
    expect(distanceFromBottom(400, LONG_H, 400)).toBe(0);
    expect(enlarged).toEqual({ follow: true, anchor: 400 });
    const reset = step(false, 500, 400, 520, VIEW_H);
    expect(distanceFromBottom(400, 520, VIEW_H)).toBe(0);
    expect(reset).toEqual({ follow: true, anchor: 400 });
  });

  it("stops on an 8px upward step and resumes only at the bottom", () => {
    const up = step(true, BOTTOM, BOTTOM - 8);
    expect(up.follow).toBe(false);
    const notYet = step(false, up.anchor, BOTTOM - 3);
    expect(notYet.follow).toBe(false);
    const edge = step(false, notYet.anchor, BOTTOM - 2);
    expect(distanceFromBottom(BOTTOM - 2, LONG_H, VIEW_H)).toBe(2);
    expect(edge.follow).toBe(true);
  });
});
