export type ParagraphHit = {
  id: string;
  label: string;
  snippet: string;
  score: number;
  page?: number | null;
};

export type SearchHit = {
  id: string;
  title: string;
  snippet: string;
  path: string;
  page?: number | null;
  chunkId?: number | null;
  score: number;
  source: string;
  previewText: string;
  highlightTerms?: string[];
  matchCount?: number;
  paragraphs?: ParagraphHit[];
  unitLabel?: string;
  mailFrom?: string;
  mailDate?: string;
  mailConversationId?: string;
  mailFolder?: string;
  docKind?: string;
};

export type PreviewFileResult = {
  units: SearchHit[];
  excerpt: boolean;
  matchIds: string[];
};

export type PreviewOrigin = "search" | "notes" | "chat";

export type PreviewTarget = {
  origin: PreviewOrigin;
  path: string;
  paragraphId?: string;
  query?: string;
  highlightTerms?: string[];
  source?: string;
  title?: string;
  fallbackBody?: string;
};

export type PreviewRescopePayload = {
  pathPrefix: string;
  label: string;
};
