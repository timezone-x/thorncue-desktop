<script lang="ts" module>
  /** One row of a menu. `disabled` is the reason, shown as the tooltip (DESIGN.md: never greyed out
   * with no reason). `note` is a muted word after the label ("no audio"). `dialog` means the item
   * opens a dialog, so focus isn't sent back to the button. */
  export type MenuEntry =
    | { label: string; onSelect: () => void; title?: string; disabled?: string; note?: string; dialog?: boolean }
    | { separator: true }
    | { info: string; code?: string };

  // Only one menu is open at a time (DESIGN.md "Menu behaviour"): opening one closes the other.
  let closeOpen: (() => void) | null = null;
</script>

<script lang="ts">
  // The one menu for the app (DESIGN.md "Header and menus" > "Menu behaviour"): Show ▾, Library
  // More ▾ and the inspector's Export ▾. Click only, never hover.
  //
  // Keys: Escape closes the menu and is left to reach the Show screen's handler, so Stop all
  // still fires. Space, Enter and the arrows are handled here and stopped, so they never reach GO
  // or move standby. Items are activated on keydown: the Show screen cancels Space's keyup (so a
  // focused button isn't clicked by GO), which would otherwise swallow the click.
  import { tick } from "svelte";

  let {
    label,
    items,
    id,
    small = false,
    buttonDisabled = false,
    buttonTitle,
    buttonLabel,
  }: {
    /** The button's text; the ▾ is added. */
    label: string;
    items: MenuEntry[];
    /** For the list's id (aria-controls). */
    id: string;
    small?: boolean;
    buttonDisabled?: boolean;
    buttonTitle?: string;
    /** Accessible name when the visible label isn't enough ("More actions for Hamlet"). */
    buttonLabel?: string;
  } = $props();

  let open = $state(false);
  let button = $state<HTMLButtonElement>();
  let list = $state<HTMLElement>();
  let pos = $state<{ top: number; left: number } | null>(null);

  const itemEls = () => [...(list?.querySelectorAll<HTMLElement>("[role=menuitem]") ?? [])];

  function close(refocus = false) {
    if (!open) return;
    open = false;
    pos = null;
    if (closeOpen === closeThis) closeOpen = null;
    if (refocus) button?.focus();
  }
  const closeThis = () => close();

  async function show(focus: "first" | "last" | null) {
    if (closeOpen && closeOpen !== closeThis) closeOpen();
    closeOpen = closeThis;
    open = true;
    await tick();
    place();
    await tick(); // visible now: a hidden item can't take focus
    if (focus) {
      const els = itemEls();
      (focus === "first" ? els[0] : els[els.length - 1])?.focus();
    } else list?.focus();
  }

  // Fixed to the viewport so a scrolling table or the inspector never clips it. Right edge on the
  // button's right edge; shifted to stay inside the window, flipped above when there's no room below.
  function place() {
    if (!button || !list) return;
    const b = button.getBoundingClientRect();
    const w = list.offsetWidth;
    const h = list.offsetHeight;
    const M = 8;
    let left = b.right - w;
    left = Math.max(M, Math.min(left, window.innerWidth - w - M));
    let top = b.bottom + 4;
    if (top + h > window.innerHeight - M && b.top - 4 - h >= M) top = b.top - 4 - h;
    top = Math.max(M, Math.min(top, window.innerHeight - h - M));
    pos = { top, left };
  }

  function activate(el: HTMLElement | null) {
    const i = el ? itemEls().indexOf(el) : -1;
    const entry = actionable[i];
    if (!entry || entry.disabled) return;
    close(!entry.dialog);
    entry.onSelect();
  }

  const actionable = $derived(items.filter((e): e is Extract<MenuEntry, { label: string }> => "label" in e));

  function listKey(e: KeyboardEvent) {
    const els = itemEls();
    const i = els.indexOf(document.activeElement as HTMLElement);
    switch (e.key) {
      case "Escape":
        // Not stopped: the Show screen's handler sees it and fires Stop all.
        close(true);
        return;
      case "Tab":
        close();
        return;
      case "ArrowDown":
        els[(i + 1) % els.length]?.focus();
        break;
      case "ArrowUp":
        els[(i - 1 + els.length) % els.length]?.focus();
        break;
      case "Home":
        els[0]?.focus();
        break;
      case "End":
        els[els.length - 1]?.focus();
        break;
      case " ":
      case "Enter":
        if (!e.repeat) activate(i >= 0 ? els[i] : null);
        break;
      default:
        return;
    }
    e.preventDefault();
    e.stopPropagation(); // never GO, never move standby
  }

  function buttonKey(e: KeyboardEvent) {
    if (e.key === "Escape") {
      close(); // and on to Stop all
      return;
    }
    if (e.key === "ArrowDown" || e.key === "ArrowUp" || e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      e.stopPropagation(); // Space here opens the menu, not GO
      if (e.repeat) return;
      if (open && (e.key === "Enter" || e.key === " ")) close();
      else void show(e.key === "ArrowUp" ? "last" : "first");
    }
  }

  $effect(() => {
    if (!open) return;
    const away = (e: PointerEvent) => {
      const t = e.target as Node;
      if (!button?.contains(t) && !list?.contains(t)) close();
    };
    const reflow = () => place();
    // Escape anywhere closes it too; the Show screen still gets the key (Stop all).
    const esc = (e: KeyboardEvent) => {
      if (e.key === "Escape") close();
    };
    window.addEventListener("keydown", esc);
    document.addEventListener("pointerdown", away, true);
    window.addEventListener("resize", reflow);
    window.addEventListener("scroll", reflow, true);
    return () => {
      window.removeEventListener("keydown", esc);
      document.removeEventListener("pointerdown", away, true);
      window.removeEventListener("resize", reflow);
      window.removeEventListener("scroll", reflow, true);
    };
  });

  // Closed if the component goes away while open (screen change).
  $effect(() => () => {
    if (closeOpen === closeThis) closeOpen = null;
  });
</script>

<span class="menu">
  <button type="button" class="btn" class:btn--small={small} bind:this={button} aria-haspopup="menu" aria-expanded={open}
    aria-controls={open ? id : undefined} aria-label={buttonLabel} disabled={buttonDisabled} title={buttonTitle}
    onkeydown={buttonKey} onclick={() => (open ? close() : show(null))}>
    {label} <span aria-hidden="true">▾</span>
  </button>
  {#if open}
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions (role=menu takes keys by design) -->
    <div class="menu__list" {id} role="menu" tabindex="-1" bind:this={list} onkeydown={listKey}
      style:top={pos ? `${pos.top}px` : undefined} style:left={pos ? `${pos.left}px` : undefined}
      style:visibility={pos ? undefined : "hidden"}>
      {#each items as entry, i (i)}
        {#if "separator" in entry}
          <div class="menu__sep" role="separator"></div>
        {:else if "info" in entry}
          <div class="menu__info" role="presentation">{entry.info}{#if entry.code}<span class="code">{entry.code}</span>{/if}</div>
        {:else}
          <button type="button" class="menu__item" role="menuitem" tabindex="-1" aria-disabled={entry.disabled ? "true" : undefined}
            title={entry.disabled ?? entry.title} onclick={(e) => activate(e.currentTarget)}>
            {entry.label}{#if entry.note}<small>{entry.note}</small>{/if}
          </button>
        {/if}
      {/each}
    </div>
  {/if}
</span>
