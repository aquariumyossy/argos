import type { SearchHit } from "../preview/types";
import {
  fileBaseName,
  formatMailDateYmd,
  formatMailFolderMeta,
  isOutlookHit,
  scopeChipLabel,
} from "../preview/hitMeta";

/** Overlay footer while history / term suggestions are open. */
export const SUGGEST_OVERLAY_HINT = "↑↓ で選択 · Enter で閉じる · Tab で確定";

/** Faint cue inside the query input. Suggestions open with Down, not while typing. */
export const INPUT_SUGGEST_HINT = "↓で履歴候補";

const CRUMB_SEP = " › ";

const EXT_TYPE: Record<string, string> = {
  pdf: "PDF",
  doc: "Word",
  docx: "Word",
  docm: "Word",
  xls: "Excel",
  xlsx: "Excel",
  xlsm: "Excel",
  ppt: "PowerPoint",
  pptx: "PowerPoint",
  txt: "テキスト",
  md: "Markdown",
  markdown: "Markdown",
  html: "HTML",
  htm: "HTML",
  json: "JSON",
  jtd: "JTD",
  eml: "メール",
  msg: "メール",
};

/** Virtual keys such as `outlook:0000…` are never shown as a location line. */
export function isInternalIdPath(path: string): boolean {
  const t = path.trim();
  return /^outlook:/i.test(t) || /^mailfolder:/i.test(t);
}

export function simplifyWindowsPath(path: string): string {
  if (isInternalIdPath(path)) return path.trim();
  let out = path.replace(/\//g, "\\");
  if (out.startsWith("\\\\?\\UNC\\")) {
    out = `\\\\${out.slice("\\\\?\\UNC\\".length)}`;
  } else if (out.startsWith("\\\\?\\")) {
    out = out.slice("\\\\?\\".length);
  }
  if (out.startsWith("\\\\")) {
    out = `\\\\${out.slice(2).replace(/\\\\+/g, "\\")}`;
  } else {
    out = out.replace(/\\\\+/g, "\\");
  }
  if (out.length > 3) out = out.replace(/\\+$/g, "");
  return out;
}

export function pathStartsWith(path: string, prefix: string): boolean {
  if (isInternalIdPath(path) || isInternalIdPath(prefix)) {
    return path.trim().toLowerCase() === prefix.trim().toLowerCase();
  }
  const p = simplifyWindowsPath(path);
  const pre = simplifyWindowsPath(prefix);
  if (!pre) return true;
  if (p.length < pre.length) return false;
  if (p.slice(0, pre.length).toLowerCase() !== pre.toLowerCase()) return false;
  return p.length === pre.length || p.charAt(pre.length) === "\\";
}

function pathSegments(path: string): string[] {
  return simplifyWindowsPath(path)
    .split("\\")
    .filter((s) => s.length > 0);
}

/** Indexed root: `publicPath` when set, otherwise the registered folder path. */
export function effectivePublicRoot(folderPath: string, publicPath: string): string {
  const pub = publicPath.trim();
  if (!pub) return simplifyWindowsPath(folderPath);
  return simplifyWindowsPath(pub);
}

/**
 * Short location: registered folder name › next folder (stage) › next folder (case).
 * Deeper directories and the file name are omitted. Full path stays in the tooltip.
 * Without a matching registered root, skip the UNC server/share or drive letter and
 * keep the first three remaining folders.
 */
export function breadcrumbForPath(path: string, roots: string[]): string {
  if (isInternalIdPath(path)) return "";
  const full = simplifyWindowsPath(path);
  const segments = pathSegments(full);
  if (segments.length === 0) return "";
  const dirs = segments.slice(0, -1);

  let bestRoot = "";
  for (const root of roots) {
    if (!root || isInternalIdPath(root)) continue;
    const simplified = simplifyWindowsPath(root);
    if (pathStartsWith(full, simplified) && simplified.length > bestRoot.length) {
      bestRoot = simplified;
    }
  }

  if (bestRoot) {
    const rootSegs = pathSegments(bestRoot);
    const rootName = rootSegs[rootSegs.length - 1] ?? "";
    const childDirs = dirs.slice(rootSegs.length);
    return [rootName, ...childDirs.slice(0, 2)].filter(Boolean).join(CRUMB_SEP);
  }

  let start = 0;
  if (full.startsWith("\\\\")) start = Math.min(2, dirs.length);
  else if (/^[A-Za-z]:$/.test(dirs[0] ?? "")) start = 1;
  return dirs.slice(start, start + 3).join(CRUMB_SEP);
}

export function hitLocation(
  hit: SearchHit,
  roots: string[],
): { text: string; title: string } {
  if (isOutlookHit(hit) || isInternalIdPath(hit.path)) {
    const folder = (hit.mailFolder ?? "").trim()
      ? formatMailFolderMeta(hit.mailFolder ?? "")
      : "";
    const from = (hit.mailFrom ?? "").trim();
    const text = [folder, from].filter((part) => part && !isInternalIdPath(part)).join(" · ");
    const title = folder && !isInternalIdPath(folder) ? folder : "メール";
    return { text: text || "メール", title };
  }
  return {
    text: breadcrumbForPath(hit.path, roots),
    title: simplifyWindowsPath(hit.path),
  };
}

export function extFromPath(path: string): string {
  if (isInternalIdPath(path)) return "";
  const base = path.replace(/\\/g, "/").split("/").pop() ?? "";
  const i = base.lastIndexOf(".");
  if (i <= 0 || i === base.length - 1) return "";
  return base.slice(i + 1).toLowerCase();
}

export function extTypeLabel(ext: string): string {
  const key = ext.toLowerCase();
  if (!key) return "ファイル";
  return EXT_TYPE[key] ?? key.toUpperCase();
}

export function hitTypeLabel(hit: SearchHit): string {
  if (isOutlookHit(hit)) return "メール";
  return extTypeLabel(extFromPath(hit.path));
}

/** Date already carried on the hit. File mtimes are not part of the search payload. */
export function hitDateLabel(hit: SearchHit): string {
  if (!isOutlookHit(hit)) return "";
  return formatMailDateYmd(hit.mailDate);
}

export function selectedHitLabel(hit: SearchHit): string {
  if (isOutlookHit(hit) || isInternalIdPath(hit.path)) {
    return hit.title.trim() || "メール";
  }
  const base = fileBaseName(hit.path);
  if (!base || isInternalIdPath(base)) return hit.title.trim() || "ファイル";
  return base;
}

/** Relative match strength within the current result set (1–5). */
export function scoreLevel(score: number, maxScore: number): number {
  if (maxScore <= 0) return 1;
  const ratio = score / maxScore;
  return Math.min(5, Math.max(1, Math.ceil(ratio * 5)));
}

export function extFilterShortLabel(keys: string[]): string {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const key of keys) {
    const label = EXT_TYPE[key] ?? (key ? key.toUpperCase() : "");
    if (!label || seen.has(label)) continue;
    seen.add(label);
    out.push(label);
  }
  return out.join("・");
}

export function folderFilterLabel(path: string, label: string | null): string {
  if (isInternalIdPath(path)) {
    const chip = scopeChipLabel(path, label);
    return isInternalIdPath(chip) ? "メール" : chip;
  }
  const raw = (label && label.trim()) || scopeChipLabel(path, label);
  const parts = raw
    .split(/[/\\／]/)
    .map((s) => s.trim())
    .filter(Boolean);
  return parts[parts.length - 1] || raw;
}

export type PopupKeyEvent = {
  key: string;
  ctrlKey: boolean;
  metaKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
  isComposing: boolean;
  keyCode: number;
};

export type PopupKeyContext = {
  imeComposing: boolean;
  selectChip: boolean;
  folderPickerOpen: boolean;
  extPickerOpen: boolean;
  datePickerOpen: boolean;
  suggestOpen: boolean;
  suggestionCount: number;
  /** Query input is the active element. */
  caretInInput: boolean;
  /**
   * True after Up/Down has moved the result highlight while the overlay is closed,
   * or after that overlay was closed with Enter or Esc.
   * Hover does not set this. Reset when the query text changes or a new external
   * search arrives. While this is true, Down moves the result selection.
   */
  resultNavStarted: boolean;
  /** Suggestions in memory were fetched for the current query. */
  suggestionsReady: boolean;
};

export type PopupKeyAction =
  | "ignore"
  | "escape-select-chip"
  | "escape-folder"
  | "escape-ext"
  | "escape-date"
  | "escape-suggest"
  | "escape-popup"
  | "picker-swallow"
  | "suggest-tab"
  | "suggest-down"
  | "suggest-up"
  | "suggest-enter-close"
  | "suggest-enter-swallow"
  | "open-suggest"
  | "result-down"
  | "result-up"
  | "preview"
  | "open-folder"
  | "open-result";

export function isImeKey(event: PopupKeyEvent, imeComposing: boolean): boolean {
  return imeComposing || event.isComposing || event.keyCode === 229;
}

function plainKey(event: PopupKeyEvent): boolean {
  return !event.ctrlKey && !event.metaKey && !event.shiftKey && !event.altKey;
}

/** Same condition as the suggestion overlay in the popup. */
export function isSuggestOverlayVisible(ctx: {
  suggestOpen: boolean;
  suggestionCount: number;
  folderPickerOpen: boolean;
  extPickerOpen: boolean;
  datePickerOpen: boolean;
  selectChip: boolean;
}): boolean {
  return (
    ctx.suggestOpen &&
    ctx.suggestionCount > 0 &&
    !ctx.folderPickerOpen &&
    !ctx.extPickerOpen &&
    !ctx.datePickerOpen &&
    !ctx.selectChip
  );
}

/**
 * Down opens suggestions only when the overlay is closed, the caret is in the
 * query input, result keyboard-navigation has not started, and the key is unmodified.
 * Closing the overlay with Enter or Esc counts as navigation having started, so the
 * next Down moves the result selection until the query changes.
 * Enter closes the overlay only while that overlay is actually visible.
 * Enter during IME composition is ignored (does not close the overlay or open a hit).
 */
export function decidePopupKey(
  event: PopupKeyEvent,
  ctx: PopupKeyContext,
): PopupKeyAction {
  const ime = isImeKey(event, ctx.imeComposing);
  if (
    ime &&
    (event.key === "Enter" ||
      event.key === "ArrowDown" ||
      event.key === "ArrowUp" ||
      event.key === "Tab")
  ) {
    return "ignore";
  }

  if (event.key === "Escape") {
    if (ctx.selectChip) return "escape-select-chip";
    if (ctx.folderPickerOpen) return "escape-folder";
    if (ctx.extPickerOpen) return "escape-ext";
    if (ctx.datePickerOpen) return "escape-date";
    if (ctx.suggestOpen) return "escape-suggest";
    return "escape-popup";
  }

  if (ctx.folderPickerOpen || ctx.extPickerOpen || ctx.datePickerOpen) {
    return "picker-swallow";
  }

  if (event.key === "Enter" && isSuggestOverlayVisible(ctx)) {
    return plainKey(event) ? "suggest-enter-close" : "suggest-enter-swallow";
  }

  if (ctx.suggestOpen && ctx.suggestionCount > 0) {
    if (event.key === "Tab") return "suggest-tab";
    if (event.key === "ArrowDown") return "suggest-down";
    if (event.key === "ArrowUp") return "suggest-up";
  }

  if (
    event.key === "ArrowDown" &&
    plainKey(event) &&
    ctx.caretInInput &&
    !ctx.resultNavStarted &&
    (!ctx.suggestionsReady || ctx.suggestionCount > 0)
  ) {
    return "open-suggest";
  }

  if (event.key === "ArrowDown") return "result-down";
  if (event.key === "ArrowUp") return "result-up";

  if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) return "preview";
  if (event.key === "Enter" && event.shiftKey) return "open-folder";
  if (event.key === "Enter") return "open-result";
  return "ignore";
}

export function popupKeyPreventsDefault(action: PopupKeyAction): boolean {
  return action !== "ignore" && action !== "picker-swallow";
}
