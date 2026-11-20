import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import Settings from "./Settings";

const { mockInvoke } = vi.hoisted(() => ({
  mockInvoke: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => mockInvoke(...args),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: async () => () => {},
}));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    onFocusChanged: async () => () => {},
  }),
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn(),
}));

const settings = {
  shortcut: "Ctrl+Alt+A",
  notesShortcut: "Ctrl+Alt+N",
  maxResults: 10,
  fontSize: 14,
  indexIntervalSecs: 3600,
  autostart: false,
  popupWidth: 640,
  popupHeight: 520,
  popupPosition: "center" as const,
  remoteServerEnabled: false,
  remoteServerPort: 17890,
  remoteServerToken: "",
  searchMode: "local" as const,
  remoteUrl: "",
  remoteToken: "",
  remoteTimeoutMs: 3000,
  posFilterEnabled: true,
  mailEnabled: false,
  mailDaysBack: 730,
  mailSyncIntervalSecs: 3600,
  mailLatestOnly: false,
  mailThreadCollapse: true,
  mailLastSyncAt: "",
  calendarEnabled: false,
  calendarDaysBack: 7,
  calendarDaysAhead: 60,
  calendarSyncIntervalSecs: 3600,
  calendarLastSyncAt: "",
  calendarTruncated: false,
  calendarIcalFeeds: [],
  llmBaseUrl: "http://127.0.0.1:11434/v1",
  llmApiKey: "",
  llmModel: "",
  llmTimeoutMs: 120000,
  llmMaxContextTokens: 131072,
  llmSystemPrompt: "独自のシステムプロンプトです。",
  llmThinking: "on" as const,
  llmMaxToolRounds: 8,
  llmSearchTopK: 4,
  searxngUrl: "",
  searxngTimeoutMs: 8000,
  llmWebSearchTopK: 5,
  officeRevisionAuthor: "",
};

function noticeText(name: string) {
  if (name === "LICENSE") return "Apache License, Version 2.0";
  if (name === "NOTICE") return "IPADIC\nNO WARRANTY\n公共データ利用規約";
  if (name === "THIRD_PARTY_LICENSES") return "MPL-2.0 components are used unmodified";
  throw new Error(`unknown notice ${name}`);
}

describe("credits license documents", () => {
  afterEach(() => {
    cleanup();
    mockInvoke.mockReset();
  });

  it("shows each bundled license file from the credits tab", async () => {
    mockInvoke.mockImplementation(async (cmd: string, args?: { name?: string }) => {
      if (cmd === "get_settings") return settings;
      if (cmd === "list_folders" || cmd === "list_exclude_paths" || cmd === "list_search_words") {
        return [];
      }
      if (cmd === "mail_list_folders") return [];
      if (cmd === "mail_indexed_count") return 0;
      if (cmd === "get_lan_ip_hint") return null;
      if (cmd === "read_bundled_notice") return noticeText(args?.name ?? "");
      return null;
    });

    render(<Settings />);
    fireEvent.click(await screen.findByRole("button", { name: "クレジット" }));
    expect(screen.getByRole("button", { name: "LICENSE を表示" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "NOTICE を表示" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "THIRD_PARTY_LICENSES を表示" })).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "NOTICE を表示" }));
    const notice = await screen.findByRole("heading", { name: "NOTICE" });
    const block = notice.parentElement;
    expect(block?.textContent).toContain("NO WARRANTY");
    expect(block?.textContent).toContain("公共データ利用規約");
    expect(mockInvoke).toHaveBeenCalledWith("read_bundled_notice", { name: "NOTICE" });

    fireEvent.click(screen.getByRole("button", { name: "THIRD_PARTY_LICENSES を表示" }));
    expect(await screen.findByText(/used unmodified/)).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "LICENSE を表示" }));
    expect(await screen.findByText(/Apache License, Version 2.0/)).toBeTruthy();
  });
});
