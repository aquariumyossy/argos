import { buildInlineLines, type NoteDiffHunk } from "./noteReview";

export type NoteVersionMeta = {
  id: string;
  title: string;
  source: string;
  createdAt: number;
  memoLen: number;
};

export type NoteHistory = {
  versions: NoteVersionMeta[];
  aiUpdatedAt: number | null;
};

export function formatAiMark(unixSeconds: number): string {
  return `AIが更新・${formatVersionClock(unixSeconds)}`;
}

export function formatVersionClock(unixSeconds: number): string {
  const d = new Date(unixSeconds * 1000);
  const hh = String(d.getHours()).padStart(2, "0");
  const mm = String(d.getMinutes()).padStart(2, "0");
  return `${hh}:${mm}`;
}

export function versionLabel(source: string, memoLen: number): string {
  if (memoLen === 0) return "空";
  if (source === "ai") return "AI";
  if (source === "revert") return "戻した";
  return "手入力";
}

export function previousVersionId(history: NoteHistory | null): string | null {
  const versions = history?.versions ?? [];
  if (versions.length < 2) return null;
  return versions[versions.length - 2]?.id ?? null;
}

function lineText(text: string): string {
  return text.length > 0 ? text : "\u00a0";
}

type ControlsProps = {
  history: NoteHistory | null;
  open: boolean;
  selectedId: string | null;
  onToggleOpen: () => void;
  onSelect: (id: string | null) => void;
};

export function NoteHistoryControls({
  history,
  open,
  selectedId,
  onToggleOpen,
  onSelect,
}: ControlsProps) {
  const versions = history?.versions ?? [];
  const latestId = versions.length > 0 ? versions[versions.length - 1].id : null;
  const mark =
    history?.aiUpdatedAt != null ? formatAiMark(history.aiUpdatedAt) : null;
  const prev = previousVersionId(history);
  return (
    <div className="notes-history-wrap">
      {mark ? (
        <button
          type="button"
          className={
            selectedId && selectedId === prev
              ? "notes-ai-mark active"
              : "notes-ai-mark"
          }
          onClick={() => onSelect(selectedId === prev ? null : prev)}
        >
          {mark}
        </button>
      ) : null}
      <button
        type="button"
        className={open ? "notes-history-toggle active" : "notes-history-toggle"}
        aria-expanded={open}
        onClick={onToggleOpen}
      >
        履歴
      </button>
      {open ? (
        <ul className="notes-history" aria-label="版の履歴">
          {versions.length === 0 ? (
            <li className="notes-history-empty">版はまだありません</li>
          ) : (
            [...versions].reverse().map((v) => {
              const latest = v.id === latestId;
              const label = latest ? "最新" : versionLabel(v.source, v.memoLen);
              return (
                <li key={v.id}>
                  <button
                    type="button"
                    className={selectedId === v.id ? "active" : ""}
                    aria-pressed={!latest && selectedId === v.id}
                    onClick={() => onSelect(latest ? null : v.id)}
                  >
                    {label}・{formatVersionClock(v.createdAt)}
                  </button>
                </li>
              );
            })
          )}
        </ul>
      ) : null}
    </div>
  );
}

type DiffProps = {
  memo: string;
  hunks: NoteDiffHunk[];
  busy: boolean;
  onRestore: () => void;
  onClose: () => void;
};

export function NoteVersionDiffView({
  memo,
  hunks,
  busy,
  onRestore,
  onClose,
}: DiffProps) {
  const lines = buildInlineLines({ memo, hunks });
  return (
    <div className="notes-version-diff">
      <div className="notes-review-bar">
        <span className="notes-review-bar-label">選んだ版と最新の違い</span>
        <div className="notes-review-bar-actions">
          <button
            type="button"
            className="notes-review-ack"
            disabled={busy}
            onClick={onRestore}
          >
            この版に戻す
          </button>
          <button type="button" disabled={busy} onClick={onClose}>
            閉じる
          </button>
        </div>
      </div>
      {lines.length === 0 ? (
        <p className="notes-history-empty">本文の違いはありません。</p>
      ) : (
        <div className="notes-inline-diff" aria-label="選んだ版と最新の差分">
          {lines.map((line, index) => (
            <div key={index} className="notes-inline-line">
              <span
                className={
                  line.kind === "add"
                    ? "notes-inline-text notes-inline-add"
                    : line.kind === "del"
                      ? "notes-inline-text notes-inline-del"
                      : "notes-inline-text"
                }
              >
                {lineText(line.text)}
              </span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
