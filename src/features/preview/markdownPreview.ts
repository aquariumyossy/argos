import { marked } from "marked";
import { collectHighlightTerms } from "../search/highlightTerms";

const PREVIEW_HIGHLIGHT_NAME = "argos-preview";

function escapeRegExp(s: string) {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
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

export function collectPreviewHighlightTerms(
  query: string,
  highlightTerms?: string[],
): string[] {
  const strippedExtra = (highlightTerms ?? [])
    .map(stripMarkdownFromTerm)
    .filter(Boolean);
  return collectHighlightTerms(stripMarkdownFromTerm(query) || query, strippedExtra);
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
    return decodeJsonStringEscapes(raw);
  }
}

/** Turn JSON string escapes into real characters so indexed chunks match formatted text. */
export function decodeJsonStringEscapes(s: string): string {
  return s
    .replace(/\\n/g, "\n")
    .replace(/\\r/g, "")
    .replace(/\\t/g, "\t")
    .replace(/\\"/g, '"');
}

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

function jsonValueHtml(value: unknown): string {
  if (value === null) {
    return `<span class="preview-json-null">null</span>`;
  }
  if (typeof value === "boolean" || typeof value === "number") {
    return `<span class="preview-json-scalar">${escapeHtml(String(value))}</span>`;
  }
  if (typeof value === "string") {
    return `<span class="preview-json-string">${escapeHtml(value)}</span>`;
  }
  if (Array.isArray(value)) {
    if (value.length === 0) {
      return `<span class="preview-json-empty">[]</span>`;
    }
    const items = value.map((v) => `<li>${jsonValueHtml(v)}</li>`).join("");
    return `<ol class="preview-json-array">${items}</ol>`;
  }
  if (typeof value !== "object") {
    return `<span class="preview-json-scalar">${escapeHtml(String(value))}</span>`;
  }
  const entries = Object.entries(value as Record<string, unknown>);
  if (entries.length === 0) {
    return `<span class="preview-json-empty">{}</span>`;
  }
  const rows = entries.map(
    ([k, v]) =>
      `<div class="preview-json-row"><dt>${escapeHtml(k)}</dt><dd>${jsonValueHtml(v)}</dd></div>`,
  );
  return `<dl class="preview-json-object">${rows.join("")}</dl>`;
}

/** Key/value HTML for generic JSON (real line breaks in strings). Null if not an object/array. */
export function formatGenericJsonHtml(raw: string): string | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw.trim());
  } catch {
    return null;
  }
  if (parsed === null || typeof parsed !== "object") return null;
  return `<div class="preview-json">${jsonValueHtml(parsed)}</div>`;
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

/**
 * Locate an index chunk inside formatted (non-JSON-source) preview text.
 * Unescapes `\\n` / `\\"` in the needle so court-case HTML can be matched.
 */
export function findFormattedContentOffset(
  haystack: string,
  previewText: string,
  snippet: string,
): number {
  const decodedPreview = decodeJsonStringEscapes(previewText);
  const decodedSnippet = decodeJsonStringEscapes(snippet);
  const fromPreview = findCollapsedNeedleOffset(haystack, decodedPreview);
  if (fromPreview >= 0) {
    const core = stripSnippetEllipsis(decodedSnippet).trim();
    if (core.length >= 8) {
      const refined = findCollapsedNeedleOffset(haystack, core);
      if (refined >= fromPreview) return refined;
    }
    return fromPreview;
  }
  const fromSnippet = findCollapsedNeedleOffset(haystack, decodedSnippet);
  if (fromSnippet >= 0) return fromSnippet;
  const run = decodedPreview.match(/[\p{L}\p{N}][\p{L}\p{N}\s、。．，]{7,}/u);
  if (run?.[0]) return findCollapsedNeedleOffset(haystack, run[0]);
  return -1;
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
export function enhanceArticleListItemsHtml(html: string): string {
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

  wrapMarkdownTables(root, doc);

  return root.innerHTML;
}

export function wrapMarkdownTables(root: HTMLElement, doc: Document): void {
  for (const table of Array.from(root.querySelectorAll("table"))) {
    if (table.parentElement?.classList.contains("md-table-wrap")) continue;
    const wrap = doc.createElement("div");
    wrap.className = "md-table-wrap";
    table.replaceWith(wrap);
    wrap.appendChild(table);
  }
}

export function clearPreviewHighlights(): void {
  CSS.highlights?.delete(PREVIEW_HIGHLIGHT_NAME);
}

function collectPreviewHighlightRanges(
  container: HTMLElement,
  terms: string[],
): Range[] {
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
  return ranges;
}

/** Paint highlights without inserting elements so inline layout stays intact. */
export function applyPreviewHighlights(
  containers: HTMLElement[],
  terms: string[],
): void {
  clearPreviewHighlights();
  if (terms.length === 0 || !CSS.highlights || containers.length === 0) return;

  const ranges = containers.flatMap((el) =>
    collectPreviewHighlightRanges(el, terms),
  );
  if (ranges.length > 0) {
    CSS.highlights.set(PREVIEW_HIGHLIGHT_NAME, new Highlight(...ranges));
  }
}
