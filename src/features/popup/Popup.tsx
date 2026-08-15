import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  applyPreviewHighlights,
  clearPreviewHighlights,
  collectPreviewHighlightTerms,
  findFormattedContentOffset,
  findJsonHitOffset,
  formatGenericJsonHtml,
  formatJsonForPreview,
  isHtmlPath,
  isJsonPath,
  isMarkdownPath,
  splitProseParagraphs,
} from "./markdownPreview";
import { formatLegalDisplayHtml, formatLegalMdHtml } from "../notes/legalMdFormat";
import { formatExportBody } from "../notes/exportNoteText";
import ChatDestPicker, { attachToChat } from "../chat/ChatDestPicker";
import NoteDestPicker, { keepToNote } from "../notes/NoteDestPicker";
import { highlightText } from "../search/highlightText";
import {
  dictionaryWordFromSelection,
  selectionIsQuoted,
  toggleAdjacentQuotes,
} from "./queryEdit";
import "./popup.css";

function HitActionIcon({ children }: { children: ReactNode }) {
  return (
    <svg
      className="hit-action-icon"
      viewBox="0 0 16 16"
      width="14"
      height="14"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      {children}
    </svg>
  );
}

function IconOpenFile() {
  return (
    <HitActionIcon>
      <path d="M9 2.5h4.5V7" />
      <path d="M13.5 2.5 7 9" />
      <path d="M7.5 3.5H3.75A1.25 1.25 0 0 0 2.5 4.75v7.5A1.25 1.25 0 0 0 3.75 13.5h7.5a1.25 1.25 0 0 0 1.25-1.25V8.5" />
    </HitActionIcon>
  );
}

function IconPreview() {
  return (
    <HitActionIcon>
      <path d="M1.75 8s2.25-4 6.25-4 6.25 4 6.25 4-2.25 4-6.25 4-6.25-4-6.25-4Z" />
      <circle cx="8" cy="8" r="1.75" />
    </HitActionIcon>
  );
}

function IconRescope() {
  return (
    <HitActionIcon>
      <circle cx="7" cy="7" r="4" />
      <path d="m13 13-2.5-2.5" />
    </HitActionIcon>
  );
}

function IconFolder() {
  return (
    <HitActionIcon>
      <path d="M2.5 5.25A1.25 1.25 0 0 1 3.75 4h2.3l1.2 1.5h5A1.25 1.25 0 0 1 13.5 6.75v4.5A1.25 1.25 0 0 1 12.25 12.5H3.75A1.25 1.25 0 0 1 2.5 11.25v-6Z" />
    </HitActionIcon>
  );
}

function IconKeep() {
  return (
    <HitActionIcon>
      <path d="M5 2.5h6v6.5l-3 2-3-2V2.5Z" />
      <path d="M8 11v2.5" />
    </HitActionIcon>
  );
}

function IconChat() {
  return (
    <HitActionIcon>
      <path d="M3.25 3.5h9.5A1.25 1.25 0 0 1 14 4.75v5.25A1.25 1.25 0 0 1 12.75 11.25H8.1L4.75 13.5v-2.25H3.25A1.25 1.25 0 0 1 2 10V4.75A1.25 1.25 0 0 1 3.25 3.5Z" />
    </HitActionIcon>
  );
}

function IconNotes() {
  return (
    <HitActionIcon>
      <path d="M3.5 3.25h9A1.25 1.25 0 0 1 13.75 4.5v7A1.25 1.25 0 0 1 12.5 12.75h-9A1.25 1.25 0 0 1 2.25 11.5v-7A1.25 1.25 0 0 1 3.5 3.25Z" />
      <path d="M5.25 6.25h5.5" />
      <path d="M5.25 8.75h4" />
    </HitActionIcon>
  );
}

function IconSettings() {
  return (
    <svg
      className="hit-action-icon"
      viewBox="0 0 24 24"
      width="14"
      height="14"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d="M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6Z" />
      <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1Z" />
    </svg>
  );
}

function IconList() {
  return (
    <HitActionIcon>
      <path d="M5.5 4h8" />
      <path d="M5.5 8h8" />
      <path d="M5.5 12h8" />
      <circle cx="3.25" cy="4" r="0.75" fill="currentColor" stroke="none" />
      <circle cx="3.25" cy="8" r="0.75" fill="currentColor" stroke="none" />
      <circle cx="3.25" cy="12" r="0.75" fill="currentColor" stroke="none" />
    </HitActionIcon>
  );
}

function OverlayCloseBtn({ onClose }: { onClose: () => void }) {
  return (
    <button
      type="button"
      className="popup-overlay-close"
      title="閉じる (Esc)"
      aria-label="閉じる"
      onMouseDown={(e) => {
        e.preventDefault();
        e.stopPropagation();
      }}
      onClick={(e) => {
        e.stopPropagation();
        onClose();
      }}
    >
      ×
    </button>
  );
}

function HitUnitCard({
  label,
  snippet,
  query,
  highlightTerms,
  previewTitle,
  onPreview,
  onKeep,
  onChat,
}: {
  label: string;
  snippet: string;
  query: string;
  highlightTerms?: string[];
  previewTitle: string;
  onPreview: () => void;
  onKeep: (id: "new" | string) => void;
  onChat: (id: "new" | string) => void;
}) {
  return (
    <li className="hit-paragraph">
      <button
        type="button"
        className="hit-paragraph-btn"
        title={previewTitle}
        onClick={(e) => {
          e.stopPropagation();
          onPreview();
        }}
      >
        {label ? (
          <span className="hit-paragraph-label">{label}</span>
        ) : null}
        <span className="hit-paragraph-snippet">
          {highlight(snippet, query, highlightTerms)}
        </span>
      </button>
      <NoteDestPicker
        buttonClassName="hit-paragraph-keep"
        title="ノートにキープ"
        ariaLabel="ノートにキープ"
        onPick={onKeep}
      >
        <IconKeep />
      </NoteDestPicker>
      <ChatDestPicker
        buttonClassName="hit-paragraph-keep"
        title="チャットに送る"
        ariaLabel="チャットに送る"
        onPick={onChat}
      >
        <IconChat />
      </ChatDestPicker>
    </li>
  );
}

/** Document with folded corner — file-type filter toolbar button. */
function IconFileType() {
  return (
    <HitActionIcon>
      <path d="M4.25 2.5h5.25L12.5 5.5v7.25A1.25 1.25 0 0 1 11.25 14h-7A1.25 1.25 0 0 1 3 12.75v-9A1.25 1.25 0 0 1 4.25 2.5Z" />
      <path d="M9.5 2.5V5h2.75" />
      <path d="M5.5 8h5" />
      <path d="M5.5 10.5h5" />
    </HitActionIcon>
  );
}

/** 16×16 paw-print pip for match score. */
function IconScoreDog({ filled }: { filled: boolean }) {
  return (
    <svg
      className={filled ? "hit-score-dog filled" : "hit-score-dog"}
      viewBox="0 0 16 16"
      width="16"
      height="16"
      aria-hidden="true"
    >
      {/* toe pads */}
      <ellipse
        cx="3.15"
        cy="5.15"
        rx="1.5"
        ry="2"
        transform="rotate(-30 3.15 5.15)"
      />
      <ellipse cx="6.1" cy="3.1" rx="1.4" ry="1.9" />
      <ellipse cx="9.9" cy="3.1" rx="1.4" ry="1.9" />
      <ellipse
        cx="12.85"
        cy="5.15"
        rx="1.5"
        ry="2"
        transform="rotate(30 12.85 5.15)"
      />
      {/* main pad: rounded △, point toward toes, wide base below */}
      <path d="M8 7.15c.85 0 2.1.75 3.25 2.05 1.9 1.7 2.3 3.95 1.55 5.15-.7.55-2.15.95-3.4.25-.4-.22-.7-.6-1.4-.6s-1 .38-1.4.6c-1.25.7-2.7.3-3.4-.25C2.45 13.15 2.85 10.9 4.75 9.2 5.9 7.9 7.15 7.15 8 7.15Z" />
    </svg>
  );
}

type ResizeDirection =
  | "East"
  | "North"
  | "NorthEast"
  | "NorthWest"
  | "South"
  | "SouthEast"
  | "SouthWest"
  | "West";

export type ParagraphHit = {
  id: string;
  label: string;
  snippet: string;
  score: number;
  page?: number | null;
};

export type SearchHit = {
  id: string;
  title: string;
  snippet: string;
  path: string;
  page?: number | null;
  chunkId?: number | null;
  score: number;
  source: string;
  previewText: string;
  highlightTerms?: string[];
  matchCount?: number;
  paragraphs?: ParagraphHit[];
  unitLabel?: string;
  mailFrom?: string;
  mailDate?: string;
  mailConversationId?: string;
  mailFolder?: string;
  docKind?: string;
};

type SearchPayload = {
  query: string;
  hits: SearchHit[];
  searching?: boolean;
};

type SearchWordRow = { id: number; word: string; reading?: string; posLabel?: string };

type SearchTermSuggestion = {
  term: string;
  displayPrefix: string;
  displayRest: string;
  kind: string;
  score: number;
  fromHistory?: boolean;
  fromRegistered?: boolean;
};

type SearchScopeRow = { path: string; label: string; isRoot: boolean };

type SearchScopesResult = { recent: SearchScopeRow[]; scopes: SearchScopeRow[] };

type FileTypeOption = { id: string; label: string; exts: string[] };

const FILE_TYPE_OPTIONS: FileTypeOption[] = [
  { id: "pdf", label: "PDF", exts: ["pdf"] },
  { id: "docx", label: "Word (docx)", exts: ["docx"] },
  { id: "doc", label: "Word (doc)", exts: ["doc"] },
  { id: "xlsx", label: "Excel (xlsx)", exts: ["xlsx"] },
  { id: "xls", label: "Excel (xls)", exts: ["xls"] },
  { id: "txt", label: "テキスト", exts: ["txt"] },
  { id: "md", label: "Markdown", exts: ["md", "markdown"] },
  { id: "html", label: "HTML", exts: ["html", "htm"] },
  { id: "json", label: "JSON", exts: ["json"] },
  { id: "jtd", label: "JTD", exts: ["jtd"] },
];

function extsFromFilterKeys(keys: string[]): string[] | null {
  const out: string[] = [];
  for (const key of keys) {
    const opt = FILE_TYPE_OPTIONS.find((o) => o.id === key);
    if (!opt) continue;
    for (const e of opt.exts) {
      if (!out.includes(e)) out.push(e);
    }
  }
  return out.length ? out : null;
}

function extFilterChipLabel(keys: string[]): string {
  return keys
    .map((id) => FILE_TYPE_OPTIONS.find((o) => o.id === id)?.label)
    .filter(Boolean)
    .join(" · ");
}

const SEARCH_DEBOUNCE_MS = 450;
const HINT_HOVER_MS = 500;
const KEEP_TOAST_MS = 2200;

type PreviewFileResult = {
  units: SearchHit[];
  excerpt: boolean;
  matchIds: string[];
};

function previewNavIds(file: PreviewFileResult | null): string[] {
  if (!file) return [];
  const present = new Set(file.units.map((u) => u.id));
  const ids = file.matchIds.filter((id) => present.has(id));
  if (ids.length) return ids;
  return file.units[0] ? [file.units[0].id] : [];
}

/** Parent directory of a Windows / UNC file path. Not for outlook: virtual paths. */
function parentDir(path: string): string | null {
  if (path.startsWith("outlook:") || path.startsWith("mailfolder:")) {
    return null;
  }
  const normalized = path.replace(/\//g, "\\").replace(/\\+$/, "");
  const i = normalized.lastIndexOf("\\");
  if (i <= 0) return null;
  const parent = normalized.slice(0, i);
  if (/^[A-Za-z]:$/.test(parent)) return `${parent}\\`;
  if (!parent || parent === "\\") return null;
  return parent;
}

const SCOPE_CHIP_LABEL_MAX = 36;

/** Shorten long chip labels for display; full value stays in title/tooltip. */
function truncateChipLabel(label: string, max = SCOPE_CHIP_LABEL_MAX): string {
  const chars = Array.from(label);
  if (chars.length <= max) return label;
  if (max <= 1) return "…";
  const head = Math.ceil((max - 1) * 0.55);
  const tail = Math.max(1, max - 1 - head);
  return `${chars.slice(0, head).join("")}…${chars.slice(-tail).join("")}`;
}

/** Split Outlook `Store / Folder / …` and drop leading repeats of the store name. */
function splitMailPathLabel(pathLabel: string): { store: string; folderParts: string[] } {
  const parts = pathLabel
    .split("/")
    .map((s) => s.trim())
    .filter(Boolean);
  if (parts.length === 0) return { store: "", folderParts: [] };
  const store = parts[0];
  let i = 1;
  while (
    i < parts.length &&
    parts[i].localeCompare(store, undefined, { sensitivity: "accent" }) === 0
  ) {
    i += 1;
  }
  return { store, folderParts: parts.slice(i) };
}

/** Chip text: `メール：Folder／Sub（Store）` — store shown once. */
function formatMailScopeLabel(pathLabel: string): string {
  const { store, folderParts } = splitMailPathLabel(pathLabel);
  if (!store) return "メール";
  if (folderParts.length === 0) return `メール：${store}`;
  return `メール：${folderParts.join("／")}（${store}）`;
}

/** Compact folder meta for hit rows (no メール： prefix). */
function formatMailFolderMeta(pathLabel: string): string {
  const { store, folderParts } = splitMailPathLabel(pathLabel);
  if (!store) return pathLabel.trim();
  if (folderParts.length === 0) return store;
  return `${folderParts.join("／")}（${store}）`;
}

function scopeChipLabel(path: string, label?: string | null): string {
  if (label && label.trim()) return label.trim();
  if (path.startsWith("mailfolder:")) {
    return formatMailScopeLabel(path.slice("mailfolder:".length));
  }
  if (path.startsWith("outlook:")) {
    return "Outlook メール";
  }
  const normalized = path.replace(/\//g, "\\").replace(/\\+$/, "");
  const base = normalized.split("\\").filter(Boolean).pop();
  return base || path;
}

function isOutlookHit(hit: SearchHit): boolean {
  return (
    hit.source === "outlook" ||
    hit.docKind === "email" ||
    hit.path.startsWith("outlook:")
  );
}

function formatMailDateYmd(unixStr?: string): string {
  if (!unixStr) return "";
  const n = Number(unixStr);
  if (!Number.isFinite(n) || n <= 0) return "";
  const d = new Date(n * 1000);
  if (Number.isNaN(d.getTime())) return "";
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, "0");
  const day = String(d.getDate()).padStart(2, "0");
  return `${y}/${m}/${day}`;
}

/** Shared highlighter (Popup + Notes). */
const highlight = highlightText;

/** File extension from a hit path (e.g. "html", "pdf"). */
function extFromPath(path: string): string {
  const base = path.replace(/\\/g, "/").split("/").pop() ?? "";
  const i = base.lastIndexOf(".");
  if (i <= 0 || i === base.length - 1) return "";
  return base.slice(i + 1).toLowerCase();
}

/** Relative match strength within the current result set (1–5). */
function scoreLevel(score: number, maxScore: number): number {
  if (maxScore <= 0) return 1;
  const ratio = score / maxScore;
  return Math.min(5, Math.max(1, Math.ceil(ratio * 5)));
}

function PreviewBody({
  hit,
  query,
  highlightTerms,
}: {
  hit: SearchHit;
  query: string;
  highlightTerms?: string[];
}) {
  const preRef = useRef<HTMLPreElement>(null);
  const jsonHtmlRef = useRef<HTMLDivElement>(null);
  const isMarkdown = isMarkdownPath(hit.path);
  const isHtml = isHtmlPath(hit.path);
  const isJson = isJsonPath(hit.path);
  const [jsonRaw, setJsonRaw] = useState<string | null>(null);
  const [jsonLoading, setJsonLoading] = useState(false);

  const markdownHtml = useMemo(() => {
    if (!isMarkdown) return "";
    return formatLegalMdHtml(hit.previewText);
  }, [hit.previewText, isMarkdown]);
  const proseParagraphs = useMemo(() => {
    if (!isHtml) return [];
    return splitProseParagraphs(hit.previewText);
  }, [hit.previewText, isHtml]);
  const jsonView = useMemo(() => {
    if (!isJson) return null;
    const raw = jsonRaw ?? hit.previewText ?? "";
    if (!raw) return null;
    const legal = formatLegalDisplayHtml(hit.path, raw);
    if (legal) {
      return {
        mode: "html" as const,
        html: legal.html,
        className:
          legal.kind === "court"
            ? "preview-body preview-body--court"
            : "preview-body preview-body--markdown",
      };
    }
    const generic = formatGenericJsonHtml(raw);
    if (generic) {
      return {
        mode: "html" as const,
        html: generic,
        className: "preview-body preview-body--json",
      };
    }
    return {
      mode: "pre" as const,
      text: formatJsonForPreview(raw),
    };
  }, [hit.path, hit.previewText, isJson, jsonRaw]);

  useEffect(() => {
    if (!isJson) {
      setJsonRaw(null);
      setJsonLoading(false);
    }
  }, [isJson]);

  useEffect(() => {
    if (!isJson || hit.source !== "remote") return;
    setJsonRaw(hit.previewText);
    setJsonLoading(false);
  }, [hit.previewText, hit.source, isJson]);

  useEffect(() => {
    if (!isJson || hit.source === "remote") return;
    let cancelled = false;
    setJsonRaw(null);
    setJsonLoading(true);
    const fallback = hit.previewText;
    void invoke<string>("read_text_file", { path: hit.path })
      .then((raw) => {
        if (cancelled) return;
        setJsonRaw(raw);
      })
      .catch(() => {
        if (cancelled) return;
        setJsonRaw(fallback);
      })
      .finally(() => {
        if (!cancelled) setJsonLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [hit.path, hit.source, isJson]);

  useLayoutEffect(() => {
    if (!isJson || jsonLoading || jsonView == null) return;

    const scrollToOffset = () => {
      if (jsonView.mode === "html") {
        const root = jsonHtmlRef.current;
        if (!root) return;
        const haystack = root.textContent ?? "";
        const offset = findFormattedContentOffset(
          haystack,
          hit.previewText ?? "",
          hit.snippet ?? "",
        );
        if (offset >= 0) {
          const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
          let pos = 0;
          while (walker.nextNode()) {
            const node = walker.currentNode as Text;
            const len = node.data.length;
            if (pos + len > offset) {
              node.parentElement?.scrollIntoView({
                block: "center",
                inline: "nearest",
              });
              return;
            }
            pos += len;
          }
        }
        root.querySelector("dd, .preview-json-string, p")?.scrollIntoView({
          block: "center",
          inline: "nearest",
        });
        return;
      }

      const pre = preRef.current;
      if (!pre) return;
      const text = jsonView.text;
      const offset = findJsonHitOffset(
        text,
        hit.previewText ?? "",
        hit.snippet ?? "",
      );
      if (offset >= 0) {
        const walker = document.createTreeWalker(pre, NodeFilter.SHOW_TEXT);
        let pos = 0;
        while (walker.nextNode()) {
          const node = walker.currentNode as Text;
          const len = node.data.length;
          if (pos + len > offset) {
            const target =
              node.parentElement?.closest("mark") ??
              node.parentElement ??
              pre;
            target.scrollIntoView({ block: "center", inline: "nearest" });
            return;
          }
          pos += len;
        }
      }
      pre.querySelector("mark")?.scrollIntoView({
        block: "center",
        inline: "nearest",
      });
    };

    const frame = requestAnimationFrame(scrollToOffset);
    return () => cancelAnimationFrame(frame);
  }, [
    hit.id,
    hit.previewText,
    hit.snippet,
    isJson,
    jsonLoading,
    jsonView,
  ]);

  useLayoutEffect(() => {
    if (!isJson || jsonView?.mode !== "html") return;
    const el = jsonHtmlRef.current;
    if (!el) return;
    applyPreviewHighlights([el], highlightTerms ?? []);
  }, [highlightTerms, isJson, jsonLoading, jsonView]);

  if (isMarkdown) {
    return (
      <div
        className="preview-body preview-body--markdown"
        dangerouslySetInnerHTML={{ __html: markdownHtml }}
      />
    );
  }

  if (isHtml) {
    return (
      <div className="preview-body preview-body--prose">
        {proseParagraphs.map((para, i) => (
          <p key={i}>{highlight(para, query, highlightTerms ?? hit.highlightTerms)}</p>
        ))}
      </div>
    );
  }

  if (isJson) {
    if (jsonLoading && jsonRaw == null) {
      return <pre className="preview-body">読み込み中…</pre>;
    }
    if (jsonView?.mode === "html") {
      return (
        <div
          ref={jsonHtmlRef}
          className={jsonView.className}
          dangerouslySetInnerHTML={{ __html: jsonView.html }}
        />
      );
    }
    const text = jsonView?.mode === "pre" ? jsonView.text : hit.previewText;
    return (
      <pre ref={preRef} className="preview-body">
        {highlight(text, query, highlightTerms ?? hit.highlightTerms)}
      </pre>
    );
  }

  return (
    <pre className="preview-body">
      {highlight(hit.previewText, query, highlightTerms ?? hit.highlightTerms)}
    </pre>
  );
}

export default function Popup() {
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<SearchHit[]>([]);
  const [index, setIndex] = useState(0);
  const [preview, setPreview] = useState<SearchHit | null>(null);
  const [previewFile, setPreviewFile] = useState<PreviewFileResult | null>(
    null,
  );
  const [previewUnitId, setPreviewUnitId] = useState<string | null>(null);
  const [matchNavIndex, setMatchNavIndex] = useState(0);
  const [maximized, setMaximized] = useState(false);
  const [searching, setSearching] = useState(false);
  const [actionError, setActionError] = useState("");
  const [keepNotice, setKeepNotice] = useState("");
  const [folderPickerOpen, setFolderPickerOpen] = useState(false);
  const [extPickerOpen, setExtPickerOpen] = useState(false);
  const [hintOpen, setHintOpen] = useState(false);
  const [selectChip, setSelectChip] = useState<{
    start: number;
    end: number;
  } | null>(null);
  const [searchWords, setSearchWords] = useState<SearchWordRow[]>([]);
  const [suggestions, setSuggestions] = useState<SearchTermSuggestion[]>([]);
  const [suggestIndex, setSuggestIndex] = useState(0);
  const [suggestOpen, setSuggestOpen] = useState(false);
  const [searchScopes, setSearchScopes] = useState<SearchScopeRow[]>([]);
  const [recentScopes, setRecentScopes] = useState<SearchScopeRow[]>([]);
  const [scopeFilter, setScopeFilter] = useState("");
  const [scopePath, setScopePath] = useState<string | null>(null);
  const [scopeLabel, setScopeLabel] = useState<string | null>(null);
  const [extFilterKeys, setExtFilterKeys] = useState<string[]>([]);
  const [expandedParas, setExpandedParas] = useState<
    Record<string, ParagraphHit[]>
  >({});
  const inputRef = useRef<HTMLInputElement>(null);
  const scopeFilterRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLUListElement>(null);
  const previewScrollRef = useRef<HTMLDivElement>(null);
  const queryRowRef = useRef<HTMLDivElement>(null);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const hintTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const keepTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const searchSeq = useRef(0);
  const previewSeq = useRef(0);
  const scopePathRef = useRef<string | null>(null);
  const extFilterRef = useRef<string[]>([]);
  const imeComposingRef = useRef(false);
  const skipSelectChipRef = useRef(false);

  useEffect(() => {
    scopePathRef.current = scopePath;
  }, [scopePath]);

  useEffect(() => {
    extFilterRef.current = extFilterKeys;
  }, [extFilterKeys]);

  useEffect(() => {
    const win = getCurrentWindow();
    let unlisten: (() => void) | undefined;
    void win.isMaximized().then(setMaximized).catch(() => {});
    void win
      .onResized(() => {
        void win.isMaximized().then(setMaximized).catch(() => {});
      })
      .then((fn) => {
        unlisten = fn;
      });
    return () => {
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    listen("search-words-updated", () => {
      void invoke<SearchWordRow[]>("list_search_words")
        .then(setSearchWords)
        .catch(console.error);
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      unlisten?.();
    };
  }, []);

  const closePreview = useCallback(() => {
    previewSeq.current += 1;
    setPreview(null);
    setPreviewFile(null);
    setPreviewUnitId(null);
    setMatchNavIndex(0);
  }, []);

  const runSearch = useCallback(
    async (q: string, pathPrefix?: string | null, exts?: string[] | null) => {
      const seq = ++searchSeq.current;
      const trimmed = q.trim();
      if (!trimmed) {
        if (seq === searchSeq.current) {
          setHits([]);
          setIndex(0);
          setSearching(false);
        }
        return;
      }
      setSearching(true);
      const prefix = pathPrefix !== undefined ? pathPrefix : scopePathRef.current;
      const extList =
        exts !== undefined ? exts : extsFromFilterKeys(extFilterRef.current);
      try {
        const next = await invoke<SearchHit[]>("search_query", {
          query: trimmed,
          pathPrefix: prefix && prefix.trim() ? prefix.trim() : null,
          exts: extList && extList.length ? extList : null,
        });
        if (seq !== searchSeq.current) return;
        setHits(next);
        setIndex(0);
        closePreview();
        setExpandedParas({});
        void invoke("record_search_query", { query: trimmed }).catch(console.error);
      } catch (e) {
        console.error(e);
      } finally {
        if (seq === searchSeq.current) {
          setSearching(false);
        }
      }
    },
    [closePreview],
  );

  const scheduleSearch = useCallback(
    (q: string, pathPrefix?: string | null) => {
      if (imeComposingRef.current) return;
      if (debounceRef.current) clearTimeout(debounceRef.current);
      debounceRef.current = setTimeout(() => {
        void runSearch(q, pathPrefix);
      }, SEARCH_DEBOUNCE_MS);
    },
    [runSearch],
  );

  const clearScope = useCallback(() => {
    setScopePath(null);
    setScopeLabel(null);
    scopePathRef.current = null;
  }, []);

  const clearExtFilter = useCallback(() => {
    setExtFilterKeys([]);
    extFilterRef.current = [];
  }, []);

  const applyExtFilter = useCallback(
    (keys: string[]) => {
      setExtFilterKeys(keys);
      extFilterRef.current = keys;
      if (debounceRef.current) clearTimeout(debounceRef.current);
      void runSearch(query, undefined, extsFromFilterKeys(keys));
      requestAnimationFrame(() => {
        inputRef.current?.focus();
      });
    },
    [query, runSearch],
  );

  const toggleExtFilterKey = useCallback(
    (id: string) => {
      const next = extFilterRef.current.includes(id)
        ? extFilterRef.current.filter((k) => k !== id)
        : [...extFilterRef.current, id];
      applyExtFilter(next);
    },
    [applyExtFilter],
  );

  const applyScope = useCallback(
    (path: string | null, label: string | null) => {
      setScopePath(path);
      setScopeLabel(label);
      scopePathRef.current = path;
      setFolderPickerOpen(false);
      setScopeFilter("");
      if (debounceRef.current) clearTimeout(debounceRef.current);
      if (path && path.trim()) {
        const chip = scopeChipLabel(path, label);
        void invoke("push_recent_search_scope", {
          path,
          label: chip,
        }).catch((e) => console.error(e));
      }
      void runSearch(query, path);
      requestAnimationFrame(() => {
        inputRef.current?.focus();
      });
    },
    [query, runSearch],
  );

  const rescopeToHitFolder = useCallback(
    (hit: SearchHit) => {
      if (isOutlookHit(hit)) {
        const folder = (hit.mailFolder ?? "").trim();
        if (!folder) {
          setActionError(
            "このメールの Outlook フォルダ名が不明なため、フォルダ内検索できません。",
          );
          return;
        }
        applyScope(`mailfolder:${folder}`, formatMailScopeLabel(folder));
        return;
      }
      const dir = parentDir(hit.path);
      if (!dir) return;
      applyScope(dir, scopeChipLabel(dir));
    },
    [applyScope],
  );

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    listen<SearchPayload>("search-results", (event) => {
      if (debounceRef.current) clearTimeout(debounceRef.current);
      searchSeq.current += 1;
      closePreview();
      clearScope();
      clearExtFilter();
      setQuery(event.payload.query);
      setHits(event.payload.hits);
      setIndex(0);
      setActionError("");
      setSearching(Boolean(event.payload.searching));
      setFolderPickerOpen(false);
      setExtPickerOpen(false);
      setSuggestOpen(false);
      setSelectChip(null);
      skipSelectChipRef.current = true;
      // Allow edit immediately after shortcut search
      requestAnimationFrame(() => {
        inputRef.current?.focus();
        inputRef.current?.select();
      });
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      unlisten?.();
      if (debounceRef.current) clearTimeout(debounceRef.current);
    };
  }, [clearScope, clearExtFilter, closePreview]);

  useEffect(() => {
    void invoke<SearchWordRow[]>("list_search_words")
      .then(setSearchWords)
      .catch(console.error);
  }, []);

  useEffect(() => {
    if (!folderPickerOpen && !extPickerOpen && !suggestOpen) {
      return;
    }
    const onPointerDown = (e: MouseEvent) => {
      if (queryRowRef.current?.contains(e.target as Node)) return;
      setFolderPickerOpen(false);
      setExtPickerOpen(false);
      setSuggestOpen(false);
    };
    document.addEventListener("mousedown", onPointerDown);
    return () => document.removeEventListener("mousedown", onPointerDown);
  }, [folderPickerOpen, extPickerOpen, suggestOpen]);

  const showKeepNotice = useCallback((text: string) => {
    setKeepNotice(text);
    if (keepTimerRef.current) clearTimeout(keepTimerRef.current);
    keepTimerRef.current = setTimeout(() => setKeepNotice(""), KEEP_TOAST_MS);
  }, []);

  const hideHint = useCallback(() => {
    if (hintTimerRef.current) {
      clearTimeout(hintTimerRef.current);
      hintTimerRef.current = null;
    }
    setHintOpen(false);
  }, []);

  const scheduleHint = useCallback(() => {
    if (imeComposingRef.current || selectChip) {
      hideHint();
      return;
    }
    if (hintTimerRef.current) clearTimeout(hintTimerRef.current);
    hintTimerRef.current = setTimeout(() => setHintOpen(true), HINT_HOVER_MS);
  }, [hideHint, selectChip]);

  const syncSelectChip = useCallback(() => {
    const input = inputRef.current;
    if (!input || imeComposingRef.current) {
      setSelectChip(null);
      return;
    }
    if (skipSelectChipRef.current) {
      setSelectChip(null);
      return;
    }
    const start = input.selectionStart ?? 0;
    const end = input.selectionEnd ?? 0;
    if (end > start) {
      setSelectChip({ start, end });
      setSuggestOpen(false);
      hideHint();
    } else {
      setSelectChip(null);
    }
  }, [hideHint]);

  const applyQuoteToggle = useCallback(() => {
    if (!selectChip) return;
    const next = toggleAdjacentQuotes(query, selectChip.start, selectChip.end);
    if (next == null) return;
    setQuery(next);
    setSelectChip(null);
    scheduleSearch(next);
    requestAnimationFrame(() => inputRef.current?.focus());
  }, [query, scheduleSearch, selectChip]);

  const registerSelectedWord = useCallback(async () => {
    if (!selectChip) return;
    const word = dictionaryWordFromSelection(
      query,
      selectChip.start,
      selectChip.end,
    );
    if (!word) {
      setActionError("登録する検索語が空です");
      return;
    }
    try {
      await invoke("add_search_word", { word });
      const words = await invoke<SearchWordRow[]>("list_search_words");
      setSearchWords(words);
      setActionError("");
      setSelectChip(null);
      scheduleSearch(query);
    } catch (e) {
      setActionError(String(e));
    }
  }, [query, scheduleSearch, selectChip]);

  const refreshSuggestions = useCallback(async (q: string) => {
    if (imeComposingRef.current) {
      setSuggestOpen(false);
      setSuggestions([]);
      return;
    }
    try {
      const next = await invoke<SearchTermSuggestion[]>("suggest_search_terms", {
        query: q,
      });
      setSuggestions(next);
      setSuggestIndex(0);
      setSuggestOpen(next.length > 0);
    } catch (e) {
      console.error(e);
      setSuggestions([]);
      setSuggestOpen(false);
    }
  }, []);

  const applySuggestion = useCallback(
    (sug: SearchTermSuggestion) => {
      const q = query;
      const delimRe = /[\s\u3000,\uFF0C\u3001]/;
      let next: string;
      if (sug.kind === "prefix") {
        const chars = Array.from(q);
        let end = chars.length;
        while (end > 0 && delimRe.test(chars[end - 1]!)) end -= 1;
        let start = end;
        while (
          start > 0 &&
          !delimRe.test(chars[start - 1]!) &&
          chars[start - 1] !== '"'
        ) {
          start -= 1;
        }
        const tokenRaw = chars.slice(start, end).join("");
        const exclude = tokenRaw.startsWith("-");
        const applied = exclude ? `-${sug.term}` : sug.term;
        next = `${chars.slice(0, start).join("")}${applied}`;
      } else {
        const trimmed = q.trimEnd();
        // If last token is a prefix of the suggestion term, replace it.
        const chars = Array.from(trimmed);
        let end = chars.length;
        let start = end;
        while (
          start > 0 &&
          !delimRe.test(chars[start - 1]!) &&
          chars[start - 1] !== '"'
        ) {
          start -= 1;
        }
        const tokenRaw = chars.slice(start, end).join("").replace(/^-/, "");
        if (tokenRaw && sug.term.startsWith(tokenRaw) && sug.term !== tokenRaw) {
          const exclude = chars.slice(start, end).join("").startsWith("-");
          const applied = exclude ? `-${sug.term}` : sug.term;
          next = `${chars.slice(0, start).join("")}${applied}`;
        } else {
          next = trimmed ? `${trimmed} ${sug.term}` : sug.term;
        }
      }
      setQuery(next);
      setSuggestOpen(false);
      setSuggestions([]);
      scheduleSearch(next);
      requestAnimationFrame(() => {
        inputRef.current?.focus();
      });
    },
    [query, scheduleSearch],
  );

  const openFolderPicker = useCallback(async () => {
    try {
      const trimmed = query.trim();
      const result = await invoke<SearchScopesResult>("list_search_scopes", {
        query: trimmed || null,
      });
      let scopes = result.scopes ?? [];
      try {
        if (trimmed) {
          // With a query: only mail folders that appear in current hits.
          const fromHits = new Set<string>();
          for (const hit of hits) {
            const folder = (hit.mailFolder ?? "").trim();
            if (folder) fromHits.add(folder);
          }
          const mailScopes: SearchScopeRow[] = [...fromHits]
            .sort((a, b) => a.localeCompare(b, "ja"))
            .map((name) => ({
              path: `mailfolder:${name}`,
              label: formatMailScopeLabel(name),
              isRoot: true,
            }));
          scopes = [...scopes, ...mailScopes];
        } else {
          // No query: append selected mail folders after file scopes.
          const mailNames = await invoke<string[]>("mail_list_selected_folder_names");
          const mailScopes: SearchScopeRow[] = (mailNames ?? []).map((name) => ({
            path: `mailfolder:${name}`,
            label: formatMailScopeLabel(name),
            isRoot: true,
          }));
          scopes = [...scopes, ...mailScopes];
        }
      } catch {
        // Outlook mail not configured — ignore
      }
      setRecentScopes(result.recent ?? []);
      setSearchScopes(scopes);
      setExtPickerOpen(false);
      setSuggestOpen(false);
      setScopeFilter("");
      setFolderPickerOpen(true);
      requestAnimationFrame(() => {
        scopeFilterRef.current?.focus();
      });
    } catch (e) {
      console.error(e);
      setActionError(String(e));
    }
  }, [query, hits]);

  const openExtPicker = useCallback(() => {
    setFolderPickerOpen(false);
    setSuggestOpen(false);
    setExtPickerOpen(true);
  }, []);

  const filteredRecentScopes = useMemo(() => {
    const q = scopeFilter.trim().toLowerCase();
    if (!q) return recentScopes;
    return recentScopes.filter(
      (s) =>
        s.label.toLowerCase().includes(q) || s.path.toLowerCase().includes(q),
    );
  }, [recentScopes, scopeFilter]);

  const filteredScopes = useMemo(() => {
    const q = scopeFilter.trim().toLowerCase();
    if (!q) return searchScopes;
    return searchScopes.filter(
      (s) =>
        s.label.toLowerCase().includes(q) || s.path.toLowerCase().includes(q),
    );
  }, [searchScopes, scopeFilter]);

  const openSelected = useCallback(
    async (path?: string) => {
      const target = path ?? hits[index]?.path;
      if (!target) return;
      setActionError("");
      try {
        await invoke("open_hit", { path: target });
        setActionError("");
      } catch (e) {
        setActionError(String(e));
      }
    },
    [hits, index],
  );

  const openFolder = useCallback(
    async (path?: string) => {
      const target = path ?? hits[index]?.path;
      if (!target) return;
      if (target.startsWith("outlook:")) return;
      setActionError("");
      try {
        await invoke("open_containing_folder", { path: target });
        setActionError("");
      } catch (e) {
        setActionError(String(e));
      }
    },
    [hits, index],
  );

  const openSettings = useCallback(async () => {
    try {
      await invoke("show_settings_window");
    } catch (e) {
      setActionError(String(e));
    }
  }, []);

  const openNotes = useCallback(async () => {
    try {
      await invoke("show_notes_window");
      setActionError("");
    } catch (e) {
      setActionError(String(e));
    }
  }, []);

  const openChat = useCallback(async () => {
    try {
      await invoke("show_chat_window");
      setActionError("");
    } catch (e) {
      setActionError(String(e));
    }
  }, []);

  const keepParagraph = useCallback(
    async (
      opts: {
        paragraphId: string;
        label?: string;
        page?: number | null;
        snippet?: string;
        body?: string;
        fileHit: SearchHit;
      },
      noteId: "new" | string,
    ) => {
      const { paragraphId, label, page, snippet, body, fileHit } = opts;
      setActionError("");
      try {
        const result = await keepToNote(
          {
            query: query.trim(),
            body: body ?? null,
            snippet: snippet ?? null,
            path: fileHit.path,
            title: fileHit.title,
            source: fileHit.source,
            docKind: fileHit.docKind ?? "",
            paragraphId,
            label: label ?? fileHit.unitLabel ?? "",
            page: page ?? fileHit.page ?? null,
            mailFrom: fileHit.mailFrom ?? "",
            mailDate: fileHit.mailDate ?? "",
            mailFolder: fileHit.mailFolder ?? "",
            highlightTerms: fileHit.highlightTerms ?? [],
          },
          noteId,
        );
        const dest = result.note?.title?.trim() || "無題のノート";
        if (result.created) {
          showKeepNotice(
            result.createdNote
              ? "新しいノートにキープした"
              : `『${dest}』にキープした`,
          );
        } else {
          showKeepNotice(`『${dest}』にすでにキープ済み`);
        }
      } catch (e) {
        setActionError(String(e));
      }
    },
    [query, showKeepNotice],
  );

  const resolveChatBody = useCallback(
    async (opts: {
      paragraphId: string;
      path: string;
      previewText?: string;
      snippet?: string;
    }): Promise<string> => {
      let body = (opts.previewText ?? "").trim();
      const snippet = (opts.snippet ?? "").trim();
      const looksThin =
        !body || body === snippet || [...body].length < 80;
      if (looksThin && opts.paragraphId) {
        try {
          const hit = await invoke<SearchHit | null>("get_preview", {
            hitId: opts.paragraphId,
          });
          const preview = hit?.previewText?.trim() ?? "";
          if ([...preview].length > [...body].length) {
            body = preview;
          }
        } catch {
          /* keep existing text */
        }
      }
      if (!body) body = snippet;
      return formatExportBody(opts.path, body);
    },
    [],
  );

  const sendHitToChat = useCallback(
    async (
      opts: {
        paragraphId: string;
        path: string;
        title: string;
        previewText?: string;
        snippet?: string;
      },
      threadId: "new" | string,
    ) => {
      setActionError("");
      try {
        const body = await resolveChatBody(opts);
        if (!body.trim()) {
          setActionError(
            "本文が取れませんでした。プレビューを開いてから送ってください。",
          );
          return;
        }
        const result = await attachToChat(
          [
            {
              path: opts.path,
              title: opts.title,
              paragraphId: opts.paragraphId,
              body,
              query: query.trim(),
              origin: "attach",
            },
          ],
          opts.title.trim() || null,
          threadId,
        );
        const dest = result.thread?.title?.trim() || "新しい会話";
        if (result.added > 0) {
          showKeepNotice(
            result.createdThread
              ? "新しいチャットに送った"
              : `『${dest}』に追加した`,
          );
        } else if (result.skipped > 0) {
          showKeepNotice("同じ出典がすでに読込前にあります");
        } else {
          showKeepNotice("本文が空のため送れませんでした");
        }
      } catch (e) {
        setActionError(String(e));
      }
    },
    [query, resolveChatBody, showKeepNotice],
  );

  const showPreview = useCallback(
    async (target?: SearchHit, focusId?: string) => {
      const hit = target ?? hits[index];
      if (!hit) return;
      const seq = ++previewSeq.current;
      setPreview(hit);
      setPreviewFile(null);
      setPreviewUnitId(focusId ?? hit.id);
      setMatchNavIndex(0);
      if (hit.source === "remote") {
        setPreviewFile({
          units: [hit],
          excerpt: true,
          matchIds: [hit.id],
        });
        return;
      }
      try {
        const file = await invoke<PreviewFileResult>("preview_file", {
          query: query.trim(),
          path: hit.path,
        });
        if (seq !== previewSeq.current) return;
        const units = file.units.length ? file.units : [hit];
        const rawIds = file.matchIds.length
          ? file.matchIds
          : focusId
            ? [focusId]
            : [hit.id];
        const present = new Set(units.map((u) => u.id));
        const inUnits = rawIds.filter((id) => present.has(id));
        const matchIds = inUnits.length
          ? inUnits
          : units[0]
            ? [units[0].id]
            : [hit.id];
        setPreviewFile({ ...file, units, matchIds });
        const want = focusId ?? hit.id;
        const found = matchIds.findIndex((id) => id === want);
        setMatchNavIndex(found >= 0 ? found : 0);
        setPreviewUnitId(want);
        const unit = units.find((u) => u.id === want) ?? units[0];
        if (unit) setPreview(unit);
      } catch (e) {
        if (seq !== previewSeq.current) return;
        console.error(e);
        setPreviewFile({
          units: [hit],
          excerpt: false,
          matchIds: [hit.id],
        });
      }
    },
    [hits, index, query],
  );

  const previewParagraph = useCallback(
    async (paraId: string, fileHit: SearchHit) => {
      await showPreview(fileHit, paraId);
    },
    [showPreview],
  );

  const scrollToMatch = useCallback((unitId: string) => {
    const root = previewScrollRef.current;
    if (!root) return;
    const el = Array.from(
      root.querySelectorAll<HTMLElement>("[data-preview-unit]"),
    ).find((node) => node.dataset.previewUnit === unitId);
    el?.scrollIntoView({ block: "center", inline: "nearest" });
  }, []);

  const stepMatch = useCallback(
    (delta: number) => {
      const ids = previewNavIds(previewFile);
      if (ids.length === 0) return;
      setMatchNavIndex((i) => {
        const next = (i + delta + ids.length) % ids.length;
        const id = ids[next];
        if (id) {
          setPreviewUnitId(id);
          const unit = previewFile?.units.find((u) => u.id === id);
          if (unit) setPreview(unit);
          requestAnimationFrame(() => scrollToMatch(id));
        }
        return next;
      });
    },
    [previewFile, scrollToMatch],
  );

  const expandHitParagraphs = useCallback(
    async (hit: SearchHit) => {
      if (expandedParas[hit.path]) return;
      try {
        const matches = await invoke<SearchHit[]>("search_path_matches", {
          query: query.trim(),
          path: hit.path,
          source: hit.source || null,
        });
        if (matches.length === 0) {
          setActionError(
            hit.source === "remote"
              ? "追加の段落を取得できませんでした。ホストを同じバージョンにしてください。"
              : "このファイルの追加段落を取得できませんでした。",
          );
          return;
        }
        const paras: ParagraphHit[] = matches.map((m) => ({
          id: m.id,
          label: m.unitLabel || "段落",
          snippet: m.snippet,
          score: m.score,
          page: m.page,
        }));
        setExpandedParas((prev) => ({ ...prev, [hit.path]: paras }));
        setActionError("");
      } catch (e) {
        setActionError(String(e));
      }
    },
    [expandedParas, query],
  );

  const hidePopup = useCallback(async () => {
    clearScope();
    setFolderPickerOpen(false);
    setExtPickerOpen(false);
    setSuggestOpen(false);
    setSelectChip(null);
    closePreview();
    await invoke("hide_popup");
  }, [clearScope, closePreview]);

  const closeQueryOverlay = useCallback(() => {
    setFolderPickerOpen(false);
    setExtPickerOpen(false);
    setSuggestOpen(false);
    setSelectChip(null);
    requestAnimationFrame(() => inputRef.current?.focus());
  }, []);

  useEffect(() => {
    if (preview) return;
    const active = listRef.current?.querySelector<HTMLElement>(".hit.active");
    active?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }, [index, hits, preview]);

  useEffect(() => {
    if (!preview || !previewFile || !previewUnitId) return;
    requestAnimationFrame(() => scrollToMatch(previewUnitId));
  }, [preview, previewFile, previewUnitId, scrollToMatch]);

  const previewHighlightTerms = useMemo(() => {
    if (!preview) return [];
    const listHit = hits.find((h) => h.path === preview.path);
    const extra = [
      ...(listHit?.highlightTerms ?? []),
      ...(preview.highlightTerms ?? []),
      ...((previewFile?.units ?? []).flatMap((u) => u.highlightTerms ?? [])),
    ];
    return collectPreviewHighlightTerms(query, extra);
  }, [hits, preview, previewFile, query]);

  useLayoutEffect(() => {
    if (!preview) {
      clearPreviewHighlights();
      return;
    }
    const root = previewScrollRef.current;
    if (!root) {
      clearPreviewHighlights();
      return;
    }
    const els = Array.from(
      root.querySelectorAll<HTMLElement>(".preview-body"),
    );
    applyPreviewHighlights(els, previewHighlightTerms);
    return () => clearPreviewHighlights();
  }, [preview, previewFile, previewHighlightTerms]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        if (selectChip) {
          setSelectChip(null);
          return;
        }
        if (folderPickerOpen) {
          setFolderPickerOpen(false);
          return;
        }
        if (extPickerOpen) {
          setExtPickerOpen(false);
          return;
        }
        if (suggestOpen) {
          setSuggestOpen(false);
          return;
        }
        if (preview) {
          closePreview();
          return;
        }
        void hidePopup();
        return;
      }
      if (folderPickerOpen || extPickerOpen) return;

      if (
        suggestOpen &&
        suggestions.length > 0 &&
        !preview &&
        !imeComposingRef.current
      ) {
        if (e.key === "Tab") {
          e.preventDefault();
          const sug = suggestions[suggestIndex] ?? suggestions[0];
          if (sug) applySuggestion(sug);
          return;
        }
        if (e.key === "ArrowDown") {
          e.preventDefault();
          setSuggestIndex((i) => (i + 1) % suggestions.length);
          return;
        }
        if (e.key === "ArrowUp") {
          e.preventDefault();
          setSuggestIndex(
            (i) => (i - 1 + suggestions.length) % suggestions.length,
          );
          return;
        }
        if (e.key === "Enter" && !query.trim()) {
          e.preventDefault();
          const sug = suggestions[suggestIndex] ?? suggestions[0];
          if (sug) applySuggestion(sug);
          return;
        }
      }

      if (preview) {
        if (e.key === "ArrowLeft" || e.key === "[") {
          e.preventDefault();
          stepMatch(-1);
          return;
        }
        if (e.key === "ArrowRight" || e.key === "]") {
          e.preventDefault();
          stepMatch(1);
          return;
        }
        if (e.key === "Enter" && e.shiftKey) {
          e.preventDefault();
          if (!isOutlookHit(preview)) void openFolder();
          return;
        }
        if (e.key === "Enter") {
          e.preventDefault();
          void openSelected();
          return;
        }
        return;
      }
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setIndex((i) => Math.min(i + 1, Math.max(hits.length - 1, 0)));
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setIndex((i) => Math.max(i - 1, 0));
        return;
      }
      if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
        e.preventDefault();
        void showPreview();
        return;
      }
      if (e.key === "Enter" && e.shiftKey) {
        e.preventDefault();
        const hit = hits[index];
        if (!hit || !isOutlookHit(hit)) void openFolder();
        return;
      }
      if (e.key === "Enter") {
        e.preventDefault();
        void openSelected();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [
    applySuggestion,
    closePreview,
    extPickerOpen,
    folderPickerOpen,
    hidePopup,
    hits,
    index,
    openFolder,
    openSelected,
    preview,
    query,
    selectChip,
    showPreview,
    stepMatch,
    suggestIndex,
    suggestOpen,
    suggestions,
  ]);

  const minimizeWindow = useCallback(async () => {
    try {
      await getCurrentWindow().minimize();
    } catch (e) {
      setActionError(String(e));
    }
  }, []);

  const toggleMaximizeWindow = useCallback(async () => {
    try {
      const win = getCurrentWindow();
      await win.toggleMaximize();
      setMaximized(await win.isMaximized());
    } catch (e) {
      setActionError(String(e));
    }
  }, []);

  const startWindowDrag = useCallback(async (e: React.MouseEvent) => {
    if (e.button !== 0) return;
    // Don't start window drag from the query input
    if ((e.target as HTMLElement).closest("input, button, textarea")) {
      return;
    }
    hideHint();
    e.preventDefault();
    try {
      await invoke("set_popup_dragging", { dragging: true });
      await getCurrentWindow().startDragging();
    } catch (err) {
      console.error(err);
    } finally {
      await invoke("set_popup_dragging", { dragging: false });
    }
  }, [hideHint]);

  const startWindowResize = useCallback(
    async (e: React.MouseEvent, direction: ResizeDirection) => {
      if (e.button !== 0) return;
      e.preventDefault();
      e.stopPropagation();
      try {
        await invoke("set_popup_dragging", { dragging: true });
        await getCurrentWindow().startResizeDragging(direction);
      } catch (err) {
        console.error(err);
      } finally {
        await invoke("set_popup_dragging", { dragging: false });
      }
    },
    [],
  );

  const resizeEdges: { dir: ResizeDirection; className: string }[] = [
    { dir: "North", className: "resize-n" },
    { dir: "South", className: "resize-s" },
    { dir: "East", className: "resize-e" },
    { dir: "West", className: "resize-w" },
    { dir: "NorthEast", className: "resize-ne" },
    { dir: "NorthWest", className: "resize-nw" },
    { dir: "SouthEast", className: "resize-se" },
    { dir: "SouthWest", className: "resize-sw" },
  ];

  const currentHit = preview ?? hits[index];
  const currentIsMail = currentHit ? isOutlookHit(currentHit) : false;

  return (
    <div className="popup">
      {resizeEdges.map(({ dir, className }) => (
        <div
          key={dir}
          className={`resize-handle ${className}`}
          onMouseDown={(e) => void startWindowResize(e, dir)}
        />
      ))}
      <header className="popup-header" onMouseDown={(e) => void startWindowDrag(e)}>
        <div
          className="popup-titlebar"
          onDoubleClick={(e) => {
            if ((e.target as HTMLElement).closest("button")) return;
            void toggleMaximizeWindow();
          }}
        >
          <span className="popup-titlebar-label">
            <img
              className="popup-titlebar-icon"
              src="/argos-icon.png"
              alt=""
              width={16}
              height={16}
              draggable={false}
            />
            Argos
          </span>
          <div className="popup-window-controls">
            <button
              type="button"
              className="popup-win-btn"
              title="最小化"
              aria-label="最小化"
              onMouseDown={(e) => e.stopPropagation()}
              onClick={() => void minimizeWindow()}
            >
              <svg viewBox="0 0 10 10" width="10" height="10" aria-hidden="true">
                <path d="M1 5h8" stroke="currentColor" strokeWidth="1.2" />
              </svg>
            </button>
            <button
              type="button"
              className="popup-win-btn"
              title={maximized ? "元のサイズに戻す" : "最大化"}
              aria-label={maximized ? "元のサイズに戻す" : "最大化"}
              onMouseDown={(e) => e.stopPropagation()}
              onClick={() => void toggleMaximizeWindow()}
            >
              {maximized ? (
                <svg viewBox="0 0 10 10" width="10" height="10" aria-hidden="true">
                  <path
                    d="M2.5 3h5v5h-5zm1.5-1.5h5V6.5"
                    fill="none"
                    stroke="currentColor"
                    strokeWidth="1.1"
                  />
                </svg>
              ) : (
                <svg viewBox="0 0 10 10" width="10" height="10" aria-hidden="true">
                  <rect
                    x="1.5"
                    y="1.5"
                    width="7"
                    height="7"
                    fill="none"
                    stroke="currentColor"
                    strokeWidth="1.1"
                  />
                </svg>
              )}
            </button>
            <button
              type="button"
              className="popup-win-btn popup-win-btn-close"
              title="閉じる"
              aria-label="閉じる"
              onMouseDown={(e) => e.stopPropagation()}
              onClick={() => void hidePopup()}
            >
              <svg viewBox="0 0 10 10" width="10" height="10" aria-hidden="true">
                <path
                  d="M2 2l6 6M8 2L2 8"
                  stroke="currentColor"
                  strokeWidth="1.2"
                  strokeLinecap="round"
                />
              </svg>
            </button>
          </div>
        </div>
        <div className="popup-kicker">
          検索
          <span className="drag-hint">
            {keepNotice
              ? keepNotice
              : searching
                ? "検索中…"
                : "入力で再検索 / 余白ドラッグで移動"}
          </span>
        </div>
        <div className="popup-query-row" ref={queryRowRef}>
          <div className="popup-query-input-wrap">
            {hintOpen && !selectChip ? (
              <div className="popup-query-hint" role="note">
                <div>スペースまたは , で区切る · &quot;隣接&quot; · -除外</div>
                <div>語をドラッグ → 隣接にする / 辞書登録</div>
              </div>
            ) : null}
            <input
              ref={inputRef}
              className="popup-query-input"
              value={query}
              placeholder={'検索 "隣接" -除外'}
              spellCheck={false}
              onChange={(e) => {
                const next = e.target.value;
                setQuery(next);
                setSelectChip(null);
                hideHint();
                scheduleSearch(next);
                if (!folderPickerOpen && !extPickerOpen) {
                  void refreshSuggestions(next);
                } else {
                  setSuggestOpen(false);
                }
              }}
              onFocus={() => {
                if (!query.trim() && !folderPickerOpen && !extPickerOpen) {
                  void refreshSuggestions("");
                }
              }}
              onCompositionStart={() => {
                imeComposingRef.current = true;
                setSuggestOpen(false);
                setSelectChip(null);
                hideHint();
                if (debounceRef.current) {
                  clearTimeout(debounceRef.current);
                  debounceRef.current = null;
                }
              }}
              onCompositionEnd={(e) => {
                imeComposingRef.current = false;
                const next = e.currentTarget.value;
                setQuery(next);
                scheduleSearch(next);
                if (!folderPickerOpen && !extPickerOpen) {
                  void refreshSuggestions(next);
                }
              }}
              onMouseEnter={() => scheduleHint()}
              onMouseLeave={() => hideHint()}
              onMouseDown={(e) => {
                e.stopPropagation();
                skipSelectChipRef.current = false;
              }}
              onMouseUp={() => syncSelectChip()}
              onKeyDown={() => {
                hideHint();
                skipSelectChipRef.current = false;
              }}
              onSelect={() => {
                if (document.activeElement === inputRef.current) {
                  syncSelectChip();
                }
              }}
            />
            {selectChip ? (
              <div className="popup-select-chip" role="toolbar" aria-label="選択した語">
                <button
                  type="button"
                  onMouseDown={(e) => e.preventDefault()}
                  onClick={() => applyQuoteToggle()}
                >
                  {selectionIsQuoted(query, selectChip.start, selectChip.end)
                    ? "引用を外す"
                    : "隣接にする"}
                </button>
                {(() => {
                  const word = dictionaryWordFromSelection(
                    query,
                    selectChip.start,
                    selectChip.end,
                  );
                  const registered =
                    !!word && searchWords.some((w) => w.word === word);
                  return (
                    <button
                      type="button"
                      disabled={!word || registered}
                      onMouseDown={(e) => e.preventDefault()}
                      onClick={() => void registerSelectedWord()}
                    >
                      {registered ? "登録済み" : "辞書に登録"}
                    </button>
                  );
                })()}
                <OverlayCloseBtn onClose={closeQueryOverlay} />
              </div>
            ) : null}
          </div>
          <button
            type="button"
            className="popup-scope-btn"
            title="検索対象フォルダを絞る"
            aria-label="検索対象フォルダを絞る"
            aria-expanded={folderPickerOpen}
            onMouseDown={(e) => e.stopPropagation()}
            onClick={() => {
              if (folderPickerOpen) {
                setFolderPickerOpen(false);
              } else {
                void openFolderPicker();
              }
            }}
          >
            @
          </button>
          <button
            type="button"
            className={`popup-ext-btn${extFilterKeys.length ? " is-active" : ""}`}
            title="ファイル種別で絞る"
            aria-label="ファイル種別で絞る"
            aria-expanded={extPickerOpen}
            onMouseDown={(e) => e.stopPropagation()}
            onClick={() => {
              if (extPickerOpen) {
                setExtPickerOpen(false);
              } else {
                openExtPicker();
              }
            }}
          >
            <IconFileType />
          </button>
          {suggestOpen &&
          suggestions.length > 0 &&
          !folderPickerOpen &&
          !extPickerOpen &&
          !selectChip ? (
            <div
              className="popup-suggest"
              role="listbox"
              aria-label="検索候補"
            >
              <div className="popup-overlay-head">
                <span className="popup-overlay-head-label">候補</span>
                <OverlayCloseBtn onClose={closeQueryOverlay} />
              </div>
              <ul>
                {suggestions.map((s, i) => (
                  <li key={`${s.kind}:${s.term}`}>
                    <button
                      type="button"
                      role="option"
                      aria-selected={i === suggestIndex}
                      className={i === suggestIndex ? "is-active" : undefined}
                      onMouseEnter={() => setSuggestIndex(i)}
                      onMouseDown={(e) => e.preventDefault()}
                      onClick={() => applySuggestion(s)}
                    >
                      {s.kind === "prefix" && s.displayPrefix ? (
                        <span className="suggest-term">
                          <span className="suggest-prefix">{s.displayPrefix}</span>
                          <span className="suggest-rest">{s.displayRest}</span>
                        </span>
                      ) : (
                        <span className="suggest-term">{s.term}</span>
                      )}
                      <span className="suggest-badges">
                        {s.fromHistory ? (
                          <span className="word-kind history">履歴</span>
                        ) : null}
                        {s.fromRegistered ? (
                          <span className="word-kind registered">登録</span>
                        ) : null}
                      </span>
                    </button>
                  </li>
                ))}
              </ul>
              <div className="popup-suggest-hint">
                {!query.trim()
                  ? "↑↓ で選択 · Enter で検索 · × または Esc で閉じる"
                  : "↑↓ で選択 · Tab で確定 · × または Esc で閉じる"}
              </div>
            </div>
          ) : null}
          {folderPickerOpen ? (
            <div
              className="popup-folder-picker"
              role="listbox"
              aria-label="検索対象フォルダ"
            >
              <div className="popup-overlay-head">
                <span className="popup-overlay-head-label">フォルダ</span>
                <OverlayCloseBtn onClose={closeQueryOverlay} />
              </div>
              <input
                ref={scopeFilterRef}
                className="popup-folder-filter"
                value={scopeFilter}
                placeholder="フォルダ名で絞り込み…"
                spellCheck={false}
                onMouseDown={(e) => e.stopPropagation()}
                onChange={(e) => setScopeFilter(e.target.value)}
              />
              {filteredScopes.length === 0 && filteredRecentScopes.length === 0 ? (
                <div className="popup-word-empty">
                  {recentScopes.length === 0 && searchScopes.length === 0
                    ? query.trim()
                      ? "この検索語にヒットするフォルダがありません。"
                      : "検索対象フォルダがありません。設定からフォルダを追加してください。"
                    : "一致するフォルダがありません。"}
                </div>
              ) : (
                <ul>
                  {filteredRecentScopes.map((s) => (
                    <li key={`recent:${s.path}`}>
                      <button
                        type="button"
                        role="option"
                        className="scope-recent"
                        title={s.path}
                        onClick={() => applyScope(s.path, s.label)}
                      >
                        <span className="scope-kind">直近</span>
                        <span className="scope-label">{s.label}</span>
                      </button>
                    </li>
                  ))}
                  {filteredRecentScopes.length > 0 && filteredScopes.length > 0 ? (
                    <li className="scope-sep" aria-hidden="true" />
                  ) : null}
                  {filteredScopes.map((s) => (
                    <li key={s.path}>
                      <button
                        type="button"
                        role="option"
                        className={s.isRoot ? "scope-root" : "scope-sub"}
                        title={s.path}
                        onClick={() => applyScope(s.path, s.label)}
                      >
                        {s.isRoot ? (
                          <span className="scope-kind">ルート</span>
                        ) : null}
                        <span className="scope-label">{s.label}</span>
                      </button>
                    </li>
                  ))}
                </ul>
              )}
            </div>
          ) : null}
          {extPickerOpen ? (
            <div
              className="popup-ext-picker"
              role="listbox"
              aria-label="ファイル種別"
              aria-multiselectable="true"
            >
              <div className="popup-overlay-head">
                <span className="popup-overlay-head-label">ファイル種別</span>
                <OverlayCloseBtn onClose={closeQueryOverlay} />
              </div>
              <ul>
                {FILE_TYPE_OPTIONS.map((opt) => {
                  const selected = extFilterKeys.includes(opt.id);
                  return (
                    <li key={opt.id}>
                      <button
                        type="button"
                        role="option"
                        aria-selected={selected}
                        className={selected ? "is-selected" : undefined}
                        onClick={() => toggleExtFilterKey(opt.id)}
                      >
                        <span className="ext-check" aria-hidden="true">
                          {selected ? "✓" : ""}
                        </span>
                        <span className="ext-label">{opt.label}</span>
                      </button>
                    </li>
                  );
                })}
              </ul>
              {extFilterKeys.length > 0 ? (
                <div className="popup-ext-picker-actions">
                  <button
                    type="button"
                    className="popup-ext-clear-btn"
                    onClick={() => applyExtFilter([])}
                  >
                    種別フィルタを解除
                  </button>
                </div>
              ) : null}
            </div>
          ) : null}
        </div>
        {scopePath || extFilterKeys.length > 0 ? (
          <div className="popup-scope-chip-row">
            {scopePath ? (
              <span
                className="popup-scope-chip"
                title={scopeChipLabel(scopePath, scopeLabel)}
              >
                <span className="popup-scope-chip-at">@</span>
                <span className="popup-scope-chip-text">
                  {truncateChipLabel(scopeChipLabel(scopePath, scopeLabel))}
                </span>
                <button
                  type="button"
                  className="popup-scope-chip-clear"
                  title="フォルダ絞り込みを解除"
                  aria-label="フォルダ絞り込みを解除"
                  onMouseDown={(e) => e.stopPropagation()}
                  onClick={() => applyScope(null, null)}
                >
                  ×
                </button>
              </span>
            ) : null}
            {extFilterKeys.length > 0 ? (
              <span
                className="popup-scope-chip popup-ext-chip"
                title={extFilterChipLabel(extFilterKeys)}
              >
                <span className="popup-scope-chip-at">種別</span>
                <span className="popup-scope-chip-text">
                  {truncateChipLabel(extFilterChipLabel(extFilterKeys))}
                </span>
                <button
                  type="button"
                  className="popup-scope-chip-clear"
                  title="種別絞り込みを解除"
                  aria-label="種別絞り込みを解除"
                  onMouseDown={(e) => e.stopPropagation()}
                  onClick={() => applyExtFilter([])}
                >
                  ×
                </button>
              </span>
            ) : null}
          </div>
        ) : null}
      </header>

      {preview ? (
        <section className="preview">
          <div className="preview-title">{preview.title}</div>
          <div
            className="preview-path"
            title={preview.path}
          >
            {isOutlookHit(preview)
              ? [
                  preview.mailFolder
                    ? formatMailFolderMeta(preview.mailFolder)
                    : "",
                  preview.mailFrom,
                  formatMailDateYmd(preview.mailDate),
                ]
                  .filter(Boolean)
                  .join(" · ") || "Outlook メール"
              : preview.path}
          </div>
          <div className="preview-actions">
            <button
              type="button"
              className="hit-action-btn"
              title="一覧に戻る (Esc)"
              aria-label="一覧に戻る"
              onClick={closePreview}
            >
              <IconList />
            </button>
            <button
              type="button"
              className="hit-action-btn"
              title={
                isOutlookHit(preview) ? "メールを開く (Enter)" : "ファイルを開く (Enter)"
              }
              aria-label={isOutlookHit(preview) ? "メールを開く" : "ファイルを開く"}
              onClick={() => void openSelected()}
            >
              <IconOpenFile />
            </button>
            {!isOutlookHit(preview) ? (
              <button
                type="button"
                className="hit-action-btn"
                title="フォルダを開く (Shift+Enter)"
                aria-label="フォルダを開く"
                onClick={() => void openFolder(preview.path)}
              >
                <IconFolder />
              </button>
            ) : null}
            <button
              type="button"
              className="hit-action-btn"
              title="このフォルダ内で再検索"
              aria-label="このフォルダ内で再検索"
              onClick={() => rescopeToHitFolder(preview)}
            >
              <IconRescope />
            </button>
            <NoteDestPicker
              buttonClassName="hit-action-btn"
              title={
                isOutlookHit(preview)
                  ? "このメールをノートにキープ"
                  : "この段落をノートにキープ"
              }
              ariaLabel="ノートにキープ"
              onPick={(id) => {
                const unit =
                  previewFile?.units.find((u) => u.id === previewUnitId) ??
                  preview;
                void keepParagraph(
                  {
                    paragraphId: unit.id,
                    label: unit.unitLabel || "",
                    page: unit.page,
                    snippet: unit.snippet,
                    body: unit.previewText,
                    fileHit: preview,
                  },
                  id,
                );
              }}
            >
              <IconKeep />
            </NoteDestPicker>
            <ChatDestPicker
              buttonClassName="hit-action-btn"
              title={
                isOutlookHit(preview)
                  ? "このメールをチャットに送る"
                  : "この段落をチャットに送る"
              }
              ariaLabel="チャットに送る"
              onPick={(id) => {
                const unit =
                  previewFile?.units.find((u) => u.id === previewUnitId) ??
                  preview;
                void sendHitToChat(
                  {
                    paragraphId: unit.id,
                    path: preview.path,
                    title: preview.title,
                    previewText: unit.previewText,
                    snippet: unit.snippet,
                  },
                  id,
                );
              }}
            >
              <IconChat />
            </ChatDestPicker>
          </div>
          {(previewFile?.matchIds.length ?? 0) > 1 ? (
            <div className="preview-occ-nav" aria-live="polite">
              <button
                type="button"
                title="前のマッチへスクロール (←)"
                aria-label="前のマッチ"
                onClick={() => stepMatch(-1)}
              >
                ←
              </button>
              <span className="preview-occ-label">
                マッチ {matchNavIndex + 1} / {previewFile?.matchIds.length}
                {preview.page != null ? ` · p.${preview.page}` : ""}
              </span>
              <button
                type="button"
                title="次のマッチへスクロール (→)"
                aria-label="次のマッチ"
                onClick={() => stepMatch(1)}
              >
                →
              </button>
            </div>
          ) : null}
          {preview.source === "remote" ? (
            <div className="preview-excerpt-note">リモートのため抜粋のみ</div>
          ) : null}
          {previewFile?.excerpt &&
          preview.source !== "remote" &&
          !isJsonPath(preview.path) ? (
            <div className="preview-excerpt-note">
              長いファイルのため、マッチ周辺の抜粋です
            </div>
          ) : null}
          <div className="preview-scroll" ref={previewScrollRef}>
            {isJsonPath(preview.path) ? (
              <PreviewBody
                hit={preview}
                query={query}
                highlightTerms={previewHighlightTerms}
              />
            ) : (
              (previewFile?.units ?? [preview]).map((unit) => {
                const isMatch = previewFile?.matchIds.includes(unit.id);
                const isActive = unit.id === previewUnitId;
                return (
                  <article
                    key={unit.id}
                    data-preview-unit={unit.id}
                    className={`preview-unit${isMatch ? " is-match" : ""}${isActive ? " is-active" : ""}`}
                    onClick={() => {
                      setPreviewUnitId(unit.id);
                      setPreview(unit);
                    }}
                  >
                    {unit.unitLabel ? (
                      <div className="preview-unit-label">{unit.unitLabel}</div>
                    ) : null}
                    <PreviewBody
                      hit={unit}
                      query={query}
                      highlightTerms={previewHighlightTerms}
                    />
                  </article>
                );
              })
            )}
          </div>
          <div className="hint">
            {(previewFile?.matchIds.length ?? 0) > 1
              ? "←→ マッチへ移動 · Esc で一覧に戻る"
              : "Esc で一覧に戻る"}
          </div>
        </section>
      ) : (
        <ul className="hit-list" ref={listRef}>
          {hits.length === 0 ? (
            <li className="empty">
              {searching
                ? "検索中…"
                : query.trim()
                  ? "結果がありません。"
                  : "検索文字列を入力するか、文書上で選択してショートカットを押してください。語をドラッグすると隣接にできます。"}
            </li>
          ) : (
            (() => {
              const maxScore = Math.max(...hits.map((h) => h.score), 0);
              return hits.map((hit, i) => {
                const level = scoreLevel(hit.score, maxScore);
                const mailHit = isOutlookHit(hit);
                const nestParagraphs =
                  (hit.paragraphs?.length ?? 0) > 0 &&
                  !(mailHit && (hit.paragraphs?.length ?? 0) <= 1);
                return (
                  <li
                    key={`${hit.source}-${hit.id}`}
                    className={i === index ? "hit active" : "hit"}
                    onMouseEnter={() => setIndex(i)}
                    onDoubleClick={() => void openSelected()}
                  >
                    <div className="hit-main">
                      <div className="hit-title-row">
                        <div className="hit-title">
                          {hit.source === "remote" ? (
                            <span className="hit-source" title="リモート">
                              リモート
                            </span>
                          ) : null}
                          {isOutlookHit(hit) ? (
                            <span className="hit-source" title="Outlook メール">
                              メール
                            </span>
                          ) : null}
                          {isOutlookHit(hit)
                            ? (() => {
                                const ymd = formatMailDateYmd(hit.mailDate);
                                return ymd ? (
                                  <span
                                    className="hit-mail-date"
                                    title="受信日"
                                  >
                                    {ymd}
                                  </span>
                                ) : null;
                              })()
                            : null}
                          {(() => {
                            if (isOutlookHit(hit)) return null;
                            const ext = extFromPath(hit.path);
                            return ext ? (
                              <span className="hit-ext" title={hit.path}>
                                {ext}
                              </span>
                            ) : null;
                          })()}
                          {highlight(hit.title, query, hit.highlightTerms)}
                        </div>
                        <div
                          className="hit-score"
                          aria-label={`マッチ度 ${level}/5`}
                          title={`マッチ度 ${level}/5`}
                        >
                          {[1, 2, 3, 4, 5].map((n) => (
                            <IconScoreDog key={n} filled={n <= level} />
                          ))}
                        </div>
                      </div>
                      {isOutlookHit(hit) ? (
                        <div className="hit-mail-meta muted">
                          {[
                            hit.mailFrom,
                            hit.mailFolder
                              ? formatMailFolderMeta(hit.mailFolder)
                              : "",
                          ]
                            .filter(Boolean)
                            .join(" · ")}
                          {(hit.matchCount ?? 0) > 1
                            ? ` · スレッド ${hit.matchCount} 通`
                            : ""}
                        </div>
                      ) : null}
                      {mailHit ? (
                        <ul className="hit-paragraphs">
                          <HitUnitCard
                            label=""
                            snippet={hit.snippet}
                            query={query}
                            highlightTerms={hit.highlightTerms}
                            previewTitle="このメールをプレビュー"
                            onPreview={() => {
                              setIndex(i);
                              void showPreview(hit);
                            }}
                            onKeep={(id) => {
                              void keepParagraph(
                                {
                                  paragraphId: hit.id,
                                  label: hit.unitLabel || "メール",
                                  page: hit.page,
                                  snippet: hit.snippet,
                                  body: hit.previewText,
                                  fileHit: hit,
                                },
                                id,
                              );
                            }}
                            onChat={(id) => {
                              void sendHitToChat(
                                {
                                  paragraphId: hit.id,
                                  path: hit.path,
                                  title: hit.title,
                                  previewText: hit.previewText,
                                  snippet: hit.snippet,
                                },
                                id,
                              );
                            }}
                          />
                        </ul>
                      ) : nestParagraphs ? (
                        <ul className="hit-paragraphs">
                          {(expandedParas[hit.path] ?? hit.paragraphs).map(
                            (p) => (
                              <HitUnitCard
                                key={p.id}
                                label={p.label || "段落"}
                                snippet={p.snippet}
                                query={query}
                                highlightTerms={hit.highlightTerms}
                                previewTitle="この段落をプレビュー"
                                onPreview={() => {
                                  setIndex(i);
                                  void previewParagraph(p.id, hit);
                                }}
                                onKeep={(id) => {
                                  void keepParagraph(
                                    {
                                      paragraphId: p.id,
                                      label: p.label,
                                      page: p.page,
                                      snippet: p.snippet,
                                      fileHit: hit,
                                    },
                                    id,
                                  );
                                }}
                                onChat={(id) => {
                                  void sendHitToChat(
                                    {
                                      paragraphId: p.id,
                                      path: hit.path,
                                      title: hit.title,
                                      snippet: p.snippet,
                                    },
                                    id,
                                  );
                                }}
                              />
                            ),
                          )}
                          {!expandedParas[hit.path] &&
                          (hit.matchCount ?? 0) >
                            (hit.paragraphs?.length ?? 0) ? (
                            <li className="hit-paragraph-more">
                              <button
                                type="button"
                                className="hit-paragraph-more-btn"
                                onClick={(e) => {
                                  e.stopPropagation();
                                  void expandHitParagraphs(hit);
                                }}
                              >
                                さらに表示（他{" "}
                                {(hit.matchCount ?? 0) -
                                  (hit.paragraphs?.length ?? 0)}{" "}
                                件）
                              </button>
                            </li>
                          ) : null}
                        </ul>
                      ) : (
                        <div className="hit-snippet">
                          {highlight(hit.snippet, query, hit.highlightTerms)}
                        </div>
                      )}
                      <div className="hit-path" title={hit.path}>
                        {!mailHit && hit.matchCount && hit.matchCount > 1
                          ? `マッチ ${hit.matchCount} 段落 · `
                          : null}
                        {hit.path}
                      </div>
                    </div>
                    <div className="hit-actions">
                      <button
                        type="button"
                        className="hit-action-btn"
                        title="プレビュー (Ctrl+Enter)"
                        aria-label="プレビュー"
                        onClick={(e) => {
                          e.stopPropagation();
                          setIndex(i);
                          void showPreview(hit);
                        }}
                      >
                        <IconPreview />
                      </button>
                      <button
                        type="button"
                        className="hit-action-btn"
                        title={mailHit ? "メールを開く (Enter)" : "ファイルを開く (Enter)"}
                        aria-label={mailHit ? "メールを開く" : "ファイルを開く"}
                        onClick={(e) => {
                          e.stopPropagation();
                          void openSelected(hit.path);
                        }}
                      >
                        <IconOpenFile />
                      </button>
                      {!mailHit ? (
                        <button
                          type="button"
                          className="hit-action-btn"
                          title="フォルダを開く (Shift+Enter)"
                          aria-label="フォルダを開く"
                          onClick={(e) => {
                            e.stopPropagation();
                            void openFolder(hit.path);
                          }}
                        >
                          <IconFolder />
                        </button>
                      ) : null}
                      <button
                        type="button"
                        className="hit-action-btn"
                        title="このフォルダ内で再検索"
                        aria-label="このフォルダ内で再検索"
                        onClick={(e) => {
                          e.stopPropagation();
                          rescopeToHitFolder(hit);
                        }}
                      >
                        <IconRescope />
                      </button>
                    </div>
                  </li>
                );
              });
            })()
          )}
        </ul>
      )}

      {actionError ? (
        <div className="popup-error">
          <span>{actionError}</span>
          {actionError.includes("場所が変わった") ? (
            <button
              type="button"
              className="popup-error-action"
              onClick={() => void openSettings()}
            >
              設定を開く
            </button>
          ) : null}
        </div>
      ) : null}

      <footer className="popup-footer">
        {preview ? (
          <>
            <span>←→ マッチへ移動</span>
            <button
              type="button"
              className="popup-footer-action"
              title={
                isOutlookHit(preview) ? "メールを開く (Enter)" : "ファイルを開く (Enter)"
              }
              onClick={() => void openSelected()}
            >
              Enter 開く
            </button>
            <button
              type="button"
              className="popup-footer-action"
              title="一覧に戻る (Esc)"
              onClick={closePreview}
            >
              Esc 一覧
            </button>
          </>
        ) : (
          <>
            <span>↑↓ 移動</span>
            <button
              type="button"
              className="popup-footer-action"
              title={currentIsMail ? "メールを開く (Enter)" : "ファイルを開く (Enter)"}
              onClick={() => void openSelected()}
            >
              Enter 開く
            </button>
            {!currentIsMail ? (
              <button
                type="button"
                className="popup-footer-action"
                title="フォルダを開く (Shift+Enter)"
                onClick={() => void openFolder()}
              >
                Shift+Enter フォルダ
              </button>
            ) : null}
            <button
              type="button"
              className="popup-footer-action"
              title="プレビュー (Ctrl+Enter)"
              onClick={() => void showPreview()}
            >
              Ctrl+Enter プレビュー
            </button>
            <button
              type="button"
              className="popup-footer-action"
              title="閉じる (Esc)"
              onClick={() => void hidePopup()}
            >
              Esc 閉じる
            </button>
          </>
        )}
        <div className="popup-footer-windows">
          <button
            type="button"
            className="popup-settings-btn"
            title="チャット"
            aria-label="チャット"
            onClick={() => void openChat()}
          >
            <IconChat />
          </button>
          <button
            type="button"
            className="popup-settings-btn"
            title="ノート"
            aria-label="ノート"
            onClick={() => void openNotes()}
          >
            <IconNotes />
          </button>
          <button
            type="button"
            className="popup-settings-btn"
            title="設定"
            aria-label="設定"
            onClick={() => void openSettings()}
          >
            <IconSettings />
          </button>
        </div>
      </footer>
    </div>
  );
}
