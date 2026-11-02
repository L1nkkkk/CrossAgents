/** Compact token counts for metrics and axes. */
export function compactUsage(n: number): string {
  const a = Math.abs(n);
  const unit = a >= 999_500_000 ? 1e9 : a >= 999_500 ? 1e6 : a >= 1e3 ? 1e3 : 1;
  return unit === 1
    ? String(Math.round(n))
    : `${+(n / unit).toFixed(a / unit >= 10 ? 0 : 1)}${unit === 1e9 ? "B" : unit === 1e6 ? "M" : "K"}`;
}
/** Compare equal-length windows; an absent or zero baseline has no percentage. */
export function comparison(
  now: number,
  previous: number | null,
): string | null {
  if (previous === null || previous === 0) return null;
  const change = Math.round(((now - previous) / previous) * 100);
  return `${change >= 0 ? "↑" : "↓"} ${Math.abs(change)}%`;
}
/** Shape-preserving cubic Hermite interpolation, including descending x for area boundaries. */
export function smoothPath(points: { x: number; y: number }[]): string {
  if (!points.length) return "";
  if (points.length === 1) return `M${points[0].x},${points[0].y}`;
  const slopes = points
    .slice(1)
    .map((p, i) => (p.y - points[i].y) / (p.x - points[i].x));
  const tangent = points.map((_, i) =>
    i === 0
      ? slopes[0]
      : i === points.length - 1
        ? slopes[i - 1]
        : slopes[i - 1] * slopes[i] <= 0
          ? 0
          : 2 / (1 / slopes[i - 1] + 1 / slopes[i]),
  );
  return (
    `M${points[0].x},${points[0].y}` +
    points
      .slice(1)
      .map((p, i) => {
        const a = points[i],
          dx = (p.x - a.x) / 3;
        return ` C${a.x + dx},${a.y + dx * tangent[i]} ${p.x - dx},${p.y - dx * tangent[i + 1]} ${p.x},${p.y}`;
      })
      .join("")
  );
}

/** Choose a rounded axis step and a top that includes the largest value. */
export function niceScale(
  max: number,
  ticks = 4,
): { top: number; step: number } {
  if (max <= 0) return { top: 1, step: 0.25 };
  const raw = max / ticks;
  const magnitude = 10 ** Math.floor(Math.log10(raw));
  const step = [1, 2, 2.5, 5, 10]
    .map((n) => n * magnitude)
    .find((n) => n >= raw)!;
  return { top: Math.ceil(max / step) * step, step };
}
