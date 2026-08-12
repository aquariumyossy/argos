import { save } from "@tauri-apps/plugin-dialog";
import { invoke } from "@tauri-apps/api/core";
import { formatCourtCaseJsonPlainText } from "./courtCaseFormat";
import {
  convertArticleKanjiNumerals,
  isLegalMdFormatTarget,
  stripEmptyParagraphMarkers,
} from "./legalMdFormat";

export type NoteExportMeta = {
  title: string;
  memo: string;
};

export type NoteExportItem = {
  path: string;
  title: string;
  label: string;
  body: string;
  query: string;
  memo: string;
  page?: number | null;
};

/** Legal MD → readable plain text for export (headings without #, numbered 項). */
export function formatLegalMdPlainText(body: string): string {
  let text = stripEmptyParagraphMarkers(convertArticleKanjiNumerals(body));
  text = text.replace(/^#{1,6}\s+/gm, "");
  text = text.replace(/^\s*[-*+]\s+\*\*([^*]+)\*\*\s*:?\s*/gm, "$1. ");
  text = text.replace(/^\s*[-*+]\s+/gm, "");
  text = text.replace(/\*\*([^*]+)\*\*/g, "$1");
  text = text.replace(/\*([^*]+)\*/g, "$1");
  return text.replace(/\n{3,}/g, "\n\n").trim();
}

export function formatExportBody(path: string, body: string): string {
  const court = formatCourtCaseJsonPlainText(body, {
    formatPlainText: convertArticleKanjiNumerals,
  });
  if (court) return court;
  if (isLegalMdFormatTarget(path, body)) {
    return formatLegalMdPlainText(body);
  }
  return body.trim();
}

export function buildNoteExportMarkdown(
  note: NoteExportMeta,
  items: NoteExportItem[],
): string {
  const parts: string[] = [];
  const title = note.title.trim() || "無題のノート";
  parts.push(`# ${title}`);

  const noteMemo = note.memo.trim();
  if (noteMemo) {
    parts.push("");
    parts.push(noteMemo);
  }

  for (const item of items) {
    parts.push("");
    parts.push("---");
    parts.push("");
    const heading =
      item.title.trim() || item.label.trim() || item.path.trim() || "（無題）";
    const pageSuffix =
      item.page != null && Number.isFinite(item.page) ? ` (p.${item.page})` : "";
    parts.push(`## ${heading}${pageSuffix}`);
    parts.push("");

    const meta: string[] = [];
    if (item.path.trim()) meta.push(`- 出典: ${item.path.trim()}`);
    if (item.query.trim()) meta.push(`- 検索: ${item.query.trim()}`);
    if (item.memo.trim()) meta.push(`- メモ: ${item.memo.trim()}`);
    if (meta.length > 0) {
      parts.push(...meta);
      parts.push("");
    }

    const body = formatExportBody(item.path, item.body);
    parts.push(body || "（本文なし）");
  }

  parts.push("");
  return parts.join("\n");
}

export function noteExportFilename(title: string): string {
  const base = title
    .trim()
    .replace(/[\\/:*?"<>|]/g, "_")
    .replace(/\s+/g, " ")
    .slice(0, 80);
  return `${base || "argos-note"}.md`;
}

/** Save markdown via Windows save dialog + write_text_file. Returns false if cancelled. */
export async function saveNoteMarkdown(
  markdown: string,
  defaultFilename: string,
): Promise<boolean> {
  const path = await save({
    title: "ノートを Markdown で保存",
    defaultPath: defaultFilename,
    filters: [{ name: "Markdown", extensions: ["md"] }],
  });
  if (!path) return false;
  await invoke("write_text_file", { path, contents: markdown });
  return true;
}
