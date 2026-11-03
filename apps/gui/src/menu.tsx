// A chip that opens a menu: the styled replacement for native <select> in the composer.
// Items carry an icon and an optional sub-line; headers split them into groups. Arrow keys
// move, Enter picks, Escape or a click outside closes.

import { useEffect, useId, useRef, useState, type ReactNode } from "react";
import { Icon } from "./components";
import { filterMenu } from "./lib/menuSearch";

export type MenuItem =
  | {
      kind: "item";
      key: string;
      label: ReactNode;
      sub?: ReactNode;
      searchText?: string;
      /** A sub-line of sentences reads left to right; model ids keep their tail by default. */
      subDir?: "ltr";
      icon?: ReactNode;
      on?: boolean;
      disabled?: boolean;
      onPick: () => void;
    }
  | { kind: "header"; key: string; label: ReactNode };

export function ChipMenu({ icon, label, title, items, align = "left", up, searchable }: { icon?: ReactNode; label: ReactNode; title?: string; items: MenuItem[]; align?: "left" | "right"; up?: boolean; searchable?: boolean }) {
  const [open, setOpen] = useState(false);
  const [cursor, setCursor] = useState(-1);
  const [query, setQuery] = useState("");
  const ref = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const listId = useId();
  const visible = filterMenu(items, searchable ? query : "");
  const picks = visible.filter((i): i is Extract<MenuItem, { kind: "item" }> => i.kind === "item");
  const activeCursor = cursor >= 0 && cursor < picks.length ? cursor : picks.findIndex((i) => !i.disabled);

  useEffect(() => {
    if (open) ref.current?.querySelector<HTMLElement>(".menu-item.cursor")?.scrollIntoView({ block: "nearest" });
  }, [open, cursor, query]);

  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => !ref.current?.contains(e.target as Node) && setOpen(false);
    window.addEventListener("mousedown", close);
    return () => window.removeEventListener("mousedown", close);
  }, [open]);

  const show = () => {
    setQuery("");
    const all = items.filter((i) => i.kind === "item");
    const on = all.findIndex((i) => i.on && !i.disabled);
    setCursor(on >= 0 ? on : all.findIndex((i) => !i.disabled));
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
    trigger.current?.focus();
  };

  return (
    <div
      className="chip-menu"
      ref={ref}
      onKeyDown={(e) => {
        if (e.nativeEvent.isComposing) return;
        if (!open) {
          if (e.key === "ArrowDown" || e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            show();
          }
          return;
        }
        if (e.key === "Escape") {
          setOpen(false);
          trigger.current?.focus();
        }
        else if (e.key === "ArrowDown") setCursor(step(activeCursor, 1));
        else if (e.key === "ArrowUp") setCursor(step(activeCursor, -1));
        else if (e.key === "Enter") pick(activeCursor);
        else return;
        e.preventDefault();
      }}
    >
      <button ref={trigger} type="button" className={`chip-pick ${open ? "open" : ""}`} onClick={() => (open ? setOpen(false) : show())} title={title} aria-haspopup="listbox" aria-controls={open ? listId : undefined} aria-expanded={open}>
        {icon}
        <span className="chip-label">{label}</span>
        <Icon name="chevron" size={12} />
      </button>
      {open && (
        <div className={`menu ${align} ${up ? "up" : ""}`}>
          {searchable && <input className="menu-search" role="combobox" aria-expanded={open} aria-controls={listId} aria-activedescendant={activeCursor >= 0 ? `${listId}-${activeCursor}` : undefined} aria-label="搜索模型" placeholder="搜索模型、平台、账号…" value={query} autoFocus onChange={(e) => { setQuery(e.target.value); setCursor(-1); }} />}
          <div id={listId} className="menu-options" role="listbox" aria-label={title}>
          {picks.length === 0 && <div className="menu-head" role="status">没有匹配的模型</div>}
          {visible.map((item) => {
            if (item.kind === "header") return <div key={item.key} className="menu-head">{item.label}</div>;
            const i = picks.indexOf(item);
            return (
              <button
                type="button"
                key={item.key}
                id={`${listId}-${i}`}
                role="option"
                aria-selected={!!item.on}
                aria-disabled={item.disabled || undefined}
                disabled={item.disabled}
                className={`menu-item ${i === activeCursor ? "cursor" : ""} ${item.disabled ? "disabled" : ""}`}
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
        </div>
      )}
    </div>
  );
}
