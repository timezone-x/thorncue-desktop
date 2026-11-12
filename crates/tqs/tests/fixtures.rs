//! Fixture tests driven by `docs/format/fixtures/index.json` (CLAUDE.md conventions).

use std::path::{Path, PathBuf};

use serde_json::Value;
use tqs::{Limits, Manifest, TqsError};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/format/fixtures")
}

fn index() -> Vec<Value> {
    let bytes = std::fs::read(fixtures().join("index.json")).unwrap();
    serde_json::from_slice::<Value>(&bytes)
        .unwrap()
        .as_array()
        .unwrap()
        .clone()
}

/// Remove what the comparison ignores: `generator`, `exported_at`, and unknown `x_...` fields
/// (which we preserve, but the expected files leave out).
fn strip(v: &mut Value) {
    match v {
        Value::Object(o) => {
            o.retain(|k, _| k != "generator" && k != "exported_at" && !k.starts_with("x_"));
            o.values_mut().for_each(strip);
        }
        Value::Array(a) => a.iter_mut().for_each(strip),
        _ => {}
    }
}

/// Structural equality with numbers compared within 1e-9; key order doesn't matter.
fn same(a: &Value, b: &Value, path: &str) -> Result<(), String> {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            let (x, y) = (x.as_f64().unwrap(), y.as_f64().unwrap());
            if (x - y).abs() > 1e-9 {
                return Err(format!("{path}: {x} != {y}"));
            }
        }
        (Value::Object(x), Value::Object(y)) => {
            let mut kx: Vec<_> = x.keys().collect();
            let mut ky: Vec<_> = y.keys().collect();
            kx.sort();
            ky.sort();
            if kx != ky {
                return Err(format!("{path}: keys {kx:?} != {ky:?}"));
            }
            for k in x.keys() {
                same(&x[k], &y[k], &format!("{path}.{k}"))?;
            }
        }
        (Value::Array(x), Value::Array(y)) => {
            if x.len() != y.len() {
                return Err(format!("{path}: length {} != {}", x.len(), y.len()));
            }
            for (i, (p, q)) in x.iter().zip(y).enumerate() {
                same(p, q, &format!("{path}[{i}]"))?;
            }
        }
        _ if a == b => {}
        _ => return Err(format!("{path}: {a} != {b}")),
    }
    Ok(())
}

fn assert_matches_expected(manifest: &Manifest, expected_rel: &str) {
    let mut got = serde_json::to_value(manifest).unwrap();
    strip(&mut got);
    let mut want: Value =
        serde_json::from_slice(&std::fs::read(fixtures().join(expected_rel)).unwrap()).unwrap();
    strip(&mut want);
    if let Err(e) = same(&got, &want, "manifest") {
        panic!("{expected_rel}: {e}");
    }
}

/// Read back what was written into the library folder, through the same normaliser.
fn manifest_on_disk(dir: &Path) -> Manifest {
    Manifest::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap())
        .unwrap()
        .0
}

fn dir_is_empty(dir: &Path) -> bool {
    std::fs::read_dir(dir).unwrap().next().is_none()
}

#[test]
fn valid_fixtures_import_and_match_expected() {
    let mut n = 0;
    for f in index().iter().filter(|f| f["expect"] == "accept") {
        let file = f["file"].as_str().unwrap();
        let parent = tempfile::tempdir().unwrap();
        let dest = parent.path().join("show");
        let (manifest, warnings) =
            tqs::read_package(&fixtures().join(file), &dest, Limits::default())
                .unwrap_or_else(|e| panic!("{file} should import: {e}"));
        assert_matches_expected(&manifest, f["expected"].as_str().unwrap());
        assert_eq!(
            manifest_on_disk(&dest),
            manifest,
            "{file}: manifest.json on disk"
        );
        for (id, a) in &manifest.assets {
            let len = std::fs::metadata(dest.join(&a.path)).unwrap().len();
            assert_eq!(len, a.bytes, "{file}: audio {id}");
        }
        // Nothing but the show folder is left next to it.
        assert_eq!(
            std::fs::read_dir(parent.path()).unwrap().count(),
            1,
            "{file}"
        );
        println!("{file}: ok, {} warnings: {warnings:?}", warnings.len());
        n += 1;
    }
    assert_eq!(n, 5);
}

#[test]
fn invalid_fixtures_are_rejected_and_leave_nothing() {
    let mut n = 0;
    for f in index().iter().filter(|f| f["expect"] == "reject") {
        let file = f["file"].as_str().unwrap();
        let parent = tempfile::tempdir().unwrap();
        let dest = parent.path().join("show");
        match tqs::read_package(&fixtures().join(file), &dest, Limits::default()) {
            Ok(_) => panic!("{file} ({}) should be rejected", f["about"]),
            Err(TqsError::Invalid(msg)) => {
                assert!(
                    msg.ends_with('.'),
                    "{file}: message should be a sentence: {msg}"
                );
                println!("{file}: {msg}");
            }
            Err(e) => panic!("{file}: rejected for the wrong reason: {e}"),
        }
        assert!(dir_is_empty(parent.path()), "{file} left files behind");
        n += 1;
    }
    assert!(n >= 25, "only {n} invalid fixtures ran");
}

#[test]
fn rejection_messages_say_what_to_do() {
    let msg = |file: &str| {
        let parent = tempfile::tempdir().unwrap();
        tqs::read_package(
            &fixtures().join(file),
            &parent.path().join("s"),
            Limits::default(),
        )
        .unwrap_err()
        .to_string()
    };
    assert!(msg("invalid/future-version.tqs").contains("Update ThornCue"));
    assert!(msg("invalid/unknown-cue-type.tqs").contains("Update ThornCue"));
    assert!(msg("invalid/unknown-continue.tqs").contains("Update ThornCue"));
    assert!(msg("invalid/sha-mismatch.tqs").contains("damaged"));
    assert!(msg("invalid/zip-bomb.tqs").contains("zip bomb"));
}

#[test]
fn fixture_03_keeps_unknown_fields() {
    let parent = tempfile::tempdir().unwrap();
    let dest = parent.path().join("show");
    let (m, warnings) = tqs::read_package(
        &fixtures().join("valid/03-needs-normalising.tqs"),
        &dest,
        Limits::default(),
    )
    .unwrap();
    assert_eq!(m.extra["x_future_field"]["readers"], "must ignore this");
    assert_eq!(m.show.extra["x_colour"], "plum");
    assert_eq!(m.assets["t"].extra["x_note"], "ignored");
    assert_eq!(m.cues[0].extra["x_colour"], "red");
    assert_eq!(manifest_on_disk(&dest).cues[0].extra["x_colour"], "red");
    let w = warnings
        .iter()
        .map(|w| w.0.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        w.contains("__MACOSX") && w.contains(".DS_Store") && w.contains("unused.wav"),
        "{w}"
    );
}

#[test]
fn legacy_zip_converts() {
    for f in index().iter().filter(|f| f["expect"] == "accept-legacy") {
        let file = f["file"].as_str().unwrap();
        let parent = tempfile::tempdir().unwrap();
        let dest = parent.path().join("show");
        let (manifest, warnings) =
            tqs::import_legacy_zip(&fixtures().join(file), &dest, Limits::default()).unwrap();
        assert_matches_expected(&manifest, f["expected"].as_str().unwrap());
        assert_eq!(manifest_on_disk(&dest), manifest);
        let w = warnings
            .iter()
            .map(|w| w.0.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(w.contains("orphan-from-deleted-cue.mp3"), "{w}");
        assert!(w.contains("File missing on the server"), "{w}");
    }
}

#[test]
fn legacy_import_applies_zip_safety_rules() {
    // A .tqs fixture that breaks a 1.3 rule is just as bad inside a legacy zip.
    for file in [
        "invalid/path-traversal.tqs",
        "invalid/case-collision.tqs",
        "invalid/zip-bomb.tqs",
        "invalid/symlink.tqs",
        "invalid/not-a-zip.tqs",
    ] {
        let parent = tempfile::tempdir().unwrap();
        let r = tqs::import_legacy_zip(
            &fixtures().join(file),
            &parent.path().join("s"),
            Limits::default(),
        );
        assert!(r.is_err(), "{file} should be refused");
        assert!(dir_is_empty(parent.path()), "{file} left files behind");
    }
}

#[test]
fn write_then_read_round_trips_fixture_02() {
    let parent = tempfile::tempdir().unwrap();
    let first = parent.path().join("first");
    let (m1, _) = tqs::read_package(
        &fixtures().join("valid/02-all-features.tqs"),
        &first,
        Limits::default(),
    )
    .unwrap();

    let out = parent.path().join("Exported.tqs");
    let written = tqs::write_package(&first, &out).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&out).unwrap().permissions().mode() & 0o777,
            0o644,
            "exports are readable by others"
        );
    }
    assert_eq!(written.generator.name, "thorncue-desktop");

    let second = parent.path().join("second");
    let (m2, warnings) = tqs::read_package(&out, &second, Limits::default()).unwrap();
    assert!(
        warnings.is_empty(),
        "a package we wrote should import cleanly: {warnings:?}"
    );

    let ignore = |m: &Manifest| {
        let mut m = m.clone();
        m.generator = tqs::Generator {
            name: String::new(),
            version: String::new(),
        };
        m.exported_at.clear();
        m
    };
    assert_eq!(ignore(&m1), ignore(&m2));
    for a in m1.assets.values() {
        assert_eq!(
            std::fs::read(first.join(&a.path)).unwrap(),
            std::fs::read(second.join(&a.path)).unwrap()
        );
    }

    // Manifest first, audio stored.
    let mut zip = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap();
    assert_eq!(zip.by_index(0).unwrap().name_raw(), b"manifest.json");
    for i in 1..zip.len() {
        assert_eq!(
            zip.by_index(i).unwrap().compression(),
            zip::CompressionMethod::Stored
        );
    }
}

#[test]
fn export_drops_missing_audio_and_marks_cues_missing() {
    let parent = tempfile::tempdir().unwrap();
    let show = parent.path().join("show");
    tqs::read_package(
        &fixtures().join("valid/02-all-features.tqs"),
        &show,
        Limits::default(),
    )
    .unwrap();
    std::fs::remove_file(show.join("audio/flac1.flac")).unwrap();
    let out = parent.path().join("out.tqs");
    let m = tqs::write_package(&show, &out).unwrap();
    assert!(!m.assets.contains_key("flac1"));
    assert!(m.cues.iter().all(|c| c.asset.as_deref() != Some("flac1")));
    tqs::read_package(&out, &parent.path().join("again"), Limits::default()).unwrap();
}

#[test]
fn running_byte_count_enforces_the_limit() {
    // Declared sizes pass a generous check, but a tiny limit stops extraction.
    let parent = tempfile::tempdir().unwrap();
    let limits = Limits {
        max_total: 1000,
        ..Limits::default()
    };
    let err = tqs::read_package(
        &fixtures().join("valid/01-minimal.tqs"),
        &parent.path().join("s"),
        limits,
    )
    .unwrap_err();
    assert!(err.to_string().contains("limit"), "{err}");
    assert!(dir_is_empty(parent.path()));
}

#[test]
fn destination_that_exists_is_not_overwritten() {
    let parent = tempfile::tempdir().unwrap();
    let dest = parent.path().join("show");
    std::fs::create_dir(&dest).unwrap();
    std::fs::write(dest.join("keep.txt"), "mine").unwrap();
    assert!(
        tqs::read_package(
            &fixtures().join("valid/01-minimal.tqs"),
            &dest,
            Limits::default()
        )
        .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(dest.join("keep.txt")).unwrap(),
        "mine"
    );
    assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 1);
}

#[test]
fn detect_kind_tells_shows_from_web_exports() {
    use tqs::PackageKind;
    let f = fixtures();
    assert_eq!(
        tqs::detect_kind(&f.join("valid/02-all-features.tqs")).unwrap(),
        PackageKind::Show
    );
    assert_eq!(
        tqs::detect_kind(&f.join("legacy/web-export-STAGE-4821.zip")).unwrap(),
        PackageKind::LegacyWebExport
    );
    assert!(tqs::detect_kind(&f.join("invalid/not-a-zip.tqs")).is_err());
}
