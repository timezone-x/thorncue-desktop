//! `.tqc` and `.tqg` writing (FORMAT.md 6.1-6.3): write a piece of fixture 06 and read it back
//! with the same reader that checks every fixture.

use std::path::{Path, PathBuf};
use tqs::{Kind, Limits, Piece};

fn fixture(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/format/fixtures")
        .join(rel)
}

/// Fixture 06 unpacked: g01 | act1: g02 g03 g04 | g05 | storm: g06 g07.
fn show(dir: &Path) -> PathBuf {
    let d = dir.join("show");
    tqs::read_package(&fixture("valid/06-groups.tqs"), &d, Limits::default()).unwrap();
    d
}

fn read_back(path: &Path, dir: &Path, kind: Kind) -> tqs::Manifest {
    tqs::read_package_of(
        path,
        &dir.join("back"),
        Limits::default(),
        &[kind],
        &mut |_| {},
    )
    .unwrap()
    .0
}

#[test]
fn a_tqc_has_the_picked_cues_in_show_order_and_no_groups() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = show(tmp.path());
    let out = tmp.path().join("cues.tqc");
    let ids = vec!["g06".to_string(), "g02".to_string(), "g05".to_string()];
    let m = tqs::write_piece(&dir, &out, &Piece::Cues(ids)).unwrap();
    assert_eq!(m.kind, Kind::Cues);
    let back = read_back(&out, tmp.path(), Kind::Cues);
    let order: Vec<_> = back.cues.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(order, ["g02", "g05", "g06"]);
    assert!(back.groups.is_empty());
    assert!(back.cues.iter().all(|c| c.group.is_none()));
    let used: Vec<_> = back
        .cues
        .iter()
        .filter_map(|c| c.asset.as_deref())
        .collect();
    assert!(
        back.assets.keys().all(|a| used.contains(&a.as_str())),
        "only used assets"
    );
    let mut zip = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap();
    let mut text = String::new();
    std::io::Read::read_to_string(&mut zip.by_name("manifest.json").unwrap(), &mut text).unwrap();
    assert!(!text.contains("\"show\""), "no show section in a .tqc");
}

#[test]
fn a_tqg_has_exactly_its_group_and_every_cue_in_it() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = show(tmp.path());
    let out = tmp.path().join("storm.tqg");
    tqs::write_piece(&dir, &out, &Piece::Group("storm".into())).unwrap();
    let back = read_back(&out, tmp.path(), Kind::Group);
    assert_eq!(back.groups.len(), 1);
    assert_eq!(back.groups[0].id, "storm");
    let ids: Vec<_> = back.cues.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, ["g06", "g07"]);
    assert!(
        back.cues
            .iter()
            .all(|c| c.group.as_deref() == Some("storm"))
    );
}

#[test]
fn nothing_to_write_is_an_error_and_leaves_no_file() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = show(tmp.path());
    let out = tmp.path().join("x.tqg");
    assert!(tqs::write_piece(&dir, &out, &Piece::Group("nope".into())).is_err());
    assert!(tqs::write_piece(&dir, &out, &Piece::Cues(vec!["nope".into()])).is_err());
    assert!(!out.exists());
}
