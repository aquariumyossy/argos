/** Japanese text is about 1.6 characters per token. Same default as the server. */
export const CHARS_PER_TOKEN = 1.6;
export const WARN_RATIO = 0.8;
/** In-turn tool results. Reserved so a send stays inside the configured limit. */
export const TOOL_RESULT_BUDGET_RATIO = 0.25;
/** Share of the context window reserved for the model's reply. */
export const REPLY_HEADROOM_RATIO = 0.1;

export function charCount(text: string): number {
  return [...(text || "")].length;
}

export function estimateTokens(text: string): number {
  return Math.ceil(charCount(text) / CHARS_PER_TOKEN);
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

/**
 * Denominator of the chat meter: the configured limit minus the reply headroom
 * and the tool-result allowance. The numerator is the next request's size from
 * `llm_context_meter`, which measures `assemble_turns`.
 */
export function contextMeterLimit(limit: number): number {
  if (!(limit > 0)) return 0;
  return Math.max(0, limit - replyHeadroomTokens(limit) - toolResultAllowanceTokens(limit));
}

/**
 * Full-file confirm. `measuredTokens` is `llm_file_grain_meter`: the next
 * request with that pending attachment counted in full, on the same basis as
 * the meter. The limit is the meter denominator, not the raw setting.
 */
export function fileGrainExceedsMeter(measuredTokens: number, maxContextTokens: number): boolean {
  return measuredTokens > contextMeterLimit(maxContextTokens);
}

export function fileGrainConfirmMessage(measuredTokens: number, maxContextTokens: number): string {
  const jp = (n: number) => n.toLocaleString("ja-JP");
  const limit = contextMeterLimit(maxContextTokens);
  return `全文にすると約 ${jp(measuredTokens)} トークンです。送信の上限は ${jp(limit)} トークンです。全文にしますか？`;
}

export function contextIsNearLimit(tokens: number, limit: number): boolean {
  return limit > 0 && tokens / limit >= WARN_RATIO;
}

export function contextMeterLabel(tokens: number | null, limit: number): string {
  const jp = (n: number) => n.toLocaleString("ja-JP");
  const used = tokens == null ? "—" : jp(tokens);
  return `${used} / ${jp(limit)} トークン`;
}
