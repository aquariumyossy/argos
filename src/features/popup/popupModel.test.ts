import { describe, expect, it } from "vitest";
import type { SearchHit } from "../preview/types";
import {
  breadcrumbForPath,
  decidePopupKey,
  extFilterShortLabel,
  folderFilterLabel,
  hitLocation,
  INPUT_SUGGEST_HINT,
  inputSuggestHintVisible,
  isInternalIdPath,
  SUGGEST_OVERLAY_HINT,
  SUGGEST_OVERLAY_SEARCH_HINT,
  suggestOverlayHint,
  type PopupKeyContext,
  type PopupKeyEvent,
} from "./popupModel";

const ROOT = "\\\\FILES\\cases\\訴訟案件";
const FILE =
  "\\\\FILES\\cases\\訴訟案件\\4. 訴え提起〜結審前\\甲野商事 売掛金請求事件\\提出書面\\準備書面（2）.docx";

function key(partial: Partial<PopupKeyEvent> & Pick<PopupKeyEvent, "key">): PopupKeyEvent {
  return {
    ctrlKey: false,
    metaKey: false,
    shiftKey: false,
    altKey: false,
    isComposing: false,
    keyCode: 0,
    ...partial,
  };
}

function ctx(partial: Partial<PopupKeyContext> = {}): PopupKeyContext {
  return {
    imeComposing: false,
    selectChip: false,
    folderPickerOpen: false,
    extPickerOpen: false,
    datePickerOpen: false,
    suggestOpen: false,
    suggestionCount: 2,
    query: "売掛金",
    suggestionHighlightActive: false,
    caretInInput: true,
    resultNavStarted: false,
    suggestionsReady: true,
    ...partial,
  };
}

describe("suggestion keyboard", () => {
  it("Enter while the overlay is open only closes it when the query is not empty", () => {
    expect(decidePopupKey(key({ key: "Enter" }), ctx({ suggestOpen: true }))).toBe(
      "suggest-enter-close",
    );
    expect(
      decidePopupKey(
        key({ key: "Enter" }),
        ctx({ suggestOpen: true, suggestionHighlightActive: true }),
      ),
    ).toBe("suggest-enter-close");
  });

  it("empty-query Enter searches only the history item the user moved onto", () => {
    const open = ctx({ suggestOpen: true, query: "", suggestionHighlightActive: false });
    expect(decidePopupKey(key({ key: "Enter" }), open)).toBe("suggest-enter-close");
    expect(decidePopupKey(key({ key: "Enter" }), open)).not.toBe("open-result");
    expect(
      decidePopupKey(
        key({ key: "Enter" }),
        ctx({ suggestOpen: true, query: "  ", suggestionHighlightActive: true }),
      ),
    ).toBe("suggest-enter-search");
    expect(
      decidePopupKey(
        key({ key: "Enter" }),
        ctx({ suggestOpen: true, query: "", suggestionHighlightActive: true }),
      ),
    ).toBe("suggest-enter-search");
  });

  it("does not swallow Enter when the overlay is not visible", () => {
    expect(
      decidePopupKey(
        key({ key: "Enter" }),
        ctx({ suggestOpen: true, suggestionCount: 0, query: "", suggestionHighlightActive: true }),
      ),
    ).toBe("open-result");
    expect(
      decidePopupKey(
        key({ key: "Enter" }),
        ctx({ suggestOpen: true, selectChip: true, query: "", suggestionHighlightActive: true }),
      ),
    ).toBe("open-result");
  });

  it("modified Enter while the overlay is open does not open a result or run a search", () => {
    const highlighted = ctx({
      suggestOpen: true,
      query: "",
      suggestionHighlightActive: true,
    });
    for (const extra of [
      { ctrlKey: true },
      { shiftKey: true },
      { altKey: true },
      { metaKey: true },
    ] as const) {
      expect(decidePopupKey(key({ key: "Enter", ...extra }), highlighted)).toBe(
        "suggest-enter-swallow",
      );
    }
    expect(
      decidePopupKey(key({ key: "Enter", ctrlKey: true }), ctx({ suggestOpen: true })),
    ).toBe("suggest-enter-swallow");
  });

  it("Enter after the overlay is closed opens the selected result", () => {
    expect(decidePopupKey(key({ key: "Enter" }), ctx({ suggestOpen: false }))).toBe(
      "open-result",
    );
    expect(
      decidePopupKey(
        key({ key: "Enter" }),
        ctx({ suggestOpen: false, query: "", suggestionHighlightActive: true }),
      ),
    ).toBe("open-result");
  });

  it("Down from the input opens suggestions until result navigation starts", () => {
    expect(decidePopupKey(key({ key: "ArrowDown" }), ctx())).toBe("open-suggest");
    expect(
      decidePopupKey(key({ key: "ArrowDown" }), ctx({ caretInInput: false })),
    ).toBe("result-down");
    expect(
      decidePopupKey(key({ key: "ArrowDown" }), ctx({ resultNavStarted: true })),
    ).toBe("result-down");
    expect(decidePopupKey(key({ key: "ArrowUp" }), ctx())).toBe("result-up");
  });

  it("Down moves the result when there are no suggestions", () => {
    expect(
      decidePopupKey(
        key({ key: "ArrowDown" }),
        ctx({ suggestionsReady: true, suggestionCount: 0 }),
      ),
    ).toBe("result-down");
  });

  it("ignores Enter during IME composition", () => {
    expect(
      decidePopupKey(
        key({ key: "Enter", isComposing: true }),
        ctx({ suggestOpen: true, query: "", suggestionHighlightActive: true }),
      ),
    ).toBe("ignore");
    expect(
      decidePopupKey(key({ key: "Enter", keyCode: 229 }), ctx({ suggestOpen: false })),
    ).toBe("ignore");
    expect(
      decidePopupKey(key({ key: "Enter" }), ctx({ imeComposing: true, suggestOpen: true })),
    ).toBe("ignore");
    expect(
      decidePopupKey(key({ key: "ArrowDown", isComposing: true }), ctx()),
    ).toBe("ignore");
  });

  it("Tab still confirms a suggestion and arrows move within the overlay", () => {
    const open = ctx({ suggestOpen: true, query: "" });
    expect(decidePopupKey(key({ key: "Tab" }), open)).toBe("suggest-tab");
    expect(decidePopupKey(key({ key: "ArrowDown" }), open)).toBe("suggest-highlight-first");
    expect(decidePopupKey(key({ key: "ArrowUp" }), open)).toBe("suggest-highlight-last");
    const moved = ctx({ suggestOpen: true, query: "", suggestionHighlightActive: true });
    expect(decidePopupKey(key({ key: "ArrowDown" }), moved)).toBe("suggest-down");
    expect(decidePopupKey(key({ key: "ArrowUp" }), moved)).toBe("suggest-up");
    expect(decidePopupKey(key({ key: "Tab" }), moved)).toBe("suggest-tab");
  });
});

describe("input suggest hint", () => {
  it("hides the hint when Down cannot reopen suggestions", () => {
    expect(
      inputSuggestHintVisible({
        overlayVisible: false,
        resultNavStarted: false,
        suggestionsReady: true,
        suggestionCount: 2,
      }),
    ).toBe(true);
    expect(
      inputSuggestHintVisible({
        overlayVisible: false,
        resultNavStarted: true,
        suggestionsReady: true,
        suggestionCount: 2,
      }),
    ).toBe(false);
    expect(
      inputSuggestHintVisible({
        overlayVisible: false,
        resultNavStarted: false,
        suggestionsReady: true,
        suggestionCount: 0,
      }),
    ).toBe(false);
    expect(
      inputSuggestHintVisible({
        overlayVisible: true,
        resultNavStarted: false,
        suggestionsReady: false,
        suggestionCount: 2,
      }),
    ).toBe(false);
  });

  it("says Enter searches only after an empty-query history item is highlighted", () => {
    expect(suggestOverlayHint("売掛金", true)).toBe(SUGGEST_OVERLAY_HINT);
    expect(suggestOverlayHint("", false)).toBe(SUGGEST_OVERLAY_HINT);
    expect(suggestOverlayHint("", true)).toBe(SUGGEST_OVERLAY_SEARCH_HINT);
  });
});

describe("hit location", () => {
  it("builds the registered-folder breadcrumb and drops deeper folders", () => {
    expect(breadcrumbForPath(FILE, [ROOT])).toBe(
      "訴訟案件 › 4. 訴え提起〜結審前 › 甲野商事 売掛金請求事件",
    );
  });

  it("falls back by skipping the UNC server and share", () => {
    expect(breadcrumbForPath(FILE, [])).toBe(
      "訴訟案件 › 4. 訴え提起〜結審前 › 甲野商事 売掛金請求事件",
    );
  });

  it("never uses an outlook id as the visible location", () => {
    expect(isInternalIdPath("outlook:0000ABCDEF")).toBe(true);
    const hit: SearchHit = {
      id: "outlook:0000ABCDEF",
      title: "売掛金の件",
      snippet: "売掛金のご請求",
      path: "outlook:0000ABCDEF",
      score: 1,
      source: "outlook",
      previewText: "",
      mailFrom: "甲野",
      mailFolder: "user@firm/受信トレイ",
      docKind: "email",
    };
    const loc = hitLocation(hit, []);
    expect(loc.text).not.toContain("outlook:");
    expect(loc.title).not.toContain("outlook:");
    expect(loc.text).toContain("受信トレイ");
  });
});

describe("filter labels", () => {
  it("shortens file types and folder names for the chips", () => {
    expect(extFilterShortLabel(["docx", "doc", "pdf"])).toBe("Word・PDF");
    expect(folderFilterLabel(`${ROOT}\\4. 訴え提起〜結審前`, "4. 訴え提起〜結審前")).toBe(
      "4. 訴え提起〜結審前",
    );
  });

  it("exposes the hint strings used by the overlay and the input", () => {
    expect(SUGGEST_OVERLAY_HINT).toContain("Enter で閉じる");
    expect(INPUT_SUGGEST_HINT).toBe("↓で履歴候補");
  });
});
