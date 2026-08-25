import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
} from "react";
import { createPortal } from "react-dom";
import type { MemoMdKind } from "./memoMdInsert";

const MENU_MARGIN = 8;
const MENU_MIN_WIDTH = 176;
const MENU_EST_HEIGHT = 260;

const ITEMS: { kind: MemoMdKind; label: string; hint: string }[] = [
  { kind: "h1", label: "見出し 1", hint: "#" },
  { kind: "h2", label: "見出し 2", hint: "##" },
  { kind: "h3", label: "見出し 3", hint: "###" },
  { kind: "list", label: "リスト", hint: "-" },
  { kind: "todo", label: "ToDo", hint: "- [ ]" },
];

function placeMenu(rect: DOMRect, dropUp: boolean, menuWidth: number): CSSProperties {
  const vw = window.innerWidth;
  let left = rect.right - menuWidth;
  if (left < MENU_MARGIN) left = MENU_MARGIN;
  if (left + menuWidth > vw - MENU_MARGIN) {
    left = Math.max(MENU_MARGIN, vw - MENU_MARGIN - menuWidth);
  }
  return {
    position: "fixed",
    top: dropUp ? undefined : rect.bottom + 6,
    bottom: dropUp ? window.innerHeight - rect.top + 6 : undefined,
    left,
    zIndex: 50,
  };
}

type Props = {
  onPick: (kind: MemoMdKind) => void;
  onCapture?: () => void;
};

export default function MemoMdHelper({ onPick, onCapture }: Props) {
  const [open, setOpen] = useState(false);
  const [dropUp, setDropUp] = useState(false);
  const [menuStyle, setMenuStyle] = useState<CSSProperties>({});
  const wrapRef = useRef<HTMLDivElement | null>(null);
  const menuRef = useRef<HTMLDivElement | null>(null);

  useLayoutEffect(() => {
    if (!open || !wrapRef.current) return;
    const place = () => {
      const btn = wrapRef.current;
      if (!btn) return;
      const rect = btn.getBoundingClientRect();
      const up =
        rect.bottom + MENU_EST_HEIGHT > window.innerHeight &&
        rect.top > MENU_EST_HEIGHT;
      setDropUp(up);
      const width = menuRef.current?.offsetWidth || MENU_MIN_WIDTH;
      setMenuStyle(placeMenu(rect, up, width));
    };
    place();
    window.addEventListener("resize", place);
    window.addEventListener("scroll", place, true);
    return () => {
      window.removeEventListener("resize", place);
      window.removeEventListener("scroll", place, true);
    };
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      const t = e.target as Node;
      if (wrapRef.current?.contains(t) || menuRef.current?.contains(t)) return;
      setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onDoc);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDoc);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  function pick(kind: MemoMdKind) {
    setOpen(false);
    onPick(kind);
  }

  return (
    <div className="notes-md-helper" ref={wrapRef}>
      <button
        type="button"
        className={open ? "notes-legal-toggle active" : "notes-legal-toggle"}
        title="見出し・リスト・ToDo・今日の日付を入れる"
        aria-label="記法を挿入"
        aria-expanded={open}
        aria-haspopup="menu"
        onMouseDown={(e) => {
          onCapture?.();
          e.preventDefault();
        }}
        onClick={() => setOpen((v) => !v)}
      >
        記法
      </button>
      {open
        ? createPortal(
            <div
              ref={menuRef}
              className={dropUp ? "notes-md-helper-menu up" : "notes-md-helper-menu"}
              role="menu"
              style={menuStyle}
              onMouseDown={(e) => e.preventDefault()}
            >
              {ITEMS.map((item) => (
                <button
                  key={item.kind}
                  type="button"
                  className="notes-md-helper-item"
                  role="menuitem"
                  onClick={() => pick(item.kind)}
                >
                  <span>{item.label}</span>
                  <span className="notes-md-helper-hint">{item.hint}</span>
                </button>
              ))}
              <div className="notes-md-helper-sep" />
              <button
                type="button"
                className="notes-md-helper-item"
                role="menuitem"
                onClick={() => pick("date")}
              >
                <span>今日の日付</span>
                <span className="notes-md-helper-hint">@日付</span>
              </button>
            </div>,
            document.body,
          )
        : null}
    </div>
  );
}
