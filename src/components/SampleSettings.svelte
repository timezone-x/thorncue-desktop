<script lang="ts">
  // Settings → Sample show (thorncue-spec#11; thorncue-web templates/account.html "Sample show").
  import { onMount } from "svelte";
  import { api, errorText } from "../lib/api";
  import { showToast } from "../lib/toast.svelte";
  import ConfirmDialog from "./ConfirmDialog.svelte";

  let { onOpen }: { onOpen: (id: string) => void } = $props();

  let sampleId = $state<string | null>(null);
  let busy = $state(false);
  let confirmReset = $state(false);

  onMount(() => {
    api.getSettings().then((s) => (sampleId = s.sample_show_id), () => {});
  });

  async function open() {
    busy = true;
    try {
      onOpen(await api.openSample());
    } catch (e) {
      showToast(errorText(e), true);
    } finally {
      busy = false;
    }
  }

  async function reset() {
    confirmReset = false;
    busy = true;
    try {
      const id = await api.resetSample();
      showToast("The sample show has been reset.");
      onOpen(id);
    } catch (e) {
      showToast(errorText(e), true);
    } finally {
      busy = false;
    }
  }
</script>

<section aria-labelledby="sample-h">
  <h2 id="sample-h">Sample show</h2>
  <p class="hint">A short show that teaches ThornCue as you play it. It's one of your shows: edit, rename or
    delete it like any other.</p>
  <div class="button-row">
    <button type="button" class="btn btn--primary" disabled={busy} onclick={open}>Open the sample show</button>
    <button type="button" class="btn btn--danger" disabled={busy || !sampleId}
      aria-describedby={sampleId ? undefined : "sample-reset-hint"} onclick={() => (confirmReset = true)}>Reset the sample show</button>
  </div>
  {#if !sampleId}
    <p class="hint" id="sample-reset-hint">There's nothing to reset until you've opened it.</p>
  {/if}
</section>

{#if confirmReset}
  <ConfirmDialog
    title="Reset the sample show?"
    message="Replace your sample show with a fresh copy? Any changes you made to it will be lost."
    confirmLabel="Reset"
    danger
    onConfirm={reset}
    onCancel={() => (confirmReset = false)}
  />
{/if}
