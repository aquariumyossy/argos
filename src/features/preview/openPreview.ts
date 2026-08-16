import { invoke } from "@tauri-apps/api/core";
import type { PreviewTarget, SearchHit } from "./types";

export async function openPreview(target: PreviewTarget): Promise<void> {
  const path = target.path.trim();
  if (!path) return;
  await invoke("show_preview_window", { target });
}

export function searchHitToPreviewTarget(
  hit: SearchHit,
  query: string,
): PreviewTarget {
  return {
    origin: "search",
    path: hit.path,
    paragraphId: hit.id,
    query: query.trim() || undefined,
    highlightTerms: hit.highlightTerms,
    source: hit.source,
    title: hit.title,
    fallbackBody: hit.previewText,
  };
}
