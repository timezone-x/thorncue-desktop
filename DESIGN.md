# DESIGN.md — ThornCue

Ported from ThornCue Web's DESIGN.md (`reference/web/DESIGN.md`). This file is the contract for every screen of the desktop app; change it here first. Web-only parts (accounts, sharing, admin) are removed.

## Subject
- Product / primary job: fire sound cues in order during a live show. Secondary: build and trim the cue list before the show, and bring shows in from a file or from ThornCue Web.
- Audience: sound operators and stage managers on a laptop in a dim control booth or at the side of the stage, during a performance. One hand near the space bar, eyes mostly on the stage. Designers prepping cues at a desk earlier in the week.
- Genre: theatre playback software (a cue stack with a GO button and an inspector for the selected cue) plus the cue light on the wall: standby, then go.
- Real artefacts used: cue-light convention (green = go), the prompt-book habit of numbering cues as Q1, Q2.

## Themes (family + mode)
- Eight themes: four families, each with a dark and a light variant. All token values live in `src/styles/tokens.css` (copied verbatim from the web's `style.css`) as one block per theme, selected by `data-theme="{family}-{dark|light}"` on `<html>`. Graphite dark is the default (`:root`).
- Two settings: `theme_family` (`graphite` | `ghost-light` | `booth` | `thorn`, default `graphite`) and `theme_mode` (`dark` | `light` | `system`, default `dark`). `system` follows `prefers-color-scheme` and switches live when the OS flips.
- `settings.json` (written by Rust) is the source of truth. Rust injects the theme into the window before the first paint; the webview caches it in localStorage under `thorncue.theme` (`{family, mode}`) only so a reload paints in the right colours. Changing theme never touches playback or focus.
- Every theme sets the same tokens, so components never branch on theme. Every pairing was checked for WCAG AA: text 4.5:1, white on GO/STOP/primary 4.5:1, control borders and focus/select line 3:1. Treat the values as final.
- GO stays cue-light green and STOP stays red in every family. Families change only the neutrals and the select colour, never what GO and STOP mean.

| Family | Why it exists | Dark id | Light id |
|---|---|---|---|
| Graphite | The default. Neutral grey like desk hardware, OS-selection blue. | `graphite-dark` | `graphite-light` |
| Ghost Light | Warm neutrals and amber select, like the lamp left on an empty stage. The light variant is labelled "Work Light" in the UI. | `ghost-light-dark` | `ghost-light-light` |
| Booth | Cool blue-grey neutrals and cyan select, like a lit console. | `booth-dark` | `booth-light` |
| Thorn | Plum-tinted neutrals and violet select, the product's own colour. | `thorn-dark` | `thorn-light` |

### Roles (reasons, Graphite dark values)
| Role | Graphite dark | Reason |
|---|---|---|
| bg | `#161616` | Booth is dark; a bright screen spills light onto the set and ruins the operator's night vision. |
| surface | `#1E1E1E` | Cue list and inspector panes. |
| chrome | `#262626` | Header, transport, toolbars. One tonal step above surface. |
| control / control-hover | `#2B2B2B` / `#353535` | Buttons and inputs. |
| border | `#333333` | Row separators (decorative). |
| border-control | `#787878` | Input and button outlines, ≥3:1 against surface. |
| text | `#E6E6E6` | 13.4:1 on surface. Off-white, not pure white, to cut glare. |
| text-muted | `#A6A6A6` | ≥5:1 on every surface including the selected row. |
| select (permitted: standby row, focus ring, links, primary non-GO buttons) | `#24364A` fill, `#7DB4F0` line/text, `#2F6FB5` button | The colour the operator reads as "this one". The standby cue is the selected cue. |
| go | `#1F7A3E` button, `#6CC788` text, `#1C3324` running row | Cue-light green. Only GO and running cues use it. |
| stop | `#B3362D` button, `#F08A80` text, `#3A1F1C` fill | Stop, delete and errors. |
| warning | `#E6B35A` | Missing audio, unsaved changes, plain-http warnings. Text only. |
| on-accent | `#FFFFFF` | Text on GO, STOP and primary buttons. |
| wave / wave-off | `#8C8C8C` / `#454545` | Waveform peaks; trimmed or cut-out audio. The canvas reads these from CSS and redraws on theme change. |
| select-wash | select-line at 22% (18% in light themes) | Trim selection over the waveform. Keep in step with select-line. |
| shadow / backdrop | black at 40–60% (8–35% in light themes) | Dialog and toast elevation; dialog backdrop. |

### Key values per theme
| Theme | bg | surface | chrome | text | select line / button | go / go-text | stop / stop-text | warning | wave / wave-off |
|---|---|---|---|---|---|---|---|---|---|
| graphite-dark | `#161616` | `#1E1E1E` | `#262626` | `#E6E6E6` | `#7DB4F0` / `#2F6FB5` | `#1F7A3E` / `#6CC788` | `#B3362D` / `#F08A80` | `#E6B35A` | `#8C8C8C` / `#454545` |
| graphite-light | `#F4F4F4` | `#FFFFFF` | `#E9E9E9` | `#1C1C1C` | `#2563A8` / `#2F6FB5` | `#1F7A3E` / `#17672F` | `#B3362D` / `#9E2F27` | `#8A5A00` | `#6E6E6E` / `#C8C8C8` |
| ghost-light-dark | `#171513` | `#1F1C19` | `#27231F` | `#EDE6DC` | `#F0B44C` / `#8F5C10` | `#1F7A3E` / `#74C98C` | `#B3362D` / `#F08A80` | `#E3D35A` | `#8F877C` / `#48423B` |
| ghost-light-light (Work Light) | `#F6F2EC` | `#FFFDF9` | `#ECE5DB` | `#221D17` | `#8F5C10` / `#8F5C10` | `#1F7A3E` / `#17672F` | `#B3362D` / `#9E2F27` | `#6B6400` | `#7A7064` / `#D6CCBE` |
| booth-dark | `#12161B` | `#181D24` | `#1F252D` | `#E2E8EF` | `#4FD1E0` / `#0B6E7C` | `#1D7A45` / `#62D093` | `#B3362D` / `#F28B82` | `#E6B35A` | `#7E8A97` / `#3A434D` |
| booth-light | `#F1F4F7` | `#FFFFFF` | `#E3E8EE` | `#17202A` | `#0B6E7C` / `#0B6E7C` | `#1F7A3E` / `#17672F` | `#B3362D` / `#9E2F27` | `#8A5A00` | `#6A7682` / `#C7D0D9` |
| thorn-dark | `#17141A` | `#1E1A21` | `#26212A` | `#ECE6F0` | `#B79CF5` / `#6847B8` | `#1F7A3E` / `#6FC98A` | `#B3362D` / `#F28C84` | `#E6B35A` | `#8E8495` / `#463E4B` |
| thorn-light | `#F5F2F7` | `#FFFFFF` | `#E9E3ED` | `#1F1A23` | `#6847B8` / `#6847B8` | `#1F7A3E` / `#17672F` | `#B3362D` / `#9E2F27` | `#8A5A00` | `#74697B` / `#D3CAD8` |

The full token set (fills, hovers, borders, muted text) is in `src/styles/tokens.css`. That file is the source; this table must match it. Components use tokens only, never hex values.

Status is always a word or glyph plus colour (Running, Paused, Ready), never colour alone.

## Type
- System UI stack only. This runs on whatever laptop is in the booth; native type reads like the rest of the operator's tools and costs no font loading at a venue with bad Wi-Fi.
- Monospace only for ThornCue Web show codes (an identifier people read out and type).
- `tabular-nums` on cue numbers, durations, times and percentages.
- Weights 400 and 600. Sentence case everywhere. No tracked caps.

## Density tier
- 14px text, 20px line, 32px controls, 4px base spacing.
- Cue rows 40px (44px under 600px wide). GO and Stop 56px tall: the two buttons pressed without looking.

## Anchor per screen
- Show: the transport (GO + the name of the standby cue).
- Library: the shows table.
- Web shows: the web shows table.
- Settings: the theme picker (family cards + mode control).
- Import / new show: the single form or dialog.

## Motion budget
- Hover and selection colour: 120ms.
- Buttons: 1px press.
- Nothing else moves. The cue list scrolls to the standby cue instantly (no smooth scroll: during a show the operator needs it there now).
- `prefers-reduced-motion` disables transitions.

## Layout (show page)
Header → transport → now-playing strip (always present, so nothing shifts when a cue starts) → toolbar → cue list → inspector for the selected cue. The inspector can be hidden for running the show.

## Signature
None. It's a work tool.

## Deliberate overrides
| Trait that would normally be a tell | Why it's right here |
|---|---|
| Dark page by default | Booth console genre: screen light spills onto the stage. Light variants exist for desk prep in daylight; the operator chooses. |
| System font as the only voice | Native operator tool; no web font download at the venue. |
| ▶ glyph in the Q column | Marks the standby cue (the playhead), like playback software does. It carries meaning, not decoration. |
| User-chosen colours on cue groups | Groups are the operator's own labels ("Act 1", "Storm"), and colour is how people tell panels apart at a glance, as in btop. Kept to a 1px rounded outline with the name on its top edge: no fills, no stripes, no coloured text, so GO green, Stop red and the select colour keep their meaning. Status inside a group is still words ("Playing Q4", "3 of 8 played"). Presets are mid-tones that read on dark and light themes. |

## Refused defaults
- Near-black + gold accent (previous version). Wave 2 "dark + one accent", and the gold was justified as "spotlight gel", which is a pun on the subject, not a convention.
- Serif "playbill" wordmark. Costume.
- Coloured left stripes on selected and playing rows. Replaced by row fill + the ▶ playhead + a "Running" word.
- Unicode-symbol icons on toolbar buttons (⬇ ⎘ ⇄ ⏸ ⏹ ⏮). Buttons now say what they do.
- Show cards grid on the Library. Shows have comparable fields, so they're a table.
- Coloured flag chips (↺ → ~ ⏱) with four extra hues. Replaced by plain words in a Notes column.

## Looks log
| Date | Product | Background | Accent | Display face | Layout | Hero |
|---|---|---|---|---|---|---|
| 2026-11-10 | ThornCue | `#161616` neutral | go green + OS-selection blue | system UI | transport / list / inspector | none (lands on the cue list) |
| 2026-11-11 | ThornCue themes | four families × dark/light; Graphite dark default | go green fixed; select hue per family | system UI | unchanged | none |
| 2026-11-12 | ThornCue desktop | unchanged | unchanged | system UI | Library home + show page as the web | none |
