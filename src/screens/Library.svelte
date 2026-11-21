<script lang="ts">
  // The home screen: every show in the library, as a table (DESIGN.md: shows are comparable).
  import { onMount } from "svelte";
  import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
  import { api, editApi, errorText, type ShowSummary } from "../lib/api";
  import { fmtBytes, fmtDate } from "../lib/format";
  import { importFiles, initImporter } from "../lib/importer.svelte";
  import { showToast } from "../lib/toast.svelte";
  import ConfirmDialog from "../components/ConfirmDialog.svelte";
  import AudioExportDialog from "../components/AudioExportDialog.svelte";
  import { audioJob } from "../lib/audioExport.svelte";
  import Menu, { type MenuEntry } from "../components/Menu.svelte";

  let { onOpen, onWeb }: { onOpen: (id: string) => void; onWeb: () => void } = $props();

  let shows = $state<ShowSummary[] | null>(null);
  let confirmDelete = $state<ShowSummary | null>(null);
  /** The show whose Export audio… dialog is open (thorncue-spec#17). */
  let audioFor = $state<string | null>(null);
  /** The first-run welcome card (thorncue-web templates/new_shows_list.html). */
  let welcome = $state(false);
  let openingSample = $state(false);

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
    api.getSettings().then((s) => (welcome = !s.welcome_seen), () => {});
  });

  async function openSample() {
    openingSample = true;
    try {
      const id = await api.openSample();
      welcome = false;
      onOpen(id);
    } catch (e) {
      showToast(errorText(e), true);
    } finally {
      openingSample = false;
    }
  }

  async function skipWelcome() {
    welcome = false;
    try {
      await api.skipWelcome();
    } catch (e) {
      showToast(errorText(e), true);
    }
  }

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

  // More ▾ (DESIGN.md "Row actions in show lists"): Open stays a button, the rest go here.
  const moreItems = (s: ShowSummary): MenuEntry[] => [
    { label: "Duplicate", onSelect: () => duplicate(s) },
    { label: "Export .tqs…", title: "Save the cue list and its audio as a .tqs file, for ThornCue Web or another computer", onSelect: () => exportShow(s), dialog: true },
    {
      label: "Export audio…",
      title: "Save the cues as audio files (WAV, MP3 or the originals) in a zip",
      disabled: audioJob.running ? "Already exporting" : undefined,
      onSelect: () => (audioFor = s.id),
      dialog: true,
    },
    { separator: true },
    { label: "Delete…", onSelect: () => (confirmDelete = s), dialog: true },
  ];

  const sourceText = (s: ShowSummary) => (s.source ? `ThornCue Web, ${s.source.code}` : "File");
</script>

<main class="page page--wide">
  <div class="page-head">
    <h1>Library</h1>
    <button type="button" class="btn" onclick={newShow}>New show</button>
    <button type="button" class="btn" onclick={onWeb}>Import from the web…</button>
    <button type="button" class="btn btn--primary" onclick={pickFile}>Import file…</button>
  </div>

  {#if welcome}
    <section class="welcome-card" aria-labelledby="welcome-heading">
      <h2 id="welcome-heading">Welcome to ThornCue</h2>
      <p>The sample show is a short cue list that teaches you as you play it: each cue's name tells you what
        to press and what to look for. It becomes one of your shows, so you can edit it, rename it or delete
        it.</p>
      <div class="button-row">
        <button type="button" class="btn btn--primary" disabled={openingSample} onclick={openSample}>
          {openingSample ? "Opening…" : "Open the sample show"}</button>
        <button type="button" class="btn" disabled={openingSample} onclick={skipWelcome}>Skip</button>
      </div>
      <p class="hint">You can open it later from Settings.</p>
    </section>
  {/if}

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
            <tr ondblclick={(e) => { if (!(e.target as HTMLElement).closest("button, .menu__list")) onOpen(s.id); }}>
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
                <Menu id={`more-${s.id}`} label="More" small buttonLabel={`More actions for ${s.name}`} items={moreItems(s)} />
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</main>

{#if audioFor}
  <AudioExportDialog show={audioFor} onClose={() => (audioFor = null)} />
{/if}

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
