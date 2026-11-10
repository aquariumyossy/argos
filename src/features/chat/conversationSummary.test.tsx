import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import {
  contextIsNearLimit,
  contextMeterLimit,
} from "./contextEstimate";
import {
  SUMMARY_FAILURE_WARNING,
  SUMMARY_STATUS_TEXT,
  SUMMARY_TITLE,
  SummaryAfterMessage,
  applyChatDelta,
  emptyChatLive,
  noticeFromSend,
} from "./conversationSummary";

describe("conversation summary", () => {
  it("triggers at 80 percent of the meter denominator, not the raw limit", () => {
    const limit = 120_000;
    const denom = contextMeterLimit(limit);
    expect(denom).toBe(78_000);
    expect(62_400 / denom).toBeCloseTo(0.8);
    expect(contextIsNearLimit(62_400, denom)).toBe(true);
    expect(contextIsNearLimit(62_399, denom)).toBe(false);
    expect(limit * 0.8).toBe(96_000);
    expect(contextIsNearLimit(62_400, limit)).toBe(false);
    expect(contextIsNearLimit(70_000, denom)).toBe(true);
    expect(70_000 < limit * 0.8).toBe(true);
  });

  it("places the collapsed summary after the covered message and keeps the originals", () => {
    const messages = [
      { id: "u1", content: "最初の質問本文" },
      { id: "a1", content: "最初の答え" },
      { id: "u2", content: "直前の質問" },
    ];
    const summary = "2024年3月1日に甲は100万円。";
    const { container } = render(
      <div>
        {messages.map((message) => (
          <div key={message.id}>
            <article>{message.content}</article>
            <SummaryAfterMessage
              messageId={message.id}
              throughId="a1"
              summary={summary}
            />
          </div>
        ))}
      </div>,
    );
    const articles = [...container.querySelectorAll("article")];
    expect(articles.map((node) => node.textContent)).toEqual([
      "最初の質問本文",
      "最初の答え",
      "直前の質問",
    ]);
    articles.forEach((node) => expect(node.textContent).not.toContain("100万円"));
    const details = container.querySelectorAll("details.chat-summary");
    expect(details).toHaveLength(1);
    expect(details[0].querySelector("summary")?.textContent).toBe(SUMMARY_TITLE);
    expect((details[0] as HTMLDetailsElement).open).toBe(false);
    expect(details[0].textContent).toContain("2024年3月1日");
    expect(details[0].textContent).toContain("100万円");
    const answer = articles[1].parentElement;
    expect(answer?.querySelector("article")?.nextElementSibling).toBe(details[0]);
  });

  it("keeps the summarizing status out of the answer", () => {
    const status = applyChatDelta(emptyChatLive, "status", SUMMARY_STATUS_TEXT);
    expect(status.status).toBe(SUMMARY_STATUS_TEXT);
    expect(status.stream).toBe("");
    const answer = applyChatDelta(status, "content", "本文です");
    expect(answer.stream).toBe("本文です");
    expect(answer.status).toBe("");
    expect(answer.stream).not.toContain("要約しています");
  });

  it("shows the summary failure warning instead of the trim notice", () => {
    expect(SUMMARY_FAILURE_WARNING).toBe(
      "要約できなかったため、古い部分を省いて送りました",
    );
    expect(noticeFromSend(SUMMARY_FAILURE_WARNING, true)).toBe(SUMMARY_FAILURE_WARNING);
    expect(noticeFromSend("", true)).toBe(
      "トークン上限のため、古いやり取りや出典から切り詰めて送りました。",
    );
    expect(noticeFromSend(null, false)).toBe("");
  });
});
