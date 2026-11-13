import type { OfficeCardFile } from "./officeCards";

export function OfficeFileCards({
  files,
  onOpen,
}: {
  files: readonly OfficeCardFile[];
  onOpen: (name: string) => void;
}) {
  if (files.length === 0) return null;
  return (
    <div className="chat-office-cards">
      {files.map((file, index) => (
        <OfficeFileCard
          key={`${index}:${file.kind}:${file.name}`}
          file={file}
          onOpen={onOpen}
        />
      ))}
    </div>
  );
}

function OfficeFileCard({
  file,
  onOpen,
}: {
  file: OfficeCardFile;
  onOpen: (name: string) => void;
}) {
  return (
    <div
      className="chat-office-card"
      data-office-card
      data-office-kind={file.kind}
    >
      <OfficeKindIcon kind={file.kind} />
      <div className="chat-office-card-main">
        <div className="chat-office-card-name">{file.name}</div>
        {file.sourceName ? (
          <div className="chat-office-card-source">元：{file.sourceName}</div>
        ) : null}
      </div>
      <button
        type="button"
        className="chat-office-open"
        aria-label={`${file.name} を開く`}
        onClick={() => onOpen(file.name)}
      >
        開く
      </button>
    </div>
  );
}

function OfficeKindIcon({ kind }: { kind: "docx" | "xlsx" }) {
  const word = kind === "docx";
  const ink = word ? "#185ABD" : "#107C41";
  return (
    <svg
      className="chat-office-icon"
      viewBox="0 0 32 32"
      width="32"
      height="32"
      aria-hidden="true"
    >
      <rect x="1" y="1" width="30" height="30" rx="7" fill={ink} />
      <path
        d="M9 7.5h9.2L23 12.2V24a1.2 1.2 0 0 1-1.2 1.2H9A1.2 1.2 0 0 1 7.8 24V8.7A1.2 1.2 0 0 1 9 7.5Z"
        fill="#fff"
      />
      <path d="M18.1 7.6 23 12.2h-4.9V7.6Z" fill={ink} />
      {word ? (
        <path
          d="M11.2 16.4 13.2 22l1.8-3.4L16.8 22l2-5.6"
          fill="none"
          stroke={ink}
          strokeWidth="1.5"
          strokeLinejoin="round"
          strokeLinecap="round"
        />
      ) : (
        <path
          d="M11 16.2h8.4v6.4H11zM11 18.3h8.4M11 20.4h8.4M13.8 16.2v6.4M16.6 16.2v6.4"
          fill="none"
          stroke={ink}
          strokeWidth="1"
        />
      )}
    </svg>
  );
}
