import { Marked } from "marked";
import { wrapMarkdownTables } from "../preview/markdownPreview";

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

function isSafeHref(href: string): boolean {
  const t = href.trim().toLowerCase();
  return t.startsWith("https://") || t.startsWith("http://");
}

let checkboxSeq = 0;

const noteMarked = new Marked({
  gfm: true,
  breaks: true,
  renderer: {
    html({ text }) {
      return escapeHtml(text);
    },
    image() {
      return "";
    },
    link(this, { href, title, tokens }) {
      const inner = this.parser.parseInline(tokens);
      if (!isSafeHref(href)) return inner;
      const t = title ? ` title="${escapeHtml(title)}"` : "";
      return `<a href="${escapeHtml(href)}"${t} rel="noreferrer noopener" target="_blank">${inner}</a>`;
    },
    checkbox({ checked }) {
      const i = checkboxSeq++;
      return `<input type="checkbox" data-note-check="${i}"${checked ? " checked" : ""} />`;
    },
  },
});

const MERMAID_LANGS = new Set([
  "mermaid",
  "flowchart",
  "graph",
  "sequence",
  "sequencediagram",
  "timeline",
]);

export type NoteMemoBlock =
  | { type: "md"; text: string }
  | { type: "mermaid"; text: string };

function fenceKind(info: string): "mermaid" | "code" {
  const lang = info.trim().split(/\s+/)[0]?.toLowerCase() ?? "";
  return MERMAID_LANGS.has(lang) ? "mermaid" : "code";
}

function isFenceClose(line: string, indent: string, marker: string): boolean {
  const m = line.match(/^([ \t]*)([`~]+)[ \t]*$/);
  if (!m) return false;
  if (m[1] !== indent) return false;
  if (m[2][0] !== marker[0]) return false;
  return m[2].length >= marker.length;
}

/** Split mermaid fences so the rest can go through marked. */
export function splitNoteMemoBlocks(src: string): NoteMemoBlock[] {
  const lines = src.split("\n");
  const blocks: NoteMemoBlock[] = [];
  let buf: string[] = [];
  const flushMd = () => {
    if (buf.length === 0) return;
    blocks.push({ type: "md", text: buf.join("\n") });
    buf = [];
  };

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i] ?? "";
    const open = line.match(/^([ \t]*)([`~]{3,})([^\n]*)$/);
    if (!open) {
      buf.push(line);
      continue;
    }
    const indent = open[1] ?? "";
    const marker = open[2] ?? "```";
    const kind = fenceKind(open[3] ?? "");
    let j = i + 1;
    const body: string[] = [];
    let closed = false;
    while (j < lines.length) {
      const cur = lines[j] ?? "";
      if (isFenceClose(cur, indent, marker[0] ?? "`")) {
        closed = true;
        break;
      }
      body.push(cur);
      j += 1;
    }
    if (kind === "mermaid") {
      flushMd();
      blocks.push({ type: "mermaid", text: body.join("\n") });
    } else {
      buf.push(line);
      buf.push(...body);
      if (closed) buf.push(lines[j] ?? "");
    }
    i = closed ? j : lines.length;
  }
  flushMd();
  return blocks;
}

export function resetNoteMemoCheckboxSeq() {
  checkboxSeq = 0;
}

export function renderNoteMemoHtml(text: string): string {
  const html = noteMarked.parse(text, { async: false }) as string;
  const doc = new DOMParser().parseFromString(
    `<div id="root">${html}</div>`,
    "text/html",
  );
  const root = doc.getElementById("root");
  if (!root) return html;
  wrapMarkdownTables(root, doc);
  return root.innerHTML;
}

const TASK_ITEM_RE = /^(\s*[-*+]\s+)\[([ xX])\]/;

/** Toggle the n-th GFM task checkbox in markdown source. */
export function toggleGfmCheckbox(md: string, index: number): string | null {
  if (index < 0) return null;
  const lines = md.split("\n");
  let n = 0;
  let inFence = false;
  let fenceCh = "";
  let fenceN = 0;
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i] ?? "";
    const open = parseFence(line);
    if (inFence) {
      if (open && open.ch === fenceCh && open.n >= fenceN && !line.slice(open.n).trim()) {
        inFence = false;
      }
      continue;
    }
    if (open) {
      inFence = true;
      fenceCh = open.ch;
      fenceN = open.n;
      continue;
    }
    if (!TASK_ITEM_RE.test(line)) continue;
    if (n === index) {
      lines[i] = line.replace(TASK_ITEM_RE, (_, prefix: string, mark: string) => {
        const next = mark.trim() ? " " : "x";
        return `${prefix}[${next}]`;
      });
      return lines.join("\n");
    }
    n += 1;
  }
  return null;
}

function parseFence(line: string): { ch: string; n: number } | null {
  const t = line.replace(/^[ \t]+/, "");
  const ch = t[0];
  if (ch !== "`" && ch !== "~") return null;
  let n = 0;
  while (t[n] === ch) n += 1;
  if (n < 3) return null;
  return { ch, n };
}

export const NOTE_MEMO_HIGHLIGHT = "argos-notes-memo";
