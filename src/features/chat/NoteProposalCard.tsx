import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { AssistantBody } from "./AssistantBody";

export type NoteProposalDiffLine = { kind: string; text: string };

export type NoteProposalRow = {
  id: string;
  threadId: string;
  noteId: string;
  requestId?: string;
  assistantMessageId: string;
  kind: string;
  heading: string;
  oldText: string;
  newText: string;
  chunk: string;
  status: string;
  createdAt: number;
  noteTitle: string;
  diff: NoteProposalDiffLine[];
};

type Props = {
  proposal: NoteProposalRow;
  onChange: (row: NoteProposalRow) => void;
  onError: (message: string) => void;
};

const emptyCiteNos = new Set<number>();

function kindLabel(kind: string): string {
  if (kind === "replace") return "置換";
  if (kind === "insert") return "新設";
  return "追記";
}

function formatInvokeError(e: unknown): string {
  if (e instanceof Error) return e.message;
  if (typeof e === "string") return e;
  try {
    return JSON.stringify(e);
  } catch {
    return String(e);
  }
}

export default function NoteProposalCard({
  proposal,
  onChange,
  onError,
}: Props) {
  const [tab, setTab] = useState<"diff" | "preview">("diff");
  const [busy, setBusy] = useState(false);
  const heading = proposal.heading.trim() || "末尾";
  const title = `ノート『${proposal.noteTitle.trim() || "無題"}』／見出し『${heading}』`;
  const pending = proposal.status === "pending";
  const applied = proposal.status === "applied";
  const preview = proposal.kind === "append" ? proposal.chunk || proposal.newText : proposal.newText;

  async function run(cmd: string) {
    if (busy) return;
    setBusy(true);
    try {
      const row = await invoke<NoteProposalRow>(cmd, { id: proposal.id });
      onChange(row);
    } catch (e) {
      onError(formatInvokeError(e));
    } finally {
      setBusy(false);
    }
  }

  async function openNote() {
    try {
      await invoke("set_active_note", { id: proposal.noteId });
      await invoke("show_notes_window");
    } catch (e) {
      onError(formatInvokeError(e));
    }
  }

  return (
    <div className={`chat-proposal chat-proposal--${proposal.status}`}>
      <div className="chat-proposal-head">
        <strong>{title}</strong>
        <span className="chat-proposal-kind">{kindLabel(proposal.kind)}</span>
      </div>
      {proposal.status === "stale" ? (
        <p className="chat-proposal-status">
          メモが変わっています。却下して再度依頼してください
        </p>
      ) : proposal.status === "applied" ? (
        <p className="chat-proposal-status">採用済み</p>
      ) : proposal.status === "dismissed" ? (
        <p className="chat-proposal-status">却下</p>
      ) : proposal.status === "superseded" ? (
        <p className="chat-proposal-status">新しい提案に置き換わりました</p>
      ) : null}
      <div className="chat-proposal-tabs" role="tablist">
        <button
          type="button"
          className={tab === "diff" ? "is-active" : ""}
          onClick={() => setTab("diff")}
        >
          変更
        </button>
        <button
          type="button"
          className={tab === "preview" ? "is-active" : ""}
          onClick={() => setTab("preview")}
        >
          プレビュー
        </button>
      </div>
      {tab === "diff" ? (
        <pre className="chat-proposal-diff">
          {(proposal.diff ?? []).map((line, i) => (
            <span
              key={i}
              className={
                line.kind === "add"
                  ? "chat-diff-add"
                  : line.kind === "del"
                    ? "chat-diff-del"
                    : "chat-diff-eq"
              }
            >
              {line.kind === "add" ? "+" : line.kind === "del" ? "−" : " "}
              {line.text}
              {"\n"}
            </span>
          ))}
        </pre>
      ) : (
        <div className="chat-proposal-preview">
          <AssistantBody
            text={preview || "（空）"}
            citeNos={emptyCiteNos}
            onCite={() => {}}
            onChoice={() => {}}
            choicesDisabled
          />
        </div>
      )}
      <div className="chat-proposal-actions">
        {pending ? (
          <>
            <button
              type="button"
              className="chat-proposal-apply"
              disabled={busy}
              onClick={() => void run("apply_note_proposal")}
            >
              採用
            </button>
            <button
              type="button"
              disabled={busy}
              onClick={() => void run("dismiss_note_proposal")}
            >
              却下
            </button>
          </>
        ) : null}
        {applied ? (
          <button
            type="button"
            disabled={busy}
            onClick={() => void run("undo_note_proposal")}
          >
            取り消し
          </button>
        ) : null}
        <button type="button" onClick={() => void openNote()}>
          ノートで開く
        </button>
      </div>
    </div>
  );
}
