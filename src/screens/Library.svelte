<script lang="ts">
  // The home screen: every show in the library, as a table (DESIGN.md: shows are comparable).
  import { onMount } from "svelte";
  import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
  import { api, editApi, errorText, type ShowSummary } from "../lib/api";
  import { fmtBytes, fmtDate } from "../lib/format";
  import { importFiles, initImporter } from "../lib/importer.svelte";
  import { showToast } from "../lib/toast.svelte";
  import ConfirmDialog from "../components/ConfirmDialog.svelte";

  let { onOpen, onWeb }: { onOpen: (id: string) => void; onWeb: () => void } = $props();

  let shows = $state<ShowSummary[] | null>(null);
  let confirmDelete = $state<ShowSummary | null>(null);

  async function refresh() {
    try {
      shows = await api.listShows();
    } catch (e) {
      showToast(errorText(e), true);
      shows = [];
    }
  }

  onMount(() => {
    void initImporter(() => refresh());
    void refresh();
  });

  async function pickFile() {
    const picked = await openDialog({
      multiple: true,
      directory: false,
      title: "Import a show",
      filters: [{ name: "ThornCue show or web export", extensions: ["tqs", "zip"] }],
    });
    if (!picked) return;
    importFiles(Array.isArray(picked) ? picked : [picked]);
  }

  async function doDelete(s: ShowSummary) {
    confirmDelete = null;
    try {
      await api.deleteShow(s.id);
      showToast(`Moved "${s.name}" to the trash.`);
    } catch (e) {
      showToast(errorText(e), true);
    }
    await refresh();
  }

  async function newShow() {
    try {
      onOpen(await editApi.newShow());
    } catch (e) {
      showToast(errorText(e), true);
    }
  }

  async function duplicate(s: ShowSummary) {
    try {
      await editApi.duplicateShow(s.id);
      showToast(`Duplicated "${s.name}".`);
      await refresh();
    } catch (e) {
      showToast(errorText(e), true);
    }
  }

  async function exportShow(s: ShowSummary) {
    try {
      const name = await editApi.exportFileName(s.id);
      const path = await saveDialog({ title: "Export show", defaultPath: name, filters: [{ name: "ThornCue show", extensions: ["tqs"] }] });
      if (!path) return;
      showToast("Exporting…");
      await editApi.exportShow(s.id, path);
      showToast(`Exported "${s.name}".`);
    } catch (e) {
      showToast(errorText(e), true);
    }
  }

  const sourceText = (s: ShowSummary) => (s.source ? `ThornCue Web, ${s.source.code}` : "File");
</script>

<main class="page page--wide">
  <div class="page-head">
    <h1>Library</h1>
    <button type="button" class="btn" onclick={newShow}>New show</button>
    <button type="button" class="btn" onclick={onWeb}>Import from the web…</button>
    <button type="button" class="btn btn--primary" onclick={pickFile}>Import file…</button>
  </div>

  {#if shows === null}
    <p class="empty">Loading your shows…</p>
  {:else if shows.length === 0}
    <div class="empty">
      <p>No shows yet.</p>
      <p>Start a new show, import a <span class="code">.tqs</span> show file or a ThornCue Web export, or drop one onto this window.</p>
    </div>
  {:else}
    <div class="table-wrap">
      <table class="table">
        <thead>
          <tr>
            <th scope="col">Name</th>
            <th scope="col" class="num">Cues</th>
            <th scope="col" class="num">Size</th>
            <th scope="col">Source</th>
            <th scope="col">Last opened</th>
            <th scope="col" class="actions"><span class="sr-only">Actions</span></th>
          </tr>
        </thead>
        <tbody>
          {#each shows as s (s.id)}
            <tr ondblclick={() => onOpen(s.id)}>
              <td class="c-name">
                {s.name}
                {#if s.damaged}<span class="status-warning"> (recovered from backup)</span>{/if}
              </td>
              <td class="num">{s.cue_count}</td>
              <td class="num">{fmtBytes(s.audio_bytes)}</td>
              <td class="muted">{sourceText(s)}</td>
              <td class="date muted">{fmtDate(s.last_opened)}</td>
              <td class="actions">
                <button type="button" class="btn btn--small btn--primary" onclick={() => onOpen(s.id)}>Open</button>
                <button type="button" class="btn btn--small" onclick={() => duplicate(s)}>Duplicate</button>
                <button type="button" class="btn btn--small" onclick={() => exportShow(s)}>Export .tqs</button>
                <button type="button" class="btn btn--small btn--danger" onclick={() => (confirmDelete = s)}>Delete</button>
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</main>

{#if confirmDelete}
  {@const s = confirmDelete}
  <ConfirmDialog
    title="Delete this show?"
    message={`"${s.name}" and its audio will be moved to the trash. You can restore it from there.`}
    confirmLabel="Move to trash"
    danger
    onConfirm={() => doDelete(s)}
    onCancel={() => (confirmDelete = null)}
  />
{/if}
