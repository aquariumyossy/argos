import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { emit } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import ChatDestPicker, { attachToChat } from "../chat/ChatDestPicker";
import NoteDestPicker, { keepToNote } from "../notes/NoteDestPicker";
import { formatExportBody } from "../notes/exportNoteText";
import { PreviewBody } from "./PreviewBody";
import {
  fileBaseName,
  formatMailDateYmd,
  formatMailFolderMeta,
  formatMailScopeLabel,
  isOutlookHit,
  parentDir,
  scopeChipLabel,
} from "./hitMeta";
import {
  applyPreviewHighlights,
  clearPreviewHighlights,
  collectPreviewHighlightTerms,
  isJsonPath,
} from "./markdownPreview";
import {
  IconChat,
  IconClose,
  IconFolder,
  IconKeep,
  IconOpenFile,
  IconRescope,
} from "./previewIcons";
import type { PreviewFileResult, PreviewTarget, SearchHit } from "./types";
import "./preview.css";

const KEEP_TOAST_MS = 2200;

function destMenuOpen(): boolean {
  return Boolean(document.querySelector('[aria-expanded="true"]'));
}

function previewNavIds(file: PreviewFileResult | null): string[] {
  if (!file) return [];
  const present = new Set(file.units.map((u) => u.id));
  const ids = file.matchIds.filter((id) => present.has(id));
  if (ids.length) return ids;
  return file.units[0] ? [file.units[0].id] : [];
}

function seedHitFromTarget(target: PreviewTarget): SearchHit {
  return {
    id: target.paragraphId || target.path,
    title: target.title || fileBaseName(target.path),
    snippet: "",
    path: target.path,
    score: 0,
    source: target.source || "",
    previewText: target.fallbackBody || "",
    highlightTerms: target.highlightTerms,
    docKind: target.source === "outlook" ? "email" : undefined,
  };
}

type SettingsData = { fontSize: number };

export default function PreviewView({
  target,
}: {
  target: PreviewTarget | null;
}) {
  const [preview, setPreview] = useState<SearchHit | null>(null);
  const [previewFile, setPreviewFile] = useState<PreviewFileResult | null>(
    null,
  );
  const [previewUnitId, setPreviewUnitId] = useState<string | null>(null);
  const [matchNavIndex, setMatchNavIndex] = useState(0);
  const [loading, setLoading] = useState(false);
  const [actionError, setActionError] = useState("");
  const [keepNotice, setKeepNotice] = useState("");
  const [fontSize, setFontSize] = useState(14);
  const previewScrollRef = useRef<HTMLDivElement>(null);
  const previewSeq = useRef(0);
  const keepTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    void invoke<SettingsData>("get_settings")
      .then((s) => {
        if (typeof s.fontSize === "number" && s.fontSize > 0) {
          setFontSize(s.fontSize);
        }
      })
      .catch(() => {});
  }, []);

  useEffect(() => {
    if (!target) {
      previewSeq.current += 1;
      setPreview(null);
      setPreviewFile(null);
      setPreviewUnitId(null);
      setMatchNavIndex(0);
      setLoading(false);
      setActionError("");
      return;
    }
    const seq = ++previewSeq.current;
    const seed = seedHitFromTarget(target);
    setPreview(seed);
    setPreviewFile(null);
    setPreviewUnitId(target.paragraphId || seed.id);
    setMatchNavIndex(0);
    setActionError("");
    if ((target.source || "").toLowerCase() === "remote") {
      setPreviewFile({
        units: [seed],
        excerpt: true,
        matchIds: [seed.id],
      });
      setLoading(false);
      return;
    }
    setLoading(true);
    void invoke<PreviewFileResult>("preview_file", {
      query: (target.query ?? "").trim(),
      path: target.path,
    })
      .then((file) => {
        if (seq !== previewSeq.current) return;
        const units = file.units.length
          ? file.units
          : seed.previewText
            ? [seed]
            : [];
        if (!units.length) {
          setPreviewFile({ units: [], excerpt: false, matchIds: [] });
          setLoading(false);
          return;
        }
        const rawIds = file.matchIds.length
          ? file.matchIds
          : target.paragraphId
            ? [target.paragraphId]
            : [units[0]!.id];
        const present = new Set(units.map((u) => u.id));
        const inUnits = rawIds.filter((id) => present.has(id));
        const matchIds = inUnits.length
          ? inUnits
          : units[0]
            ? [units[0].id]
            : [];
        setPreviewFile({ ...file, units, matchIds });
        const want = target.paragraphId || seed.id;
        const unit = units.find((u) => u.id === want) ?? units[0];
        if (unit) {
          const found = matchIds.findIndex((id) => id === unit.id);
          setMatchNavIndex(found >= 0 ? found : 0);
          setPreviewUnitId(unit.id);
          setPreview(unit);
        }
        setLoading(false);
      })
      .catch((e) => {
        if (seq !== previewSeq.current) return;
        if (seed.previewText) {
          setPreviewFile({
            units: [seed],
            excerpt: false,
            matchIds: [seed.id],
          });
        } else {
          setActionError(String(e));
          setPreviewFile({ units: [], excerpt: false, matchIds: [] });
        }
        setLoading(false);
      });
  }, [target]);

  useEffect(() => {
    if (!preview) return;
    const title =
      preview.title.trim() || fileBaseName(preview.path) || "プレビュー";
    void getCurrentWindow().setTitle(title).catch(() => {});
  }, [preview]);

  const query = (target?.query ?? "").trim();

  const previewHighlightTerms = useMemo(() => {
    if (!preview) return [];
    const extra = [
      ...(target?.highlightTerms ?? []),
      ...(preview.highlightTerms ?? []),
      ...((previewFile?.units ?? []).flatMap((u) => u.highlightTerms ?? [])),
    ];
    return collectPreviewHighlightTerms(query, extra);
  }, [preview, previewFile, query, target?.highlightTerms]);

  useLayoutEffect(() => {
    if (!preview) {
      clearPreviewHighlights();
      return;
    }
    const root = previewScrollRef.current;
    if (!root) {
      clearPreviewHighlights();
      return;
    }
    const els = Array.from(
      root.querySelectorAll<HTMLElement>(".preview-body"),
    );
    applyPreviewHighlights(els, previewHighlightTerms);
    return () => clearPreviewHighlights();
  }, [preview, previewFile, previewHighlightTerms]);

  const scrollToMatch = useCallback((unitId: string) => {
    const root = previewScrollRef.current;
    if (!root) return;
    const el = Array.from(
      root.querySelectorAll<HTMLElement>("[data-preview-unit]"),
    ).find((node) => node.dataset.previewUnit === unitId);
    el?.scrollIntoView({ block: "center", inline: "nearest" });
  }, []);

  useEffect(() => {
    if (!preview || !previewFile || !previewUnitId) return;
    requestAnimationFrame(() => scrollToMatch(previewUnitId));
  }, [preview, previewFile, previewUnitId, scrollToMatch]);

  const stepMatch = useCallback(
    (delta: number) => {
      const ids = previewNavIds(previewFile);
      if (ids.length === 0) return;
      setMatchNavIndex((i) => {
        const next = (i + delta + ids.length) % ids.length;
        const id = ids[next];
        if (id) {
          setPreviewUnitId(id);
          const unit = previewFile?.units.find((u) => u.id === id);
          if (unit) setPreview(unit);
          requestAnimationFrame(() => scrollToMatch(id));
        }
        return next;
      });
    },
    [previewFile, scrollToMatch],
  );

  const showKeepNotice = useCallback((text: string) => {
    setKeepNotice(text);
    if (keepTimerRef.current) clearTimeout(keepTimerRef.current);
    keepTimerRef.current = setTimeout(() => setKeepNotice(""), KEEP_TOAST_MS);
  }, []);

  const closePreview = useCallback(async () => {
    try {
      await invoke("hide_preview_window");
    } catch (e) {
      console.error(e);
    }
  }, []);

  const openFile = useCallback(async () => {
    if (!preview) return;
    setActionError("");
    try {
      await invoke("open_hit", { path: preview.path });
    } catch (e) {
      setActionError(String(e));
    }
  }, [preview]);

  const openFolder = useCallback(async () => {
    if (!preview || isOutlookHit(preview)) return;
    setActionError("");
    try {
      await invoke("open_containing_folder", { path: preview.path });
    } catch (e) {
      setActionError(String(e));
    }
  }, [preview]);

  const rescopeToHitFolder = useCallback(async () => {
    if (!preview) return;
    setActionError("");
    let pathPrefix = "";
    let label = "";
    if (isOutlookHit(preview)) {
      const folder = (preview.mailFolder ?? "").trim();
      if (!folder) {
        setActionError(
          "このメールの Outlook フォルダ名が不明なため、フォルダ内検索できません。",
        );
        return;
      }
      pathPrefix = `mailfolder:${folder}`;
      label = formatMailScopeLabel(folder);
    } else {
      const dir = parentDir(preview.path);
      if (!dir) return;
      pathPrefix = dir;
      label = scopeChipLabel(dir);
    }
    try {
      await emit("preview-rescope", { pathPrefix, label });
      await invoke("show_popup_window");
    } catch (e) {
      setActionError(String(e));
    }
  }, [preview]);

  const keepParagraph = useCallback(
    async (noteId: "new" | string) => {
      if (!preview) return;
      const unit =
        previewFile?.units.find((u) => u.id === previewUnitId) ?? preview;
      setActionError("");
      try {
        const result = await keepToNote(
          {
            query,
            body: unit.previewText ?? null,
            snippet: unit.snippet ?? null,
            path: preview.path,
            title: preview.title,
            source: preview.source,
            docKind: preview.docKind ?? "",
            paragraphId: unit.id,
            label: unit.unitLabel ?? "",
            page: unit.page ?? preview.page ?? null,
            mailFrom: preview.mailFrom ?? "",
            mailDate: preview.mailDate ?? "",
            mailFolder: preview.mailFolder ?? "",
            highlightTerms: preview.highlightTerms ?? [],
          },
          noteId,
        );
        const dest = result.note?.title?.trim() || "無題のノート";
        if (result.created) {
          showKeepNotice(
            result.createdNote
              ? "新しいノートにキープした"
              : `『${dest}』にキープした`,
          );
        } else {
          showKeepNotice(`『${dest}』にすでにキープ済み`);
        }
      } catch (e) {
        setActionError(String(e));
      }
    },
    [preview, previewFile, previewUnitId, query, showKeepNotice],
  );

  const sendHitToChat = useCallback(
    async (threadId: "new" | string) => {
      if (!preview) return;
      const unit =
        previewFile?.units.find((u) => u.id === previewUnitId) ?? preview;
      setActionError("");
      try {
        let body = (unit.previewText ?? "").trim();
        const snippet = (unit.snippet ?? "").trim();
        const looksThin = !body || body === snippet || [...body].length < 80;
        if (looksThin && unit.id) {
          try {
            const hit = await invoke<SearchHit | null>("get_preview", {
              hitId: unit.id,
            });
            const previewText = hit?.previewText?.trim() ?? "";
            if ([...previewText].length > [...body].length) {
              body = previewText;
            }
          } catch {
            /* keep existing text */
          }
        }
        if (!body) body = snippet;
        body = formatExportBody(preview.path, body);
        if (!body.trim()) {
          setActionError(
            "本文が取れませんでした。プレビューを開いてから送ってください。",
          );
          return;
        }
        const result = await attachToChat(
          [
            {
              path: preview.path,
              title: preview.title,
              paragraphId: unit.id,
              body,
              query,
              origin: "attach",
            },
          ],
          preview.title.trim() || null,
          threadId,
        );
        const dest = result.thread?.title?.trim() || "新しい会話";
        if (result.added > 0) {
          showKeepNotice(
            result.createdThread
              ? "新しいチャットに送った"
              : `『${dest}』に追加した`,
          );
        } else if (result.skipped > 0) {
          showKeepNotice("同じ出典がすでに読込前にあります");
        } else {
          showKeepNotice("本文が空のため送れませんでした");
        }
      } catch (e) {
        setActionError(String(e));
      }
    },
    [preview, previewFile, previewUnitId, query, showKeepNotice],
  );

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        void closePreview();
        return;
      }
      if (destMenuOpen()) return;
      if (!preview) return;
      if (e.key === "ArrowLeft" || e.key === "[") {
        e.preventDefault();
        stepMatch(-1);
        return;
      }
      if (e.key === "ArrowRight" || e.key === "]") {
        e.preventDefault();
        stepMatch(1);
        return;
      }
      if (e.key === "Enter" && e.shiftKey) {
        e.preventDefault();
        if (!isOutlookHit(preview)) void openFolder();
        return;
      }
      if (e.key === "Enter") {
        e.preventDefault();
        void openFile();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [closePreview, openFile, openFolder, preview, stepMatch]);

  if (!target) {
    return (
      <div className="preview-window" style={{ fontSize: `${fontSize}px` }}>
        <section className="preview">
          <div className="preview-empty">プレビューするファイルを選んでください</div>
        </section>
      </div>
    );
  }

  const mail = preview ? isOutlookHit(preview) : false;
  const fromSearch = target.origin === "search";

  return (
    <div className="preview-window" style={{ fontSize: `${fontSize}px` }}>
      <section className="preview">
        <div className="preview-title">
          {preview?.title || target.title || fileBaseName(target.path)}
        </div>
        <div className="preview-path" title={target.path}>
          {preview && mail
            ? [
                preview.mailFolder
                  ? formatMailFolderMeta(preview.mailFolder)
                  : "",
                preview.mailFrom,
                formatMailDateYmd(preview.mailDate),
              ]
                .filter(Boolean)
                .join(" · ") || "Outlook メール"
            : target.path}
        </div>
        <div className="preview-actions">
          <button
            type="button"
            className="hit-action-btn"
            title={mail ? "メールを開く (Enter)" : "ファイルを開く (Enter)"}
            aria-label={mail ? "メールを開く" : "ファイルを開く"}
            onClick={() => void openFile()}
            disabled={!preview}
          >
            <IconOpenFile />
          </button>
          {preview && !mail ? (
            <button
              type="button"
              className="hit-action-btn"
              title="フォルダを開く (Shift+Enter)"
              aria-label="フォルダを開く"
              onClick={() => void openFolder()}
            >
              <IconFolder />
            </button>
          ) : null}
          {fromSearch ? (
            <button
              type="button"
              className="hit-action-btn"
              title="このフォルダ内で再検索"
              aria-label="このフォルダ内で再検索"
              onClick={() => void rescopeToHitFolder()}
              disabled={!preview}
            >
              <IconRescope />
            </button>
          ) : null}
          <NoteDestPicker
            buttonClassName="hit-action-btn"
            title={mail ? "このメールをノートにキープ" : "この段落をノートにキープ"}
            ariaLabel="ノートにキープ"
            disabled={!preview}
            onPick={(id) => void keepParagraph(id)}
          >
            <IconKeep />
          </NoteDestPicker>
          <ChatDestPicker
            buttonClassName="hit-action-btn"
            title={mail ? "このメールをチャットに送る" : "この段落をチャットに送る"}
            ariaLabel="チャットに送る"
            disabled={!preview}
            onPick={(id) => void sendHitToChat(id)}
          >
            <IconChat />
          </ChatDestPicker>
          <button
            type="button"
            className="hit-action-btn"
            title="閉じる (Esc)"
            aria-label="閉じる"
            onClick={() => void closePreview()}
          >
            <IconClose />
          </button>
        </div>
        {keepNotice ? <div className="preview-notice">{keepNotice}</div> : null}
        {actionError ? <div className="preview-error">{actionError}</div> : null}
        {(previewFile?.matchIds.length ?? 0) > 1 ? (
          <div className="preview-occ-nav" aria-live="polite">
            <button
              type="button"
              title="前のマッチへスクロール (←)"
              aria-label="前のマッチ"
              onClick={() => stepMatch(-1)}
            >
              ←
            </button>
            <span className="preview-occ-label">
              マッチ {matchNavIndex + 1} / {previewFile?.matchIds.length}
              {preview?.page != null ? ` · p.${preview.page}` : ""}
            </span>
            <button
              type="button"
              title="次のマッチへスクロール (→)"
              aria-label="次のマッチ"
              onClick={() => stepMatch(1)}
            >
              →
            </button>
          </div>
        ) : null}
        {preview?.source === "remote" ? (
          <div className="preview-excerpt-note">リモートのため抜粋のみ</div>
        ) : null}
        {previewFile?.excerpt &&
        preview?.source !== "remote" &&
        preview &&
        !isJsonPath(preview.path) ? (
          <div className="preview-excerpt-note">
            長いファイルのため、マッチ周辺の抜粋です
          </div>
        ) : null}
        <div className="preview-scroll" ref={previewScrollRef}>
          {loading && !previewFile ? (
            <div className="preview-empty">読み込み中…</div>
          ) : preview && isJsonPath(preview.path) ? (
            <PreviewBody
              hit={preview}
              query={query}
              highlightTerms={previewHighlightTerms}
            />
          ) : previewFile && previewFile.units.length > 0 ? (
            previewFile.units.map((unit) => {
              const isMatch = previewFile.matchIds.includes(unit.id);
              const isActive = unit.id === previewUnitId;
              return (
                <article
                  key={unit.id}
                  data-preview-unit={unit.id}
                  className={`preview-unit${isMatch ? " is-match" : ""}${isActive ? " is-active" : ""}`}
                  onClick={() => {
                    setPreviewUnitId(unit.id);
                    setPreview(unit);
                  }}
                >
                  {unit.unitLabel ? (
                    <div className="preview-unit-label">{unit.unitLabel}</div>
                  ) : null}
                  <PreviewBody
                    hit={unit}
                    query={query}
                    highlightTerms={previewHighlightTerms}
                  />
                </article>
              );
            })
          ) : (
            <div className="preview-empty">本文を表示できませんでした</div>
          )}
        </div>
        <div className="preview-hint">
          {(previewFile?.matchIds.length ?? 0) > 1
            ? "←→ マッチへ移動 · Esc で閉じる"
            : "Esc で閉じる"}
        </div>
      </section>
    </div>
  );
}
