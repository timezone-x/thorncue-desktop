// Trim and cut maths, ported from reference/web/app.js (L128-176). Times are source-file
// seconds; the file is never changed. Rust (tqs + engine) is the authority: this is only for
// drawing and for previewing a drag before it is committed.

export const MIN_LEN = 0.05;

export interface EditFields {
  trim_start: number;
  trim_end: number | null;
  cuts: [number, number][];
}

export const r4 = (x: number) => Math.round(x * 1e4) / 1e4;
export const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));
export const outPoint = (e: EditFields, dur: number) => (e.trim_end == null ? dur : Math.min(e.trim_end, dur));

export function subtractCuts(ranges: [number, number][], cuts: [number, number][]): [number, number][] {
  let out = ranges;
  for (const [a, b] of cuts) {
    const next: [number, number][] = [];
    for (const [s, e] of out) {
      if (b <= s || a >= e) {
        next.push([s, e]);
        continue;
      }
      if (a > s) next.push([s, a]);
      if (b < e) next.push([b, e]);
    }
    out = next;
  }
  return out.filter(([s, e]) => e - s > 0.001);
}

export function keptSegments(e: EditFields, dur: number): [number, number][] {
  const start = clamp(e.trim_start, 0, dur);
  let end = outPoint(e, dur);
  if (end <= start) end = dur;
  return subtractCuts([[start, end]], e.cuts);
}

export const segmentsLength = (segs: [number, number][]) => segs.reduce((n, [s, e]) => n + e - s, 0);

export function mergeCuts(cuts: [number, number][]): [number, number][] {
  const sorted = cuts.map((p) => [r4(p[0]), r4(p[1])] as [number, number]).sort((a, b) => a[0] - b[0]);
  const out: [number, number][] = [];
  for (const p of sorted) {
    const last = out[out.length - 1];
    if (last && p[0] <= last[1]) last[1] = Math.max(last[1], p[1]);
    else out.push(p);
  }
  return out;
}
