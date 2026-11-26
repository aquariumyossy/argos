import { describe, expect, it } from "vitest";
import { noteHeadings } from "./noteToc";

describe("noteHeadings", () => {
  it("lists ATX headings and keeps their depth", () => {
    const md = "# 請求原因\n本文\n## 売買\n### 代金\n#### 内訳\n";
    expect(noteHeadings(md)).toEqual([
      { level: 1, text: "請求原因", line: 0 },
      { level: 2, text: "売買", line: 2 },
      { level: 3, text: "代金", line: 3 },
      { level: 4, text: "内訳", line: 4 },
    ]);
  });

  it("ignores hash marks inside fenced code", () => {
    const md = [
      "# 本編",
      "```",
      "# 偽の見出し",
      "```",
      "~~~md",
      "## これもコード",
      "~~~",
      "## 本当の節",
    ].join("\n");
    expect(noteHeadings(md)).toEqual([
      { level: 1, text: "本編", line: 0 },
      { level: 2, text: "本当の節", line: 7 },
    ]);
  });

  it("returns nothing when the note has no headings", () => {
    expect(noteHeadings("ただの本文\n#見出しではない\n")).toEqual([]);
    expect(noteHeadings("")).toEqual([]);
  });
});
