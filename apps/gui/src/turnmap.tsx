// The marks on the right of a conversation, one per message from a person, like the desktop
// apps of Claude and Codex: the message being read has the longer mark; hovering the marks lists
// the messages, and a click scrolls to one.

import { useEffect, useState, type RefObject } from "react";

export type MapEntry = { id: string; text: string };

const find = (scroller: HTMLElement, id: string) => scroller.querySelector<HTMLElement>(`[data-turn="${CSS.escape(id)}"]`);

export function TurnMap({ scroller, entries }: { scroller: RefObject<HTMLDivElement | null>; entries: MapEntry[] }) {
  const [current, setCurrent] = useState(0);
  const [scrollable, setScrollable] = useState(false);
  const [open, setOpen] = useState(false);

  useEffect(() => {
    const el = scroller.current;
    if (!el) return;
    const update = () => {
      setScrollable(el.scrollHeight > el.clientHeight + 8);
      // The last message whose top has passed near the top of the view; at the end, the last.
      const line = el.getBoundingClientRect().top + Math.min(120, el.clientHeight / 4);
      let at = 0;
      entries.forEach((e, i) => {
        const node = find(el, e.id);
        if (node && node.getBoundingClientRect().top <= line) at = i;
      });
      if (el.scrollHeight - el.scrollTop - el.clientHeight < 4) at = entries.length - 1;
      setCurrent(at);
    };
    update();
    el.addEventListener("scroll", update, { passive: true });
    const ro = new ResizeObserver(update);
    ro.observe(el);
    if (el.firstElementChild) ro.observe(el.firstElementChild);
    return () => {
      el.removeEventListener("scroll", update);
      ro.disconnect();
    };
  }, [scroller, entries]);

  if (entries.length < 2 || !scrollable) return null;
  const go = (id: string) => {
    const el = scroller.current;
    if (el) find(el, id)?.scrollIntoView({ block: "start", behavior: "smooth" });
  };

  return (
    <nav className="turn-map" aria-label="跳到某条消息" onMouseEnter={() => setOpen(true)} onMouseLeave={() => setOpen(false)}>
      {open && (
        <div className="turn-list">
          <div className="turn-list-inner">
            {entries.map((e, i) => (
              <button key={e.id} className={`turn-entry ${i === current ? "on" : ""}`} onClick={() => go(e.id)} title={e.text.slice(0, 300)}>
                <span className="turn-text">{e.text.replace(/\s+/g, " ").trim() || "（空消息）"}</span>
                <span className="tick" />
              </button>
            ))}
          </div>
        </div>
      )}
      <div className="turn-ticks">
        {entries.map((e, i) => (
          <button key={e.id} className={`tick ${i === current ? "on" : ""}`} onClick={() => go(e.id)} aria-label={`第 ${i + 1} 条消息`} />
        ))}
      </div>
    </nav>
  );
}
