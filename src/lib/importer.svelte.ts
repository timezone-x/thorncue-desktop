// Imports run one at a time: files, web shows and share links share one queue and one
// progress dialog. Progress comes from Rust's import://progress events, matched by `key`.
import { listen } from "@tauri-apps/api/event";
import { api, errorText, type ImportProgress, type ImportResult } from "./api";

interface Job {
  key: string;
  label: string;
  run: () => Promise<ImportResult>;
}

export type ImportState =
  | { phase: "idle" }
  | { phase: "working"; file: string; key: string; done: number; total: number }
  | { phase: "done"; file: string; result: ImportResult }
  | { phase: "failed"; file: string; error: string };

export const importer = $state<{ current: ImportState; queue: Job[] }>({
  current: { phase: "idle" },
  queue: [],
});

const baseName = (p: string) => p.split(/[\\/]/).pop() ?? p;

let listening = false;
let onImported: (r: ImportResult) => void = () => {};

/** What to do after each successful import (the Library refreshes itself). */
export async function initImporter(after: (r: ImportResult) => void) {
  onImported = after;
  if (listening) return;
  listening = true;
  await listen<ImportProgress>("import://progress", (e) => {
    const c = importer.current;
    if (c.phase === "working" && c.key === e.payload.path) {
      c.done = e.payload.done;
      c.total = e.payload.total;
    }
  });
}

function enqueue(job: Job) {
  importer.queue.push(job);
  if (importer.current.phase !== "working") void next();
}

/** Files dropped or picked: returns how many were ignored (not .tqs or .zip). */
export function importFiles(paths: string[]) {
  const wanted = paths.filter((p) => /\.(tqs|zip)$/i.test(p));
  for (const path of wanted) enqueue({ key: path, label: baseName(path), run: () => api.importFile(path) });
  return paths.length - wanted.length;
}

/** A web import: `key` must match the progress events Rust sends ("web:<code>"). */
export function importJob(key: string, label: string, run: () => Promise<ImportResult>) {
  enqueue({ key, label, run });
}

async function next() {
  const job = importer.queue.shift();
  if (!job) return;
  importer.current = { phase: "working", file: job.label, key: job.key, done: 0, total: 0 };
  try {
    const result = await job.run();
    importer.current = { phase: "done", file: job.label, result };
    onImported(result);
  } catch (e) {
    importer.current = { phase: "failed", file: job.label, error: errorText(e) };
  }
}

/** Close the result; carry on with the next queued import, if any. */
export function dismissImport() {
  importer.current = { phase: "idle" };
  void next();
}
