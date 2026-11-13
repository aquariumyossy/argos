import { describe, expect, it } from "vitest";
import { officeCardsByMessage, type OfficeCardMessage } from "./officeCards";

const docxNotice = [
  "Wordファイルを保存しました（Argos の出力フォルダ）",
  "[[argos-office]]\tdocx\t契約メモ.docx",
].join("\n");

const xlsxNotice = [
  "Excelファイルを保存しました（Argos の出力フォルダ）",
  "[[argos-office]]\txlsx\t売上集計.xlsx",
].join("\n");

const editReport = [
  "出力ファイル: 契約書_編集済み（2026-11-12 09-30-00）.docx",
  "[[argos-office]]\tdocx\t契約書_編集済み（2026-11-12 09-30-00）.docx",
  "元ファイル: 契約書.docx",
  String.raw`保存先: C:\Users\me\AppData\Roaming\Argos\output\契約書_編集済み（2026-11-12 09-30-00）.docx`,
  "残存: あり",
  "「山田太郎」が残っています。",
].join("\n");

function msg(
  id: string,
  role: string,
  content: string,
  extra: Partial<OfficeCardMessage> = {},
): OfficeCardMessage {
  return { id, role, content, ...extra };
}

describe("officeCardsByMessage", () => {
  it("puts one create card per file on the assistant reply", () => {
    const cards = officeCardsByMessage([
      msg("u", "user", "作って"),
      msg("call", "assistant", ""),
      msg("n1", "assistant", docxNotice, { kind: "notice" }),
      msg("n2", "assistant", xlsxNotice, { kind: "notice" }),
      msg("cut", "assistant", "考えが長くなったため途中で打ち切りました", {
        kind: "notice",
      }),
      msg("reply", "assistant", "できました。"),
    ]);
    expect(cards.get("reply")).toEqual([
      { kind: "docx", name: "契約メモ.docx" },
      { kind: "xlsx", name: "売上集計.xlsx" },
    ]);
    expect(cards.has("n1")).toBe(false);
    expect(cards.has("n2")).toBe(false);
    expect(cards.has("call")).toBe(false);
    expect(cards.has("cut")).toBe(false);
  });

  it("puts an edit card, with the original name, on the same reply", () => {
    const cards = officeCardsByMessage([
      msg("u", "user", "直して"),
      msg("call", "assistant", "編集します。"),
      msg("tool", "tool", editReport, { toolName: "edit_office" }),
      msg("n1", "assistant", docxNotice, { kind: "notice" }),
      msg("reply", "assistant", "編集しました。"),
    ]);
    expect(cards.get("reply")).toEqual([
      {
        kind: "docx",
        name: "契約書_編集済み（2026-11-12 09-30-00）.docx",
        sourceName: "契約書.docx",
      },
      { kind: "docx", name: "契約メモ.docx" },
    ]);
    expect(cards.has("tool")).toBe(false);
    expect(cards.has("n1")).toBe(false);
  });

  it("keeps the card on the notice or tool row when the reply never arrived", () => {
    const cards = officeCardsByMessage([
      msg("u", "user", "作って"),
      msg("call", "assistant", "作ります。"),
      msg("n1", "assistant", docxNotice, { kind: "notice" }),
      msg("tool", "tool", editReport, { toolName: "edit_office" }),
      msg("cut", "assistant", "考えが長くなったため途中で打ち切りました", {
        kind: "notice",
      }),
    ]);
    expect(cards.get("n1")).toEqual([{ kind: "docx", name: "契約メモ.docx" }]);
    expect(cards.get("tool")).toEqual([
      {
        kind: "docx",
        name: "契約書_編集済み（2026-11-12 09-30-00）.docx",
        sourceName: "契約書.docx",
      },
    ]);
    expect(cards.has("call")).toBe(false);
    expect(cards.has("cut")).toBe(false);
  });

  it("does not move a file onto the next turn's reply", () => {
    const cards = officeCardsByMessage([
      msg("u1", "user", "作って"),
      msg("n1", "assistant", docxNotice, { kind: "notice" }),
      msg("u2", "user", "ありがとう"),
      msg("later", "assistant", "どういたしまして。"),
    ]);
    expect(cards.get("n1")).toEqual([{ kind: "docx", name: "契約メモ.docx" }]);
    expect(cards.has("later")).toBe(false);
  });

  it("still shows an older edit that has no original name", () => {
    const cards = officeCardsByMessage([
      msg("u", "user", "直して"),
      msg("tool", "tool", "[[argos-office]]\txlsx\t一覧_編集済み.xlsx", {
        toolName: "edit_office",
      }),
      msg("reply", "assistant", "できました。"),
    ]);
    expect(cards.get("reply")).toEqual([
      { kind: "xlsx", name: "一覧_編集済み.xlsx" },
    ]);
  });

  it("puts an HTML create and edit on the reply, with the original name", () => {
    const edited = "手数料_編集済み（2026-11-13 02-50-00）.html";
    const htmlNotice = [
      "HTMLファイルを保存しました（Argos の出力フォルダ）",
      "[[argos-office]]\thtml\t手数料.html",
    ].join("\n");
    const editReport = [
      `出力ファイル: ${edited}`,
      `[[argos-office]]\thtml\t${edited}`,
      "元ファイル: 手数料.html",
    ].join("\n");
    const cards = officeCardsByMessage([
      msg("u", "user", "計算して"),
      msg("n1", "assistant", htmlNotice, { kind: "notice" }),
      msg("tool", "tool", editReport, { toolName: "edit_html" }),
      msg("reply", "assistant", "できました。"),
    ]);
    expect(cards.get("reply")).toEqual([
      { kind: "html", name: "手数料.html" },
      { kind: "html", name: edited, sourceName: "手数料.html" },
    ]);
    expect(cards.has("n1")).toBe(false);
    expect(cards.has("tool")).toBe(false);
  });

  it("ignores a path posing as the original name and text that is not a saved file", () => {
    const cards = officeCardsByMessage([
      msg("u", "user", "直して"),
      msg("tool", "tool", "[[argos-office]]\tdocx\tout.docx\n元ファイル: C:\\secret\\契約書.docx", {
        toolName: "edit_office",
      }),
      msg("other", "tool", editReport, { toolName: "search_index" }),
      msg("reply", "assistant", "できました。"),
    ]);
    expect(cards.get("reply")).toEqual([{ kind: "docx", name: "out.docx" }]);
  });
});
