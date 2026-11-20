import { parseOfficeFileNotice, type OfficeKind } from "./officeFile";
import { editOfficeFileLink } from "./officeOutput";

export type OfficeCardFile = {
  kind: OfficeKind;
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
 * The same output file (its bare name; the card has no path) keeps only the
 * last card in that turn. If the reply was never stored, that last card stays
 * on the notice or tool row that announced it, so a cancelled turn can still
 * be opened.
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
  const unique = keepLastOutputFile(found);
  if (unique.length === 0) return;
  const lastFile = unique[unique.length - 1].index;
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
      unique.map((item) => item.file),
    );
    return;
  }
  for (const item of unique) {
    const list = assigned.get(item.id) ?? [];
    list.push(item.file);
    assigned.set(item.id, list);
  }
}

/** Later result for the same output name wins. Its place in the turn is kept. */
function keepLastOutputFile(
  found: readonly { index: number; id: string; file: OfficeCardFile }[],
): { index: number; id: string; file: OfficeCardFile }[] {
  const lastAt = new Map<string, number>();
  found.forEach((item, index) => {
    lastAt.set(item.file.name, index);
  });
  return found.filter((item, index) => lastAt.get(item.file.name) === index);
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
