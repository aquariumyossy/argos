/** One ATX heading in a note memo. `line` is 0-based in the source. */
export type NoteHeading = {
  level: number;
  text: string;
  line: number;
};

function parseFenceOpen(line: string): { ch: string; n: number } | null {
  const t = line.trimStart();
  const ch = t[0];
  if (ch !== "`" && ch !== "~") return null;
  let n = 0;
  while (n < t.length && t[n] === ch) n += 1;
  if (n < 3) return null;
  return { ch, n };
}

function isFenceClose(line: string, ch: string, n: number): boolean {
  const t = line.trimStart();
  if (t.length < n) return false;
  for (let i = 0; i < t.length; i += 1) {
    if (t[i] !== ch) return false;
  }
  return true;
}

/** ATX heading. A missing space after the marks, or an empty title, is not a heading. */
function parseAtx(line: string): { level: number; text: string } | null {
  if (line.startsWith("    ") || line.startsWith("\t")) return null;
  let i = 0;
  while (i < line.length && line[i] === "#") i += 1;
  if (i === 0 || i > 6) return null;
  if (i < line.length && line[i] !== " " && line[i] !== "\t") return null;
  const text = line
    .slice(i)
    .trim()
    .replace(/#+$/, "")
    .trim();
  if (!text) return null;
  return { level: i, text };
}

/** Headings in source order. Marks inside fenced code are not headings. */
export function noteHeadings(md: string): NoteHeading[] {
  const lines = md.split(/\r?\n/);
  const out: NoteHeading[] = [];
  let fence: { ch: string; n: number } | null = null;
  for (let lineNo = 0; lineNo < lines.length; lineNo += 1) {
    const line = lines[lineNo].replace(/[ \t]+$/, "");
    if (fence) {
      if (isFenceClose(line, fence.ch, fence.n)) fence = null;
      continue;
    }
    const open = parseFenceOpen(line);
    if (open) {
      fence = open;
      continue;
    }
    const heading = parseAtx(line);
    if (heading) out.push({ ...heading, line: lineNo });
  }
  return out;
}
