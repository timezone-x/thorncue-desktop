# Bundled sample show

`sample-show.tqs` is a byte-for-byte copy of `samples/sample-show.tqs` from `timezone-x/thorncue-spec` (the same file thorncue-web ships as `sample/sample-show.tqs`). The spec repo builds it (`samples/make_sample.py`) and holds its credits (`samples/README.md`; all three recordings are CC0 from Freesound).

Don't edit it here, and don't rename its cues: the cue names are the teaching text. To update it, rebuild it in the spec repo, copy the new file in, and change the hash below. `src-tauri/src/sample.rs` (test `the_bundled_sample_is_the_spec_file`) checks the copy against it.

The app embeds it with `include_bytes!` (about 4 MB), so it can't go missing from an install. Opening it imports a fresh copy through the normal `.tqs` import (see README "Sample show").

sha256: d0f8cb329f09cde1cace187ea9c6b871be4e61d4984292311572ba6dd1d5f55a
