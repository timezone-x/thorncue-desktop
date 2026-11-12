// Theme: family + mode, as the web. Rust stores it in settings.json; the injected
// init script applied it before first paint. This module keeps it applied, follows
// the OS live in System mode, and caches it so a webview reload paints correctly.
// Changing the theme only touches data-theme on <html>: never playback, never focus.
import { api, type ThemeFamily, type ThemeMode } from "./api";

export const FAMILIES: { id: ThemeFamily; name: string; lightName: string; why: string }[] = [
  { id: "graphite", name: "Graphite", lightName: "Graphite light", why: "Neutral grey, like desk hardware." },
  { id: "ghost-light", name: "Ghost Light", lightName: "Work Light", why: "Warm neutrals and amber, like the lamp on an empty stage." },
  { id: "booth", name: "Booth", lightName: "Booth light", why: "Cool blue-grey, like a lit console." },
  { id: "thorn", name: "Thorn", lightName: "Thorn light", why: "Plum neutrals and violet, ThornCue's own colour." },
];

type Theme = { family: ThemeFamily; mode: ThemeMode };

declare global {
  interface Window {
    __THORNCUE_THEME__?: Theme;
  }
}

const media = window.matchMedia("(prefers-color-scheme: light)");

export const theme = $state<Theme>(window.__THORNCUE_THEME__ ?? { family: "graphite", mode: "dark" });

export function themeId(t: Theme = theme): string {
  const mode = t.mode === "system" ? (media.matches ? "light" : "dark") : t.mode;
  return `${t.family}-${mode}`;
}

function apply() {
  document.documentElement.setAttribute("data-theme", themeId());
  try {
    localStorage.setItem("thorncue.theme", JSON.stringify({ family: theme.family, mode: theme.mode }));
  } catch {
    /* storage unavailable: the init script falls back to settings.json */
  }
}

media.addEventListener("change", () => {
  if (theme.mode === "system") apply();
});

/** Pull the stored theme from Rust (the source of truth) and apply it. */
export async function loadTheme() {
  const s = await api.getSettings();
  theme.family = s.theme_family;
  theme.mode = s.theme_mode;
  apply();
}

export async function setTheme(family: ThemeFamily, mode: ThemeMode) {
  theme.family = family;
  theme.mode = mode;
  apply();
  await api.setTheme(family, mode);
}
