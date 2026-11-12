# Reference: ThornCue Web

Read-only copies from `timezone-x/thorncue-web` at commit `a966603`, for porting the look and the behaviour. Not built, not imported, not edited. Line numbers in `docs/AUDIT.md` and `docs/ARCHITECTURE.md` refer to these files.

| File | Port from it |
|---|---|
| `DESIGN.md` | The design contract: themes, type, density, layout, motion, refused defaults |
| `style.css` | Colour and layout tokens (copy verbatim), component styles |
| `show.html` | Show page structure: header, transport, now-playing, toolbar, cue table, inspector, dialogs |
| `app.js` | Playback behaviour, edit model, keyboard, inspector, trim editor |

Delete this folder before the repo goes public if you'd rather not publish the web code.
