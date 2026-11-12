<script lang="ts">
  // ThornCue Web: the server's shows with their status in this library, import with
  // Replace / Keep both, and import from a pasted join link.
  import { onMount } from "svelte";
  import { errorText, webApi, type LinkInfo, type LocalStatus, type WebConnection, type WebShowRow } from "../lib/api";
  import { fmtBytes, fmtDate, plural } from "../lib/format";
  import { importJob, initImporter } from "../lib/importer.svelte";

  let { onOpen, onSettings }: { onOpen: (id: string) => void; onSettings: () => void } = $props();

  let conn = $state<WebConnection | null>(null);
  let rows = $state<WebShowRow[] | null>(null);
  let error = $state("");
  let loading = $state(false);

  /** A show that's already here: ask Replace or Keep both. */
  let choose = $state<{ name: string; status: LocalStatus; localId: string; run: (replace: string | null) => void } | null>(null);
  let chooseDialog = $state<HTMLDialogElement>();

  let linkDialog: HTMLDialogElement;
  let link = $state("");
  let linkInfo = $state<LinkInfo | null>(null);
  let linkError = $state("");
  let linkBusy = $state(false);

  const STATUS: Record<LocalStatus, { text: string; cls: string }> = {
    not_imported: { text: "Not imported", cls: "muted" },
    imported: { text: "Imported", cls: "status-ok" },
    update_available: { text: "Update available", cls: "status-warning" },
    changed_both: { text: "Changed here and on the web", cls: "status-warning" },
  };

  async function refresh() {
    error = "";
    loading = true;
    try {
      conn = await webApi.connection();
      if (!conn.server || !conn.has_token) {
        rows = null;
        return;
      }
      rows = await webApi.list();
    } catch (e) {
      error = errorText(e);
    } finally {
      loading = false;
    }
  }

  onMount(() => {
    void initImporter(() => refresh());
    void refresh();
  });

  $effect(() => {
    if (choose && chooseDialog && !chooseDialog.open) chooseDialog.showModal();
  });

  function startImport(name: string, key: string, status: LocalStatus, localId: string | null, run: (replace: string | null) => Promise<import("../lib/api").ImportResult>) {
    if (status === "not_imported" || !localId) {
      importJob(key, name, () => run(null));
      return;
    }
    choose = { name, status, localId, run: (replace) => importJob(key, name, () => run(replace)) };
  }

  function importRow(r: WebShowRow) {
    startImport(r.name, `web:${r.code}`, r.status, r.local_id, (replace) => webApi.importShow(r.code, replace));
  }

  function decide(replace: boolean) {
    const c = choose;
    chooseDialog?.close();
    choose = null;
    c?.run(replace ? c.localId : null);
  }

  function openLink() {
    link = "";
    linkInfo = null;
    linkError = "";
    linkDialog.showModal();
  }

  async function lookUpLink(e: SubmitEvent) {
    e.preventDefault();
    linkBusy = true;
    linkError = "";
    linkInfo = null;
    try {
      linkInfo = await webApi.linkInfo(link);
    } catch (err) {
      linkError = errorText(err);
    } finally {
      linkBusy = false;
    }
  }

  function importFromLink() {
    const info = linkInfo;
    const url = link;
    if (!info) return;
    linkDialog.close();
    startImport(info.name, `web:${info.code}`, info.status, info.local_id, (replace) => webApi.importLink(url, replace));
  }

  function action(r: WebShowRow): string {
    switch (r.status) {
      case "not_imported":
        return "Import";
      case "imported":
        return "Import again";
      default:
        return "Update";
    }
  }
</script>

<main class="page page--wide">
  <div class="page-head">
    <h1>ThornCue Web</h1>
    {#if conn?.server}<span class="muted">{conn.server}</span>{/if}
    <button type="button" class="btn" onclick={openLink}>Import from a link…</button>
    {#if conn?.has_token}<button type="button" class="btn" disabled={loading} onclick={refresh}>Refresh</button>{/if}
  </div>

  {#if error}<div class="error-list" role="alert">{error}</div>{/if}

  {#if conn && (!conn.server || !conn.has_token)}
    <div class="empty">
      <p>Connect to your ThornCue Web server to see your shows here.</p>
      <p><button type="button" class="btn btn--primary" onclick={onSettings}>Open Settings</button></p>
      <p class="hint">You can import a show from a share link without connecting.</p>
    </div>
  {:else if rows === null}
    {#if !error}<p class="empty">Loading shows from the web…</p>{/if}
  {:else if rows.length === 0}
    <p class="empty">You have no shows on ThornCue Web yet, and none have been shared with you.</p>
  {:else}
    <div class="table-wrap">
      <table class="table">
        <thead>
          <tr>
            <th scope="col">Name</th>
            <th scope="col">Code</th>
            <th scope="col">Owner</th>
            <th scope="col" class="num">Cues</th>
            <th scope="col" class="num">Size</th>
            <th scope="col">Updated</th>
            <th scope="col">Here</th>
            <th scope="col" class="actions"><span class="sr-only">Actions</span></th>
          </tr>
        </thead>
        <tbody>
          {#each rows as r (r.code)}
            {@const st = STATUS[r.status]}
            <tr>
              <td class="c-name">{r.name}</td>
              <td class="code">{r.code}</td>
              <td class="muted">{r.owner ?? ""}</td>
              <td class="num">{r.cue_count}</td>
              <td class="num">{fmtBytes(r.audio_bytes)}</td>
              <td class="date muted">{fmtDate(r.updated_at)}</td>
              <td class="status-cell {st.cls}">{st.text}</td>
              <td class="actions">
                {#if r.local_id}
                  <button type="button" class="btn btn--small" onclick={() => onOpen(r.local_id!)}>Open</button>
                {/if}
                <button type="button" class="btn btn--small" class:btn--primary={r.status !== "imported"}
                  onclick={() => importRow(r)}>{action(r)}</button>
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</main>

<dialog bind:this={linkDialog} aria-labelledby="link-h">
  <form onsubmit={lookUpLink}>
    <div class="dialog-head"><h2 id="link-h">Import from a link</h2></div>
    <div class="dialog-body">
      <div class="field">
        <label for="link-url">Share link</label>
        <!-- svelte-ignore a11y_autofocus -->
        <input id="link-url" type="url" placeholder="https://cues.example.com/join/…" bind:value={link} required autofocus />
        <p class="hint">A link someone shared from ThornCue Web with playback allowed.</p>
      </div>
      {#if linkError}<div class="error-list" role="alert">{linkError}</div>{/if}
      {#if linkInfo}
        <p><strong>{linkInfo.name}</strong>{linkInfo.author ? ` by ${linkInfo.author}` : ""}: {plural(linkInfo.cue_count, "cue")}, {fmtBytes(linkInfo.audio_bytes)}.</p>
        <p class="hint">From {linkInfo.server}. {STATUS[linkInfo.status].text}.</p>
        {#if linkInfo.insecure}<p class="hint status-warning">This link uses plain http.</p>{/if}
      {/if}
    </div>
    <div class="dialog-foot">
      <button type="button" class="btn" onclick={() => linkDialog.close()}>Cancel</button>
      {#if linkInfo}
        <button type="button" class="btn btn--primary" onclick={importFromLink}>Import this show</button>
      {:else}
        <button type="submit" class="btn btn--primary" disabled={linkBusy || !link}>{linkBusy ? "Checking…" : "Check link"}</button>
      {/if}
    </div>
  </form>
</dialog>

{#if choose}
  <dialog bind:this={chooseDialog} aria-labelledby="choose-h" oncancel={(e) => { e.preventDefault(); chooseDialog?.close(); choose = null; }}>
    <div class="dialog-head"><h2 id="choose-h">"{choose.name}" is already in your library</h2></div>
    <div class="dialog-body">
      <p>Replace swaps in the web's version and keeps your standby position. Keep both adds the web's version as a separate show.</p>
      {#if choose.status === "changed_both"}
        <p class="status-warning">This show has been changed here since it was imported. Replace discards those changes (the old copy goes to the trash).</p>
      {/if}
    </div>
    <div class="dialog-foot">
      <button type="button" class="btn" onclick={() => { chooseDialog?.close(); choose = null; }}>Cancel</button>
      <button type="button" class="btn" onclick={() => decide(false)}>Keep both</button>
      <button type="button" class="btn" class:btn--primary={choose.status !== "changed_both"}
        class:btn--danger={choose.status === "changed_both"} onclick={() => decide(true)}>Replace</button>
    </div>
  </dialog>
{/if}
