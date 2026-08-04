import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  applyPreviewHighlights,
  clearPreviewHighlights,
  collectPreviewHighlightTerms,
  isMarkdownPath,
  renderMarkdownHtml,
} from "./markdownPreview";
import "./popup.css";

type ResizeDirection =
  | "East"
  | "North"
  | "NorthEast"
  | "NorthWest"
  | "South"
  | "SouthEast"
  | "SouthWest"
  | "West";

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
};

type SearchPayload = {
  query: string;
  hits: SearchHit[];
};

type SearchWordRow = { id: number; word: string; reading?: string; posLabel?: string };

type SearchScopeRow = { path: string; label: string; isRoot: boolean };

type SearchScopesResult = {
  recent: SearchScopeRow[];
  scopes: SearchScopeRow[];
};

const SEARCH_DEBOUNCE_MS = 220;

/** Parent directory of a Windows / UNC file path. */
function parentDir(path: string): string | null {
  const normalized = path.replace(/\//g, "\\").replace(/\\+$/, "");
  const i = normalized.lastIndexOf("\\");
  if (i <= 0) return null;
  const parent = normalized.slice(0, i);
  if (/^[A-Za-z]:$/.test(parent)) return `${parent}\\`;
  if (!parent || parent === "\\") return null;
  return parent;
}

function scopeChipLabel(path: string, label?: string | null): string {
  if (label && label.trim()) return label.trim();
  const normalized = path.replace(/\//g, "\\").replace(/\\+$/, "");
  const base = normalized.split("\\").filter(Boolean).pop();
  return base || path;
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

function PreviewBody({
  hit,
  query,
}: {
  hit: SearchHit;
  query: string;
}) {
  const containerRef = useRef<HTMLDivElement>(null);
  const isMarkdown = isMarkdownPath(hit.path);
  const markdownHtml = useMemo(() => {
    if (!isMarkdown) return "";
    return renderMarkdownHtml(hit.previewText);
  }, [hit.previewText, isMarkdown]);
  const highlightTerms = useMemo(
    () => collectPreviewHighlightTerms(query, hit.highlightTerms),
    [query, hit.highlightTerms],
  );

  useEffect(() => {
    if (!isMarkdown) return;
    const el = containerRef.current;
    if (!el) return;
    applyPreviewHighlights(el, highlightTerms);
    return () => clearPreviewHighlights();
  }, [highlightTerms, isMarkdown, markdownHtml]);

  if (isMarkdown) {
    return (
      <div
        ref={containerRef}
        className="preview-body preview-body--markdown"
        dangerouslySetInnerHTML={{ __html: markdownHtml }}
      />
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
  const [searching, setSearching] = useState(false);
  const [actionError, setActionError] = useState("");
  const [wordPickerOpen, setWordPickerOpen] = useState(false);
  const [folderPickerOpen, setFolderPickerOpen] = useState(false);
  const [helpOpen, setHelpOpen] = useState(false);
  const [searchWords, setSearchWords] = useState<SearchWordRow[]>([]);
  const [searchScopes, setSearchScopes] = useState<SearchScopeRow[]>([]);
  const [recentScopes, setRecentScopes] = useState<SearchScopeRow[]>([]);
  const [scopeFilter, setScopeFilter] = useState("");
  const [scopePath, setScopePath] = useState<string | null>(null);
  const [scopeLabel, setScopeLabel] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const scopeFilterRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLUListElement>(null);
  const wordPickerRef = useRef<HTMLDivElement>(null);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const searchSeq = useRef(0);
  const scopePathRef = useRef<string | null>(null);

  useEffect(() => {
    scopePathRef.current = scopePath;
  }, [scopePath]);

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

  const runSearch = useCallback(async (q: string, pathPrefix?: string | null) => {
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
    try {
      const next = await invoke<SearchHit[]>("search_query", {
        query: trimmed,
        pathPrefix: prefix && prefix.trim() ? prefix.trim() : null,
      });
      if (seq !== searchSeq.current) return;
      setHits(next);
      setIndex(0);
      setPreview(null);
    } catch (e) {
      console.error(e);
    } finally {
      if (seq === searchSeq.current) {
        setSearching(false);
      }
    }
  }, []);

  const scheduleSearch = useCallback(
    (q: string, pathPrefix?: string | null) => {
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
    (filePath: string) => {
      const dir = parentDir(filePath);
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
      setQuery(event.payload.query);
      setHits(event.payload.hits);
      setIndex(0);
      setPreview(null);
      setActionError("");
      setSearching(false);
      setWordPickerOpen(false);
      setFolderPickerOpen(false);
      setHelpOpen(false);
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
  }, [clearScope]);

  useEffect(() => {
    if (!wordPickerOpen && !helpOpen && !folderPickerOpen) return;
    const onPointerDown = (e: MouseEvent) => {
      if (wordPickerRef.current?.contains(e.target as Node)) return;
      setWordPickerOpen(false);
      setFolderPickerOpen(false);
      setHelpOpen(false);
    };
    document.addEventListener("mousedown", onPointerDown);
    return () => document.removeEventListener("mousedown", onPointerDown);
  }, [wordPickerOpen, helpOpen, folderPickerOpen]);

  const openWordPicker = useCallback(async () => {
    try {
      const words = await invoke<SearchWordRow[]>("list_search_words");
      setSearchWords(words);
      setHelpOpen(false);
      setFolderPickerOpen(false);
      setWordPickerOpen(true);
    } catch (e) {
      console.error(e);
      setActionError(String(e));
    }
  }, []);

  const openFolderPicker = useCallback(async () => {
    try {
      const trimmed = query.trim();
      const result = await invoke<SearchScopesResult>("list_search_scopes", {
        query: trimmed || null,
      });
      setRecentScopes(result.recent ?? []);
      setSearchScopes(result.scopes ?? []);
      setHelpOpen(false);
      setWordPickerOpen(false);
      setScopeFilter("");
      setFolderPickerOpen(true);
      requestAnimationFrame(() => {
        scopeFilterRef.current?.focus();
      });
    } catch (e) {
      console.error(e);
      setActionError(String(e));
    }
  }, [query]);

  const toggleHelp = useCallback(() => {
    setWordPickerOpen(false);
    setFolderPickerOpen(false);
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

  const openSelected = useCallback(async () => {
    const hit = hits[index];
    if (!hit) return;
    setActionError("");
    try {
      await invoke("open_hit", { path: hit.path });
    } catch (e) {
      setActionError(String(e));
    }
  }, [hits, index]);

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

  const showPreview = useCallback(async () => {
    const hit = hits[index];
    if (!hit) return;
    setPreview(hit);
  }, [hits, index]);

  const hidePopup = useCallback(async () => {
    clearScope();
    setFolderPickerOpen(false);
    setWordPickerOpen(false);
    setHelpOpen(false);
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
        if (wordPickerOpen) {
          setWordPickerOpen(false);
          return;
        }
        if (helpOpen) {
          setHelpOpen(false);
          return;
        }
        if (preview) {
          setPreview(null);
          return;
        }
        void hidePopup();
        return;
      }
      if (wordPickerOpen || helpOpen || folderPickerOpen) return;
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
    folderPickerOpen,
    helpOpen,
    hidePopup,
    hits.length,
    openFolder,
    openSelected,
    preview,
    showPreview,
    wordPickerOpen,
  ]);

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
            className="popup-help-btn"
            title="検索構文のヒント"
            aria-label="検索構文のヒント"
            aria-expanded={helpOpen}
            onMouseDown={(e) => e.stopPropagation()}
            onClick={() => toggleHelp()}
          >
            ？
          </button>
          <button
            type="button"
            className="popup-word-add-btn"
            title="登録済み検索ワードを追加"
            aria-label="登録済み検索ワードを追加"
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
            ＋
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
            <div className="popup-word-picker" role="listbox" aria-label="登録済み検索ワード">
              <div className="popup-word-picker-actions">
                <button
                  type="button"
                  className="popup-word-register-btn"
                  onClick={() => void registerCurrentQueryWord()}
                  disabled={!query.trim()}
                >
                  入力中の語を辞書登録
                </button>
              </div>
              {searchWords.length === 0 ? (
                <div className="popup-word-empty">
                  登録済みの検索ワードはありません。上のボタンまたは設定の「検索ワード登録」から追加できます。
                </div>
              ) : (
                <ul>
                  {searchWords.map((w) => (
                    <li key={w.id}>
                      <button
                        type="button"
                        role="option"
                        onClick={() => appendSearchWord(w.word)}
                      >
                        {w.word}
                      </button>
                    </li>
                  ))}
                </ul>
              )}
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
        </div>
        {scopePath ? (
          <div className="popup-scope-chip-row">
            <span className="popup-scope-chip" title={scopePath}>
              <span className="popup-scope-chip-at">@</span>
              {scopeChipLabel(scopePath, scopeLabel)}
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
          </div>
        ) : null}
      </header>

      {preview ? (
        <section className="preview">
          <div className="preview-title">{preview.title}</div>
          <div className="preview-path">{preview.path}</div>
          <div className="preview-actions">
            <button type="button" onClick={() => void openSelected()}>
              ファイルを開く
            </button>
            <button type="button" onClick={() => void openFolder(preview.path)}>
              フォルダを開く
            </button>
            <button
              type="button"
              className="preview-rescope-btn"
              title="このフォルダ内で再検索"
              aria-label="このフォルダ内で再検索"
              onClick={() => rescopeToHitFolder(preview.path)}
            >
              🔍
            </button>
          </div>
          <PreviewBody hit={preview} query={query} />
          <div className="hint">Esc で一覧に戻る</div>
        </section>
      ) : (
        <ul className="hit-list" ref={listRef}>
          {hits.length === 0 ? (
            <li className="empty">
              {query.trim()
                ? "結果がありません。"
                : "検索文字列を入力するか、文書上で選択してショートカットを押してください。"}
            </li>
          ) : (
            hits.map((hit, i) => (
              <li
                key={`${hit.source}-${hit.id}`}
                className={i === index ? "hit active" : "hit"}
                onMouseEnter={() => setIndex(i)}
                onDoubleClick={() => void openSelected()}
              >
                <div className="hit-main">
                  <div className="hit-title">
                    {hit.source === "remote" ? (
                      <span className="hit-source" title="リモート">
                        リモート
                      </span>
                    ) : null}
                    {(() => {
                      const ext = extFromPath(hit.path);
                      return ext ? (
                        <span className="hit-ext" title={hit.path}>
                          {ext}
                        </span>
                      ) : null;
                    })()}
                    📄 {highlight(hit.title, query, hit.highlightTerms)}
                  </div>
                  <div className="hit-snippet">
                    {highlight(hit.snippet, query, hit.highlightTerms)}
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
                  <button
                    type="button"
                    className="hit-folder-btn"
                    title="このフォルダ内で再検索"
                    aria-label="このフォルダ内で再検索"
                    onClick={(e) => {
                      e.stopPropagation();
                      rescopeToHitFolder(hit.path);
                    }}
                  >
                    🔍
                  </button>
                  <button
                    type="button"
                    className="hit-folder-btn"
                    title="フォルダを開く (Shift+Enter)"
                    onClick={(e) => {
                      e.stopPropagation();
                      void openFolder(hit.path);
                    }}
                  >
                    📁
                  </button>
                </div>
              </li>
            ))
          )}
        </ul>
      )}

      {actionError ? <div className="popup-error">{actionError}</div> : null}

      <footer className="popup-footer">
        <span>↑↓ 移動</span>
        <span>Enter 開く</span>
        <span>Shift+Enter フォルダ</span>
        <span>Ctrl+Enter プレビュー</span>
        <span>Esc / 外クリック 閉じる</span>
      </footer>
    </div>
  );
}
