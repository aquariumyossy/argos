import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import ChatScopePicker from "./ChatScopePicker";
import { AssistantBody } from "./AssistantBody";
import { openPreview } from "../preview/openPreview";
import "./chat.css";

type LlmThreadRow = {
  id: string;
  title: string;
  /** Folder scope for index searches in this thread. Empty means the whole index. */
  pathPrefix?: string;
  createdAt: number;
  updatedAt: number;
};

type LlmMessageRow = {
  id: string;
  threadId: string;
  role: string;
  content: string;
  createdAt: number;
};

type LlmSourceRow = {
  id: string;
  threadId: string;
  sortOrder: number;
  origin: string;
  path: string;
  title: string;
  paragraphId: string;
  body: string;
  query: string;
  createdAt: number;
  grain?: string;
  injectedUserMessageId?: string;
  citedAssistantMessageId?: string;
  citeNo?: number;
};

type LlmFilePreview = {
  chars: number;
  units: number;
  hardCap: number;
};

type LlmGrainResult = {
  source: LlmSourceRow;
  sources: LlmSourceRow[];
  removed: number;
};

type SettingsData = {
  fontSize: number;
  llmMaxContextChars?: number;
};

type LlmSendResult = {
  thread: LlmThreadRow;
  userMessage: LlmMessageRow;
  assistantMessage: LlmMessageRow | null;
  cancelled: boolean;
  error: string | null;
  truncated?: boolean;
  contextChars?: number;
  warning?: string | null;
};

type LlmChatDelta = {
  requestId: string;
  threadId: string;
  text: string;
  kind?: string;
};

const TPL_SUMMARY =
  "添付した出典を日本語で要約してください。重要な結論と根拠を短く。";
const TPL_POINTS =
  "争点、結論、根拠を箇条書きで整理してください。根拠には出典番号 [n] を付けてください。";
const TPL_FACTS =
  "添付出典だけを根拠に、要件事実の関係を mermaid の flowchart LR でフェンス1本にまとめてください。左から右へ、請求原因 → 抗弁 → 再抗弁。請求原因はノードを1つだけにし、複数の主要事実は同じノード内に番号で書いてください。抗弁は種類ごとに分け、その右に対応する再抗弁を置いてください。ノードは短く。ラベルは A[\"請求原因 [n]\"] のように二重引用符で囲んでください。矢印は --> または -.-> で、途中に空白を入れないでください。添付出典にない事実はノードにしないでください。";
const TPL_TIMELINE =
  "添付出典に書かれた出来事だけを時系列で整理してください。図にする場合は mermaid の flowchart LR または timeline をフェンス1本にしてください。ノードラベルは二重引用符で囲み、出典番号 [n] をラベル内に書いてください。出典にない出来事は入れないでください。";

function sleep(ms: number) {
  return new Promise<void>((resolve) => setTimeout(resolve, ms));
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

async function waitReady(maxAttempts = 50): Promise<void> {
  for (let attempt = 0; attempt < maxAttempts; attempt++) {
    try {
      const ready = await invoke<boolean>("is_app_ready");
      if (ready) return;
    } catch {
      /* still starting */
    }
    await sleep(Math.min(50 * 2 ** Math.min(attempt, 4), 1000));
  }
}

function sourceLabel(s: LlmSourceRow): string {
  const t = s.title.trim();
  if (t) return t;
  const p = s.path.trim();
  if (p) return p;
  return "出典";
}

function isFileGrain(s: LlmSourceRow): boolean {
  return (s.grain ?? "unit").toLowerCase() === "file";
}

function sourceCanExpand(s: LlmSourceRow): boolean {
  return s.path.trim().length > 0;
}

function isPendingSource(s: LlmSourceRow): boolean {
  const injected = (s.injectedUserMessageId ?? "").trim();
  if (injected) return false;
  return (s.origin ?? "attach").toLowerCase() !== "tool";
}

function isUncitedToolSource(s: LlmSourceRow): boolean {
  const injected = (s.injectedUserMessageId ?? "").trim();
  return !injected && (s.origin ?? "").toLowerCase() === "tool";
}

function citeNoOf(s: LlmSourceRow, fallback: number): number {
  return s.citeNo && s.citeNo > 0 ? s.citeNo : fallback;
}

function openableCites(
  rows: LlmSourceRow[],
  fallbackStart: number,
): { nos: Set<number>; byNo: Map<number, LlmSourceRow> } {
  const nos = new Set<number>();
  const byNo = new Map<number, LlmSourceRow>();
  rows.forEach((s, i) => {
    const n = citeNoOf(s, fallbackStart + i);
    byNo.set(n, s);
    if (s.path.trim()) nos.add(n);
  });
  return { nos, byNo };
}

function citedForMessage(
  sources: LlmSourceRow[],
  messageId: string,
): LlmSourceRow[] {
  return sources
    .filter((s) => (s.citedAssistantMessageId ?? "") === messageId)
    .sort((a, b) => (a.citeNo ?? 0) - (b.citeNo ?? 0) || a.sortOrder - b.sortOrder);
}

function IconOpenFile() {
  return (
    <svg width="14" height="14" viewBox="0 0 24 24" aria-hidden="true">
      <path
        fill="currentColor"
        d="M14 3h7v7h-2V6.41l-9.29 9.3-1.42-1.42 9.3-9.29H14V3zM5 5h6v2H7v10h10v-4h2v6H5V5z"
      />
    </svg>
  );
}

function countChars(s: string): number {
  return [...s].length;
}

export default function Chat() {
  const [threads, setThreads] = useState<LlmThreadRow[]>([]);
  const [active, setActive] = useState<LlmThreadRow | null>(null);
  const [messages, setMessages] = useState<LlmMessageRow[]>([]);
  const [sources, setSources] = useState<LlmSourceRow[]>([]);
  const [draft, setDraft] = useState("");
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [busy, setBusy] = useState(false);
  const [stream, setStream] = useState("");
  const [thinking, setThinking] = useState("");
  const [toolHint, setToolHint] = useState("");
  const [fontSize, setFontSize] = useState(14);
  const [maxContextChars, setMaxContextChars] = useState(80_000);
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState("");
  const listRef = useRef<HTMLDivElement | null>(null);
  const bootstrappingRef = useRef(false);
  const activeIdRef = useRef<string | null>(null);
  const busyRef = useRef(false);
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);

  useEffect(() => {
    activeIdRef.current = active?.id ?? null;
  }, [active]);

  useEffect(() => {
    busyRef.current = busy;
  }, [busy]);

  const loadThreadContent = useCallback(async (thread: LlmThreadRow | null) => {
    if (!thread) {
      setMessages([]);
      setSources([]);
      return;
    }
    const [msgs, srcs] = await Promise.all([
      invoke<LlmMessageRow[]>("llm_list_messages", { threadId: thread.id }),
      invoke<LlmSourceRow[]>("llm_list_sources", { threadId: thread.id }),
    ]);
    setMessages(msgs);
    setSources(srcs);
  }, []);

  const loadThreads = useCallback(async () => {
    const list = await invoke<LlmThreadRow[]>("llm_list_threads");
    setThreads(list);
    let current = await invoke<LlmThreadRow | null>("llm_get_active_thread");
    if (!current && list.length > 0) {
      current = await invoke<LlmThreadRow>("llm_set_active_thread", {
        id: list[0].id,
      });
    }
    setActive(current);
    await loadThreadContent(current);
  }, [loadThreadContent]);

  const bootstrap = useCallback(async () => {
    if (bootstrappingRef.current) return;
    bootstrappingRef.current = true;
    try {
      await waitReady();
      try {
        const s = await invoke<SettingsData>("get_settings");
        if (typeof s.fontSize === "number" && s.fontSize > 0) {
          setFontSize(s.fontSize);
        }
        if (
          typeof s.llmMaxContextChars === "number" &&
          s.llmMaxContextChars > 0
        ) {
          setMaxContextChars(s.llmMaxContextChars);
        }
      } catch {
        /* keep default */
      }
      await loadThreads();
      setError("");
    } catch (e) {
      setError(formatInvokeError(e));
    } finally {
      bootstrappingRef.current = false;
    }
  }, [loadThreads]);

  useEffect(() => {
    void bootstrap();
  }, [bootstrap]);

  useEffect(() => {
    const win = getCurrentWindow();
    let cancelled = false;
    let unlistenFocus: (() => void) | undefined;
    let unlistenReady: (() => void) | undefined;
    void win
      .onFocusChanged((event) => {
        if (event.payload && !busyRef.current) void bootstrap();
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlistenFocus = fn;
      });
    void listen("argos-ready", () => {
      void bootstrap();
    }).then((fn) => {
      if (cancelled) fn();
      else unlistenReady = fn;
    });
    return () => {
      cancelled = true;
      unlistenFocus?.();
      unlistenReady?.();
    };
  }, [bootstrap]);

  useEffect(() => {
    let unlistenDelta: (() => void) | undefined;
    let unlistenError: (() => void) | undefined;
    let unlistenSources: (() => void) | undefined;
    void listen<LlmChatDelta>("llm-chat-delta", (event) => {
      if (event.payload.threadId !== activeIdRef.current) return;
      const kind = event.payload.kind ?? "content";
      if (kind === "reasoning") {
        setThinking((prev) => prev + event.payload.text);
      } else if (kind === "tool") {
        setToolHint(event.payload.text || "インデックスを検索しています…");
      } else {
        setStream((prev) => prev + event.payload.text);
      }
    }).then((fn) => {
      unlistenDelta = fn;
    });
    void listen<{ threadId: string; message: string }>("llm-chat-error", (event) => {
      if (event.payload.threadId !== activeIdRef.current) return;
      setError(event.payload.message);
    }).then((fn) => {
      unlistenError = fn;
    });
    void listen<{ threadId: string }>("llm-sources-updated", (event) => {
      const tid = event.payload.threadId;
      if (busyRef.current) {
        if (tid === activeIdRef.current) {
          void invoke<LlmSourceRow[]>("llm_list_sources", { threadId: tid })
            .then(setSources)
            .catch((e) => setError(formatInvokeError(e)));
        }
        return;
      }
      const prev = activeIdRef.current;
      void loadThreads()
        .then(() => {
          if (tid !== prev) {
            setDraft("");
            setStream("");
            setThinking("");
            setToolHint("");
            setError("");
          }
        })
        .catch((e) => setError(formatInvokeError(e)));
    }).then((fn) => {
      unlistenSources = fn;
    });
    return () => {
      unlistenDelta?.();
      unlistenError?.();
      unlistenSources?.();
    };
  }, [loadThreads]);

  const scrollLog = useCallback(() => {
    const el = listRef.current;
    if (!el) return;
    el.scrollTop = el.scrollHeight;
  }, []);

  useEffect(() => {
    scrollLog();
  }, [messages, stream, thinking, toolHint, busy, scrollLog]);

  const estimatedChars = useMemo(() => {
    const src = sources.reduce(
      (n, s) => n + countChars(s.body) + countChars(s.title) + 24,
      0,
    );
    const hist = messages.reduce((n, m) => n + countChars(m.content), 0);
    return src + hist + countChars(draft);
  }, [sources, messages, draft]);

  const pendingSources = useMemo(
    () => sources.filter(isPendingSource),
    [sources],
  );

  const liveCites = useMemo(() => {
    const tools = sources.filter(isUncitedToolSource);
    return [...pendingSources, ...tools];
  }, [pendingSources, sources]);

  const maxCitedNo = useMemo(
    () =>
      sources
        .filter((s) => (s.injectedUserMessageId ?? "").trim())
        .reduce((n, s) => Math.max(n, s.citeNo ?? 0), 0),
    [sources],
  );

  const liveCiteInfo = useMemo(
    () => openableCites(liveCites, maxCitedNo + 1),
    [liveCites, maxCitedNo],
  );

  const overBudget = estimatedChars > maxContextChars;

  async function selectThread(id: string) {
    if (busy) return;
    try {
      const t = await invoke<LlmThreadRow>("llm_set_active_thread", { id });
      setActive(t);
      await loadThreadContent(t);
      setStream("");
      setThinking("");
      setToolHint("");
      setError("");
      setNotice("");
    } catch (e) {
      setError(formatInvokeError(e));
    }
  }

  async function createThread() {
    if (busy) return;
    try {
      const t = await invoke<LlmThreadRow>("llm_create_thread", { title: "" });
      setThreads((prev) => [t, ...prev.filter((x) => x.id !== t.id)]);
      setActive(t);
      setMessages([]);
      setSources([]);
      setStream("");
      setThinking("");
      setToolHint("");
      setError("");
      setNotice("");
      textareaRef.current?.focus();
    } catch (e) {
      setError(formatInvokeError(e));
    }
  }

  async function deleteThread(id: string) {
    if (busy) return;
    try {
      await invoke("llm_delete_thread", { id });
      await loadThreads();
      setStream("");
      setThinking("");
    } catch (e) {
      setError(formatInvokeError(e));
    }
  }

  async function commitRename(id: string) {
    const title = renameDraft.trim();
    setRenamingId(null);
    try {
      const t = await invoke<LlmThreadRow>("llm_rename_thread", { id, title });
      setThreads((prev) => prev.map((x) => (x.id === id ? t : x)));
      if (active?.id === id) setActive(t);
    } catch (e) {
      setError(formatInvokeError(e));
    }
  }

  const applyThreadScope = useCallback((t: LlmThreadRow) => {
    setThreads((prev) => prev.map((x) => (x.id === t.id ? t : x)));
    setActive((prev) => (prev?.id === t.id ? t : prev));
  }, []);

  async function openSource(s: LlmSourceRow) {
    const path = s.path.trim();
    if (!path) return;
    try {
      await openPreview({
        origin: "chat",
        path,
        paragraphId: s.paragraphId,
        query: s.query,
        title: s.title,
        fallbackBody: s.body,
      });
    } catch (e) {
      setError(formatInvokeError(e));
    }
  }

  async function removeSource(id: string) {
    try {
      await invoke("llm_remove_source", { id });
      setSources((prev) => prev.filter((s) => s.id !== id));
    } catch (e) {
      setError(formatInvokeError(e));
    }
  }

  async function toggleSourceGrain(s: LlmSourceRow) {
    if (busy || !sourceCanExpand(s)) return;
    setError("");
    setNotice("");
    try {
      if (isFileGrain(s)) {
        const result = await invoke<LlmGrainResult>("llm_set_source_grain", {
          id: s.id,
          grain: "unit",
        });
        setSources(result.sources);
        return;
      }
      const preview = await invoke<LlmFilePreview>("llm_preview_source_file", {
        id: s.id,
      });
      if (preview.chars > maxContextChars) {
        const ok = window.confirm(
          `このファイルは約 ${preview.chars.toLocaleString()} 文字です。送信の上限は ${maxContextChars.toLocaleString()} 文字です。全文にしますか？`,
        );
        if (!ok) return;
      }
      const result = await invoke<LlmGrainResult>("llm_set_source_grain", {
        id: s.id,
        grain: "file",
      });
      setSources(result.sources);
      if (result.removed > 0) {
        setNotice(
          result.removed === 1
            ? "同じファイルの他の段落を外して全文にしました。"
            : `同じファイルの段落 ${result.removed} 件を外して全文にしました。`,
        );
      }
    } catch (e) {
      setError(formatInvokeError(e));
    }
  }

  function applyTemplate(text: string) {
    setDraft(text);
    requestAnimationFrame(() => {
      const el = textareaRef.current;
      if (!el) return;
      el.focus();
      el.selectionStart = el.selectionEnd = text.length;
    });
  }

  async function send() {
    const text = draft.trim();
    if (!text || busy) return;
    setBusy(true);
    setError("");
    setNotice("");
    setDraft("");
    setStream("");
    setThinking("");
    setToolHint("");
    let thread: LlmThreadRow;
    try {
      thread =
        active ?? (await invoke<LlmThreadRow>("llm_create_thread", { title: "" }));
      if (!active) {
        setActive(thread);
        activeIdRef.current = thread.id;
        const created = thread;
        setThreads((prev) => [created, ...prev.filter((x) => x.id !== created.id)]);
      }
    } catch (e) {
      setBusy(false);
      setDraft(text);
      setError(formatInvokeError(e));
      return;
    }
    const optimistic: LlmMessageRow = {
      id: `tmp-${Date.now()}`,
      threadId: thread.id,
      role: "user",
      content: text,
      createdAt: Math.floor(Date.now() / 1000),
    };
    setMessages((prev) => [...prev, optimistic]);
    try {
      const result = await invoke<LlmSendResult>("llm_send", {
        threadId: thread.id,
        content: text,
      });
      setActive(result.thread);
      setThreads((prev) => {
        const rest = prev.filter((x) => x.id !== result.thread.id);
        return [result.thread, ...rest];
      });
      const msgs = await invoke<LlmMessageRow[]>("llm_list_messages", {
        threadId: result.thread.id,
      });
      const srcs = await invoke<LlmSourceRow[]>("llm_list_sources", {
        threadId: result.thread.id,
      });
      setMessages(msgs);
      setSources(srcs);
      setStream("");
      setThinking("");
      setToolHint("");
      if (result.cancelled) {
        setError("生成を停止しました。");
      } else if (result.error) {
        setError(result.error);
      } else if (result.warning) {
        setNotice(result.warning);
      } else if (result.truncated) {
        setNotice("文字数上限のため、古い出典から切り詰めて送りました。");
      }
    } catch (e) {
      setStream("");
      setThinking("");
      setToolHint("");
      const msg = formatInvokeError(e);
      if (msg.includes("生成中です")) {
        try {
          await invoke("llm_cancel");
        } catch {
          /* ignore */
        }
        setDraft(text);
        setError(
          "前の生成が途中で止まっていました。停止したので、もう一度送信してください。",
        );
      } else {
        setError(msg);
      }
      try {
        const msgs = await invoke<LlmMessageRow[]>("llm_list_messages", {
          threadId: thread.id,
        });
        setMessages(msgs);
      } catch {
        /* keep optimistic */
      }
    } finally {
      setBusy(false);
    }
  }

  async function stop() {
    try {
      await invoke("llm_cancel");
    } catch (e) {
      setError(formatInvokeError(e));
    }
  }

  return (
    <div className="chat" style={{ fontSize: `${fontSize}px` }}>
      <aside className="chat-sidebar">
        <header className="chat-sidebar-head">
          <h1>チャット</h1>
          <button
            type="button"
            className="chat-btn primary"
            onClick={() => void createThread()}
            disabled={busy}
          >
            新規
          </button>
        </header>
        {threads.length === 0 ? (
          <p className="chat-muted chat-sidebar-empty">会話はまだありません。</p>
        ) : (
          <ul className="chat-thread-list">
            {threads.map((t) => (
              <li
                key={t.id}
                className={
                  t.id === active?.id ? "chat-thread-item active" : "chat-thread-item"
                }
              >
                {renamingId === t.id ? (
                  <input
                    className="chat-rename"
                    value={renameDraft}
                    autoFocus
                    onChange={(e) => setRenameDraft(e.target.value)}
                    onBlur={() => void commitRename(t.id)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter") {
                        e.preventDefault();
                        void commitRename(t.id);
                      }
                      if (e.key === "Escape") setRenamingId(null);
                    }}
                  />
                ) : (
                  <button
                    type="button"
                    className="chat-thread-title"
                    onClick={() => void selectThread(t.id)}
                    onDoubleClick={() => {
                      setRenamingId(t.id);
                      setRenameDraft(t.title);
                    }}
                  >
                    {t.title.trim() || "新しい会話"}
                  </button>
                )}
                <button
                  type="button"
                  className="chat-icon-btn danger"
                  title="削除"
                  aria-label="会話を削除"
                  disabled={busy}
                  onClick={() => void deleteThread(t.id)}
                >
                  ×
                </button>
              </li>
            ))}
          </ul>
        )}
      </aside>
      <main className="chat-main">
        <div className="chat-log" ref={listRef}>
          {messages.length === 0 && !stream && !thinking && !toolHint && !busy ? (
            <p className="chat-muted chat-empty">
              検索やノートの「チャット」から出典を送り、会話を選べます。出典なしでも、設定の「ローカルLLM」に接続して会話できます。足りない資料はモデルがインデックスを検索することもあります。
            </p>
          ) : (
            messages.map((m) => {
              const cites =
                m.role === "assistant" ? citedForMessage(sources, m.id) : [];
              const citeInfo = openableCites(cites, 1);
              return (
                <article
                  key={m.id}
                  className={m.role === "user" ? "chat-msg user" : "chat-msg assistant"}
                >
                  {m.role === "assistant" ? (
                    <AssistantBody
                      text={m.content}
                      citeNos={citeInfo.nos}
                      onCite={(n) => {
                        const s = citeInfo.byNo.get(n);
                        if (s) void openSource(s);
                      }}
                      onChoice={applyTemplate}
                      choicesDisabled={busy}
                      onLayout={scrollLog}
                    />
                  ) : (
                    <div className="chat-msg-body">{m.content}</div>
                  )}
                  {cites.length > 0 ? (
                    <div className="chat-msg-cites" aria-label="出典">
                      {cites.map((s, i) => {
                        const n = citeNoOf(s, i + 1);
                        const path = s.path.trim();
                        if (!path) {
                          return (
                            <span key={s.id} className="chat-cite-badge">
                              [{n}] {sourceLabel(s)}
                            </span>
                          );
                        }
                        return (
                          <button
                            key={s.id}
                            type="button"
                            className="chat-cite-badge clickable"
                            title="プレビュー"
                            onClick={() => void openSource(s)}
                          >
                            [{n}] {sourceLabel(s)}
                          </button>
                        );
                      })}
                    </div>
                  ) : null}
                </article>
              );
            })
          )}
          {busy || stream || thinking || toolHint ? (
            <article className="chat-msg assistant streaming">
              {toolHint ? <p className="chat-tool-hint">{toolHint}</p> : null}
              {thinking ? (
                <details className="chat-thinking">
                  <summary>{busy ? "思考中（表示する）" : "思考"}</summary>
                  <div className="chat-thinking-body">{thinking}</div>
                </details>
              ) : null}
              <AssistantBody
                text={stream || (!thinking && !toolHint ? "生成中…" : "")}
                citeNos={liveCiteInfo.nos}
                onCite={(n) => {
                  const s = liveCiteInfo.byNo.get(n);
                  if (s) void openSource(s);
                }}
                onChoice={applyTemplate}
                choicesDisabled
                streaming={!!stream}
                showCaret={!!stream || (!thinking && !toolHint)}
                onLayout={scrollLog}
              />
              {liveCites.length > 0 ? (
                <div className="chat-msg-cites" aria-label="今回の出典">
                  {liveCites.map((s, i) => {
                    const n = citeNoOf(s, maxCitedNo + i + 1);
                    const path = s.path.trim();
                    if (!path) {
                      return (
                        <span key={s.id} className="chat-cite-badge">
                          [{n}] {sourceLabel(s)}
                        </span>
                      );
                    }
                    return (
                      <button
                        key={s.id}
                        type="button"
                        className="chat-cite-badge clickable"
                        title="プレビュー"
                        onClick={() => void openSource(s)}
                      >
                        [{n}] {sourceLabel(s)}
                      </button>
                    );
                  })}
                </div>
              ) : null}
            </article>
          ) : null}
        </div>
        {error ? <p className="chat-error">{error}</p> : null}
        {notice ? <p className="chat-notice">{notice}</p> : null}
        <form
          className="chat-composer"
          onSubmit={(e) => {
            e.preventDefault();
            void send();
          }}
        >
          {pendingSources.length > 0 ? (
            <div className="chat-sources" aria-label="これから読む出典">
              {pendingSources.map((s, i) => {
                const expandable = sourceCanExpand(s);
                const fileGrain = isFileGrain(s);
                const n = citeNoOf(s, maxCitedNo + i + 1);
                return (
                  <span
                    key={s.id}
                    className={
                      fileGrain
                        ? "chat-source-chip file"
                        : "chat-source-chip"
                    }
                  >
                    {expandable ? (
                      <button
                        type="button"
                        className="chat-source-grain"
                        disabled={busy}
                        title={
                          fileGrain
                            ? "クリックで段落に戻す"
                            : "クリックでファイル全体を読ませる"
                        }
                        onClick={() => void toggleSourceGrain(s)}
                      >
                        [{n}] {sourceLabel(s)}
                        {fileGrain ? (
                          <span className="chat-source-badge">全文</span>
                        ) : null}
                      </button>
                    ) : (
                      <span className="chat-source-grain static">
                        [{n}] {sourceLabel(s)}
                      </span>
                    )}
                    {s.path.trim() ? (
                      <button
                        type="button"
                        className="chat-source-openfile"
                        title="プレビュー"
                        aria-label="プレビュー"
                        onClick={() => void openSource(s)}
                      >
                        <IconOpenFile />
                      </button>
                    ) : null}
                    <button
                      type="button"
                      className="chat-source-remove"
                      title="出典を外す"
                      aria-label={`${sourceLabel(s)}を外す`}
                      disabled={busy}
                      onClick={() => void removeSource(s.id)}
                    >
                      ×
                    </button>
                  </span>
                );
              })}
            </div>
          ) : null}
          <div className="chat-composer-tools">
            <ChatScopePicker
              disabled={busy || !active}
              threadId={active?.id ?? null}
              pathPrefix={active?.pathPrefix ?? ""}
              onApplied={applyThreadScope}
              onError={setError}
            />
            <button
              type="button"
              className="chat-tpl"
              disabled={busy}
              onClick={() => applyTemplate(TPL_SUMMARY)}
            >
              要約
            </button>
            <button
              type="button"
              className="chat-tpl"
              disabled={busy}
              onClick={() => applyTemplate(TPL_POINTS)}
            >
              要点
            </button>
            <button
              type="button"
              className="chat-tpl"
              disabled={busy}
              onClick={() => applyTemplate(TPL_FACTS)}
            >
              要件事実
            </button>
            <button
              type="button"
              className="chat-tpl"
              disabled={busy}
              onClick={() => applyTemplate(TPL_TIMELINE)}
            >
              時系列
            </button>
            <span
              className={overBudget ? "chat-char-meter warn" : "chat-char-meter"}
              title="読込済みの出典本文も履歴の一部として数えます"
            >
              約 {estimatedChars.toLocaleString()} / {maxContextChars.toLocaleString()}{" "}
              文字
              {overBudget ? " · 古い出典から切り詰めます" : ""}
            </span>
          </div>
          <div className="chat-composer-row">
            <textarea
              ref={textareaRef}
              rows={3}
              value={draft}
              placeholder="メッセージを入力（Enter で送信、Shift+Enter で改行）"
              disabled={busy}
              onChange={(e) => setDraft(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  void send();
                }
              }}
            />
            <div className="chat-composer-actions">
              {busy ? (
                <button type="button" className="chat-btn" onClick={() => void stop()}>
                  停止
                </button>
              ) : (
                <button
                  type="submit"
                  className="chat-btn primary"
                  disabled={!draft.trim()}
                >
                  送信
                </button>
              )}
            </div>
          </div>
        </form>
      </main>
    </div>
  );
}
