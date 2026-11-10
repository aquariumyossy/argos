import { describe, expect, it } from "vitest";
import {
  CHARS_PER_TOKEN,
  REPLY_HEADROOM_RATIO,
  TOOL_RESULT_BUDGET_RATIO,
  WARN_RATIO,
  contextIsNearLimit,
  contextMeterLabel,
  contextMeterLimit,
  estimateTokens,
  fileGrainConfirmMessage,
  replyHeadroomTokens,
  toolResultAllowanceTokens,
  toolSchemaTokens,
} from "./contextEstimate";

describe("context tokens", () => {
  it("counts Japanese at 1.6 characters a token", () => {
    expect(CHARS_PER_TOKEN).toBe(1.6);
    expect(estimateTokens("あ".repeat(16))).toBe(10);
    expect(estimateTokens("")).toBe(0);
  });

  it("warns from 80 percent of the limit", () => {
    expect(WARN_RATIO).toBe(0.8);
    expect(contextIsNearLimit(79, 100)).toBe(false);
    expect(contextIsNearLimit(80, 100)).toBe(true);
    expect(contextIsNearLimit(10, 0)).toBe(false);
  });

  it("labels the meter in tokens", () => {
    expect(contextMeterLabel(12345, 131072)).toBe("12,345 / 131,072 トークン");
    expect(contextMeterLabel(null, 78000)).toBe("— / 78,000 トークン");
  });

  it("counts schema text the same way as other text", () => {
    expect(REPLY_HEADROOM_RATIO).toBe(0.1);
    expect(toolSchemaTokens("あ".repeat(16))).toBe(10);
    expect(toolSchemaTokens("")).toBe(0);
    const limit = 260_000;
    expect(replyHeadroomTokens(limit)).toBe(26_000);
    expect(toolResultAllowanceTokens(limit)).toBe(65_000);
    expect(toolResultAllowanceTokens(limit)).toBe(Math.floor(limit * TOOL_RESULT_BUDGET_RATIO));
  });

  it("sets the denominator to the limit minus unused reserves", () => {
    const limit = 120_000;
    expect(replyHeadroomTokens(limit)).toBe(12_000);
    expect(toolResultAllowanceTokens(limit)).toBe(30_000);
    expect(contextMeterLimit(limit)).toBe(78_000);
    expect(contextMeterLimit(0)).toBe(0);
    expect(contextMeterLabel(2_410, contextMeterLimit(limit))).toBe("2,410 / 78,000 トークン");
    expect(contextIsNearLimit(62_400, contextMeterLimit(limit))).toBe(true);
    expect(contextIsNearLimit(62_399, contextMeterLimit(limit))).toBe(false);
  });

  it("states the assembled size and the meter limit in the full-file confirm", () => {
    expect(contextMeterLimit(131_072)).toBe(85_197);
    expect(fileGrainConfirmMessage(80_087, 131_072)).toBe(
      "全文にすると約 80,087 トークンです。送信の上限は 85,197 トークンです。全文にしますか？",
    );
  });
});
