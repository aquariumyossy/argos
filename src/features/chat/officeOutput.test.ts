import { describe, expect, it } from "vitest";
import { editOfficeFileLink } from "./officeOutput";

const saved = [
  "出力ファイル: 契約書_編集済み（2026-11-12 09-30-00）.docx",
  "[[argos-office]]\tdocx\t契約書_編集済み（2026-11-12 09-30-00）.docx",
  String.raw`保存先: C:\Users\me\AppData\Roaming\Argos\output\契約書_編集済み（2026-11-12 09-30-00）.docx`,
  "残存: あり",
  "「山田太郎」が残っています。",
  "- フィールド（REF 山田太郎）",
  "文字列置換: 「山田太郎」→「佐藤花子」を 2 箇所",
].join("\n");

describe("editOfficeFileLink", () => {
  it("opens an edit_office result by file name and keeps leftover lines", () => {
    expect(editOfficeFileLink("edit_office", saved)).toEqual({
      kind: "docx",
      name: "契約書_編集済み（2026-11-12 09-30-00）.docx",
      leftovers: ["「山田太郎」が残っています。", "- フィールド（REF 山田太郎）"],
    });
  });

  it("keeps a bare original file name and ignores a path", () => {
    const withSource = [
      "元ファイル: 契約書.docx",
      saved,
    ].join("\n");
    expect(editOfficeFileLink("edit_office", withSource)).toMatchObject({
      kind: "docx",
      name: "契約書_編集済み（2026-11-12 09-30-00）.docx",
      sourceName: "契約書.docx",
      leftovers: ["「山田太郎」が残っています。", "- フィールド（REF 山田太郎）"],
    });
    expect(
      editOfficeFileLink(
        "edit_office",
        [
          saved,
          String.raw`元ファイル: C:\Users\me\契約書.docx`,
        ].join("\n"),
      )?.sourceName,
    ).toBeUndefined();
    const xlsx = editOfficeFileLink(
      "edit_office",
      "[[argos-office]]\txlsx\t一覧_編集済み.xlsx\n元ファイル: 一覧.xlsx",
    );
    expect(xlsx).toMatchObject({
      kind: "xlsx",
      name: "一覧_編集済み.xlsx",
      sourceName: "一覧.xlsx",
    });
  });

  it("does not open another tool even when the text contains a path", () => {
    expect(editOfficeFileLink("search_index", saved)).toBeNull();
    expect(editOfficeFileLink("read_unit", saved)).toBeNull();
    expect(editOfficeFileLink(undefined, saved)).toBeNull();
  });

  it("does not treat a saved-path line as something to open", () => {
    const pathOnly = String.raw`保存先: C:\Users\me\AppData\Roaming\Argos\output\契約書.docx`;
    expect(editOfficeFileLink("edit_office", pathOnly)).toBeNull();
    expect(
      editOfficeFileLink(
        "edit_office",
        "[[argos-office]]\tdocx\tC:\\Windows\\cmd.docx",
      ),
    ).toBeNull();
    expect(
      editOfficeFileLink(
        "edit_office",
        "[[argos-office]]\tdocx\t..\\契約書.docx",
      ),
    ).toBeNull();
    expect(
      editOfficeFileLink(
        "edit_office",
        "[[argos-office]]\txlsx\t\\\\server\\share\\a.xlsx",
      ),
    ).toBeNull();
  });
});
