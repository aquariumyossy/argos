import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { contextIsNearLimit, contextMeterLimit } from "./contextEstimate";

/** Same idea as the thread-list search wait, long enough to skip a keystroke. */
export const METER_DEBOUNCE_MS = 200;

export function useChatContextMeter(args: {
  threadId: string;
  noteId: string;
  pathPrefix: string;
  draft: string;
  webSearch: boolean;
  maxContextTokens: number;
  searchMode: string;
  messages: unknown;
  sources: unknown;
}): { used: number | null; limit: number; warn: boolean } {
  const limit = contextMeterLimit(args.maxContextTokens);
  const [used, setUsed] = useState<number | null>(null);
  const threadRef = useRef(args.threadId);

  useEffect(() => {
    let cancelled = false;
    if (threadRef.current !== args.threadId) {
      threadRef.current = args.threadId;
      setUsed(null);
    }
    const timer = window.setTimeout(() => {
      void invoke<number>("llm_context_meter", {
        threadId: args.threadId,
        draft: args.draft,
        webSearch: args.webSearch,
      })
        .then((next) => {
          if (cancelled) return;
          if (!Number.isFinite(next)) {
            setUsed(null);
            return;
          }
          setUsed(next);
        })
        .catch(() => {
          if (!cancelled) setUsed(null);
        });
    }, METER_DEBOUNCE_MS);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [
    args.threadId,
    args.noteId,
    args.pathPrefix,
    args.draft,
    args.webSearch,
    args.maxContextTokens,
    args.searchMode,
    args.messages,
    args.sources,
  ]);

  return {
    used,
    limit,
    warn: used != null && contextIsNearLimit(used, limit),
  };
}
