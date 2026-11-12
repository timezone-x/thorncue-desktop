// File import: one file at a time, progress from Rust's import://progress events.
import { listen } from "@tauri-apps/api/event";
import { api, errorText, type ImportProgress, type ImportResult } from "./api";

export type ImportState =
  | { phase: "idle" }
  | { phase: "working"; file: string; done: number; total: number }
  | { phase: "done"; file: string; result: ImportResult }
  | { phase: "failed"; file: string; error: string };

export const importer = $state<{ current: ImportState; queue: string[] }>({
  current: { phase: "idle" },
  queue: [],
});

const baseName = (p: string) => p.split(/[\\/]/).pop() ?? p;

let listening = false;
let onImported: (r: ImportResult) => void = () => {};

/** Called once by the Library screen: what to do after each successful import. */
export async function initImporter(after: (r: ImportResult) => void) {
  onImported = after;
  if (listening) return;
  listening = true;
  await listen<ImportProgress>("import://progress", (e) => {
    const c = importer.current;
    if (c.phase === "working") {
      c.done = e.payload.done;
      c.total = e.payload.total;
    }
  });
}

export function importFiles(paths: string[]) {
  const wanted = paths.filter((p) => /\.(tqs|zip)$/i.test(p));
  const ignored = paths.length - wanted.length;
  importer.queue.push(...wanted);
  if (importer.current.phase !== "working") void next();
  return ignored;
}

async function next() {
  const path = importer.queue.shift();
  if (!path) return;
  const file = baseName(path);
  importer.current = { phase: "working", file, done: 0, total: 0 };
  try {
    const result = await api.importFile(path);
    importer.current = { phase: "done", file, result };
    onImported(result);
  } catch (e) {
    importer.current = { phase: "failed", file, error: errorText(e) };
  }
}

/** Close the result; carry on with the next queued file, if any. */
export function dismissImport() {
  importer.current = { phase: "idle" };
  void next();
}
