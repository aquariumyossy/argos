/** Toggle `"…"` around a selected query span. Leading `-` becomes `-"…"`. */
export function toggleAdjacentQuotes(
  query: string,
  start: number,
  end: number,
): string | null {
  let s = start;
  let e = end;
  while (s < e && /\s/.test(query.charAt(s))) s += 1;
  while (e > s && /\s/.test(query.charAt(e - 1))) e -= 1;
  if (s >= e) return null;
  const selected = query.slice(s, e);
  const exclude = selected.startsWith("-");
  const core = exclude ? selected.slice(1) : selected;
  if (!core) return null;
  const quoted =
    core.startsWith('"') && core.endsWith('"') && core.length >= 2;
  const inner = quoted ? core.slice(1, -1) : core.replace(/^"|"$/g, "");
  if (!inner) return null;
  const replacement = quoted
    ? exclude
      ? `-${inner}`
      : inner
    : exclude
      ? `-"${inner}"`
      : `"${inner}"`;
  return query.slice(0, s) + replacement + query.slice(e);
}

/** Surface to register in the user dictionary (no quotes / leading `-`). */
export function dictionaryWordFromSelection(
  query: string,
  start: number,
  end: number,
): string | null {
  let s = start;
  let e = end;
  while (s < e && /\s/.test(query.charAt(s))) s += 1;
  while (e > s && /\s/.test(query.charAt(e - 1))) e -= 1;
  if (s >= e) return null;
  let word = query.slice(s, e).trim();
  if (word.startsWith("-")) word = word.slice(1);
  word = word.replace(/^"+|"+$/g, "").trim();
  return word || null;
}

export function selectionIsQuoted(
  query: string,
  start: number,
  end: number,
): boolean {
  let s = start;
  let e = end;
  while (s < e && /\s/.test(query.charAt(s))) s += 1;
  while (e > s && /\s/.test(query.charAt(e - 1))) e -= 1;
  if (s >= e) return false;
  let core = query.slice(s, e);
  if (core.startsWith("-")) core = core.slice(1);
  return core.startsWith('"') && core.endsWith('"') && core.length >= 2;
}
