/** Kept in step with `PREVIEW_CSP` / `PREVIEW_SANDBOX` in `src-tauri/src/html_preview.rs`. */
export const PREVIEW_CSP =
  "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; font-src data:; media-src data:; connect-src 'none'; form-action 'none'; frame-src 'none'; child-src 'none'; object-src 'none'; base-uri 'none'; worker-src 'none'; manifest-src 'none'; sandbox allow-scripts";

/** Scripts run. Same-origin, popups, and top navigation stay off. */
export const PREVIEW_SANDBOX = "allow-scripts";

/** Tauri serves `htmlpreview` as `http://htmlpreview.localhost` on Windows. */
export function htmlPreviewSrc(name: string, windows: boolean): string {
  const query = `name=${encodeURIComponent(name)}`;
  if (windows) return `http://htmlpreview.localhost/preview?${query}`;
  return `htmlpreview://localhost/preview?${query}`;
}
