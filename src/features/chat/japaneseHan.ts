import { HAN_BMP_A, HAN_BMP_A0, HAN_BMP_B, HAN_BMP_B0, HAN_EXTRA } from "./japaneseHanData";

const SHOWN_LIMIT = 12;

function decodeBase64(value: string): Uint8Array {
  const binary = atob(value);
  const out = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) {
    out[i] = binary.charCodeAt(i);
  }
  return out;
}

const bmpA = decodeBase64(HAN_BMP_A);
const bmpB = decodeBase64(HAN_BMP_B);
const extra = new Set(HAN_EXTRA);

function bitAt(table: Uint8Array, index: number): boolean {
  return (table[index >> 3] & (1 << (index & 7))) !== 0;
}

/** CJK unified / compatibility ideographs, including extensions. */
export function isCjkIdeograph(cp: number): boolean {
  return (
    (cp >= 0x3400 && cp <= 0x4dbf) ||
    (cp >= 0x4e00 && cp <= 0x9fff) ||
    (cp >= 0xf900 && cp <= 0xfaff) ||
    (cp >= 0x20000 && cp <= 0x2a6df) ||
    (cp >= 0x2a700 && cp <= 0x2b73f) ||
    (cp >= 0x2b740 && cp <= 0x2b81f) ||
    (cp >= 0x2b820 && cp <= 0x2ceaf) ||
    (cp >= 0x2ceb0 && cp <= 0x2ebef) ||
    (cp >= 0x30000 && cp <= 0x3134f) ||
    (cp >= 0x31350 && cp <= 0x323af)
  );
}

export function isJapaneseHan(cp: number): boolean {
  if (cp >= HAN_BMP_A0 && cp < HAN_BMP_A0 + bmpA.length * 8) {
    return bitAt(bmpA, cp - HAN_BMP_A0);
  }
  if (cp >= HAN_BMP_B0 && cp < HAN_BMP_B0 + bmpB.length * 8) {
    return bitAt(bmpB, cp - HAN_BMP_B0);
  }
  return extra.has(cp);
}

/**
 * Writing systems that have no place in a Japanese legal document.
 * Latin and Greek stay allowed: URLs, ISO, and α版 are legitimate.
 */
export function isForeignScript(cp: number): boolean {
  return (
    (cp >= 0x0400 && cp <= 0x052f) ||
    (cp >= 0x0590 && cp <= 0x05ff) ||
    (cp >= 0x0600 && cp <= 0x06ff) ||
    (cp >= 0x0700 && cp <= 0x077f) ||
    (cp >= 0x0900 && cp <= 0x097f) ||
    (cp >= 0x0e00 && cp <= 0x0e7f) ||
    (cp >= 0x1100 && cp <= 0x11ff) ||
    (cp >= 0x2e80 && cp <= 0x2fdf) ||
    (cp >= 0x3100 && cp <= 0x312f) ||
    (cp >= 0x3130 && cp <= 0x318f) ||
    (cp >= 0xa960 && cp <= 0xa97f) ||
    (cp >= 0xac00 && cp <= 0xd7ff) ||
    (cp >= 0xfb50 && cp <= 0xfdff) ||
    (cp >= 0xfe70 && cp <= 0xfefc) ||
    (cp >= 0xffa0 && cp <= 0xffdc)
  );
}

/** Distinct glyphs that do not belong in Japanese text, in first-seen order. */
export function foreignChars(...parts: string[]): string[] {
  const seen = new Set<string>();
  const ordered: string[] = [];
  for (const part of parts) {
    if (!part) {
      continue;
    }
    for (const ch of part) {
      const cp = ch.codePointAt(0);
      if (cp === undefined || seen.has(ch)) {
        continue;
      }
      const foreign = isForeignScript(cp) || (isCjkIdeograph(cp) && !isJapaneseHan(cp));
      if (!foreign) {
        continue;
      }
      seen.add(ch);
      ordered.push(ch);
    }
  }
  return ordered;
}

function formatChars(chars: string[]): string {
  const shown = chars.slice(0, SHOWN_LIMIT);
  const more = chars.length > SHOWN_LIMIT ? ` ほか${chars.length - SHOWN_LIMIT}字` : "";
  return `${shown.join("、")}${more}`;
}

/** Chat-pane notice. Empty string or Japanese-only text returns null. */
export function foreignCharNotice(...parts: string[]): string | null {
  const chars = foreignChars(...parts);
  if (!chars.length) {
    return null;
  }
  return `日本語で用いない文字: ${formatChars(chars)}`;
}

type Json = null | boolean | number | string | Json[] | { [key: string]: Json };

function parseJson(raw: string): Json | null {
  try {
    return JSON.parse(raw) as Json;
  } catch {
    return null;
  }
}

function asObject(value: Json | undefined): { [key: string]: Json } | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  return value;
}

function objectsOf(value: Json | undefined): { [key: string]: Json }[] {
  if (Array.isArray(value)) {
    return value.flatMap((item) => {
      const obj = asObject(item);
      return obj ? [obj] : [];
    });
  }
  const obj = asObject(value);
  if (obj) return [obj];
  if (typeof value === "string") {
    const parsed = parseJson(value);
    return objectsOf(parsed ?? undefined);
  }
  return [];
}

function pushString(out: string[], value: Json | undefined) {
  if (typeof value === "string" && value) out.push(value);
}

function pushRows(out: string[], rows: Json | undefined) {
  if (!Array.isArray(rows)) return;
  for (const row of rows) {
    if (Array.isArray(row)) {
      for (const cell of row) pushString(out, cell);
    } else {
      pushString(out, row);
    }
  }
}

/** Text the model is writing, not a string used to find existing text. */
export function authoredToolTexts(name: string, args: Json): string[] {
  const obj = asObject(args) ?? {};
  const out: string[] = [];
  if (name === "edit_office") {
    for (const item of objectsOf(obj.replacements)) {
      pushString(out, item.replace);
      pushString(out, item.comment);
    }
    for (const item of objectsOf(obj.paragraphs)) {
      pushString(out, item.text);
    }
    return out;
  }
  if (name === "create_docx") {
    for (const block of objectsOf(obj.blocks)) {
      pushString(out, block.text);
      if (Array.isArray(block.items)) {
        for (const item of block.items) pushString(out, item);
      }
      pushRows(out, block.rows);
    }
    return out;
  }
  if (name === "create_xlsx") {
    for (const sheet of objectsOf(obj.sheets)) {
      pushString(out, sheet.name);
      pushRows(out, sheet.rows);
    }
    return out;
  }
  if (name === "create_html" || name === "edit_html") {
    pushString(out, obj.html);
    return out;
  }
  if (name === "create_note" || name === "write_note") {
    pushString(out, obj.title);
    pushString(out, obj.memo);
    return out;
  }
  return out;
}

/** Reply text, plus model-authored arguments stored on the assistant message. */
export function foreignCharNoticeForAssistant(
  content: string,
  toolCallsJson = "",
): string | null {
  const parts = [content];
  const parsed = toolCallsJson.trim() ? parseJson(toolCallsJson) : null;
  if (Array.isArray(parsed)) {
    for (const item of parsed) {
      const call = asObject(item);
      const fn = asObject(call?.function);
      const name = typeof fn?.name === "string" ? fn.name : "";
      const raw = typeof fn?.arguments === "string" ? fn.arguments : "";
      const args = raw ? parseJson(raw) : null;
      if (name && args) parts.push(...authoredToolTexts(name, args));
    }
  }
  return foreignCharNotice(...parts);
}
