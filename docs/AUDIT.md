# ThornCue audit

Commit audited: `a966603` ("Custom Themes and settings"), `timezone-x/thorncue-web`. 25 tracked files, about 7,300 lines: `app.py` (1,119), `utils.py` (835), `static/app.js` (1,927), `static/style.css` (820), 9 templates, design docs.

Every line was read. Line references are to that commit.

## Summary

The code is careful. Input validation, path handling, the trim/cut model, the audio-clock fades and the save queue are all well done, and the design system is the strongest part of the project. Nothing here needs to be thrown away on quality grounds.

**Is it fast enough?** For the button itself, yes. When a cue is already decoded, GO in `app.js` creates a buffer source and starts it on the audio clock; the delay you'd hear is the OS output latency (typically 10–40 ms), not JavaScript. Rust will not make GO faster in any way you can hear.

What does make the web version risky in a show is **architecture, not language**:

1. **Everything is decoded into RAM** in the browser tab (`app.js` L1920–1927, L231–255). Decoded audio is 32-bit float at the context's rate: about 21 MB per minute of stereo at 44.1 kHz, and edited cues are stored a second time (`renders`, L265–274). A 60-minute show is ~1.3 GB in one tab. On a school-booth laptop with 8 GB, that's how a browser kills a tab mid-show.
2. **The audio lives on a server.** Every session downloads every file from cadbury before it can be decoded. Venue Wi-Fi, a tunnel hiccup or a sleeping server means no show.
3. **A browser decides things for you:** output device (whatever the OS default is), sleep, tab discarding, autoplay rules, and updates that arrive mid-run.
4. **Two engine bugs that can bite live** (W1, W2 below).

So the move to a native app is right, for these reasons. The design, the cue model and the playback rules carry straight over; that's why the `.tqs` spec in this pack is written from the current behaviour.

The web side has four bugs worth fixing whatever happens to the client: a data-loss bug in saving (S1), a corruption risk in how shows are written (S2), revoked share links that keep working (S3), and a default secret key that would let anyone forge an admin session (S4).

Severity: **High** = can lose a show, break a performance, or let someone in. **Medium** = real but needs bad luck. **Low** = tidy-up.

---

## Server (`app.py`, `utils.py`, `config.py`)

| # | Severity | Where | Problem | Fix |
|---|---|---|---|---|
| S1 | High | `app.py` L447–457 | `/save` rebuilds the cue list from whatever the browser sends and **drops every cue not in the payload**. Two tabs, or two people with Edit access: A adds a cue, B's next autosave (every edit, 500 ms debounce) deletes it from `show.json`. Its audio file is orphaned on disk and nobody is told. | Treat the payload as *updates plus order*. Cues the server has but the payload doesn't are kept (appended in their old relative order). Deleting stays the job of `/cue/<id>/delete`. |
| S2 | High | `utils.py` L453–464, `run.sh` L4 | `save_show` opens with `"w"` (truncates) and writes in place: a crash, OOM kill or full disk mid-write leaves an empty or half-written `show.json`, and the show is gone. And there's no locking, but gunicorn runs **2 workers**, so two requests that load-modify-save the same show at once lose one update (add cue + autosave is exactly that pattern). | Write to a temp file in the same directory, `fsync`, `os.replace`. Wrap every load-modify-save of a show in a per-show `fcntl.flock` on a lock file. |
| S3 | High | `app.py` L277–292, L79–86, L948–961 | Link permissions are copied **into the session cookie** on `/join/<token>`. Revoking or editing the link only changes the database, so anyone who already opened it keeps their access (and old permissions) until their browser session ends. | Store the token in the session, not the permissions. Look the token up on each request; revoked = gone. |
| S4 | High | `config.py` L11, `app.py` L22 | If `.env` is missing, the secret key is the literal string `change-me-in-production`. Sessions are signed cookies that contain `is_admin`, so anyone who knows the default can mint an admin session. | Refuse to start (outside debug) when the key is missing or is the default. |
| S5 | Medium | `app.py` L161, L198, L64–67, L1061–1078 | `is_admin` and `user_id` are trusted from the session. Demoting or deleting a user doesn't take effect until their session expires (8 h). | `before_request`: reload the user; if gone, clear the session; refresh `is_admin`. |
| S6 | Medium | `config.py` L14–15, `app.py` L24 | `MAX_CONTENT_LENGTH` is the per-file limit (200 MB) and `IMPORT_MAX_MB` (500) is never used, so any show over 200 MB can be exported but not imported back. | Global limit = import limit; check the per-file limit inside the upload routes. |
| S7 | Medium | `utils.py` L699–716 | Import extracts every audio entry with no total-size check, so a small zip can unpack to fill the disk (zip bomb). Cue fields other than trims aren't validated on import, and `type` isn't checked. | Apply FORMAT.md 1.3 (declared sizes, counted sizes, ratio) and run imported cues through the same validation as saves. |
| S8 | Medium | `app.py` L695–716 | Export builds the whole zip in a `BytesIO`: a 500 MB show is 500 MB of RAM per download, per worker. It also packs every file in the audio folder, including files orphaned by S1. | Stream from a temp file; include only audio that cues reference. (The new `.tqs` export does both.) |
| S9 | Medium | (deployment) | If this is served through a Cloudflare tunnel, Cloudflare caps request bodies at 100 MB on the free plan, so the 200 MB upload limit can't actually be reached and large imports fail with a Cloudflare 413. Downloads are not affected. | Either lower `MAX_UPLOAD_MB` to 95 so the app gives a proper message, or note it in the README. |
| S10 | Low | `utils.py` L809–835 | The yt-dlp download thread isn't cancelled at the 60 s timeout. The request returns "timed out" and the thread may still write a file afterwards, orphaned. | Run yt-dlp as a subprocess with a hard timeout and kill it. |
| S11 | Low | `app.py` L480–499, `app.js` L293–296, L1564–1566 | Fade-out cues are created with `fade_out = 2`, but playback uses `duration` as the fade time and never reads `fade_out`. Harmless, but it's a field that lies. | Stop setting it. (`.tqs` defines `duration` as the fade time.) |
| S12 | Low | `app.py` L169–203, L149–166 | Open registration and no login rate limit. Fine on a LAN; on a public URL anyone can make an account. | Decide: invite-only toggle, or keep it open. A simple per-IP login limit either way. |
| S13 | Low | `app.py` L206 | `/logout` is a GET, so any page can sign you out with an `<img>`. | Make it POST. |
| S14 | Low | `app.py` L754–769 and L1098–1109 | Show deletion is written twice. | One helper. |
| S15 | Low | repo root `themes.css` | Byte-identical copy of the eight theme blocks already in `static/style.css`, and nothing loads it. Leftover from the theme work. | Delete. |
| S16 | Low | (whole repo) | No tests at all. | Start with the new API and the S1–S3 fixes. |

Things checked and fine: password hashing (bcrypt), path handling on audio routes (`Path(...).name`), the `next` redirect guard (L138–142), content sniffing of uploads, Jinja autoescaping, the security headers, SameSite=Lax cookies as CSRF protection for the POST routes, show-code validation.

---

## Show page (`static/app.js`, `templates/show.html`)

| # | Severity | Where | Problem | What the client does instead |
|---|---|---|---|---|
| W1 | High | L285–305, L403–408 | GO on a cue whose audio is still loading awaits the download and decode. **Stop all doesn't cancel that**, so the cue starts after you've pressed Stop. | Every pending start (loading or in pre-wait) is cancellable, and Stop all cancels all of them. |
| W2 | Medium | L417–423 | Hard stop closes the AudioContext; a GO still awaiting its decode then runs against the closed context and throws. | "Restart audio engine" rebuilds the engine and refuses commands until it's back. |
| W3 | High (long shows) | L1920–1927, L265–274 | Decode-everything-on-open, plus a second copy for every edited cue: see the summary. | Decode in a background thread into a memory budget; never block GO on it (ARCHITECTURE.md). |
| W4 | Medium | L633–642 | While audio is still loading a row shows nothing. The operator can't tell "ready" from "not yet". | Every cue shows Loading / Ready / Missing / Can't decode, and the transport says when the whole show is ready. |
| W5 | Medium | L1313 | Audition plays through the show output: previewing a cue during a performance plays it to the audience. | Audition to a separate output device (headphones). |
| W6 | Low | L1866–1870 | Space ignores key-repeat but there's no protection against a double tap. | Optional GO lockout (setting, off by default). |
| W7 | Low | L403–408 | Stop all is an instant cut. | Optional Stop-all fade time (setting, default 0 to match). |
| W8 | Low | L902–912 | Volume up to 200% with nothing after the master: two loud cues together clip. | Optional limiter on the master. |
| W9 | Low | L521–544 | Waveforms are drawn from the left channel only. | Draw from the louder of the channels. |

Things checked and fine: fades scheduled on the audio clock (L329–347) so they're sample-accurate; the silent-buffer trick for silence and fade-out cues so they're timed by the audio clock too (L349–368); 4 ms ramps at cut joins (L180–202); the serialised request queue (L62–69); `keepStandby` across reorders; Space handling outside text fields; the theme observer that repaints without touching playback or focus.

---

## Feature by feature: what the desktop client does with each

**Port** = same behaviour, new code. **Rebuild** = same purpose, different design. **Drop** = not in the client.

| Feature (web) | Client | Notes |
|---|---|---|
| GO, Stop all, Pause/Resume, standby, ↑ ↓, Back to top, J jump | Port | Same keys. Logic moves into the Rust engine. |
| Hard stop | Rebuild | "Restart audio engine", also the fix for an unplugged interface. |
| Audio, silence and fade-out cues; pre-wait, fades, loop, auto-continue | Port | Exactly the rules in FORMAT.md 2.5. |
| Live volume change on a running cue | Port | |
| Master volume | Port | |
| Trim and cut editor, waveform, undo, audition from marker | Port | Later milestone. Waveform peaks computed in Rust. |
| Audition | Rebuild | Separate output device. |
| Export WAV of an edited cue | Later | Low priority. |
| Original file download | Rebuild | "Show file in folder". |
| Replace file | Port | |
| Duplicate cue | Port | Shares the asset; no file copy. |
| Drag to reorder | Port | |
| Autosave | Rebuild | Local, atomic, with a backup copy; no server. |
| Add from URL (yt-dlp) | **Drop** | Would mean bundling Python and ffmpeg for three OSes, breaks whenever YouTube changes, and it's the legally shakiest feature. Download audio separately. |
| Accounts, sharing, links, transfer, admin | **Drop** | That's what the web version is for. |
| Show codes | **Drop** | Local shows have local ids. The web code is kept as `source.code` for re-imports. |
| Duplicate show | Port | |
| Export ZIP / Import | Rebuild | `.tqs` both ways; legacy zip import too. |
| Themes (4 families × dark/light/system), DESIGN.md | Port | Tokens copied verbatim. |
| Inspector hide and resize, shortcuts dialog, toasts | Port | |
| **New:** output device choice, engine status, keep-awake during a show, crash-safe autosave, per-cue ready state, import from web | New | See PROMPT.md milestones. |

---

## Decisions made in this pack (change them before you start if you disagree)

1. **Tauri 2 + Svelte 5 + TypeScript** for the client. Svelte rather than plain TS because the show page already has a lot of hand-written DOM syncing (`renderRow`, `renderStandby`, `setValue`...) that a reactive framework removes.
2. **Rust owns all audio and all files.** The webview is only a UI and can be reloaded without a sound dropping.
3. **`kira`** for playback (built on `cpal` + `symphonia`). It has sample-accurate clocks and tweens, which is most of a cue engine.
4. **`.tqs` folder = library format.** A show in the client's library is an unpacked `.tqs` (manifest.json + audio/), so export is "zip the folder" and there's one schema, not two.
5. **API tokens before device-code sign-in.** You paste a token from the web's Settings page. Device-code sign-in can replace it later without changing any endpoint.
6. **No sync.** Import only.
7. **GPL-3.0** for the client unless you say otherwise.
