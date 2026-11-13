import { describe, expect, it } from "vitest";
import {
  PREVIEW_CSP,
  PREVIEW_SANDBOX,
  SHELL_CSP,
  htmlPreviewSrc,
} from "./htmlPreviewPolicy";

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

  it("loads the chat preview through the shell, not the user document", () => {
    expect(htmlPreviewSrc("手数料.html", false)).toBe(
      "htmlpreview://localhost/shell?name=%E6%89%8B%E6%95%B0%E6%96%99.html",
    );
    expect(htmlPreviewSrc("a&b.html", true)).toBe(
      "http://htmlpreview.localhost/shell?name=a%26b.html",
    );
    expect(SHELL_CSP).toContain("frame-src 'self'");
    expect(SHELL_CSP).toContain("script-src 'none'");
    expect(SHELL_CSP).toContain("connect-src 'none'");
    expect(SHELL_CSP.includes("tauri.localhost")).toBe(false);
    expect(SHELL_CSP.includes("localhost:1420")).toBe(false);
    expect(SHELL_CSP.includes("https:")).toBe(false);
  });
});
