<script lang="ts">
  import { errorText, type ThemeFamily, type ThemeMode } from "../lib/api";
  import { FAMILIES, setTheme, theme } from "../lib/theme.svelte";
  import { showToast } from "../lib/toast.svelte";
  import AudioSettings from "../components/AudioSettings.svelte";
  import WebSettings from "../components/WebSettings.svelte";

  const MODES: { id: ThemeMode; label: string }[] = [
    { id: "dark", label: "Dark" },
    { id: "light", label: "Light" },
    { id: "system", label: "System" },
  ];

  async function choose(family: ThemeFamily, mode: ThemeMode) {
    try {
      await setTheme(family, mode);
    } catch (e) {
      showToast(errorText(e), true);
    }
  }

  // Swatches show each family in the current mode (dark for System when the OS is dark).
  const swatchMode = $derived(
    theme.mode === "system"
      ? window.matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark"
      : theme.mode,
  );
</script>

<main class="page">
  <div class="page-head">
    <h1>Settings</h1>
  </div>

  <section aria-labelledby="theme-h">
    <h2 id="theme-h">Theme</h2>
    <fieldset class="theme-picker">
      <legend>Colours</legend>
      <div class="theme-cards">
        {#each FAMILIES as f (f.id)}
          <label class="theme-card">
            <span class="check">
              <input type="radio" name="theme-family" value={f.id} checked={theme.family === f.id}
                onchange={() => choose(f.id, theme.mode)} />
              {swatchMode === "light" ? f.lightName : f.name}
            </span>
            <span class="theme-swatch" data-theme={`${f.id}-${swatchMode}`} aria-hidden="true">
              <span class="sw-chrome"></span><span class="sw-select"></span><span class="sw-go"></span><span class="sw-stop"></span>
            </span>
            <span class="muted">{f.why}</span>
          </label>
        {/each}
      </div>
    </fieldset>
    <fieldset class="theme-picker">
      <legend>Mode</legend>
      <div class="segmented" role="radiogroup" aria-label="Mode">
        {#each MODES as m (m.id)}
          <label class="btn">
            <input type="radio" name="theme-mode" value={m.id} checked={theme.mode === m.id}
              onchange={() => choose(theme.family, m.id)} />
            {m.label}
          </label>
        {/each}
      </div>
      <p class="hint">System follows your computer's light or dark setting.</p>
    </fieldset>
  </section>

  <AudioSettings />

  <WebSettings />
</main>
