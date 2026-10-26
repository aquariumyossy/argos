import { useEffect, useRef, useState } from "react";
import {
  type SavedPrompt,
  firstLine,
  forgetSavedPrompt,
  loadSavedPrompts,
  needsReplaceConfirm,
  rememberSavedPrompt,
  saveBlockReason,
  saveWasTooLong,
  titleFromBody,
} from "./savedPrompts";

type LibraryMode = "list" | "name" | "confirm";

type PromptLibraryProps = {
  text: string;
  disabled: boolean;
  onRecall: (body: string) => void;
};

function isImeEnter(event: { nativeEvent: KeyboardEvent; keyCode: number }): boolean {
  return event.nativeEvent.isComposing || event.keyCode === 229;
}

export default function PromptLibrary({ text, disabled, onRecall }: PromptLibraryProps) {
  const [open, setOpen] = useState(false);
  const [prompts, setPrompts] = useState<SavedPrompt[]>([]);
  const [mode, setMode] = useState<LibraryMode>("list");
  const [draftTitle, setDraftTitle] = useState("");
  const [pending, setPending] = useState<SavedPrompt | null>(null);
  const [tooLong, setTooLong] = useState(false);
  const rootRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (disabled) setOpen(false);
  }, [disabled]);

  useEffect(() => {
    if (!open) return;
    const onDoc = (event: MouseEvent) => {
      const target = event.target;
      if (!(target instanceof Node)) return;
      if (rootRef.current?.contains(target)) return;
      setOpen(false);
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onDoc);
    window.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDoc);
      window.removeEventListener("keydown", onKey);
    };
  }, [open]);

  function showList() {
    setMode("list");
    setPending(null);
    setDraftTitle("");
  }

  function openPanel() {
    setPrompts(loadSavedPrompts());
    setTooLong(false);
    showList();
    setOpen(true);
  }

  function startSave() {
    const blocked = saveBlockReason(text);
    if (blocked === "empty") return;
    if (blocked === "tooLong") {
      setTooLong(true);
      showList();
      return;
    }
    setTooLong(false);
    setDraftTitle(titleFromBody(text));
    setMode("name");
  }

  function commitSave() {
    const result = rememberSavedPrompt({ title: draftTitle, body: text });
    if (!result.ok) {
      setTooLong(saveWasTooLong(result));
      showList();
      return;
    }
    setPrompts(result.prompts);
    setTooLong(false);
    showList();
  }

  function pick(prompt: SavedPrompt) {
    if (needsReplaceConfirm(text, prompt.body)) {
      setPending(prompt);
      setMode("confirm");
      return;
    }
    onRecall(prompt.body);
    setOpen(false);
  }

  function confirmRecall() {
    if (!pending) return;
    onRecall(pending.body);
    setOpen(false);
  }

  return (
    <div className="chat-prompt-wrap" ref={rootRef}>
      <button
        type="button"
        className="chat-tpl"
        title="プロンプトの保存と呼出"
        aria-label="プロンプトの保存と呼出"
        aria-expanded={open}
        disabled={disabled}
        onClick={() => {
          if (open) setOpen(false);
          else openPanel();
        }}
      >
        保存した文章
      </button>
      {open ? (
        <div className="chat-prompt-panel" role="dialog" aria-label="保存した文章">
          {mode === "name" ? (
            <>
              <label className="chat-prompt-name">
                <span>名前</span>
                <input
                  value={draftTitle}
                  autoFocus
                  aria-label="名前"
                  onChange={(e) => setDraftTitle(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key !== "Enter" || isImeEnter(e)) return;
                    e.preventDefault();
                    commitSave();
                  }}
                />
              </label>
              <div className="chat-prompt-actions">
                <button type="button" className="chat-btn primary" onClick={commitSave}>
                  保存
                </button>
                <button type="button" className="chat-btn" onClick={showList}>
                  戻る
                </button>
              </div>
            </>
          ) : mode === "confirm" && pending ? (
            <>
              <p className="chat-prompt-title">今の文章を置き換えます</p>
              <p className="chat-prompt-preview">{pending.title}</p>
              <div className="chat-prompt-actions">
                <button type="button" className="chat-btn primary" onClick={confirmRecall}>
                  呼び出す
                </button>
                <button type="button" className="chat-btn" onClick={showList}>
                  戻る
                </button>
              </div>
            </>
          ) : (
            <>
              <button
                type="button"
                className="chat-btn primary"
                disabled={text.trim().length === 0}
                onClick={startSave}
              >
                この文章を保存
              </button>
              {tooLong ? (
                <p className="chat-prompt-warn">2万字を超えるので保存できません。</p>
              ) : null}
              <div className="chat-prompt-list">
                {prompts.length === 0 ? (
                  <p className="chat-prompt-empty">保存した文章はまだありません。</p>
                ) : (
                  prompts.map((prompt) => {
                    const preview = firstLine(prompt.body);
                    return (
                      <div className="chat-prompt-row" key={prompt.id}>
                        <button
                          type="button"
                          className="chat-prompt-pick"
                          onClick={() => pick(prompt)}
                        >
                          <span className="chat-prompt-title">{prompt.title}</span>
                          {preview && preview !== prompt.title ? (
                            <span className="chat-prompt-preview">{preview}</span>
                          ) : null}
                        </button>
                        <button
                          type="button"
                          className="chat-prompt-drop"
                          aria-label={`${prompt.title} を削除`}
                          onClick={() => {
                            setPrompts(forgetSavedPrompt(prompt.id));
                            if (pending?.id === prompt.id) showList();
                          }}
                        >
                          削除
                        </button>
                      </div>
                    );
                  })
                )}
              </div>
            </>
          )}
        </div>
      ) : null}
    </div>
  );
}
