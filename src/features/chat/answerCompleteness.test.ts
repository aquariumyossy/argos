import { describe, expect, it } from "vitest";
import { answerCompletenessLabel } from "./answerCompleteness";

describe("answerCompletenessLabel", () => {
  it("marks a retry that still failed", () => {
    expect(answerCompletenessLabel("incomplete")).toBe("不完全な可能性");
  });

  it("shows that a retry rewrote the conclusion", () => {
    expect(answerCompletenessLabel("retried")).toBe("結論を本文に書き直しました");
  });

  it("stays quiet for a normal answer and for a notice", () => {
    expect(answerCompletenessLabel("")).toBe("");
    expect(answerCompletenessLabel(undefined)).toBe("");
    expect(answerCompletenessLabel("notice")).toBe("");
  });
});
