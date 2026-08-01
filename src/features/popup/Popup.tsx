import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
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

type SearchWordRow = { id: number; word: string };

const SEARCH_DEBOUNCE_MS = 220;

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
  const fromQuery = highlightTermsFromQuery(query);
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

export default function Popup() {
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<SearchHit[]>([]);
  const [index, setIndex] = useState(0);
  const [preview, setPreview] = useState<SearchHit | null>(null);
  const [searching, setSearching] = useState(false);
  const [actionError, setActionError] = useState("");
  const [wordPickerOpen, setWordPickerOpen] = useState(false);
  const [helpOpen, setHelpOpen] = useState(false);
  const [searchWords, setSearchWords] = useState<SearchWordRow[]>([]);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLUListElement>(null);
  const wordPickerRef = useRef<HTMLDivElement>(null);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const searchSeq = useRef(0);

  const runSearch = useCallback(async (q: string) => {
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
    try {
      const next = await invoke<SearchHit[]>("search_query", { query: trimmed });
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
    (q: string) => {
      if (debounceRef.current) clearTimeout(debounceRef.current);
      debounceRef.current = setTimeout(() => {
        void runSearch(q);
      }, SEARCH_DEBOUNCE_MS);
    },
    [runSearch],
  );

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    listen<SearchPayload>("search-results", (event) => {
      if (debounceRef.current) clearTimeout(debounceRef.current);
      searchSeq.current += 1;
      setQuery(event.payload.query);
      setHits(event.payload.hits);
      setIndex(0);
      setPreview(null);
      setActionError("");
      setSearching(false);
      setWordPickerOpen(false);
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
  }, []);

  useEffect(() => {
    if (!wordPickerOpen && !helpOpen) return;
    const onPointerDown = (e: MouseEvent) => {
      if (wordPickerRef.current?.contains(e.target as Node)) return;
      setWordPickerOpen(false);
      setHelpOpen(false);
    };
    document.addEventListener("mousedown", onPointerDown);
    return () => document.removeEventListener("mousedown", onPointerDown);
  }, [wordPickerOpen, helpOpen]);

  const openWordPicker = useCallback(async () => {
    try {
      const words = await invoke<SearchWordRow[]>("list_search_words");
      setSearchWords(words);
      setHelpOpen(false);
      setWordPickerOpen(true);
    } catch (e) {
      console.error(e);
      setActionError(String(e));
    }
  }, []);

  const toggleHelp = useCallback(() => {
    setWordPickerOpen(false);
    setHelpOpen((v) => !v);
  }, []);

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

  useEffect(() => {
    if (preview) return;
    const active = listRef.current?.querySelector<HTMLElement>(".hit.active");
    active?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }, [index, hits, preview]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
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
        void invoke("hide_popup");
        return;
      }
      if (wordPickerOpen || helpOpen) return;
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
  }, [helpOpen, hits.length, openFolder, openSelected, preview, showPreview, wordPickerOpen]);

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
              {searchWords.length === 0 ? (
                <div className="popup-word-empty">
                  登録済みの検索ワードはありません。設定の「検索ワード登録」から追加できます。
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
        </div>
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
          </div>
          <pre className="preview-body">
            {highlight(preview.previewText, query, preview.highlightTerms)}
          </pre>
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
