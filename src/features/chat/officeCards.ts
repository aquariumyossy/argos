import { parseOfficeFileNotice } from "./officeFile";
import { editOfficeFileLink } from "./officeOutput";

export type OfficeCardFile = {
  kind: "docx" | "xlsx";
  name: string;
  /** Bare name of the attachment an edit started from. */
  sourceName?: string;
};

export type OfficeCardMessage = {
  id: string;
  role: string;
  content: string;
  kind?: string;
  toolName?: string;
};

/**
 * One card per output file, on the assistant reply that follows those files.
 * If that reply was never stored, the card stays on the notice or tool row
 * that announced the file, so a cancelled turn can still be opened.
 */
export function officeCardsByMessage(
  messages: readonly OfficeCardMessage[],
): Map<string, OfficeCardFile[]> {
  const assigned = new Map<string, OfficeCardFile[]>();
  let turn: OfficeCardMessage[] = [];
  const flush = () => {
    if (turn.length > 0) assignTurn(turn, assigned);
    turn = [];
  };
  for (const message of messages) {
    if (message.role === "user") {
      flush();
      continue;
    }
    turn.push(message);
  }
  flush();
  return assigned;
}

function assignTurn(
  turn: readonly OfficeCardMessage[],
  assigned: Map<string, OfficeCardFile[]>,
) {
  const found: { index: number; id: string; file: OfficeCardFile }[] = [];
  turn.forEach((message, index) => {
    const file = officeCardFromMessage(message);
    if (file) found.push({ index, id: message.id, file });
  });
  if (found.length === 0) return;
  const lastFile = found[found.length - 1].index;
  let replyId = "";
  for (let index = turn.length - 1; index > lastFile; index -= 1) {
    const message = turn[index];
    if (message.role === "assistant" && message.kind !== "notice") {
      replyId = message.id;
      break;
    }
  }
  if (replyId) {
    assigned.set(
      replyId,
      found.map((item) => item.file),
    );
    return;
  }
  for (const item of found) {
    const list = assigned.get(item.id) ?? [];
    list.push(item.file);
    assigned.set(item.id, list);
  }
}

function officeCardFromMessage(message: OfficeCardMessage): OfficeCardFile | null {
  if (message.kind === "notice") {
    const parsed = parseOfficeFileNotice(message.content);
    if (!parsed) return null;
    return { kind: parsed.kind, name: parsed.name };
  }
  if (message.role !== "tool") return null;
  const parsed = editOfficeFileLink(message.toolName, message.content);
  if (!parsed) return null;
  return parsed.sourceName
    ? { kind: parsed.kind, name: parsed.name, sourceName: parsed.sourceName }
    : { kind: parsed.kind, name: parsed.name };
}
