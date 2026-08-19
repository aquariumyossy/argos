import { useEffect, useMemo, useRef, useState } from "react";
import {
  parseChoiceLines,
  renderAssistantMdHtml,
  splitAssistantBlocks,
  type AssistantBlock,
} from "./assistantMd";
import { attachResizableChatTables } from "./chatTableResize";
import { sanitizeMermaidSource } from "./mermaidSanitize";

type MermaidApi = {
  initialize: (opts: Record<string, unknown>) => void;
  render: (
    id: string,
    source: string,
  ) => Promise<{ svg: string }>;
};

let mermaidLoader: Promise<MermaidApi> | null = null;
let mermaidSeq = 0;

function loadMermaid(): Promise<MermaidApi> {
  if (!mermaidLoader) {
    mermaidLoader = import("mermaid").then((mod) => mod.default as unknown as MermaidApi);
  }
  return mermaidLoader;
}

function mermaidInitOpts(el: HTMLElement | null): Record<string, unknown> {
  const from = el?.closest(".chat") ?? el;
  const cs = from ? getComputedStyle(from) : null;
  const fontSize =
    cs?.fontSize && cs.fontSize !== "0px" ? cs.fontSize : "14px";
  return {
    startOnLoad: false,
    securityLevel: "strict",
    theme: "neutral",
    themeVariables: {
      fontSize,
      fontFamily: cs?.fontFamily || "var(--font-ui)",
    },
    flowchart: {
      htmlLabels: false,
      useMaxWidth: false,
      nodeSpacing: 28,
      rankSpacing: 80,
    },
  };
}

function MermaidBlock({
  source,
  onLayout,
}: {
  source: string;
  onLayout?: () => void;
}) {
  const [mode, setMode] = useState<"diagram" | "source">("diagram");
  const [error, setError] = useState<string | null>(null);
  const hostRef = useRef<HTMLDivElement>(null);

  const renderSource = useMemo(() => sanitizeMermaidSource(source), [source]);

  useEffect(() => {
    if (mode !== "diagram") return;
    const id = `argosMmd${++mermaidSeq}`;
    let cancelled = false;
    setError(null);
    void loadMermaid()
      .then(async (api) => {
        api.initialize(mermaidInitOpts(hostRef.current));
        const { svg } = await api.render(id, renderSource);
        if (cancelled || !hostRef.current) return;
        hostRef.current.innerHTML = svg;
        onLayout?.();
      })
      .catch((e) => {
        document.getElementById(id)?.remove();
        document.getElementById(`d${id}`)?.remove();
        if (cancelled) return;
        setError(e instanceof Error ? e.message : String(e));
        setMode("source");
        if (hostRef.current) hostRef.current.innerHTML = "";
      });
    return () => {
      cancelled = true;
      if (hostRef.current) hostRef.current.innerHTML = "";
      document.getElementById(id)?.remove();
      document.getElementById(`d${id}`)?.remove();
    };
  }, [renderSource, mode, onLayout]);

  return (
    <div className="chat-mermaid">
      <div className="chat-mermaid-toolbar">
        <button
          type="button"
          className={mode === "diagram" ? "chat-tpl is-active" : "chat-tpl"}
          onClick={() => {
            setError(null);
            setMode("diagram");
          }}
        >
          図
        </button>
        <button
          type="button"
          className={mode === "source" ? "chat-tpl is-active" : "chat-tpl"}
          onClick={() => setMode("source")}
        >
          ソース
        </button>
      </div>
      {mode === "source" || error ? (
        <div className="chat-mermaid-view">
          {error ? <p className="chat-mermaid-error">{error}</p> : null}
          <pre>
            <code>{renderSource}</code>
          </pre>
        </div>
      ) : (
        <div className="chat-mermaid-view" ref={hostRef} />
      )}
    </div>
  );
}

function MdHtml({
  text,
  citeNos,
  onCite,
}: {
  text: string;
  citeNos: ReadonlySet<number>;
  onCite: (n: number) => void;
}) {
  const rootRef = useRef<HTMLDivElement>(null);
  const citeKey = [...citeNos].sort((a, b) => a - b).join(",");
  const html = useMemo(() => {
    const set = new Set(
      citeKey
        .split(",")
        .filter(Boolean)
        .map((s) => Number(s)),
    );
    return renderAssistantMdHtml(text, set);
  }, [text, citeKey]);

  useEffect(() => {
    attachResizableChatTables(rootRef.current);
  }, [html]);

  return (
    <div
      ref={rootRef}
      dangerouslySetInnerHTML={{ __html: html }}
      onClick={(e) => {
        const btn = (e.target as HTMLElement).closest("button.md-cite");
        if (!(btn instanceof HTMLButtonElement)) return;
        const n = Number(btn.dataset.cite);
        if (Number.isFinite(n) && n > 0) onCite(n);
      }}
    />
  );
}

function FenceFallback({ text }: { text: string }) {
  return (
    <pre>
      <code>{text}</code>
    </pre>
  );
}

function BlockView({
  block,
  citeNos,
  onCite,
  onChoice,
  choicesDisabled,
  onLayout,
}: {
  block: AssistantBlock;
  citeNos: ReadonlySet<number>;
  onCite: (n: number) => void;
  onChoice: (text: string) => void;
  choicesDisabled?: boolean;
  onLayout?: () => void;
}) {
  if (block.type === "md") {
    return <MdHtml text={block.text} citeNos={citeNos} onCite={onCite} />;
  }
  if (block.type === "mermaid") {
    if (!block.closed) return <FenceFallback text={block.text} />;
    return <MermaidBlock source={block.text} onLayout={onLayout} />;
  }
  if (block.type === "choices") {
    if (!block.closed) return <FenceFallback text={block.text} />;
    const lines = parseChoiceLines(block.text);
    if (lines.length === 0) return null;
    return (
      <div className="chat-choices">
        {lines.map((line, i) => (
          <button
            key={`${i}:${line}`}
            type="button"
            className="chat-choice"
            disabled={choicesDisabled}
            onClick={() => onChoice(line)}
          >
            {line}
          </button>
        ))}
      </div>
    );
  }
  return <FenceFallback text={block.text} />;
}

export function AssistantBody({
  text,
  citeNos,
  onCite,
  onChoice,
  choicesDisabled,
  streaming,
  showCaret,
  onLayout,
}: {
  text: string;
  citeNos: ReadonlySet<number>;
  onCite: (n: number) => void;
  onChoice: (text: string) => void;
  choicesDisabled?: boolean;
  streaming?: boolean;
  showCaret?: boolean;
  onLayout?: () => void;
}) {
  const [shown, setShown] = useState(text);

  useEffect(() => {
    if (!streaming) {
      setShown(text);
      return;
    }
    const t = window.setTimeout(() => setShown(text), 50);
    return () => window.clearTimeout(t);
  }, [text, streaming]);

  const blocks = useMemo(() => splitAssistantBlocks(shown), [shown]);

  return (
    <div className="chat-msg-body md-body md-body--hierarchy">
      {blocks.map((block, i) => (
        <BlockView
          key={i}
          block={block}
          citeNos={citeNos}
          onCite={onCite}
          onChoice={onChoice}
          choicesDisabled={choicesDisabled}
          onLayout={onLayout}
        />
      ))}
      {showCaret ? <span className="chat-caret" /> : null}
    </div>
  );
}
