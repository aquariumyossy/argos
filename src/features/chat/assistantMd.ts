import { Marked } from "marked";
import { convertArticleKanjiNumerals } from "../notes/legalMdFormat";
import { wrapMarkdownTables } from "../preview/markdownPreview";

const CALLOUT_LABELS = new Set(["結論", "推測", "注意", "根拠", "補足", "備考"]);
const CALLOUT_HEADER_RE = /^(結論|推測|注意|根拠|補足|備考)/;

export type AssistantBlock =
  | { type: "md"; text: string }
  | { type: "mermaid"; text: string; closed: boolean }
  | { type: "choices"; text: string; closed: boolean }
  | { type: "code"; lang: string; text: string; closed: boolean };

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

const chatMarked = new Marked({
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
    checkbox() {
      return "";
    },
  },
});

const MERMAID_FENCE_LANGS = new Set([
  "mermaid",
  "flowchart",
  "graph",
  "sequence",
  "sequencediagram",
  "timeline",
]);

function fenceKind(info: string): "mermaid" | "choices" | "code" {
  const lang = info.trim().split(/\s+/)[0]?.toLowerCase() ?? "";
  if (MERMAID_FENCE_LANGS.has(lang)) return "mermaid";
  if (lang === "choices" || lang === "choice") return "choices";
  return "code";
}

function looksLikeMermaidSource(text: string): boolean {
  const first = text.trim().split("\n")[0]?.trim() ?? "";
  return /^(flowchart|graph|sequenceDiagram|classDiagram|stateDiagram(?:-v2)?|erDiagram|journey|gantt|pie|gitGraph|mindmap|timeline|quadrantChart)\b/.test(
    first,
  );
}

function isFenceClose(line: string, indent: string, marker: string): boolean {
  const m = line.match(/^([ \t]*)([`~]+)[ \t]*$/);
  if (!m) return false;
  if (m[1] !== indent) return false;
  if (m[2][0] !== marker[0]) return false;
  return m[2].length >= marker.length;
}

function isChoicesOpenTag(line: string): boolean {
  return /^\s*<choices>\s*$/i.test(line);
}

function isChoicesCloseTag(line: string): boolean {
  return /^\s*<\/choices>\s*$/i.test(line);
}

function stripStrayChoiceTags(text: string): string {
  return text
    .split("\n")
    .filter((line) => !/^\s*<\/?choices>\s*$/i.test(line))
    .join("\n");
}

/** Split markdown into md / mermaid / choices / code fences. Unclosed fences stay open. */
export function splitAssistantBlocks(src: string): AssistantBlock[] {
  const lines = src.replace(/\r\n/g, "\n").split("\n");
  const blocks: AssistantBlock[] = [];
  let mdBuf: string[] = [];
  let i = 0;

  const flushMd = () => {
    const text = stripStrayChoiceTags(mdBuf.join("\n"));
    mdBuf = [];
    if (text.length > 0) blocks.push({ type: "md", text });
  };

  const openRe = /^([ \t]*)(`{3,}|~{3,})(.*)$/;

  const readUntil = (closer: (line: string) => boolean): { text: string; closed: boolean } => {
    const body: string[] = [];
    let closed = false;
    while (i < lines.length) {
      if (closer(lines[i])) {
        closed = true;
        i += 1;
        break;
      }
      body.push(lines[i]);
      i += 1;
    }
    return { text: body.join("\n"), closed };
  };

  while (i < lines.length) {
    if (isChoicesOpenTag(lines[i])) {
      flushMd();
      i += 1;
      const { text, closed } = readUntil(
        (line) => isChoicesCloseTag(line) || isFenceClose(line, "", "```"),
      );
      blocks.push({ type: "choices", text, closed });
      continue;
    }

    const open = lines[i].match(openRe);
    if (open) {
      flushMd();
      const indent = open[1] ?? "";
      const marker = open[2] ?? "```";
      const info = open[3] ?? "";
      i += 1;
      const kind = fenceKind(info);
      const { text, closed } = readUntil((line) => {
        if (isFenceClose(line, indent, marker)) return true;
        return kind === "choices" && isChoicesCloseTag(line);
      });
      if (kind === "mermaid" || (kind === "code" && looksLikeMermaidSource(text))) {
        blocks.push({ type: "mermaid", text, closed });
      } else if (kind === "choices") {
        blocks.push({ type: "choices", text, closed });
      } else {
        const lang = info.trim().split(/\s+/)[0] ?? "";
        blocks.push({ type: "code", lang, text, closed });
      }
      continue;
    }
    mdBuf.push(lines[i]);
    i += 1;
  }
  flushMd();
  return blocks;
}

export function parseChoiceLines(text: string): string[] {
  const out: string[] = [];
  for (const raw of text.split(/\r?\n/)) {
    let s = raw.trim();
    if (!s) continue;
    s = s.replace(/^[-*+]\s+/, "");
    s = s.replace(/^\d+[.)]\s+/, "");
    s = s.replace(/^\[[ xX]\]\s+/, "");
    if (/^<\/?choices>$/i.test(s)) continue;
    if (/^`{3,}$/.test(s) || /^~{3,}$/.test(s)) continue;
    if (s) out.push(s);
  }
  return out;
}

const CJK =
  /[\u3040-\u30ff\u3400-\u9fff\uf900-\ufaff\u3001-\u303f\uff01-\uff60\uffe0-\uffee]/;

function isCjkChar(ch: string | undefined): boolean {
  return !!ch && CJK.test(ch);
}

function firstVisibleChar(s: string): string | undefined {
  const t = s.replace(/^[\s\u3000]+/, "");
  return t[0];
}

function lastVisibleChar(s: string): string | undefined {
  const t = s.replace(/[\s\u3000]+$/, "");
  return t[t.length - 1];
}

function joinCjkAware(a: string, b: string): string {
  const left = a.replace(/[ \t]+$/, "");
  const right = b.replace(/^[ \t]+/, "");
  const inner = right.replace(/^(\*\*|__|\*|_)([\s\S]*)\1$/, "$2");
  const glue =
    isCjkChar(lastVisibleChar(left)) || isCjkChar(firstVisibleChar(inner))
      ? ""
      : " ";
  return `${left}${glue}${right}`;
}

/** Join a line that is only emphasis with the previous/next line (common LLM wrapping). */
function collapseWrappedEmphasis(src: string): string {
  const lines = src.replace(/\r\n/g, "\n").split("\n");
  const onlyEm = (s: string) =>
    /^\s*((\*\*|__)[^*_\n]+?\2|(\*|_)[^*_\n]+?\3)\s*$/.test(s);
  const out: string[] = [];
  for (const line of lines) {
    if (
      onlyEm(line) &&
      out.length > 0 &&
      out[out.length - 1] !== undefined &&
      !/^\s*$/.test(out[out.length - 1]!) &&
      !/^\s*```/.test(out[out.length - 1]!)
    ) {
      out[out.length - 1] = joinCjkAware(out[out.length - 1]!, line.trim());
      continue;
    }
    if (
      out.length > 0 &&
      onlyEm(out[out.length - 1]!) &&
      !/^\s*$/.test(line) &&
      !/^\s*```/.test(line) &&
      !/^\s*#{1,6}\s/.test(line)
    ) {
      out[out.length - 1] = joinCjkAware(out[out.length - 1]!.trim(), line);
      continue;
    }
    out.push(line);
  }
  return out.join("\n");
}

/** Drop ASCII spaces around `**bold**` when both sides are Japanese. */
function tightenCjkMdEmphasis(src: string): string {
  const cjk = CJK.source;
  const left = new RegExp(`(${cjk})[ \\t]+(\\*\\*|__)([^*_\\n]+?)\\2`, "g");
  const right = new RegExp(`(\\*\\*|__)([^*_\\n]+?)\\1[ \\t]+(?=${cjk})`, "g");
  return src.replace(left, "$1$2$3$2").replace(right, "$1$2$1");
}

function skipSpaceNodes(
  node: ChildNode | null,
  dir: "prev" | "next",
): ChildNode | null {
  let n = node;
  while (
    n &&
    n.nodeType === Node.TEXT_NODE &&
    /^[\s\u3000]*$/.test((n as Text).data)
  ) {
    n = dir === "prev" ? n.previousSibling : n.nextSibling;
  }
  return n;
}

function unwrapBreaksAroundEmphasis(el: Element): void {
  const inner = el.textContent ?? "";
  const prev = skipSpaceNodes(el.previousSibling, "prev");
  if (prev?.nodeName === "BR") {
    const before = skipSpaceNodes(prev.previousSibling, "prev");
    const beforeText = before?.textContent ?? "";
    if (
      isCjkChar(lastVisibleChar(beforeText)) ||
      isCjkChar(firstVisibleChar(inner))
    ) {
      prev.parentNode?.removeChild(prev);
    }
  }
  const next = skipSpaceNodes(el.nextSibling, "next");
  if (next?.nodeName === "BR") {
    const after = skipSpaceNodes(next.nextSibling, "next");
    const afterText = after?.textContent ?? "";
    if (
      isCjkChar(lastVisibleChar(inner)) ||
      isCjkChar(firstVisibleChar(afterText))
    ) {
      next.parentNode?.removeChild(next);
    }
  }
}

/** Model-wrapped Japanese lines become `<br>` and look like huge gaps around bold. */
function unwrapCjkBreaksInLists(root: HTMLElement): void {
  for (const scope of Array.from(root.querySelectorAll("li, blockquote"))) {
    for (const br of Array.from(scope.querySelectorAll("br"))) {
      if (br.closest("pre, code")) continue;
      const prev = skipSpaceNodes(br.previousSibling, "prev");
      const next = skipSpaceNodes(br.nextSibling, "next");
      const prevText = prev?.textContent ?? "";
      const nextText = next?.textContent ?? "";
      if (
        isCjkChar(lastVisibleChar(prevText)) &&
        isCjkChar(firstVisibleChar(nextText))
      ) {
        br.remove();
      }
    }
  }
}

/** Loose lists split a sentence across <p> tags when the model wraps **bold**. */
function mergeListItemParagraphs(root: HTMLElement, doc: Document): void {
  for (const li of Array.from(root.querySelectorAll("li"))) {
    if (li.querySelector(":scope > ul, :scope > ol, :scope > pre, :scope > table")) {
      continue;
    }
    const ps = Array.from(li.querySelectorAll(":scope > p"));
    if (ps.length <= 1) continue;
    const first = ps[0];
    if (!first) continue;
    for (const p of ps.slice(1)) {
      if (first.lastChild && first.lastChild.nodeType === Node.TEXT_NODE) {
        const t = first.lastChild as Text;
        if (!/[\s\u3000]$/.test(t.data) && !isCjkChar(lastVisibleChar(t.data))) {
          t.data += " ";
        }
      } else if (first.lastChild) {
        const last = first.lastChild.textContent ?? "";
        if (!isCjkChar(lastVisibleChar(last))) {
          first.appendChild(doc.createTextNode(" "));
        }
      }
      while (p.firstChild) first.appendChild(p.firstChild);
      p.remove();
    }
  }
}

function tightenCjkInlineGaps(root: HTMLElement): void {
  for (const el of Array.from(root.querySelectorAll("strong, em, b, i"))) {
    for (const node of Array.from(el.childNodes)) {
      if (node.nodeType !== Node.TEXT_NODE) continue;
      const t = node as Text;
      if (CJK.test(t.data)) {
        t.data = t.data.replace(/^[\s\u3000]+|[\s\u3000]+$/g, "");
      }
    }

    const inner = el.textContent ?? "";
    const prev = el.previousSibling;
    if (prev?.nodeType === Node.TEXT_NODE) {
      const t = prev as Text;
      if (
        /[\s\u3000]$/.test(t.data) &&
        (isCjkChar(lastVisibleChar(t.data)) || isCjkChar(firstVisibleChar(inner)))
      ) {
        t.data = t.data.replace(/[\s\u3000]+$/, "");
      }
    }
    const next = el.nextSibling;
    if (next?.nodeType === Node.TEXT_NODE) {
      const t = next as Text;
      if (
        /^[\s\u3000]/.test(t.data) &&
        (isCjkChar(lastVisibleChar(inner)) || isCjkChar(firstVisibleChar(t.data)))
      ) {
        t.data = t.data.replace(/^[\s\u3000]+/, "");
      }
    }
    unwrapBreaksAroundEmphasis(el);
  }
}

function enhanceCallouts(root: HTMLElement): void {
  for (const bq of Array.from(root.querySelectorAll("blockquote"))) {
    const strong = bq.querySelector("strong");
    let label = (strong?.textContent ?? "").replace(/[:：]/g, "").trim();
    if (!CALLOUT_LABELS.has(label)) {
      const t = (bq.textContent ?? "").trim();
      const m = t.match(CALLOUT_HEADER_RE);
      label = m?.[1] ?? "";
    }
    if (!CALLOUT_LABELS.has(label)) continue;
    bq.classList.add("md-callout", `md-callout-${label}`);
  }
}

/** One-column tables headed 補足/注意/… are callouts, not data tables. */
function unwrapCalloutTables(root: HTMLElement, doc: Document): void {
  for (const table of Array.from(root.querySelectorAll("table"))) {
    const rows = Array.from(table.querySelectorAll("tr"));
    if (rows.length === 0) continue;
    const colCount = Math.max(
      ...rows.map((r) => r.querySelectorAll("th, td").length),
      0,
    );
    if (colCount !== 1) continue;
    const headerCell = rows[0].querySelector("th, td");
    const header = (headerCell?.textContent ?? "").trim();
    const m = header.match(CALLOUT_HEADER_RE);
    if (!m?.[1]) continue;

    const bq = doc.createElement("blockquote");
    bq.className = `md-callout md-callout-${m[1]}`;
    const title = doc.createElement("p");
    const strong = doc.createElement("strong");
    strong.textContent = header;
    title.appendChild(strong);
    bq.appendChild(title);
    for (const row of rows.slice(1)) {
      const cell = row.querySelector("td, th");
      if (!cell) continue;
      while (cell.firstChild) bq.appendChild(cell.firstChild);
    }
    for (const hr of Array.from(bq.querySelectorAll("hr"))) {
      if (!hr.nextSibling) hr.remove();
    }
    const wrap = table.closest(".md-table-wrap") ?? table;
    wrap.replaceWith(bq);
  }
}

function linkCiteNumbers(
  root: HTMLElement,
  doc: Document,
  citeNos: ReadonlySet<number>,
): void {
  if (citeNos.size === 0) return;
  const walker = doc.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
    acceptNode(node) {
      const el = (node as Text).parentElement;
      if (!el) return NodeFilter.FILTER_REJECT;
      if (el.closest("pre, code, a, button")) return NodeFilter.FILTER_REJECT;
      return NodeFilter.FILTER_ACCEPT;
    },
  });

  const hits: { node: Text; parts: Array<string | number> }[] = [];
  let n: Node | null;
  while ((n = walker.nextNode())) {
    const textNode = n as Text;
    const text = textNode.data;
    if (!/\[\d+\]/.test(text)) continue;
    const parts: Array<string | number> = [];
    const re = /\[(\d+)\]/g;
    let last = 0;
    let m: RegExpExecArray | null;
    let any = false;
    while ((m = re.exec(text))) {
      const num = Number(m[1]);
      if (!citeNos.has(num)) continue;
      any = true;
      if (m.index > last) parts.push(text.slice(last, m.index));
      parts.push(num);
      last = m.index + m[0].length;
    }
    if (!any) continue;
    if (last < text.length) parts.push(text.slice(last));
    hits.push({ node: textNode, parts });
  }

  for (const { node, parts } of hits) {
    const frag = doc.createDocumentFragment();
    for (const p of parts) {
      if (typeof p === "string") {
        frag.appendChild(doc.createTextNode(p));
      } else {
        const btn = doc.createElement("button");
        btn.type = "button";
        btn.className = "md-cite";
        btn.dataset.cite = String(p);
        btn.textContent = `[${p}]`;
        frag.appendChild(btn);
      }
    }
    node.parentNode?.replaceChild(frag, node);
  }
}

/** Safe Markdown → HTML for assistant messages. Does not use prepareMarkdownChunk. */
export function renderAssistantMdHtml(
  text: string,
  citeNos: ReadonlySet<number>,
): string {
  const prepared = tightenCjkMdEmphasis(
    collapseWrappedEmphasis(convertArticleKanjiNumerals(text)),
  );
  const html = chatMarked.parse(prepared, { async: false }) as string;
  const doc = new DOMParser().parseFromString(
    `<div id="root">${html}</div>`,
    "text/html",
  );
  const root = doc.getElementById("root");
  if (!root) return html;
  wrapMarkdownTables(root, doc);
  unwrapCalloutTables(root, doc);
  mergeListItemParagraphs(root, doc);
  enhanceCallouts(root);
  unwrapCjkBreaksInLists(root);
  tightenCjkInlineGaps(root);
  linkCiteNumbers(root, doc, citeNos);
  return root.innerHTML;
}
