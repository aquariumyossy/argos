import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  INITIAL_PROMPTS,
  SAVED_PROMPTS_KEY,
  SAVED_PROMPTS_SEEDED_KEY,
  forgetSavedPrompt,
  loadSavedPrompts,
  needsReplaceConfirm,
  rememberSavedPrompt,
  restoreSavedPrompts,
  reviseSavedPrompt,
} from "./savedPrompts";

const memory = new Map<string, string>();

function prompt(id: string, title: string, body: string, updatedAt: number) {
  return { id, title, body, updatedAt };
}

describe("saved prompts", () => {
  beforeEach(() => {
    memory.clear();
    vi.stubGlobal("localStorage", {
      getItem: (key: string) => memory.get(key) ?? null,
      setItem: (key: string, value: string) => {
        memory.set(key, value);
      },
      removeItem: (key: string) => {
        memory.delete(key);
      },
    });
    localStorage.setItem(SAVED_PROMPTS_SEEDED_KEY, "1");
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("stores a trimmed prompt and names it from the first line when the name is blank", () => {
    const result = rememberSavedPrompt({
      id: "a",
      title: "   ",
      body: "  一行目  \n二行目",
      now: 1,
    });
    expect(result).toEqual({
      ok: true,
      prompts: [prompt("a", "一行目", "一行目  \n二行目", 1)],
    });
    expect(loadSavedPrompts()).toEqual([prompt("a", "一行目", "一行目  \n二行目", 1)]);
  });

  it("keeps a name the user typed and cuts it at 40 characters", () => {
    const result = rememberSavedPrompt({
      id: "a",
      title: `  ${"条".repeat(45)}  `,
      body: "本文",
      now: 1,
    });
    expect(result).toEqual({
      ok: true,
      prompts: [prompt("a", "条".repeat(40), "本文", 1)],
    });
  });

  it("updates the same body in place and leaves a different prompt behind it", () => {
    rememberSavedPrompt({ id: "a", title: "旧", body: "同じ本文", now: 1 });
    rememberSavedPrompt({ id: "b", title: "間", body: "別の本文", now: 2 });
    const result = rememberSavedPrompt({
      id: "c",
      title: "新",
      body: "  同じ本文  ",
      now: 3,
    });
    expect(result).toEqual({
      ok: true,
      prompts: [prompt("a", "新", "同じ本文", 3), prompt("b", "間", "別の本文", 2)],
    });
    expect(loadSavedPrompts()).toEqual([
      prompt("a", "新", "同じ本文", 3),
      prompt("b", "間", "別の本文", 2),
    ]);
  });

  it("reads storage again, so a save from another window is kept", () => {
    rememberSavedPrompt({ id: "a", title: "先", body: "先の本文", now: 1 });
    localStorage.setItem(
      SAVED_PROMPTS_KEY,
      JSON.stringify([prompt("b", "別", "別の本文", 2)])
    );
    const result = rememberSavedPrompt({ id: "c", title: "追加", body: "追加の本文", now: 3 });
    expect(result).toEqual({
      ok: true,
      prompts: [prompt("c", "追加", "追加の本文", 3), prompt("b", "別", "別の本文", 2)],
    });
    expect(loadSavedPrompts().map((row) => row.body)).toEqual(["追加の本文", "別の本文"]);
  });

  it("refuses an empty body and leaves what was already stored", () => {
    rememberSavedPrompt({ id: "a", title: "残す", body: "残る", now: 1 });
    const result = rememberSavedPrompt({ id: "b", title: "空", body: "  \n  ", now: 2 });
    expect(result).toEqual({ ok: false, reason: "empty" });
    expect(loadSavedPrompts()).toEqual([prompt("a", "残す", "残る", 1)]);
  });

  it("refuses a body past 20,000 characters and leaves what was already stored", () => {
    rememberSavedPrompt({ id: "a", title: "残す", body: "残る", now: 1 });
    const kept = rememberSavedPrompt({
      id: "edge",
      title: "ちょうど",
      body: "あ".repeat(20_000),
      now: 2,
    });
    expect(kept).toEqual({
      ok: true,
      prompts: [prompt("edge", "ちょうど", "あ".repeat(20_000), 2), prompt("a", "残す", "残る", 1)],
    });
    const result = rememberSavedPrompt({
      id: "b",
      title: "大",
      body: "あ".repeat(20_001),
      now: 3,
    });
    expect(result).toEqual({ ok: false, reason: "tooLong" });
    expect(loadSavedPrompts().map((row) => row.id)).toEqual(["edge", "a"]);
  });

  it("drops the oldest prompt once the list passes 80", () => {
    for (let n = 1; n <= 81; n += 1) {
      rememberSavedPrompt({ id: `id${n}`, title: `名${n}`, body: `本文${n}`, now: n });
    }
    const saved = loadSavedPrompts();
    expect(saved[0]).toEqual(prompt("id81", "名81", "本文81", 81));
    expect(saved[79]).toEqual(prompt("id2", "名2", "本文2", 2));
    expect(saved.map((row) => row.body)).not.toContain("本文1");
  });

  it("treats broken storage as an empty list and skips a row that is not a prompt", () => {
    localStorage.setItem(SAVED_PROMPTS_KEY, "{");
    expect(loadSavedPrompts()).toEqual([]);
    localStorage.setItem(SAVED_PROMPTS_KEY, JSON.stringify({ prompts: [] }));
    expect(loadSavedPrompts()).toEqual([]);
    localStorage.setItem(
      SAVED_PROMPTS_KEY,
      JSON.stringify([
        prompt("ok", "名", "本文", 1),
        { id: "", title: "空id", body: "落ちる", updatedAt: 2 },
        { title: "idなし", body: "落ちる", updatedAt: 3 },
        { id: "blank", title: "空白", body: "   ", updatedAt: 4 },
        "nope",
      ])
    );
    expect(loadSavedPrompts()).toEqual([prompt("ok", "名", "本文", 1)]);
  });

  it("fills a blank stored name from the body and cuts a stored name at 40 characters", () => {
    localStorage.setItem(
      SAVED_PROMPTS_KEY,
      JSON.stringify([
        { id: "a", title: "  ", body: "  保存名  \n続き", updatedAt: 1 },
        { id: "b", title: "条".repeat(45), body: "本文", updatedAt: 2 },
      ])
    );
    expect(loadSavedPrompts()).toEqual([
      prompt("a", "保存名", "保存名  \n続き", 1),
      prompt("b", "条".repeat(40), "本文", 2),
    ]);
  });

  it("removes one prompt and does nothing for an id that is not stored", () => {
    rememberSavedPrompt({ id: "a", title: "甲", body: "甲の本文", now: 1 });
    rememberSavedPrompt({ id: "b", title: "乙", body: "乙の本文", now: 2 });
    expect(forgetSavedPrompt("missing")).toEqual([
      prompt("b", "乙", "乙の本文", 2),
      prompt("a", "甲", "甲の本文", 1),
    ]);
    expect(loadSavedPrompts().map((row) => row.id)).toEqual(["b", "a"]);
    expect(forgetSavedPrompt("a")).toEqual([prompt("b", "乙", "乙の本文", 2)]);
    expect(loadSavedPrompts()).toEqual([prompt("b", "乙", "乙の本文", 2)]);
  });

  it("asks before replacing a different draft and skips the ask when the draft is empty or the same", () => {
    expect(needsReplaceConfirm("", "本文")).toBe(false);
    expect(needsReplaceConfirm("   ", "本文")).toBe(false);
    expect(needsReplaceConfirm("  本文  ", "本文")).toBe(false);
    expect(needsReplaceConfirm("別の下書き", "本文")).toBe(true);
  });

  it("seeds the four initial prompts once and does not put a deleted one back", () => {
    localStorage.removeItem(SAVED_PROMPTS_SEEDED_KEY);
    expect(loadSavedPrompts()).toEqual(
      INITIAL_PROMPTS.map((spec) => prompt(spec.id, spec.title, spec.body, 0)),
    );
    expect(localStorage.getItem(SAVED_PROMPTS_SEEDED_KEY)).toBe("1");
    forgetSavedPrompt(INITIAL_PROMPTS[0].id);
    expect(loadSavedPrompts().map((row) => row.id)).toEqual(
      INITIAL_PROMPTS.slice(1).map((spec) => spec.id),
    );
    expect(loadSavedPrompts().map((row) => row.title)).toEqual(["要点", "要件事実", "時系列"]);
  });

  it("keeps prompts the user already saved when the initial four are added", () => {
    localStorage.removeItem(SAVED_PROMPTS_SEEDED_KEY);
    localStorage.setItem(
      SAVED_PROMPTS_KEY,
      JSON.stringify([
        prompt("u2", "後", "後の本文", 6),
        prompt("u1", "先", "先の本文", 5),
        prompt("copy", "写し", INITIAL_PROMPTS[0].body, 4),
      ]),
    );
    const loaded = loadSavedPrompts();
    expect(loaded.slice(0, 4)).toEqual(
      INITIAL_PROMPTS.map((spec) => prompt(spec.id, spec.title, spec.body, 0)),
    );
    expect(loaded.slice(4)).toEqual([
      prompt("u2", "後", "後の本文", 6),
      prompt("u1", "先", "先の本文", 5),
      prompt("copy", "写し", INITIAL_PROMPTS[0].body, 4),
    ]);
    expect(loadSavedPrompts()).toEqual(loaded);
  });

  it("does not drop a saved prompt to make room for the initial four", () => {
    const users = Array.from({ length: 80 }, (_, index) =>
      prompt(`u${index}`, `名${index}`, `本文${index}`, index + 1),
    );
    localStorage.setItem(SAVED_PROMPTS_KEY, JSON.stringify(users));
    localStorage.removeItem(SAVED_PROMPTS_SEEDED_KEY);
    expect(loadSavedPrompts().slice(4)).toEqual(users);
    expect(rememberSavedPrompt({ id: "new", title: "追加", body: "追加の本文", now: 100 })).toEqual({
      ok: true,
      prompts: [
        prompt("new", "追加", "追加の本文", 100),
        ...INITIAL_PROMPTS.map((spec) => prompt(spec.id, spec.title, spec.body, 0)),
        ...users.slice(0, 79),
      ],
    });
  });

  it("restores deleted and edited initial prompts without changing prompts the user saved", () => {
    localStorage.removeItem(SAVED_PROMPTS_SEEDED_KEY);
    localStorage.setItem(
      SAVED_PROMPTS_KEY,
      JSON.stringify([
        prompt("u2", "後", "後の本文", 6),
        prompt("u1", "先", "先の本文", 5),
        prompt("copy", "写し", INITIAL_PROMPTS[0].body, 4),
      ]),
    );
    loadSavedPrompts();
    forgetSavedPrompt(INITIAL_PROMPTS[1].id);
    expect(reviseSavedPrompt({
      id: INITIAL_PROMPTS[0].id,
      title: "短い要約",
      body: "書き換えた要約",
      now: 9,
    })).toEqual({
      ok: true,
      prompts: [
        prompt(INITIAL_PROMPTS[0].id, "短い要約", "書き換えた要約", 9),
        prompt(INITIAL_PROMPTS[2].id, INITIAL_PROMPTS[2].title, INITIAL_PROMPTS[2].body, 0),
        prompt(INITIAL_PROMPTS[3].id, INITIAL_PROMPTS[3].title, INITIAL_PROMPTS[3].body, 0),
        prompt("u2", "後", "後の本文", 6),
        prompt("u1", "先", "先の本文", 5),
        prompt("copy", "写し", INITIAL_PROMPTS[0].body, 4),
      ],
    });

    expect(restoreSavedPrompts(11)).toEqual([
      prompt(INITIAL_PROMPTS[0].id, "要約", INITIAL_PROMPTS[0].body, 11),
      prompt(INITIAL_PROMPTS[2].id, INITIAL_PROMPTS[2].title, INITIAL_PROMPTS[2].body, 0),
      prompt(INITIAL_PROMPTS[3].id, INITIAL_PROMPTS[3].title, INITIAL_PROMPTS[3].body, 0),
      prompt("u2", "後", "後の本文", 6),
      prompt("u1", "先", "先の本文", 5),
      prompt("copy", "写し", INITIAL_PROMPTS[0].body, 4),
      prompt(INITIAL_PROMPTS[1].id, "要点", INITIAL_PROMPTS[1].body, 11),
    ]);
    forgetSavedPrompt(INITIAL_PROMPTS[3].id);
    expect(loadSavedPrompts().some((row) => row.id === INITIAL_PROMPTS[3].id)).toBe(false);
    expect(loadSavedPrompts().filter((row) => row.id === "copy")).toEqual([
      prompt("copy", "写し", INITIAL_PROMPTS[0].body, 4),
    ]);
  });
});
