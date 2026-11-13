/** Kept in step with `OFFICE_MARKER` in `src-tauri/src/llm/create_office.rs`. */
export const OFFICE_MARKER = "[[argos-office]]";

export type OfficeKind = "docx" | "xlsx" | "html";

export type OfficeFileLink = {
  kind: OfficeKind;
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
    const name = parts[2]?.trim() ?? "";
    const kind = officeKind(parts[1] ?? "", name);
    if (!kind) return null;
    found = { kind, name, label: "" };
  }
  if (!found) return null;
  found.label = labelLines.join("\n") || savedLabel(found.kind);
  return found;
}

export function bareOfficeKind(name: string): OfficeKind | null {
  if (!isBareOutputName(name)) return null;
  const lower = name.toLowerCase();
  if (lower.endsWith(".xlsx")) return "xlsx";
  if (lower.endsWith(".html")) return "html";
  if (lower.endsWith(".docx")) return "docx";
  return null;
}

function officeKind(kind: string, name: string): OfficeKind | null {
  const actual = bareOfficeKind(name);
  if (!actual || actual !== kind) return null;
  return actual;
}

function savedLabel(kind: OfficeKind): string {
  if (kind === "docx") return "Wordファイルを保存しました";
  if (kind === "html") return "HTMLファイルを保存しました";
  return "Excelファイルを保存しました";
}

function isBareOutputName(name: string): boolean {
  if (!name || name.startsWith(".")) return false;
  if (/[/\\:\0]/.test(name) || name.includes("..")) return false;
  if ([...name].some((ch) => ch.charCodeAt(0) < 32)) return false;
  const lower = name.toLowerCase();
  return (
    lower.endsWith(".docx") || lower.endsWith(".xlsx") || lower.endsWith(".html")
  );
}
