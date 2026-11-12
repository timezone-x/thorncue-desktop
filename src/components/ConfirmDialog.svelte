<script lang="ts">
  // A yes/no question. The confirm button says what it does.
  let {
    title,
    message,
    confirmLabel,
    danger = false,
    onConfirm,
    onCancel,
  }: {
    title: string;
    message: string;
    confirmLabel: string;
    danger?: boolean;
    onConfirm: () => void;
    onCancel: () => void;
  } = $props();
  let dialog: HTMLDialogElement;

  $effect(() => {
    dialog.showModal();
  });
</script>

<dialog bind:this={dialog} aria-labelledby="confirm-h" oncancel={(e) => { e.preventDefault(); onCancel(); }}>
  <div class="dialog-head"><h2 id="confirm-h">{title}</h2></div>
  <div class="dialog-body"><p>{message}</p></div>
  <div class="dialog-foot">
    <button type="button" class="btn" onclick={onCancel}>Cancel</button>
    <button type="button" class="btn" class:btn--danger={danger} class:btn--primary={!danger} onclick={onConfirm}>{confirmLabel}</button>
  </div>
</dialog>
