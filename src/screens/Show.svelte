<script lang="ts">
  // The Show screen (reference/web/show.html): header, transport, now-playing strip, cue table.
  // Everything that plays is decided in Rust; this screen sends intents and draws the engine's
  // snapshot. It never moves focus when state changes.
  import { onMount, tick, untrack } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
  import {
    AUDIO_EXTENSIONS,
    PIECE_EXTENSIONS,
    editApi,
    errorText,
    transport,
    type Cue,
    type CuePatch,
    type CuePrep,
    type EditResult,
    type Group,
    type Piece,
    type ShowView,
  } from "../lib/api";
  import { GROUP_COLORS, safeColor } from "../lib/groups";
  import GroupDialog from "../components/GroupDialog.svelte";
  import { act, engine, startEngineMirror } from "../lib/engine.svelte";
  import { fmtBytes, fmtTime, plural, rowNotes } from "../lib/format";
  import { showToast } from "../lib/toast.svelte";
  import { syncValue } from "../lib/sync";
  import ConfirmDialog from "../components/ConfirmDialog.svelte";
  import OutputsBar from "../components/OutputsBar.svelte";
  import Inspector from "../components/Inspector.svelte";

  let {
    show,
    onClose,
    onChanged,
    onOpenShow,
  }: {
    show: ShowView;
    onClose: () => void;
    onChanged: (v: ShowView) => void;
    onOpenShow: (id: string) => void;
  } = $props();

  let jumpDialog: HTMLDialogElement;
  let shortcutsDialog: HTMLDialogElement;
  let jumpValue = $state<number | null>(null);
  let jumpError = $state("");
  let listWrap: HTMLElement;

  const snap = $derived(engine.snap);
  const cues = $derived(show.cues);
  const standby = $derived(snap && snap.cue_count === cues.length ? snap.standby : show.local.standby_index);
  const runningByIndex = $derived(new Map((snap?.running ?? []).map((r) => [r.index, r.phase])));

  // ---- Editing ----

  let inspectedId = $state<string | null>(null);
  const inspectedIndex = $derived.by(() => {
    const i = inspectedId ? cues.findIndex((c) => c.id === inspectedId) : -1;
    return i >= 0 ? i : Math.min(standby, cues.length - 1);
  });
  const inspected = $derived(cues[inspectedIndex] ?? null);

  let saveState = $state<"saved" | "unsaved" | "error">("saved");
  let saveMessage = $state("");
  let confirmDelete = $state<{ cue: Cue; index: number } | null>(null);
  let inspector = $state<ReturnType<typeof Inspector>>();
  let inspectorHidden = $state(false);

  // ---- Groups (thorncue-spec#2; web static/app.js "Groups") ----
  // Rust owns membership and order; collapse is per machine (local.json); played counts are
  // this screen's, since the show opened or Back to top (as the web: per page load).

  const groups = $derived(show.groups ?? []);
  const groupById = $derived(new Map(groups.map((g) => [g.id, g])));
  const collapsed = $derived(new Set(show.local.collapsed_groups ?? []));
  const cueById = (id: string) => cues.find((c) => c.id === id);
  const cueIndex = (id: string) => cues.findIndex((c) => c.id === id);
  const membersOf = (gid: string) => cues.filter((c) => c.group === gid);

  type Block = { kind: "loose"; rows: number[] } | { kind: "group"; group: Group; rows: number[] };
  const blocks = $derived.by(() => {
    const out: Block[] = [];
    cues.forEach((c, i) => {
      const g = c.group ? groupById.get(c.group) : undefined;
      const last = out[out.length - 1];
      if (!g) {
        if (last?.kind === "loose") last.rows.push(i);
        else out.push({ kind: "loose", rows: [i] });
      } else if (last?.kind === "group" && last.group.id === g.id) last.rows.push(i);
      else out.push({ kind: "group", group: g, rows: [i] });
    });
    return out;
  });

  let played = $state(new Set<string>());
  $effect(() => {
    const running = snap?.running ?? [];
    untrack(() => {
      if (running.every((r) => played.has(r.id))) return;
      const next = new Set(played);
      for (const r of running) next.add(r.id);
      played = next;
    });
  });

  // Shift-click selects a range from the inspected (or standby) cue, as the web's selectRange.
  let selection = $state<string[]>([]);
  const selected = $derived(new Set(selection.filter((id) => cues.some((c) => c.id === id))));

  // Another show opened in this screen (Duplicate show): start its counts and selection afresh.
  let shownId = untrack(() => show.id);
  $effect(() => {
    if (show.id === shownId) return;
    shownId = show.id;
    played = new Set();
    selection = [];
  });

  function groupSummary(g: Group, rows: number[]): string {
    const n = rows.length;
    const runningRows = rows.filter((i) => runningByIndex.has(i));
    const playedN = rows.filter((i) => played.has(cues[i].id)).length;
    const parts: string[] = [];
    if (runningRows.length) {
      // The most recently started cue in the group, as the now-playing strip.
      const started = engine.positions.filter((p) => !p.audition && rows.includes(p.index));
      const i = started[started.length - 1]?.index ?? runningRows[runningRows.length - 1];
      parts.push(`Playing Q${i + 1}`);
    } else if (rows.includes(standby) && collapsed.has(g.id)) {
      parts.push(`Standby Q${standby + 1}`);
    }
    parts.push(playedN || runningRows.length ? `${playedN} of ${n} played` : plural(n, "cue"));
    return parts.join(" · ");
  }

  async function toggleGroup(id: string) {
    try {
      const list = await editApi.setGroupCollapsed(id, !collapsed.has(id));
      onChanged({ ...show, local: { ...show.local, collapsed_groups: list } });
    } catch (e) {
      showToast(errorText(e), true);
    }
  }

  function selectRange(to: number) {
    const fromId = inspectedId ?? cues[standby]?.id ?? cues[to]?.id;
    const from = fromId ? cueIndex(fromId) : to;
    const [a, b] = from <= to ? [from, to] : [to, from];
    selection = cues.slice(a, b + 1).map((c) => c.id);
  }

  function onRowClick(e: MouseEvent, i: number) {
    if (e.shiftKey) {
      e.preventDefault();
      window.getSelection()?.removeAllRanges();
      selectRange(i);
      return;
    }
    selection = [];
    selectRow(i);
  }

  type GroupDialogState =
    | { mode: "new"; ids: string[]; name: string; color: string; hint: string }
    | { mode: "edit"; group: Group; hint: string };
  let groupDialog = $state<GroupDialogState | null>(null);

  /** Grouping the middle of a group would split it in two (web `splitsAGroup`); Rust refuses it too. */
  function splitsAGroup(ids: string[]): Group | null {
    const idx = ids.map(cueIndex).sort((a, b) => a - b);
    const [lo, hi] = [idx[0], idx[idx.length - 1]];
    for (const g of groups) {
      const m = membersOf(g.id).map((c) => cueIndex(c.id));
      if (m.length && m[0] < lo && hi < m[m.length - 1]) return g;
    }
    return null;
  }

  function openGroupDialog(group?: Group) {
    if (group) {
      const n = membersOf(group.id).length;
      groupDialog = { mode: "edit", group, hint: `${plural(n, "cue")}. Drag the group's title bar to move them all.` };
      return;
    }
    const ids = selected.size ? cues.filter((c) => selected.has(c.id)).map((c) => c.id) : inspected ? [inspected.id] : [];
    if (!ids.length) {
      showToast("Select a cue first. Shift-click another cue to select a range.", true);
      return;
    }
    const split = splitsAGroup(ids);
    if (split) {
      showToast(`Those cues are in the middle of "${split.name}". Select from its start or end, or ungroup it first.`, true);
      return;
    }
    const used = new Set(groups.map((g) => g.color));
    const color = (GROUP_COLORS.find(([, hex]) => !used.has(hex)) ?? GROUP_COLORS[groups.length % GROUP_COLORS.length])[1];
    let n = groups.length + 1;
    while (groups.some((g) => g.name === `Group ${n}`)) n++;
    const first = cueIndex(ids[0]) + 1;
    const last = cueIndex(ids[ids.length - 1]) + 1;
    const hint =
      ids.length === 1
        ? `Groups Q${first}. Drag more cues into the group, or shift-click to select a range first.`
        : `Groups Q${first} to Q${last} (${ids.length} cues).`;
    groupDialog = { mode: "new", ids, name: `Group ${n}`, color, hint };
  }

  async function saveGroup(name: string, color: string) {
    const d = groupDialog;
    groupDialog = null;
    if (!d) return;
    if (d.mode === "new") {
      await edit(() => editApi.createGroup(d.ids, name, color));
      selection = [];
    } else {
      await edit(() => editApi.editGroup(d.group.id, name, color));
    }
  }

  async function ungroup(g: Group) {
    groupDialog = null;
    await edit(() => editApi.ungroup(g.id));
    showToast(`Ungrouped ${g.name}. The cues are still in the list.`);
  }

  onMount(() => {
    void startEngineMirror();
    let un: (() => void) | undefined;
    void listen<{ state: "saved" | "unsaved" | "error"; message: string | null }>("show://save", (e) => {
      saveState = e.payload.state;
      saveMessage = e.payload.message ?? "";
    }).then((f) => (un = f));
    return () => un?.();
  });

  /** Run an edit; show what Rust sends back. */
  async function edit(f: () => Promise<EditResult>) {
    try {
      const r = await f();
      onChanged(r.show);
      if (r.focus) inspectedId = r.focus;
      if (r.problems.length) showToast(r.problems.join(" "), true);
    } catch (e) {
      showToast(errorText(e), true);
    }
  }

  const updateCue = (id: string, patch: CuePatch) => edit(() => editApi.updateCue(id, patch));

  // Many files at once (thorncue-spec#4, web `addAudioFiles`): Rust copies them in natural
  // file-name order off the engine thread, skips any that fail and adds the rest in one edit;
  // decoding then happens on the prep pool, so GO is never waiting on any of it.
  let adding = $state(false);
  async function addAudio(paths: string[]) {
    const audio = paths.filter((p) => AUDIO_EXTENSIONS.includes(p.split(".").pop()?.toLowerCase() ?? ""));
    if (audio.length < paths.length) showToast("Only audio files (MP3, WAV, OGG, FLAC, M4A, AIFF) can be added as cues.", true);
    if (!audio.length) return;
    if (adding) {
      showToast("Still adding the last files. Try again when they're in.", true);
      return;
    }
    adding = true;
    if (audio.length > 1) showToast(`Adding ${audio.length} files…`);
    try {
      const before = cues.length;
      const r = await editApi.addAudioCues(audio);
      onChanged(r.show);
      if (r.focus) inspectedId = r.focus;
      const added = r.show.cues.length - before;
      // One toast, so a later "Added" never hides which files failed.
      if (r.problems.length) showToast(`Added ${added} of ${audio.length}. Couldn't add: ${r.problems.join(" ")}`, true);
      else showToast(added === 1 ? `Added Q${r.show.cues.length} "${r.show.cues[r.show.cues.length - 1].name}".` : `Added ${added} cues.`);
    } catch (e) {
      showToast(errorText(e), true);
    } finally {
      adding = false;
    }
  }

  async function pickAudio() {
    const picked = await openDialog({
      multiple: true,
      title: "Add cue",
      filters: [{ name: "Audio", extensions: AUDIO_EXTENSIONS }],
    });
    if (picked) await addAudio(Array.isArray(picked) ? picked : [picked]);
  }

  async function replaceFile(id: string) {
    const picked = await openDialog({ multiple: false, title: "Replace file", filters: [{ name: "Audio", extensions: AUDIO_EXTENSIONS }] });
    if (typeof picked === "string") await edit(() => editApi.replaceCueFile(id, picked));
  }

  async function doDelete() {
    const c = confirmDelete;
    confirmDelete = null;
    if (c) await edit(() => editApi.deleteCue(c.cue.id));
  }

  async function duplicateShow() {
    try {
      const id = await editApi.duplicateShow(show.id);
      showToast("Duplicated. You're now in the copy.");
      onOpenShow(id);
    } catch (e) {
      showToast(errorText(e), true);
    }
  }

  async function exportShow() {
    try {
      const name = await editApi.exportFileName(show.id);
      const path = await saveDialog({ title: "Export show", defaultPath: name, filters: [{ name: "ThornCue show", extensions: ["tqs"] }] });
      if (!path) return;
      showToast("Exporting…");
      await editApi.exportShow(show.id, path);
      showToast(`Exported to ${path.split(/[\\/]/).pop()}.`);
    } catch (e) {
      showToast(errorText(e), true);
    }
  }

  // ---- Dragging (web app.js "Dragging", on pointer events: HTML5 drag-and-drop is unavailable
  // in the webview on Windows while native file drops are on). A cue by its grip; a whole group
  // by its title bar. Drop on the top or bottom half of a row to land before or after it; a cue
  // joins the group of the row it lands next to. On a group's title bar: top half = just above
  // the group, ungrouped; bottom half = first cue in the group. Empty space below = the end.

  type Drag = { kind: "cue" | "group"; id: string };
  type Target = { end: true } | { cue: string; after: boolean } | { group: string; after: boolean };
  type Plan = { ids: string[]; anchor: string; after: boolean; group?: string | null };
  type Mark = { type: "row" | "group"; id: string; after: boolean } | { type: "head"; id: string; after?: undefined };
  let drag = $state<Drag | null>(null);
  let mark = $state<Mark | null>(null);

  function dropTarget(x: number, y: number): Target | null {
    const el = document.elementFromPoint(x, y);
    if (!el || !listWrap.contains(el) || el.closest(".cue-table--head")) return null;
    const tr = el.closest<HTMLElement>("tr[data-id]");
    const head = tr ? null : el.closest<HTMLElement>(".cue-group__head");
    if (tr) {
      const r = tr.getBoundingClientRect();
      return { cue: tr.dataset.id!, after: y > r.top + r.height / 2 };
    }
    if (head) {
      const r = head.getBoundingClientRect();
      return { group: head.closest<HTMLElement>(".cue-group")!.dataset.group!, after: y > r.top + r.height / 2 };
    }
    return el.closest(".cue-group") ? null : { end: true };
  }

  function planDrop(d: Drag, t: Target): Plan | null {
    const last = cues[cues.length - 1];
    if (!last) return null;
    if (d.kind === "cue") {
      const c = cueById(d.id);
      if (!c) return null;
      if ("end" in t) return { ids: [c.id], anchor: last.id, after: true, group: null };
      if ("cue" in t) {
        if (t.cue === c.id) return null;
        return { ids: [c.id], anchor: t.cue, after: t.after, group: cueById(t.cue)?.group ?? null };
      }
      const first = membersOf(t.group).find((x) => x.id !== c.id);
      if (!first) return t.after ? null : { ids: [c.id], anchor: c.id, after: false, group: null };
      return { ids: [c.id], anchor: first.id, after: false, group: t.after ? t.group : null };
    }
    const ids = membersOf(d.id).map((c) => c.id);
    if (!ids.length) return null;
    if ("end" in t) return { ids, anchor: last.id, after: true };
    const gid = "cue" in t ? (cueById(t.cue)?.group ?? null) : t.group;
    if (gid === d.id) return null;
    if (!gid) return "cue" in t ? { ids, anchor: t.cue, after: t.after } : null;
    const m = membersOf(gid);
    if ("group" in t) return { ids, anchor: m[0].id, after: false };
    return t.after ? { ids, anchor: m[m.length - 1].id, after: true } : { ids, anchor: m[0].id, after: false };
  }

  /** Where the drop will land: beside a row, or beside a whole group when the moved cues end up
   * outside it (or it is collapsed), as the web's markDrop. */
  function markFor(d: Drag, p: Plan | null): Mark | null {
    if (!p) return null;
    const ag = cueById(p.anchor)?.group ?? null;
    if (ag && (d.kind === "group" || p.group !== ag)) return { type: "group", id: ag, after: p.after };
    if (ag && collapsed.has(ag)) return { type: "head", id: ag };
    return { type: "row", id: p.anchor, after: p.after };
  }

  function startDrag(e: PointerEvent, d: Drag) {
    if (e.button !== 0 || (e.target as HTMLElement).closest("button")) return;
    e.preventDefault();
    e.stopPropagation();
    const grip = e.currentTarget as HTMLElement;
    grip.setPointerCapture(e.pointerId);
    const [x0, y0] = [e.clientX, e.clientY];
    let plan: Plan | null = null;
    const move = (ev: PointerEvent) => {
      if (!drag && Math.hypot(ev.clientX - x0, ev.clientY - y0) < 4) return;
      if (!drag) {
        drag = d;
        selection = [];
      }
      const t = dropTarget(ev.clientX, ev.clientY);
      plan = t ? planDrop(d, t) : null;
      mark = markFor(d, plan);
    };
    const finish = (commit: boolean) => {
      grip.removeEventListener("pointermove", move);
      grip.removeEventListener("pointerup", onUp);
      grip.removeEventListener("pointercancel", onCancel);
      const p = commit && drag ? plan : null;
      drag = null;
      mark = null;
      if (p) void edit(() => editApi.moveCues(p.ids, p.anchor, p.after, d.kind === "cue" ? p.group : undefined));
    };
    const onUp = () => finish(true);
    const onCancel = () => finish(false);
    grip.addEventListener("pointermove", move);
    grip.addEventListener("pointerup", onUp);
    grip.addEventListener("pointercancel", onCancel);
  }

  // Sent on every keystroke, like cue names: Rust debounces the save, not the UI.
  function renameShow(value: string) {
    void edit(() => editApi.setShowName(value.slice(0, 100)));
  }

  function selectRow(i: number) {
    inspectedId = cues[i]?.id ?? null;
    void act(() => transport.setStandby(i));
  }

  /** Dropped files on the Show screen: audio becomes cues, .tqc/.tqg are imported (App routes
   * drops here). */
  export function dropFiles(paths: string[]) {
    if (paths.some((p) => /\.(tqs|zip)$/i.test(p))) {
      showToast("Import shows from the Library.", true);
      return;
    }
    const pieces = paths.filter((p) => /\.(tqc|tqg)$/i.test(p));
    const rest = paths.filter((p) => !/\.(tqc|tqg)$/i.test(p));
    if (pieces.length) void importPieces(pieces);
    if (rest.length) void addAudio(rest);
  }

  // ---- .tqc / .tqg (thorncue-spec#3; web importPieces, build_package) ----

  /** Add cue and group files at the end of the show (FORMAT.md 6.4). Each file is checked whole
   * in Rust first; a bad one is skipped and named. Standby doesn't move. */
  async function importPieces(paths: string[]) {
    showToast(paths.length === 1 ? `Importing ${paths[0].split(/[\\/]/).pop()}…` : `Importing ${paths.length} files…`);
    try {
      const r = await editApi.importPieces(paths);
      onChanged(r.show);
      const done = r.added.map((a) =>
        a.kind === "group" ? `group "${a.group}" (${plural(a.cues, "cue")}) from ${a.file}` : `${plural(a.cues, "cue")} from ${a.file}`,
      );
      const text = [done.length ? `Added ${done.join("; ")}.` : "", ...r.problems].filter(Boolean).join(" ");
      showToast(text, r.problems.length > 0);
    } catch (e) {
      showToast(errorText(e), true);
    }
  }

  async function pickPieces() {
    const picked = await openDialog({
      multiple: true,
      title: "Import cues",
      filters: [{ name: "ThornCue cues or group", extensions: PIECE_EXTENSIONS }],
    });
    if (picked) await importPieces(Array.isArray(picked) ? picked : [picked]);
  }

  async function exportPiece(piece: Piece) {
    try {
      const name = await editApi.pieceFileName(piece);
      const isGroup = !!piece.group;
      const path = await saveDialog({
        title: isGroup ? "Export group" : "Export cues",
        defaultPath: name,
        filters: [isGroup ? { name: "ThornCue group", extensions: ["tqg"] } : { name: "ThornCue cues", extensions: ["tqc"] }],
      });
      if (!path) return;
      await editApi.exportPiece(piece, path);
      showToast(`Exported to ${path.split(/[\\/]/).pop()}.`);
    } catch (e) {
      showToast(errorText(e), true);
    }
  }

  /** As the web's exportCueIds: the shift-selected range, else the inspected cue. */
  const exportIds = $derived(selected.size > 1 ? cues.filter((c) => selected.has(c.id)).map((c) => c.id) : inspected ? [inspected.id] : []);

  // Keep the standby row in view, instantly (DESIGN.md: no smooth scroll during a show).
  $effect(() => {
    const i = standby;
    void tick().then(() => {
      // In a collapsed group the row isn't drawn: bring its title bar into view instead.
      const row = listWrap?.querySelector<HTMLElement>(`tr[data-index="${i}"]`);
      const target = row?.offsetParent ? row : row?.closest(".cue-group")?.querySelector<HTMLElement>(".cue-group__head");
      target?.scrollIntoView({ block: "nearest" });
    });
  });

  // ---- Transport text ----

  const standbyName = $derived(
    cues[standby] ? `Q${standby + 1} ${cues[standby].name}` : cues.length ? "End of cue list" : "No cues",
  );

  const status = $derived.by((): { text: string; kind: "" | "running" | "paused" | "error" } => {
    if (!snap) return { text: "Starting…", kind: "" };
    if (snap.status === "restarting") return { text: "Restarting audio engine", kind: "paused" };
    if (snap.status === "device_lost") return { text: "Device lost", kind: "error" };
    if (snap.status === "no_output") return { text: "No audio output", kind: "error" };
    if (snap.paused) return { text: "Paused", kind: "paused" };
    const n = snap.running.length;
    if (n > 0) return { text: n === 1 ? "Running" : `Running ${n} cues`, kind: "running" };
    if (snap.prep_done < snap.prep_total) return { text: `Loading ${snap.prep_done} of ${snap.prep_total}`, kind: "" };
    return { text: "Ready", kind: "" };
  });

  const loadingNote = $derived(
    snap && snap.prep_done < snap.prep_total && (snap.running.length > 0 || snap.paused)
      ? `Loading ${snap.prep_done} of ${snap.prep_total}`
      : "",
  );

  // ---- Now playing: the most recently started cue, as the web ----

  const nowPlaying = $derived.by(() => {
    // The most recently started audio cue; a silence/fade-out cue only when nothing else plays.
    const cuesOnly = engine.positions.filter((p) => !p.audition);
    const audio = cuesOnly.filter((p) => !p.control);
    const p = audio[audio.length - 1] ?? cuesOnly[cuesOnly.length - 1];
    if (!p) {
      const r = snap?.running[snap.running.length - 1];
      if (r) return { name: `Q${r.index + 1} ${cues[r.index]?.name ?? ""}`, times: r.phase === "starting" ? "Loading audio…" : "", fraction: 0 };
      return null;
    }
    const phase = snap?.running.find((r) => r.index === p.index)?.phase ?? "running";
    const cue = cues[p.index];
    const name = `Q${p.index + 1} ${cue?.name ?? ""}${phase === "fading" ? " (fading out)" : ""}`;
    let times: string;
    if (phase === "waiting") times = `Waiting (pre-wait ${cue?.pre_wait ?? 0} s)`;
    else if (p.control) times = `${fmtTime(p.remaining ?? 0, true)} left`;
    else if (p.remaining === null) times = `${fmtTime(p.elapsed % Math.max(p.length, 0.001))} of ${fmtTime(p.length)}, looping`;
    else times = `${fmtTime(p.elapsed)} of ${fmtTime(p.length)}, ${fmtTime(p.remaining)} left`;
    const into = p.remaining === null ? p.elapsed % Math.max(p.length, 0.001) : p.elapsed;
    const fraction = p.length > 0 ? Math.min(1, into / p.length) : 0;
    return { name, times, fraction };
  });

  // ---- Rows ----

  function lengthText(i: number): string {
    const c = cues[i];
    if (c.type !== "audio") return fmtTime(c.duration);
    const len = snap?.prep[i]?.length;
    return len == null ? "" : fmtTime(len);
  }

  function prepText(p: CuePrep | undefined, hasAsset: boolean): { text: string; missing: boolean; title?: string } {
    if (!hasAsset) return { text: "Missing", missing: true, title: "This cue has no audio file." };
    if (!p) return { text: "", missing: false };
    switch (p.state) {
      case "pending":
      case "decoding":
        return { text: "Loading", missing: false };
      case "ready":
        return { text: "Ready", missing: false };
      case "missing":
        return { text: "Missing", missing: true, title: "The audio file isn't in the show folder." };
      case "failed":
        return { text: "Can't decode", missing: true, title: p.reason };
      default:
        return { text: "", missing: false };
    }
  }

  const PHASE_TEXT = { starting: "Loading", waiting: "Waiting", running: "Running", fading: "Fading" } as const;

  function rowState(i: number) {
    const phase = runningByIndex.get(i);
    if (phase) return { text: PHASE_TEXT[phase], missing: false, running: true, title: undefined };
    const c = cues[i];
    if (c.type !== "audio") return { text: "", missing: false, running: false, title: undefined };
    return { ...prepText(snap?.prep[i], c.asset !== null), running: false };
  }

  // ---- Actions ----

  const go = () => act(transport.go);
  let lastEsc = 0;
  function stopAll() {
    // Double Esc within 500 ms: panic (no fade, even when a stop fade is set).
    const now = performance.now();
    const panic = now - lastEsc < 500;
    lastEsc = now;
    void act(() => transport.stopAll(panic));
  }
  const togglePause = () => act(transport.togglePause);
  const backToTop = () => {
    played = new Set();
    return act(() => transport.setStandby(0));
  };
  const restart = () => act(transport.restart);
  // The slider sends every move; the engine ramps 20 ms per change so it never clicks.
  const setMaster = (v: number) => act(() => transport.setMaster(v));

  function openJump() {
    jumpValue = null;
    jumpError = "";
    jumpDialog.showModal();
  }

  function submitJump(e: SubmitEvent) {
    e.preventDefault();
    const n = jumpValue ?? NaN;
    if (!Number.isInteger(n) || n < 1 || n > cues.length) {
      jumpError = `There is no Q${jumpValue ?? "?"}. This show has ${plural(cues.length, "cue")}.`;
      return;
    }
    jumpDialog.close();
    void act(() => transport.jump(n));
  }

  // ---- Keyboard (reference/web/app.js L1842-1895). Space is GO outside text fields. ----

  const isTyping = (t: EventTarget | null) =>
    t instanceof HTMLElement &&
    (t.isContentEditable ||
      t.matches("textarea, select, input:not([type=checkbox]):not([type=range]):not([type=button]):not([type=file])"));

  function onKeydown(e: KeyboardEvent) {
    const t = e.target;
    // Stop all always wins (CLAUDE.md rule 3): Esc stops everything even with a dialog open
    // (the dialog closes too). The web only closed the dialog.
    if (e.key === "Escape") {
      if (isTyping(t)) (t as HTMLElement).blur();
      stopAll();
      return;
    }
    if (document.querySelector("dialog[open]")) return;
    if (isTyping(t)) return;
    if ((e.ctrlKey || e.metaKey) && !e.shiftKey && e.key.toLowerCase() === "z") {
      e.preventDefault();
      inspector?.trimKey("undo");
      return;
    }
    if (e.ctrlKey || e.metaKey || e.altKey) return;
    const onRange = t instanceof HTMLElement && t.matches("input[type=range]");
    switch (e.key) {
      case " ":
        e.preventDefault();
        if (!e.repeat) void go();
        break;
      case "ArrowDown":
        if (onRange) return;
        e.preventDefault();
        inspectedId = cues[Math.min(standby + 1, cues.length - 1)]?.id ?? null;
        void act(() => transport.moveStandby(1));
        break;
      case "ArrowUp":
        if (onRange) return;
        e.preventDefault();
        inspectedId = cues[Math.max(standby - 1, 0)]?.id ?? null;
        void act(() => transport.moveStandby(-1));
        break;
      case "p":
      case "P":
        void togglePause();
        break;
      case "j":
      case "J":
        e.preventDefault();
        openJump();
        break;
      case "?":
        shortcutsDialog.showModal();
        break;
      case "\\":
        inspector?.toggle();
        break;
      case "g":
      case "G":
        e.preventDefault();
        openGroupDialog();
        break;
      case "i":
      case "I":
        inspector?.trimKey("in");
        break;
      case "o":
      case "O":
        inspector?.trimKey("out");
        break;
      case "x":
      case "X":
        inspector?.trimKey("cut");
        break;
    }
  }

  // Stop Space from also clicking whichever button or checkbox has focus.
  function onKeyupCapture(e: KeyboardEvent) {
    if (e.key === " " && !isTyping(e.target) && !document.querySelector("dialog[open]")) e.preventDefault();
  }
</script>

<svelte:window onkeydown={onKeydown} />
<svelte:document onkeyupcapture={onKeyupCapture} />

<div class="page-show">
  <header class="show-head">
    <button type="button" class="btn" onclick={onClose}>Library</button>
    <label for="show-name" class="sr-only">Show name</label>
    <input type="text" id="show-name" class="show-head__name" maxlength="100" autocomplete="off" use:syncValue={show.name}
      oninput={(e) => renameShow(e.currentTarget.value)} />
    {#if show.source}<span class="btn show-head__code" title="ThornCue Web show code">{show.source.code}</span>{/if}
    <label class="master" for="master-vol">Master
      <input type="range" id="master-vol" min="0" max="1" step="0.01" value={snap?.master ?? 1}
        oninput={(e) => setMaster(Number(e.currentTarget.value))} />
      <output class="num" for="master-vol">{Math.round((snap?.master ?? 1) * 100)}%</output>
    </label>
    {#if snap && snap.decoded_bytes > 0}<span class="muted num" title="Decoded audio held in memory">{fmtBytes(snap.decoded_bytes)} in memory</span>{/if}
    <span class="save-state" class:status-error={saveState === "error"} aria-live="polite" title={saveMessage}>
      {#if saveState === "unsaved"}Saving…{:else if saveState === "error"}Couldn't save{/if}
    </span>
    <button type="button" class="btn" aria-controls="inspector" aria-pressed={!inspectorHidden} aria-keyshortcuts={"\\"}
      title={inspectorHidden ? "Show inspector (\\)" : "Hide inspector (\\)"} onclick={() => inspector?.toggle()}>Inspector</button>
    <button type="button" class="btn" aria-keyshortcuts="?" onclick={() => shortcutsDialog.showModal()}>Shortcuts</button>
    <button type="button" class="btn" onclick={exportShow}>Export .tqs</button>
    <button type="button" class="btn" onclick={duplicateShow}>Duplicate show</button>
  </header>

  <OutputsBar />

  <section class="transport" aria-label="Transport">
    <button type="button" class="transport__go" aria-keyshortcuts="Space" onclick={go}>GO</button>
    <button type="button" class="transport__stop" aria-keyshortcuts="Escape" onclick={stopAll}>Stop all</button>
    <div class="transport__more">
      <button type="button" class="btn" aria-keyshortcuts="P" aria-pressed={snap?.paused ?? false} onclick={togglePause}>
        {snap?.paused ? "Resume" : "Pause"}
      </button>
      <button type="button" class="btn" onclick={backToTop}>Back to top</button>
      <button type="button" class="btn" class:btn--danger={snap?.status === "device_lost" || snap?.status === "no_output"}
        title="Stops everything and reopens the audio output. Use it after an interface is unplugged."
        disabled={snap?.status === "restarting"} onclick={restart}>Restart audio engine</button>
    </div>
    <div class="standby">
      <span class="standby__label">Standby</span>
      <span class="standby__name">{standbyName}</span>
    </div>
    <span class="transport__status" class:is-running={status.kind === "running"} class:is-paused={status.kind === "paused"}
      class:is-error={status.kind === "error"} aria-live="polite">
      {status.text}{#if loadingNote}<span class="muted"> · {loadingNote}</span>{/if}
    </span>
  </section>

  {#if snap?.status_message}
    <div class="flash flash--error device-alert" role="alert">{snap.status_message}</div>
  {/if}
  {#if show.recovered}
    <div class="flash flash--error recovered" role="alert">{show.recovered}</div>
  {/if}

  <div class="now-playing">
    <span class="now-playing__name">{nowPlaying?.name ?? "Nothing running"}</span>
    <div class="np-bar" aria-hidden="true"><div class="np-bar__fill" style:width={`${(nowPlaying?.fraction ?? 0) * 100}%`}></div></div>
    <span class="now-playing__times num">{nowPlaying?.times ?? ""}</span>
  </div>

  <div class="cue-toolbar">
    <button type="button" class="btn btn--primary" disabled={adding} onclick={pickAudio}>{adding ? "Adding…" : "Add cue"}</button>
    <button type="button" class="btn" onclick={() => edit(() => editApi.addControlCue(true))}>Add silence</button>
    <button type="button" class="btn" onclick={() => edit(() => editApi.addControlCue(false))}>Add fade-out</button>
    <button type="button" class="btn" aria-keyshortcuts="G" title="Put the selected cues in a group (G). Shift-click a cue to select a range."
      onclick={() => openGroupDialog()}>Group</button>
    <button type="button" class="btn" title="Add cues from a .tqc (cues) or .tqg (group) file. You can also drop them on the cue list."
      onclick={pickPieces}>Import cues</button>
    <span class="cue-toolbar__selected" aria-live="polite">{selected.size > 1 ? `${selected.size} selected` : ""}</span>
    <span class="cue-toolbar__count num">{plural(cues.length, "cue")}</span>
  </div>

  {#snippet row(i: number)}
    {@const c = cues[i]}
    {@const notes = rowNotes(c)}
    {@const st = rowState(i)}
    <tr class="cue-row" data-index={i} data-id={c.id} class:is-standby={i === standby} class:is-running={st.running}
      class:is-inspected={i === inspectedIndex} class:is-selected={selected.has(c.id)}
      class:is-dragging={drag?.kind === "cue" && drag.id === c.id}
      class:is-drop-before={mark?.type === "row" && mark.id === c.id && !mark.after}
      class:is-drop-after={mark?.type === "row" && mark.id === c.id && mark.after}
      onclick={(e) => onRowClick(e, i)}>
      <td class="c-grip" title="Drag to reorder" aria-hidden="true" onpointerdown={(e) => startDrag(e, { kind: "cue", id: c.id })}
        onclick={(e) => e.stopPropagation()}>⠿</td>
      <td class="c-q num">{i + 1}</td>
      <td class="c-name" title={c.name}>{c.name}</td>
      <td class="c-dur num">{lengthText(i)}</td>
      <td class="c-vol num">{c.type === "audio" ? `${Math.round(c.volume * 100)}%` : ""}</td>
      <td class="c-notes" title={notes}>{notes}</td>
      <td class="c-state" class:is-missing={st.missing} class:muted={st.text === "Ready" || st.text === "Loading"} title={st.title}>{st.text}</td>
    </tr>
  {/snippet}

  <main class="cue-list-wrap" bind:this={listWrap}>
    {#if cues.length}
      <table class="cue-table cue-table--head">
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
      </table>
      <div class="cue-list">
        {#each blocks as b (b.kind === "group" ? `g:${b.group.id}` : `l:${cues[b.rows[0]].id}`)}
          {#if b.kind === "loose"}
            <table class="cue-table"><tbody>{#each b.rows as i (cues[i].id)}{@render row(i)}{/each}</tbody></table>
          {:else}
            {@const g = b.group}
            {@const isCol = collapsed.has(g.id)}
            <section class="cue-group" data-group={g.id} aria-label={`Group: ${g.name}`} style:--group-color={safeColor(g.color)}
              class:is-collapsed={isCol} class:is-running={b.rows.some((i) => runningByIndex.has(i))}
              class:is-standby={isCol && b.rows.includes(standby)}
              class:is-dragging={drag?.kind === "group" && drag.id === g.id}
              class:is-drop-before={mark?.type === "group" && mark.id === g.id && !mark.after}
              class:is-drop-after={mark?.type === "group" && mark.id === g.id && mark.after}>
              <!-- svelte-ignore a11y_no_static_element_interactions -->
              <div class="cue-group__head" class:is-drop-after={mark?.type === "head" && mark.id === g.id}
                title="Drag the title bar to move the whole group" onpointerdown={(e) => startDrag(e, { kind: "group", id: g.id })}>
                <button type="button" class="cue-group__toggle" aria-expanded={!isCol} title={isCol ? "Expand group" : "Collapse group"}
                  onclick={() => toggleGroup(g.id)}>
                  <span class="cue-group__chev" aria-hidden="true"></span>
                  <span class="cue-group__name">{g.name}</span>
                </button>
                <span class="cue-group__fill" aria-hidden="true"></span>
                <span class="cue-group__summary num">{groupSummary(g, b.rows)}</span>
                <button type="button" class="cue-group__edit" title={`Rename, recolour or ungroup ${g.name}`}
                  onclick={() => openGroupDialog(g)}>Edit</button>
              </div>
              <div class="cue-group__body" hidden={isCol}>
                <table class="cue-table"><tbody>{#each b.rows as i (cues[i].id)}{@render row(i)}{/each}</tbody></table>
              </div>
            </section>
          {/if}
        {/each}
      </div>
    {:else}
      <div class="cue-list-empty">
        <p>This show has no cues.</p>
        <p><button type="button" class="btn btn--primary" onclick={pickAudio}>Add a cue</button></p>
        <p class="hint">Or drop audio files onto this window.</p>
      </div>
    {/if}
  </main>

  <Inspector bind:this={inspector} bind:hidden={inspectorHidden} cue={inspected} index={inspectedIndex} {groups} onEdit={updateCue}
    onSetGroup={(id, group) => edit(() => editApi.setCueGroup(id, group))}
    exportCount={exportIds.length} onExport={() => exportPiece({ ids: exportIds })}
    onDuplicate={(id) => edit(() => editApi.duplicateCue(id))} onReplace={replaceFile}
    onDelete={(cue, index) => (confirmDelete = { cue, index })} />
</div>

{#if groupDialog}
  {@const d = groupDialog}
  {#if d.mode === "new"}
    <GroupDialog mode="new" initialName={d.name} initialColor={d.color} hint={d.hint} onSave={saveGroup}
      onCancel={() => (groupDialog = null)} />
  {:else}
    <GroupDialog mode="edit" initialName={d.group.name} initialColor={d.group.color} hint={d.hint} onSave={saveGroup}
      onUngroup={() => ungroup(d.group)} onCancel={() => (groupDialog = null)}
      onExport={() => { groupDialog = null; void exportPiece({ group: d.group.id }); }} />
  {/if}
{/if}

{#if confirmDelete}
  {@const d = confirmDelete}
  <ConfirmDialog
    title="Delete this cue?"
    message={`Q${d.index + 1} "${d.cue.name}" will be removed from the show.`}
    confirmLabel="Delete cue"
    danger
    onConfirm={doDelete}
    onCancel={() => (confirmDelete = null)}
  />
{/if}

<dialog bind:this={shortcutsDialog} aria-labelledby="shortcuts-title">
  <div class="dialog-head">
    <h2 id="shortcuts-title">Keyboard shortcuts</h2>
    <button type="button" class="btn" onclick={() => shortcutsDialog.close()}>Close</button>
  </div>
  <div class="dialog-body">
    <table class="shortcuts">
      <tbody>
        <tr><td><kbd>Space</kbd></td><td>GO: play the standby cue</td></tr>
        <tr><td><kbd>Esc</kbd></td><td>Stop all</td></tr>
        <tr><td><kbd>Esc</kbd> <kbd>Esc</kbd></td><td>Panic: stop all at once, with no fade</td></tr>
        <tr><td><kbd>P</kbd></td><td>Pause or resume everything</td></tr>
        <tr><td><kbd>↑</kbd> <kbd>↓</kbd></td><td>Move standby up or down</td></tr>
        <tr><td><kbd>J</kbd></td><td>Jump to a cue number and play it</td></tr>
        <tr><td><kbd>&#92;</kbd></td><td>Show or hide the inspector</td></tr>
        <tr><td><kbd>G</kbd></td><td>Group the selected cues (shift-click a cue to select a range)</td></tr>
        <tr><td><kbd>I</kbd> <kbd>O</kbd></td><td>Set the in or out point at the marker (or the audition playhead)</td></tr>
        <tr><td><kbd>X</kbd></td><td>Cut the selected section</td></tr>
        <tr><td><kbd>Ctrl</kbd> <kbd>Z</kbd></td><td>Undo the last trim or cut</td></tr>
        <tr><td><kbd>?</kbd></td><td>This list</td></tr>
      </tbody>
    </table>
  </div>
</dialog>

<dialog bind:this={jumpDialog} aria-labelledby="jump-title">
  <form onsubmit={submitJump}>
    <div class="dialog-head"><h2 id="jump-title">Jump to cue</h2></div>
    <div class="dialog-body">
      <div class="field">
        <label for="jump-input">Cue number</label>
        <!-- svelte-ignore a11y_autofocus -->
        <input type="number" id="jump-input" min="1" step="1" required autofocus bind:value={jumpValue}
          aria-describedby="jump-error" />
        {#if jumpError}<p class="hint status-error" id="jump-error" role="alert">{jumpError}</p>{/if}
      </div>
    </div>
    <div class="dialog-foot">
      <button type="button" class="btn" onclick={() => jumpDialog.close()}>Cancel</button>
      <button type="submit" class="btn btn--primary">Jump and play</button>
    </div>
  </form>
</dialog>
