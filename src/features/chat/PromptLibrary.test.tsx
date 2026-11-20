import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import PromptLibrary from "./PromptLibrary";
import { INITIAL_PROMPTS } from "./savedPrompts";

describe("PromptLibrary", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  afterEach(() => {
    cleanup();
    localStorage.clear();
  });

  function openLibrary(text = "") {
    const onRecall = vi.fn();
    render(<PromptLibrary text={text} disabled={false} onRecall={onRecall} />);
    fireEvent.click(screen.getByRole("button", { name: "プロンプトの保存と呼出" }));
    return onRecall;
  }

  it("names the library プロンプト and recalls an initial prompt", () => {
    const onRecall = openLibrary("下書き");
    expect(screen.getByRole("button", { name: "プロンプトの保存と呼出" }).textContent).toBe(
      "プロンプト",
    );
    expect(screen.getByRole("dialog", { name: "プロンプト" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "このプロンプトを保存" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "初期状態に戻す" })).toBeTruthy();
    for (const spec of INITIAL_PROMPTS) {
      expect(screen.getByText(spec.title)).toBeTruthy();
    }
    fireEvent.click(screen.getByText("要約"));
    expect(screen.getByText("今のプロンプトを置き換えます")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "呼び出す" }));
    expect(onRecall).toHaveBeenCalledWith(INITIAL_PROMPTS[0].body);
  });

  it("edits and deletes an initial prompt, then restore keeps a prompt the user saved", () => {
    openLibrary("自分用の依頼");
    fireEvent.click(screen.getByRole("button", { name: "このプロンプトを保存" }));
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    fireEvent.click(screen.getByRole("button", { name: "要約 を削除" }));
    fireEvent.click(screen.getByRole("button", { name: "要点 を編集" }));
    fireEvent.change(screen.getByRole("textbox", { name: "プロンプト" }), {
      target: { value: "書き換えた要点" },
    });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    expect(screen.getByText("書き換えた要点")).toBeTruthy();
    expect(screen.queryByText("要約")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "初期状態に戻す" }));
    expect(screen.queryByText("書き換えた要点")).toBeNull();
    expect(screen.getByText("要約")).toBeTruthy();
    expect(screen.getByText(INITIAL_PROMPTS[1].body)).toBeTruthy();
    expect(screen.getByText("自分用の依頼")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "時系列 を削除" }));
    fireEvent.click(screen.getByRole("button", { name: "プロンプトの保存と呼出" }));
    fireEvent.click(screen.getByRole("button", { name: "プロンプトの保存と呼出" }));
    expect(screen.queryByText("時系列")).toBeNull();
    expect(screen.getByText("自分用の依頼")).toBeTruthy();
    expect(screen.getByText("要約")).toBeTruthy();
  });

  it("shows an empty library after every prompt is deleted, and restore brings the initial four back", () => {
    openLibrary();
    for (const spec of INITIAL_PROMPTS) {
      fireEvent.click(screen.getByRole("button", { name: `${spec.title} を削除` }));
    }
    expect(screen.getByText("保存したプロンプトはまだありません。")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "初期状態に戻す" }));
    expect(screen.queryByText("保存したプロンプトはまだありません。")).toBeNull();
    for (const spec of INITIAL_PROMPTS) {
      expect(screen.getByText(spec.title)).toBeTruthy();
    }
  });
});
