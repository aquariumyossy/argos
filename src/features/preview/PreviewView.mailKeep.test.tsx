import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import PreviewView from "./PreviewView";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string) => {
    if (cmd === "get_settings") return { fontSize: 14 };
    if (cmd === "preview_file") {
      return { units: [], excerpt: false, matchIds: [] };
    }
    return null;
  }),
}));

vi.mock("@tauri-apps/api/event", () => ({
  emit: vi.fn(),
}));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    setTitle: async () => {},
  }),
}));

afterEach(() => {
  cleanup();
});

describe("preview actions for a kept mail", () => {
  it("offers Outlook and the folder from the preview", async () => {
    render(
      <PreviewView
        target={{
          origin: "notes",
          path: "outlook:store/entry",
          paragraphId: "outlook:store/entry#1",
          title: "準備書面",
          source: "outlook",
          fallbackBody: "本文",
        }}
      />,
    );
    expect(await screen.findByRole("button", { name: "Outlookで開く" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "フォルダを開く" })).toBeTruthy();
  });
});
