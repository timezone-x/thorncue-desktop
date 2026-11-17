// Cue groups (thorncue-spec#2), as thorncue-web static/app.js "Groups". A group is a label
// around consecutive cues; it never changes playback. Rust (editing.rs) applies and checks
// every change; this file only holds the presets and the drawing helpers.

/** Presets, as the web's GROUP_COLORS: mid-tones that read on dark and light themes
 * (design/DESIGN.md, "User-chosen colours on cue groups"). These are user label colours,
 * not theme colours, which is why they aren't tokens. */
export const GROUP_COLORS: [string, string][] = [
  ["Blue", "#3e7bd6"],
  ["Teal", "#1f9e9a"],
  ["Green", "#4a9a3c"],
  ["Amber", "#c98a14"],
  ["Orange", "#d9622b"],
  ["Red", "#d0473d"],
  ["Pink", "#c2449a"],
  ["Purple", "#8a5cd6"],
  ["Grey", "#8a8f98"],
];

export const COLOR_RE = /^#[0-9a-f]{6}$/i;

export const safeColor = (c: string) => (COLOR_RE.test(c) ? c.toLowerCase() : GROUP_COLORS[0][1]);
