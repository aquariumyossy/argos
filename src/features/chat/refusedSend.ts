const QUESTION_DOES_NOT_FIT = "この質問はモデルのコンテキストに入りません";

/** A question that cannot fit is deleted from history, so the typed text goes back to the box. */
export function shouldRestoreDraft(error: string | null | undefined): boolean {
  return typeof error === "string" && error.includes(QUESTION_DOES_NOT_FIT);
}
