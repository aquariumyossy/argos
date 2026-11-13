import { describe, expect, it } from "vitest";
import { PREVIEW_CSP, PREVIEW_SANDBOX, htmlPreviewSrc } from "./htmlPreviewPolicy";

describe("html preview isolation", () => {
  it("allows inline script and style and nothing from the network", () => {
    expect(PREVIEW_SANDBOX).toBe("allow-scripts");
    expect(PREVIEW_CSP).toContain("default-src 'none'");
    expect(PREVIEW_CSP).toContain("script-src 'unsafe-inline' 'unsafe-eval'");
    expect(PREVIEW_CSP).toContain("style-src 'unsafe-inline'");
    expect(PREVIEW_CSP).toContain("connect-src 'none'");
    expect(PREVIEW_CSP).toContain("frame-src 'none'");
    expect(PREVIEW_CSP).toContain("object-src 'none'");
    expect(PREVIEW_CSP).toContain("base-uri 'none'");
    expect(PREVIEW_CSP).toContain("worker-src 'none'");
    expect(PREVIEW_CSP).toContain("sandbox allow-scripts");
    expect(PREVIEW_CSP.includes("allow-same-origin")).toBe(false);
    expect(PREVIEW_CSP.includes("http:")).toBe(false);
    expect(PREVIEW_CSP.includes("https:")).toBe(false);
  });

  it("points the frame at the output file name on the preview origin", () => {
    expect(htmlPreviewSrc("手数料.html", false)).toBe(
      "htmlpreview://localhost/preview?name=%E6%89%8B%E6%95%B0%E6%96%99.html",
    );
    expect(htmlPreviewSrc("a&b.html", true)).toBe(
      "http://htmlpreview.localhost/preview?name=a%26b.html",
    );
  });
});
