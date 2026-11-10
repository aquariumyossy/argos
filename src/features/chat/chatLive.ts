export type LiveGeneration = {
  threadId: string;
  stream: string;
  thinking: string;
  toolHint: string;
  status: string;
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
    return { ...live, stream: "", thinking: "", toolHint: "", status: "" };
  }
  if (kind === "reasoning") {
    return { ...live, status: "", thinking: live.thinking + delta.text };
  }
  if (kind === "tool") {
    return {
      ...live,
      status: "",
      toolHint: delta.text || "インデックスを検索しています…",
    };
  }
  if (kind === "status") {
    return { ...live, status: delta.text };
  }
  return { ...live, status: "", stream: live.stream + delta.text };
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

/** Ids or count. Same messages in a new array do not count. */
export function messageListKey(ids: readonly string[]): string {
  return ids.join("\0");
}

/**
 * Outside a live reply, only a real message-list change or an already-bottom
 * log may jump. A reload of the same ids must not.
 */
export function shouldFollowScrollToBottom(input: {
  live: boolean;
  listChanged: boolean;
  atBottom: boolean;
}): boolean {
  if (input.live) return true;
  return input.listChanged || input.atBottom;
}

/**
 * Stop only when scrollTop decreased and the log is no longer at the bottom.
 * Reaching the bottom resumes following in either direction, including a clamp.
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
  if (atBottom) return { follow: true, anchor: scrollTop };
  if (scrollTop < anchor) return { follow: false, anchor: scrollTop };
  return { follow, anchor: scrollTop };
}
