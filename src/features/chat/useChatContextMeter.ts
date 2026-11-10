import { useEffect, useState } from "react";
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
  // The count belongs to the configured limit it was measured under. The
  // denominator is derived from that setting and can stay put when the setting
  // moves by one token, while the measured size can still change. Until a
  // reading for the new setting arrives, hide the numerator so the old count
  // is not shown against the new denominator.
  const [reading, setReading] = useState<{
    threadId: string;
    maxContextTokens: number;
    used: number | null;
  } | null>(null);
  const used =
    reading != null &&
    reading.threadId === args.threadId &&
    reading.maxContextTokens === args.maxContextTokens
      ? reading.used
      : null;

  useEffect(() => {
    let cancelled = false;
    const threadId = args.threadId;
    const maxContextTokens = args.maxContextTokens;
    const timer = window.setTimeout(() => {
      void invoke<number>("llm_context_meter", {
        threadId: args.threadId,
        draft: args.draft,
        webSearch: args.webSearch,
      })
        .then((next) => {
          if (cancelled) return;
          if (!Number.isFinite(next)) {
            setReading({ threadId, maxContextTokens, used: null });
            return;
          }
          setReading({ threadId, maxContextTokens, used: next });
        })
        .catch(() => {
          if (!cancelled) setReading({ threadId, maxContextTokens, used: null });
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
