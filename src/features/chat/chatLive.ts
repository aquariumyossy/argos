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

/** Treat a few pixels of slack as the bottom so subpixel layout does not unpin. */
export const SCROLL_FOLLOW_SLACK_PX = 48;

export function isNearBottom(
  scrollTop: number,
  scrollHeight: number,
  clientHeight: number,
  slack = SCROLL_FOLLOW_SLACK_PX,
): boolean {
  return scrollHeight - scrollTop - clientHeight <= slack;
}
