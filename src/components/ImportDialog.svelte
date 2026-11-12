<script lang="ts">
  // Shows the file being imported, its progress, and then warnings or the reason it was refused.
  import { dismissImport, importer } from "../lib/importer.svelte";
  import { fmtBytes, plural } from "../lib/format";

  let { onOpen }: { onOpen: (id: string) => void } = $props();
  let dialog: HTMLDialogElement;

  const cur = $derived(importer.current);

  $effect(() => {
    if (cur.phase === "idle") {
      if (dialog.open) dialog.close();
    } else if (!dialog.open) {
      dialog.showModal();
    }
  });

  function close() {
    dismissImport();
  }
</script>

<dialog bind:this={dialog} aria-labelledby="import-h" oncancel={(e) => { if (cur.phase === "working") e.preventDefault(); else close(); }}>
  <div class="dialog-head">
    <h2 id="import-h">
      {#if cur.phase === "working"}Importing{:else if cur.phase === "done"}{cur.result.name ? "Imported" : "Already up to date"}{:else if cur.phase === "failed"}Couldn't import{/if}
    </h2>
  </div>
  <div class="dialog-body">
    {#if cur.phase === "working"}
      <p>{cur.file}</p>
      <progress class="import-progress" max={cur.total || 1} value={cur.done}></progress>
      <p class="hint num" aria-live="polite">
        {#if cur.total > 0}{fmtBytes(cur.done)} of {fmtBytes(cur.total)}{:else if cur.done > 0}{fmtBytes(cur.done)}{:else}Starting…{/if}
      </p>
    {:else if cur.phase === "done"}
      {#if cur.result.name}
        <p><strong>{cur.result.name}</strong> is in your library, with {plural(cur.result.cue_count, "cue")}.</p>
      {/if}
      {#if !cur.result.name}
        {#each cur.result.warnings as w, i (i)}<p>{w}</p>{/each}
      {:else if cur.result.warnings.length}
        <p class="status-warning">Some things were skipped or changed:</p>
        <ul class="import-warnings">
          {#each cur.result.warnings as w, i (i)}<li>{w}</li>{/each}
        </ul>
      {/if}
    {:else if cur.phase === "failed"}
      <p>{cur.file}</p>
      <div class="error-list" role="alert">{cur.error}</div>
      <p class="hint">Nothing was added to your library.</p>
    {/if}
    {#if importer.queue.length}
      <p class="hint">{plural(importer.queue.length, "more file")} waiting.</p>
    {/if}
  </div>
  {#if cur.phase !== "working"}
    <div class="dialog-foot">
      {#if cur.phase === "done" && !importer.queue.length && cur.result.id}
        <button type="button" class="btn" onclick={close}>Close</button>
        <!-- svelte-ignore a11y_autofocus -->
        <button type="button" class="btn btn--primary" autofocus onclick={() => { const id = cur.result.id; close(); onOpen(id); }}>Open show</button>
      {:else}
        <!-- svelte-ignore a11y_autofocus -->
        <button type="button" class="btn btn--primary" autofocus onclick={close}>{importer.queue.length ? "Next file" : "Close"}</button>
      {/if}
    </div>
  {/if}
</dialog>
