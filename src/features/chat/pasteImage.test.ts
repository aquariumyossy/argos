import { describe, expect, it } from "vitest";
import { fileLabel } from "./sourceGroups";
import {
  IMAGE_MAX_BYTES,
  PASTE_WHILE_GENERATING,
  PASTE_WHILE_READING,
  bytesToBase64,
  chooseClipboardPaste,
  clipboardImages,
  pasteBlockedMessage,
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

  it("pastes text when a picture rides along, and keeps a lone copied image", () => {
    const shot = imageFile([9, 8, 7]);
    const cells: ClipboardLike = {
      files: [shot],
      getData: (type) => {
        if (type === "text/plain") return "売上\t12";
        if (type === "text/html") return "<table><tr><td>売上</td><td>12</td></tr></table>";
        return "";
      },
    };
    expect(chooseClipboardPaste(cells)).toEqual({ kind: "text" });
    const copiedImage: ClipboardLike = {
      files: [shot],
      getData: (type) => {
        if (type === "text/plain") return "https://example.com/koseki.png";
        if (type === "text/html") {
          return "<html><body><!--StartFragment--><img src=\"https://example.com/koseki.png\" alt=\"戸籍\"/><!--EndFragment--></body></html>";
        }
        return "";
      },
    };
    expect(chooseClipboardPaste(copiedImage)).toEqual({ kind: "image", files: [shot] });
    expect(chooseClipboardPaste({ files: [shot], getData: () => "" })).toEqual({
      kind: "image",
      files: [shot],
    });
    expect(chooseClipboardPaste({ files: [shot], getData: () => "   " })).toEqual({
      kind: "image",
      files: [shot],
    });
  });

  it("names generation separately from a pre-read", () => {
    expect(pasteBlockedMessage(true, true)).toBe(PASTE_WHILE_GENERATING);
    expect(pasteBlockedMessage(true, false)).toBe(PASTE_WHILE_GENERATING);
    expect(pasteBlockedMessage(false, true)).toBe(PASTE_WHILE_READING);
    expect(pasteBlockedMessage(false, false)).toBeNull();
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
