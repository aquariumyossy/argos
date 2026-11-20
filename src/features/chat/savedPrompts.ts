export const SAVED_PROMPTS_KEY = "argos.savedPrompts.v1";
/** Set once the four initial prompts have been inserted. Deleting them does not clear this. */
export const SAVED_PROMPTS_SEEDED_KEY = "argos.savedPrompts.seeded.v1";

export const MAX_SAVED_PROMPTS = 80;
export const MAX_SAVED_PROMPT_TITLE = 40;
export const MAX_SAVED_PROMPT_BODY = 20_000;

/**
 * The composer templates 「要約」「要点」「要件事実」「時系列」.
 * They are ordinary saved prompts: the same id is how restore finds an edit or a deletion.
 * They do not consume one of the 80 slots.
 */
export const INITIAL_PROMPTS: readonly { id: string; title: string; body: string }[] = [
  {
    id: "argos.prompt.summary",
    title: "要約",
    body: "添付した出典を日本語で要約してください。重要な結論と根拠を短く。",
  },
  {
    id: "argos.prompt.points",
    title: "要点",
    body: "争点、結論、根拠を箇条書きで整理してください。根拠には出典番号 [n] を付けてください。",
  },
  {
    id: "argos.prompt.facts",
    title: "要件事実",
    body: "添付出典だけを根拠に、要件事実の関係を mermaid の flowchart LR でフェンス1本にまとめてください。左から右へ、請求原因 → 抗弁 → 再抗弁。請求原因はノードを1つだけにし、複数の主要事実は同じノード内に番号で書いてください。抗弁は種類ごとに分け、その右に対応する再抗弁を置いてください。ノードは短く。ラベルは A[\"請求原因 [n]\"] のように二重引用符で囲んでください。矢印は --> または -.-> で、途中に空白を入れないでください。添付出典にない事実はノードにしないでください。",
  },
  {
    id: "argos.prompt.timeline",
    title: "時系列",
    body: "添付出典に書かれた出来事だけを時系列で整理してください。図にする場合は mermaid の flowchart LR または timeline をフェンス1本にしてください。ノードラベルは二重引用符で囲み、出典番号 [n] をラベル内に書いてください。出典にない出来事は入れないでください。",
  },
];

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

function initialIds(): Set<string> {
  return new Set(INITIAL_PROMPTS.map((spec) => spec.id));
}

function initialRow(spec: (typeof INITIAL_PROMPTS)[number], updatedAt: number): SavedPrompt {
  return { id: spec.id, title: spec.title, body: spec.body, updatedAt };
}

/**
 * Missing initial prompts go in front, in template order.
 * A stored row with the same id or the same text is kept, so an upgrade does not
 * add a second copy of a template the user already saved.
 */
function seedInitialPrompts(list: SavedPrompt[]): SavedPrompt[] {
  const haveId = new Set(list.map((row) => row.id));
  const haveBody = new Set(list.map((row) => row.body));
  const missing = INITIAL_PROMPTS.filter(
    (spec) => !haveId.has(spec.id) && !haveBody.has(spec.body),
  ).map((spec) => initialRow(spec, 0));
  if (missing.length === 0) return list;
  return [...missing, ...list];
}

/**
 * Edited initial prompts take their original text in place. Deleted ones are
 * added at the end, in template order. Prompts the user saved stay put.
 */
function restoreInitialPrompts(list: SavedPrompt[], now: number): SavedPrompt[] {
  const specs = new Map(INITIAL_PROMPTS.map((spec) => [spec.id, spec]));
  let changed = false;
  const next = list.map((row) => {
    const spec = specs.get(row.id);
    if (!spec) return row;
    if (row.title === spec.title && row.body === spec.body) return row;
    changed = true;
    return initialRow(spec, now);
  });
  const have = new Set(next.map((row) => row.id));
  const missing = INITIAL_PROMPTS.filter((spec) => !have.has(spec.id)).map((spec) =>
    initialRow(spec, now),
  );
  if (missing.length === 0) return changed ? next : list;
  return [...next, ...missing];
}

/** The 80-prompt cap counts prompts the user saved. The four initial entries do not take a slot. */
function capSavedPrompts(list: SavedPrompt[]): SavedPrompt[] {
  const ids = initialIds();
  let keptUsers = 0;
  let dropped = false;
  const keep = new Set<string>();
  for (const row of list) {
    if (ids.has(row.id)) {
      keep.add(row.id);
      continue;
    }
    if (keptUsers >= MAX_SAVED_PROMPTS) {
      dropped = true;
      continue;
    }
    keep.add(row.id);
    keptUsers += 1;
  }
  if (!dropped) return list;
  return list.filter((row) => keep.has(row.id));
}

export function rememberPrompt(
  list: SavedPrompt[],
  draft: { id: string; title: string; body: string; now: number },
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
  return { ok: true, prompts: capSavedPrompts([next, ...rest]) };
}

export function revisePrompt(
  list: SavedPrompt[],
  draft: { id: string; title: string; body: string; now: number },
): RememberResult {
  const blocked = saveBlockReason(draft.body);
  if (blocked) {
    return { ok: false, reason: blocked };
  }
  const index = list.findIndex((row) => row.id === draft.id);
  if (index < 0) {
    return { ok: false, reason: "empty" };
  }
  const body = draft.body.trim();
  const title = resolveTitle(draft.title, body);
  const current = list[index];
  if (current.title === title && current.body === body) {
    return { ok: true, prompts: list };
  }
  const next = list.slice();
  next[index] = { id: draft.id, title, body, updatedAt: draft.now };
  return { ok: true, prompts: next };
}

export function forgetPrompt(list: SavedPrompt[], id: string): SavedPrompt[] {
  if (!list.some((row) => row.id === id)) {
    return list;
  }
  return list.filter((row) => row.id !== id);
}

export function loadSavedPrompts(): SavedPrompt[] {
  let stored: SavedPrompt[] = [];
  try {
    stored = parseSavedPrompts(localStorage.getItem(SAVED_PROMPTS_KEY));
    if (localStorage.getItem(SAVED_PROMPTS_SEEDED_KEY) === "1") {
      return stored;
    }
  } catch {
    return stored;
  }
  const seeded = seedInitialPrompts(stored);
  try {
    if (seeded !== stored) {
      writeSavedPrompts(seeded);
    }
    localStorage.setItem(SAVED_PROMPTS_SEEDED_KEY, "1");
  } catch {
    // The seeded list did not fit (quota). Returning [] would make the next
    // save replace the stored prompts with a single new row.
    return stored;
  }
  return seeded;
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

export function reviseSavedPrompt(draft: {
  id: string;
  title: string;
  body: string;
  now?: number;
}): RememberResult {
  const current = loadSavedPrompts();
  const result = revisePrompt(current, {
    id: draft.id,
    title: draft.title,
    body: draft.body,
    now: draft.now ?? Date.now(),
  });
  if (result.ok && result.prompts !== current) {
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

export function restoreSavedPrompts(now = Date.now()): SavedPrompt[] {
  try {
    const stored = parseSavedPrompts(localStorage.getItem(SAVED_PROMPTS_KEY));
    const next = restoreInitialPrompts(stored, now);
    if (next !== stored) {
      writeSavedPrompts(next);
    }
    localStorage.setItem(SAVED_PROMPTS_SEEDED_KEY, "1");
    return next;
  } catch {
    return [];
  }
}
