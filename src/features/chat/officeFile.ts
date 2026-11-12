/** Kept in step with `OFFICE_MARKER` in `src-tauri/src/llm/create_office.rs`. */
export const OFFICE_MARKER = "[[argos-office]]";

export type OfficeFileLink = {
  kind: "docx" | "xlsx";
  name: string;
  label: string;
};

/** A notice Argos saved after create_docx / create_xlsx. The button carries a file name, not a path. */
export function parseOfficeFileNotice(content: string): OfficeFileLink | null {
  const lines = content.split(/\r?\n/);
  let found: OfficeFileLink | null = null;
  const labelLines: string[] = [];
  for (const line of lines) {
    if (!line.startsWith(`${OFFICE_MARKER}\t`)) {
      if (line.trim()) labelLines.push(line.trim());
      continue;
    }
    if (found) return null;
    const parts = line.split("\t");
    if (parts.length < 3) return null;
    const kind = parts[1];
    const name = parts[2]?.trim() ?? "";
    if ((kind !== "docx" && kind !== "xlsx") || !isBareOfficeName(name, kind)) return null;
    found = { kind, name, label: "" };
  }
  if (!found) return null;
  found.label =
    labelLines.join("\n") ||
    (found.kind === "docx"
      ? "Wordファイルを保存しました"
      : "Excelファイルを保存しました");
  return found;
}

function isBareOfficeName(name: string, kind: "docx" | "xlsx"): boolean {
  if (!name || name.startsWith(".")) return false;
  if (/[/\\:\0]/.test(name) || name.includes("..")) return false;
  if ([...name].some((ch) => ch.charCodeAt(0) < 32)) return false;
  return kind === "docx"
    ? name.toLowerCase().endsWith(".docx")
    : name.toLowerCase().endsWith(".xlsx");
}
