/** A file the edit_office tool saved. The path is opened with the OS. */
export type OfficeOutputLink = {
  name: string;
  path: string;
  leftovers: string[];
};

const FILE_LINE = "出力ファイル: ";
const PATH_LINE = "保存先: ";

/**
 * Read the tool result. The first lines are the file name and path so a
 * truncated result still has something to open.
 */
export function officeOutputLink(content: string): OfficeOutputLink | null {
  const lines = content.split(/\r?\n/);
  let name = "";
  let path = "";
  const leftovers: string[] = [];
  let inLeftovers = false;
  for (const raw of lines) {
    const line = raw.trim();
    if (line.startsWith(FILE_LINE)) {
      name = line.slice(FILE_LINE.length).trim();
      inLeftovers = false;
      continue;
    }
    if (line.startsWith(PATH_LINE)) {
      path = line.slice(PATH_LINE.length).trim();
      inLeftovers = false;
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
  if (!name || !path) return null;
  return { name, path, leftovers };
}
