export type CourtCaseDate = {
  era?: string;
  year?: number;
  month?: number;
  day?: number;
};

export type CourtCaseJson = {
  trial_type?: string;
  date?: CourtCaseDate;
  case_number?: string;
  case_name?: string;
  court_name?: string;
  result_type?: string;
  result?: string;
  article_info?: string;
  original_court_name?: string;
  original_date?: CourtCaseDate;
  gist?: string;
  case_gist?: string;
  ref_law?: string;
  /** Full judgment text when present (courts.go.jp dumps). */
  contents?: string;
  lawsuit_id?: string;
  detail_page_link?: string;
  full_pdf_link?: string;
};

type FormatOptions = {
  /** Optional plain-text transform (e.g. kanji article numerals → Arabic). */
  formatPlainText?: (s: string) => string;
};

type CourtCasePlainRow = { label: string; value: string };

const ERA_JA: Record<string, string> = {
  Meiji: "明治",
  Taisho: "大正",
  Showa: "昭和",
  Heisei: "平成",
  Reiwa: "令和",
};

const TRIAL_TYPE_JA: Record<string, string> = {
  SupremeCourt: "最高裁",
  HighCourt: "高裁",
  DistrictCourt: "地裁",
  FamilyCourt: "家裁",
  SummaryCourt: "簡裁",
};

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

function asNonEmptyString(v: unknown): string | null {
  if (typeof v !== "string") return null;
  const t = v.trim();
  return t.length > 0 ? t : null;
}

function formatEraDate(d: CourtCaseDate | undefined): string | null {
  if (!d || typeof d !== "object") return null;
  const eraKey = typeof d.era === "string" ? d.era : "";
  const era = ERA_JA[eraKey] ?? (eraKey || null);
  const year = typeof d.year === "number" && Number.isFinite(d.year) ? d.year : null;
  const month =
    typeof d.month === "number" && Number.isFinite(d.month) ? d.month : null;
  const day = typeof d.day === "number" && Number.isFinite(d.day) ? d.day : null;
  if (era == null || year == null) return null;
  let out = `${era}${year}年`;
  if (month != null) out += `${month}月`;
  if (day != null) out += `${day}日`;
  return out;
}

function htmlRow(label: string, htmlValue: string): string {
  return `<div class="notes-court-row"><dt>${escapeHtml(label)}</dt><dd>${htmlValue}</dd></div>`;
}

function collectCourtCasePlainRows(
  data: CourtCaseJson,
  transform: (s: string) => string,
): CourtCasePlainRow[] {
  const rows: CourtCasePlainRow[] = [];

  const courtName = asNonEmptyString(data.court_name);
  const trialType = asNonEmptyString(data.trial_type);
  const trialJa = trialType ? TRIAL_TYPE_JA[trialType] : undefined;
  if (courtName || trialJa) {
    let value = courtName ? transform(courtName) : "";
    if (trialJa) {
      value = value ? `${value}（${trialJa}）` : trialJa;
    }
    rows.push({ label: "裁判所", value });
  }

  const decisionDate = formatEraDate(data.date);
  if (decisionDate) rows.push({ label: "判決日", value: decisionDate });

  const caseNumber = asNonEmptyString(data.case_number);
  if (caseNumber) rows.push({ label: "事件番号", value: transform(caseNumber) });

  const caseName = asNonEmptyString(data.case_name);
  if (caseName) rows.push({ label: "事件名", value: transform(caseName) });

  const resultType = asNonEmptyString(data.result_type);
  const result = asNonEmptyString(data.result);
  if (resultType || result) {
    const parts = [resultType, result].filter(Boolean) as string[];
    rows.push({ label: "結果", value: transform(parts.join("・")) });
  }

  const articleInfo = asNonEmptyString(data.article_info);
  if (articleInfo) rows.push({ label: "掲載", value: transform(articleInfo) });

  const originalCourt = asNonEmptyString(data.original_court_name);
  const originalDate = formatEraDate(data.original_date);
  if (originalCourt || originalDate) {
    let value = originalCourt ? transform(originalCourt) : "";
    if (originalDate) {
      value = value ? `${value}（${originalDate}）` : originalDate;
    }
    rows.push({ label: "原審", value });
  }

  const gist = asNonEmptyString(data.gist);
  if (gist) rows.push({ label: "要旨", value: transform(gist) });

  const caseGist = asNonEmptyString(data.case_gist);
  if (caseGist) rows.push({ label: "判示事項", value: transform(caseGist) });

  const refLaw = asNonEmptyString(data.ref_law);
  if (refLaw) rows.push({ label: "参照法令", value: transform(refLaw) });

  const contents = asNonEmptyString(data.contents);
  if (contents) rows.push({ label: "本文", value: transform(contents) });

  return rows;
}

/** True when body looks like a courts.go.jp-style case JSON object. */
export function isCourtCaseJsonTarget(body: string): boolean {
  return tryParseCourtCaseJson(body) != null;
}

export function tryParseCourtCaseJson(body: string): CourtCaseJson | null {
  const trimmed = body.trim();
  if (!trimmed.startsWith("{")) return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(trimmed);
  } catch {
    return null;
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return null;
  const o = parsed as Record<string, unknown>;
  const hasMarker =
    asNonEmptyString(o.lawsuit_id) != null ||
    asNonEmptyString(o.case_number) != null ||
    asNonEmptyString(o.case_gist) != null ||
    asNonEmptyString(o.trial_type) != null;
  if (!hasMarker) return null;
  return o as CourtCaseJson;
}

/** Plain-text court-case formatting for export; null if not applicable. */
export function formatCourtCaseJsonPlainText(
  body: string,
  options?: FormatOptions,
): string | null {
  const data = tryParseCourtCaseJson(body);
  if (!data) return null;
  const transform = options?.formatPlainText ?? ((s: string) => s);
  const rows = collectCourtCasePlainRows(data, transform);
  if (rows.length === 0) return null;
  return rows.map((r) => `${r.label}: ${r.value}`).join("\n");
}

/** Display-only HTML for court-case JSON; null if not applicable. */
export function formatCourtCaseJsonHtml(
  body: string,
  options?: FormatOptions,
): string | null {
  const data = tryParseCourtCaseJson(body);
  if (!data) return null;

  const transform = options?.formatPlainText ?? ((s: string) => s);
  const plainRows = collectCourtCasePlainRows(data, transform);
  if (plainRows.length === 0) return null;

  const rows = plainRows.map((r) => {
    if (r.label === "裁判所") {
      const escaped = escapeHtml(r.value).replace(
        /（([^）]+)）$/,
        ' <span class="notes-court-badge">（$1）</span>',
      );
      return htmlRow(r.label, escaped);
    }
    return htmlRow(r.label, escapeHtml(r.value));
  });

  return `<dl class="notes-court-case">${rows.join("")}</dl>`;
}
