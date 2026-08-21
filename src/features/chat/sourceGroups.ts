export type GroupableSource = {
  id: string;
  path: string;
  title: string;
  paragraphId?: string;
  body?: string;
  kind?: string;
  citeNo?: number;
  sortOrder: number;
  ocrStatus?: string;
  grain?: string;
  origin?: string;
  injectedUserMessageId?: string;
  citedAssistantMessageId?: string;
};

export type SourceGroup<T extends GroupableSource = GroupableSource> = {
  key: string;
  representative: T;
  members: T[];
  citeNo: number;
  citeNos: number[];
  label: string;
};

export function isImageSource(s: { kind?: string }): boolean {
  return (s.kind ?? "text").toLowerCase() === "image";
}

export function imageGroupKey(s: { kind?: string; path: string }): string | null {
  if (!isImageSource(s)) return null;
  const p = s.path.trim().replace(/\//g, "\\").toLowerCase();
  return p || null;
}

export function fileLabel(s: { path: string; title: string }): string {
  const p = s.path.trim().replace(/\\/g, "/");
  if (p) {
    const name = p.split("/").filter(Boolean).pop();
    if (name) return name;
  }
  const t = s.title.trim();
  const cut = t.split("（")[0]?.trim() ?? "";
  return cut || t || "出典";
}

function parsePdfPage(paragraphId: string | undefined): number | null {
  const m = /^(?:pdf-page:)(\d+)$/.exec((paragraphId ?? "").trim());
  if (!m) return null;
  const n = Number(m[1]);
  return Number.isFinite(n) ? n : null;
}

function byPage<T extends GroupableSource>(a: T, b: T): number {
  const pa = parsePdfPage(a.paragraphId);
  const pb = parsePdfPage(b.paragraphId);
  if (pa != null && pb != null && pa !== pb) return pa - pb;
  if (pa != null && pb == null) return -1;
  if (pa == null && pb != null) return 1;
  return a.sortOrder - b.sortOrder;
}

export function groupSources<T extends GroupableSource>(
  rows: T[],
  fallbackStart: number,
): SourceGroup<T>[] {
  const used = new Set<string>();
  const out: SourceGroup<T>[] = [];
  let fi = 0;
  for (const s of rows) {
    const key = imageGroupKey(s);
    if (key) {
      if (used.has(key)) continue;
      used.add(key);
      const members = rows.filter((r) => imageGroupKey(r) === key).sort(byPage);
      const fallback = fallbackStart + fi;
      fi += 1;
      out.push(buildGroup(key, members, fallback));
    } else {
      const fallback = fallbackStart + fi;
      fi += 1;
      out.push(buildGroup(`id:${s.id}`, [s], fallback));
    }
  }
  return out;
}

function buildGroup<T extends GroupableSource>(
  key: string,
  members: T[],
  fallback: number,
): SourceGroup<T> {
  const assigned = members
    .map((m) => m.citeNo)
    .filter((n): n is number => typeof n === "number" && n > 0);
  const citeNo = assigned.length ? Math.min(...assigned) : fallback;
  const citeNos = new Set<number>([citeNo, ...assigned]);
  const representative = members[0];
  return {
    key,
    representative,
    members,
    citeNo,
    citeNos: [...citeNos],
    label: fileLabel(representative),
  };
}

export function openableCitesFromGroups<T extends GroupableSource>(
  groups: SourceGroup<T>[],
): { nos: Set<number>; byNo: Map<number, T> } {
  const nos = new Set<number>();
  const byNo = new Map<number, T>();
  for (const g of groups) {
    for (const n of g.citeNos) {
      byNo.set(n, g.representative);
      if (g.representative.path.trim()) nos.add(n);
    }
  }
  return { nos, byNo };
}

export function ocrIncomplete(s: {
  kind?: string;
  ocrStatus?: string;
  body?: string;
}): boolean {
  const st = (s.ocrStatus ?? "").trim().toLowerCase();
  if (st === "pending" || st === "error") return true;
  return isImageSource(s) && !(s.body ?? "").trim();
}

export function groupOcrState<T extends GroupableSource>(
  g: SourceGroup<T>,
  ocrBusy: boolean,
): {
  done: number;
  total: number;
  reading: boolean;
  interrupted: boolean;
  hasError: boolean;
  badge: string | null;
} {
  const images = g.members.filter(isImageSource);
  const total = images.length || g.members.length;
  const done = images.filter((m) => !ocrIncomplete(m)).length;
  const hasError = images.some(
    (m) => (m.ocrStatus ?? "").trim().toLowerCase() === "error",
  );
  const anyPending = images.some(
    (m) => (m.ocrStatus ?? "").trim().toLowerCase() === "pending",
  );
  const reading = anyPending && ocrBusy;
  const interrupted = hasError || (anyPending && !ocrBusy);
  let badge: string | null = null;
  if (reading) {
    badge = total > 1 ? `読み取り中 ${done}/${total}` : "読み取り中";
  } else if (interrupted) {
    if (hasError && total > 1 && done > 0) badge = "一部失敗";
    else if (hasError) badge = "失敗";
    else badge = "中断";
  } else if (images.length) {
    badge = "画像";
  }
  return { done, total, reading, interrupted, hasError, badge };
}
