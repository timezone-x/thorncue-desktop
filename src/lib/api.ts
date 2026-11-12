// Typed wrappers around Tauri commands. Every command returns a user-facing message on error.
import { invoke } from "@tauri-apps/api/core";

export type ThemeFamily = "graphite" | "ghost-light" | "booth" | "thorn";
export type ThemeMode = "dark" | "light" | "system";

export interface Settings {
  theme_family: ThemeFamily;
  theme_mode: ThemeMode;
}

export const isTauri = () => "__TAURI_INTERNALS__" in window;

export function errorText(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return "Something went wrong.";
}

export const api = {
  getSettings: () => invoke<Settings>("get_settings"),
  setTheme: (family: ThemeFamily, mode: ThemeMode) => invoke<Settings>("set_theme", { family, mode }),
  appReady: () => invoke<void>("app_ready"),
};
