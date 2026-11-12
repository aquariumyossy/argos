import { describe, expect, it } from "vitest";
import { fileLabel } from "./sourceGroups";
import {
  IMAGE_MAX_BYTES,
  bytesToBase64,
  clipboardImages,
  pastedImageSizeError,
  pastedImageTypeError,
  type ClipboardImageFile,
  type ClipboardLike,
} from "./pasteImage";

function imageFile(
  bytes: number[],
  type = "image/png",
  size = bytes.length,
): ClipboardImageFile {
  const data = Uint8Array.from(bytes);
  return {
    type,
    size,
    arrayBuffer: async () => data.buffer.slice(data.byteOffset, data.byteOffset + data.byteLength),
  };
}

describe("clipboard paste", () => {
  it("leaves a text-only clipboard as text", () => {
    const data: ClipboardLike = {
      files: [],
      items: [
        {
          kind: "string",
          type: "text/plain",
          getAsFile: () => null,
        },
      ],
    };
    expect(clipboardImages(data)).toEqual([]);
    expect(clipboardImages(null)).toEqual([]);
  });

  it("takes an image from files or from a clipboard item", () => {
    const shot = imageFile([137, 80, 78, 71]);
    expect(clipboardImages({ files: [shot], items: [] })).toEqual([shot]);
    const fromItem = imageFile([1, 2, 3], "image/jpeg");
    expect(
      clipboardImages({
        files: [],
        items: [{ kind: "file", type: "image/jpeg", getAsFile: () => fromItem }],
      }),
    ).toEqual([fromItem]);
    expect(
      clipboardImages({
        files: [shot],
        items: [{ kind: "file", type: "image/jpeg", getAsFile: () => fromItem }],
      }),
    ).toEqual([shot]);
  });

  it("uses the same 8MB limit and the paste label as a filename", () => {
    expect(pastedImageSizeError(1)).toBeNull();
    expect(pastedImageSizeError(IMAGE_MAX_BYTES)).toBeNull();
    expect(pastedImageSizeError(0)).toBe("画像ファイルが空です。");
    expect(pastedImageSizeError(IMAGE_MAX_BYTES + 1)).toBe(
      "画像が大きすぎます（8 MBまで）。",
    );
    expect(pastedImageTypeError("image/png")).toBeNull();
    expect(pastedImageTypeError("image/jpeg; charset=binary")).toBeNull();
    expect(pastedImageTypeError("image/svg+xml")).toBe("対応していない形式です。");
    const title = "貼り付け画像（2026-11-12 11時59分01秒）";
    expect(fileLabel({ path: title, title })).toBe(title);
    expect(bytesToBase64(Uint8Array.from([137, 80, 78, 71]))).toBe(
      btoa(String.fromCharCode(137, 80, 78, 71)),
    );
  });
});
