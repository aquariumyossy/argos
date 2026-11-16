/** Kind stored on an assistant row after the one completeness retry. */
export const ANSWER_KIND_RETRIED = "retried";
export const ANSWER_KIND_CONTINUED = "continued";
export const ANSWER_KIND_INCOMPLETE = "incomplete";

/** Small label for a retried answer. Empty when this row was not retried. */
export function answerCompletenessLabel(kind: string | undefined): string {
  if (kind === ANSWER_KIND_INCOMPLETE) return "不完全な可能性";
  if (kind === ANSWER_KIND_CONTINUED) return "途中で切れた回答の続きを補いました";
  if (kind === ANSWER_KIND_RETRIED) return "結論を本文に書き直しました";
  return "";
}
