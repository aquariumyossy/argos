import type { ReactNode } from "react";
import { highlightText } from "../search/highlightText";

const MARK_RE = /〔(?:-|\+|注)[^〕]*〕/g;

function markClass(token: string): string | null {
  if (token.startsWith("〔-")) return "preview-rev preview-rev--del";
  if (token.startsWith("〔+")) return "preview-rev preview-rev--ins";
  if (token.startsWith("〔注")) return "preview-rev preview-rev--note";
  return null;
}

/** 〔-著者: 本文〕 or 〔- 本文〕 → label vs deleted wording. */
function splitDelMark(token: string): { prefix: string; body: string; suffix: string } {
  const inner = token.slice(2, -1);
  if (inner.startsWith(" ")) {
    return { prefix: "〔- ", body: inner.slice(1), suffix: "〕" };
  }
  const sep = inner.indexOf(": ");
  if (sep >= 0) {
    return {
      prefix: `〔-${inner.slice(0, sep + 2)}`,
      body: inner.slice(sep + 2),
      suffix: "〕",
    };
  }
  return { prefix: token, body: "", suffix: "" };
}

function renderMark(
  token: string,
  key: number,
  query: string,
  highlightTerms?: string[],
): ReactNode {
  const cls = markClass(token);
  if (cls?.includes("preview-rev--del")) {
    const { prefix, body, suffix } = splitDelMark(token);
    return (
      <span key={key} className={cls}>
        {highlightText(prefix, query, highlightTerms)}
        {body ? (
          <span className="preview-rev-del-text">
            {highlightText(body, query, highlightTerms)}
          </span>
        ) : null}
        {suffix ? highlightText(suffix, query, highlightTerms) : null}
      </span>
    );
  }
  return (
    <span key={key} className={cls ?? undefined}>
      {highlightText(token, query, highlightTerms)}
    </span>
  );
}

/** Color 〔-〕 / 〔+〕 / 〔注〕 spans in extracted Word text. */
export function highlightDocxMarkup(
  text: string,
  query: string,
  highlightTerms?: string[],
): ReactNode {
  if (!text.includes("〔")) {
    return highlightText(text, query, highlightTerms);
  }
  const nodes: ReactNode[] = [];
  let last = 0;
  let i = 0;
  const re = new RegExp(MARK_RE.source, "g");
  let m: RegExpExecArray | null;
  while ((m = re.exec(text))) {
    if (m.index > last) {
      nodes.push(
        <span key={i++}>
          {highlightText(text.slice(last, m.index), query, highlightTerms)}
        </span>,
      );
    }
    nodes.push(renderMark(m[0], i++, query, highlightTerms));
    last = m.index + m[0].length;
  }
  if (last < text.length) {
    nodes.push(
      <span key={i++}>
        {highlightText(text.slice(last), query, highlightTerms)}
      </span>,
    );
  }
  return nodes.length ? nodes : highlightText(text, query, highlightTerms);
}
