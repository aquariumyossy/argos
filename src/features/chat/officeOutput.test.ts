import { describe, expect, it } from "vitest";
import { officeOutputLink } from "./officeOutput";

describe("officeOutputLink", () => {
  it("reads the saved file and leftover locations", () => {
    const content = [
      "出力ファイル: 契約書_編集済み（2026-11-12 09-30-00）.docx",
      String.raw`保存先: C:\Users\me\AppData\Roaming\Argos\出力\契約書_編集済み（2026-11-12 09-30-00）.docx`,
      "残存: あり",
      "「山田太郎」が残っています。",
      "- 文書のプロパティ（山田太郎事件）",
      "文字列置換: 「山田太郎」→「佐藤花子」を 6 箇所",
      "段落の書き換え: 1 件",
    ].join("\n");
    const link = officeOutputLink(content);
    expect(link?.name).toBe("契約書_編集済み（2026-11-12 09-30-00）.docx");
    expect(link?.path).toBe(
      String.raw`C:\Users\me\AppData\Roaming\Argos\出力\契約書_編集済み（2026-11-12 09-30-00）.docx`,
    );
    expect(link?.leftovers).toEqual([
      "「山田太郎」が残っています。",
      "- 文書のプロパティ（山田太郎事件）",
    ]);
  });

  it("returns no leftovers when the check is clean", () => {
    const link = officeOutputLink(
      "出力ファイル: 一覧_編集済み（2026-11-12 10-00-00）.xlsx\n保存先: /tmp/一覧.xlsx\n残存: なし\n",
    );
    expect(link?.leftovers).toEqual([]);
  });

  it("ignores tool results that are not an edited file", () => {
    expect(officeOutputLink("出典 [1] 契約書")).toBeNull();
  });
});
