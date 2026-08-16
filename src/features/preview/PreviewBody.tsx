import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { formatLegalDisplayHtml, formatLegalMdHtml } from "../notes/legalMdFormat";
import { highlightText } from "../search/highlightText";
import {
  applyPreviewHighlights,
  findFormattedContentOffset,
  findJsonHitOffset,
  formatGenericJsonHtml,
  formatJsonForPreview,
  isHtmlPath,
  isJsonPath,
  isMarkdownPath,
  splitProseParagraphs,
} from "./markdownPreview";
import type { SearchHit } from "./types";

export function PreviewBody({
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
          <p key={i}>{highlightText(para, query, highlightTerms ?? hit.highlightTerms)}</p>
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
        {highlightText(text, query, highlightTerms ?? hit.highlightTerms)}
      </pre>
    );
  }

  return (
    <pre className="preview-body">
      {highlightText(hit.previewText, query, highlightTerms ?? hit.highlightTerms)}
    </pre>
  );
}
