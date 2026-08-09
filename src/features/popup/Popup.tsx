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
  findJsonHitOffset,
  formatJsonForPreview,
  isHtmlPath,
  isJsonPath,
  isMarkdownPath,
  renderMarkdownHtml,
  splitProseParagraphs,
} from "./markdownPreview";
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

function IconNotes() {
  return (
    <HitActionIcon>
      <path d="M3.5 3.25h9A1.25 1.25 0 0 1 13.75 4.5v7A1.25 1.25 0 0 1 12.5 12.75h-9A1.25 1.25 0 0 1 2.25 11.5v-7A1.25 1.25 0 0 1 3.5 3.25Z" />
      <path d="M5.25 6.25h5.5" />
      <path d="M5.25 8.75h4" />
    </HitActionIcon>
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

type SearchHistoryTermRow = { term: string; count: number; last: number };

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

function formatMailDate(unixStr?: string): string {
  if (!unixStr) return "";
  const n = Number(unixStr);
  if (!Number.isFinite(n) || n <= 0) return "";
  try {
    return new Date(n * 1000).toLocaleString();
  } catch {
    return "";
  }
}

/** Split query for highlight using the same delimiters as the Rust parser (outside quotes). */
function highlightTermsFromQuery(query: string): string[] {
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

function highlight(text: string, query: string, highlightTerms?: string[]) {
  const fromHit = (highlightTerms ?? []).filter((t) => t.trim().length > 0);
  // When the backend already returns morph content terms (光景 / 見慣れ), do not also
  // highlight the entire unspaced Japanese query as one giant term — that hides
  // which content words actually matched.
  const fromQuery =
    fromHit.length > 0
      ? []
      : highlightTermsFromQuery(query).filter((t) => {
          const hasDelim = /[\s\u3000,\uFF0C\u3001]/.test(t);
          // Ignore a single long run of Japanese with no delimiters (selection paste).
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

function escapeRegExp(s: string) {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

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
}: {
  hit: SearchHit;
  query: string;
}) {
  const containerRef = useRef<HTMLDivElement>(null);
  const preRef = useRef<HTMLPreElement>(null);
  const isMarkdown = isMarkdownPath(hit.path);
  const isHtml = isHtmlPath(hit.path);
  const isJson = isJsonPath(hit.path);
  const [jsonText, setJsonText] = useState<string | null>(null);
  const [jsonLoading, setJsonLoading] = useState(false);

  const markdownHtml = useMemo(() => {
    if (!isMarkdown) return "";
    return renderMarkdownHtml(hit.previewText);
  }, [hit.previewText, isMarkdown]);
  const proseParagraphs = useMemo(() => {
    if (!isHtml) return [];
    return splitProseParagraphs(hit.previewText);
  }, [hit.previewText, isHtml]);
  const highlightTerms = useMemo(
    () => collectPreviewHighlightTerms(query, hit.highlightTerms),
    [query, hit.highlightTerms],
  );

  useEffect(() => {
    if (!isJson) {
      setJsonText(null);
      setJsonLoading(false);
    }
  }, [isJson]);

  useEffect(() => {
    if (!isJson || hit.source !== "remote") return;
    setJsonText(formatJsonForPreview(hit.previewText));
    setJsonLoading(false);
  }, [hit.previewText, hit.source, isJson]);

  useEffect(() => {
    if (!isJson || hit.source === "remote") return;
    let cancelled = false;
    setJsonText(null);
    setJsonLoading(true);
    const fallback = hit.previewText;
    void invoke<string>("read_text_file", { path: hit.path })
      .then((raw) => {
        if (cancelled) return;
        setJsonText(formatJsonForPreview(raw));
      })
      .catch(() => {
        if (cancelled) return;
        setJsonText(formatJsonForPreview(fallback));
      })
      .finally(() => {
        if (!cancelled) setJsonLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [hit.path, hit.source, isJson]);

  useEffect(() => {
    if (!isMarkdown) return;
    const el = containerRef.current;
    if (!el) return;
    applyPreviewHighlights(el, highlightTerms);
    return () => clearPreviewHighlights();
  }, [highlightTerms, isMarkdown, markdownHtml]);

  useLayoutEffect(() => {
    if (!isJson || jsonLoading || jsonText == null) return;
    const pre = preRef.current;
    if (!pre) return;

    const offset = findJsonHitOffset(
      jsonText,
      hit.previewText ?? "",
      hit.snippet ?? "",
    );

    const scrollToOffset = () => {
      if (offset >= 0) {
        const walker = document.createTreeWalker(pre, NodeFilter.SHOW_TEXT);
        let pos = 0;
        let target: Element | null = null;
        while (walker.nextNode()) {
          const node = walker.currentNode as Text;
          const len = node.data.length;
          if (pos + len > offset) {
            target =
              node.parentElement?.closest("mark") ??
              node.parentElement ??
              pre;
            break;
          }
          pos += len;
        }
        if (target) {
          target.scrollIntoView({ block: "center", inline: "nearest" });
          return;
        }
      }
      const firstMark = pre.querySelector("mark");
      if (firstMark) {
        firstMark.scrollIntoView({ block: "center", inline: "nearest" });
      }
    };

    // Wait for React to commit mark nodes before measuring.
    const frame = requestAnimationFrame(scrollToOffset);
    return () => cancelAnimationFrame(frame);
  }, [
    hit.id,
    hit.previewText,
    hit.snippet,
    isJson,
    jsonLoading,
    jsonText,
  ]);

  if (isMarkdown) {
    return (
      <div
        ref={containerRef}
        className="preview-body preview-body--markdown"
        dangerouslySetInnerHTML={{ __html: markdownHtml }}
      />
    );
  }

  if (isHtml) {
    return (
      <div className="preview-body preview-body--prose">
        {proseParagraphs.map((para, i) => (
          <p key={i}>{highlight(para, query, hit.highlightTerms)}</p>
        ))}
      </div>
    );
  }

  if (isJson) {
    if (jsonLoading && jsonText == null) {
      return <pre className="preview-body">読み込み中…</pre>;
    }
    const text = jsonText ?? formatJsonForPreview(hit.previewText);
    return (
      <pre ref={preRef} className="preview-body">
        {highlight(text, query, hit.highlightTerms)}
      </pre>
    );
  }

  return (
    <pre className="preview-body">
      {highlight(hit.previewText, query, hit.highlightTerms)}
    </pre>
  );
}

export default function Popup() {
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<SearchHit[]>([]);
  const [index, setIndex] = useState(0);
  const [preview, setPreview] = useState<SearchHit | null>(null);
  const [maximized, setMaximized] = useState(false);
  const [occurrences, setOccurrences] = useState<SearchHit[]>([]);
  const [occIndex, setOccIndex] = useState(0);
  const [searching, setSearching] = useState(false);
  const [actionError, setActionError] = useState("");
  const [wordPickerOpen, setWordPickerOpen] = useState(false);
  const [wordPickerFilter, setWordPickerFilter] = useState<
    "all" | "history" | "registered"
  >("all");
  const [folderPickerOpen, setFolderPickerOpen] = useState(false);
  const [extPickerOpen, setExtPickerOpen] = useState(false);
  const [helpOpen, setHelpOpen] = useState(false);
  const [searchWords, setSearchWords] = useState<SearchWordRow[]>([]);
  const [historyTerms, setHistoryTerms] = useState<SearchHistoryTermRow[]>([]);
  const [suggestions, setSuggestions] = useState<SearchTermSuggestion[]>([]);
  const [suggestIndex, setSuggestIndex] = useState(0);
  const [suggestOpen, setSuggestOpen] = useState(false);
  const [searchScopes, setSearchScopes] = useState<SearchScopeRow[]>([]);
  const [recentScopes, setRecentScopes] = useState<SearchScopeRow[]>([]);
  const [scopeFilter, setScopeFilter] = useState("");
  const [scopePath, setScopePath] = useState<string | null>(null);
  const [scopeLabel, setScopeLabel] = useState<string | null>(null);
  const [extFilterKeys, setExtFilterKeys] = useState<string[]>([]);
  const inputRef = useRef<HTMLInputElement>(null);
  const scopeFilterRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLUListElement>(null);
  const wordPickerRef = useRef<HTMLDivElement>(null);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const searchSeq = useRef(0);
  const scopePathRef = useRef<string | null>(null);
  const extFilterRef = useRef<string[]>([]);
  const imeComposingRef = useRef(false);

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
        setPreview(null);
        setOccurrences([]);
        setOccIndex(0);
        void invoke("record_search_query", { query: trimmed }).catch(console.error);
      } catch (e) {
        console.error(e);
      } finally {
        if (seq === searchSeq.current) {
          setSearching(false);
        }
      }
    },
    [],
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
      clearScope();
      clearExtFilter();
      setQuery(event.payload.query);
      setHits(event.payload.hits);
      setIndex(0);
      setPreview(null);
      setOccurrences([]);
      setOccIndex(0);
      setActionError("");
      setSearching(Boolean(event.payload.searching));
      setWordPickerOpen(false);
      setFolderPickerOpen(false);
      setExtPickerOpen(false);
      setHelpOpen(false);
      setSuggestOpen(false);
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
  }, [clearScope, clearExtFilter]);

  useEffect(() => {
    if (
      !wordPickerOpen &&
      !helpOpen &&
      !folderPickerOpen &&
      !extPickerOpen &&
      !suggestOpen
    ) {
      return;
    }
    const onPointerDown = (e: MouseEvent) => {
      if (wordPickerRef.current?.contains(e.target as Node)) return;
      setWordPickerOpen(false);
      setFolderPickerOpen(false);
      setExtPickerOpen(false);
      setHelpOpen(false);
      setSuggestOpen(false);
    };
    document.addEventListener("mousedown", onPointerDown);
    return () => document.removeEventListener("mousedown", onPointerDown);
  }, [wordPickerOpen, helpOpen, folderPickerOpen, extPickerOpen, suggestOpen]);

  const openWordPicker = useCallback(async () => {
    try {
      const [words, history] = await Promise.all([
        invoke<SearchWordRow[]>("list_search_words"),
        invoke<SearchHistoryTermRow[]>("list_search_history_terms"),
      ]);
      setSearchWords(words);
      setHistoryTerms(history);
      setHelpOpen(false);
      setFolderPickerOpen(false);
      setExtPickerOpen(false);
      setSuggestOpen(false);
      setWordPickerOpen(true);
    } catch (e) {
      console.error(e);
      setActionError(String(e));
    }
  }, []);

  const clearHistoryTerms = useCallback(async () => {
    try {
      await invoke("clear_search_term_history");
      setHistoryTerms([]);
    } catch (e) {
      console.error(e);
      setActionError(String(e));
    }
  }, []);

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
      setHelpOpen(false);
      setWordPickerOpen(false);
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
    setHelpOpen(false);
    setWordPickerOpen(false);
    setFolderPickerOpen(false);
    setSuggestOpen(false);
    setExtPickerOpen(true);
  }, []);

  const toggleHelp = useCallback(() => {
    setWordPickerOpen(false);
    setFolderPickerOpen(false);
    setExtPickerOpen(false);
    setSuggestOpen(false);
    setHelpOpen((v) => !v);
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

  const appendSearchWord = useCallback(
    (word: string) => {
      const trimmed = word.trim();
      if (!trimmed) return;
      const next = query.trim() ? `${query.trim()} ${trimmed}` : trimmed;
      setQuery(next);
      setWordPickerOpen(false);
      setSuggestOpen(false);
      scheduleSearch(next);
      requestAnimationFrame(() => {
        inputRef.current?.focus();
      });
    },
    [query, scheduleSearch],
  );

  const registerCurrentQueryWord = useCallback(async () => {
    const word = query.trim();
    if (!word) {
      setActionError("登録する検索語が空です");
      return;
    }
    let toRegister = word;
    const input = inputRef.current;
    if (input && input.selectionStart != null && input.selectionEnd != null) {
      const start = input.selectionStart;
      const end = input.selectionEnd;
      if (end > start) {
        toRegister = query.slice(start, end).trim() || word;
      }
    }
    if (toRegister === word && /[\s\u3000,\uFF0C\u3001]/.test(word)) {
      const parts = word.split(/[\s\u3000,\uFF0C\u3001]+/).filter(Boolean);
      const last = parts[parts.length - 1]
        ?.replace(/^-+/, "")
        .replace(/^"|"$/g, "");
      if (last) toRegister = last;
    }
    try {
      await invoke("add_search_word", { word: toRegister });
      const words = await invoke<SearchWordRow[]>("list_search_words");
      setSearchWords(words);
      setActionError("");
      scheduleSearch(query);
    } catch (e) {
      setActionError(String(e));
    }
  }, [query, scheduleSearch]);

  const openSelected = useCallback(
    async (path?: string) => {
      const target = path ?? hits[index]?.path;
      if (!target) return;
      setActionError("");
      try {
        await invoke("open_hit", { path: target });
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
      setActionError("");
      try {
        await invoke("open_containing_folder", { path: target });
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

  const keepParagraph = useCallback(
    async (opts: {
      paragraphId: string;
      label?: string;
      page?: number | null;
      snippet?: string;
      body?: string;
      fileHit: SearchHit;
    }) => {
      const { paragraphId, label, page, snippet, body, fileHit } = opts;
      setActionError("");
      try {
        await invoke("keep_to_note", {
          payload: {
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
        });
      } catch (e) {
        setActionError(String(e));
      }
    },
    [query],
  );

  const keepHitFallback = useCallback(
    async (hit: SearchHit) => {
      await keepParagraph({
        paragraphId: hit.id,
        label: hit.unitLabel || "",
        page: hit.page,
        snippet: hit.snippet,
        body: hit.previewText || undefined,
        fileHit: hit,
      });
    },
    [keepParagraph],
  );

  const showPreview = useCallback(
    async (target?: SearchHit) => {
      const hit = target ?? hits[index];
      if (!hit) return;
      setPreview(hit);
      setOccurrences([hit]);
      setOccIndex(0);
      if (hit.source === "remote") return;
      try {
        const matches = await invoke<SearchHit[]>("search_path_matches", {
          query: query.trim(),
          path: hit.path,
        });
        if (!matches.length) return;
        setOccurrences(matches);
        const found = matches.findIndex((m) => m.id === hit.id);
        setOccIndex(found >= 0 ? found : 0);
        setPreview(matches[found >= 0 ? found : 0] ?? hit);
      } catch (e) {
        console.error(e);
      }
    },
    [hits, index, query],
  );

  const previewParagraph = useCallback(
    async (paraId: string, fileHit: SearchHit) => {
      setActionError("");
      try {
        if (fileHit.source === "remote") {
          const hit = await invoke<SearchHit | null>("get_preview", {
            hitId: paraId,
          });
          if (hit) {
            setPreview(hit);
            setOccurrences([hit]);
            setOccIndex(0);
          }
          return;
        }
        const matches = await invoke<SearchHit[]>("search_path_matches", {
          query: query.trim(),
          path: fileHit.path,
        });
        if (!matches.length) {
          const hit = await invoke<SearchHit | null>("get_preview", {
            hitId: paraId,
          });
          if (hit) {
            setPreview(hit);
            setOccurrences([hit]);
            setOccIndex(0);
          }
          return;
        }
        setOccurrences(matches);
        const found = matches.findIndex((m) => m.id === paraId);
        const idx = found >= 0 ? found : 0;
        setOccIndex(idx);
        setPreview(matches[idx] ?? fileHit);
      } catch (e) {
        setActionError(String(e));
      }
    },
    [query],
  );

  const stepOccurrence = useCallback(
    (delta: number) => {
      if (occurrences.length <= 1) return;
      setOccIndex((i) => {
        const next = (i + delta + occurrences.length) % occurrences.length;
        const hit = occurrences[next];
        if (hit) setPreview(hit);
        return next;
      });
    },
    [occurrences],
  );

  const closePreview = useCallback(() => {
    setPreview(null);
    setOccurrences([]);
    setOccIndex(0);
  }, []);

  const hidePopup = useCallback(async () => {
    clearScope();
    setFolderPickerOpen(false);
    setWordPickerOpen(false);
    setExtPickerOpen(false);
    setHelpOpen(false);
    setSuggestOpen(false);
    setPreview(null);
    setOccurrences([]);
    setOccIndex(0);
    await invoke("hide_popup");
  }, [clearScope]);

  useEffect(() => {
    if (preview) return;
    const active = listRef.current?.querySelector<HTMLElement>(".hit.active");
    active?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }, [index, hits, preview]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        if (folderPickerOpen) {
          setFolderPickerOpen(false);
          return;
        }
        if (extPickerOpen) {
          setExtPickerOpen(false);
          return;
        }
        if (wordPickerOpen) {
          setWordPickerOpen(false);
          return;
        }
        if (helpOpen) {
          setHelpOpen(false);
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
      if (wordPickerOpen || helpOpen || folderPickerOpen || extPickerOpen) return;

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
      }

      if (preview) {
        if (e.key === "ArrowLeft" || e.key === "[") {
          e.preventDefault();
          stepOccurrence(-1);
          return;
        }
        if (e.key === "ArrowRight" || e.key === "]") {
          e.preventDefault();
          stepOccurrence(1);
          return;
        }
        if (e.key === "Enter" && e.shiftKey) {
          e.preventDefault();
          void openFolder();
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
        void openFolder();
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
    helpOpen,
    hidePopup,
    hits.length,
    openFolder,
    openSelected,
    preview,
    showPreview,
    stepOccurrence,
    suggestIndex,
    suggestOpen,
    suggestions,
    wordPickerOpen,
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
    e.preventDefault();
    try {
      await invoke("set_popup_dragging", { dragging: true });
      await getCurrentWindow().startDragging();
    } catch (err) {
      console.error(err);
    } finally {
      await invoke("set_popup_dragging", { dragging: false });
    }
  }, []);

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
            {searching ? "検索中…" : "入力で再検索 / 余白ドラッグで移動"}
          </span>
        </div>
        <div className="popup-query-row" ref={wordPickerRef}>
          <input
            ref={inputRef}
            className="popup-query-input"
            value={query}
            placeholder={'検索 "隣接" -除外'}
            spellCheck={false}
            onChange={(e) => {
              const next = e.target.value;
              setQuery(next);
              scheduleSearch(next);
              if (
                !wordPickerOpen &&
                !folderPickerOpen &&
                !extPickerOpen &&
                !helpOpen
              ) {
                void refreshSuggestions(next);
              } else {
                setSuggestOpen(false);
              }
            }}
            onCompositionStart={() => {
              imeComposingRef.current = true;
              setSuggestOpen(false);
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
              if (
                !wordPickerOpen &&
                !folderPickerOpen &&
                !extPickerOpen &&
                !helpOpen
              ) {
                void refreshSuggestions(next);
              }
            }}
            onMouseDown={(e) => e.stopPropagation()}
          />
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
          <button
            type="button"
            className="popup-word-add-btn"
            title="履歴・登録ワード"
            aria-label="履歴・登録ワード"
            aria-expanded={wordPickerOpen}
            onMouseDown={(e) => e.stopPropagation()}
            onClick={() => {
              if (wordPickerOpen) {
                setWordPickerOpen(false);
              } else {
                void openWordPicker();
              }
            }}
          >
            <IconList />
          </button>
          <button
            type="button"
            className="popup-help-btn"
            title="検索構文のヒント"
            aria-label="検索構文のヒント"
            aria-expanded={helpOpen}
            onMouseDown={(e) => e.stopPropagation()}
            onClick={() => toggleHelp()}
          >
            ？
          </button>
          {helpOpen ? (
            <div className="popup-syntax-help" role="dialog" aria-label="検索構文のヒント">
              <div className="popup-syntax-help-title">検索の書き方</div>
              <ul>
                <li>
                  <code>スペース</code> または <code>,</code> <code>、</code> で区切る（半角・全角OK）
                </li>
                <li>
                  <code>&quot;損害賠償&quot;</code> … 隣接フレーズ（1語として検索）
                </li>
                <li>
                  <code>-慰謝料</code> … その語を含む結果を除外
                </li>
                <li>
                  <code>-&quot;損害賠償&quot;</code> … その隣接フレーズを除外
                </li>
              </ul>
              <div className="popup-syntax-help-example">
                例: <code>契約 &quot;損害賠償&quot; -慰謝料</code>
              </div>
            </div>
          ) : null}
          {wordPickerOpen ? (
            <div className="popup-word-picker" role="listbox" aria-label="検索ワード">
              <div className="popup-word-picker-toolbar">
                <button
                  type="button"
                  className="popup-word-register-btn"
                  title="入力中の語を辞書登録"
                  aria-label="入力中の語を辞書登録"
                  onClick={() => void registerCurrentQueryWord()}
                  disabled={!query.trim()}
                >
                  辞書登録
                </button>
                <div
                  className="popup-word-filter"
                  role="tablist"
                  aria-label="表示切り替え"
                >
                  {(
                    [
                      ["all", "すべて"],
                      ["history", "履歴"],
                      ["registered", "登録"],
                    ] as const
                  ).map(([id, label]) => (
                    <button
                      key={id}
                      type="button"
                      role="tab"
                      aria-selected={wordPickerFilter === id}
                      className={
                        wordPickerFilter === id
                          ? "popup-word-filter-btn is-active"
                          : "popup-word-filter-btn"
                      }
                      onClick={() => setWordPickerFilter(id)}
                    >
                      {label}
                    </button>
                  ))}
                </div>
              </div>
              {(() => {
                const showHistory =
                  wordPickerFilter === "all" || wordPickerFilter === "history";
                const showRegistered =
                  wordPickerFilter === "all" ||
                  wordPickerFilter === "registered";
                const hist = showHistory ? historyTerms : [];
                const words = showRegistered ? searchWords : [];
                if (hist.length === 0 && words.length === 0) {
                  return (
                    <div className="popup-word-empty">
                      {wordPickerFilter === "history"
                        ? "履歴はまだありません。検索するとここに残ります。"
                        : wordPickerFilter === "registered"
                          ? "登録ワードはありません。上のボタンまたは設定から追加できます。"
                          : "履歴・登録ワードはまだありません。検索すると履歴に残り、設定の「検索ワード登録」からも追加できます。"}
                    </div>
                  );
                }
                return (
                  <ul>
                    {hist.map((h) => (
                      <li key={`hist:${h.term}`}>
                        <button
                          type="button"
                          role="option"
                          onClick={() => appendSearchWord(h.term)}
                        >
                          <span className="word-kind history">履歴</span>
                          <span className="word-label">{h.term}</span>
                        </button>
                      </li>
                    ))}
                    {hist.length > 0 && words.length > 0 ? (
                      <li className="scope-sep" aria-hidden="true" />
                    ) : null}
                    {words.map((w) => (
                      <li key={w.id}>
                        <button
                          type="button"
                          role="option"
                          onClick={() => appendSearchWord(w.word)}
                        >
                          <span className="word-kind registered">登録</span>
                          <span className="word-label">{w.word}</span>
                        </button>
                      </li>
                    ))}
                  </ul>
                );
              })()}
              {historyTerms.length > 0 &&
              (wordPickerFilter === "all" ||
                wordPickerFilter === "history") ? (
                <div className="popup-word-picker-actions popup-word-picker-footer">
                  <button
                    type="button"
                    className="popup-ext-clear-btn"
                    onClick={() => void clearHistoryTerms()}
                  >
                    履歴をクリア
                  </button>
                </div>
              ) : null}
            </div>
          ) : null}
          {suggestOpen &&
          suggestions.length > 0 &&
          !wordPickerOpen &&
          !folderPickerOpen &&
          !extPickerOpen &&
          !helpOpen ? (
            <div
              className="popup-suggest"
              role="listbox"
              aria-label="検索候補"
            >
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
                ↑↓ で選択 · Tab で確定 · Esc で閉じる
              </div>
            </div>
          ) : null}
          {folderPickerOpen ? (
            <div
              className="popup-folder-picker"
              role="listbox"
              aria-label="検索対象フォルダ"
            >
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
                  formatMailDate(preview.mailDate),
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
              title="ファイルを開く (Enter)"
              aria-label="ファイルを開く"
              onClick={() => void openSelected()}
            >
              <IconOpenFile />
            </button>
            <button
              type="button"
              className="hit-action-btn"
              title="フォルダを開く (Shift+Enter)"
              aria-label="フォルダを開く"
              onClick={() => void openFolder(preview.path)}
            >
              <IconFolder />
            </button>
            <button
              type="button"
              className="hit-action-btn"
              title="このフォルダ内で再検索"
              aria-label="このフォルダ内で再検索"
              onClick={() => rescopeToHitFolder(preview)}
            >
              <IconRescope />
            </button>
            <button
              type="button"
              className="hit-action-btn"
              title="この出現箇所をノートにキープ"
              aria-label="ノートにキープ"
              onClick={() =>
                void keepParagraph({
                  paragraphId: preview.id,
                  label: preview.unitLabel || "",
                  page: preview.page,
                  snippet: preview.snippet,
                  body: preview.previewText,
                  fileHit: preview,
                })
              }
            >
              <IconKeep />
            </button>
          </div>
          {occurrences.length > 0 ? (
            <div className="preview-occ-nav" aria-live="polite">
              <button
                type="button"
                disabled={occurrences.length <= 1}
                title="前の出現箇所 (←)"
                aria-label="前の出現箇所"
                onClick={() => stepOccurrence(-1)}
              >
                ←
              </button>
              <span className="preview-occ-label">
                {occIndex + 1} / {occurrences.length}
                {preview.page != null ? ` · p.${preview.page}` : ""}
              </span>
              <button
                type="button"
                disabled={occurrences.length <= 1}
                title="次の出現箇所 (→)"
                aria-label="次の出現箇所"
                onClick={() => stepOccurrence(1)}
              >
                →
              </button>
            </div>
          ) : null}
          <PreviewBody hit={preview} query={query} />
          <div className="hint">
            {occurrences.length > 1
              ? "←→ 出現箇所 · Esc で一覧に戻る"
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
                  : "検索文字列を入力するか、文書上で選択してショートカットを押してください。"}
            </li>
          ) : (
            (() => {
              const maxScore = Math.max(...hits.map((h) => h.score), 0);
              return hits.map((hit, i) => {
                const level = scoreLevel(hit.score, maxScore);
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
                            formatMailDate(hit.mailDate),
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
                      {hit.paragraphs && hit.paragraphs.length > 0 ? (
                        <ul className="hit-paragraphs">
                          {hit.paragraphs.map((p) => (
                            <li key={p.id} className="hit-paragraph">
                              <button
                                type="button"
                                className="hit-paragraph-btn"
                                title="この段落をプレビュー"
                                onClick={(e) => {
                                  e.stopPropagation();
                                  setIndex(i);
                                  void previewParagraph(p.id, hit);
                                }}
                              >
                                <span className="hit-paragraph-label">
                                  {p.label || "段落"}
                                </span>
                                <span className="hit-paragraph-snippet">
                                  {highlight(
                                    p.snippet,
                                    query,
                                    hit.highlightTerms,
                                  )}
                                </span>
                              </button>
                              <button
                                type="button"
                                className="hit-paragraph-keep"
                                title="ノートにキープ"
                                aria-label="ノートにキープ"
                                onClick={(e) => {
                                  e.stopPropagation();
                                  void keepParagraph({
                                    paragraphId: p.id,
                                    label: p.label,
                                    page: p.page,
                                    snippet: p.snippet,
                                    fileHit: hit,
                                  });
                                }}
                              >
                                <IconKeep />
                              </button>
                            </li>
                          ))}
                          {(hit.matchCount ?? 0) > hit.paragraphs.length ? (
                            <li className="hit-paragraph-more">
                              他 {(hit.matchCount ?? 0) - hit.paragraphs.length}{" "}
                              件（プレビューでキープ可）
                            </li>
                          ) : null}
                        </ul>
                      ) : (
                        <div className="hit-snippet">
                          {highlight(hit.snippet, query, hit.highlightTerms)}
                        </div>
                      )}
                      <div className="hit-path" title={hit.path}>
                        {hit.matchCount && hit.matchCount > 1
                          ? `マッチ ${hit.matchCount} 段落 · `
                          : null}
                        {hit.path}
                      </div>
                      {hit.highlightTerms && hit.highlightTerms.length > 0 ? (
                        <div className="hit-terms">
                          {hit.highlightTerms.map((t) => (
                            <span key={t} className="hit-term">
                              {t}
                            </span>
                          ))}
                        </div>
                      ) : null}
                    </div>
                    <div className="hit-actions">
                      {!(hit.paragraphs && hit.paragraphs.length > 0) ? (
                        <button
                          type="button"
                          className="hit-action-btn"
                          title="ノートにキープ"
                          aria-label="ノートにキープ"
                          onClick={(e) => {
                            e.stopPropagation();
                            void keepHitFallback(hit);
                          }}
                        >
                          <IconKeep />
                        </button>
                      ) : null}
                      <button
                        type="button"
                        className="hit-action-btn"
                        title="ファイルを開く (Enter)"
                        aria-label="ファイルを開く"
                        onClick={(e) => {
                          e.stopPropagation();
                          void openSelected(hit.path);
                        }}
                      >
                        <IconOpenFile />
                      </button>
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
                        title="このフォルダ内で再検索"
                        aria-label="このフォルダ内で再検索"
                        onClick={(e) => {
                          e.stopPropagation();
                          rescopeToHitFolder(hit);
                        }}
                      >
                        <IconRescope />
                      </button>
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
                    </div>
                  </li>
                );
              });
            })()
          )}
        </ul>
      )}

      {actionError ? <div className="popup-error">{actionError}</div> : null}

      <footer className="popup-footer">
        {preview ? (
          <>
            <span>←→ 出現箇所</span>
            <button
              type="button"
              className="popup-footer-action"
              title="ファイルを開く (Enter)"
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
              title="ファイルを開く (Enter)"
              onClick={() => void openSelected()}
            >
              Enter 開く
            </button>
            <button
              type="button"
              className="popup-footer-action"
              title="フォルダを開く (Shift+Enter)"
              onClick={() => void openFolder()}
            >
              Shift+Enter フォルダ
            </button>
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
        <button
          type="button"
          className="popup-settings-btn"
          title="ノートを開く"
          onClick={() => void openNotes()}
        >
          <IconNotes />
          ノート
        </button>
        <button
          type="button"
          className="popup-settings-btn"
          onClick={() => void openSettings()}
        >
          設定
        </button>
      </footer>
    </div>
  );
}
