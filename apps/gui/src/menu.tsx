// A chip that opens a menu: the styled replacement for native <select> in the composer.
// Items carry an icon and an optional sub-line; headers split them into groups. Arrow keys
// move, Enter picks, Escape or a click outside closes.
//
// The menu is rendered into <body> (a portal) at a fixed position next to its chip, so a
// scrolling or clipping container around the chip (the plan dock, a card) cannot cut it off. It
// opens below the chip (above with `up`) and flips when that side has less room, stays inside
// the viewport left and right, is at most as tall as the room it has, and follows the chip
// when anything scrolls or the window is resized.

import { useCallback, useEffect, useId, useLayoutEffect, useRef, useState, type CSSProperties, type ReactNode } from "react";
import { createPortal } from "react-dom";
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

/** Gap between the chip and its menu, and the least distance kept from the viewport's edges. */
const GAP = 6;
const EDGE = 8;
/** The tallest a menu gets, room permitting (as `.menu` in styles.css). */
const MAX_HEIGHT = 360;

export type MenuPlace = { top?: number; bottom?: number; left: number; maxHeight: number; up: boolean };

/**
 * Where a menu of `size` goes next to an `anchor` rectangle in a `view`port: on the preferred
 * side (`up` or below) unless the other side has more room for it, aligned to the anchor's left
 * or right edge and kept inside the viewport, no taller than the room on its side. Pure.
 */
export function placeMenu(
  anchor: { top: number; bottom: number; left: number; right: number },
  size: { width: number; height: number },
  view: { width: number; height: number },
  { align, up }: { align: "left" | "right"; up?: boolean },
): MenuPlace {
  const want = Math.min(size.height, MAX_HEIGHT);
  const below = view.height - anchor.bottom - GAP - EDGE;
  const above = anchor.top - GAP - EDGE;
  const goUp = up ? above >= want || above >= below : below < want && above > below;
  const room = Math.max(0, goUp ? above : below);
  const maxHeight = Math.min(MAX_HEIGHT, room);
  const raw = align === "right" ? anchor.right - size.width : anchor.left;
  const left = Math.max(EDGE, Math.min(raw, view.width - size.width - EDGE));
  return goUp
    ? { bottom: view.height - anchor.top + GAP, left, maxHeight, up: true }
    : { top: anchor.bottom + GAP, left, maxHeight, up: false };
}

export function ChipMenu({
  icon,
  label,
  title,
  items,
  align = "left",
  up,
  menuClassName,
  searchable,
}: {
  icon?: ReactNode;
  label: ReactNode;
  title?: string;
  items: MenuItem[];
  align?: "left" | "right";
  up?: boolean;
  /** Extra class on the menu itself: it lives in <body>, so styles cannot reach it through the chip's parents. */
  menuClassName?: string;
  searchable?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [cursor, setCursor] = useState(-1);
  const [query, setQuery] = useState("");
  const [place, setPlace] = useState<MenuPlace | null>(null);
  const ref = useRef<HTMLDivElement>(null);
  const menu = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const listId = useId();
  const visible = filterMenu(items, searchable ? query : "");
  const picks = visible.filter((i): i is Extract<MenuItem, { kind: "item" }> => i.kind === "item");
  const activeCursor = cursor >= 0 && cursor < picks.length ? cursor : picks.findIndex((i) => !i.disabled);

  const reposition = useCallback(() => {
    const anchor = ref.current;
    const el = menu.current;
    if (!anchor || !el) return;
    const r = anchor.getBoundingClientRect();
    // The chip scrolled out of sight: a menu floating on its own would point at nothing.
    if (r.bottom < 0 || r.top > window.innerHeight || r.right < 0 || r.left > window.innerWidth) {
      setOpen(false);
      return;
    }
    const options = el.querySelector<HTMLElement>(".menu-options");
    const height = el.scrollHeight + (options ? Math.max(0, options.scrollHeight - options.clientHeight) : 0);
    const next = placeMenu(r, { width: el.offsetWidth, height }, { width: window.innerWidth, height: window.innerHeight }, { align, up });
    setPlace((old) =>
      old && old.top === next.top && old.bottom === next.bottom && old.left === next.left && old.maxHeight === next.maxHeight ? old : next,
    );
  }, [align, up]);

  // Measured before the first paint, so the menu never shows at a wrong place.
  useLayoutEffect(() => {
    if (!open) {
      setPlace(null);
      return;
    }
    reposition();
  }, [open, reposition, items.length, query, visible.length]);

  // A portal is hidden until measured; autofocus during its hidden mount cannot focus it.
  const placed = place !== null;
  useEffect(() => {
    if (open && placed) menu.current?.querySelector<HTMLInputElement>(".menu-search")?.focus();
  }, [open, placed]);

  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => {
      const target = e.target as Node;
      if (ref.current?.contains(target) || menu.current?.contains(target)) return;
      setOpen(false);
    };
    let frame = 0;
    const follow = (e: Event) => {
      // Scrolling the menu's own list moves nothing.
      if (e.target instanceof Node && menu.current?.contains(e.target)) return;
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(reposition);
    };
    window.addEventListener("mousedown", close);
    window.addEventListener("scroll", follow, true);
    window.addEventListener("resize", follow);
    return () => {
      cancelAnimationFrame(frame);
      window.removeEventListener("mousedown", close);
      window.removeEventListener("scroll", follow, true);
      window.removeEventListener("resize", follow);
    };
  }, [open, reposition]);

  // The cursor follows the keys: keep it in view inside a menu that scrolls.
  useEffect(() => {
    if (!open) return;
    menu.current?.querySelector<HTMLElement>(".menu-item.cursor")?.scrollIntoView({ block: "nearest" });
  }, [open, activeCursor, query]);

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

  const style: CSSProperties = place
    ? { position: "fixed", top: place.top ?? "auto", bottom: place.bottom ?? "auto", left: place.left, right: "auto", maxHeight: place.maxHeight }
    : { position: "fixed", top: 0, left: 0, visibility: "hidden" };

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
      {open && createPortal(
        <div ref={menu} className={`menu pop ${place?.up ? "up" : ""} ${menuClassName ?? ""}`} style={style}>
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
      , document.body)}
    </div>
  );
}
