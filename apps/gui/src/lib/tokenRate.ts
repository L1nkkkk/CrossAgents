// How fast a running turn produces output, from the cumulative token counts `agent.usage`
// carries (crates/core/src/event.rs `AgentUsage.tokens`). Pure, so the sidebar row, the session
// header and the tests compute it the same way (activity.ts keeps the samples per agent).

/** A cumulative output count as the stream reported it, at `at` (ms). */
export type RateSample = { at: number; tokens: number };

/** The sliding window the rate is taken over, ms. */
export const RATE_WINDOW = 10_000;
/** The shortest span a rate is divided by, so two close samples do not make a spike. */
const MIN_SPAN = 1_000;

/**
 * The samples with one more: those older than the window are dropped, except the newest of
 * them (the count at the window's start). A count that went down is a new session, not
 * negative output: it starts over from that sample.
 */
export function addSample(samples: RateSample[], at: number, tokens: number, window = RATE_WINDOW): RateSample[] {
  const last = samples[samples.length - 1];
  if (last && tokens < last.tokens) return [{ at, tokens }];
  const next = [...samples, { at, tokens }];
  let first = 0;
  while (first + 1 < next.length && next[first + 1].at <= at - window) first++;
  return next.slice(first);
}

/**
 * Output tokens per second over the last `window` ms before `now`: what the count grew by since
 * the window began (interpolated between the samples around its start, or from the first
 * sample when they all fall inside it), over the time from there to now. A turn that stops
 * producing slows down and then shows nothing.
 * Undefined when there is nothing to tell (fewer than two samples, no growth in the window).
 */
export function tokenRate(samples: RateSample[], now: number, window = RATE_WINDOW): number | undefined {
  if (samples.length < 2) return undefined;
  const start = now - window;
  // The count when the window began: between the two samples around it, as if output grew
  // evenly between them; the first sample when they all fall inside the window.
  let from = samples[0];
  for (let i = 0; i + 1 < samples.length && samples[i].at < start; i++) {
    const a = samples[i];
    const b = samples[i + 1];
    from = b.at <= start ? b : { at: start, tokens: a.tokens + ((b.tokens - a.tokens) * (start - a.at)) / (b.at - a.at) };
  }
  const last = samples[samples.length - 1];
  const grown = last.tokens - from.tokens;
  if (grown <= 0) return undefined;
  const span = Math.max(MIN_SPAN, now - from.at);
  return (grown * 1000) / span;
}

/** `42 tok/s`, `1.2k tok/s`; undefined for a rate that rounds to nothing (the dot is shown alone). */
export function formatRate(rate: number | undefined): string | undefined {
  if (rate === undefined || !Number.isFinite(rate)) return undefined;
  const n = Math.round(rate);
  if (n <= 0) return undefined;
  if (n >= 1000) return `${+(rate / 1000).toFixed(1)}k tok/s`;
  return `${n} tok/s`;
}
