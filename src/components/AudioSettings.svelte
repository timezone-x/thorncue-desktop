<script lang="ts">
  // Settings → Audio: GO lockout and Stop-all fade (W6, W7). Both off by default, as the web.
  import { onMount } from "svelte";
  import { api, errorText } from "../lib/api";
  import { showToast } from "../lib/toast.svelte";

  let lockout = $state(0);
  let fade = $state(0);
  let loaded = $state(false);

  onMount(async () => {
    try {
      const s = await api.getSettings();
      lockout = s.go_lockout_ms;
      fade = s.stop_fade;
      loaded = true;
    } catch (e) {
      showToast(errorText(e), true);
    }
  });

  async function save() {
    try {
      const s = await api.setAudioOptions(Math.round(Number(lockout) || 0), Number(fade) || 0);
      lockout = s.go_lockout_ms;
      fade = s.stop_fade;
      showToast("Audio settings saved.");
    } catch (e) {
      showToast(errorText(e), true);
    }
  }
</script>

<section aria-labelledby="audio-h">
  <h2 id="audio-h">Audio</h2>
  <form onsubmit={(e) => { e.preventDefault(); void save(); }}>
    <div class="field-row">
      <div class="field">
        <label for="go-lockout">GO lockout (ms)</label>
        <input id="go-lockout" type="number" min="0" max="2000" step="50" bind:value={lockout} disabled={!loaded}
          aria-describedby="go-lockout-hint" />
        <p class="hint" id="go-lockout-hint">Ignore a second GO pressed within this time, so a double tap can't skip a cue. 0 turns it off.</p>
      </div>
      <div class="field">
        <label for="stop-fade">Stop all fade (s)</label>
        <input id="stop-fade" type="number" min="0" max="10" step="0.1" bind:value={fade} disabled={!loaded}
          aria-describedby="stop-fade-hint" />
        <p class="hint" id="stop-fade-hint">Stop all fades out over this time. 0 stops at once. Pressing Esc twice always stops at once.</p>
      </div>
    </div>
    <button type="submit" class="btn btn--primary" disabled={!loaded}>Save audio settings</button>
  </form>
</section>
