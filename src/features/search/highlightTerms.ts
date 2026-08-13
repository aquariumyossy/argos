/** Split query for highlight using the same delimiters as the Rust parser (outside quotes). */
export function highlightTermsFromQuery(query: string): string[] {
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

/** Particles / connectives used to split an unspaced Japanese query into content words. */
const CONTENT_DELIMS = [
  "については",
  "について",
  "に対して",
  "に対する",
  "において",
  "における",
  "による",
  "として",
  "という",
  "および",
  "または",
  "ならびに",
  "など",
  "より",
  "から",
  "まで",
  "の",
  "を",
  "に",
  "は",
  "が",
  "も",
  "と",
  "で",
  "へ",
  "や",
  "か",
].sort((a, b) => Array.from(b).length - Array.from(a).length);

function isSingleHiragana(s: string): boolean {
  const chars = Array.from(s);
  return chars.length === 1 && /^[\u3041-\u3096]$/u.test(chars[0] ?? "");
}

export function isNoiseHighlightTerm(s: string): boolean {
  const t = s.trim();
  if (!t) return true;
  if (CONTENT_DELIMS.includes(t)) return true;
  return isSingleHiragana(t);
}

/** Split `反社会的勢力の排除` → `反社会的勢力`, `排除`. */
export function contentPartsFromTerm(term: string): string[] {
  const chars = Array.from(term.trim());
  if (chars.length === 0) return [];
  const parts: string[] = [];
  let buf: string[] = [];
  let i = 0;
  while (i < chars.length) {
    const rest = chars.slice(i).join("");
    const delim = CONTENT_DELIMS.find((d) => rest.startsWith(d));
    if (delim) {
      const piece = buf.join("").trim();
      if (piece) parts.push(piece);
      buf = [];
      i += Array.from(delim).length;
      continue;
    }
    buf.push(chars[i] ?? "");
    i += 1;
  }
  const piece = buf.join("").trim();
  if (piece) parts.push(piece);
  return parts;
}

function isUsefulHighlightTerm(term: string): boolean {
  if (!term || isNoiseHighlightTerm(term)) return false;
  if (!/[\p{L}\p{N}\p{Script=Han}]/u.test(term)) return false;
  return true;
}

/** Drop 1-char pieces inside a longer term (解/除 in 解除). Keep content words. */
function dropTinySubsumedTerms(terms: string[]): string[] {
  const sorted = [...terms].sort(
    (a, b) => Array.from(b).length - Array.from(a).length,
  );
  const kept: string[] = [];
  for (const term of sorted) {
    const lower = term.toLowerCase();
    if (kept.some((k) => k.toLowerCase() === lower)) continue;
    const charLen = Array.from(term).length;
    const subsumed = kept.some(
      (longer) =>
        Array.from(longer).length > charLen && longer.toLowerCase().includes(lower),
    );
    if (subsumed && charLen <= 1) continue;
    kept.push(term);
  }
  return kept;
}

/**
 * Terms to paint in list / preview: the typed query, plus content words
 * (morph-like splits and backend chips). Particles and 1-char debris stay out.
 */
export function collectHighlightTerms(
  query: string,
  highlightTerms?: string[],
): string[] {
  const fromHit = (highlightTerms ?? []).filter((t) => t.trim().length > 0);
  const fromQuery = highlightTermsFromQuery(query).filter((t) => {
    const hasDelim = /[\s\u3000,\uFF0C\u3001]/.test(t);
    if (!hasDelim && Array.from(t).length > 40) return false;
    return true;
  });
  const rawQuery = query.trim();
  const extra =
    rawQuery && Array.from(rawQuery).length <= 40 ? [rawQuery] : [];
  const expanded: string[] = [];
  for (const t of [...fromHit, ...fromQuery, ...extra]) {
    const trimmed = t.trim();
    if (!trimmed) continue;
    expanded.push(trimmed);
    for (const part of contentPartsFromTerm(trimmed)) {
      expanded.push(part);
    }
  }
  const cleaned = Array.from(new Set(expanded.filter(isUsefulHighlightTerm)));
  return dropTinySubsumedTerms(cleaned).sort(
    (a, b) => Array.from(b).length - Array.from(a).length,
  );
}
