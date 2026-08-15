import {
  collectPreviewHighlightTerms,
  isMarkdownPath,
  renderMarkdownHtml,
} from "../popup/markdownPreview";
import {
  formatCourtCaseJsonHtml,
  isCourtCaseJsonTarget,
} from "./courtCaseFormat";

const LEGAL_MD_HIGHLIGHT_NAME = "argos-notes-legal";

const KANJI_DIGIT: Record<string, number> = {
  〇: 0,
  零: 0,
  一: 1,
  二: 2,
  三: 3,
  四: 4,
  五: 5,
  六: 6,
  七: 7,
  八: 8,
  九: 9,
};

const KANJI_NUMERAL_CHARS = "〇零一二三四五六七八九十百千万";
const BRANCH_NUMERAL_CHARS = `${KANJI_NUMERAL_CHARS}０-９0-9`;
/** Legal unit suffixes that follow 第… (条/項/号 and common structural units). */
const LEGAL_UNIT_SUFFIX = "条|項|号|編|章|節|款|目";

const LEGAL_REF_KANJI_RE = new RegExp(
  `第([${KANJI_NUMERAL_CHARS}]+)(${LEGAL_UNIT_SUFFIX})(?:の([${BRANCH_NUMERAL_CHARS}]+))?`,
  "g",
);

function parseArabicNumeral(text: string): number | null {
  const n = Number(
    text.replace(/[０-９]/g, (c) =>
      String.fromCharCode(c.charCodeAt(0) - 0xff10 + 0x30),
    ),
  );
  return Number.isFinite(n) ? n : null;
}

/**
 * Digit-by-digit kanji (第五〇九条, 第一〇条). 〇/零 is a place-holding zero,
 * not a value that replaces the previous digit.
 */
function parseKanjiDigitRun(text: string): number | null {
  let n = 0;
  let digits = 0;
  for (const c of text) {
    if (!(c in KANJI_DIGIT)) return null;
    n = n * 10 + KANJI_DIGIT[c]!;
    digits += 1;
    if (digits > 8) return null;
  }
  return digits > 0 ? n : null;
}

/** Traditional place-value (第五百二条, 第百二十一項). */
function parseKanjiPlaceValue(text: string): number | null {
  let result = 0;
  let current = 0;
  for (const c of text) {
    if (c in KANJI_DIGIT) {
      current = KANJI_DIGIT[c]!;
      continue;
    }
    if (c === "十") {
      result += (current || 1) * 10;
      current = 0;
      continue;
    }
    if (c === "百") {
      result += (current || 1) * 100;
      current = 0;
      continue;
    }
    if (c === "千") {
      result += (current || 1) * 1000;
      current = 0;
      continue;
    }
    if (c === "万") {
      result = (result + current) * 10000;
      current = 0;
      continue;
    }
    return null;
  }
  return result + current;
}

/** Parse Japanese legal-style numerals (e.g. 五百二 → 502, 五〇九 → 509). */
export function parseKanjiNumeral(text: string): number | null {
  const s = text.trim();
  if (!s) return null;

  if (/^[0-9０-９]+$/.test(s)) {
    return parseArabicNumeral(s);
  }

  // 十/百/千/万 が無い列（五〇九・一〇・二三）はアラビア数字と同じ桁の並び。
  if (![...s].some((c) => c === "十" || c === "百" || c === "千" || c === "万")) {
    return parseKanjiDigitRun(s);
  }
  return parseKanjiPlaceValue(s);
}

const PLACE_DIGIT = ["", "一", "二", "三", "四", "五", "六", "七", "八", "九"];

function formatPlaceBelow10000(n: number): string {
  if (n <= 0) return "";
  let s = "";
  const sen = Math.floor(n / 1000);
  const hyaku = Math.floor(n / 100) % 10;
  const ju = Math.floor(n / 10) % 10;
  const ichi = n % 10;
  if (sen) {
    if (sen > 1) s += PLACE_DIGIT[sen];
    s += "千";
  }
  if (hyaku) {
    if (hyaku > 1) s += PLACE_DIGIT[hyaku];
    s += "百";
  }
  if (ju) {
    if (ju > 1) s += PLACE_DIGIT[ju];
    s += "十";
  }
  if (ichi) s += PLACE_DIGIT[ichi];
  return s;
}

export function formatKanjiPlaceNumeral(n: number): string {
  const x = Math.floor(n);
  if (!Number.isFinite(x) || x < 0) return "";
  if (x === 0) return "〇";
  if (x < 10000) return formatPlaceBelow10000(x);
  const man = Math.floor(x / 10000);
  const rest = x % 10000;
  let s = man === 1 ? "万" : `${formatPlaceBelow10000(man)}万`;
  if (rest) s += formatPlaceBelow10000(rest);
  return s;
}

export function formatKanjiDigitNumeral(n: number, zero = "〇"): string {
  const x = Math.floor(n);
  if (!Number.isFinite(x) || x < 0) return "";
  if (x === 0) return zero;
  const digits = ["〇", "一", "二", "三", "四", "五", "六", "七", "八", "九"];
  digits[0] = zero;
  return String(x).replace(/[0-9]/g, (d) => digits[Number(d)] ?? d);
}

function formatFullwidthNumeral(n: number): string {
  const x = Math.floor(n);
  if (!Number.isFinite(x) || x < 0) return "";
  return String(x).replace(/[0-9]/g, (d) =>
    String.fromCharCode("０".charCodeAt(0) + Number(d)),
  );
}

function legalRefSearchRe(): RegExp {
  return new RegExp(
    `第([${KANJI_NUMERAL_CHARS}0-9０-９]+)(${LEGAL_UNIT_SUFFIX})(?:の([${BRANCH_NUMERAL_CHARS}]+))?`,
    "g",
  );
}

type NumStyle = "arabic" | "fullwidth" | "digitZero" | "digitRei" | "place";

function formatStyledNum(n: number, style: NumStyle): string {
  switch (style) {
    case "arabic":
      return String(Math.floor(n));
    case "fullwidth":
      return formatFullwidthNumeral(n);
    case "digitZero":
      return formatKanjiDigitNumeral(n, "〇");
    case "digitRei":
      return formatKanjiDigitNumeral(n, "零");
    case "place":
      return formatKanjiPlaceNumeral(n);
  }
}

function rewriteLegalRefs(text: string, style: NumStyle): string {
  return text.replace(
    legalRefSearchRe(),
    (match, main: string, unit: string, branch?: string) => {
      const mainNum = parseKanjiNumeral(main);
      if (mainNum == null || mainNum > 999999) return match;
      let out = `第${formatStyledNum(mainNum, style)}${unit}`;
      if (branch) {
        const branchNum = parseKanjiNumeral(branch);
        if (branchNum != null) {
          out += `の${formatStyledNum(branchNum, style)}`;
        } else {
          out += `の${branch}`;
        }
      }
      return out;
    },
  );
}

/** Search/highlight surfaces for 第555条 ↔ 第五五五条 ↔ 第五百五十五条. */
export function legalRefTextVariants(text: string): string[] {
  if (!legalRefSearchRe().test(text)) return [text];
  const out: string[] = [];
  const push = (s: string) => {
    if (s && !out.includes(s)) out.push(s);
  };
  push(text);
  for (const style of [
    "arabic",
    "fullwidth",
    "digitZero",
    "digitRei",
    "place",
  ] as NumStyle[]) {
    push(rewriteLegalRefs(text, style));
  }
  return out;
}

/**
 * Convert 第…条 / 第…項 / 第…号 etc. kanji numerals to Arabic (display-only).
 * e.g. 第五百二条 → 第502条, 第五〇九条 → 第509条, 第一項第八号 → 第1項第8号
 */
export function convertArticleKanjiNumerals(text: string): string {
  return text.replace(
    LEGAL_REF_KANJI_RE,
    (_match, main: string, unit: string, branch?: string) => {
      const mainNum = parseKanjiNumeral(main);
      if (mainNum == null) return _match;
      let out = `第${mainNum}${unit}`;
      if (branch) {
        const branchNum = parseKanjiNumeral(branch);
        out += branchNum != null ? `の${branchNum}` : `の${branch}`;
      }
      return out;
    },
  );
}

/** Whether this note item should use legal MD formatting when the toggle is on. */
export function isLegalMdFormatTarget(path: string, body: string): boolean {
  if (isMarkdownPath(path)) return true;
  if (/^#{1,6}\s*第.+/m.test(body)) return true;
  if (/^\s*[-*+]\s+\*\*[^*]+\*\*/m.test(body)) return true;
  // Single-paragraph articles often use an empty bold placeholder: - ****:
  if (/^\s*[-*+]\s+\*\*\*\*/m.test(body)) return true;
  return false;
}

/** MD or court-case JSON that the 整形 toggle can format. */
export function isLegalDisplayTarget(path: string, body: string): boolean {
  if (isCourtCaseJsonTarget(body)) return true;
  return isLegalMdFormatTarget(path, body);
}

/**
 * Drop empty 項 placeholders (`****`) used when an article has only one
 * unnumbered paragraph, e.g. `- ****: 本文` → `- 本文`.
 */
export function stripEmptyParagraphMarkers(text: string): string {
  return text.replace(/^(\s*[-*+]\s+)\*\*\*\*\s*:?\s*/gm, "$1");
}

/** Display-only: kanji article numerals → Arabic, then Markdown → HTML. */
export function formatLegalMdHtml(body: string): string {
  const prepared = stripEmptyParagraphMarkers(convertArticleKanjiNumerals(body));
  return renderMarkdownHtml(prepared);
}

export type LegalDisplayKind = "court" | "md";

export type LegalDisplayResult = {
  html: string;
  kind: LegalDisplayKind;
};

/** Display-only formatter for the 整形 toggle (court JSON first, then legal MD). */
export function formatLegalDisplayHtml(
  path: string,
  body: string,
): LegalDisplayResult | null {
  if (isCourtCaseJsonTarget(body)) {
    const html = formatCourtCaseJsonHtml(body, {
      formatPlainText: convertArticleKanjiNumerals,
    });
    if (html) return { html, kind: "court" };
  }
  if (isLegalMdFormatTarget(path, body)) {
    return { html: formatLegalMdHtml(body), kind: "md" };
  }
  return null;
}

export function legalMdHighlightTerms(
  query: string,
  highlightTerms?: string[],
): string[] {
  return collectPreviewHighlightTerms(query, highlightTerms);
}

function escapeRegExp(s: string) {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function collectRanges(container: HTMLElement, terms: string[]): Range[] {
  if (terms.length === 0) return [];
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

export function clearLegalMdHighlights(): void {
  CSS.highlights?.delete(LEGAL_MD_HIGHLIGHT_NAME);
}

/** Apply CSS Custom Highlight across multiple legal-MD note bodies. */
export function applyLegalMdHighlights(
  entries: { el: HTMLElement; terms: string[] }[],
): void {
  clearLegalMdHighlights();
  if (!CSS.highlights || entries.length === 0) return;

  const ranges: Range[] = [];
  for (const { el, terms } of entries) {
    ranges.push(...collectRanges(el, terms));
  }
  if (ranges.length > 0) {
    CSS.highlights.set(LEGAL_MD_HIGHLIGHT_NAME, new Highlight(...ranges));
  }
}
