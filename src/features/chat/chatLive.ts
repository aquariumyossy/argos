export type LiveGeneration = {
  threadId: string;
  stream: string;
  thinking: string;
  toolHint: string;
};

export type ChatDelta = {
  threadId: string;
  text: string;
  kind?: string;
};

/** Append a streamed delta only when it belongs to the generation already running. */
export function applyChatDelta(live: LiveGeneration, delta: ChatDelta): LiveGeneration {
  if (delta.threadId !== live.threadId) return live;
  const kind = delta.kind ?? "content";
  if (kind === "reset") {
    return { ...live, stream: "", thinking: "", toolHint: "" };
  }
  if (kind === "reasoning") {
    return { ...live, thinking: live.thinking + delta.text };
  }
  if (kind === "tool") {
    return {
      ...live,
      toolHint: delta.text || "インデックスを検索しています…",
    };
  }
  return { ...live, stream: live.stream + delta.text };
}

/** Subpixel rounding still counts as the bottom. Anything larger is scrolled up. */
export const SCROLL_BOTTOM_SLACK_PX = 2;

export function distanceFromBottom(
  scrollTop: number,
  scrollHeight: number,
  clientHeight: number,
): number {
  return scrollHeight - scrollTop - clientHeight;
}

export function isAtBottom(
  scrollTop: number,
  scrollHeight: number,
  clientHeight: number,
  slack = SCROLL_BOTTOM_SLACK_PX,
): boolean {
  return distanceFromBottom(scrollTop, scrollHeight, clientHeight) <= slack;
}

/**
 * Stop only when scrollTop decreased and the log is no longer at the bottom.
 * A clamp, shrink, or resize that leaves it at the bottom does not stop.
 * Resume only when a later scroll rests at the bottom.
 */
export function followAfterUserScroll(input: {
  follow: boolean;
  scrollTop: number;
  anchor: number;
  scrollHeight: number;
  clientHeight: number;
}): { follow: boolean; anchor: number } {
  const { scrollTop, anchor, scrollHeight, clientHeight, follow } = input;
  const atBottom = isAtBottom(scrollTop, scrollHeight, clientHeight);
  if (scrollTop < anchor) {
    return { follow: atBottom ? follow : false, anchor: scrollTop };
  }
  if (atBottom) return { follow: true, anchor: scrollTop };
  return { follow, anchor: scrollTop };
}
