/** Same ceiling as `IMAGE_MAX_BYTES` in the Rust file store. */
export const IMAGE_MAX_BYTES = 8 * 1024 * 1024;

const IMAGE_MIMES = new Set([
  "image/png",
  "image/jpeg",
  "image/jpg",
  "image/gif",
  "image/webp",
]);

export function imageMime(raw: string): string {
  return raw.split(";")[0]?.trim().toLowerCase() ?? "";
}

export function pastedImageTypeError(mime: string): string | null {
  if (IMAGE_MIMES.has(imageMime(mime))) return null;
  return "対応していない形式です。";
}

export function pastedImageSizeError(byteLength: number): string | null {
  if (!Number.isFinite(byteLength) || byteLength <= 0) {
    return "画像ファイルが空です。";
  }
  if (byteLength > IMAGE_MAX_BYTES) {
    return "画像が大きすぎます（8 MBまで）。";
  }
  return null;
}

export type ClipboardImageFile = {
  type: string;
  size: number;
  arrayBuffer: () => Promise<ArrayBuffer>;
};

type ClipboardItemLike = {
  kind: string;
  type: string;
  getAsFile?: () => ClipboardImageFile | null;
};

export type ClipboardLike = {
  files?: ArrayLike<ClipboardImageFile | null>;
  items?: ArrayLike<ClipboardItemLike>;
  getData?: (type: string) => string;
};

export const PASTE_WHILE_READING =
  "読み取り中のため貼り付けできませんでした。終わってから貼り直してください。";

export const PASTE_WHILE_GENERATING =
  "生成中のため貼り付けできませんでした。終わってから貼り直してください。";

/** Why an image paste cannot be attached. Generation wins when both are true. */
export function pasteBlockedMessage(
  generating: boolean,
  reading: boolean,
): string | null {
  if (generating) return PASTE_WHILE_GENERATING;
  if (reading) return PASTE_WHILE_READING;
  return null;
}

function isImageFile(
  file: ClipboardImageFile | null | undefined,
): file is ClipboardImageFile {
  return !!file && imageMime(file.type).startsWith("image/");
}

/** Image files on the clipboard. Text-only clipboards return an empty list. */
export function clipboardImages(
  data: ClipboardLike | null | undefined,
): ClipboardImageFile[] {
  if (!data) return [];
  const fromFiles: ClipboardImageFile[] = [];
  if (data.files) {
    for (const file of Array.from(data.files)) {
      if (isImageFile(file)) fromFiles.push(file);
    }
  }
  if (fromFiles.length > 0) return fromFiles;
  const fromItems: ClipboardImageFile[] = [];
  if (data.items) {
    for (const item of Array.from(data.items)) {
      if (item.kind !== "file" || !imageMime(item.type).startsWith("image/")) {
        continue;
      }
      const file = item.getAsFile?.() ?? null;
      if (isImageFile(file)) fromItems.push(file);
    }
  }
  return fromItems;
}

function clipboardFormat(data: ClipboardLike, type: string): string {
  try {
    return data.getData?.(type) ?? "";
  } catch {
    return "";
  }
}

/**
 * Browser "copy image" stores one img and no other visible text in text/html,
 * often wrapped in a fragment comment. Excel and PowerPoint also put a picture
 * next to real text, and that HTML is not a lone image.
 */
export function htmlIsSingleImage(html: string): boolean {
  const stripped = html
    .replace(/<!--[\s\S]*?-->/g, "")
    .replace(/<script[\s\S]*?<\/script>/gi, "")
    .replace(/<style[\s\S]*?<\/style>/gi, "");
  const images = stripped.match(/<img\b/gi);
  if (!images || images.length !== 1) return false;
  const text = stripped
    .replace(/<img\b[^>]*>/gi, "")
    .replace(/<[^>]+>/g, "")
    .replace(/&nbsp;|&#160;/gi, " ")
    .replace(/\s+/g, "");
  return text.length === 0;
}

export type PasteDecision =
  | { kind: "text" }
  | { kind: "image"; files: ClipboardImageFile[] };

/**
 * Text wins when the clipboard has both a picture and non-empty text/plain.
 * A lone screenshot, or HTML that is only one image, is attached instead.
 */
export function chooseClipboardPaste(
  data: ClipboardLike | null | undefined,
): PasteDecision {
  const images = clipboardImages(data);
  const html = data ? clipboardFormat(data, "text/html") : "";
  if (images.length > 0 && htmlIsSingleImage(html)) {
    return { kind: "image", files: images };
  }
  const text = (data ? clipboardFormat(data, "text/plain") : "").trim();
  if (text.length > 0) return { kind: "text" };
  if (images.length > 0) return { kind: "image", files: images };
  return { kind: "text" };
}

export function bytesToBase64(bytes: Uint8Array): string {
  const chunk = 0x8000;
  let binary = "";
  for (let i = 0; i < bytes.length; i += chunk) {
    const slice = bytes.subarray(i, i + chunk);
    binary += String.fromCharCode(...slice);
  }
  return btoa(binary);
}
