import { bareOfficeKind, type OfficeKind } from "./officeFile";

/** Kept in step with `OFFICE_MARKER` in `office_edit`. The third field is a bare file name. */
export const OFFICE_MARKER = "[[argos-office]]";
/** Kept in step with `SOURCE_LINE` in `office_edit`. A bare file name, never a path. */
export const SOURCE_LINE = "元ファイル: ";

export type EditOfficeFile = {
  kind: OfficeKind;
  name: string;
  sourceName?: string;
  leftovers: string[];
};

/**
 * Open card for an `edit_office` tool result only.
 * The name is the marker's file name. A path in the tool text is never returned.
 */
export function editOfficeFileLink(
  toolName: string | undefined,
  content: string,
): EditOfficeFile | null {
  const tool = (toolName ?? "").trim();
  if (tool !== "edit_office" && tool !== "edit_html") return null;
  const lines = content.split(/\r?\n/);
  let name = "";
  let sourceName = "";
  const leftovers: string[] = [];
  let inLeftovers = false;
  for (const raw of lines) {
    const line = raw.trim();
    if (line.startsWith(`${OFFICE_MARKER}\t`)) {
      if (name) return null;
      const parts = line.split("\t");
      if (parts.length < 3) return null;
      const candidate = parts[2]?.trim() ?? "";
      if (!bareOfficeKind(candidate)) return null;
      name = candidate;
      inLeftovers = false;
      continue;
    }
    if (line.startsWith(SOURCE_LINE)) {
      const candidate = line.slice(SOURCE_LINE.length).trim();
      if (!sourceName && bareOfficeKind(candidate)) sourceName = candidate;
      continue;
    }
    if (line === "残存: あり") {
      inLeftovers = true;
      continue;
    }
    if (line === "残存: なし") {
      inLeftovers = false;
      continue;
    }
    if (!inLeftovers) continue;
    if (
      line.startsWith("文字列置換:") ||
      line.startsWith("段落の書き換え:") ||
      line.startsWith("シート名")
    ) {
      inLeftovers = false;
      continue;
    }
    if (line) leftovers.push(line);
  }
  if (!name) return null;
  const kind = bareOfficeKind(name);
  if (!kind) return null;
  return sourceName
    ? { kind, name, sourceName, leftovers }
    : { kind, name, leftovers };
}
