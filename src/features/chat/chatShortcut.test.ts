import { describe, expect, it } from "vitest";
import {
  composerFocusLanded,
  planChatShortcut,
  shortcutInputLocked,
} from "./chatShortcut";

describe("chat shortcut", () => {
  it("focuses the conversation already open", () => {
    expect(
      planChatShortcut({
        threadId: "a",
        activeId: "a",
        knownThreadIds: ["a", "b"],
      }),
    ).toEqual({ action: "focus" });
  });

  it("selects another conversation that is already in the list", () => {
    expect(
      planChatShortcut({
        threadId: "b",
        activeId: "a",
        knownThreadIds: ["a", "b"],
      }),
    ).toEqual({ action: "select", threadId: "b" });
  });

  it("reloads when the shortcut names a conversation the screen does not have yet", () => {
    expect(
      planChatShortcut({
        threadId: "new",
        activeId: null,
        knownThreadIds: [],
      }),
    ).toEqual({ action: "reload" });
  });

  it("starts an empty conversation only when none exists and none was named", () => {
    expect(
      planChatShortcut({
        threadId: "",
        activeId: null,
        knownThreadIds: [],
      }),
    ).toEqual({ action: "create" });
    expect(
      planChatShortcut({
        threadId: "  ",
        activeId: "a",
        knownThreadIds: ["a"],
      }),
    ).toEqual({ action: "focus" });
  });
});

describe("shortcut focus", () => {
  it("counts as landed only when the window and the input both have focus", () => {
    const input = new EventTarget();
    const other = new EventTarget();
    expect(composerFocusLanded(true, input, input)).toBe(true);
    expect(composerFocusLanded(false, input, input)).toBe(false);
    expect(composerFocusLanded(true, other, input)).toBe(false);
    expect(composerFocusLanded(true, input, null)).toBe(false);
  });

  it("locks the input while a reply or an attachment is in progress", () => {
    expect(shortcutInputLocked(false, false)).toBe(false);
    expect(shortcutInputLocked(true, false)).toBe(true);
    expect(shortcutInputLocked(false, true)).toBe(true);
  });
});
