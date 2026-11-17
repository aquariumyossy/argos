import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import {
  authoredToolTexts,
  foreignCharNotice,
  foreignCharNoticeForAssistant,
  foreignChars,
  isCjkIdeograph,
  isJapaneseHan,
} from "./japaneseHan";
import { HAN_BMP_A, HAN_BMP_B, HAN_EXTRA } from "./japaneseHanData";

describe("foreignChars", () => {
  it("ignores Japanese legal prose", () => {
    expect(foreignChars("第1条　甲は乙に対し、瑕疵を理由に契約を解除できる。")).toEqual([]);
    expect(foreignChars("損害賠償　原告　被告　謄本　捺印")).toEqual([]);
  });

  it("flags simplified Chinese that is not Japanese running text", () => {
    // 个 is a JIS X 0208 kanji, so it stays. 这问題 are not.
    expect(foreignChars("这个合同的问题")).toEqual(["这", "问", "题"]);
    expect(foreignChars("门发经问题")).toEqual(["门", "发", "经", "问", "题"]);
    // 从 is JIS X 0208. The other simplified forms are not.
    expect(foreignChars("为了对从开关")).toEqual(["为", "对", "开", "关"]);
    expect(foreignChars("们这说话门问题")).toEqual(["们", "这", "说", "话", "门", "问", "题"]);
  });

  it("flags hangul, which slipped into a contract as 기타", () => {
    expect(foreignChars("権利侵害の主張 기타 クレームについては")).toEqual(["기", "타"]);
    expect(foreignChars("계약")).toEqual(["계", "약"]);
    expect(foreignChars("ﾯ")).toEqual(["ﾯ"]);
  });

  it("flags other scripts a drifting model reaches for", () => {
    expect(foreignChars("Привет")).toEqual(["П", "р", "и", "в", "е", "т"]);
    expect(foreignChars("สัญญา").length).toBeGreaterThan(0);
  });

  it("flags Kangxi radicals that look like kanji but break search", () => {
    expect(foreignChars("⼀")).toEqual(["⼀"]);
    expect(foreignChars("一")).toEqual([]);
  });

  it("keeps shared shinjitai and Japanese kyujitai", () => {
    expect(foreignChars("国会学体来与門発國學")).toEqual([]);
  });

  it("keeps JIS X 0208 拡張新字体 that Unihan lists as simplified Chinese", () => {
    expect(foreignChars("嘘をつく")).toEqual([]);
    expect(foreignChars("侠客の躯を掴み、麹と蝉と泪")).toEqual([]);
    expect(isJapaneseHan("嘘".codePointAt(0)!)).toBe(true);
    expect(isJapaneseHan("噓".codePointAt(0)!)).toBe(true);
    expect(isJapaneseHan("个".codePointAt(0)!)).toBe(true);
  });

  it("ignores kana, punctuation, latin and greek", () => {
    expect(foreignChars("はい。OK 123 「引用」")).toEqual([]);
    expect(foreignChars("α版　μm　ＡＩツール　ｱｲｳ")).toEqual([]);
    expect(foreignChars("https://example.com/a?b=1")).toEqual([]);
    expect(isCjkIdeograph("あ".codePointAt(0)!)).toBe(false);
  });

  it("dedupes in first-seen order across parts", () => {
    expect(foreignChars("这这", "问这")).toEqual(["这", "问"]);
  });

  it("keeps JIS X 0208 kanji that look simplified, and still flags the rest", () => {
    expect(isJapaneseHan("個".codePointAt(0)!)).toBe(true);
    expect(isJapaneseHan("个".codePointAt(0)!)).toBe(true);
    expect(isJapaneseHan("无".codePointAt(0)!)).toBe(true);
    expect(isJapaneseHan("从".codePointAt(0)!)).toBe(true);
    expect(isJapaneseHan("广".codePointAt(0)!)).toBe(true);
    for (const ch of ["们", "这", "说", "话", "门", "问", "题"]) {
      expect(isJapaneseHan(ch.codePointAt(0)!)).toBe(false);
    }
  });
});

describe("foreignCharNotice", () => {
  it("returns null when nothing is foreign", () => {
    expect(foreignCharNotice("条項を点検しました。")).toBeNull();
  });

  it("lists the glyphs for the chat pane", () => {
    expect(foreignCharNotice("这个条款。")).toBe("日本語で用いない文字: 这");
    expect(foreignCharNotice("主張 기타 クレーム")).toBe("日本語で用いない文字: 기、타");
  });

  it("reads model-authored tool arguments and skips find strings", () => {
    const calls = JSON.stringify([
      {
        function: {
          name: "edit_office",
          arguments: JSON.stringify({
            replacements: [{ find: "最高人民法院", replace: "本契約", comment: "这个" }],
          }),
        },
      },
    ]);
    expect(foreignCharNoticeForAssistant("条項です。", calls)).toBe("日本語で用いない文字: 这");
    expect(
      authoredToolTexts("edit_office", {
        replacements: [{ find: "这个", replace: "本文" }],
      }),
    ).toEqual(["本文"]);
  });
});

describe("JIS and CP932 allowlist", () => {
  it("keeps 髙 and 𠮷, and still flags hangul", () => {
    expect(foreignChars("髙橋𠮷田")).toEqual([]);
    expect(isJapaneseHan(0x9ad9)).toBe(true);
    expect(isJapaneseHan(0x20bb7)).toBe(true);
    expect(foreignChars("기타")).toEqual(["기", "타"]);
  });

  it("matches the Rust allowlist byte for byte", () => {
    const rust = readFileSync(resolve("src-tauri/src/llm/japanese_han_data.rs"), "utf8");
    const quoted = (name: string) => {
      const at = rust.indexOf(`pub const ${name}:`);
      const rest = rust.slice(at);
      const body = rest.slice(rest.indexOf('r"') + 2);
      return body.slice(0, body.indexOf('"'));
    };
    expect(quoted("HAN_BMP_A")).toBe(HAN_BMP_A);
    expect(quoted("HAN_BMP_B")).toBe(HAN_BMP_B);
    const header = "HAN_EXTRA: &[u32] = &[";
    const start = rust.indexOf(header) + header.length;
    const list = rust.slice(start, rust.indexOf("];", start));
    const nums = [...list.matchAll(/\d+/g)].map((hit) => Number(hit[0]));
    expect(nums).toEqual([...HAN_EXTRA]);
  });
});
