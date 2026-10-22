// Small charts for the usage page, drawn by hand (no chart library; the CSP allows only local
// code). One series each, so no legend: the metric switch above the chart names what is plotted.
// The table under every chart stays as the exact view.

import { useLayoutEffect, useRef, useState } from "react";

/** 1234 → "1.2K", 338034592 → "338M". */
export function compact(n: number): string {
  const a = Math.abs(n);
  if (a >= 1e9) return `${+(n / 1e9).toFixed(a >= 1e10 ? 0 : 1)}B`;
  if (a >= 1e6) return `${+(n / 1e6).toFixed(a >= 1e7 ? 0 : 1)}M`;
  if (a >= 1e3) return `${+(n / 1e3).toFixed(a >= 1e4 ? 0 : 1)}K`;
  return String(Math.round(n));
}

/** A clean top for the y axis and its step: 0 / 100M / 200M / 300M / 400M. */
function niceScale(max: number, ticks = 4): { top: number; step: number } {
  if (max <= 0) return { top: 1, step: 1 };
  const raw = max / ticks;
  const mag = 10 ** Math.floor(Math.log10(raw));
  const step = [1, 2, 2.5, 5, 10].map((m) => m * mag).find((s) => s >= raw)!;
  return { top: Math.ceil(max / step) * step, step };
}

const DAY_MS = 86_400_000;

/** One series in a chart: a stable key, its name, and its colour (a CSS colour or var). */
export type Series = { key: string; label: string; color: string };

/**
 * Every day from the first to the last day any series has (missing days are 0), the last `max`
 * of them. `rows[i]` are series i's {day, value} pairs.
 */
export function dailySeries(rows: { day: string; value: number }[][], max = 30): { day: string; values: number[] }[] {
  const days = rows.flat().map((r) => r.day).sort();
  if (!days.length) return [];
  const maps = rows.map((rs) => new Map(rs.map((r) => [r.day, r.value])));
  const out: { day: string; values: number[] }[] = [];
  for (let t = Date.parse(days[0]); t <= Date.parse(days[days.length - 1]); t += DAY_MS) {
    const day = new Date(t).toISOString().slice(0, 10);
    out.push({ day, values: maps.map((m) => m.get(day) ?? 0) });
  }
  return out.slice(-max);
}

/**
 * Columns over days, stacked by series with a 2px surface gap, a hairline grid and compact y
 * ticks. Hovering a day shows every series' value and the total. Two or more series get a
 * legend; one series is named by `label`.
 */
export function DayColumns({ data, series, format, label }: { data: { day: string; values: number[] }[]; series: Series[]; format: (n: number) => string; label: string }) {
  const ref = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(600);
  const [hover, setHover] = useState<number>();
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const ro = new ResizeObserver(([e]) => setWidth(Math.max(240, e.contentRect.width)));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const height = 180;
  const pad = { top: 10, right: 4, bottom: 22, left: 44 };
  const plotW = width - pad.left - pad.right;
  const plotH = height - pad.top - pad.bottom;
  const totals = data.map((d) => d.values.reduce((a, b) => a + b, 0));
  const { top, step } = niceScale(Math.max(...totals, 0));
  const slot = plotW / Math.max(1, data.length);
  const bar = Math.max(2, Math.min(24, slot - 2));
  const y = (v: number) => pad.top + plotH - (v / top) * plotH;
  const every = Math.max(1, Math.ceil(46 / slot));
  const GAP = 2;

  /** A column segment from v0 up to v1; only the top one of a stack gets the rounded end. */
  const segment = (x: number, v0: number, v1: number, round: boolean) => {
    const yb = y(v0) - (v0 > 0 ? GAP : 0);
    const yt = y(v1);
    if (yb - yt < 0.5) return null;
    const r = round ? Math.min(4, bar / 2, yb - yt) : 0;
    return `M${x},${yb} V${yt + r} Q${x},${yt} ${x + r},${yt} H${x + bar - r} Q${x + bar},${yt} ${x + bar},${yt + r} V${yb} Z`;
  };

  return (
    <div className="chart" ref={ref}>
      {series.length > 1 && (
        <div className="chart-legend">
          {series.map((s) => (
            <span key={s.key}>
              <i style={{ background: s.color }} />
              {s.label}
            </span>
          ))}
        </div>
      )}
      <svg width={width} height={height} role="img" aria-label={label} onMouseLeave={() => setHover(undefined)}>
        {Array.from({ length: Math.round(top / step) + 1 }, (_, i) => i * step).map((v) => (
          <g key={v}>
            <line className="chart-grid" x1={pad.left} x2={width - pad.right} y1={y(v)} y2={y(v)} />
            <text className="chart-tick" x={pad.left - 8} y={y(v)} dy="0.32em" textAnchor="end">
              {compact(v)}
            </text>
          </g>
        ))}
        {data.map((d, i) => {
          const x = pad.left + i * slot + (slot - bar) / 2;
          const last = d.values.map((v, k) => (v > 0 ? k : -1)).reduce((a, b) => Math.max(a, b), -1);
          let acc = 0;
          return (
            <g key={d.day} className={hover === i ? "on" : ""}>
              {d.values.map((v, k) => {
                const v0 = acc;
                acc += v;
                const path = v > 0 ? segment(x, v0, acc, k === last) : null;
                return path && <path key={series[k].key} className="chart-bar" style={{ fill: series[k].color }} d={path} />;
              })}
              {i % every === (data.length - 1) % every && (
                <text className="chart-tick" x={x + bar / 2} y={height - 6} textAnchor="middle">
                  {d.day.slice(5)}
                </text>
              )}
              {/* The hit target is the whole slot, taller and wider than the column. */}
              <rect x={pad.left + i * slot} y={pad.top} width={slot} height={plotH} fill="transparent" onMouseEnter={() => setHover(i)} />
            </g>
          );
        })}
      </svg>
      {hover !== undefined && data[hover] && (
        <div className="chart-tip" style={{ left: Math.min(width - 180, Math.max(0, pad.left + hover * slot + slot / 2 - 90)) }}>
          <div className="chart-tip-head">
            {data[hover].day} · {label}
          </div>
          {series.map((s, k) => (
            <div key={s.key} className="chart-tip-row">
              <span className="chart-key" style={{ background: s.color }} />
              {s.label}
              <b>{format(data[hover].values[k])}</b>
            </div>
          ))}
          {series.length > 1 && (
            <div className="chart-tip-row total">
              合计<b>{format(totals[hover])}</b>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

/** Ranked horizontal bars: label, bar, value; the top `max` rows, the rest folded into one. */
export function RankBars({ rows, format, max = 8, other = "其他" }: { rows: { key: string; label: string; value: number }[]; format: (n: number) => string; max?: number; other?: string }) {
  const sorted = [...rows].sort((a, b) => b.value - a.value);
  const rest = sorted.slice(max);
  const shown = rest.length ? [...sorted.slice(0, max), { key: "__other", label: `${other}（${rest.length}）`, value: rest.reduce((s, r) => s + r.value, 0) }] : sorted;
  const top = Math.max(...shown.map((r) => r.value), 0) || 1;
  return (
    <div className="rank">
      {shown.map((r) => (
        <div key={r.key} className="rank-row" title={`${r.label}: ${format(r.value)}`}>
          <span className="rank-label">{r.label}</span>
          <span className="rank-track">
            <span className={`rank-bar ${r.key === "__other" ? "other" : ""}`} style={{ width: `${(r.value / top) * 100}%` }} />
          </span>
          <span className="rank-value">{format(r.value)}</span>
        </div>
      ))}
    </div>
  );
}
