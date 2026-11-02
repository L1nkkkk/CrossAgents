// A dock's row of tabs, and dragging them: to reorder, or to the other dock. Pointer events rather
// than HTML5 drag and drop, which would hide the drop zones behind a system cursor and stop at a
// terminal's canvas. Where a tab lands is lib/docks.ts `dropTarget`. In the floating drawer tabs
// do not drag: there is nowhere else to put them.

import { useEffect, useRef, useState, useSyncExternalStore, type PointerEvent as ReactPointerEvent } from "react";
import { Icon } from "./components";
import { dropTarget, type DockId, type Drop, type Rect, type Strip } from "./lib/docks";
import { closedKinds, dropTab, isTermTab, openTool, paneLabel, pickTab, shutTab, useLayout, type TabId } from "./panes";
import { closeTerminal, newTerminal, terminalsAvailable, useTabLabel, useTerminalScope } from "./terminal";

/** Pixels a pointer moves before a press on a tab becomes a drag. */
const DRAG_FROM = 5;

type Drag = { tab: TabId; x: number; y: number; drop: Drop | null };
let drag: Drag | null = null;
const listeners = new Set<() => void>();
const setDrag = (next: Drag | null) => {
  drag = next;
  listeners.forEach((l) => l());
};
const useDrag = () =>
  useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => drag,
  );

const rectOf = (el: Element | null): Rect | null => {
  if (!el) return null;
  const { left, top, right, bottom } = el.getBoundingClientRect();
  return { left, top, right, bottom };
};

/** What is on screen now, for `dropTarget`. */
function measure(x: number, y: number, tab: TabId): Drop | null {
  const center = rectOf(document.querySelector(".center-col > .main"));
  if (!center) return null;
  const strips: Strip[] = [...document.querySelectorAll<HTMLElement>("[data-tabstrip]")].map((el) => ({
    dock: el.dataset.tabstrip as DockId,
    rect: rectOf(el)!,
    tabs: [...el.querySelectorAll<HTMLElement>("[data-tab]")].map((t) => {
      const r = t.getBoundingClientRect();
      return { id: t.dataset.tab!, left: r.left, right: r.right };
    }),
  }));
  return dropTarget(x, y, tab, { strips, center, right: rectOf(document.querySelector("[data-tabdock=right]")), bottom: rectOf(document.querySelector("[data-tabdock=bottom]")) });
}

/** Press on a tab: past DRAG_FROM it is a drag, and letting go drops it; Escape calls it off. */
function startDrag(e: ReactPointerEvent, tab: TabId, onDragged: () => void) {
  if (e.button !== 0) return;
  const x0 = e.clientX;
  const y0 = e.clientY;
  let moving = false;
  const move = (ev: PointerEvent) => {
    if (!moving && Math.hypot(ev.clientX - x0, ev.clientY - y0) < DRAG_FROM) return;
    if (!moving) {
      moving = true;
      onDragged();
      document.body.classList.add("tab-dragging");
    }
    setDrag({ tab, x: ev.clientX, y: ev.clientY, drop: measure(ev.clientX, ev.clientY, tab) });
  };
  const finish = () => {
    window.removeEventListener("pointermove", move);
    window.removeEventListener("pointerup", up);
    window.removeEventListener("pointercancel", finish);
    window.removeEventListener("keydown", key, true);
    document.body.classList.remove("tab-dragging");
    setDrag(null);
  };
  const up = () => {
    const drop = drag?.drop;
    finish();
    if (drop) dropTab(tab, drop.dock, drop.index);
  };
  const key = (ev: KeyboardEvent) => {
    if (ev.key !== "Escape") return;
    ev.stopPropagation();
    finish();
  };
  window.addEventListener("pointermove", move);
  window.addEventListener("pointerup", up);
  window.addEventListener("pointercancel", finish);
  window.addEventListener("keydown", key, true);
}

/** What a tab is called. */
function useLabel() {
  const termLabel = useTabLabel();
  return (tab: TabId) => (isTermTab(tab) ? termLabel(tab) : paneLabel(tab));
}

export function TabRow({ dock, tabs, active, narrow, onCloseTask }: { dock: DockId; tabs: TabId[]; active: TabId | null; narrow: boolean; onCloseTask?: () => void }) {
  const dragging = useDrag();
  const label = useLabel();
  // A drag ends in a click on whatever it let go over: that click is not a pick.
  const dragged = useRef(false);
  const shut = (tab: TabId) => {
    if (isTermTab(tab)) return closeTerminal(tab);
    shutTab(tab);
    if (tab === "task") onCloseTask?.();
  };
  return (
    <div className="tab-row" role="tablist" data-tabstrip={narrow ? undefined : dock}>
      {tabs.map((tab) => (
        <div
          key={tab}
          data-tab={tab}
          className={`dock-tab ${tab === active ? "active" : ""} ${dragging?.tab === tab ? "dragged" : ""}`}
          onPointerDown={(e) => {
            dragged.current = false;
            if (!narrow && !(e.target as Element).closest(".dock-tab-x")) startDrag(e, tab, () => (dragged.current = true));
          }}
          onAuxClick={(e) => {
            if (e.button === 1) shut(tab);
          }}
        >
          <button
            type="button"
            role="tab"
            className="dock-tab-pick"
            aria-selected={tab === active}
            onClick={() => {
              if (!dragged.current) pickTab(dock, tab);
            }}
          >
            {isTermTab(tab) && <Icon name="terminal" size={13} />}
            {label(tab)}
          </button>
          <button type="button" className="dock-tab-x" onClick={() => shut(tab)} title={`关闭${label(tab)}`} aria-label={`关闭${label(tab)}`}>
            <Icon name="close" size={12} />
          </button>
        </div>
      ))}
      <AddTab dock={dock} />
    </div>
  );
}

/** + : a new terminal, and what has no tab now. */
function AddTab({ dock }: { dock: DockId }) {
  const { state } = useLayout();
  const [at, setAt] = useState<{ left: number; top: number } | null>(null);
  const ref = useRef<HTMLDivElement>(null);
  const closed = closedKinds(state.docks);
  const canTerminal = terminalsAvailable() && !!useTerminalScope();
  useEffect(() => {
    if (!at) return;
    const close = (e: MouseEvent) => !ref.current?.contains(e.target as Node) && setAt(null);
    window.addEventListener("mousedown", close);
    return () => window.removeEventListener("mousedown", close);
  }, [at]);
  if (!closed.length && !canTerminal) return null;
  const rows = closed.length + (canTerminal ? 1 : 0);
  return (
    <div className="dock-add" ref={ref}>
      <button
        type="button"
        className="ghost icon"
        title="打开标签"
        aria-label="打开标签"
        aria-haspopup="menu"
        aria-expanded={!!at}
        onClick={(e) => {
          if (at) return setAt(null);
          // The row scrolls sideways, which would clip a menu inside it.
          const box = e.currentTarget.getBoundingClientRect();
          setAt({ left: Math.min(box.left, window.innerWidth - 160), top: dock === "bottom" ? box.top - 8 - 36 * rows : box.bottom + 4 });
        }}
      >
        <Icon name="plus" size={15} />
      </button>
      {at && (
        <div className="menu" role="menu" style={{ left: at.left, top: at.top, right: "auto", bottom: "auto" }}>
          {canTerminal && (
            <button
              type="button"
              role="menuitem"
              className="menu-item"
              onClick={() => {
                setAt(null);
                void newTerminal(dock);
              }}
            >
              <span className="menu-icon">
                <Icon name="terminal" size={14} />
              </span>
              <span className="menu-text">
                <span className="menu-label">新建终端</span>
              </span>
            </button>
          )}
          {closed.map((p) => (
            <button
              type="button"
              key={p.kind}
              role="menuitem"
              className="menu-item"
              onClick={() => {
                setAt(null);
                // Opens here rather than wherever a new tab would go by default.
                dropTab(p.kind, dock);
                openTool(p.kind);
              }}
            >
              <span className="menu-text">
                <span className="menu-label">{p.label}</span>
              </span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

/** While a tab is dragged: where it would land, and the tab under the pointer. */
export function TabDragLayer() {
  const d = useDrag();
  const label = useLabel();
  if (!d) return null;
  const zone = d.drop?.zone;
  return (
    <>
      {zone && d.drop?.marker === undefined && <div className="drop-zone" style={{ left: zone.left, top: zone.top, width: zone.right - zone.left, height: zone.bottom - zone.top }} />}
      {zone && d.drop?.marker !== undefined && <div className="drop-marker" style={{ left: d.drop.marker - 1, top: zone.top + 6, height: zone.bottom - zone.top - 12 }} />}
      <div className="tab-ghost" style={{ left: d.x, top: d.y }}>
        {label(d.tab)}
      </div>
    </>
  );
}
