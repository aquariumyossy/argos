import { describe, expect, it } from "vitest";
import { shouldRestoreDraft } from "./refusedSend";

describe("refused question", () => {
  it("puts the typed question back only when it does not fit", () => {
    expect(
      shouldRestoreDraft(
        "この質問はモデルのコンテキストに入りません。短くしてから送ってください。",
      ),
    ).toBe(true);
    expect(
      shouldRestoreDraft(
        "この会話はモデルのコンテキストに対して長すぎます。新しい会話に切り替えるか、古い内容を減らしてください。",
      ),
    ).toBe(false);
    expect(shouldRestoreDraft(null)).toBe(false);
    expect(shouldRestoreDraft(undefined)).toBe(false);
  });
});
