/** Japanese text is about 1.6 characters per token. Same default as the server. */
export const CHARS_PER_TOKEN = 1.6;
export const WARN_RATIO = 0.8;
/** In-turn tool results. Reserved so a send stays inside the configured limit. */
export const TOOL_RESULT_BUDGET_RATIO = 0.25;
/** Share of the context window reserved for the model's reply. */
export const REPLY_HEADROOM_RATIO = 0.1;

/**
 * Token counts of `tools_schema` (compact JSON), measured with the same
 * 1.6 chars/token estimate as the server. The Rust test
 * `tool_schema_token_counts_match_the_chat_meter` locks these numbers.
 */
export const TOOL_SCHEMA_TOKENS = {
  base: 2105,
  web: 2357,
  remote: 2150,
  webRemote: 2402,
  remoteOnly: 2112,
  webRemoteOnly: 2365,
} as const;

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

export function replyHeadroomTokens(limit: number): number {
  if (!(limit > 0)) return 0;
  return Math.floor(limit * REPLY_HEADROOM_RATIO);
}

export function toolResultAllowanceTokens(limit: number): number {
  if (!(limit > 0)) return 0;
  return Math.floor(limit * TOOL_RESULT_BUDGET_RATIO);
}

/** Same character estimate the server uses for the tool-schema JSON. */
export function toolSchemaTokens(schemaJson: string): number {
  return estimateTokens(schemaJson || "");
}

export function toolSchemaTokenCount(opts: {
  webSearch: boolean;
  remoteIndex: boolean;
  remoteOnly: boolean;
}): number {
  if (opts.remoteOnly && opts.webSearch) return TOOL_SCHEMA_TOKENS.webRemoteOnly;
  if (opts.remoteOnly) return TOOL_SCHEMA_TOKENS.remoteOnly;
  if (opts.webSearch && opts.remoteIndex) return TOOL_SCHEMA_TOKENS.webRemote;
  if (opts.webSearch) return TOOL_SCHEMA_TOKENS.web;
  if (opts.remoteIndex) return TOOL_SCHEMA_TOKENS.remote;
  return TOOL_SCHEMA_TOKENS.base;
}

export function contextUsageTokens(parts: {
  messages: { content?: string; toolCalls?: string; kind?: string }[];
  sources: { body?: string; title?: string }[];
  draft: string;
  schemaTokens?: number;
  limit?: number;
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
  if (parts.schemaTokens && parts.schemaTokens > 0) {
    total += parts.schemaTokens;
  }
  if (parts.limit && parts.limit > 0) {
    total += replyHeadroomTokens(parts.limit) + toolResultAllowanceTokens(parts.limit);
  }
  return total;
}

export function contextIsNearLimit(tokens: number, limit: number): boolean {
  return limit > 0 && tokens / limit >= WARN_RATIO;
}

export function contextMeterLabel(tokens: number, limit: number): string {
  const jp = (n: number) => n.toLocaleString("ja-JP");
  return `${jp(tokens)} / ${jp(limit)} トークン`;
}
