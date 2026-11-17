<script lang="ts">
  // New group / Edit group (web templates/show.html #group-dialog, app.js openGroupDialog).
  // Name, nine presets plus a custom colour; editing also offers Ungroup and Export group.
  import { GROUP_COLORS, safeColor } from "../lib/groups";

  let {
    mode,
    initialName,
    initialColor,
    hint,
    onSave,
    onUngroup,
    onExport,
    onCancel,
  }: {
    mode: "new" | "edit";
    initialName: string;
    initialColor: string;
    hint: string;
    onSave: (name: string, color: string) => void;
    onUngroup?: () => void;
    onExport?: () => void;
    onCancel: () => void;
  } = $props();

  let dialog: HTMLDialogElement;
  let nameInput: HTMLInputElement;
  // svelte-ignore state_referenced_locally
  let name = $state(initialName);
  // svelte-ignore state_referenced_locally
  let color = $state(safeColor(initialColor));
  const isPreset = $derived(GROUP_COLORS.some(([, hex]) => hex === color));

  $effect(() => {
    dialog.showModal();
    nameInput.select();
  });

  function submit(e: SubmitEvent) {
    e.preventDefault();
    const n = name.trim().slice(0, 60);
    if (!n) {
      nameInput.focus();
      return;
    }
    onSave(n, color);
  }
</script>

<dialog bind:this={dialog} aria-labelledby="group-title" oncancel={(e) => { e.preventDefault(); onCancel(); }}>
  <form onsubmit={submit}>
    <div class="dialog-head"><h2 id="group-title">{mode === "new" ? "New group" : "Edit group"}</h2></div>
    <div class="dialog-body">
      <div class="field">
        <label for="group-name">Name</label>
        <input type="text" id="group-name" maxlength="60" autocomplete="off" required bind:this={nameInput} bind:value={name} />
      </div>
      <fieldset class="field swatches">
        <legend>Colour</legend>
        <div class="swatches__row">
          {#each GROUP_COLORS as [label, hex] (hex)}
            <label class="swatch" title={label} style:--swatch={hex}>
              <input type="radio" name="group-color" value={hex} aria-label={label} checked={color === hex}
                onchange={() => (color = hex)} />
            </label>
          {/each}
        </div>
        <label class="swatch-custom" class:is-checked={!isPreset}>
          <input type="color" value={color} oninput={(e) => (color = safeColor(e.currentTarget.value))} />
          <span>Custom colour</span>
        </label>
      </fieldset>
      <p class="hint">{hint}</p>
    </div>
    <div class="dialog-foot">
      {#if mode === "edit"}
        <button type="button" class="btn" onclick={onUngroup}>Ungroup</button>
        {#if onExport}
          <button type="button" class="btn" title="Save this group and its cues as a .tqg file, to import into another show"
            onclick={onExport}>Export group</button>
        {/if}
      {/if}
      <span class="dialog-foot__spacer"></span>
      <button type="button" class="btn" onclick={onCancel}>Cancel</button>
      <button type="submit" class="btn btn--primary">{mode === "new" ? "Create group" : "Save"}</button>
    </div>
  </form>
</dialog>
