<script lang="ts">
  // Show output, audition output and audition volume (thorncue-spec#9), between the header and
  // the transport. The rule: an audition never plays through the show output unless the
  // operator chose that on purpose. The engine enforces it; this bar explains it and asks
  // before the one choice that lets the audience hear auditions.
  import { errorText, transport, type AuditionOutput, type OutputDevice, type ShowOutput } from "../lib/api";
  import { engine } from "../lib/engine.svelte";
  import { outputsUi, setFolded } from "../lib/outputs.svelte";
  import { showToast } from "../lib/toast.svelte";
  import ConfirmDialog from "./ConfirmDialog.svelte";

  const out = $derived(engine.snap?.outputs ?? null);
  const devices = $derived(out?.devices ?? []);

  // Saved devices that aren't connected still show in their list, marked, so the select
  // doesn't silently claim something else is chosen.
  function withMissing(list: OutputDevice[], saved: OutputDevice | null): { value: string; label: string }[] {
    const opts = list.map((d) => ({ value: `dev:${d.id}`, label: d.name }));
    if (saved && !list.some((d) => d.id === saved.id || d.name === saved.name)) {
      opts.push({ value: `dev:${saved.id}`, label: `${saved.name} (not connected)` });
    }
    return opts;
  }

  function deviceValue(saved: OutputDevice): string {
    const found = devices.find((d) => d.id === saved.id) ?? devices.find((d) => d.name === saved.name);
    return `dev:${(found ?? saved).id}`;
  }

  const showValue = $derived(
    !out || out.show_choice.kind === "system_default" ? "default" : deviceValue(out.show_choice),
  );
  const auditionValue = $derived(
    !out
      ? "none"
      : out.audition_choice.kind === "not_chosen"
        ? "none"
        : out.audition_choice.kind === "same_as_show"
          ? "same"
          : deviceValue(out.audition_choice),
  );
  const showOptions = $derived(withMissing(devices, out?.show_choice.kind === "device" ? out.show_choice : null));
  const auditionOptions = $derived(
    withMissing(devices, out?.audition_choice.kind === "device" ? out.audition_choice : null),
  );

  function findDevice(value: string): OutputDevice | null {
    const id = value.slice(4);
    if (devices.some((d) => d.id === id)) return devices.find((d) => d.id === id) ?? null;
    const saved = [out?.show_choice, out?.audition_choice].find((c) => c?.kind === "device" && c.id === id);
    return saved && saved.kind === "device" ? { id: saved.id, name: saved.name } : null;
  }

  /** The device the show is really on now (its own, or what the system default is). */
  function showDeviceNow(): OutputDevice | null {
    if (out?.show.device) return out.show.device;
    return devices.find((d) => d.id === out?.default_device) ?? null;
  }

  // ---- Changes ----

  let confirmSame = $state<null | { show: ShowOutput }>(null);
  let auditionSelect = $state<HTMLSelectElement>();

  async function apply(show: ShowOutput, audition: AuditionOutput) {
    try {
      await transport.setOutputs(show, audition);
    } catch (err) {
      showToast(errorText(err), true);
    }
  }

  function currentShow(): ShowOutput {
    return out?.show_choice ?? { kind: "system_default" };
  }

  function currentAudition(): AuditionOutput {
    return out?.audition_choice ?? { kind: "not_chosen" };
  }

  function onShowChange(value: string) {
    const show: ShowOutput =
      value === "default" ? { kind: "system_default" } : { kind: "device", ...(findDevice(value) as OutputDevice) };
    // Landing on the audition device: the engine stops a running audition and blocks
    // auditions until the operator picks another one; the bar says so.
    void apply(show, currentAudition());
  }

  function onAuditionChange(value: string) {
    if (value === "none") {
      void apply(currentShow(), { kind: "not_chosen" });
      return;
    }
    if (value === "same") {
      confirmSame = { show: currentShow() };
      return;
    }
    const d = findDevice(value);
    if (!d) return;
    const showDev = showDeviceNow();
    if (showDev && (showDev.id === d.id || showDev.name === d.name)) {
      // Picking the show's own device is the same choice as "Same as show output".
      confirmSame = { show: currentShow() };
      return;
    }
    void apply(currentShow(), { kind: "device", ...d });
  }

  function confirmThroughShow() {
    const c = confirmSame;
    confirmSame = null;
    if (c) void apply(c.show, { kind: "same_as_show" });
  }

  function cancelThroughShow() {
    confirmSame = null;
    // Cancel reverts: put the select back to what is saved.
    if (auditionSelect) auditionSelect.value = auditionValue;
  }

  let volTimer: ReturnType<typeof setTimeout> | undefined;
  let volDraft = $state<number | null>(null);
  function setVolume(v: number) {
    volDraft = v;
    clearTimeout(volTimer);
    volTimer = setTimeout(async () => {
      try {
        await transport.setAuditionVolume(v);
      } catch (err) {
        showToast(errorText(err), true);
      }
      volDraft = null;
    }, 60);
  }
  const volume = $derived(volDraft ?? out?.audition_volume ?? 1);

  // ---- What the bar says ----

  const auditionName = $derived.by(() => {
    const a = out?.audition;
    if (!a) return "";
    if (a.state === "ready") return a.through_show ? `${a.name} (the show output)` : a.name;
    if (a.state === "opening") return `${a.name} (opening)`;
    return a.reason.kind === "not_chosen" ? "Not chosen" : "Blocked";
  });
  const blockedMessage = $derived(out?.audition.state === "blocked" ? out.audition.message : null);
  const throughShow = $derived(out?.audition.state === "ready" && out.audition.through_show);
  const showFallback = $derived(
    out?.show.fallback
      ? `The show output "${out.show_choice.kind === "device" ? out.show_choice.name : ""}" isn't connected, so the show is playing on the system default.`
      : null,
  );
</script>

{#if out}
  <section class="outputs" class:is-folded={outputsUi.folded} aria-label="Outputs">
    {#if outputsUi.folded}
      <span class="outputs__line">
        <span>Show: <strong>{out.show.name}</strong></span>
        <span aria-hidden="true">·</span>
        <span>Audition: <strong>{auditionName}</strong></span>
      </span>
    {:else}
      <label class="outputs__field">
        <span>Show output</span>
        <select value={showValue} onfocus={() => transport.refreshDevices()}
          onchange={(e) => onShowChange(e.currentTarget.value)}>
          <option value="default">System default</option>
          {#each showOptions as o (o.value)}<option value={o.value}>{o.label}</option>{/each}
        </select>
      </label>
      <label class="outputs__field">
        <span>Audition output</span>
        <select bind:this={auditionSelect} value={auditionValue} onfocus={() => transport.refreshDevices()}
          onchange={(e) => onAuditionChange(e.currentTarget.value)}>
          <option value="none">Choose headphones…</option>
          {#each auditionOptions as o (o.value)}<option value={o.value}>{o.label}</option>{/each}
          <option value="same">Same as show output (the audience hears it)</option>
        </select>
      </label>
      <label class="outputs__field outputs__vol" for="audition-vol">
        <span>Audition volume</span>
        <input type="range" id="audition-vol" min="0" max="1" step="0.01" value={volume}
          oninput={(e) => setVolume(Number(e.currentTarget.value))} />
        <output class="num" for="audition-vol">{Math.round(volume * 100)}%</output>
      </label>
    {/if}
    <span class="outputs__status" aria-live="polite">
      {#if blockedMessage}
        <span class="status-error" title={blockedMessage}><span aria-hidden="true">⊘ </span>Auditions off: {blockedMessage}</span>
      {:else if throughShow}
        <span class="status-warning"><span aria-hidden="true">⚠ </span>Auditions play through the show output. The audience hears them.</span>
      {/if}
      {#if showFallback}
        <span class="status-warning"><span aria-hidden="true">⚠ </span>{showFallback}</span>
      {/if}
    </span>
    <button type="button" class="btn outputs__toggle" aria-expanded={!outputsUi.folded}
      onclick={() => setFolded(!outputsUi.folded)}>{outputsUi.folded ? "Change outputs" : "Hide"}</button>
  </section>
{/if}

{#if confirmSame}
  <ConfirmDialog
    title="Play auditions through the show output?"
    message="Auditions will play through the show output, so the audience will hear them."
    confirmLabel="Play auditions on the show output"
    danger
    onConfirm={confirmThroughShow}
    onCancel={cancelThroughShow}
  />
{/if}

<style>
  .outputs {
    display: flex;
    align-items: center;
    gap: 8px 16px;
    flex-wrap: wrap;
    padding: 4px 16px;
    border-bottom: 1px solid var(--border);
    background: var(--chrome);
    font-size: 13px;
  }
  .outputs__field {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .outputs__field select {
    max-width: 260px;
  }
  .outputs__vol input {
    width: 96px;
  }
  .outputs__line {
    display: inline-flex;
    gap: 8px;
    min-width: 0;
  }
  .outputs__status {
    display: inline-flex;
    flex-direction: column;
    gap: 2px;
    flex: 1 1 240px;
    min-width: 0;
  }
  .outputs__toggle {
    margin-left: auto;
  }
</style>
