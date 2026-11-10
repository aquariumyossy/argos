import { AssistantBody } from "./AssistantBody";

export const SUMMARY_TITLE = "ここまでの要約";
export const SUMMARY_STATUS_TEXT = "会話が長くなったため、古いやり取りを要約しています…";
export const SUMMARY_FAILURE_WARNING = "要約できなかったため、古い部分を省いて送りました";
const TRIM_NOTICE = "トークン上限のため、古いやり取りや出典から切り詰めて送りました。";

const NO_CITES = new Set<number>();

export type ChatLive = {
  stream: string;
  thinking: string;
  toolHint: string;
  status: string;
};

export const emptyChatLive: ChatLive = {
  stream: "",
  thinking: "",
  toolHint: "",
  status: "",
};

/** Progress while summarizing must not become part of the answer stream. */
export function applyChatDelta(state: ChatLive, kind: string, text: string): ChatLive {
  if (kind === "reset") return emptyChatLive;
  if (kind === "reasoning") {
    return { ...state, status: "", thinking: state.thinking + text };
  }
  if (kind === "tool") {
    return {
      ...state,
      status: "",
      toolHint: text || "インデックスを検索しています…",
    };
  }
  if (kind === "status") {
    return { ...state, status: text };
  }
  return { ...state, status: "", stream: state.stream + text };
}

/** The failure warning replaces the generic trim notice for that send. */
export function noticeFromSend(
  warning: string | null | undefined,
  truncated: boolean,
): string {
  const text = (warning ?? "").trim();
  if (text) return text;
  if (truncated) return TRIM_NOTICE;
  return "";
}

export function SummaryAfterMessage({
  messageId,
  throughId,
  summary,
}: {
  messageId: string;
  throughId: string;
  summary: string;
}) {
  const text = summary.trim();
  if (!text || messageId !== throughId.trim()) return null;
  return (
    <details className="chat-summary">
      <summary>{SUMMARY_TITLE}</summary>
      <AssistantBody
        text={text}
        citeNos={NO_CITES}
        onCite={() => {}}
        onChoice={() => {}}
      />
    </details>
  );
}
