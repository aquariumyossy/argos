/** Kept in step with `OFFICE_MARKER` in `src-tauri/src/llm/create_office.rs`. */
export const OFFICE_MARKER = "[[argos-office]]";

export type OfficeFileLink = {
  kind: "docx" | "xlsx";
  name: string;
  path: string;
  label: string;
};

/** A notice Argos saved after create_docx / create_xlsx. Other messages are ignored. */
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
    if (parts.length !== 4) return null;
    const kind = parts[1];
    const name = parts[2]?.trim() ?? "";
    const path = parts[3]?.trim() ?? "";
    if ((kind !== "docx" && kind !== "xlsx") || !name || !path) return null;
    if (kind === "docx" && !name.toLowerCase().endsWith(".docx")) return null;
    if (kind === "xlsx" && !name.toLowerCase().endsWith(".xlsx")) return null;
    if (path.includes("\0")) return null;
    const segments = path.replace(/\\/g, "/").split("/");
    if (segments.some((seg) => seg === ".." || seg === ".")) return null;
    if (!segments[segments.length - 1]?.endsWith(name) && !path.endsWith(name)) return null;
    found = { kind, name, path, label: "" };
  }
  if (!found) return null;
  found.label =
    labelLines.join("\n") ||
    (found.kind === "docx"
      ? "Wordファイルを保存しました"
      : "Excelファイルを保存しました");
  return found;
}
