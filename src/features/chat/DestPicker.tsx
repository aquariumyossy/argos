import { useCallback, useEffect, useRef, useState, type CSSProperties } from "react";
import "./chatDestPicker.css";

export type DestItem = {
  id: string;
  title: string;
};

function readLast(key: string): string {
  try {
    return localStorage.getItem(key) ?? "";
  } catch {
    return "";
  }
}

function itemTitle(t: DestItem, fallback: string): string {
  const s = t.title.trim();
  return s || fallback;
}

type Props = {
  className?: string;
  buttonClassName?: string;
  title: string;
  ariaLabel?: string;
  disabled?: boolean;
  children: React.ReactNode;
  newLabel: string;
  emptyLabel: string;
  fallbackTitle: string;
  lastKey: string;
  loadItems: () => Promise<DestItem[]>;
  loadActiveId: () => Promise<string>;
  onPick: (id: "new" | string) => void;
};

export default function DestPicker({
  className,
  buttonClassName,
  title,
  ariaLabel,
  disabled,
  children,
  newLabel,
  emptyLabel,
  fallbackTitle,
  lastKey,
  loadItems,
  loadActiveId,
  onPick,
}: Props) {
  const [open, setOpen] = useState(false);
  const [dropUp, setDropUp] = useState(false);
  const [menuStyle, setMenuStyle] = useState<CSSProperties>({});
  const [items, setItems] = useState<DestItem[]>([]);
  const [activeId, setActiveId] = useState("");
  const wrapRef = useRef<HTMLDivElement | null>(null);

  const load = useCallback(async () => {
    const list = await loadItems();
    setItems(list);
    try {
      setActiveId(await loadActiveId());
    } catch {
      setActiveId("");
    }
  }, [loadItems, loadActiveId]);

  useEffect(() => {
    if (!open) return;
    void load().catch(() => setItems([]));
  }, [open, load]);

  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      const el = wrapRef.current;
      if (!el) return;
      if (e.target instanceof Node && !el.contains(e.target)) {
        setOpen(false);
      }
    };
    document.addEventListener("mousedown", onDoc);
    return () => document.removeEventListener("mousedown", onDoc);
  }, [open]);

  function toggle() {
    if (disabled) return;
    setOpen((v) => {
      const next = !v;
      if (next && wrapRef.current) {
        const rect = wrapRef.current.getBoundingClientRect();
        const up = rect.bottom + 260 > window.innerHeight && rect.top > 260;
        setDropUp(up);
        setMenuStyle({
          position: "fixed",
          top: up ? undefined : rect.bottom + 6,
          bottom: up ? window.innerHeight - rect.top + 6 : undefined,
          right: Math.max(8, window.innerWidth - rect.right),
          left: "auto",
        });
      }
      return next;
    });
  }

  function pick(id: "new" | string) {
    setOpen(false);
    onPick(id);
  }

  const lastDest = readLast(lastKey);

  return (
    <div
      className={className ? `chat-dest-wrap ${className}` : "chat-dest-wrap"}
      ref={wrapRef}
      onClick={(e) => e.stopPropagation()}
      onMouseDown={(e) => e.stopPropagation()}
    >
      <button
        type="button"
        className={buttonClassName}
        title={title}
        aria-label={ariaLabel ?? title}
        aria-expanded={open}
        aria-haspopup="menu"
        disabled={disabled}
        onClick={toggle}
      >
        {children}
      </button>
      {open ? (
        <div
          className={dropUp ? "chat-dest-menu up" : "chat-dest-menu"}
          role="menu"
          style={menuStyle}
        >
          <button
            type="button"
            className="chat-dest-item"
            role="menuitem"
            onClick={() => pick("new")}
          >
            {newLabel}
          </button>
          {items.length === 0 ? (
            <p className="chat-dest-empty">{emptyLabel}</p>
          ) : (
            <ul className="chat-dest-list">
              {items.map((t) => {
                const isActive = t.id === activeId;
                const isLast = t.id === lastDest;
                return (
                  <li key={t.id}>
                    <button
                      type="button"
                      className={
                        isActive ? "chat-dest-item active" : "chat-dest-item"
                      }
                      role="menuitem"
                      onClick={() => pick(t.id)}
                    >
                      <span className="chat-dest-title">
                        {itemTitle(t, fallbackTitle)}
                      </span>
                      {isLast ? (
                        <span className="chat-dest-tag">前回</span>
                      ) : null}
                      {isActive ? (
                        <span className="chat-dest-tag">表示中</span>
                      ) : null}
                    </button>
                  </li>
                );
              })}
            </ul>
          )}
        </div>
      ) : null}
    </div>
  );
}
