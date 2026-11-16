// Typed wrappers around Tauri commands. Every command rejects with a sentence for the operator.
import { invoke } from "@tauri-apps/api/core";

export type ThemeFamily = "graphite" | "ghost-light" | "booth" | "thorn";
export type ThemeMode = "dark" | "light" | "system";

export interface Settings {
  theme_family: ThemeFamily;
  theme_mode: ThemeMode;
  go_lockout_ms: number;
  stop_fade: number;
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
  setAudioOptions: (goLockoutMs: number, stopFade: number) =>
    invoke<Settings>("set_audio_options", { goLockoutMs, stopFade }),
  appReady: () => invoke<void>("app_ready"),
  listShows: () => invoke<ShowSummary[]>("list_shows"),
  importFile: (path: string) => invoke<ImportResult>("import_file", { path }),
  deleteShow: (id: string) => invoke<void>("delete_show", { id }),
  openShow: (id: string) => invoke<ShowView>("open_show", { id }),
  closeShow: () => invoke<void>("close_show"),
};

// ---- Engine (crates/engine/src/state.rs) ----

export type PrepState =
  | { state: "not_audio" }
  | { state: "pending" }
  | { state: "decoding" }
  | { state: "ready" }
  | { state: "missing" }
  | { state: "failed"; reason: string };

export type CuePrep = PrepState & { length: number | null };

export type RunPhase = "starting" | "waiting" | "running" | "fading";

export interface EngineSnapshot {
  revision: number;
  status: "ok" | "device_lost" | "restarting" | "no_output";
  status_message: string | null;
  paused: boolean;
  standby: number;
  cue_count: number;
  running: { index: number; id: string; phase: RunPhase }[];
  audition: number | null;
  prep: CuePrep[];
  prep_done: number;
  prep_total: number;
  decoded_bytes: number;
  master: number;
  outputs: OutputsState;
}

// ---- Outputs (crates/engine/src/outputs.rs, thorncue-spec#9) ----

export interface OutputDevice {
  id: string;
  name: string;
}

export type ShowOutput = { kind: "system_default" } | ({ kind: "device" } & OutputDevice);
export type AuditionOutput = { kind: "not_chosen" } | { kind: "same_as_show" } | ({ kind: "device" } & OutputDevice);

export type AuditionBlock =
  | { kind: "not_chosen" }
  | { kind: "missing"; detail: string }
  | { kind: "same_device"; detail: string }
  | { kind: "failed"; detail: string };

export type AuditionState =
  | { state: "ready"; name: string; through_show: boolean }
  | { state: "opening"; name: string }
  | { state: "blocked"; reason: AuditionBlock; message: string };

export interface OutputsState {
  show_choice: ShowOutput;
  audition_choice: AuditionOutput;
  show: { device: OutputDevice | null; name: string; fallback: boolean };
  audition: AuditionState;
  audition_volume: number;
  devices: OutputDevice[];
  default_device: string | null;
}

export interface Position {
  index: number;
  id: string;
  elapsed: number;
  remaining: number | null;
  length: number;
  control: boolean;
  /** The trim editor's audition (not a cue in the show); source_time is its playhead. */
  audition: boolean;
  source_time: number | null;
}

export interface Notice {
  text: string;
  error: boolean;
}

export const transport = {
  state: () => invoke<EngineSnapshot>("get_engine_state"),
  openShow: () => invoke<ShowView | null>("get_open_show"),
  go: () => invoke<void>("go"),
  stopAll: (panic: boolean) => invoke<void>("stop_all", { panic }),
  togglePause: () => invoke<void>("toggle_pause"),
  setStandby: (index: number) => invoke<void>("set_standby", { index }),
  moveStandby: (delta: number) => invoke<void>("move_standby", { delta }),
  jump: (number: number) => invoke<void>("jump", { number }),
  restart: () => invoke<void>("restart_engine"),
  setMaster: (volume: number) => invoke<void>("set_master", { volume }),
  setOutputs: (show: ShowOutput, audition: AuditionOutput) => invoke<Settings>("set_outputs", { show, audition }),
  setAuditionVolume: (volume: number) => invoke<void>("set_audition_volume", { volume }),
  refreshDevices: () => invoke<void>("refresh_devices"),
};

// ---- ThornCue Web (src-tauri/src/web_cmds.rs) ----

export interface WebConnection {
  server: string | null;
  insecure: boolean;
  has_token: boolean;
  token_store: "keychain" | "settings_file" | null;
  username: string | null;
}

export type LocalStatus = "not_imported" | "imported" | "update_available" | "changed_both";

export interface WebShow {
  code: string;
  name: string;
  author: string;
  owner: string | null;
  role: string | null;
  cue_count: number;
  audio_bytes: number;
  revision: string;
  updated_at: string;
}

export interface WebShowRow extends WebShow {
  status: LocalStatus;
  local_id: string | null;
  local_count: number;
}

export interface LinkInfo extends WebShow {
  server: string;
  insecure: boolean;
  status: LocalStatus;
  local_id: string | null;
}

export const webApi = {
  connection: () => invoke<WebConnection>("web_connection"),
  connect: (server: string, token: string) => invoke<WebConnection>("web_connect", { server, token }),
  check: () => invoke<WebConnection>("web_check"),
  disconnect: () => invoke<WebConnection>("web_disconnect"),
  list: () => invoke<WebShowRow[]>("web_list"),
  linkInfo: (link: string) => invoke<LinkInfo>("web_link_info", { link }),
  importShow: (code: string, replaceId: string | null) => invoke<ImportResult>("web_import", { code, replaceId }),
  importLink: (link: string, replaceId: string | null) => invoke<ImportResult>("web_import_link", { link, replaceId }),
};

// ---- Editing (src-tauri/src/edit_cmds.rs) ----

export interface EditResult {
  show: ShowView;
  focus: string | null;
  problems: string[];
}

export type CuePatch = Partial<Pick<Cue, "name" | "volume" | "fade_in" | "fade_out" | "pre_wait" | "duration" | "loop" | "continue" | "trim_start" | "trim_end" | "cuts">>;

export const editApi = {
  updateCue: (id: string, patch: CuePatch) => invoke<EditResult>("update_cue", { id, patch }),
  moveCue: (id: string, to: number) => invoke<EditResult>("move_cue", { id, to }),
  addControlCue: (silence: boolean) => invoke<EditResult>("add_control_cue", { silence }),
  duplicateCue: (id: string) => invoke<EditResult>("duplicate_cue", { id }),
  deleteCue: (id: string) => invoke<EditResult>("delete_cue", { id }),
  setShowName: (name: string) => invoke<EditResult>("set_show_name", { name }),
  addAudioCues: (paths: string[]) => invoke<EditResult>("add_audio_cues", { paths }),
  replaceCueFile: (id: string, path: string) => invoke<EditResult>("replace_cue_file", { id, path }),
  newShow: () => invoke<string>("new_show"),
  duplicateShow: (id: string) => invoke<string>("duplicate_show", { id }),
  exportFileName: (id: string) => invoke<string>("export_file_name", { id }),
  exportShow: (id: string, path: string) => invoke<void>("export_show", { id, path }),
};

export const AUDIO_EXTENSIONS = ["mp3", "wav", "ogg", "flac", "m4a", "aiff", "aif"];

// ---- Trim editor (M7) ----

export interface Peaks {
  duration: number;
  min: number[];
  max: number[];
}

export const trimApi = {
  peaks: (id: string) => invoke<Peaks>("get_peaks", { id }),
  audition: (id: string, from: number) => invoke<void>("audition", { id, from }),
  stopAudition: () => invoke<void>("stop_audition"),
  /** The gain playback applies in each of `columns` slices of the file (null: not played). */
  fadeCurve: (shape: FadeShape, duration: number, columns: number) =>
    invoke<(number | null)[]>("fade_curve", { shape, duration, columns }),
};

export interface FadeShape {
  trim_start: number;
  trim_end: number | null;
  cuts: [number, number][];
  fade_in: number;
  fade_out: number;
  loop: boolean;
}
