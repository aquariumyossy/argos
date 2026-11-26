import { lexNoteMemo, type NoteMemoToken } from "./noteMemoMd";

/** One heading the preview will render. `offset` is its start in the source. */
export type NoteHeading = {
  level: number;
  text: string;
  offset: number;
};

function nestedBlocks(token: NoteMemoToken): NoteMemoToken[] | null {
  if (token.type === "heading") return null;
  if (token.items?.length) return token.items;
  if (token.tokens?.length) return token.tokens;
  return null;
}

function walk(
  tokens: NoteMemoToken[],
  source: string,
  regionStart: number,
  out: NoteHeading[],
) {
  let cursor = regionStart;
  for (const token of tokens) {
    const raw = token.raw ?? "";
    let start = raw ? source.indexOf(raw, cursor) : cursor;
    if (start < 0) start = cursor;
    if (token.type === "heading") {
      out.push({
        level: token.depth ?? 1,
        text: token.text ?? "",
        offset: start,
      });
    } else {
      const nested = nestedBlocks(token);
      if (nested) walk(nested, source, start, out);
    }
    cursor = start + raw.length;
  }
}

/**
 * Headings in the same order the note preview renders them.
 * Uses the preview's marked lexer, so fences, setext, and nested headings match.
 */
export function noteHeadings(md: string): NoteHeading[] {
  const out: NoteHeading[] = [];
  walk(lexNoteMemo(md), md, 0, out);
  return out;
}

/** Scroll a textarea so the source offset sits at the top, including wrapped lines. */
export function scrollTextareaToOffset(ta: HTMLTextAreaElement, offset: number) {
  const clamped = Math.max(0, Math.min(offset, ta.value.length));
  ta.focus({ preventScroll: true });
  ta.setSelectionRange(clamped, clamped);
  ta.scrollTop = textareaPrefixHeight(ta, clamped);
}

function textareaPrefixHeight(ta: HTMLTextAreaElement, offset: number): number {
  const style = window.getComputedStyle(ta);
  const padLeft = Number.parseFloat(style.paddingLeft) || 0;
  const padRight = Number.parseFloat(style.paddingRight) || 0;
  const contentWidth = Math.max(0, ta.clientWidth - padLeft - padRight);
  const mirror = document.createElement("div");
  mirror.style.position = "fixed";
  mirror.style.left = "0";
  mirror.style.top = "0";
  mirror.style.visibility = "hidden";
  mirror.style.pointerEvents = "none";
  mirror.style.boxSizing = "content-box";
  mirror.style.width = `${contentWidth}px`;
  mirror.style.height = "auto";
  mirror.style.minHeight = "0";
  mirror.style.maxHeight = "none";
  mirror.style.padding = "0";
  mirror.style.border = "0";
  mirror.style.overflow = "visible";
  mirror.style.whiteSpace = "pre-wrap";
  mirror.style.overflowWrap = style.overflowWrap || "break-word";
  mirror.style.wordWrap = "break-word";
  mirror.style.wordBreak = style.wordBreak;
  mirror.style.font = style.font;
  mirror.style.fontSize = style.fontSize;
  mirror.style.fontFamily = style.fontFamily;
  mirror.style.fontWeight = style.fontWeight;
  mirror.style.fontStyle = style.fontStyle;
  mirror.style.lineHeight = style.lineHeight;
  mirror.style.letterSpacing = style.letterSpacing;
  mirror.style.tabSize = style.tabSize;
  const prefix = ta.value.slice(0, offset);
  mirror.append(document.createTextNode(prefix), document.createElement("span"));
  const marker = mirror.lastElementChild;
  document.body.appendChild(mirror);
  const top = marker instanceof HTMLElement ? marker.offsetTop : 0;
  mirror.remove();
  return Math.max(0, top);
}
