import { marked } from "marked";

const PREVIEW_HIGHLIGHT_NAME = "argos-preview";

function escapeRegExp(s: string) {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/** Split query for highlight using the same delimiters as the Rust parser (outside quotes). */
function highlightTermsFromQuery(query: string): string[] {
  const chars = Array.from(query.trim());
  const out: string[] = [];
  let i = 0;
  const isDelim = (c: string) => /[\s\u3000,\uFF0C\u3001]/.test(c);

  while (i < chars.length) {
    while (i < chars.length && isDelim(chars[i])) i += 1;
    if (i >= chars.length) break;

    const exclude = chars[i] === "-" && i + 1 < chars.length && !isDelim(chars[i + 1]);
    if (exclude) i += 1;

    if (chars[i] === '"') {
      i += 1;
      const start = i;
      let closed = false;
      while (i < chars.length) {
        if (chars[i] === '"') {
          closed = true;
          break;
        }
        i += 1;
      }
      const inner = chars.slice(start, i).join("").trim();
      if (closed) i += 1;
      if (!exclude && inner) out.push(inner);
      if (!closed) break;
      continue;
    }

    const start = i;
    while (i < chars.length && !isDelim(chars[i])) i += 1;
    const term = chars.slice(start, i).join("").trim();
    if (!exclude && term) out.push(term);
  }
  return out;
}

/** Strip markdown syntax so search terms match rendered text, not raw markers. */
function stripMarkdownFromTerm(term: string): string {
  let t = term.trim();
  t = t.replace(/^#+\s*/, "");
  t = t.replace(/^[-*+]\s+/, "");
  t = t.replace(/\*\*([^*]+)\*\*/g, "$1");
  t = t.replace(/\*([^*]+)\*/g, "$1");
  t = t.replace(/__([^_]+)__/g, "$1");
  t = t.replace(/_([^_]+)_/g, "$1");
  t = t.replace(/[*#_`~[\]]/g, "");
  t = t.replace(/^[:\-–—・]+|[:\-–—・]+$/g, "").trim();
  return t;
}

function isUsefulHighlightTerm(term: string): boolean {
  if (!term) return false;
  return /[\p{L}\p{N}\p{Script=Han}]/u.test(term);
}

/** Drop tokens that are substrings of a longer kept term (e.g. 解/除 inside 解除). */
function dropSubsumedTerms(terms: string[]): string[] {
  const sorted = [...terms].sort((a, b) => b.length - a.length);
  const kept: string[] = [];
  for (const term of sorted) {
    const lower = term.toLowerCase();
    const subsumed = kept.some(
      (longer) => longer.length > term.length && longer.toLowerCase().includes(lower),
    );
    if (!subsumed) kept.push(term);
  }
  return kept;
}

export function collectPreviewHighlightTerms(
  query: string,
  highlightTerms?: string[],
): string[] {
  const raw = [
    ...(highlightTerms ?? []).filter((t) => t.trim().length > 0),
    ...highlightTermsFromQuery(query),
  ];
  const cleaned = Array.from(
    new Set(raw.map(stripMarkdownFromTerm).filter(isUsefulHighlightTerm)),
  );
  return dropSubsumedTerms(cleaned).sort((a, b) => b.length - a.length);
}

/** Normalize chunk text so block-level markdown parses even when the chunk starts mid-document. */
function prepareMarkdownChunk(text: string): string {
  return text
    .replace(/([^\n])(#{1,6}\s)/g, "$1\n\n$2")
    .replace(/([^\n])(-\s+\*\*)/g, "$1\n$2")
    .replace(/([^\n])(-\s+[^\s\-])/g, "$1\n$2");
}

export function isMarkdownPath(path: string): boolean {
  const base = path.replace(/\\/g, "/").split("/").pop() ?? "";
  const i = base.lastIndexOf(".");
  if (i <= 0 || i === base.length - 1) return false;
  const ext = base.slice(i + 1).toLowerCase();
  return ext === "md" || ext === "markdown";
}

export function renderMarkdownHtml(text: string): string {
  const prepared = prepareMarkdownChunk(text);
  const html = marked.parse(prepared, { breaks: true, gfm: true, async: false }) as string;
  return enhanceArticleListItemsHtml(html);
}

function isArticleNumber(text: string): boolean {
  const t = text.trim();
  return /^[\d０-９]+$/.test(t);
}

/** Split list item label (e.g. １) from body so the label stays top-aligned when text wraps. */
function enhanceArticleListItemsHtml(html: string): string {
  const doc = new DOMParser().parseFromString(`<div id="root">${html}</div>`, "text/html");
  const root = doc.getElementById("root");
  if (!root) return html;

  for (const li of Array.from(root.querySelectorAll("ul > li"))) {
    const strong =
      li.querySelector(":scope > strong:first-child") ??
      li.querySelector(":scope > p:first-child > strong:first-child");
    if (!strong || !isArticleNumber(strong.textContent ?? "")) continue;

    const row = (strong.parentElement === li ? li : strong.parentElement) as HTMLElement;
    row.classList.add("preview-article-row");
    strong.classList.add("preview-article-no");

    if (!strong.nextSibling) continue;

    const body = doc.createElement("span");
    body.className = "preview-article-body";
    while (strong.nextSibling) {
      body.appendChild(strong.nextSibling);
    }
    row.appendChild(body);
  }

  return root.innerHTML;
}

export function clearPreviewHighlights(): void {
  CSS.highlights?.delete(PREVIEW_HIGHLIGHT_NAME);
}

/** Paint highlights without inserting elements so inline layout stays intact. */
export function applyPreviewHighlights(container: HTMLElement, terms: string[]): void {
  clearPreviewHighlights();
  if (terms.length === 0 || !CSS.highlights) return;

  const ranges: Range[] = [];
  const walker = document.createTreeWalker(container, NodeFilter.SHOW_TEXT, {
    acceptNode(node) {
      if (node.parentElement?.closest("pre, code")) {
        return NodeFilter.FILTER_REJECT;
      }
      return NodeFilter.FILTER_ACCEPT;
    },
  });

  let node: Node | null;
  while ((node = walker.nextNode())) {
    const textNode = node as Text;
    const text = textNode.textContent ?? "";
    if (!text) continue;

    for (const term of terms) {
      const pattern = new RegExp(escapeRegExp(term), "gi");
      let match: RegExpExecArray | null;
      while ((match = pattern.exec(text)) !== null) {
        const range = document.createRange();
        range.setStart(textNode, match.index);
        range.setEnd(textNode, match.index + match[0].length);
        ranges.push(range);
      }
    }
  }

  if (ranges.length > 0) {
    CSS.highlights.set(PREVIEW_HIGHLIGHT_NAME, new Highlight(...ranges));
  }
}
