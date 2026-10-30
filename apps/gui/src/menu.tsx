// A chip that opens a menu: the styled replacement for native <select> in the composer.
// Items carry an icon and an optional sub-line; headers split them into groups. Arrow keys
// move, Enter picks, Escape or a click outside closes.

import { useEffect, useRef, useState, type ReactNode } from "react";
import { Icon } from "./components";

export type MenuItem =
  | {
      kind: "item";
      key: string;
      label: ReactNode;
      sub?: ReactNode;
      /** A sub-line of sentences reads left to right; model ids keep their tail by default. */
      subDir?: "ltr";
      icon?: ReactNode;
      on?: boolean;
      disabled?: boolean;
      onPick: () => void;
    }
  | { kind: "header"; key: string; label: ReactNode };

export function ChipMenu({ icon, label, title, items, align = "left", up }: { icon?: ReactNode; label: ReactNode; title?: string; items: MenuItem[]; align?: "left" | "right"; up?: boolean }) {
  const [open, setOpen] = useState(false);
  const [cursor, setCursor] = useState(-1);
  const ref = useRef<HTMLDivElement>(null);
  const picks = items.filter((i): i is Extract<MenuItem, { kind: "item" }> => i.kind === "item");

  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => !ref.current?.contains(e.target as Node) && setOpen(false);
    window.addEventListener("mousedown", close);
    return () => window.removeEventListener("mousedown", close);
  }, [open]);

  const show = () => {
    const on = picks.findIndex((i) => i.on && !i.disabled);
    setCursor(on >= 0 ? on : Math.max(0, picks.findIndex((i) => !i.disabled)));
    setOpen(true);
  };
  const step = (from: number, delta: number) => {
    for (let n = 1; n <= picks.length; n++) {
      const i = (from + delta * n + picks.length * 2) % picks.length;
      if (!picks[i]?.disabled) return i;
    }
    return from;
  };
  const pick = (i: number) => {
    const item = picks[i];
    if (!item || item.disabled) return;
    item.onPick();
    setOpen(false);
  };

  return (
    <div
      className="chip-menu"
      ref={ref}
      onKeyDown={(e) => {
        if (!open) {
          if (e.key === "ArrowDown" || e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            show();
          }
          return;
        }
        if (e.key === "Escape") setOpen(false);
        else if (e.key === "ArrowDown") setCursor((c) => step(c, 1));
        else if (e.key === "ArrowUp") setCursor((c) => step(c, -1));
        else if (e.key === "Enter") pick(cursor);
        else return;
        e.preventDefault();
      }}
    >
      <button type="button" className={`chip-pick ${open ? "open" : ""}`} onClick={() => (open ? setOpen(false) : show())} title={title} aria-haspopup="listbox" aria-expanded={open}>
        {icon}
        <span className="chip-label">{label}</span>
        <Icon name="chevron" size={12} />
      </button>
      {open && (
        <div className={`menu ${align} ${up ? "up" : ""}`} role="listbox">
          {items.map((item) => {
            if (item.kind === "header") return <div key={item.key} className="menu-head">{item.label}</div>;
            const i = picks.indexOf(item);
            return (
              <button
                type="button"
                key={item.key}
                role="option"
                aria-selected={!!item.on}
                aria-disabled={item.disabled || undefined}
                disabled={item.disabled}
                className={`menu-item ${i === cursor ? "cursor" : ""} ${item.disabled ? "disabled" : ""}`}
                onMouseEnter={() => !item.disabled && setCursor(i)}
                onClick={() => pick(i)}
              >
                {item.icon && <span className="menu-icon">{item.icon}</span>}
                <span className="menu-text">
                  <span className="menu-label">{item.label}</span>
                  {item.sub && <span className={`menu-sub ${item.subDir === "ltr" ? "ltr" : ""}`}>{item.sub}</span>}
                </span>
                {item.on && <Icon name="check" size={14} />}
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}
