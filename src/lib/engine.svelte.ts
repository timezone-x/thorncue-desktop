// The engine's state as the UI sees it. Rust owns playback; this only mirrors what it reports
// (engine://state, engine://positions) and sends intents. A reloaded page asks for a snapshot
// first, so it redraws whatever is playing without waiting for the next event.
//
// The mirror heals itself: if positions mention a cue the snapshot doesn't show as running
// (seen once after a reload during testing, not reproduced), or the window regains focus,
// it asks Rust for a fresh snapshot. Rust is always right.
import { listen } from "@tauri-apps/api/event";
import { errorText, transport, type EngineSnapshot, type Notice, type Position } from "./api";
import { showToast } from "./toast.svelte";

export const engine = $state<{ snap: EngineSnapshot | null; positions: Position[] }>({
  snap: null,
  positions: [],
});

let started = false;
let resyncing = false;

const isSnapshot = (s: unknown): s is EngineSnapshot =>
  typeof s === "object" && s !== null && typeof (s as EngineSnapshot).revision === "number" &&
  Array.isArray((s as EngineSnapshot).running) && Array.isArray((s as EngineSnapshot).prep);

export async function startEngineMirror() {
  if (started) return;
  started = true;
  await listen<EngineSnapshot>("engine://state", (e) => accept(e.payload));
  await listen<Position[]>("engine://positions", (e) => {
    if (!Array.isArray(e.payload)) return;
    engine.positions = e.payload;
    const snap = engine.snap;
    if (snap && e.payload.some((p) => !p.audition && !snap.running.some((r) => r.index === p.index))) void resync();
  });
  await listen<Notice>("engine://notice", (e) => showToast(e.payload.text, e.payload.error));
  window.addEventListener("focus", () => void resync());
  await resync();
  setTimeout(() => void resync(), 500);
}

/** Replace whatever we have with Rust's current snapshot. */
export async function resync() {
  if (resyncing) return;
  resyncing = true;
  try {
    const s = await transport.state();
    if (isSnapshot(s)) {
      engine.snap = s;
      if (s.running.length === 0 && s.audition === null) engine.positions = [];
    }
  } catch {
    /* the next event or resync will do */
  } finally {
    resyncing = false;
  }
}

function accept(s: EngineSnapshot) {
  if (!isSnapshot(s)) return;
  if (engine.snap && s.revision < engine.snap.revision && s.status === engine.snap.status) return;
  engine.snap = s;
  if (s.running.length === 0 && s.audition === null) engine.positions = [];
}

/** Send an intent; a refusal (e.g. "restarting") becomes a toast. Never throws. */
export async function act(f: () => Promise<void>) {
  try {
    await f();
  } catch (e) {
    showToast(errorText(e), true);
  }
}
