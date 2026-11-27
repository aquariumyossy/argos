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

  it("matches a heading after several quoted lines", () => {
    expectParity("> 前文です。\n> 次の行です。\n>\n> # 見出し\n> 続きです。\n\n# 外");
  });

  it("matches a heading in a list item that has a continuation line", () => {
    expectParity("- 一行目です\n  継続行です\n\n  # 見出し\n  さらに続く\n\n# 外");
  });

  it("matches a heading in a list inside a blockquote", () => {
    expectParity("> - # item\n>   本文\n>\n> tail\n\n# 外");
  });

  it("keeps the same headings when the memo uses CRLF", () => {
    const lf = "> 前文です。\n> # 引用\n> 本文\n\n# 外\n- 一行目\n  継続\n\n  # 項目\n";
    const crlf = lf.replace(/\n/g, "\r\n");
    expectParity(crlf);
    const fromLf = noteHeadings(lf);
    const fromCrlf = noteHeadings(crlf);
    expect(fromCrlf).toEqual(fromLf);
  });

  it("matches a CRLF note's heading offset to the LF textarea value", () => {
    const crlf = "前文\r\n\r\n# 見出し\r\n> # 引用\r\n> 本文\r\n";
    const ta = document.createElement("textarea");
    ta.value = crlf;
    const headings = noteHeadings(crlf);
    expect(ta.value.includes("\r")).toBe(false);
    for (const heading of headings) {
      expect(ta.value.slice(heading.offset, heading.offset + 80)).toContain(
        heading.text === "" ? "#" : heading.text,
      );
      expect(heading.offset).toBeLessThan(ta.value.length);
    }
    const top = headings.find((h) => h.text === "見出し");
    const quoted = headings.find((h) => h.text === "引用");
    expect(top).toBeTruthy();
    expect(quoted).toBeTruthy();
    expect(top?.offset).toBe(ta.value.indexOf("# 見出し"));
    expect(quoted?.offset).toBe(ta.value.indexOf("# 引用"));
    expect(crlf.indexOf("# 見出し")).toBeGreaterThan(top?.offset ?? 0);
  });
});

describe("noteHeadings performance", () => {
  it("extracts headings from a 100k-character note of quotes and lists within a few milliseconds", () => {
    const quote = `> ${"甲は乙に対し金員の支払を求める。".repeat(6)}\n> ${"判決の理由は次のとおりである。".repeat(6)}`;
    const list = `- ${"争点は次のとおりである。".repeat(4)}\n  ${"継続して主張する。".repeat(4)}`;
    const nested = `> - ${"引用の中の項目である。".repeat(4)}\n>   ${"引用の継続行である。".repeat(4)}\n>   # 引用中の見出し`;
    const parts: string[] = [];
    let size = 0;
    let n = 0;
    while (size < 100_000) {
      const chunk = `# 見出し ${n}\n${quote}\n>\n> # 条 ${n}\n> なお書き。\n\n${list}\n\n${nested}\n\n`;
      parts.push(chunk);
      size += chunk.length;
      n += 1;
    }
    const lf = parts.join("");
    const crlf = lf.replace(/\n/g, "\r\n");
    expect(lf.length).toBeGreaterThanOrEqual(100_000);
    expect(crlf.length).toBeGreaterThan(lf.length);

    const measure = (src: string) => {
      let headings = noteHeadings(src);
      const samples: number[] = [];
      for (let i = 0; i < 3; i++) {
        const start = performance.now();
        headings = noteHeadings(src);
        samples.push(performance.now() - start);
      }
      samples.sort((a, b) => a - b);
      return { ms: samples[1] ?? samples[0] ?? 0, headings };
    };
    const lfRun = measure(lf);
    const crlfRun = measure(crlf);
    expect(lfRun.headings.length).toBeGreaterThan(50);
    expect(crlfRun.headings.map((h) => h.text)).toEqual(lfRun.headings.map((h) => h.text));
    let prev = -1;
    for (const heading of crlfRun.headings) {
      expect(heading.offset).toBeGreaterThan(prev);
      prev = heading.offset;
      const slice = lf.slice(heading.offset, heading.offset + 40);
      expect(slice.includes("#")).toBe(true);
      expect(slice.includes(heading.text)).toBe(true);
    }
    expect(crlfRun.headings.map((h) => h.offset)).toEqual(lfRun.headings.map((h) => h.offset));
    // Previously about 75ms (LF) and 220ms (CRLF). The marked lexer is most of what remains.
    expect(lfRun.ms, `LF ${lfRun.ms.toFixed(1)}ms`).toBeLessThan(100);
    expect(crlfRun.ms, `CRLF ${crlfRun.ms.toFixed(1)}ms`).toBeLessThan(100);
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
