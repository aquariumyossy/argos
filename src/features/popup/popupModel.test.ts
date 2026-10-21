import { describe, expect, it } from "vitest";
import type { SearchHit } from "../preview/types";
import {
  breadcrumbForPath,
  decidePopupKey,
  extFilterShortLabel,
  folderFilterLabel,
  hitLocation,
  INPUT_SUGGEST_HINT,
  isInternalIdPath,
  SUGGEST_OVERLAY_HINT,
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
    caretInInput: true,
    resultNavStarted: false,
    suggestionsReady: true,
    ...partial,
  };
}

describe("suggestion keyboard", () => {
  it("Enter while the overlay is open only closes it", () => {
    expect(decidePopupKey(key({ key: "Enter" }), ctx({ suggestOpen: true }))).toBe(
      "suggest-enter-close",
    );
    expect(
      decidePopupKey(
        key({ key: "Enter" }),
        ctx({ suggestOpen: true, suggestionCount: 0 }),
      ),
    ).toBe("suggest-enter-close");
  });

  it("modified Enter while the overlay is open does not open a result", () => {
    expect(
      decidePopupKey(key({ key: "Enter", ctrlKey: true }), ctx({ suggestOpen: true })),
    ).toBe("suggest-enter-swallow");
    expect(
      decidePopupKey(key({ key: "Enter", shiftKey: true }), ctx({ suggestOpen: true })),
    ).toBe("suggest-enter-swallow");
  });

  it("Enter after the overlay is closed opens the selected result", () => {
    expect(decidePopupKey(key({ key: "Enter" }), ctx({ suggestOpen: false }))).toBe(
      "open-result",
    );
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
        ctx({ suggestOpen: true }),
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
    expect(decidePopupKey(key({ key: "Tab" }), ctx({ suggestOpen: true }))).toBe(
      "suggest-tab",
    );
    expect(decidePopupKey(key({ key: "ArrowDown" }), ctx({ suggestOpen: true }))).toBe(
      "suggest-down",
    );
    expect(decidePopupKey(key({ key: "ArrowUp" }), ctx({ suggestOpen: true }))).toBe(
      "suggest-up",
    );
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
