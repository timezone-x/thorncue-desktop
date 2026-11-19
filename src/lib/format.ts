// Display helpers, ported from reference/web/app.js.
import type { Cue } from "./api";

/** m:ss, or m:ss.cc with centis (app.js fmtTime). */
export function fmtTime(s: number, centis = false): string {
  if (!Number.isFinite(s) || s < 0) s = 0;
  if (centis) {
    const t = Math.round(s * 100);
    return `${Math.floor(t / 6000)}:${String(Math.floor(t / 100) % 60).padStart(2, "0")}.${String(t % 100).padStart(2, "0")}`;
  }
  const t = Math.floor(s);
  return `${Math.floor(t / 60)}:${String(t % 60).padStart(2, "0")}`;
}

export function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(0)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

/** "12 Nov 2026, 14:05" in the operator's locale, or "Never". */
export function fmtDate(iso: string | null): string {
  if (!iso) return "Never";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  return d.toLocaleString(undefined, { day: "numeric", month: "short", year: "numeric", hour: "2-digit", minute: "2-digit" });
}

/** The cue at `index` (from 0): "Q3", or with `pad` zero-padded to the width of `count` ("Q03"
 * in a show of 24). The only place the UI builds a cue number (Rust's is tqs::cue_number).
 * Custom cue numbers (thorncue-spec#16) will replace it. */
export function cueNumber(index: number, count = 0, pad = false): string {
  const width = pad ? String(Math.max(1, count)).length : 1;
  return `Q${String(index + 1).padStart(width, "0")}`;
}

export const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;

/** The Notes column: plain words, no colour (app.js rowNotes). */
export function rowNotes(c: Cue): string {
  const n: string[] = [];
  if (c.type === "silence") n.push("Stops everything, then waits");
  if (c.type === "fadeout") n.push("Fades out everything running");
  if (c.type === "audio") {
    if (c.loop) n.push("Loop");
    if (c.fade_in) n.push(`Fade in ${c.fade_in} s`);
    if (c.fade_out) n.push(`Fade out ${c.fade_out} s`);
    if (c.pre_wait) n.push(`Wait ${c.pre_wait} s`);
    if (c.trim_start > 0 || c.trim_end != null) n.push("Trimmed");
    if (c.cuts.length) n.push(c.cuts.length === 1 ? "1 cut" : `${c.cuts.length} cuts`);
  }
  if (c.continue === "after_end") n.push("Auto-continue");
  return n.join(", ");
}
