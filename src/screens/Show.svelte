<script lang="ts">
  // The Show screen (reference/web/show.html). M2: header and cue table, read-only.
  import type { ShowView } from "../lib/api";
  import { fmtTime, plural, rowNotes } from "../lib/format";

  let { show, onClose }: { show: ShowView; onClose: () => void } = $props();

  const standby = $derived(show.local.standby_index);

  function lengthText(c: ShowView["cues"][number]): string {
    // Audio lengths come from the engine once decoded (M3); silence and fade-out have one.
    return c.type === "audio" ? "" : fmtTime(c.duration);
  }

  function stateText(c: ShowView["cues"][number]): { text: string; missing: boolean } {
    if (c.type === "audio" && !c.asset) return { text: "Missing", missing: true };
    return { text: "", missing: false };
  }
</script>

<div class="page-show">
  <header class="show-head">
    <button type="button" class="btn" onclick={onClose}>Library</button>
    <h1 class="show-head__name show-head__title">{show.name}</h1>
    {#if show.source}<span class="btn show-head__code" title="ThornCue Web show code">{show.source.code}</span>{/if}
  </header>

  {#if show.recovered}
    <div class="flash flash--error recovered" role="alert">{show.recovered}</div>
  {/if}

  <div class="cue-toolbar">
    <span class="cue-toolbar__count num">{plural(show.cues.length, "cue")}</span>
  </div>

  <main class="cue-list-wrap">
    {#if show.cues.length}
      <table class="cue-table">
        <thead>
          <tr>
            <th class="c-grip" scope="col"><span class="sr-only">Reorder</span></th>
            <th class="c-q" scope="col">Q</th>
            <th class="c-name" scope="col">Name</th>
            <th class="c-dur num" scope="col">Length</th>
            <th class="c-vol num" scope="col">Volume</th>
            <th class="c-notes" scope="col">Notes</th>
            <th class="c-state" scope="col">State</th>
          </tr>
        </thead>
        <tbody>
          {#each show.cues as c, i (c.id)}
            {@const notes = rowNotes(c)}
            {@const st = stateText(c)}
            <tr class="cue-row" class:is-standby={i === standby}>
              <td class="c-grip" aria-hidden="true"></td>
              <td class="c-q num">{i + 1}</td>
              <td class="c-name" title={c.name}>{c.name}</td>
              <td class="c-dur num">{lengthText(c)}</td>
              <td class="c-vol num">{c.type === "audio" ? `${Math.round(c.volume * 100)}%` : ""}</td>
              <td class="c-notes" title={notes}>{notes}</td>
              <td class="c-state" class:is-missing={st.missing}>{st.text}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {:else}
      <div class="cue-list-empty"><p>This show has no cues.</p></div>
    {/if}
  </main>
</div>
