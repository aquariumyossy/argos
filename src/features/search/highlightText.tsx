import type { ReactNode } from "react";

function escapeRegExp(s: string) {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

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

/** Highlight matching terms in plain text (returns React nodes with <mark>). */
export function highlightText(
  text: string,
  query: string,
  highlightTerms?: string[],
): ReactNode {
  const fromHit = (highlightTerms ?? []).filter((t) => t.trim().length > 0);
  // When the backend already returns morph content terms, do not also highlight
  // the entire unspaced Japanese query as one giant term.
  const fromQuery =
    fromHit.length > 0
      ? []
      : highlightTermsFromQuery(query).filter((t) => {
          const hasDelim = /[\s\u3000,\uFF0C\u3001]/.test(t);
          if (!hasDelim && Array.from(t).length >= 8) return false;
          return true;
        });
  const terms = Array.from(new Set([...fromHit, ...fromQuery].filter(Boolean))).sort(
    (a, b) => b.length - a.length,
  );
  if (terms.length === 0) return text;

  const pattern = terms.map(escapeRegExp).join("|");
  const parts = text.split(new RegExp(`(${pattern})`, "gi"));
  return parts.map((part, i) =>
    terms.some((t) => part.toLowerCase() === t.toLowerCase()) ? (
      <mark key={i}>{part}</mark>
    ) : (
      <span key={i}>{part}</span>
    ),
  );
}
