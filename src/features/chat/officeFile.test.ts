import { describe, expect, it } from "vitest";
import { parseOfficeFileNotice } from "./officeFile";

describe("parseOfficeFileNotice", () => {
  it("reads the filename and path from a saved-file notice", () => {
    const path = "C:\\Users\\a\\AppData\\Roaming\\Argos\\output\\契約メモ.docx";
    const notice = [
      "Wordファイルを保存しました（Argos の出力フォルダ）",
      `[[argos-office]]\tdocx\t契約メモ.docx\t${path}`,
    ].join("\n");
    expect(parseOfficeFileNotice(notice)).toEqual({
      kind: "docx",
      name: "契約メモ.docx",
      path,
      label: "Wordファイルを保存しました（Argos の出力フォルダ）",
    });
  });

  it("reads an Excel notice with forward slashes", () => {
    const path = "/tmp/Argos/output/売上集計.xlsx";
    const notice = `Excelファイルを保存しました（Argos の出力フォルダ）\n[[argos-office]]\txlsx\t売上集計.xlsx\t${path}`;
    const parsed = parseOfficeFileNotice(notice);
    expect(parsed?.kind).toBe("xlsx");
    expect(parsed?.name).toBe("売上集計.xlsx");
    expect(parsed?.path).toBe(path);
  });

  it("ignores ordinary assistant text and a path that climbs out", () => {
    expect(parseOfficeFileNotice("契約メモ.docx を作りました")).toBeNull();
    expect(
      parseOfficeFileNotice(
        "[[argos-office]]\tdocx\t契約メモ.docx\tC:\\output\\..\\Windows\\契約メモ.docx",
      ),
    ).toBeNull();
  });
});
