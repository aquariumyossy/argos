import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { sanitizeMermaidSource } from "../chat/mermaidSanitize";
import {
  NOTE_MEMO_HIGHLIGHT,
  renderNoteMemoHtml,
  resetNoteMemoCheckboxSeq,
  splitNoteMemoBlocks,
  toggleGfmCheckbox,
} from "./noteMemoMd";

type MermaidApi = {
  initialize: (opts: Record<string, unknown>) => void;
  render: (id: string, source: string) => Promise<{ svg: string }>;
};

let mermaidLoader: Promise<MermaidApi> | null = null;
let mermaidSeq = 0;

function loadMermaid(): Promise<MermaidApi> {
  if (!mermaidLoader) {
    mermaidLoader = import("mermaid").then(
      (mod) => mod.default as unknown as MermaidApi,
    );
  }
  return mermaidLoader;
}

function MemoMermaid({ source }: { source: string }) {
  const [mode, setMode] = useState<"diagram" | "source">("diagram");
  const [error, setError] = useState<string | null>(null);
  const hostRef = useRef<HTMLDivElement>(null);
  const renderSource = useMemo(() => sanitizeMermaidSource(source), [source]);

  useEffect(() => {
    if (mode !== "diagram") return;
    const id = `argosNoteMmd${++mermaidSeq}`;
    let cancelled = false;
    setError(null);
    void loadMermaid()
      .then(async (api) => {
        api.initialize({
          startOnLoad: false,
          securityLevel: "strict",
          theme: "neutral",
          flowchart: { htmlLabels: false, useMaxWidth: false },
        });
        const { svg } = await api.render(id, renderSource);
        if (cancelled || !hostRef.current) return;
        hostRef.current.innerHTML = svg;
      })
      .catch((e) => {
        document.getElementById(id)?.remove();
        document.getElementById(`d${id}`)?.remove();
        if (!cancelled) {
          setError(e instanceof Error ? e.message : String(e));
          if (hostRef.current) hostRef.current.innerHTML = "";
        }
      });
    return () => {
      cancelled = true;
    };
  }, [mode, renderSource]);

  return (
    <div className="notes-memo-mermaid">
      <div className="notes-memo-mermaid-toolbar">
        <button
          type="button"
          className={mode === "diagram" ? "active" : ""}
          onClick={() => setMode("diagram")}
        >
          図
        </button>
        <button
          type="button"
          className={mode === "source" ? "active" : ""}
          onClick={() => setMode("source")}
        >
          ソース
        </button>
      </div>
      {mode === "source" || error ? (
        <pre className="notes-memo-mermaid-src">{source}</pre>
      ) : null}
      {error ? <p className="notes-error">{error}</p> : null}
      {mode === "diagram" ? (
        <div ref={hostRef} className="notes-memo-mermaid-host" />
      ) : null}
    </div>
  );
}

type Props = {
  memo: string;
  highlightQuery?: string;
  onToggleCheckbox: (next: string) => void;
};

export default function NoteMemoView({
  memo,
  highlightQuery,
  onToggleCheckbox,
}: Props) {
  const rootRef = useRef<HTMLDivElement>(null);
  const blocks = useMemo(() => {
    const raw = splitNoteMemoBlocks(memo);
    resetNoteMemoCheckboxSeq();
    return raw.map((b) =>
      b.type === "mermaid"
        ? b
        : { type: "md" as const, html: renderNoteMemoHtml(b.text) },
    );
  }, [memo]);

  const onClick = (e: React.MouseEvent) => {
    const t = e.target as HTMLElement | null;
    const input = t?.closest("input[data-note-check]") as HTMLInputElement | null;
    if (!input) return;
    e.preventDefault();
    const idx = Number(input.getAttribute("data-note-check"));
    if (!Number.isFinite(idx)) return;
    const next = toggleGfmCheckbox(memo, idx);
    if (next != null) onToggleCheckbox(next);
  };

  useLayoutEffect(() => {
    const el = rootRef.current;
    CSS.highlights?.delete(NOTE_MEMO_HIGHLIGHT);
    const q = highlightQuery?.trim();
    if (!el || !q || !CSS.highlights) return;
    const needle = q.normalize("NFKC").toLowerCase();
    const ranges: Range[] = [];
    const walker = document.createTreeWalker(el, NodeFilter.SHOW_TEXT);
    let node: Node | null;
    while ((node = walker.nextNode())) {
      const text = node.textContent ?? "";
      const lower = text.normalize("NFKC").toLowerCase();
      let from = 0;
      while (from < lower.length) {
        const at = lower.indexOf(needle, from);
        if (at < 0) break;
        const range = document.createRange();
        range.setStart(node, at);
        range.setEnd(node, at + needle.length);
        ranges.push(range);
        from = at + needle.length;
      }
    }
    if (ranges.length > 0) {
      CSS.highlights.set(NOTE_MEMO_HIGHLIGHT, new Highlight(...ranges));
    }
    return () => {
      CSS.highlights?.delete(NOTE_MEMO_HIGHLIGHT);
    };
  }, [memo, highlightQuery, blocks]);

  return (
    <div
      ref={rootRef}
      className="notes-memo-preview md-body"
      onClick={onClick}
    >
      {blocks.map((b, i) =>
        b.type === "mermaid" ? (
          <MemoMermaid key={i} source={b.text} />
        ) : (
          <div
            key={i}
            className="notes-memo-md"
            dangerouslySetInnerHTML={{ __html: b.html }}
          />
        ),
      )}
    </div>
  );
}
