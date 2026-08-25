export type NoteTask = {
  noteId: string;
  noteTitle: string;
  text: string;
  due: string | null;
  done: boolean;
  line: number;
};

const TASK_RE = /^(\s*[-*+]\s+)\[([ xX])\]\s*(.*)$/;
const DUE_RE = /@(\d{4}-\d{2}-\d{2})/;

export function parseNoteTasks(
  noteId: string,
  noteTitle: string,
  memo: string,
): NoteTask[] {
  const out: NoteTask[] = [];
  const lines = memo.split("\n");
  let inFence = false;
  let fenceCh = "";
  let fenceN = 0;
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i] ?? "";
    const t = line.replace(/^[ \t]+/, "");
    const ch = t[0];
    if (inFence) {
      if (
        (ch === "`" || ch === "~") &&
        ch === fenceCh &&
        countFence(t, ch) >= fenceN &&
        !t.slice(countFence(t, ch)).trim()
      ) {
        inFence = false;
      }
      continue;
    }
    if (ch === "`" || ch === "~") {
      const n = countFence(t, ch);
      if (n >= 3) {
        inFence = true;
        fenceCh = ch;
        fenceN = n;
        continue;
      }
    }
    const m = line.match(TASK_RE);
    if (!m) continue;
    const body = (m[3] ?? "").trim();
    const due = body.match(DUE_RE)?.[1] ?? null;
    out.push({
      noteId,
      noteTitle,
      text: body || "（無題）",
      due,
      done: !!(m[2] && m[2].trim()),
      line: i,
    });
  }
  return out;
}

function countFence(t: string, ch: string): number {
  let n = 0;
  while (t[n] === ch) n += 1;
  return n;
}

export function toggleTaskLine(memo: string, line: number): string | null {
  const lines = memo.split("\n");
  const cur = lines[line];
  if (cur == null) return null;
  const next = cur.replace(TASK_RE, (_full, prefix: string, mark: string, rest: string) => {
    const checked = mark.trim();
    return `${prefix}[${checked ? " " : "x"}] ${rest}`;
  });
  if (next === cur) return null;
  lines[line] = next;
  return lines.join("\n");
}

export function memoHasOpenWork(memo: string): boolean {
  return parseNoteTasks("", "", memo).some((t) => !t.done || t.due);
}
