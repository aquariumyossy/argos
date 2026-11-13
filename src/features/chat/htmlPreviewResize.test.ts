import { describe, expect, it } from "vitest";
import { HTML_PREVIEW_MIN_HEIGHT, clampPreviewHeight } from "./HtmlPreview";

describe("in-chat preview height", () => {
  it("stays at the default height when dragged shorter than that", () => {
    expect(clampPreviewHeight(HTML_PREVIEW_MIN_HEIGHT - 80, 900)).toBe(
      HTML_PREVIEW_MIN_HEIGHT,
    );
    expect(clampPreviewHeight(Number.NaN, 900)).toBe(HTML_PREVIEW_MIN_HEIGHT);
  });

  it("grows with the drag and stops at the chat view height", () => {
    expect(clampPreviewHeight(HTML_PREVIEW_MIN_HEIGHT + 40, 900)).toBe(
      HTML_PREVIEW_MIN_HEIGHT + 40,
    );
    expect(clampPreviewHeight(5000, 640)).toBe(640);
    expect(clampPreviewHeight(HTML_PREVIEW_MIN_HEIGHT + 10, 80)).toBe(
      HTML_PREVIEW_MIN_HEIGHT,
    );
  });
});
