import type { ReactNode } from "react";
import {
  collectHighlightTerms,
  highlightTermsFromQuery,
} from "./highlightTerms";

export { collectHighlightTerms, highlightTermsFromQuery };

function escapeRegExp(s: string) {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/** Highlight matching terms in plain text (returns React nodes with <mark>). */
export function highlightText(
  text: string,
  query: string,
  highlightTerms?: string[],
): ReactNode {
  const terms = collectHighlightTerms(query, highlightTerms);
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
