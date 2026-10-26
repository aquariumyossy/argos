/** Japanese text is about 1.6 characters per token. Same default as LexCrew-Doc. */
export const CHARS_PER_TOKEN = 1.6;
export const WARN_RATIO = 0.8;
export const TOOL_RESULT_BUDGET_RATIO = 0.25;

export function charCount(text: string): number {
  return [...(text || "")].length;
}

export function estimateTokens(text: string): number {
  return Math.ceil(charCount(text) / CHARS_PER_TOKEN);
}

/** Role and delimiters cost a few tokens, matching the server-side count. */
export function messageTokens(message: {
  content?: string;
  toolCalls?: string;
}): number {
  return estimateTokens(message.content || "") + estimateTokens(message.toolCalls || "") + 4;
}

export function contextUsageTokens(parts: {
  messages: { content?: string; toolCalls?: string; kind?: string }[];
  sources: { body?: string; title?: string }[];
  draft: string;
}): number {
  let total = 0;
  for (const message of parts.messages) {
    if (message.kind === "notice") continue;
    total += messageTokens(message);
  }
  for (const source of parts.sources) {
    total += estimateTokens(source.body || "") + estimateTokens(source.title || "");
  }
  total += estimateTokens(parts.draft);
  return total;
}

export function contextIsNearLimit(tokens: number, limit: number): boolean {
  return limit > 0 && tokens / limit >= WARN_RATIO;
}

export function contextMeterLabel(tokens: number, limit: number): string {
  const jp = (n: number) => n.toLocaleString("ja-JP");
  return `${jp(tokens)} / ${jp(limit)} トークン`;
}
