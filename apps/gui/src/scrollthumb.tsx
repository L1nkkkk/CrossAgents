// The conversation's scrollbar, drawn by the page: the thumb's length is the share of the whole
// conversation in view and its place is how far down you are, both read from the scroller itself
// and kept current as the conversation grows, so it lines up with the content and stops at the
// end. Drag the thumb, or click the track to jump there.

import { useEffect, useRef, useState, type PointerEvent as ReactPointerEvent, type RefObject } from "react";

const MIN_THUMB = 28;

export function ScrollThumb({ scroller }: { scroller: RefObject<HTMLDivElement | null> }) {
  const track = useRef<HTMLDivElement>(null);
  const [geo, setGeo] = useState({ top: 0, size: 0, show: false });
  const [dragging, setDragging] = useState(false);

  useEffect(() => {
    const el = scroller.current;
    if (!el) return;
    const update = () => {
      const h = track.current?.clientHeight ?? el.clientHeight;
      const range = el.scrollHeight - el.clientHeight;
      if (range <= 1) return setGeo({ top: 0, size: 0, show: false });
      const size = Math.max(MIN_THUMB, (el.clientHeight / el.scrollHeight) * h);
      const top = (Math.min(Math.max(el.scrollTop, 0), range) / range) * (h - size);
      setGeo({ top, size, show: true });
    };
    update();
    el.addEventListener("scroll", update, { passive: true });
    const ro = new ResizeObserver(update);
    ro.observe(el);
    // The content growing (a reply streaming in) changes the length too.
    if (el.firstElementChild) ro.observe(el.firstElementChild);
    return () => {
      el.removeEventListener("scroll", update);
      ro.disconnect();
    };
  }, [scroller]);

  /** Moving the thumb by `dy` pixels moves the content by this much. */
  const ratio = () => {
    const el = scroller.current!;
    const h = track.current!.clientHeight;
    return (el.scrollHeight - el.clientHeight) / Math.max(1, h - geo.size);
  };

  const drag = (e: ReactPointerEvent<HTMLDivElement>) => {
    e.preventDefault();
    e.stopPropagation();
    const el = scroller.current;
    if (!el) return;
    const startY = e.clientY;
    const startTop = el.scrollTop;
    const k = ratio();
    setDragging(true);
    const move = (ev: PointerEvent) => {
      el.scrollTop = startTop + (ev.clientY - startY) * k;
    };
    const up = () => {
      setDragging(false);
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  };

  // A click on the track centres the thumb there.
  const jump = (e: ReactPointerEvent<HTMLDivElement>) => {
    const el = scroller.current;
    const box = track.current?.getBoundingClientRect();
    if (!el || !box) return;
    el.scrollTop = (e.clientY - box.top - geo.size / 2) * ratio();
  };

  return (
    // Always laid out (its height is the scale), shown only when there is something to scroll.
    <div className={`scroll-track ${geo.show ? "" : "off"} ${dragging ? "dragging" : ""}`} ref={track} onPointerDown={jump} aria-hidden="true">
      <div className="scroll-thumb" style={{ height: geo.size, transform: `translateY(${geo.top}px)` }} onPointerDown={drag} />
    </div>
  );
}
