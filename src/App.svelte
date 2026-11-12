<script lang="ts">
  import { onMount } from "svelte";
  import { api, errorText, isTauri } from "./lib/api";
  import { loadTheme } from "./lib/theme.svelte";
  import { toast, showToast } from "./lib/toast.svelte";
  import Library from "./screens/Library.svelte";
  import Settings from "./screens/Settings.svelte";

  type Screen = "library" | "settings";
  let screen = $state<Screen>("library");

  onMount(async () => {
    if (!isTauri()) return;
    try {
      await loadTheme();
    } catch (e) {
      showToast(errorText(e), true);
    }
    // Two frames: the themed page has painted before the window appears.
    requestAnimationFrame(() => requestAnimationFrame(() => api.appReady()));
  });
</script>

<nav class="site-nav" aria-label="Main">
  <span class="site-nav__brand">ThornCue</span>
  <a href="#library" aria-current={screen === "library" ? "page" : undefined}
    onclick={(e) => { e.preventDefault(); screen = "library"; }}>Library</a>
  <a href="#settings" aria-current={screen === "settings" ? "page" : undefined}
    onclick={(e) => { e.preventDefault(); screen = "settings"; }}>Settings</a>
</nav>

{#if screen === "library"}
  <Library />
{:else}
  <Settings />
{/if}

{#if toast.text}
  {#key toast.id}
    <div class="toast" class:toast--error={toast.error} role="status" aria-live="polite">{toast.text}</div>
  {/key}
{/if}
