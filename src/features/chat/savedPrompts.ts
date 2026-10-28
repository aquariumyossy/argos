export const SAVED_PROMPTS_KEY = "argos.savedPrompts.v1";

export const MAX_SAVED_PROMPTS = 80;
export const MAX_SAVED_PROMPT_TITLE = 40;
export const MAX_SAVED_PROMPT_BODY = 20_000;

export type SavedPrompt = {
  id: string;
  title: string;
  body: string;
  updatedAt: number;
};

export type SaveBlock = "empty" | "tooLong";

export type RememberResult =
  | { ok: true; prompts: SavedPrompt[] }
  | { ok: false; reason: SaveBlock };

export function saveBlockReason(body: string): SaveBlock | null {
  const text = body.trim();
  if (!text) {
    return "empty";
  }
  if (text.length > MAX_SAVED_PROMPT_BODY) {
    return "tooLong";
  }
  return null;
}

export function titleFromBody(body: string): string {
  const line = firstLine(body);
  return line.slice(0, MAX_SAVED_PROMPT_TITLE);
}

export function firstLine(body: string): string {
  const line = body.split(/\r?\n/).find((row) => row.trim().length > 0);
  return line?.trim() ?? "";
}

export function saveWasTooLong(result: RememberResult): boolean {
  return "reason" in result && result.reason === "tooLong";
}

export function needsReplaceConfirm(current: string, body: string): boolean {
  const text = current.trim();
  return text.length > 0 && text !== body;
}

function resolveTitle(title: string, body: string): string {
  const named = title.trim().slice(0, MAX_SAVED_PROMPT_TITLE);
  return named || titleFromBody(body);
}

function isRow(value: unknown): value is SavedPrompt {
  if (!value || typeof value !== "object") {
    return false;
  }
  const row = value as Record<string, unknown>;
  return (
    typeof row.id === "string" &&
    row.id.length > 0 &&
    typeof row.title === "string" &&
    typeof row.body === "string" &&
    row.body.trim().length > 0 &&
    typeof row.updatedAt === "number" &&
    Number.isFinite(row.updatedAt)
  );
}

export function parseSavedPrompts(raw: string | null): SavedPrompt[] {
  if (!raw) {
    return [];
  }
  try {
    const parsed = JSON.parse(raw) as unknown;
    if (!Array.isArray(parsed)) {
      return [];
    }
    return parsed.filter(isRow).map((row) => {
      const body = row.body.trim();
      return {
        id: row.id,
        title: resolveTitle(row.title, body),
        body,
        updatedAt: row.updatedAt,
      };
    });
  } catch {
    return [];
  }
}

export function rememberPrompt(
  list: SavedPrompt[],
  draft: { id: string; title: string; body: string; now: number }
): RememberResult {
  const blocked = saveBlockReason(draft.body);
  if (blocked) {
    return { ok: false, reason: blocked };
  }
  const body = draft.body.trim();
  const title = resolveTitle(draft.title, body);
  const existing = list.find((row) => row.body === body);
  const next: SavedPrompt = {
    id: existing?.id ?? draft.id,
    title,
    body,
    updatedAt: draft.now,
  };
  const rest = list.filter((row) => row.body !== body);
  return { ok: true, prompts: [next, ...rest].slice(0, MAX_SAVED_PROMPTS) };
}

export function forgetPrompt(list: SavedPrompt[], id: string): SavedPrompt[] {
  if (!list.some((row) => row.id === id)) {
    return list;
  }
  return list.filter((row) => row.id !== id);
}

export function loadSavedPrompts(): SavedPrompt[] {
  try {
    return parseSavedPrompts(localStorage.getItem(SAVED_PROMPTS_KEY));
  } catch {
    return [];
  }
}

function writeSavedPrompts(prompts: SavedPrompt[]): void {
  localStorage.setItem(SAVED_PROMPTS_KEY, JSON.stringify(prompts));
}

export function rememberSavedPrompt(draft: {
  title: string;
  body: string;
  now?: number;
  id?: string;
}): RememberResult {
  const result = rememberPrompt(loadSavedPrompts(), {
    id: draft.id ?? crypto.randomUUID(),
    title: draft.title,
    body: draft.body,
    now: draft.now ?? Date.now(),
  });
  if (result.ok) {
    writeSavedPrompts(result.prompts);
  }
  return result;
}

export function forgetSavedPrompt(id: string): SavedPrompt[] {
  const current = loadSavedPrompts();
  const next = forgetPrompt(current, id);
  if (next !== current) {
    writeSavedPrompts(next);
  }
  return next;
}
