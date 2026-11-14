<script lang="ts">
  // The Show screen (reference/web/show.html): header, transport, now-playing strip, cue table.
  // Everything that plays is decided in Rust; this screen sends intents and draws the engine's
  // snapshot. It never moves focus when state changes.
  import { onMount, tick } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
  import {
    AUDIO_EXTENSIONS,
    editApi,
    errorText,
    transport,
    type Cue,
    type CuePatch,
    type CuePrep,
    type EditResult,
    type ShowView,
  } from "../lib/api";
  import { act, engine, startEngineMirror } from "../lib/engine.svelte";
  import { fmtBytes, fmtTime, plural, rowNotes } from "../lib/format";
  import { showToast } from "../lib/toast.svelte";
  import { syncValue } from "../lib/sync";
  import ConfirmDialog from "../components/ConfirmDialog.svelte";
  import Inspector from "../components/Inspector.svelte";

  let {
    show,
    onClose,
    onChanged,
    onOpenShow,
  }: {
    show: ShowView;
    onClose: () => void;
    onChanged: (v: ShowView) => void;
    onOpenShow: (id: string) => void;
  } = $props();

  let jumpDialog: HTMLDialogElement;
  let shortcutsDialog: HTMLDialogElement;
  let jumpValue = $state<number | null>(null);
  let jumpError = $state("");
  let listWrap: HTMLElement;

  const snap = $derived(engine.snap);
  const cues = $derived(show.cues);
  const standby = $derived(snap && snap.cue_count === cues.length ? snap.standby : show.local.standby_index);
  const runningByIndex = $derived(new Map((snap?.running ?? []).map((r) => [r.index, r.phase])));

  // ---- Editing ----

  let inspectedId = $state<string | null>(null);
  const inspectedIndex = $derived.by(() => {
    const i = inspectedId ? cues.findIndex((c) => c.id === inspectedId) : -1;
    return i >= 0 ? i : Math.min(standby, cues.length - 1);
  });
  const inspected = $derived(cues[inspectedIndex] ?? null);

  let saveState = $state<"saved" | "unsaved" | "error">("saved");
  let saveMessage = $state("");
  let confirmDelete = $state<{ cue: Cue; index: number } | null>(null);
  let inspector = $state<ReturnType<typeof Inspector>>();

  onMount(() => {
    void startEngineMirror();
    let un: (() => void) | undefined;
    void listen<{ state: "saved" | "unsaved" | "error"; message: string | null }>("show://save", (e) => {
      saveState = e.payload.state;
      saveMessage = e.payload.message ?? "";
    }).then((f) => (un = f));
    return () => un?.();
  });

  /** Run an edit; show what Rust sends back. */
  async function edit(f: () => Promise<EditResult>) {
    try {
      const r = await f();
      onChanged(r.show);
      if (r.focus) inspectedId = r.focus;
      if (r.problems.length) showToast(r.problems.join(" "), true);
    } catch (e) {
      showToast(errorText(e), true);
    }
  }

  const updateCue = (id: string, patch: CuePatch) => edit(() => editApi.updateCue(id, patch));

  async function addAudio(paths: string[]) {
    const audio = paths.filter((p) => AUDIO_EXTENSIONS.includes(p.split(".").pop()?.toLowerCase() ?? ""));
    if (audio.length < paths.length) showToast("Only audio files (MP3, WAV, OGG, FLAC, M4A, AIFF) can be added as cues.", true);
    if (!audio.length) return;
    const before = cues.length;
    await edit(() => editApi.addAudioCues(audio));
    const added = show.cues.length - before;
    if (added > 0) showToast(added === 1 ? `Added Q${show.cues.length}.` : `Added ${added} cues.`);
  }

  async function pickAudio() {
    const picked = await openDialog({
      multiple: true,
      title: "Add cue",
      filters: [{ name: "Audio", extensions: AUDIO_EXTENSIONS }],
    });
    if (picked) await addAudio(Array.isArray(picked) ? picked : [picked]);
  }

  async function replaceFile(id: string) {
    const picked = await openDialog({ multiple: false, title: "Replace file", filters: [{ name: "Audio", extensions: AUDIO_EXTENSIONS }] });
    if (typeof picked === "string") await edit(() => editApi.replaceCueFile(id, picked));
  }

  async function doDelete() {
    const c = confirmDelete;
    confirmDelete = null;
    if (c) await edit(() => editApi.deleteCue(c.cue.id));
  }

  async function duplicateShow() {
    try {
      const id = await editApi.duplicateShow(show.id);
      showToast("Duplicated. You're now in the copy.");
      onOpenShow(id);
    } catch (e) {
      showToast(errorText(e), true);
    }
  }

  async function exportShow() {
    try {
      const name = await editApi.exportFileName(show.id);
      const path = await saveDialog({ title: "Export show", defaultPath: name, filters: [{ name: "ThornCue show", extensions: ["tqs"] }] });
      if (!path) return;
      showToast("Exporting…");
      await editApi.exportShow(show.id, path);
      showToast(`Exported to ${path.split(/[\\/]/).pop()}.`);
    } catch (e) {
      showToast(errorText(e), true);
    }
  }

  // Drag a row by its grip to reorder (pointer events: HTML5 drag-and-drop is unavailable in
  // the webview on Windows while native file drops are on).
  let dragFrom = $state<number | null>(null);
  let dropAt = $state<number | null>(null);
  function startDrag(e: PointerEvent, i: number) {
    e.preventDefault();
    e.stopPropagation();
    const grip = e.currentTarget as HTMLElement;
    grip.setPointerCapture(e.pointerId);
    dragFrom = i;
    dropAt = i;
    const move = (ev: PointerEvent) => {
      const row = document.elementFromPoint(ev.clientX, ev.clientY)?.closest<HTMLElement>("tr[data-index]");
      if (row) dropAt = Number(row.dataset.index);
    };
    const up = () => {
      grip.removeEventListener("pointermove", move);
      grip.removeEventListener("pointerup", up);
      grip.removeEventListener("pointercancel", up);
      const from = dragFrom;
      const to = dropAt;
      dragFrom = null;
      dropAt = null;
      if (from !== null && to !== null && from !== to) void edit(() => editApi.moveCue(cues[from].id, to));
    };
    grip.addEventListener("pointermove", move);
    grip.addEventListener("pointerup", up);
    grip.addEventListener("pointercancel", up);
  }

  // Sent on every keystroke, like cue names: Rust debounces the save, not the UI.
  function renameShow(value: string) {
    void edit(() => editApi.setShowName(value.slice(0, 100)));
  }

  function selectRow(i: number) {
    inspectedId = cues[i]?.id ?? null;
    void act(() => transport.setStandby(i));
  }

  /** Dropped files on the Show screen: audio becomes cues (App routes drops here). */
  export function dropFiles(paths: string[]) {
    if (paths.some((p) => /\.(tqs|zip)$/i.test(p))) {
      showToast("Import shows from the Library.", true);
      return;
    }
    void addAudio(paths);
  }

  // Keep the standby row in view, instantly (DESIGN.md: no smooth scroll during a show).
  $effect(() => {
    const i = standby;
    void tick().then(() => {
      listWrap?.querySelector<HTMLElement>(`tr[data-index="${i}"]`)?.scrollIntoView({ block: "nearest" });
    });
  });

  // ---- Transport text ----

  const standbyName = $derived(
    cues[standby] ? `Q${standby + 1} ${cues[standby].name}` : cues.length ? "End of cue list" : "No cues",
  );

  const status = $derived.by((): { text: string; kind: "" | "running" | "paused" | "error" } => {
    if (!snap) return { text: "Starting…", kind: "" };
    if (snap.status === "restarting") return { text: "Restarting audio engine", kind: "paused" };
    if (snap.status === "device_lost") return { text: "Device lost", kind: "error" };
    if (snap.status === "no_output") return { text: "No audio output", kind: "error" };
    if (snap.paused) return { text: "Paused", kind: "paused" };
    const n = snap.running.length;
    if (n > 0) return { text: n === 1 ? "Running" : `Running ${n} cues`, kind: "running" };
    if (snap.prep_done < snap.prep_total) return { text: `Loading ${snap.prep_done} of ${snap.prep_total}`, kind: "" };
    return { text: "Ready", kind: "" };
  });

  const loadingNote = $derived(
    snap && snap.prep_done < snap.prep_total && (snap.running.length > 0 || snap.paused)
      ? `Loading ${snap.prep_done} of ${snap.prep_total}`
      : "",
  );

  // ---- Now playing: the most recently started cue, as the web ----

  const nowPlaying = $derived.by(() => {
    // The most recently started audio cue; a silence/fade-out cue only when nothing else plays.
    const cuesOnly = engine.positions.filter((p) => !p.audition);
    const audio = cuesOnly.filter((p) => !p.control);
    const p = audio[audio.length - 1] ?? cuesOnly[cuesOnly.length - 1];
    if (!p) {
      const r = snap?.running[snap.running.length - 1];
      if (r) return { name: `Q${r.index + 1} ${cues[r.index]?.name ?? ""}`, times: r.phase === "starting" ? "Loading audio…" : "", fraction: 0 };
      return null;
    }
    const phase = snap?.running.find((r) => r.index === p.index)?.phase ?? "running";
    const cue = cues[p.index];
    const name = `Q${p.index + 1} ${cue?.name ?? ""}${phase === "fading" ? " (fading out)" : ""}`;
    let times: string;
    if (phase === "waiting") times = `Waiting (pre-wait ${cue?.pre_wait ?? 0} s)`;
    else if (p.control) times = `${fmtTime(p.remaining ?? 0, true)} left`;
    else if (p.remaining === null) times = `${fmtTime(p.elapsed % Math.max(p.length, 0.001))} of ${fmtTime(p.length)}, looping`;
    else times = `${fmtTime(p.elapsed)} of ${fmtTime(p.length)}, ${fmtTime(p.remaining)} left`;
    const into = p.remaining === null ? p.elapsed % Math.max(p.length, 0.001) : p.elapsed;
    const fraction = p.length > 0 ? Math.min(1, into / p.length) : 0;
    return { name, times, fraction };
  });

  // ---- Rows ----

  function lengthText(i: number): string {
    const c = cues[i];
    if (c.type !== "audio") return fmtTime(c.duration);
    const len = snap?.prep[i]?.length;
    return len == null ? "" : fmtTime(len);
  }

  function prepText(p: CuePrep | undefined, hasAsset: boolean): { text: string; missing: boolean; title?: string } {
    if (!hasAsset) return { text: "Missing", missing: true, title: "This cue has no audio file." };
    if (!p) return { text: "", missing: false };
    switch (p.state) {
      case "pending":
      case "decoding":
        return { text: "Loading", missing: false };
      case "ready":
        return { text: "Ready", missing: false };
      case "missing":
        return { text: "Missing", missing: true, title: "The audio file isn't in the show folder." };
      case "failed":
        return { text: "Can't decode", missing: true, title: p.reason };
      default:
        return { text: "", missing: false };
    }
  }

  const PHASE_TEXT = { starting: "Loading", waiting: "Waiting", running: "Running", fading: "Fading" } as const;

  function rowState(i: number) {
    const phase = runningByIndex.get(i);
    if (phase) return { text: PHASE_TEXT[phase], missing: false, running: true, title: undefined };
    const c = cues[i];
    if (c.type !== "audio") return { text: "", missing: false, running: false, title: undefined };
    return { ...prepText(snap?.prep[i], c.asset !== null), running: false };
  }

  // ---- Actions ----

  const go = () => act(transport.go);
  let lastEsc = 0;
  function stopAll() {
    // Double Esc within 500 ms: panic (no fade, even when a stop fade is set).
    const now = performance.now();
    const panic = now - lastEsc < 500;
    lastEsc = now;
    void act(() => transport.stopAll(panic));
  }
  const togglePause = () => act(transport.togglePause);
  const backToTop = () => act(() => transport.setStandby(0));
  const restart = () => act(transport.restart);
  // The slider sends every move; the engine ramps 20 ms per change so it never clicks.
  const setMaster = (v: number) => act(() => transport.setMaster(v));

  function openJump() {
    jumpValue = null;
    jumpError = "";
    jumpDialog.showModal();
  }

  function submitJump(e: SubmitEvent) {
    e.preventDefault();
    const n = jumpValue ?? NaN;
    if (!Number.isInteger(n) || n < 1 || n > cues.length) {
      jumpError = `There is no Q${jumpValue ?? "?"}. This show has ${plural(cues.length, "cue")}.`;
      return;
    }
    jumpDialog.close();
    void act(() => transport.jump(n));
  }

  // ---- Keyboard (reference/web/app.js L1842-1895). Space is GO outside text fields. ----

  const isTyping = (t: EventTarget | null) =>
    t instanceof HTMLElement &&
    (t.isContentEditable ||
      t.matches("textarea, select, input:not([type=checkbox]):not([type=range]):not([type=button]):not([type=file])"));

  function onKeydown(e: KeyboardEvent) {
    const t = e.target;
    // Stop all always wins (CLAUDE.md rule 3): Esc stops everything even with a dialog open
    // (the dialog closes too). The web only closed the dialog.
    if (e.key === "Escape") {
      if (isTyping(t)) (t as HTMLElement).blur();
      stopAll();
      return;
    }
    if (document.querySelector("dialog[open]")) return;
    if (isTyping(t)) return;
    if ((e.ctrlKey || e.metaKey) && !e.shiftKey && e.key.toLowerCase() === "z") {
      e.preventDefault();
      inspector?.trimKey("undo");
      return;
    }
    if (e.ctrlKey || e.metaKey || e.altKey) return;
    const onRange = t instanceof HTMLElement && t.matches("input[type=range]");
    switch (e.key) {
      case " ":
        e.preventDefault();
        if (!e.repeat) void go();
        break;
      case "ArrowDown":
        if (onRange) return;
        e.preventDefault();
        inspectedId = cues[Math.min(standby + 1, cues.length - 1)]?.id ?? null;
        void act(() => transport.moveStandby(1));
        break;
      case "ArrowUp":
        if (onRange) return;
        e.preventDefault();
        inspectedId = cues[Math.max(standby - 1, 0)]?.id ?? null;
        void act(() => transport.moveStandby(-1));
        break;
      case "p":
      case "P":
        void togglePause();
        break;
      case "j":
      case "J":
        e.preventDefault();
        openJump();
        break;
      case "?":
        shortcutsDialog.showModal();
        break;
      case "i":
      case "I":
        inspector?.trimKey("in");
        break;
      case "o":
      case "O":
        inspector?.trimKey("out");
        break;
      case "x":
      case "X":
        inspector?.trimKey("cut");
        break;
    }
  }

  // Stop Space from also clicking whichever button or checkbox has focus.
  function onKeyupCapture(e: KeyboardEvent) {
    if (e.key === " " && !isTyping(e.target) && !document.querySelector("dialog[open]")) e.preventDefault();
  }
</script>

<svelte:window onkeydown={onKeydown} />
<svelte:document onkeyupcapture={onKeyupCapture} />

<div class="page-show">
  <header class="show-head">
    <button type="button" class="btn" onclick={onClose}>Library</button>
    <label for="show-name" class="sr-only">Show name</label>
    <input type="text" id="show-name" class="show-head__name" maxlength="100" autocomplete="off" use:syncValue={show.name}
      oninput={(e) => renameShow(e.currentTarget.value)} />
    {#if show.source}<span class="btn show-head__code" title="ThornCue Web show code">{show.source.code}</span>{/if}
    <label class="master" for="master-vol">Master
      <input type="range" id="master-vol" min="0" max="1" step="0.01" value={snap?.master ?? 1}
        oninput={(e) => setMaster(Number(e.currentTarget.value))} />
      <output class="num" for="master-vol">{Math.round((snap?.master ?? 1) * 100)}%</output>
    </label>
    {#if snap && snap.decoded_bytes > 0}<span class="muted num" title="Decoded audio held in memory">{fmtBytes(snap.decoded_bytes)} in memory</span>{/if}
    <span class="save-state" class:status-error={saveState === "error"} aria-live="polite" title={saveMessage}>
      {#if saveState === "unsaved"}Saving…{:else if saveState === "error"}Couldn't save{/if}
    </span>
    <button type="button" class="btn" aria-keyshortcuts="?" onclick={() => shortcutsDialog.showModal()}>Shortcuts</button>
    <button type="button" class="btn" onclick={exportShow}>Export .tqs</button>
    <button type="button" class="btn" onclick={duplicateShow}>Duplicate show</button>
  </header>

  <section class="transport" aria-label="Transport">
    <button type="button" class="transport__go" aria-keyshortcuts="Space" onclick={go}>GO</button>
    <button type="button" class="transport__stop" aria-keyshortcuts="Escape" onclick={stopAll}>Stop all</button>
    <div class="transport__more">
      <button type="button" class="btn" aria-keyshortcuts="P" aria-pressed={snap?.paused ?? false} onclick={togglePause}>
        {snap?.paused ? "Resume" : "Pause"}
      </button>
      <button type="button" class="btn" onclick={backToTop}>Back to top</button>
      <button type="button" class="btn" class:btn--danger={snap?.status === "device_lost" || snap?.status === "no_output"}
        title="Stops everything and reopens the audio output. Use it after an interface is unplugged."
        disabled={snap?.status === "restarting"} onclick={restart}>Restart audio engine</button>
    </div>
    <div class="standby">
      <span class="standby__label">Standby</span>
      <span class="standby__name">{standbyName}</span>
    </div>
    <span class="transport__status" class:is-running={status.kind === "running"} class:is-paused={status.kind === "paused"}
      class:is-error={status.kind === "error"} aria-live="polite">
      {status.text}{#if loadingNote}<span class="muted"> · {loadingNote}</span>{/if}
    </span>
  </section>

  {#if snap?.status_message}
    <div class="flash flash--error device-alert" role="alert">{snap.status_message}</div>
  {/if}
  {#if show.recovered}
    <div class="flash flash--error recovered" role="alert">{show.recovered}</div>
  {/if}

  <div class="now-playing">
    <span class="now-playing__name">{nowPlaying?.name ?? "Nothing running"}</span>
    <div class="np-bar" aria-hidden="true"><div class="np-bar__fill" style:width={`${(nowPlaying?.fraction ?? 0) * 100}%`}></div></div>
    <span class="now-playing__times num">{nowPlaying?.times ?? ""}</span>
  </div>

  <div class="cue-toolbar">
    <button type="button" class="btn btn--primary" onclick={pickAudio}>Add cue</button>
    <button type="button" class="btn" onclick={() => edit(() => editApi.addControlCue(true))}>Add silence</button>
    <button type="button" class="btn" onclick={() => edit(() => editApi.addControlCue(false))}>Add fade-out</button>
    <span class="cue-toolbar__count num">{plural(cues.length, "cue")}</span>
  </div>

  <main class="cue-list-wrap" bind:this={listWrap}>
    {#if cues.length}
      <table class="cue-table">
        <thead>
          <tr>
            <th class="c-grip" scope="col"><span class="sr-only">Reorder</span></th>
            <th class="c-q" scope="col">Q</th>
            <th class="c-name" scope="col">Name</th>
            <th class="c-dur num" scope="col">Length</th>
            <th class="c-vol num" scope="col">Volume</th>
            <th class="c-notes" scope="col">Notes</th>
            <th class="c-state" scope="col">State</th>
          </tr>
        </thead>
        <tbody>
          {#each cues as c, i (c.id)}
            {@const notes = rowNotes(c)}
            {@const st = rowState(i)}
            <tr class="cue-row" data-index={i} class:is-standby={i === standby} class:is-running={st.running}
              class:is-inspected={i === inspectedIndex} class:is-dragging={dragFrom === i}
              class:is-drop-target={dropAt === i && dragFrom !== null && dragFrom !== i}
              onclick={() => selectRow(i)}>
              <td class="c-grip" title="Drag to reorder" aria-hidden="true" onpointerdown={(e) => startDrag(e, i)}
                onclick={(e) => e.stopPropagation()}>⠿</td>
              <td class="c-q num">{i + 1}</td>
              <td class="c-name" title={c.name}>{c.name}</td>
              <td class="c-dur num">{lengthText(i)}</td>
              <td class="c-vol num">{c.type === "audio" ? `${Math.round(c.volume * 100)}%` : ""}</td>
              <td class="c-notes" title={notes}>{notes}</td>
              <td class="c-state" class:is-missing={st.missing} class:muted={st.text === "Ready" || st.text === "Loading"} title={st.title}>{st.text}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {:else}
      <div class="cue-list-empty">
        <p>This show has no cues.</p>
        <p><button type="button" class="btn btn--primary" onclick={pickAudio}>Add a cue</button></p>
        <p class="hint">Or drop audio files onto this window.</p>
      </div>
    {/if}
  </main>

  <Inspector bind:this={inspector} cue={inspected} index={inspectedIndex} onEdit={updateCue}
    onDuplicate={(id) => edit(() => editApi.duplicateCue(id))} onReplace={replaceFile}
    onDelete={(cue, index) => (confirmDelete = { cue, index })} />
</div>

{#if confirmDelete}
  {@const d = confirmDelete}
  <ConfirmDialog
    title="Delete this cue?"
    message={`Q${d.index + 1} "${d.cue.name}" will be removed from the show.`}
    confirmLabel="Delete cue"
    danger
    onConfirm={doDelete}
    onCancel={() => (confirmDelete = null)}
  />
{/if}

<dialog bind:this={shortcutsDialog} aria-labelledby="shortcuts-title">
  <div class="dialog-head">
    <h2 id="shortcuts-title">Keyboard shortcuts</h2>
    <button type="button" class="btn" onclick={() => shortcutsDialog.close()}>Close</button>
  </div>
  <div class="dialog-body">
    <table class="shortcuts">
      <tbody>
        <tr><td><kbd>Space</kbd></td><td>GO: play the standby cue</td></tr>
        <tr><td><kbd>Esc</kbd></td><td>Stop all</td></tr>
        <tr><td><kbd>Esc</kbd> <kbd>Esc</kbd></td><td>Panic: stop all at once, with no fade</td></tr>
        <tr><td><kbd>P</kbd></td><td>Pause or resume everything</td></tr>
        <tr><td><kbd>↑</kbd> <kbd>↓</kbd></td><td>Move standby up or down</td></tr>
        <tr><td><kbd>J</kbd></td><td>Jump to a cue number and play it</td></tr>
        <tr><td><kbd>I</kbd> <kbd>O</kbd></td><td>Set the in or out point at the marker (or the audition playhead)</td></tr>
        <tr><td><kbd>X</kbd></td><td>Cut the selected section</td></tr>
        <tr><td><kbd>Ctrl</kbd> <kbd>Z</kbd></td><td>Undo the last trim or cut</td></tr>
        <tr><td><kbd>?</kbd></td><td>This list</td></tr>
      </tbody>
    </table>
  </div>
</dialog>

<dialog bind:this={jumpDialog} aria-labelledby="jump-title">
  <form onsubmit={submitJump}>
    <div class="dialog-head"><h2 id="jump-title">Jump to cue</h2></div>
    <div class="dialog-body">
      <div class="field">
        <label for="jump-input">Cue number</label>
        <!-- svelte-ignore a11y_autofocus -->
        <input type="number" id="jump-input" min="1" step="1" required autofocus bind:value={jumpValue}
          aria-describedby="jump-error" />
        {#if jumpError}<p class="hint status-error" id="jump-error" role="alert">{jumpError}</p>{/if}
      </div>
    </div>
    <div class="dialog-foot">
      <button type="button" class="btn" onclick={() => jumpDialog.close()}>Cancel</button>
      <button type="submit" class="btn btn--primary">Jump and play</button>
    </div>
  </form>
</dialog>
