import { describe, expect, it } from "vitest";
import { parseOfficeFileNotice } from "./officeFile";

describe("parseOfficeFileNotice", () => {
  it("reads only the file name from a saved-file notice", () => {
    const notice = [
      "Wordファイルを保存しました（Argos の出力フォルダ）",
      "[[argos-office]]\tdocx\t契約メモ.docx",
    ].join("\n");
    expect(parseOfficeFileNotice(notice)).toEqual({
      kind: "docx",
      name: "契約メモ.docx",
      label: "Wordファイルを保存しました（Argos の出力フォルダ）",
    });
  });

  it("keeps the file name when an old notice still carries a path", () => {
    const notice =
      "Excelファイルを保存しました（Argos の出力フォルダ）\n[[argos-office]]\txlsx\t売上集計.xlsx\tC:\\Users\\a\\AppData\\Roaming\\Argos\\output\\売上集計.xlsx";
    expect(parseOfficeFileNotice(notice)).toEqual({
      kind: "xlsx",
      name: "売上集計.xlsx",
      label: "Excelファイルを保存しました（Argos の出力フォルダ）",
    });
  });

  it("reads an HTML file name from a saved-file notice", () => {
    const notice = [
      "HTMLファイルを保存しました（Argos の出力フォルダ）",
      "[[argos-office]]\thtml\t手数料計算.html",
    ].join("\n");
    expect(parseOfficeFileNotice(notice)).toEqual({
      kind: "html",
      name: "手数料計算.html",
      label: "HTMLファイルを保存しました（Argos の出力フォルダ）",
    });
    expect(
      parseOfficeFileNotice("[[argos-office]]\thtml\t..\\手数料.html"),
    ).toBeNull();
    expect(
      parseOfficeFileNotice("[[argos-office]]\thtml\tC:\\Windows\\a.html"),
    ).toBeNull();
    expect(
      parseOfficeFileNotice("[[argos-office]]\tdocx\t手数料.html"),
    ).toBeNull();
  });

  it("ignores ordinary text and names that are not a single file", () => {
    expect(parseOfficeFileNotice("契約メモ.docx を作りました")).toBeNull();
    expect(
      parseOfficeFileNotice(
        "[[argos-office]]\tdocx\t..\\契約メモ.docx\tC:\\output\\契約メモ.docx",
      ),
    ).toBeNull();
    expect(
      parseOfficeFileNotice("[[argos-office]]\tdocx\tC:\\Windows\\cmd.docx"),
    ).toBeNull();
    expect(
      parseOfficeFileNotice("[[argos-office]]\txlsx\t\\\\server\\share\\a.xlsx"),
    ).toBeNull();
  });
});
