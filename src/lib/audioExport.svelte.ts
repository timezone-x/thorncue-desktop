// The audio export that's running (one at a time, in Rust): its progress for the Export audio
// dialog and for the inspector's range export. src-tauri/src/export_cmds.rs does the work.
import { listen } from "@tauri-apps/api/event";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import { exportApi, type AudioExportDone, type AudioExportOptions, type AudioExportProgress } from "./api";

export const audioJob = $state<{ running: boolean; progress: AudioExportProgress | null }>({ running: false, progress: null });

let listening: Promise<unknown> | null = null;

/** Ask where to save the zip, then build it. "cancelled" if the save dialog was closed, null if
 * Stop was pressed. Throws the error sentence if it failed. */
export async function runAudioExport(show: string | null, options: AudioExportOptions): Promise<AudioExportDone | null | "cancelled"> {
  if (audioJob.running) throw "An audio export is already running.";
  listening ??= listen<AudioExportProgress>("audio-export://progress", (e) => (audioJob.progress = e.payload));
  await listening;
  const name = await exportApi.fileName(show, options);
  const path = await saveDialog({ title: "Export audio", defaultPath: name, filters: [{ name: "Zip", extensions: ["zip"] }] });
  if (!path) return "cancelled";
  audioJob.running = true;
  audioJob.progress = null;
  try {
    return await exportApi.start(show, options, path);
  } finally {
    audioJob.running = false;
  }
}

/** "Rendering 7 of 24", "Copying 3 of 5", "Writing the zip…". */
export function progressText(p: AudioExportProgress | null): string {
  if (!p || !p.total) return "Starting…";
  if (p.phase === "zipping") return "Writing the zip…";
  return `${p.phase === "copying" ? "Copying" : "Rendering"} ${Math.min(p.done + 1, p.total)} of ${p.total}`;
}

/** "about 180 MB" (web fmtBytes: decimal units, as the size a file manager shows). */
export function aboutBytes(n: number): string {
  if (n < 1e6) return `${Math.max(1, Math.round(n / 1e3))} KB`;
  if (n < 1e9) return `${n < 1e7 ? (n / 1e6).toFixed(1) : Math.round(n / 1e6)} MB`;
  return `${(n / 1e9).toFixed(1)} GB`;
}
