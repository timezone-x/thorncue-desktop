<script lang="ts">
  // Settings → ThornCue Web: server address and API token, tested before they're saved.
  // The token goes to the OS keychain; it is never shown again once saved.
  import { onMount } from "svelte";
  import { errorText, webApi, type WebConnection } from "../lib/api";
  import { showToast } from "../lib/toast.svelte";

  let conn = $state<WebConnection | null>(null);
  let server = $state("");
  let token = $state("");
  let busy = $state(false);
  let error = $state("");

  onMount(async () => {
    try {
      conn = await webApi.connection();
      server = conn.server ?? "";
    } catch (e) {
      error = errorText(e);
    }
  });

  const plainHttp = $derived(/^\s*http:\/\//i.test(server));

  async function connect(e: SubmitEvent) {
    e.preventDefault();
    busy = true;
    error = "";
    try {
      conn = await webApi.connect(server, token);
      server = conn.server ?? server;
      token = "";
      showToast(`Connected to ThornCue Web as ${conn.username}.`);
    } catch (err) {
      error = errorText(err);
    } finally {
      busy = false;
    }
  }

  async function test() {
    busy = true;
    error = "";
    try {
      conn = await webApi.check();
    } catch (err) {
      error = errorText(err);
    } finally {
      busy = false;
    }
  }

  async function disconnect() {
    try {
      conn = await webApi.disconnect();
      showToast("The API token was removed from this computer.");
    } catch (err) {
      error = errorText(err);
    }
  }
</script>

<section aria-labelledby="web-h">
  <h2 id="web-h">ThornCue Web</h2>
  <p class="hint web-intro">Import shows from your ThornCue Web server. Create an API token on the web's Settings page and paste it here.</p>

  {#if conn?.has_token}
    <p class="web-status">
      {#if conn.username}<span class="status-ok">Connected as {conn.username}</span>{:else}<span>Token saved</span>{/if}
      <span class="muted"> · {conn.server}</span>
    </p>
    {#if conn.token_store === "settings_file"}
      <p class="hint status-warning">This computer has no keychain, so the token is kept in ThornCue's settings file, readable only by your user account.</p>
    {/if}
  {/if}

  <form onsubmit={connect}>
    <div class="field">
      <label for="web-server">Server address</label>
      <input id="web-server" type="url" inputmode="url" autocomplete="url" placeholder="https://cues.example.com"
        bind:value={server} required />
      {#if plainHttp}
        <p class="hint status-warning">This address uses plain http, so the token is sent unencrypted. Use it only on a network you trust.</p>
      {/if}
    </div>
    <div class="field">
      <label for="web-token">API token</label>
      <input id="web-token" type="password" autocomplete="off" spellcheck="false"
        placeholder={conn?.has_token ? "Saved. Paste a new one to replace it." : ""} bind:value={token} />
    </div>
    {#if error}<div class="error-list" role="alert">{error}</div>{/if}
    <div class="theme-save">
      <button type="submit" class="btn btn--primary" disabled={busy || !server || !token}>{busy ? "Checking…" : "Connect"}</button>
      {#if conn?.has_token}
        <button type="button" class="btn" disabled={busy} onclick={test}>Test connection</button>
        <button type="button" class="btn btn--danger" disabled={busy} onclick={disconnect}>Remove token</button>
      {/if}
    </div>
  </form>
</section>
