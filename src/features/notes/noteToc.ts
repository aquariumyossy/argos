import { lexNoteMemo, type NoteMemoToken } from "./noteMemoMd";

/** One heading the preview will render. `offset` is its start in the source. */
export type NoteHeading = {
  level: number;
  text: string;
  offset: number;
};

/** Marked can nest headings only inside these blocks. */
const CONTAINERS = new Set(["blockquote", "list", "list_item"]);

function containerChildren(token: NoteMemoToken): NoteMemoToken[] | null {
  if (!CONTAINERS.has(token.type)) return null;
  if (token.items?.length) return token.items;
  if (token.tokens?.length) return token.tokens;
  return null;
}

/**
 * Find `needle` in `source[from, end)`. A miss stops at `end` and never reads
 * the rest of the note. Inner blockquote and list tokens often drop `> ` or
 * the continuation indent, so their raw text is not in the source at all.
 */
function locate(source: string, needle: string, from: number, end: number): number {
  if (!needle || from >= end || needle.length > end - from) return -1;
  if (source.startsWith(needle, from)) return from;
  const last = end - needle.length;
  const first = needle.charCodeAt(0);
  for (let i = from + 1; i <= last; i++) {
    if (source.charCodeAt(i) !== first) continue;
    if (source.startsWith(needle, i)) return i;
  }
  return -1;
}

function walk(
  tokens: NoteMemoToken[],
  source: string,
  regionStart: number,
  regionEnd: number,
  out: NoteHeading[],
): number {
  let cursor = regionStart;
  for (const token of tokens) {
    const raw = token.raw ?? "";
    const nested = containerChildren(token);
    const isHeading = token.type === "heading";
    let start = cursor;
    let matched = false;
    if (raw && (isHeading || nested)) {
      const found = locate(source, raw, cursor, regionEnd);
      if (found >= 0) {
        start = found;
        matched = true;
      }
    } else if (raw && cursor + raw.length <= regionEnd && source.startsWith(raw, cursor)) {
      matched = true;
    }
    if (isHeading) {
      out.push({
        level: token.depth ?? 1,
        text: token.text ?? "",
        offset: start,
      });
    } else if (nested) {
      const childEnd = matched ? Math.min(regionEnd, start + raw.length) : regionEnd;
      const childCursor = walk(nested, source, start, childEnd, out);
      if (!matched && childCursor > cursor) {
        cursor = Math.min(regionEnd, childCursor);
        continue;
      }
    }
    if (matched) cursor = Math.min(regionEnd, start + raw.length);
  }
  return cursor;
}

/**
 * Headings in the same order the note preview renders them.
 * Uses the preview's marked lexer, so fences, setext, and nested headings match.
 * Offsets are in LF coordinates, the same as a textarea value (browsers drop CR).
 */
export function noteHeadings(md: string): NoteHeading[] {
  const text = md.replace(/\r\n/g, "\n").replace(/\r/g, "\n");
  const out: NoteHeading[] = [];
  walk(lexNoteMemo(text), text, 0, text.length, out);
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
