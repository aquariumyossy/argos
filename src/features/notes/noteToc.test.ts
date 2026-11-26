import { describe, expect, it } from "vitest";
import {
  renderNoteMemoHtml,
  resetNoteMemoCheckboxSeq,
  splitNoteMemoBlocks,
} from "./noteMemoMd";
import { noteHeadings, scrollTextareaToOffset } from "./noteToc";

function previewHeadings(md: string): { level: number; text: string }[] {
  resetNoteMemoCheckboxSeq();
  const html = splitNoteMemoBlocks(md)
    .map((block) => (block.type === "md" ? renderNoteMemoHtml(block.text) : ""))
    .join("");
  const doc = new DOMParser().parseFromString(`<div>${html}</div>`, "text/html");
  return [...doc.querySelectorAll("h1, h2, h3, h4, h5, h6")].map((el) => ({
    level: Number(el.tagName.slice(1)),
    text: el.textContent ?? "",
  }));
}

function expectParity(md: string) {
  const toc = noteHeadings(md);
  expect(toc.map(({ level, text }) => ({ level, text }))).toEqual(previewHeadings(md));
  let prev = -1;
  for (const heading of toc) {
    expect(heading.offset).toBeGreaterThan(prev);
    prev = heading.offset;
    const slice = md.slice(heading.offset, heading.offset + 80);
    if (heading.text === "") {
      expect(slice.startsWith("#")).toBe(true);
    } else {
      expect(slice.includes(heading.text)).toBe(true);
    }
  }
}

describe("noteHeadings", () => {
  it("matches the preview for fullwidth space after the marks", () => {
    expectParity("#　全角\n本文");
  });

  it("matches the preview for a setext heading", () => {
    expectParity("Title\n---\n本文");
  });

  it("matches the preview for a heading inside a blockquote", () => {
    expectParity("> # quote\n> 本文");
  });

  it("matches the preview for a heading inside a list", () => {
    expectParity("- # item\n  本文");
  });

  it("matches the preview for a heading indented by fewer than four spaces", () => {
    expectParity("   # indented\n本文");
  });

  it("matches the preview and skips a fenced heading", () => {
    const md = "```\n# fence\n```\n# after";
    expectParity(md);
    expect(noteHeadings(md).map((h) => h.text)).toEqual(["after"]);
  });

  it("keeps duplicate headings as separate entries", () => {
    const md = "# real\n# real";
    expectParity(md);
    const headings = noteHeadings(md);
    expect(headings).toHaveLength(2);
    expect(headings[0]?.offset).not.toBe(headings[1]?.offset);
  });

  it("includes an empty hash heading so the preview index still lines up", () => {
    expectParity("# \n# 次");
  });
});

describe("scrollTextareaToOffset", () => {
  it("places the caret at the heading and scrolls to the measured top", () => {
    const ta = document.createElement("textarea");
    document.body.appendChild(ta);
    ta.value = `${"あ".repeat(40)}\n# 見出し\n`;
    const offset = ta.value.indexOf("# 見出し");
    const previous = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "offsetTop");
    Object.defineProperty(HTMLElement.prototype, "offsetTop", {
      configurable: true,
      get() {
        return 320;
      },
    });
    try {
      scrollTextareaToOffset(ta, offset);
      expect(ta.selectionStart).toBe(offset);
      expect(ta.selectionEnd).toBe(offset);
      expect(ta.scrollTop).toBe(320);
    } finally {
      if (previous) Object.defineProperty(HTMLElement.prototype, "offsetTop", previous);
      else delete (HTMLElement.prototype as { offsetTop?: number }).offsetTop;
      ta.remove();
    }
  });
});
