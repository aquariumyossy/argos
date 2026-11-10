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
  base: 2410,
  web: 2662,
  remote: 2455,
  webRemote: 2707,
  remoteOnly: 2417,
  webRemoteOnly: 2670,
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

/**
 * Denominator of the chat meter: the configured limit minus the reply headroom
 * and the tool-result allowance. The numerator is the next request's size from
 * `llm_context_meter`, which measures `assemble_turns`.
 */
export function contextMeterLimit(limit: number): number {
  if (!(limit > 0)) return 0;
  return Math.max(0, limit - replyHeadroomTokens(limit) - toolResultAllowanceTokens(limit));
}

export function contextIsNearLimit(tokens: number, limit: number): boolean {
  return limit > 0 && tokens / limit >= WARN_RATIO;
}

export function contextMeterLabel(tokens: number | null, limit: number): string {
  const jp = (n: number) => n.toLocaleString("ja-JP");
  const used = tokens == null ? "—" : jp(tokens);
  return `${used} / ${jp(limit)} トークン`;
}
