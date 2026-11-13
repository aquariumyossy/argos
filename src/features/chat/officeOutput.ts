/** Kept in step with `OFFICE_MARKER` in `office_edit`. The third field is a bare file name. */
export const OFFICE_MARKER = "[[argos-office]]";
/** Kept in step with `SOURCE_LINE` in `office_edit`. A bare file name, never a path. */
export const SOURCE_LINE = "元ファイル: ";

export type EditOfficeFile = {
  kind: "docx" | "xlsx";
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
  if ((toolName ?? "").trim() !== "edit_office") return null;
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
      if (!isBareOfficeName(candidate)) return null;
      name = candidate;
      inLeftovers = false;
      continue;
    }
    if (line.startsWith(SOURCE_LINE)) {
      const candidate = line.slice(SOURCE_LINE.length).trim();
      if (!sourceName && isBareOfficeName(candidate)) sourceName = candidate;
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
  const kind = name.toLowerCase().endsWith(".xlsx") ? "xlsx" : "docx";
  return sourceName
    ? { kind, name, sourceName, leftovers }
    : { kind, name, leftovers };
}

function isBareOfficeName(name: string): boolean {
  if (!name || name.startsWith(".")) return false;
  if (/[/\\:\0]/.test(name) || name.includes("..")) return false;
  if ([...name].some((ch) => ch.charCodeAt(0) < 32)) return false;
  const lower = name.toLowerCase();
  return lower.endsWith(".docx") || lower.endsWith(".xlsx");
}
