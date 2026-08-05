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
  const fromHit = (highlightTerms ?? []).filter((t) => t.trim().length > 0);
  const fromQuery =
    fromHit.length > 0
      ? []
      : highlightTermsFromQuery(query).filter((t) => {
          const hasDelim = /[\s\u3000,\uFF0C\u3001]/.test(t);
          if (!hasDelim && Array.from(t).length >= 8) return false;
          return true;
        });
  const raw = [...fromHit, ...fromQuery];
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

export function isHtmlPath(path: string): boolean {
  const base = path.replace(/\\/g, "/").split("/").pop() ?? "";
  const i = base.lastIndexOf(".");
  if (i <= 0 || i === base.length - 1) return false;
  const ext = base.slice(i + 1).toLowerCase();
  return ext === "html" || ext === "htm";
}

export function isJsonPath(path: string): boolean {
  const base = path.replace(/\\/g, "/").split("/").pop() ?? "";
  const i = base.lastIndexOf(".");
  if (i <= 0 || i === base.length - 1) return false;
  const ext = base.slice(i + 1).toLowerCase();
  return ext === "json";
}

/** Pretty-print JSON when valid; otherwise return the raw text unchanged. */
export function formatJsonForPreview(raw: string): string {
  try {
    return JSON.stringify(JSON.parse(raw), null, 2);
  } catch {
    return raw;
  }
}

/** Collapse whitespace for matching index chunks against pretty-printed JSON. */
export function collapseWhitespace(s: string): string {
  return s.replace(/\s+/g, "");
}

/** Strip snippet ellipsis (… / ...) added by the search backend. */
export function stripSnippetEllipsis(s: string): string {
  return s.replace(/\u2026/g, "").replace(/\.{2,}/g, "");
}

/**
 * Find the character offset in `haystack` that corresponds to a needle from an
 * index chunk (whitespace-tolerant). Returns -1 if not found.
 */
export function findCollapsedNeedleOffset(
  haystack: string,
  needle: string,
): number {
  const collapsedNeedle = collapseWhitespace(stripSnippetEllipsis(needle));
  if (!collapsedNeedle) return -1;
  // Prefer a stable prefix so oversized chunks still locate near the hit start.
  const probeLen = Math.min(200, collapsedNeedle.length);
  // Very short probes collide easily in JSON punctuation; require some substance.
  if (probeLen < 8) return -1;
  const probe = collapsedNeedle.slice(0, probeLen);

  let collapsedPos = 0;
  let matchStart = -1;
  for (let i = 0; i < haystack.length; i += 1) {
    const ch = haystack[i];
    if (ch === undefined || /\s/.test(ch)) continue;
    if (ch === probe[collapsedPos]) {
      if (collapsedPos === 0) matchStart = i;
      collapsedPos += 1;
      if (collapsedPos === probe.length) return matchStart;
    } else if (ch === probe[0]) {
      matchStart = i;
      collapsedPos = 1;
    } else {
      matchStart = -1;
      collapsedPos = 0;
    }
  }
  return -1;
}

/**
 * Locate a search hit inside pretty-printed JSON text.
 * Prefer the indexed chunk body over the UI snippet (snippets include ellipsis).
 */
export function findJsonHitOffset(
  displayText: string,
  previewText: string,
  snippet: string,
): number {
  const fromPreview = findCollapsedNeedleOffset(displayText, previewText);
  if (fromPreview >= 0) {
    // Refine to the snippet anchor within the chunk when possible.
    const core = stripSnippetEllipsis(snippet).trim();
    if (core.length >= 8) {
      const refined = findCollapsedNeedleOffset(displayText, core);
      if (refined >= fromPreview) return refined;
    }
    return fromPreview;
  }
  return findCollapsedNeedleOffset(displayText, snippet);
}

/** Split extracted HTML body on blank lines for prose preview paragraphs. */
export function splitProseParagraphs(text: string): string[] {
  return text
    .split(/\n\s*\n/)
    .map((p) => p.trim())
    .filter((p) => p.length > 0);
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
