import { describe, expect, it } from "vitest";
import {
  applyHanMap,
  authoredToolTexts,
  foreignCharNotice,
  foreignCharNoticeForAssistant,
  foreignChars,
  HAN_REWRITE_NOTE,
  HAN_REWRITE_STOPPED_NOTE,
  hanMapNote,
  hanMarkedHtml,
  isCjkIdeograph,
  isJapaneseHan,
  markHanFixes,
  parseScriptFix,
  asksGenericTranslation,
  foreignNoticeText,
  namesForeignScript,
  replyIsMostlyForeign,
  shouldSkipForeignFix,
} from "./japaneseHan";

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

describe("applyHanMap", () => {
  it("maps 现在 to 現在 and leaves Japanese, Latin, and Greek", () => {
    expect(applyHanMap("现在")).toBe("現在");
    expect(applyHanMap("说")).toBe("説");
    expect(applyHanMap("第1条　甲は乙に対し、瑕疵を理由に契約を解除できる。")).toBe(
      "第1条　甲は乙に対し、瑕疵を理由に契約を解除できる。",
    );
    expect(applyHanMap("α版　μm　OK 123")).toBe("α版　μm　OK 123");
    expect(applyHanMap("发")).toBe("发");
    expect(applyHanMap("国学个干后面里")).toBe("国学个干后面里");
    expect(applyHanMap("线")).toBe("線");
    expect(applyHanMap("举")).toBe("挙");
    expect(applyHanMap("鉴")).toBe("鑑");
    expect(applyHanMap("净")).toBe("浄");
    expect(applyHanMap("账")).toBe("帳");
  });

  it("describes the places that were replaced", () => {
    expect(
      hanMapNote([
        { at: 0, from: "现", to: "現" },
        { at: 3, from: "说", to: "説" },
        { at: 8, from: "现", to: "現" },
      ]),
    ).toBe("现→現 など3か所を直しました");
    expect(hanMapNote([])).toBeNull();
    expect(HAN_REWRITE_NOTE).toBe("AIが外国文字を含む部分を書き直しました");
    expect(HAN_REWRITE_STOPPED_NOTE).toBe("書き直しを中止しました");
    expect(parseScriptFix("")).toBeNull();
    expect(parseScriptFix('{"fixes":[{"at":0,"from":"现","to":"現"}],"rewritten":true}')).toEqual({
      fixes: [{ at: 0, from: "现", to: "現" }],
      rewritten: true,
      rewriteStopped: false,
      suppressWarning: false,
    });
    expect(namesForeignScript("中国語に訳して")).toBe(true);
    expect(namesForeignScript("簡体字で書いて")).toBe(true);
    expect(namesForeignScript("繁体字に直して")).toBe(true);
    expect(namesForeignScript("ハングルで")).toBe(true);
    expect(namesForeignScript("Please write this in Chinese")).toBe(true);
    expect(namesForeignScript("中国語で書いて")).toBe(true);
    expect(namesForeignScript("中国語で答えて")).toBe(true);
    expect(namesForeignScript("中文に翻訳")).toBe(true);
    expect(namesForeignScript("韓国語訳")).toBe(true);
    expect(namesForeignScript("into Chinese")).toBe(true);
    expect(namesForeignScript("in Korean")).toBe(true);
    expect(namesForeignScript("translate this to Chinese")).toBe(true);
    expect(namesForeignScript("翻訳会社との契約")).toBe(false);
    expect(namesForeignScript("このメールを中国語にしてください")).toBe(true);
    expect(namesForeignScript("簡体字にして")).toBe(true);
    expect(namesForeignScript("中文版を作って")).toBe(true);
    expect(namesForeignScript("한국어로 번역해 주세요")).toBe(true);
    expect(namesForeignScript("한국말로")).toBe(true);
    expect(namesForeignScript("この中国語の契約書の要点を日本語で教えて")).toBe(true);
    expect(namesForeignScript("中文版と日文版のどちらが優先？")).toBe(true);
    expect(namesForeignScript("Chinese wall の運用は？")).toBe(true);
    expect(namesForeignScript("中国語から日本語に訳して")).toBe(true);
    expect(asksGenericTranslation("和訳して")).toBe(true);
    expect(asksGenericTranslation("Please translate this")).toBe(true);
    expect(asksGenericTranslation("契約を要約して")).toBe(false);
    const chinese =
      "本合同自签订之日起生效，双方应当遵守本合同的全部条款并及时履行各自的义务。";
    const japanese = "第5条は、契约が现在も有効であることを定めています。甲は乙に対し通知しなければなりません。";
    expect(replyIsMostlyForeign(chinese)).toBe(true);
    expect(replyIsMostlyForeign(japanese)).toBe(false);
    expect(shouldSkipForeignFix("この英文契約の第5条を和訳して", japanese)).toBe(false);
    expect(shouldSkipForeignFix("翻訳会社との業務委託契約の注意点は？", japanese)).toBe(false);
    expect(shouldSkipForeignFix("第1条を中国語に訳して\n第2条も同様に", chinese)).toBe(true);
    expect(shouldSkipForeignFix("中国語に訳して", japanese)).toBe(false);
    expect(shouldSkipForeignFix("この中国語の契約書の要点を日本語で", japanese)).toBe(false);
    const mixed = "以下が中国語版です。\n本合同自签订之日起生效";
    expect(shouldSkipForeignFix("このメールを中国語にしてください", mixed)).toBe(false);
    expect(foreignNoticeText("このメールを中国語にしてください", mixed)).toBe("以下が中国語版です。");
    expect(foreignNoticeText("このメールを中国語にしてください", chinese)).toBe("");
    expect(foreignNoticeText("韓国語に訳して", "韓国語訳は次のとおりです。\n안녕하세요")).toBe(
      "韓国語訳は次のとおりです。",
    );
    expect(foreignNoticeText("中国語に訳して", `${chinese}\n（注：现の訳は参考です。）`)).toBe(
      "（注：现の訳は参考です。）",
    );
    expect(foreignNoticeText("中国語に訳して", "・签订之日起生效")).toBe("");
    expect(foreignNoticeText("中国語に訳して", "ー签订之日起生效")).toBe("");
    const table = "| 当事者の義務 | 当事人的义务 |";
    const cells = foreignNoticeText("中国語に訳して", table);
    expect(cells).toContain("当事者の義務");
    expect(cells).not.toContain("义");
    expect(cells).not.toContain("务");
    const toyota =
      "本合同由トヨタ自動車株式会社（以下称“甲方”）与上海某某有限公司…签订。";
    const panasonic = "본 계약은 パナソニック株式会社와 체결합니다.";
    expect(foreignNoticeText("このメールを中国語にしてください", toyota)).toBe("");
    expect(foreignNoticeText("한국어로 번역해 주세요", panasonic)).toBe("");
    expect(foreignNoticeText("このメールを中国語にしてください", "现在の契約")).toBe("现在の契約");
    const katakanaCell = "| トヨタ现在 | 当事者の现在 |";
    const katakanaCells = foreignNoticeText("このメールを中国語にしてください", katakanaCell);
    expect(katakanaCells).toContain("当事者の现在");
    expect(katakanaCells).not.toContain("トヨタ");
    expect(foreignNoticeText("", toyota)).toBe(toyota);
    expect(shouldSkipForeignFix("この中国語の契約書の要点を日本語で教えて", japanese)).toBe(
      false,
    );
    expect(shouldSkipForeignFix("中文版と日文版のどちらが優先？", japanese)).toBe(false);
    expect(shouldSkipForeignFix("Chinese wall の運用は？", japanese)).toBe(false);
    expect(applyHanMap("谭")).toBe("譚");
    expect(applyHanMap("岗")).toBe("崗");
    expect(applyHanMap("岗")).not.toBe("岡");
    expect(applyHanMap("箫")).toBe("簫");
    expect(applyHanMap("鲶")).toBe("鯰");
    expect(applyHanMap("恶")).toBe("恶");
    expect(applyHanMap("脏")).toBe("脏");
    expect(applyHanMap("够")).toBe("够");
    expect(applyHanMap("线")).toBe("線");
  });

  it("does not underline inside a URL or an HTML attribute", () => {
    const url = "参照 https://example.com/现 です";
    const at = [...url].findIndex((ch) => ch === "现");
    expect(markHanFixes(url, [{ at, from: "现", to: "現" }])).toBe(url);
    const html = `<a href="https://example.com/\uE000現\uE001">\uE000現\uE001</a>`;
    const marked = hanMarkedHtml(html);
    expect(marked).toContain('href="https://example.com/現"');
    expect(marked).toContain('<span class="chat-han-fix">現</span>');
    expect(marked).not.toContain("\uE000");
  });
});

describe("JIS and CP932 allowlist", () => {
  it("keeps 髙 and 𠮷, and still flags hangul", () => {
    expect(foreignChars("髙橋𠮷田")).toEqual([]);
    expect(isJapaneseHan(0x9ad9)).toBe(true);
    expect(isJapaneseHan(0x20bb7)).toBe(true);
    expect(foreignChars("기타")).toEqual(["기", "타"]);
  });
});
