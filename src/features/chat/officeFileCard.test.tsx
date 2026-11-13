import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import Chat from "./Chat";

const thread = {
  id: "thread-a",
  title: "契約の相談",
  pathPrefix: "",
  noteId: "",
  sortOrder: 0,
  createdAt: 1,
  updatedAt: 2,
};

const docxNotice = [
  "Wordファイルを保存しました（Argos の出力フォルダ）",
  "[[argos-office]]\tdocx\t契約メモ.docx",
].join("\n");

const xlsxNotice = [
  "Excelファイルを保存しました（Argos の出力フォルダ）",
  "[[argos-office]]\txlsx\t売上集計.xlsx",
].join("\n");

const editName = "契約書_編集済み（2026-11-12 09-30-00）.docx";
const editReport = [
  `出力ファイル: ${editName}`,
  `[[argos-office]]\tdocx\t${editName}`,
  "元ファイル: 契約書.docx",
  String.raw`保存先: C:\Users\me\AppData\Roaming\Argos\output\${editName}`,
  "残存: あり",
  "「山田太郎」が残っています。",
].join("\n");

type Row = {
  id: string;
  threadId: string;
  role: string;
  content: string;
  createdAt: number;
  kind?: string;
  toolName?: string;
};

let rows: Row[] = [];
let openError: Error | null = null;

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

vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({
    onDragDropEvent: async () => () => {},
  }),
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn(),
}));

vi.mock("@tauri-apps/plugin-clipboard-manager", () => ({
  writeText: vi.fn(),
}));

function row(
  id: string,
  role: string,
  content: string,
  extra: Partial<Row> = {},
): Row {
  return {
    id,
    threadId: thread.id,
    role,
    content,
    createdAt: rows.length + 1,
    ...extra,
  };
}

async function renderChat() {
  render(<Chat />);
}

beforeEach(() => {
  rows = [];
  openError = null;
  mockInvoke.mockReset();
  mockInvoke.mockImplementation(async (cmd: string) => {
    switch (cmd) {
      case "is_app_ready":
        return true;
      case "get_settings":
        return {
          fontSize: 14,
          llmMaxContextTokens: 131_072,
          searxngUrl: "",
          searchMode: "local",
        };
      case "llm_thinking_budget":
        return { state: "unset" };
      case "llm_list_threads":
        return [thread];
      case "llm_get_active_thread":
        return thread;
      case "llm_list_messages":
        return rows;
      case "llm_list_sources":
        return [];
      case "llm_context_meter":
        return 10;
      case "consume_chat_shortcut_focus":
        return false;
      case "open_output_file":
        if (openError) throw openError;
        return null;
      default:
        return null;
    }
  });
});

afterEach(() => {
  cleanup();
});

function openButtons() {
  return screen.getAllByRole("button", { name: /を開く$/ });
}

describe("office file cards", () => {
  it("shows one create card per file at the end of the reply and opens it", async () => {
    rows = [
      row("u", "user", "作って"),
      row("call", "assistant", ""),
      row("n1", "assistant", docxNotice, { kind: "notice" }),
      row("n2", "assistant", xlsxNotice, { kind: "notice" }),
      row("reply", "assistant", "できました。"),
    ];
    await renderChat();
    const docx = await screen.findByRole("button", { name: "契約メモ.docx を開く" });
    const xlsx = screen.getByRole("button", { name: "売上集計.xlsx を開く" });
    const reply = screen.getByText("できました。").closest("article");
    expect(reply).toBeTruthy();
    expect(reply?.querySelectorAll("[data-office-card]")).toHaveLength(2);
    expect(reply?.querySelector("[data-office-kind='docx']")).toBeTruthy();
    expect(reply?.querySelector("[data-office-kind='xlsx']")).toBeTruthy();
    expect(
      screen.getByText("できました。").compareDocumentPosition(docx) &
        Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
    expect(
      screen
        .getByText("Wordファイルを保存しました（Argos の出力フォルダ）")
        .closest("article")
        ?.querySelector("[data-office-card]"),
    ).toBeNull();
    expect(
      screen
        .getByText("Excelファイルを保存しました（Argos の出力フォルダ）")
        .closest("article")
        ?.querySelector("[data-office-card]"),
    ).toBeNull();
    expect(document.querySelector(".chat-cite-badge")).toBeNull();
    expect(screen.queryByRole("button", { name: "フォルダを開く" })).toBeNull();
    expect(openButtons()).toHaveLength(2);
    expect(docx.textContent).toBe("開く");
    expect(xlsx.textContent).toBe("開く");

    fireEvent.click(docx);
    expect(mockInvoke).toHaveBeenCalledWith("open_output_file", {
      name: "契約メモ.docx",
    });
    fireEvent.click(xlsx);
    expect(mockInvoke).toHaveBeenCalledWith("open_output_file", {
      name: "売上集計.xlsx",
    });
  });

  it("shows an edit card with the original name and opens the new file", async () => {
    rows = [
      row("u", "user", "直して"),
      row("call", "assistant", "編集します。"),
      row("tool", "tool", editReport, { toolName: "edit_office" }),
      row("reply", "assistant", "編集しました。"),
    ];
    await renderChat();
    const open = await screen.findByRole("button", { name: `${editName} を開く` });
    const reply = screen.getByText("編集しました。").closest("article");
    expect(reply?.contains(open)).toBe(true);
    expect(reply?.querySelectorAll("[data-office-card]")).toHaveLength(1);
    expect(reply?.querySelector("[data-office-kind='docx']")).toBeTruthy();
    expect(screen.getByText("元：契約書.docx").closest("article")).toBe(reply);
    expect(
      screen.getByText("編集しました。").compareDocumentPosition(open) &
        Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
    const leftovers = screen.getAllByText("「山田太郎」が残っています。");
    expect(leftovers.length).toBeGreaterThan(0);
    expect(leftovers.every((node) => node.closest("article") !== reply)).toBe(true);
    expect(document.querySelector(".chat-cite-badge")).toBeNull();
    expect(screen.queryByRole("button", { name: "フォルダを開く" })).toBeNull();
    expect(open.textContent).toBe("開く");

    fireEvent.click(open);
    expect(mockInvoke).toHaveBeenCalledWith("open_output_file", { name: editName });
    expect(mockInvoke).not.toHaveBeenCalledWith("open_output_file", {
      name: "契約書.docx",
    });
  });

  it("keeps a card on the announcing row when there is no later reply", async () => {
    rows = [
      row("u", "user", "作って"),
      row("n1", "assistant", docxNotice, { kind: "notice" }),
      row("tool", "tool", editReport, { toolName: "edit_office" }),
    ];
    await renderChat();
    const created = await screen.findByRole("button", { name: "契約メモ.docx を開く" });
    const edited = screen.getByRole("button", { name: `${editName} を開く` });
    expect(openButtons()).toHaveLength(2);
    expect(document.querySelectorAll("[data-office-card]")).toHaveLength(2);
    expect(document.querySelector(".chat-cite-badge")).toBeNull();
    expect(screen.getByText("元：契約書.docx")).toBeTruthy();
    expect(
      screen
        .getByText("Wordファイルを保存しました（Argos の出力フォルダ）")
        .closest("article")
        ?.contains(created),
    ).toBe(true);

    fireEvent.click(edited);
    expect(mockInvoke).toHaveBeenCalledWith("open_output_file", { name: editName });
  });

  it("shows an HTML card, a sandboxed preview, and opens the large view", async () => {
    const edited = "手数料_編集済み（2026-11-13 02-50-00）.html";
    const htmlNotice = [
      "HTMLファイルを保存しました（Argos の出力フォルダ）",
      "[[argos-office]]\thtml\t手数料.html",
    ].join("\n");
    const editReport = [
      `出力ファイル: ${edited}`,
      `[[argos-office]]\thtml\t${edited}`,
      "元ファイル: 手数料.html",
    ].join("\n");
    rows = [
      row("u", "user", "計算して"),
      row("n1", "assistant", htmlNotice, { kind: "notice" }),
      row("tool", "tool", editReport, { toolName: "edit_html" }),
      row("reply", "assistant", "できました。"),
    ];
    await renderChat();
    const open = await screen.findByRole("button", { name: "手数料.html を開く" });
    const openEdited = screen.getByRole("button", { name: `${edited} を開く` });
    const reply = screen.getByText("できました。").closest("article");
    expect(reply?.querySelectorAll("[data-office-card]")).toHaveLength(2);
    expect(reply?.querySelector("[data-office-kind='html']")).toBeTruthy();
    expect(screen.getByText("元：手数料.html").closest("article")).toBe(reply);
    const frames = reply?.querySelectorAll("iframe") ?? [];
    expect(frames).toHaveLength(2);
    for (const frame of frames) {
      expect(frame.getAttribute("sandbox")).toBe("allow-scripts");
      expect(frame.getAttribute("csp")).toContain("connect-src 'none'");
      expect(frame.getAttribute("csp")?.includes("allow-same-origin")).toBe(false);
      expect(frame.getAttribute("referrerpolicy")).toBe("no-referrer");
    }
    expect(frames[0]?.getAttribute("src")).toContain(
      "preview?name=%E6%89%8B%E6%95%B0%E6%96%99.html",
    );
    expect(screen.getAllByRole("button", { name: "大きく表示" })).toHaveLength(2);

    fireEvent.click(open);
    expect(mockInvoke).toHaveBeenCalledWith("open_output_file", { name: "手数料.html" });
    fireEvent.click(screen.getAllByRole("button", { name: "大きく表示" })[1]!);
    expect(mockInvoke).toHaveBeenCalledWith("show_html_preview", { name: edited });
    fireEvent.click(openEdited);
    expect(mockInvoke).toHaveBeenCalledWith("open_output_file", { name: edited });
  });

  it("shows the same open failure as before", async () => {
    rows = [
      row("u", "user", "作って"),
      row("n1", "assistant", docxNotice, { kind: "notice" }),
      row("reply", "assistant", "できました。"),
    ];
    openError = new Error("ファイルが見つかりません。");
    await renderChat();
    fireEvent.click(await screen.findByRole("button", { name: "契約メモ.docx を開く" }));
    expect(await screen.findByText("ファイルが見つかりません。")).toBeTruthy();
  });
});
