import type { OfficeCardFile } from "./officeCards";
import { PREVIEW_CSP, PREVIEW_SANDBOX, htmlPreviewSrc } from "./htmlPreview";

export function HtmlPreviews({
  files,
  onExpand,
}: {
  files: readonly OfficeCardFile[];
  onExpand: (name: string) => void;
}) {
  const htmlFiles = files.filter((file) => file.kind === "html");
  if (htmlFiles.length === 0) return null;
  const windows =
    typeof navigator !== "undefined" && /Windows/i.test(navigator.userAgent);
  return (
    <div className="chat-html-previews">
      {htmlFiles.map((file, index) => (
        <HtmlPreview
          key={`${index}:${file.name}`}
          name={file.name}
          src={htmlPreviewSrc(file.name, windows)}
          onExpand={onExpand}
        />
      ))}
    </div>
  );
}

function HtmlPreview({
  name,
  src,
  onExpand,
}: {
  name: string;
  src: string;
  onExpand: (name: string) => void;
}) {
  return (
    <div className="chat-html-preview" data-html-preview={name}>
      <div className="chat-html-preview-bar">
        <button
          type="button"
          className="chat-html-preview-expand"
          onClick={() => onExpand(name)}
        >
          大きく表示
        </button>
      </div>
      <iframe
        className="chat-html-preview-frame"
        title={`${name} のプレビュー`}
        sandbox={PREVIEW_SANDBOX}
        csp={PREVIEW_CSP}
        referrerPolicy="no-referrer"
        src={src}
      />
    </div>
  );
}
