export type MemoMdKind = "h1" | "h2" | "h3" | "list" | "todo" | "date";

export type MemoMdInsertResult = {
  next: string;
  caret: number;
};

const HEADING_PREFIX = /^#{1,6}\s*/;
const TASK_PREFIX = /^[-*+]\s+\[[ xX]\]\s/;
const LIST_PREFIX = /^[-*+]\s+/;

function clampSel(
  value: string,
  start: number,
  end: number,
): { start: number; end: number } {
  const a = Math.max(0, Math.min(start, value.length));
  const b = Math.max(0, Math.min(end, value.length));
  return a <= b ? { start: a, end: b } : { start: b, end: a };
}

function lineRange(
  value: string,
  start: number,
  end: number,
): { from: number; to: number } {
  let from = start;
  while (from > 0 && value.charCodeAt(from - 1) !== 10) from -= 1;
  let to = end;
  while (to < value.length && value.charCodeAt(to) !== 10) to += 1;
  return { from, to };
}

function splitIndent(line: string): { indent: string; rest: string } {
  const m = line.match(/^([ \t]*)(.*)$/);
  return { indent: m?.[1] ?? "", rest: m?.[2] ?? "" };
}

function applyHeadingLine(line: string, hashes: string): string {
  const { indent, rest } = splitIndent(line);
  return `${indent}${hashes} ${rest.replace(HEADING_PREFIX, "")}`;
}

function applyListLine(line: string): string {
  const { indent, rest } = splitIndent(line);
  if (TASK_PREFIX.test(rest) || LIST_PREFIX.test(rest)) return line;
  return `${indent}- ${rest}`;
}

function applyTodoLine(line: string): string {
  const { indent, rest } = splitIndent(line);
  if (TASK_PREFIX.test(rest)) return line;
  if (LIST_PREFIX.test(rest)) {
    return `${indent}- [ ] ${rest.replace(LIST_PREFIX, "")}`;
  }
  return `${indent}- [ ] ${rest}`;
}

function prefixLines(
  value: string,
  start: number,
  end: number,
  mapLine: (line: string) => string,
): MemoMdInsertResult {
  const sel = clampSel(value, start, end);
  const { from, to } = lineRange(value, sel.start, sel.end);
  const origLines = value.slice(from, to).split("\n");
  const caretOrig = sel.end - from;
  let origRel = 0;
  let newRel = 0;
  let caretOut: number | null = null;
  const nextLines = origLines.map((line, i) => {
    const next = mapLine(line);
    const origStart = origRel;
    const origEnd = origRel + line.length;
    if (caretOut == null && caretOrig >= origStart && caretOrig <= origEnd) {
      caretOut = newRel + (caretOrig - origStart) + (next.length - line.length);
    }
    const nl = i < origLines.length - 1 ? 1 : 0;
    origRel = origEnd + nl;
    newRel += next.length + nl;
    return next;
  });
  const nextBlock = nextLines.join("\n");
  return {
    next: value.slice(0, from) + nextBlock + value.slice(to),
    caret: from + (caretOut ?? nextBlock.length),
  };
}

export function formatTodayAt(now: Date = new Date()): string {
  const y = now.getFullYear();
  const m = String(now.getMonth() + 1).padStart(2, "0");
  const d = String(now.getDate()).padStart(2, "0");
  return `@${y}-${m}-${d}`;
}

function insertAt(
  value: string,
  start: number,
  end: number,
  text: string,
): MemoMdInsertResult {
  const sel = clampSel(value, start, end);
  let insert = text;
  const before = sel.start > 0 ? value[sel.start - 1] : "";
  const after = sel.end < value.length ? value[sel.end] : "";
  if (before && !/[\s]/.test(before)) insert = ` ${insert}`;
  if (after && !/[\s]/.test(after)) insert = `${insert} `;
  return {
    next: value.slice(0, sel.start) + insert + value.slice(sel.end),
    caret: sel.start + insert.length,
  };
}

export function applyMemoMdInsert(
  value: string,
  start: number,
  end: number,
  kind: MemoMdKind,
  now: Date = new Date(),
): MemoMdInsertResult {
  switch (kind) {
    case "h1":
      return prefixLines(value, start, end, (line) => applyHeadingLine(line, "#"));
    case "h2":
      return prefixLines(value, start, end, (line) => applyHeadingLine(line, "##"));
    case "h3":
      return prefixLines(value, start, end, (line) => applyHeadingLine(line, "###"));
    case "list":
      return prefixLines(value, start, end, applyListLine);
    case "todo":
      return prefixLines(value, start, end, applyTodoLine);
    case "date":
      return insertAt(value, start, end, formatTodayAt(now));
  }
}
