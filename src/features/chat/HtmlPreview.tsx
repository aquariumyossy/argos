import { useEffect, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import type { OfficeCardFile } from "./officeCards";
import { htmlPreviewSrc } from "./htmlPreviewPolicy";

/** Current default frame height. Dragging cannot go below this. */
export const HTML_PREVIEW_MIN_HEIGHT = 220;

/** Keep the preview at least the default height, and no taller than the chat view. */
export function clampPreviewHeight(next: number, viewHeight: number): number {
  const max = Math.max(HTML_PREVIEW_MIN_HEIGHT, viewHeight);
  if (!Number.isFinite(next)) return HTML_PREVIEW_MIN_HEIGHT;
  return Math.min(max, Math.max(HTML_PREVIEW_MIN_HEIGHT, Math.round(next)));
}

function chatViewHeight(root: HTMLElement | null): number {
  const view = root?.closest(".chat-log-wrap");
  if (!(view instanceof HTMLElement)) return HTML_PREVIEW_MIN_HEIGHT;
  return view.clientHeight;
}

export function HtmlPreviews({
  files,
  resetKey = 0,
  onExpand,
}: {
  files: readonly OfficeCardFile[];
  /** Bumps when the chat window is opened again. Heights are not stored. */
  resetKey?: number;
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
          resetKey={resetKey}
          onExpand={onExpand}
        />
      ))}
    </div>
  );
}

function HtmlPreview({
  name,
  src,
  resetKey,
  onExpand,
}: {
  name: string;
  src: string;
  resetKey: number;
  onExpand: (name: string) => void;
}) {
  const rootRef = useRef<HTMLDivElement>(null);
  const dragRef = useRef<(() => void) | null>(null);
  const mountedRef = useRef(true);
  const [height, setHeight] = useState(HTML_PREVIEW_MIN_HEIGHT);
  const [dragging, setDragging] = useState(false);

  useEffect(() => {
    setHeight(HTML_PREVIEW_MIN_HEIGHT);
  }, [resetKey]);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      dragRef.current?.();
      dragRef.current = null;
    };
  }, []);

  function onGrabPointerDown(event: ReactPointerEvent<HTMLDivElement>) {
    event.preventDefault();
    dragRef.current?.();
    const pointerId = event.pointerId;
    const startY = event.clientY;
    const startHeight = height;
    const viewHeight = chatViewHeight(rootRef.current);
    try {
      event.currentTarget.setPointerCapture(pointerId);
    } catch {
      /* jsdom and a lost pointer do not support capture */
    }
    // A cross-origin iframe swallows pointer moves. The overlay is in the
    // DOM before the next event, so the drag continues over the preview.
    const overlay = document.createElement("div");
    overlay.className = "chat-html-preview-drag-overlay";
    overlay.dataset.previewDragOverlay = "";
    document.body.appendChild(overlay);
    if (mountedRef.current) setDragging(true);

    const samePointer = (ev: PointerEvent) =>
      ev.pointerId == null || ev.pointerId === pointerId;
    let stopped = false;
    const end = () => {
      if (stopped) return;
      stopped = true;
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      window.removeEventListener("pointercancel", onUp);
      overlay.remove();
      if (dragRef.current === end) dragRef.current = null;
      if (mountedRef.current) setDragging(false);
    };
    const onMove = (ev: PointerEvent) => {
      if (!samePointer(ev)) return;
      setHeight(clampPreviewHeight(startHeight + (ev.clientY - startY), viewHeight));
    };
    const onUp = (ev: PointerEvent) => {
      if (!samePointer(ev)) return;
      end();
    };
    dragRef.current = end;
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
    window.addEventListener("pointercancel", onUp);
  }

  return (
    <div
      className={dragging ? "chat-html-preview is-resizing" : "chat-html-preview"}
      data-html-preview={name}
      ref={rootRef}
    >
      <div className="chat-html-preview-bar">
        <button
          type="button"
          className="chat-html-preview-expand"
          onClick={() => onExpand(name)}
        >
          大きく表示
        </button>
      </div>
      {/* Unsandboxed so the shell keeps the htmlpreview origin. Its response CSP frame-src 'self' then limits the inner frame. */}
      <iframe
        className="chat-html-preview-frame"
        title={`${name} のプレビュー`}
        referrerPolicy="no-referrer"
        src={src}
        style={{ height }}
      />
      <div
        className="chat-html-preview-resize"
        role="separator"
        aria-orientation="horizontal"
        aria-label="プレビューの高さを変更"
        aria-valuemin={HTML_PREVIEW_MIN_HEIGHT}
        aria-valuenow={height}
        onPointerDown={onGrabPointerDown}
      />
    </div>
  );
}
