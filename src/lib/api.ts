// Typed wrappers around Tauri commands. Every command rejects with a sentence for the operator.
import { invoke } from "@tauri-apps/api/core";

export type ThemeFamily = "graphite" | "ghost-light" | "booth" | "thorn";
export type ThemeMode = "dark" | "light" | "system";

export interface Settings {
  theme_family: ThemeFamily;
  theme_mode: ThemeMode;
}

export interface ShowSummary {
  id: string;
  name: string;
  cue_count: number;
  audio_bytes: number;
  source: { server: string; code: string } | null;
  last_opened: string | null;
  imported_at: string | null;
  damaged: boolean;
}

export interface ImportResult {
  id: string;
  name: string;
  cue_count: number;
  warnings: string[];
}

export interface ImportProgress {
  path: string;
  done: number;
  total: number;
}

export type CueType = "audio" | "silence" | "fadeout";

/** A cue as in manifest.json (FORMAT.md 2.4), already normalised by Rust. */
export interface Cue {
  id: string;
  type: CueType;
  name: string;
  asset: string | null;
  volume: number;
  fade_in: number;
  fade_out: number;
  pre_wait: number;
  duration: number;
  loop: boolean;
  continue: "none" | "after_end";
  trim_start: number;
  trim_end: number | null;
  cuts: [number, number][];
}

export interface LocalState {
  last_opened: string | null;
  standby_index: number;
  imported_at: string | null;
  imported_revision: string | null;
  modified_since_import: boolean;
}

export interface ShowView {
  id: string;
  name: string;
  author: string;
  notes: string;
  source: { kind: string; server: string; code: string; revision: string } | null;
  cues: Cue[];
  local: LocalState;
  recovered: string | null;
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
  listShows: () => invoke<ShowSummary[]>("list_shows"),
  importFile: (path: string) => invoke<ImportResult>("import_file", { path }),
  deleteShow: (id: string) => invoke<void>("delete_show", { id }),
  openShow: (id: string) => invoke<ShowView>("open_show", { id }),
  closeShow: () => invoke<void>("close_show"),
};
