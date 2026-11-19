<script lang="ts">
  // Export audio… (FORMAT.md section 7; thorncue-spec#17): the same options, defaults and
  // wording as thorncue-web templates/show.html #audio-dialog and static/app.js "Export audio".
  // Rust builds the zip on a blocking task; this shows progress and Stop. Playback carries on.
  import { onMount } from "svelte";
  import { AUDIO_DEFAULTS, errorText, exportApi, type AudioExportOptions, type AudioFormat, type CueChoice, type ExportOutline } from "../lib/api";
  import { aboutBytes, audioJob, progressText, runAudioExport } from "../lib/audioExport.svelte";
  import { cueNumber, safeFileName } from "../lib/format";
  import { showToast } from "../lib/toast.svelte";

  let {
    show = null,
    selection = [],
    currentGroup = null,
    onClose,
  }: {
    /** A library show's id (the Library row), or null for the open show. */
    show?: string | null;
    /** The shift-selected cue ids, in show order. */
    selection?: string[];
    /** The inspected cue's group: the group picker starts on it. */
    currentGroup?: string | null;
    onClose: () => void;
  } = $props();

  let dialog: HTMLDialogElement;
  let outline = $state<ExportOutline | null>(null);
  let cuesChoice = $state<"all" | "selected" | "group">("all");
  let group = $state("");
  let edits = $state(AUDIO_DEFAULTS.edits);
  let format = $state<AudioFormat>(AUDIO_DEFAULTS.format);
  let numbers = $state(AUDIO_DEFAULTS.numbers);
  let folders = $state(AUDIO_DEFAULTS.folders);
  let cueList = $state(AUDIO_DEFAULTS.cue_list);
  let tqs = $state(AUDIO_DEFAULTS.tqs);
  let summary = $state("Working out the size…");
  let nothing = $state(false);
  let error = $state("");
  /** Hidden while exporting, so the Show screen has the keyboard (GO) back. */
  let hiddenWhileRunning = false;

  onMount(() => {
    dialog.showModal();
    exportApi.outline(show).then(
      (o) => {
        outline = o;
        group = o.groups.find((g) => g.id === currentGroup)?.id ?? o.groups[0]?.id ?? "";
      },
      (e) => (error = errorText(e)),
    );
  });

  const cues = $derived(outline?.cues ?? []);
  const groups = $derived(outline?.groups ?? []);
  const selected = $derived(cues.filter((c) => selection.includes(c.id)));

  const options = $derived<AudioExportOptions>({
    cues: (cuesChoice === "selected" ? { kind: "selected", value: selected.map((c) => c.id) }
      : cuesChoice === "group" ? { kind: "group", value: group } : { kind: "all" }) as CueChoice,
    edits, format, numbers, folders, cue_list: cueList, tqs,
  });

  // Examples use the first audio cue of what's being exported (web updateAudioDialog).
  const picked = $derived(cuesChoice === "selected" ? selected : cuesChoice === "group" ? cues.filter((c) => c.group === group) : cues);
  const eg = $derived(picked.find((c) => c.audio_ext) ?? picked[0] ?? cues[0]);
  const ext = $derived(edits ? format : (eg?.audio_ext ?? "wav"));
  const stem = $derived.by(() => {
    if (!eg) return "Q01 Storm";
    const n = cueNumber(cues.indexOf(eg), cues.length, true);
    return safeFileName(numbers ? `${n} ${eg.name}` : eg.name, n);
  });
  const folderEg = $derived.by(() => {
    const g = groups.find((x) => x.id === eg?.group) ?? groups[0];
    return safeFileName(g?.name ?? "Group", "Group", 60);
  });

  let seq = 0;
  $effect(() => {
    const o = options;
    if (!outline) return;
    const mine = ++seq;
    summary = "Working out the size…";
    const t = setTimeout(async () => {
      try {
        const d = await exportApi.estimate(show, o);
        if (mine !== seq) return;
        const extra = [o.cue_list && "the cue list", o.tqs && "the .tqs"].filter(Boolean);
        nothing = !d.files && !o.cue_list && !o.tqs;
        summary = d.files
          ? `${d.files} ${d.files === 1 ? "file" : "files"}, about ${aboutBytes(d.bytes)}${extra.length ? `, plus ${extra.join(" and ")}` : ""}.`
          : "No audio files to export: these cues have no audio (or their files are missing).";
      } catch (e) {
        if (mine === seq) summary = `Couldn't work out the size: ${errorText(e)}`;
      }
    }, 150);
    return () => clearTimeout(t);
  });

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    if (audioJob.running || (cuesChoice === "group" && !group)) return;
    error = "";
    try {
      const r = await runAudioExport(show, options);
      if (r === "cancelled") return;
      if (r === null) {
        if (hiddenWhileRunning) showToast("Stopped the audio export.");
        return;
      }
      showToast(`Exported ${r.files} ${r.files === 1 ? "file" : "files"} to ${r.file_name}.`);
      if (!hiddenWhileRunning) close();
    } catch (err) {
      if (hiddenWhileRunning) showToast(errorText(err), true);
      else error = errorText(err);
    }
  }

  function close() {
    dialog.close();
    onClose();
  }

  /** Close the dialog and let the export carry on (its progress shows on Export audio…). */
  function hide() {
    hiddenWhileRunning = true;
    close();
  }

  function cancel() {
    if (audioJob.running) void exportApi.stop();
    else close();
  }
</script>

<!-- Esc still reaches Stop all (the Show screen's handler); it doesn't throw away a running export. -->
<dialog bind:this={dialog} aria-labelledby="ae-title" oncancel={(e) => { e.preventDefault(); if (!audioJob.running) close(); }}>
  <form onsubmit={submit}>
    <div class="dialog-head"><h2 id="ae-title">Export audio</h2></div>
    <div class="dialog-body">
      <fieldset class="field options" disabled={audioJob.running}>
        <legend>Cues</legend>
        <label class="check"><input type="radio" bind:group={cuesChoice} value="all" /> All</label>
        {#if selected.length > 1}
          <label class="check"><input type="radio" bind:group={cuesChoice} value="selected" />
            Selected ({selected.length} cues, {cueNumber(cues.indexOf(selected[0]))} to {cueNumber(cues.indexOf(selected[selected.length - 1]))})</label>
        {/if}
        {#if groups.length}
          <span class="options__row">
            <label class="check"><input type="radio" bind:group={cuesChoice} value="group" /> Group</label>
            <select aria-label="Group" bind:value={group} disabled={cuesChoice !== "group"}
              onpointerdown={() => (cuesChoice = "group")}>
              {#each groups as g (g.id)}<option value={g.id}>{g.name}</option>{/each}
            </select>
          </span>
        {/if}
      </fieldset>
      <fieldset class="field options" disabled={audioJob.running}>
        <legend>Files</legend>
        <label class="check"><input type="checkbox" bind:checked={edits} /> Apply edits (trims, cuts, fades, volume)</label>
        {#if edits}
          <span class="options__row options__indent" role="radiogroup" aria-label="Format">
            <span class="muted">Format</span>
            <label class="check"><input type="radio" bind:group={format} value="wav" /> WAV</label>
            <label class="check"><input type="radio" bind:group={format} value="mp3" /> MP3</label>
          </span>
        {:else}
          <span class="hint options__indent">The original files go in exactly as they were added.</span>
        {/if}
        <label class="check"><input type="checkbox" bind:checked={numbers} /> Keep cue numbers in file names</label>
        <span class="hint options__indent">{stem}.{ext}</span>
        <label class="check"><input type="checkbox" bind:checked={folders} /> Folder per group</label>
        <span class="hint options__indent">{folders ? `${folderEg}/${stem}.${ext}. Cues not in a group stay at the top level.` : "Every file at the top level."}</span>
      </fieldset>
      <fieldset class="field options" disabled={audioJob.running}>
        <legend>Also include</legend>
        <label class="check"><input type="checkbox" bind:checked={cueList} /> The cue list (cue-list.csv)</label>
        <label class="check"><input type="checkbox" bind:checked={tqs} /> The .tqs, so the export can be imported again</label>
      </fieldset>
      {#if audioJob.running}
        <div class="options__progress">
          <progress max="1" value={audioJob.progress && audioJob.progress.total ? audioJob.progress.done / audioJob.progress.total : undefined}></progress>
          <span aria-live="polite">{progressText(audioJob.progress)}</span>
        </div>
      {:else}
        <p class="options__summary" aria-live="polite">{summary}</p>
      {/if}
      {#if error}<p class="hint status-error" role="alert">{error}</p>{/if}
    </div>
    <div class="dialog-foot">
      {#if audioJob.running}
        <button type="button" class="btn" title="Close this and keep exporting; GO and the keyboard work again" onclick={hide}>Hide</button>
      {/if}
      <button type="button" class="btn" onclick={cancel}>{audioJob.running ? "Stop" : "Cancel"}</button>
      <button type="submit" class="btn btn--primary" disabled={audioJob.running || nothing || !outline}>Export</button>
    </div>
  </form>
</dialog>
