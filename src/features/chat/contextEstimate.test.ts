import { describe, expect, it } from "vitest";
import {
  CHARS_PER_TOKEN,
  WARN_RATIO,
  contextIsNearLimit,
  contextMeterLabel,
  contextUsageTokens,
  estimateTokens,
  messageTokens,
} from "./contextEstimate";

describe("context tokens", () => {
  it("counts Japanese at 1.6 characters a token", () => {
    expect(CHARS_PER_TOKEN).toBe(1.6);
    expect(estimateTokens("あ".repeat(16))).toBe(10);
    expect(estimateTokens("")).toBe(0);
  });

  it("adds a small fixed cost per message and counts tool calls, not thinking", () => {
    expect(messageTokens({ content: "あ".repeat(16), toolCalls: "" })).toBe(14);
    expect(messageTokens({ content: "本文", toolCalls: "search_index" })).toBe(
      estimateTokens("本文") + estimateTokens("search_index") + 4,
    );
  });

  it("sums messages, sources, and the draft", () => {
    const total = contextUsageTokens({
      messages: [{ content: "質問" }, { content: "答え", toolCalls: "[]" }],
      sources: [{ title: "民法", body: "第一条" }],
      draft: "続き",
    });
    expect(total).toBe(
      messageTokens({ content: "質問" }) +
        messageTokens({ content: "答え", toolCalls: "[]" }) +
        estimateTokens("民法") +
        estimateTokens("第一条") +
        estimateTokens("続き"),
    );
  });

  it("warns from 80 percent of the limit", () => {
    expect(WARN_RATIO).toBe(0.8);
    expect(contextIsNearLimit(79, 100)).toBe(false);
    expect(contextIsNearLimit(80, 100)).toBe(true);
    expect(contextIsNearLimit(10, 0)).toBe(false);
  });

  it("labels the meter in tokens", () => {
    expect(contextMeterLabel(12345, 131072)).toBe("12,345 / 131,072 トークン");
  });
});
