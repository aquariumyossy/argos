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
};

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

export function bytesToBase64(bytes: Uint8Array): string {
  const chunk = 0x8000;
  let binary = "";
  for (let i = 0; i < bytes.length; i += chunk) {
    const slice = bytes.subarray(i, i + chunk);
    binary += String.fromCharCode(...slice);
  }
  return btoa(binary);
}
