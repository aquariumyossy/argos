import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

const MAX_SCOPES = 8;

export type ChatScopeThread = {
  id: string;
  title: string;
  pathPrefix?: string;
  createdAt: number;
  updatedAt: number;
};

type SearchScopeRow = { path: string; label: string; isRoot: boolean };

type SearchScopesResult = { recent: SearchScopeRow[]; scopes: SearchScopeRow[] };

function formatMailScopeLabel(raw: string): string {
  const parts = raw
    .split(/[\\/]/)
    .map((p) => p.trim())
    .filter(Boolean);
  if (parts.length <= 1) return raw.trim();
  const store = parts[0];
  return `${parts.slice(1).join("／")}（${store}）`;
}

function scopeChipLabel(path: string, label?: string | null): string {
  if (label && label.trim()) return label.trim();
  if (path.startsWith("mailfolder:")) {
    return formatMailScopeLabel(path.slice("mailfolder:".length));
  }
  const normalized = path.replace(/\//g, "\\").replace(/\\+$/, "");
  return normalized.split("\\").filter(Boolean).pop() || path;
}

export function parseScopes(pathPrefix: string | undefined | null): string[] {
  if (!pathPrefix) return [];
  return pathPrefix
    .split("\n")
    .map((s) => s.trim())
    .filter(Boolean);
}

function pathStartsWith(path: string, prefix: string): boolean {
  const a = path.replace(/\//g, "\\").replace(/\\+$/, "").toLowerCase();
  const b = prefix.replace(/\//g, "\\").replace(/\\+$/, "").toLowerCase();
  if (!b) return true;
  if (a === b) return true;
  return a.startsWith(`${b}\\`);
}

function collapseScopes(paths: string[]): string[] {
  const out: string[] = [];
  for (const raw of paths) {
    const p = raw.trim();
    if (!p) continue;
    if (out.some((kept) => pathStartsWith(p, kept))) continue;
    for (let i = out.length - 1; i >= 0; i--) {
      if (pathStartsWith(out[i]!, p)) out.splice(i, 1);
    }
    out.push(p);
    if (out.length >= MAX_SCOPES) break;
  }
  return out;
}

function samePath(a: string, b: string): boolean {
  return a.replace(/\//g, "\\").toLowerCase() === b.replace(/\//g, "\\").toLowerCase();
}

export function scopeButtonLabel(pathPrefix: string | undefined | null): string {
  const paths = parseScopes(pathPrefix);
  if (paths.length === 0) return "検索範囲";
  if (paths.length === 1) return `範囲: ${scopeChipLabel(paths[0]!)}`;
  return `範囲: ${scopeChipLabel(paths[0]!)} ほか${paths.length - 1}件`;
}

function formatInvokeError(e: unknown): string {
  if (e instanceof Error) return e.message;
  if (typeof e === "string") return e;
  if (e && typeof e === "object" && "message" in e) {
    return String((e as { message: unknown }).message);
  }
  try {
    return JSON.stringify(e);
  } catch {
    return String(e);
  }
}

type Props = {
  disabled: boolean;
  threadId: string | null;
  pathPrefix: string;
  onApplied: (thread: ChatScopeThread) => void;
  onError: (message: string) => void;
};

export default function ChatScopePicker({
  disabled,
  threadId,
  pathPrefix,
  onApplied,
  onError,
}: Props) {
  const [open, setOpen] = useState(false);
  const [loading, setLoading] = useState(false);
  const [scopeRows, setScopeRows] = useState<SearchScopeRow[]>([]);
  const [recentRows, setRecentRows] = useState<SearchScopeRow[]>([]);
  const [scopeFilter, setScopeFilter] = useState("");
  const [pending, setPending] = useState<string[]>([]);
  const wrapRef = useRef<HTMLDivElement | null>(null);
  const filterRef = useRef<HTMLInputElement | null>(null);
  const genRef = useRef(0);

  const applied = useMemo(() => parseScopes(pathPrefix), [pathPrefix]);

  const close = useCallback(() => {
    genRef.current += 1;
    setOpen(false);
    setLoading(false);
    setScopeFilter("");
  }, []);

  const loadScopes = useCallback(async () => {
    const gen = ++genRef.current;
    setLoading(true);
    try {
      const result = await invoke<SearchScopesResult>("list_search_scopes", {
        query: null,
      });
      if (gen !== genRef.current) return;
      const recent = result.recent ?? [];
      let rows = result.scopes ?? [];
      try {
        const mailNames = await invoke<string[]>("mail_list_selected_folder_names");
        const mailRows: SearchScopeRow[] = (mailNames ?? []).map((name) => ({
          path: `mailfolder:${name}`,
          label: formatMailScopeLabel(name),
          isRoot: true,
        }));
        rows = [
          ...rows,
          ...mailRows.filter(
            (m) =>
              !rows.some((s) => samePath(s.path, m.path)) &&
              !recent.some((s) => samePath(s.path, m.path)),
          ),
        ];
      } catch {
        // Outlook mail not configured — file scopes are enough.
      }
      if (gen !== genRef.current) return;
      setRecentRows(recent);
      setScopeRows(rows);
    } catch (e) {
      if (gen !== genRef.current) return;
      onError(formatInvokeError(e));
      close();
    } finally {
      if (gen === genRef.current) setLoading(false);
    }
  }, [close, onError]);

  const openPicker = useCallback(() => {
    setPending(parseScopes(pathPrefix));
    setScopeFilter("");
    setOpen(true);
    void loadScopes();
    requestAnimationFrame(() => {
      filterRef.current?.focus();
    });
  }, [loadScopes, pathPrefix]);

  useEffect(() => {
    close();
  }, [threadId, close]);

  useEffect(() => {
    if (!open) return;
    const onPointerDown = (e: MouseEvent) => {
      const el = wrapRef.current;
      if (!el) return;
      if (e.target instanceof Node && el.contains(e.target)) return;
      close();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        close();
      }
    };
    document.addEventListener("mousedown", onPointerDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onPointerDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open, close]);

  const filteredRecent = useMemo(() => {
    const q = scopeFilter.trim().toLowerCase();
    if (!q) return recentRows;
    return recentRows.filter(
      (s) =>
        s.label.toLowerCase().includes(q) || s.path.toLowerCase().includes(q),
    );
  }, [recentRows, scopeFilter]);

  const filteredScopes = useMemo(() => {
    const q = scopeFilter.trim().toLowerCase();
    if (!q) return scopeRows;
    return scopeRows.filter(
      (s) =>
        s.label.toLowerCase().includes(q) || s.path.toLowerCase().includes(q),
    );
  }, [scopeRows, scopeFilter]);

  const collapsedPending = useMemo(() => collapseScopes(pending), [pending]);
  const atLimit = collapsedPending.length >= MAX_SCOPES;

  const isSelected = useCallback(
    (path: string) => pending.some((p) => samePath(p, path)),
    [pending],
  );

  const toggle = useCallback((path: string) => {
    setPending((prev) => {
      if (prev.some((p) => samePath(p, path))) {
        return prev.filter((p) => !samePath(p, path));
      }
      if (prev.some((p) => pathStartsWith(path, p))) {
        return prev;
      }
      const next = collapseScopes([...prev, path]);
      if (next.length > MAX_SCOPES) return prev;
      return next;
    });
  }, []);

  const apply = useCallback(
    async (paths: string[]) => {
      const id = threadId;
      if (!id) return;
      const collapsed = collapseScopes(paths).slice(0, MAX_SCOPES);
      close();
      try {
        const t = await invoke<ChatScopeThread>("llm_set_thread_scope", {
          id,
          pathPrefixes: collapsed,
        });
        onApplied(t);
        const labels = new Map<string, string>();
        for (const row of [...recentRows, ...scopeRows]) {
          labels.set(row.path.toLowerCase(), row.label);
        }
        for (const path of collapsed) {
          const label = labels.get(path.toLowerCase()) ?? scopeChipLabel(path);
          void invoke("push_recent_search_scope", {
            path,
            label,
          }).catch(() => {
            /* recent is optional */
          });
        }
      } catch (e) {
        onError(formatInvokeError(e));
      }
    },
    [close, onApplied, onError, recentRows, scopeRows, threadId],
  );

  const empty =
    !loading &&
    filteredScopes.length === 0 &&
    filteredRecent.length === 0;

  return (
    <div className="chat-scope" ref={wrapRef}>
      <button
        type="button"
        className={`chat-tpl${applied.length > 0 ? " is-active" : ""}`}
        disabled={disabled}
        title="この会話の検索対象フォルダを絞る"
        aria-expanded={open}
        onClick={() => {
          if (open) close();
          else openPicker();
        }}
      >
        {scopeButtonLabel(pathPrefix)}
      </button>
      {applied.length > 0 ? (
        <button
          type="button"
          className="chat-scope-clear"
          title="検索範囲を解除"
          aria-label="検索範囲を解除"
          disabled={disabled}
          onClick={() => void apply([])}
        >
          ×
        </button>
      ) : null}
      {open ? (
        <div
          className="chat-scope-picker"
          role="listbox"
          aria-multiselectable="true"
          aria-label="検索対象フォルダ"
        >
          <div className="chat-scope-picker-head">
            <span className="chat-scope-picker-head-label">フォルダ</span>
            <button
              type="button"
              className="chat-scope-picker-close"
              title="閉じる (Esc)"
              aria-label="閉じる"
              onMouseDown={(e) => {
                e.preventDefault();
                e.stopPropagation();
              }}
              onClick={() => close()}
            >
              ×
            </button>
          </div>
          <input
            ref={filterRef}
            className="chat-scope-filter"
            value={scopeFilter}
            placeholder="フォルダ名で絞り込み…"
            spellCheck={false}
            onMouseDown={(e) => e.stopPropagation()}
            onChange={(e) => setScopeFilter(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") e.preventDefault();
            }}
          />
          <div className="chat-scope-picker-list">
            {loading ? (
              <div className="chat-scope-empty">読み込み中…</div>
            ) : empty ? (
              <div className="chat-scope-empty">
                {scopeRows.length === 0 && recentRows.length === 0
                  ? "検索対象フォルダがありません。設定からフォルダを追加してください。"
                  : "一致するフォルダがありません。"}
              </div>
            ) : (
              <ul>
                <li>
                  <button
                    type="button"
                    role="option"
                    aria-selected={collapsedPending.length === 0}
                    onClick={() => void apply([])}
                  >
                    <span className="chat-scope-check" aria-hidden="true">
                      {collapsedPending.length === 0 ? "✓" : ""}
                    </span>
                    <span className="chat-scope-kind">全体</span>
                    <span className="chat-scope-label">索引全体</span>
                  </button>
                </li>
                {filteredRecent.map((s) => (
                  <li key={`recent:${s.path}`}>
                    <button
                      type="button"
                      role="option"
                      aria-selected={isSelected(s.path)}
                      className="scope-recent"
                      title={s.path}
                      onClick={() => toggle(s.path)}
                    >
                      <span className="chat-scope-check" aria-hidden="true">
                        {isSelected(s.path) ? "✓" : ""}
                      </span>
                      <span className="chat-scope-kind">直近</span>
                      <span className="chat-scope-label">{s.label}</span>
                    </button>
                  </li>
                ))}
                {filteredRecent.length > 0 && filteredScopes.length > 0 ? (
                  <li className="chat-scope-sep" aria-hidden="true" />
                ) : null}
                {filteredScopes.map((s) => {
                  const selected = isSelected(s.path);
                  const blocked =
                    !selected &&
                    (atLimit || pending.some((p) => pathStartsWith(s.path, p)));
                  return (
                    <li key={s.path}>
                      <button
                        type="button"
                        role="option"
                        aria-selected={selected}
                        className={s.isRoot ? "scope-root" : "scope-sub"}
                        title={
                          blocked && atLimit
                            ? `検索範囲は最大 ${MAX_SCOPES} 件です`
                            : s.path
                        }
                        disabled={blocked}
                        onClick={() => toggle(s.path)}
                      >
                        <span className="chat-scope-check" aria-hidden="true">
                          {selected ? "✓" : ""}
                        </span>
                        {s.isRoot ? (
                          <span className="chat-scope-kind">ルート</span>
                        ) : null}
                        <span className="chat-scope-label">{s.label}</span>
                      </button>
                    </li>
                  );
                })}
              </ul>
            )}
          </div>
          <div className="chat-scope-picker-actions">
            <span className="chat-scope-picker-count">
              {collapsedPending.length === 0
                ? "索引全体"
                : `${collapsedPending.length} 件選択`}
            </span>
            <button
              type="button"
              className="chat-scope-apply"
              onClick={() => void apply(pending)}
            >
              適用
            </button>
          </div>
        </div>
      ) : null}
    </div>
  );
}
