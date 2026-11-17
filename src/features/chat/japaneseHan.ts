import { HAN_BMP_A, HAN_BMP_A0, HAN_BMP_B, HAN_BMP_B0, HAN_EXTRA } from "./japaneseHanData";
import { HAN_JP_PAIRS } from "./japaneseHanMap";

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

function loadHanMap(): Map<number, string> {
  const bytes = decodeBase64(HAN_JP_PAIRS.replace(/\s+/g, ""));
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const map = new Map<number, string>();
  for (let i = 0; i + 8 <= bytes.length; i += 8) {
    map.set(view.getUint32(i, true), String.fromCodePoint(view.getUint32(i + 4, true)));
  }
  return map;
}

const hanMap = loadHanMap();

/** One-to-one substitutions. Kana, Latin, and Greek are not in the table. */
export function applyHanMap(text: string): string {
  let out = "";
  for (const ch of text) {
    const cp = ch.codePointAt(0);
    const to = cp === undefined ? undefined : hanMap.get(cp);
    if (cp !== undefined && to) {
      const dst = to.codePointAt(0);
      if (
        dst !== undefined &&
        isCjkIdeograph(cp) &&
        !isJapaneseHan(cp) &&
        isJapaneseHan(dst)
      ) {
        out += to;
        continue;
      }
    }
    out += ch;
  }
  return out;
}

export type HanFix = { at: number; from: string; to: string };

export type ScriptFix = {
  fixes: HanFix[];
  rewritten: boolean;
  /** The user stopped the rewrite. The step-1 text is what is stored. */
  rewriteStopped: boolean;
  /**
   * The user asked for a translation or for Chinese/Korean. The orange warning
   * is hidden. Quotes and code are not corrected, but they still warn.
   */
  suppressWarning: boolean;
};

export function parseScriptFix(raw: string | undefined | null): ScriptFix | null {
  const text = (raw ?? "").trim();
  if (!text) return null;
  try {
    const parsed = JSON.parse(text) as {
      fixes?: unknown;
      rewritten?: unknown;
      rewriteStopped?: unknown;
      suppressWarning?: unknown;
    };
    const fixes: HanFix[] = [];
    if (Array.isArray(parsed.fixes)) {
      for (const item of parsed.fixes) {
        if (!item || typeof item !== "object") continue;
        const row = item as { at?: unknown; from?: unknown; to?: unknown };
        if (
          typeof row.at !== "number" ||
          typeof row.from !== "string" ||
          typeof row.to !== "string"
        ) {
          continue;
        }
        fixes.push({ at: row.at, from: row.from, to: row.to });
      }
    }
    return {
      fixes,
      rewritten: parsed.rewritten === true,
      rewriteStopped: parsed.rewriteStopped === true,
      suppressWarning: parsed.suppressWarning === true,
    };
  } catch {
    return null;
  }
}

/** Language names. A bare mention does not skip correction. Same list as Rust. */
export const FOREIGN_SCRIPT_KEYWORDS = [
  "中国語",
  "韓国語",
  "朝鮮語",
  "中文",
  "한국어",
  "簡体字",
  "繁体字",
  "ハングル",
  "chinese",
  "korean",
] as const;

/** Generic translation words. They skip only when the reply is mostly foreign. */
export const GENERIC_TRANSLATION_KEYWORDS = ["訳して", "翻訳", "translate", "和訳", "英訳"] as const;

/** Same numbers as the Rust constants. */
export const MOSTLY_FOREIGN_MIN_LETTERS = 24;
export const MOSTLY_FOREIGN_MIN_FOREIGN = 6;
export const MOSTLY_FOREIGN_MAX_KANA = 2;

/** How many recent user turns are checked. Same as the Rust constant. */
export const RECENT_USER_TURNS = 4;

function containsKeyword(prompt: string, words: readonly string[]): boolean {
  const lower = prompt.toLowerCase();
  return words.some((word) => lower.includes(word.toLowerCase()));
}

/**
 * The user asked for the reply itself to be in that script.
 * 「中国語に訳して」「中国語で書いて」「中文に翻訳」「韓国語訳」「in Korean」
 * 「into Chinese」 count. 「中国語の契約書」「中文版」「Chinese wall」 do not.
 */
export function namesForeignScript(prompt: string): boolean {
  const lower = prompt.toLowerCase();
  for (const lang of FOREIGN_SCRIPT_KEYWORDS) {
    if (lang === "chinese" || lang === "korean") {
      if (englishTarget(lower, lang)) return true;
    } else if (japaneseTarget(lower, lang)) {
      return true;
    }
  }
  return false;
}

function englishTarget(lower: string, lang: string): boolean {
  for (const frame of [`into ${lang}`, `in ${lang}`, `to ${lang}`]) {
    let from = 0;
    while (from <= lower.length) {
      const idx = lower.indexOf(frame, from);
      if (idx < 0) break;
      const prev = idx === 0 ? "" : lower[idx - 1];
      if (!prev || !/[0-9A-Za-z]/.test(prev)) return true;
      from = idx + 1;
    }
  }
  return false;
}

function japaneseTarget(text: string, lang: string): boolean {
  let rest = text;
  while (rest.length > 0) {
    const idx = rest.indexOf(lang);
    if (idx < 0) return false;
    const after = rest.slice(idx + lang.length).trimStart();
    if (japaneseTargetSuffix(after)) return true;
    rest = rest.slice(idx + lang.length);
  }
  return false;
}

function japaneseTargetSuffix(after: string): boolean {
  return (
    after.startsWith("に訳") ||
    after.startsWith("に翻訳") ||
    after.startsWith("に直") ||
    after.startsWith("で書いて") ||
    after.startsWith("で書け") ||
    after.startsWith("で答えて") ||
    bareTranslationNoun(after) ||
    bareDe(after)
  );
}

function bareTranslationNoun(after: string): boolean {
  if (!after.startsWith("訳")) return false;
  const rest = after.slice(1);
  if (rest.startsWith("の")) return false;
  return rest.length === 0 || "をはがに、。！？\n ".includes(rest[0]) || rest.startsWith("して");
}

function bareDe(after: string): boolean {
  if (!after.startsWith("で")) return false;
  const rest = after.slice(1).trimStart();
  return rest.length === 0 || "。、！？\n".includes(rest[0]);
}

export function asksGenericTranslation(prompt: string): boolean {
  return containsKeyword(prompt, GENERIC_TRANSLATION_KEYWORDS);
}

export function recentUserText(messages: { role: string; content: string }[]): string {
  const users = messages.filter((message) => message.role === "user");
  return users
    .slice(-RECENT_USER_TURNS)
    .map((message) => message.content)
    .join("\n");
}

function isKana(ch: string): boolean {
  const cp = ch.codePointAt(0) ?? 0;
  // U+30FB ・ and U+30FC ー are not kana. A line of only those is not Japanese.
  if (cp === 0x30fb || cp === 0x30fc) return false;
  return (
    (cp >= 0x3040 && cp <= 0x309f) ||
    (cp >= 0x30a0 && cp <= 0x30ff) ||
    (cp >= 0x31f0 && cp <= 0x31ff) ||
    (cp >= 0xff66 && cp <= 0xff9d)
  );
}

/**
 * Same threshold as the Rust check. A long reply with almost no kana and a
 * real share of non-Japanese characters is left alone, warning included.
 */
export function replyIsMostlyForeign(text: string): boolean {
  let kana = 0;
  let foreign = 0;
  let letters = 0;
  for (const ch of text) {
    const cp = ch.codePointAt(0) ?? 0;
    if (isKana(ch)) {
      letters += 1;
      kana += 1;
    } else if (isForeignScript(cp) || (isCjkIdeograph(cp) && !isJapaneseHan(cp))) {
      letters += 1;
      foreign += 1;
    } else if (isCjkIdeograph(cp) || /\p{L}/u.test(ch)) {
      letters += 1;
    }
  }
  if (letters < MOSTLY_FOREIGN_MIN_LETTERS || foreign < MOSTLY_FOREIGN_MIN_FOREIGN) return false;
  if (kana > MOSTLY_FOREIGN_MAX_KANA || kana * 20 > letters) return false;
  return foreign * 6 >= letters;
}

/** More kana than the "almost none" bar. A normal Japanese sentence qualifies. */
export function hasEnoughKana(text: string): boolean {
  let kana = 0;
  let letters = 0;
  for (const ch of text) {
    const cp = ch.codePointAt(0) ?? 0;
    if (isKana(ch)) {
      letters += 1;
      kana += 1;
    } else if (isForeignScript(cp) || isCjkIdeograph(cp) || /\p{L}/u.test(ch)) {
      letters += 1;
    }
  }
  if (kana === 0) return false;
  return kana > MOSTLY_FOREIGN_MAX_KANA || kana * 20 > letters;
}

/**
 * Pieces kept for the orange warning after a named-script reply is left as-is.
 * A markdown table row is split on `|` and each cell is judged alone.
 */
export function foreignNoticeText(prompt: string, text: string): string {
  if (!namesForeignScript(prompt)) return text;
  const kept: string[] = [];
  for (const line of text.split("\n")) {
    if ((line.match(/\|/g) ?? []).length >= 2) {
      for (const cell of line.split("|")) {
        if ([...cell].some((ch) => isKana(ch))) kept.push(cell);
      }
    } else if ([...line].some((ch) => isKana(ch))) {
      kept.push(line);
    }
  }
  return kept.join("\n");
}

/**
 * A named script skips the whole reply. Generic translation words skip only a
 * mostly-foreign reply. Enough kana keeps the correction otherwise.
 */
export function shouldSkipForeignFix(prompt: string, text: string): boolean {
  if (namesForeignScript(prompt)) return true;
  if (hasEnoughKana(text)) return false;
  return asksGenericTranslation(prompt) && replyIsMostlyForeign(text);
}

export function recentShouldSkipForeignFix(
  messages: { role: string; content: string }[],
  text: string,
): boolean {
  return shouldSkipForeignFix(recentUserText(messages), text);
}

/** 「现→現 など3か所を直しました」 */
export function hanMapNote(fixes: HanFix[]): string | null {
  const first = fixes[0];
  if (!first) return null;
  return `${first.from}→${first.to} など${fixes.length}か所を直しました`;
}

export const HAN_REWRITE_NOTE = "AIが外国文字を含む部分を書き直しました";

export const HAN_REWRITE_STOPPED_NOTE = "書き直しを中止しました";

const HAN_MARK_L = "\uE000";
const HAN_MARK_R = "\uE001";

function startsAt(chars: string[], index: number, needle: string): boolean {
  for (let i = 0; i < needle.length; i += 1) {
    if (chars[index + i] !== needle[i]) return false;
  }
  return true;
}

function urlIndexes(text: string): Set<number> {
  const blocked = new Set<number>();
  const chars = [...text];
  let i = 0;
  while (i < chars.length) {
    const https = startsAt(chars, i, "https://");
    const http = !https && startsAt(chars, i, "http://");
    if (!https && !http) {
      i += 1;
      continue;
    }
    const start = i;
    i += https ? "https://".length : "http://".length;
    while (i < chars.length && !/\s/.test(chars[i] ?? "")) i += 1;
    for (let at = start; at < i; at += 1) blocked.add(at);
  }
  return blocked;
}

/** Wrap substituted characters so the markdown renderer can underline them. */
export function markHanFixes(text: string, fixes: HanFix[] | undefined): string {
  if (!fixes?.length) return text;
  const blocked = urlIndexes(text);
  const ats = new Set(fixes.map((fix) => fix.at).filter((at) => !blocked.has(at)));
  let out = "";
  let index = 0;
  for (const ch of text) {
    out += ats.has(index) ? `${HAN_MARK_L}${ch}${HAN_MARK_R}` : ch;
    index += 1;
  }
  return out;
}

export function stripHanMarks(text: string): string {
  return text.replace(/\uE000([^\uE001]*)\uE001/g, "$1");
}

/** Underline markers in text nodes only. Markers inside a tag or attribute are removed. */
export function hanMarkedHtml(html: string): string {
  let out = "";
  let index = 0;
  while (index < html.length) {
    if (html[index] === "<") {
      const end = html.indexOf(">", index);
      const tag = end < 0 ? html.slice(index) : html.slice(index, end + 1);
      out += tag.replace(/\uE000([^\uE001]*)\uE001/g, "$1");
      index = end < 0 ? html.length : end + 1;
      continue;
    }
    const next = html.indexOf("<", index);
    const chunk = next < 0 ? html.slice(index) : html.slice(index, next);
    out += chunk.replace(/\uE000([^\uE001]*)\uE001/g, '<span class="chat-han-fix">$1</span>');
    index = next < 0 ? html.length : next;
  }
  return out;
}
