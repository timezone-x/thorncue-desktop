<script lang="ts">
  // The inspector for one cue (reference/web/show.html "Inspector", app.js renderInspector and
  // the bind* handlers). Every change goes straight to Rust, which validates, autosaves and
  // tells the engine; a playing cue's volume follows the slider live.
  // Hide and height are a per-viewer convenience, remembered in localStorage.
  import type { Cue, CuePatch, Group } from "../lib/api";
  import { syncValue } from "../lib/sync";
  import Trim from "./Trim.svelte";

  let {
    cue,
    index,
    onEdit,
    onDuplicate,
    onReplace,
    onDelete,
    groups = [],
    onSetGroup,
    exportCount = 1,
    onExport,
    onExportWav,
    hidden = $bindable(),
  }: {
    cue: Cue | null;
    index: number;
    onEdit: (id: string, patch: CuePatch) => void;
    onDuplicate: (id: string) => void;
    onReplace: (id: string) => void;
    onDelete: (cue: Cue, index: number) => void;
    groups?: Group[];
    /** The Group field (thorncue-spec#2): join puts the cue at the group's end. */
    onSetGroup?: (id: string, group: string | null) => void;
    /** Export cue (.tqc): the shift-selected range when there is one, else this cue. */
    exportCount?: number;
    onExport?: () => void;
    /** Export WAV (web inspector): this cue's audio as it plays. Resolves when done or cancelled. */
    onExportWav?: (id: string) => Promise<void>;
    /** Bound by the Show screen for its header Inspector button (thorncue-spec#6). */
    hidden?: boolean;
  } = $props();

  const KEY = "thorncue.inspector";
  function load(): { hidden: boolean; height: number | null } {
    try {
      const v = JSON.parse(localStorage.getItem(KEY) ?? "null");
      if (v && typeof v.hidden === "boolean") return { hidden: v.hidden, height: typeof v.height === "number" ? v.height : null };
    } catch {
      /* storage unavailable or damaged: defaults */
    }
    return { hidden: false, height: null };
  }
  const saved = load();
  hidden = saved.hidden;
  let height = $state<number | null>(saved.height);
  let resizing = $state(false);

  function remember() {
    try {
      localStorage.setItem(KEY, JSON.stringify({ hidden, height }));
    } catch {
      /* not remembered this time */
    }
  }

  /** The one show/hide: the bar's chevron, the header's Inspector button and `\` all use it. */
  export function toggle() {
    hidden = !hidden;
    remember();
  }

  function startResize(e: PointerEvent) {
    if (hidden) return;
    const grip = e.currentTarget as HTMLElement;
    grip.setPointerCapture(e.pointerId);
    resizing = true;
    const startY = e.clientY;
    const panel = grip.parentElement!.nextElementSibling as HTMLElement;
    const startH = panel.getBoundingClientRect().height;
    const move = (ev: PointerEvent) => {
      height = Math.max(120, Math.min(window.innerHeight * 0.75, startH - (ev.clientY - startY)));
    };
    const up = () => {
      resizing = false;
      grip.removeEventListener("pointermove", move);
      grip.removeEventListener("pointerup", up);
      remember();
    };
    grip.addEventListener("pointermove", move);
    grip.addEventListener("pointerup", up);
  }

  function resetHeight() {
    height = null;
    remember();
  }

  const audio = $derived(cue?.type === "audio");
  let exportingWav = $state(false);
  async function exportWav(id: string) {
    if (!onExportWav || exportingWav) return;
    exportingWav = true;
    try {
      await onExportWav(id);
    } finally {
      exportingWav = false;
    }
  }
  let trim = $state<ReturnType<typeof Trim>>();
  /** A fade handle being dragged on the waveform: the fields follow it live. */
  let draftFades = $state<{ fade_in: number; fade_out: number } | null>(null);

  /** I, O, X and Ctrl+Z from the Show screen's keyboard handler. */
  export function trimKey(key: "in" | "out" | "cut" | "undo") {
    if (hidden || !trim) return;
    if (key === "in") trim.setIn();
    else if (key === "out") trim.setOut();
    else if (key === "cut") trim.cutSelection();
    else trim.undo();
  }

  // Numbers commit on change (as the web's bindNumber): blank or negative is refused.
  function number(field: "fade_in" | "fade_out" | "pre_wait" | "duration", el: HTMLInputElement, max: number) {
    if (!cue) return;
    const v = Number.parseFloat(el.value);
    if (!Number.isFinite(v) || v < 0) {
      el.value = String(cue[field]);
      return;
    }
    onEdit(cue.id, { [field]: Math.round(Math.min(v, max) * 1e4) / 1e4 });
  }

  function volume(pct: number) {
    if (!cue || !Number.isFinite(pct)) return;
    onEdit(cue.id, { volume: Math.max(0, Math.min(200, Math.round(pct))) / 100 });
  }

  // Every keystroke goes to Rust at once (Rust debounces the disk write), so quitting right
  // after typing loses nothing: a UI-side delay here once dropped the last rename on quit.
  function name(el: HTMLInputElement) {
    if (!cue) return;
    onEdit(cue.id, { name: el.value.slice(0, 100) });
  }
</script>

<div class="inspector-bar" class:is-resizing={resizing}>
  <div class="inspector-bar__grip" role="separator" aria-orientation="horizontal" aria-label="Resize inspector"
    title="Drag to resize the inspector (double-click to reset)" onpointerdown={startResize} ondblclick={resetHeight}></div>
  <button type="button" class="inspector-bar__toggle" aria-controls="inspector" aria-expanded={!hidden}
    title={hidden ? "Show inspector" : "Hide inspector"} aria-label={hidden ? "Show inspector" : "Hide inspector"} onclick={toggle}>
    <svg viewBox="0 0 12 12" width="12" height="12" aria-hidden="true"><path d="M2 4.5 6 8.5 10 4.5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/></svg>
  </button>
</div>

<section class="inspector" id="inspector" aria-labelledby="insp-title" hidden={hidden}
  class:is-sized={height !== null} style:height={height !== null ? `${height}px` : undefined}>
  <div class="inspector__head">
    <h2 class="inspector__title" id="insp-title">{cue ? `Q${index + 1} ${cue.name}` : "Inspector"}</h2>
    {#if cue}
      <button type="button" class="btn" onclick={() => onDuplicate(cue.id)}>Duplicate</button>
      {#if audio && onExportWav}
        <button type="button" class="btn" title="Save the audio as it plays: trims, cuts, volume and fades applied"
          disabled={exportingWav} onclick={() => exportWav(cue.id)}>{exportingWav ? "Exporting…" : "Export WAV"}</button>
      {/if}
      {#if onExport}
        <button type="button" class="btn" title="Save as a .tqc file, audio and edits included, to import into another show"
          onclick={onExport}>{exportCount > 1 ? `Export ${exportCount} cues` : "Export cue"}</button>
      {/if}
      {#if audio}<button type="button" class="btn" onclick={() => onReplace(cue.id)}>Replace file</button>{/if}
      <button type="button" class="btn btn--danger" onclick={() => onDelete(cue, index)}>Delete</button>
    {/if}
  </div>
  {#if !cue}
    <p class="inspector__empty">Select a cue to see its settings.</p>
  {:else}
    {#key cue.id}
      <div class="inspector__body">
        <div class="inspector__fields">
          <div class="field">
            <label for="f-name">Name</label>
            <input type="text" id="f-name" maxlength="100" autocomplete="off" use:syncValue={cue.name}
              oninput={(e) => name(e.currentTarget)} />
          </div>
          {#if groups.length}
            <div class="field">
              <label for="f-group">Group</label>
              <select id="f-group" value={cue.group ?? ""}
                onchange={(e) => onSetGroup?.(cue.id, e.currentTarget.value || null)}>
                <option value="">No group</option>
                {#each groups as g (g.id)}<option value={g.id}>{g.name}</option>{/each}
              </select>
            </div>
          {/if}
          {#if audio}
            <div class="field">
              <label for="f-vol">Volume (%)</label>
              <div class="vol-row">
                <input type="range" min="0" max="200" step="1" aria-label="Volume" value={Math.round(cue.volume * 100)}
                  oninput={(e) => volume(Number.parseFloat(e.currentTarget.value))} />
                <input type="number" id="f-vol" min="0" max="200" step="1" value={Math.round(cue.volume * 100)}
                  onchange={(e) => volume(Number.parseFloat(e.currentTarget.value))} />
              </div>
            </div>
            <div class="field-row">
              <div class="field">
                <label for="f-fadein">Fade in (s)</label>
                <input type="number" id="f-fadein" min="0" max="600" step="0.1" value={draftFades?.fade_in ?? cue.fade_in}
                  onchange={(e) => number("fade_in", e.currentTarget, 600)} />
              </div>
              <div class="field">
                <label for="f-fadeout">Fade out (s)</label>
                <input type="number" id="f-fadeout" min="0" max="600" step="0.1" value={draftFades?.fade_out ?? cue.fade_out}
                  onchange={(e) => number("fade_out", e.currentTarget, 600)} />
              </div>
            </div>
            <div class="field">
              <label for="f-prewait">Pre-wait (s)</label>
              <input type="number" id="f-prewait" min="0" max="3600" step="0.1" value={cue.pre_wait}
                aria-describedby="f-prewait-hint" onchange={(e) => number("pre_wait", e.currentTarget, 3600)} />
              <p class="hint" id="f-prewait-hint">Delay between GO and the sound starting.</p>
            </div>
          {:else}
            <div class="field">
              <label for="f-duration">{cue.type === "silence" ? "Wait (s)" : "Fade time (s)"}</label>
              <input type="number" id="f-duration" min="0" max="3600" step="0.1" value={cue.duration}
                aria-describedby="f-duration-hint" onchange={(e) => number("duration", e.currentTarget, 3600)} />
              <p class="hint" id="f-duration-hint">
                {cue.type === "silence"
                  ? "Stops everything, then waits this long. Useful before an auto-continue."
                  : "Everything running fades to silence over this time."}
              </p>
            </div>
          {/if}
          <div class="checks">
            {#if audio}
              <label class="check"><input type="checkbox" checked={cue.loop}
                onchange={(e) => onEdit(cue.id, { loop: e.currentTarget.checked })} /> Loop</label>
            {/if}
            <label class="check"><input type="checkbox" checked={cue.continue === "after_end"}
              onchange={(e) => onEdit(cue.id, { continue: e.currentTarget.checked ? "after_end" : "none" })} />
              Auto-continue to the next cue when this one ends</label>
          </div>
        </div>
        {#if audio && cue.asset}
          <Trim bind:this={trim} bind:draftFades {cue} {index} {onEdit} />
        {/if}
      </div>
    {/key}
  {/if}
</section>
