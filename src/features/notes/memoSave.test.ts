import { describe, expect, it } from "vitest";
import { finishMemoWrite, settleMemoSave, type PendingMemo } from "./memoSave";

const draft: PendingMemo = { id: "n1", memo: "下書き" };
const restored: PendingMemo = { id: "n1", memo: "戻した本文" };

describe("settleMemoSave", () => {
  it("applies a save that is still the current edit", () => {
    expect(settleMemoSave(3, 3, draft, null)).toEqual({
      applyLocal: true,
      rewrite: null,
    });
  });

  it("drops a save superseded by a restore whose text is not known yet", () => {
    expect(settleMemoSave(3, 4, draft, null)).toEqual({
      applyLocal: false,
      rewrite: null,
    });
  });

  it("rewrites when a stale draft would replace the restored memo", () => {
    expect(settleMemoSave(3, 4, draft, restored)).toEqual({
      applyLocal: false,
      rewrite: restored,
    });
  });

  it("does not rewrite when the saved text is already the restored memo", () => {
    expect(settleMemoSave(3, 4, restored, restored)).toEqual({
      applyLocal: false,
      rewrite: null,
    });
  });

  it("keeps an edit that started after the restore", () => {
    expect(settleMemoSave(4, 4, draft, restored)).toEqual({
      applyLocal: true,
      rewrite: null,
    });
  });
});

describe("finishMemoWrite", () => {
  it("writes the restored memo back over a stale draft", async () => {
    const writes: string[] = [];
    const outcome = await finishMemoWrite(1, 2, draft, restored, async (memo) => {
      writes.push(memo.memo);
    });
    expect(outcome).toBe("rewritten");
    expect(writes).toEqual(["戻した本文"]);
  });

  it("does not write again when the save is still current", async () => {
    const writes: string[] = [];
    const outcome = await finishMemoWrite(1, 1, draft, null, async (memo) => {
      writes.push(memo.memo);
    });
    expect(outcome).toBe("apply");
    expect(writes).toEqual([]);
  });
});
