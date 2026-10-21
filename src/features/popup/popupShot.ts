import type { SearchHit } from "../preview/types";

export type ShotSuggestion = {
  term: string;
  displayPrefix: string;
  displayRest: string;
  kind: string;
  score: number;
  fromHistory?: boolean;
  fromRegistered?: boolean;
};

export type ShotSeed = {
  mode: "enter" | "layout";
  query: string;
  hits: SearchHit[];
  suggestions: ShotSuggestion[];
  roots: string[];
  scopePath: string | null;
  scopeLabel: string | null;
  extFilterKeys: string[];
  /** Fictitious dates for the dev preview only. Live file hits have no date field. */
  dates: Record<string, string>;
};

const ROOT = "\\\\FILES\\cases\\訴訟案件";

function fileHit(
  path: string,
  title: string,
  snippet: string,
  score: number,
): SearchHit {
  return {
    id: path,
    title,
    snippet,
    path,
    page: null,
    chunkId: null,
    score,
    source: "local",
    previewText: snippet,
    highlightTerms: ["売掛金"],
    matchCount: 1,
    paragraphs: [],
    unitLabel: "",
    mailFrom: "",
    mailDate: "",
    mailConversationId: "",
    mailFolder: "",
    docKind: "file",
  };
}

const HITS: SearchHit[] = [
  fileHit(
    `${ROOT}\\4. 訴え提起〜結審前\\甲野商事 売掛金請求事件\\提出書面\\準備書面（2）.docx`,
    "準備書面（2）.docx",
    "…被告は、本件売掛金債権の発生原因たる売買契約の成立を争うが、原告が提出した甲第3号証の納品書には被告担当者の受領印があり…",
    10,
  ),
  fileHit(
    `${ROOT}\\4. 訴え提起〜結審前\\甲野商事 売掛金請求事件\\証拠\\証拠説明書（甲1〜甲8）.docx`,
    "証拠説明書（甲1〜甲8）.docx",
    "甲第5号証　売掛金元帳（写し）　作成者：原告　立証趣旨：令和7年4月から同年9月までの取引額及び未払残高…",
    8,
  ),
  fileHit(
    `${ROOT}\\4. 訴え提起〜結審前\\乙川建物明渡請求事件\\訴状\\訴状.docx`,
    "訴状.docx",
    "…なお、被告に対する売掛金との相殺の主張が予想されるが、本件賃料債務とは別個の債権であり…",
    6,
  ),
  fileHit(
    `${ROOT}\\4. 訴え提起〜結審前\\甲野商事 売掛金請求事件\\提出書面\\答弁書.pdf`,
    "答弁書.pdf",
    "第2　請求の原因に対する認否　1　請求の原因1のうち、売掛金の額については否認し、その余は認める。",
    4,
  ),
];

const DATES: Record<string, string> = {
  [HITS[0]!.path]: "2026/06/18",
  [HITS[1]!.path]: "2026/05/30",
  [HITS[2]!.path]: "2026/04/11",
  [HITS[3]!.path]: "2026/03/02",
};

const SUGGESTIONS: ShotSuggestion[] = [
  {
    term: "AC",
    displayPrefix: "",
    displayRest: "",
    kind: "history",
    score: 2,
    fromHistory: true,
  },
  {
    term: "グループ",
    displayPrefix: "",
    displayRest: "",
    kind: "history",
    score: 1,
    fromHistory: true,
  },
];

/** Dev-only fixture for `?popup-shot=enter|layout`. Null in production and in tests. */
export function loadShotSeed(): ShotSeed | null {
  if (!import.meta.env.DEV || typeof window === "undefined") return null;
  const mode = new URLSearchParams(window.location.search).get("popup-shot");
  if (mode !== "enter" && mode !== "layout") return null;
  return {
    mode,
    query: "売掛金",
    hits: HITS,
    suggestions: mode === "enter" ? SUGGESTIONS : [],
    roots: [ROOT],
    scopePath:
      mode === "layout" ? `${ROOT}\\4. 訴え提起〜結審前` : null,
    scopeLabel: mode === "layout" ? "4. 訴え提起〜結審前" : null,
    extFilterKeys: mode === "layout" ? ["docx", "pdf"] : [],
    dates: DATES,
  };
}
