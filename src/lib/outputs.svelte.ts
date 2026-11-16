// The outputs bar's shared state (thorncue-spec#9): whether it is folded, and what an
// Audition button should say when auditions are blocked.
import type { AuditionState } from "./api";
import { engine } from "./engine.svelte";
import { showToast } from "./toast.svelte";

const KEY = "thorncue.outputs.folded";

function loadFolded(): boolean {
  try {
    return localStorage.getItem(KEY) === "1";
  } catch {
    return false;
  }
}

export const outputsUi = $state({ folded: loadFolded() });

export function setFolded(folded: boolean) {
  outputsUi.folded = folded;
  try {
    localStorage.setItem(KEY, folded ? "1" : "0");
  } catch {
    /* per-viewer convenience only */
  }
}

export function auditionState(): AuditionState | null {
  return engine.snap?.outputs?.audition ?? null;
}

/** The reason auditions can't play now, or null when they can. */
export function auditionBlocked(): string | null {
  const a = auditionState();
  if (!a) return null;
  if (a.state === "blocked") return a.message;
  if (a.state === "opening") return `The audition output "${a.name}" is still opening.`;
  return null;
}

/**
 * Call before starting an audition. When blocked: say why (toast), unfold the bar so the
 * operator can fix it, and return false. The button still responds; it just does nothing else.
 */
export function checkAudition(): boolean {
  const why = auditionBlocked();
  if (!why) return true;
  showToast(why, true);
  setFolded(false);
  return false;
}
