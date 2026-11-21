<script lang="ts">
  import { onMount } from "svelte";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import { api, errorText, isTauri, transport, type ShowView } from "./lib/api";
  import { engine, startEngineMirror } from "./lib/engine.svelte";
  import { plural } from "./lib/format";
  import ConfirmDialog from "./components/ConfirmDialog.svelte";
  import { importFiles } from "./lib/importer.svelte";
  import { loadTheme } from "./lib/theme.svelte";
  import { toast, showToast } from "./lib/toast.svelte";
  import ImportDialog from "./components/ImportDialog.svelte";
  import Library from "./screens/Library.svelte";
  import Settings from "./screens/Settings.svelte";
  import Show from "./screens/Show.svelte";
  import WebShows from "./screens/WebShows.svelte";

  type Screen = "library" | "web" | "settings" | "show";
  let screen = $state<Screen>("library");
  let show = $state<ShowView | null>(null);
  let dragging = $state(false);
  let showScreen = $state<ReturnType<typeof Show>>();

  async function openShow(id: string) {
    try {
      show = await api.openShow(id);
      screen = "show";
    } catch (e) {
      showToast(errorText(e), true);
    }
  }

  async function closeShow() {
    try {
      await api.closeShow();
    } catch (e) {
      showToast(errorText(e), true);
    }
    show = null;
    screen = "library";
  }

  type Place = Exclude<Screen, "show">;
  /** Where the nav was asked to go from the Show screen while cues were playing. */
  let leaveTo = $state<Place | null>(null);
  const runningCount = $derived(engine.snap?.running.length ?? 0);

  async function go(to: Place) {
    if (screen !== "show") {
      screen = to;
      return;
    }
    // From the Show screen the nav closes the show, which stops it. A stray click on Settings
    // mid-show mustn't do that silently, so ask while anything is playing.
    if (runningCount > 0 && leaveTo === null) {
      leaveTo = to;
      return;
    }
    leaveTo = null;
    await closeShow();
    screen = to;
  }

  onMount(() => {
    if (!isTauri()) return;
    let unlisten: (() => void) | undefined;
    (async () => {
      try {
        await loadTheme();
        await startEngineMirror();
        // After a webview reload the show is still open (and maybe playing) in Rust: go back to it.
        const open = await transport.openShow();
        if (open) {
          show = open;
          screen = "show";
        } else {
          // The last run ended without closing properly (crash, power cut): Rust reopens the
          // show it had open, at its standby cue. Nothing plays until GO.
          const r = await api.recoverSession();
          if (r) {
            show = r.show; // r.message is also the Show screen's banner
            screen = "show";
          }
        }
      } catch (e) {
        showToast(errorText(e), true);
      }
      // Two frames: the themed page has painted before the window appears.
      requestAnimationFrame(() => requestAnimationFrame(() => void api.appReady()));

      // Drop a .tqs or .zip anywhere on the window (outside the Show screen) to import it.
      unlisten = await getCurrentWebview().onDragDropEvent((e) => {
        const p = e.payload;
        // On the Show screen, dropped audio becomes cues (no dialog over the transport);
        // shows are imported from the Library.
        if (screen === "show") {
          dragging = false;
          if (p.type === "drop") showScreen?.dropFiles(p.paths);
          return;
        }
        if (p.type === "enter" || p.type === "over") dragging = true;
        else if (p.type === "leave") dragging = false;
        else if (p.type === "drop") {
          dragging = false;
          const ignored = importFiles(p.paths);
          if (ignored) showToast("Only .tqs show files and ThornCue Web .zip exports can be imported.", true);
        }
      });
    })();
    return () => unlisten?.();
  });
</script>

<nav class="site-nav" aria-label="Main">
  <span class="site-nav__brand">ThornCue</span>
  <a href="#library" aria-current={screen === "library" ? "page" : undefined}
    onclick={(e) => { e.preventDefault(); void go("library"); }}>Library</a>
  <a href="#web" aria-current={screen === "web" ? "page" : undefined}
    onclick={(e) => { e.preventDefault(); void go("web"); }}>ThornCue Web</a>
  <a href="#settings" aria-current={screen === "settings" ? "page" : undefined}
    onclick={(e) => { e.preventDefault(); void go("settings"); }}>Settings</a>
</nav>
{#if screen === "show" && show}
  <Show bind:this={showScreen} {show} onChanged={(v) => (show = v)} onOpenShow={openShow} />
{:else if screen === "library"}
  <Library onOpen={openShow} onWeb={() => go("web")} />
{:else if screen === "web"}
  <WebShows onOpen={openShow} onSettings={() => go("settings")} />
{:else}
  <Settings onOpen={openShow} />
{/if}

{#if leaveTo}
  <ConfirmDialog
    title="Leave the show?"
    message={`Leaving closes the show, which stops the ${plural(runningCount, "cue")} playing.`}
    confirmLabel="Stop and leave"
    danger
    onConfirm={() => leaveTo && go(leaveTo)}
    onCancel={() => (leaveTo = null)}
  />
{/if}

<ImportDialog onOpen={openShow} />

{#if dragging}
  <div class="drop-overlay" aria-hidden="true">Drop to import</div>
{/if}

{#if toast.text}
  {#key toast.id}
    <div class="toast" class:toast--error={toast.error} role="status" aria-live="polite">{toast.text}</div>
  {/key}
{/if}
