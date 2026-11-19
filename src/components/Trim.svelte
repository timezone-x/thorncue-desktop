<script lang="ts" module>
  // Peaks per audio file, for this session (they don't change while the file doesn't).
  import type { Peaks } from "../lib/api";
  const peakCache = new Map<string, Peaks>();
  // Undo stacks per cue id, for this session (as the web).
  const undoStacks = new Map<string, import("../lib/edits").EditFields[]>();
</script>

<script lang="ts">
  // The trim and cut editor (reference/web/app.js L1149-1464): waveform, in/out markers, select
  // and cut, put back, undo, restore, audition from marker. Peaks come from Rust (both
  // channels, W9); edits go to Rust like any other cue edit and are committed when a drag ends.
  import { onMount } from "svelte";
  import { errorText, trimApi, type Cue, type CuePatch } from "../lib/api";
  import { engine } from "../lib/engine.svelte";
  import {
    MIN_LEN,
    clamp,
    keptSegments,
    mergeCuts,
    outPoint,
    r4,
    segmentsLength,
    toPlayedTime,
    toSourceTime,
    type EditFields,
  } from "../lib/edits";
  import { cueNumber, fmtTime } from "../lib/format";
  import { showToast } from "../lib/toast.svelte";
  import { auditionBlocked, checkAudition } from "../lib/outputs.svelte";

  let {
    cue,
    index,
    onEdit,
    draftFades = $bindable(null),
  }: {
    cue: Cue;
    index: number;
    onEdit: (id: string, patch: CuePatch) => void;
    /** While a fade handle is dragged: the fades as they will be, for the inspector's fields. */
    draftFades?: { fade_in: number; fade_out: number } | null;
  } = $props();

  let canvas: HTMLCanvasElement;
  let peaks = $state<Peaks | null>(null);
  let loadError = $state("");
  let marker = $state<number | null>(null);
  let sel = $state<[number, number] | null>(null);
  /** While a marker is being dragged: the edit as it will be when released. */
  let draft = $state<EditFields | null>(null);
  let undoCount = $state(0);
  let colour = $state<Record<string, string>>({});

  const edits = $derived<EditFields>(
    draft ?? { trim_start: cue.trim_start, trim_end: cue.trim_end, cuts: cue.cuts, fade_in: cue.fade_in, fade_out: cue.fade_out },
  );
  const looping = $derived(cue.loop);
  const dur = $derived(peaks?.duration ?? 0);
  // Primitive deriveds: every edit hands us a new `cue` object, and effects that read these
  // only re-run when the cue or its file actually changes (not after each edit).
  const assetKey = $derived(cue.asset ?? "");
  const cueId = $derived(cue.id);

  const auditionPos = $derived(engine.positions.find((p) => p.audition && p.index === index) ?? null);
  const auditioning = $derived(engine.snap?.audition === index);
  const playhead = $derived(auditionPos?.source_time ?? null);

  // ---- Loading peaks ----

  $effect(() => {
    const key = assetKey;
    const id = cueId;
    marker = null;
    sel = null;
    undoCount = undoStacks.get(id)?.length ?? 0;
    if (!key) return;
    const hit = peakCache.get(key);
    if (hit) {
      peaks = hit;
      loadError = "";
      return;
    }
    peaks = null;
    loadError = "";
    trimApi.peaks(id).then(
      (p) => {
        peakCache.set(key, p);
        if (assetKey === key) peaks = p;
      },
      (e) => {
        if (assetKey === key) loadError = errorText(e);
      },
    );
  });

  // ---- The gain line (thorncue-spec#5): computed in Rust by playback's own envelope ----

  let curve = $state<(number | null)[] | null>(null);
  let curveSeq = 0;
  let canvasW = $state(0);

  $effect(() => {
    const shape = {
      trim_start: edits.trim_start,
      trim_end: edits.trim_end,
      cuts: edits.cuts,
      fade_in: edits.fade_in,
      fade_out: edits.fade_out,
      loop: looping,
    };
    const d = dur;
    const w = canvasW;
    if (!peaks || !d || !w) return;
    const seq = ++curveSeq;
    trimApi.fadeCurve(shape, d, w).then(
      (c) => {
        if (seq === curveSeq) curve = c;
      },
      () => {},
    );
  });

  // ---- Drawing ----

  function readColours() {
    const css = getComputedStyle(document.documentElement);
    const v = (n: string) => css.getPropertyValue(n).trim();
    colour = {
      bg: v("--bg"),
      wave: v("--wave"),
      waveOff: v("--wave-off"),
      marker: v("--select-line"),
      selection: v("--select-wash"),
      playhead: v("--text"),
    };
  }

  onMount(() => {
    readColours();
    // The canvas reads its colours from the theme; repaint when the theme changes.
    const themeWatch = new MutationObserver(readColours);
    themeWatch.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    const sizeWatch = new ResizeObserver(() => {
      canvasW = Math.max(1, Math.round(canvas.clientWidth * (window.devicePixelRatio || 1)));
      draw();
    });
    sizeWatch.observe(canvas);
    return () => {
      themeWatch.disconnect();
      sizeWatch.disconnect();
    };
  });

  $effect(() => {
    // Redraw whenever anything it shows changes.
    void [peaks, edits, marker, sel, playhead, colour, loadError, curve];
    draw();
  });

  function draw() {
    if (!canvas) return;
    const dpr = window.devicePixelRatio || 1;
    const w = Math.max(1, Math.round(canvas.clientWidth * dpr));
    const h = Math.max(1, Math.round(canvas.clientHeight * dpr));
    if (canvas.width !== w || canvas.height !== h) {
      canvas.width = w;
      canvas.height = h;
    }
    const g = canvas.getContext("2d");
    if (!g) return;
    g.fillStyle = colour.bg;
    g.fillRect(0, 0, w, h);
    if (!peaks) {
      g.fillStyle = colour.wave;
      g.font = `${13 * dpr}px system-ui, sans-serif`;
      g.fillText(loadError ? `Can't show the waveform: ${loadError}` : "Loading audio…", 8 * dpr, 20 * dpr);
      return;
    }
    const segs = keptSegments(edits, dur);
    const kept = (t: number) => segs.some(([s, e]) => t >= s && t <= e);
    const cols = peaks.max.length;
    const mid = h / 2;
    for (let x = 0; x < w; x++) {
      // Each pixel column takes the extremes of the peak columns it covers.
      const a = Math.floor((x / w) * cols);
      const b = Math.max(a + 1, Math.floor(((x + 1) / w) * cols));
      let lo = 0;
      let hi = 0;
      for (let i = a; i < b && i < cols; i++) {
        lo = Math.min(lo, peaks.min[i]);
        hi = Math.max(hi, peaks.max[i]);
      }
      g.fillStyle = kept(((x + 0.5) / w) * dur) ? colour.wave : colour.waveOff;
      const top = mid - Math.max(0.5, -lo * mid * 0.95);
      const bot = mid + Math.max(0.5, hi * mid * 0.95);
      g.fillRect(x, top, 1, bot - top);
    }
    const X = (t: number) => Math.round((t / dur) * w);
    drawFades(g, segs, w, h, dpr);
    if (sel) {
      g.fillStyle = colour.selection;
      g.fillRect(X(sel[0]), 0, Math.max(1, X(sel[1]) - X(sel[0])), h);
    }
    // In and out markers, with a tab at the top to grab.
    g.fillStyle = colour.marker;
    for (const [t, dir] of [
      [edits.trim_start, 1],
      [outPoint(edits, dur), -1],
    ] as const) {
      const x = clamp(X(t), 0, w - 2 * dpr);
      g.fillRect(x, 0, 2 * dpr, h);
      g.fillRect(dir > 0 ? x : x - 8 * dpr + 2 * dpr, 0, 8 * dpr, 10 * dpr);
    }
    if (marker != null) {
      g.fillStyle = colour.playhead;
      for (let y = 0; y < h; y += 6 * dpr) g.fillRect(X(marker), y, 1 * dpr, 3 * dpr);
    }
    if (playhead != null) {
      g.fillStyle = colour.playhead;
      g.fillRect(X(playhead), 0, 2 * dpr, h);
    }
    drawFadeHandles(g, segs, w, dpr);
  }

  // Fades on the waveform (as the web): the audio a fade turns down is dimmed, the gain is a
  // line, and each fade has a round handle just under its marker's tab, dragged inwards.
  const FADE_HANDLE_Y = 18; // CSS px from the top
  const FADE_HANDLE_R = 5;

  function fadeHandleTimes(segs: [number, number][]) {
    const len = segmentsLength(segs);
    return {
      len,
      inAt: toSourceTime(segs, Math.min(edits.fade_in, len)),
      outAt: toSourceTime(segs, Math.max(0, len - Math.min(edits.fade_out, len))),
    };
  }

  function drawFades(g: CanvasRenderingContext2D, segs: [number, number][], w: number, h: number, dpr: number) {
    if (!segs.length || !curve || !curve.length) return;
    const col = (x: number) => curve![Math.min(curve!.length - 1, Math.floor((x / w) * curve!.length))];
    g.save();
    g.globalAlpha = 0.6;
    g.fillStyle = colour.bg;
    for (let x = 0; x < w; x++) {
      const gain = col(x);
      if (gain != null && gain < 1) g.fillRect(x, 0, 1, Math.round((1 - gain) * h));
    }
    g.globalAlpha = 1;
    g.strokeStyle = colour.marker;
    g.lineWidth = 1.5 * dpr;
    g.beginPath();
    let pen = false;
    for (let x = 0; x < w; x++) {
      const gain = col(x);
      if (gain == null || gain >= 1) {
        pen = false;
        continue;
      }
      const y = (1 - gain) * h;
      if (pen) g.lineTo(x, y);
      else {
        g.moveTo(x, y);
        pen = true;
      }
    }
    g.stroke();
    g.restore();
  }

  function drawFadeHandles(g: CanvasRenderingContext2D, segs: [number, number][], w: number, dpr: number) {
    if (!segs.length) return;
    const { inAt, outAt } = fadeHandleTimes(segs);
    const X = (t: number) => clamp(Math.round((t / dur) * w), 0, w);
    g.save();
    // The fade-out handle is dimmed while the cue loops: the fade-out isn't used then.
    for (const [t, dim] of [
      [inAt, false],
      [outAt, looping],
    ] as const) {
      g.globalAlpha = dim ? 0.4 : 1;
      const r = FADE_HANDLE_R * dpr;
      g.beginPath();
      g.arc(clamp(X(t), r + dpr, w - r - dpr), FADE_HANDLE_Y * dpr, r, 0, Math.PI * 2);
      g.fillStyle = colour.marker;
      g.fill();
      g.lineWidth = 1.5 * dpr;
      g.strokeStyle = colour.bg;
      g.stroke();
    }
    g.restore();
  }

  // ---- Edits, undo ----

  const snapshotOf = (e: EditFields): EditFields => ({
    trim_start: e.trim_start,
    trim_end: e.trim_end,
    cuts: e.cuts.map((p) => [...p] as [number, number]),
    fade_in: e.fade_in,
    fade_out: e.fade_out,
  });
  const patchOf = (e: EditFields): CuePatch => ({
    trim_start: e.trim_start,
    trim_end: e.trim_end,
    cuts: e.cuts,
    fade_in: e.fade_in,
    fade_out: e.fade_out,
  });
  const same = (a: EditFields, b: EditFields) => JSON.stringify(a) === JSON.stringify(b);

  function commit(next: EditFields, before: EditFields) {
    if (same(next, before)) return;
    const stack = undoStacks.get(cue.id) ?? [];
    stack.push(before);
    if (stack.length > 100) stack.shift();
    undoStacks.set(cue.id, stack);
    undoCount = stack.length;
    onEdit(cue.id, patchOf(next));
  }

  export function undo() {
    const stack = undoStacks.get(cue.id);
    const prev = stack?.pop();
    if (!prev) return;
    undoCount = stack!.length;
    onEdit(cue.id, patchOf(prev));
  }

  /** Where I and O set a point: the audition playhead if auditioning, else the marker. */
  function markerOrPlayhead(): number | null {
    if (playhead != null) return playhead;
    if (marker != null) return marker;
    showToast("Click the waveform to place a marker, or audition and press the key while it plays.");
    return null;
  }

  export function setIn() {
    if (!peaks) return;
    const t = markerOrPlayhead();
    if (t == null) return;
    if (t > outPoint(edits, dur) - MIN_LEN) {
      showToast("The in point has to be before the out point.", true);
      return;
    }
    commit({ ...snapshotOf(edits), trim_start: r4(t) }, snapshotOf(edits));
  }

  export function setOut() {
    if (!peaks) return;
    const t = markerOrPlayhead();
    if (t == null) return;
    if (t < edits.trim_start + MIN_LEN) {
      showToast("The out point has to be after the in point.", true);
      return;
    }
    commit({ ...snapshotOf(edits), trim_end: t >= dur - 0.005 ? null : r4(t) }, snapshotOf(edits));
  }

  export function cutSelection() {
    if (!peaks || !sel) return;
    const a = Math.max(sel[0], edits.trim_start);
    const b = Math.min(sel[1], outPoint(edits, dur));
    if (b - a < 0.01) {
      showToast("The selection is outside the part that plays.", true);
      return;
    }
    const cuts = mergeCuts([...edits.cuts, [a, b]]);
    if (segmentsLength(keptSegments({ ...edits, cuts }, dur)) < MIN_LEN) {
      showToast("That would cut the whole cue.", true);
      return;
    }
    const before = snapshotOf(edits);
    sel = null;
    commit({ ...before, cuts }, before);
  }

  function putBack(i: number) {
    const before = snapshotOf(edits);
    commit({ ...before, cuts: before.cuts.filter((_, k) => k !== i) }, before);
  }

  function restore() {
    const before = snapshotOf(edits);
    commit({ ...before, trim_start: 0, trim_end: null, cuts: [] }, before);
    showToast(`${cueNumber(index)} plays the full file again. Undo brings the edits back.`);
  }

  // ---- Audition ----

  const blockedWhy = $derived(auditionBlocked());

  async function toggleAudition() {
    if (!auditioning && !checkAudition()) return;
    try {
      if (auditioning) await trimApi.stopAudition();
      else await trimApi.audition(cue.id, marker ?? edits.trim_start);
    } catch (e) {
      showToast(errorText(e), true);
    }
  }

  // ---- Pointer: drag a marker, drag to select, click to place the marker ----

  const HANDLE_PX = 8;
  let drag:
    | { kind: "in" | "out" | "fadein" | "fadeout"; before: EditFields }
    | { kind: "select"; anchor: number; startX: number; moved: boolean }
    | null = null;

  function xToTime(clientX: number) {
    const r = canvas.getBoundingClientRect();
    return clamp((clientX - r.left) / r.width, 0, 1) * dur;
  }

  function handleAt(clientX: number, clientY: number): "in" | "out" | "fadein" | "fadeout" | null {
    const r = canvas.getBoundingClientRect();
    const x = clientX - r.left;
    const at = (t: number) => (t / dur) * r.width;
    // Fade handles first: they sit under the in/out tabs and may overlap the markers.
    const segs = keptSegments(edits, dur);
    if (segs.length) {
      const { inAt, outAt } = fadeHandleTimes(segs);
      const y = clientY - r.top;
      const near = (t: number) => Math.hypot(x - at(t), y - FADE_HANDLE_Y) <= FADE_HANDLE_R + 5;
      const nIn = near(inAt);
      const nOut = near(outAt);
      if (nIn && nOut) return Math.abs(x - at(inAt)) <= Math.abs(x - at(outAt)) ? "fadein" : "fadeout";
      if (nIn) return "fadein";
      if (nOut) return "fadeout";
    }
    const dIn = Math.abs(x - at(edits.trim_start));
    const dOut = Math.abs(x - at(outPoint(edits, dur)));
    if (Math.min(dIn, dOut) > HANDLE_PX) return null;
    return dIn <= dOut ? "in" : "out";
  }

  function onPointerDown(e: PointerEvent) {
    if (!peaks) return;
    canvas.setPointerCapture(e.pointerId);
    canvas.focus();
    const h = handleAt(e.clientX, e.clientY);
    drag = h ? { kind: h, before: snapshotOf(edits) } : { kind: "select", anchor: xToTime(e.clientX), startX: e.clientX, moved: false };
  }

  function onPointerMove(e: PointerEvent) {
    if (!peaks) return;
    if (!drag) {
      const h = handleAt(e.clientX, e.clientY);
      canvas.style.cursor = h === "fadein" || h === "fadeout" ? "grab" : h ? "ew-resize" : "crosshair";
      canvas.title =
        h === "fadein"
          ? `Fade in ${fmtFade(edits.fade_in)}: drag right to lengthen`
          : h === "fadeout"
            ? `Fade out ${fmtFade(edits.fade_out)}${looping ? " (not used while the cue loops)" : ""}: drag left to lengthen`
            : "";
      return;
    }
    const t = xToTime(e.clientX);
    if (drag.kind === "fadein" || drag.kind === "fadeout") {
      const segs = keptSegments(edits, dur);
      const len = segmentsLength(segs);
      const p = toPlayedTime(segs, t);
      // Back onto its marker (within a few pixels): no fade.
      const r = canvas.getBoundingClientRect();
      const markerAt = drag.kind === "fadein" ? edits.trim_start : outPoint(edits, dur);
      const px = Math.abs(e.clientX - r.left - (markerAt / dur) * r.width);
      const next = snapshotOf(edits);
      if (drag.kind === "fadein") next.fade_in = px < 4 ? 0 : r4(clamp(p, 0, Math.min(600, len - edits.fade_out)));
      else next.fade_out = px < 4 ? 0 : r4(clamp(len - p, 0, Math.min(600, len - edits.fade_in)));
      draft = next;
      draftFades = { fade_in: next.fade_in, fade_out: next.fade_out };
      canvas.style.cursor = "grabbing";
    } else if (drag.kind === "in") {
      draft = { ...snapshotOf(edits), trim_start: r4(clamp(t, 0, outPoint(edits, dur) - MIN_LEN)) };
    } else if (drag.kind === "out") {
      const v = clamp(t, edits.trim_start + MIN_LEN, dur);
      draft = { ...snapshotOf(edits), trim_end: v >= dur - 0.005 ? null : r4(v) };
    } else if (drag.kind === "select" && Math.abs(e.clientX - drag.startX) > 3) {
      drag.moved = true;
      sel = [Math.min(drag.anchor, t), Math.max(drag.anchor, t)];
    }
  }

  function onPointerUp(e: PointerEvent) {
    const d = drag;
    drag = null;
    if (!d || !peaks) return;
    if (d.kind === "select") {
      if (!d.moved) {
        marker = xToTime(e.clientX);
        sel = null;
        if (auditioning) void trimApi.audition(cue.id, marker).catch((err) => showToast(errorText(err), true));
      }
    } else if (draft) {
      const next = draft;
      draft = null;
      draftFades = null;
      commit(next, d.before);
    }
  }

  const fmtFade = (s: number) => (s > 0 ? `${s.toFixed(s < 10 ? 2 : 1)} s` : "off");

  const canRestore = $derived(edits.trim_start > 0 || edits.trim_end != null || edits.cuts.length > 0);
</script>

<div class="trim">
  <h3>Trim and cut</h3>
  <canvas class="trim__wave" bind:this={canvas} tabindex="0"
    aria-label="Waveform. Drag the in and out markers to trim. Drag across a section to select it. Click to place the audition start."
    onpointerdown={onPointerDown} onpointermove={onPointerMove} onpointerup={onPointerUp} onpointercancel={onPointerUp}></canvas>
  <p class="trim__readout num">
    <span>In <b>{fmtTime(edits.trim_start, true)}</b></span>
    <span>Out <b>{peaks ? fmtTime(outPoint(edits, dur), true) : edits.trim_end == null ? "end" : fmtTime(edits.trim_end, true)}</b></span>
    <span>Plays for <b>{peaks ? fmtTime(segmentsLength(keptSegments(edits, dur)), true) : "…"}</b></span>
    {#if sel}<span>Selected <b>{fmtTime(sel[0], true)} to {fmtTime(sel[1], true)}</b></span>{/if}
  </p>
  <div class="trim__buttons">
    <button type="button" class="btn" class:is-blocked={!auditioning && blockedWhy} aria-pressed={auditioning}
      disabled={!peaks} title={!auditioning && blockedWhy ? blockedWhy : undefined} onclick={toggleAudition}>
      {#if !auditioning && blockedWhy}<span aria-hidden="true">⊘ </span>{/if}{auditioning ? "Stop audition" : "Audition from marker"}
    </button>
    <button type="button" class="btn" aria-keyshortcuts="I" disabled={!peaks} onclick={setIn}>Set in</button>
    <button type="button" class="btn" aria-keyshortcuts="O" disabled={!peaks} onclick={setOut}>Set out</button>
    <button type="button" class="btn" aria-keyshortcuts="X" disabled={!sel} onclick={cutSelection}>Cut selection</button>
    <button type="button" class="btn" aria-keyshortcuts="Control+Z" disabled={undoCount === 0} onclick={undo}>Undo</button>
    <button type="button" class="btn" disabled={!canRestore} onclick={restore}>Restore full file</button>
  </div>
  {#if edits.cuts.length}
    <ul class="trim__cuts">
      {#each edits.cuts as [a, b], i (i)}
        <li class="num">Cut {fmtTime(a, true)} to {fmtTime(b, true)}
          <button type="button" class="btn btn--small" onclick={() => putBack(i)}>Put back</button></li>
      {/each}
    </ul>
  {/if}
  <p class="trim__note">The audio file is never changed. Restore full file undoes every trim and cut. Drag the round handles under the markers to fade in or out; drag one back onto its marker for no fade. Audition plays on the audition output (headphones), never the show output unless you chose that, and keeps playing through Stop all: use Stop audition.</p>
</div>
