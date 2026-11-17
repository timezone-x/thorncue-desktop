//! `docs/format` is an exact copy of timezone-x/thorncue-spec's `format/` folder. Never edit it
//! here: change the spec repo, then re-sync with its `tools/sync-format.sh`. This test is the
//! Rust twin of the spec's `tools/checksums.py check` and the web's `tests/test_format_copy.py`.

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn format_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/format")
}

fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.is_dir() {
            if name != "__pycache__" {
                walk(root, &path, out);
            }
        } else if name != "CHECKSUMS" && name != ".DS_Store" {
            let rel = path.strip_prefix(root).unwrap();
            let rel = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            out.insert(rel, path);
        }
    }
}

fn sha256_hex(path: &Path) -> String {
    let digest = Sha256::digest(std::fs::read(path).unwrap());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn docs_format_matches_its_checksums() {
    let root = format_dir();
    let text = std::fs::read_to_string(root.join("CHECKSUMS"))
        .expect("docs/format/CHECKSUMS is missing: this copy wasn't made by tools/sync-format.sh");
    let mut lines = text.lines();
    let header = lines.next().unwrap_or_default();
    assert!(header.starts_with("# thorncue-spec "), "bad CHECKSUMS header: {header}");
    let want: BTreeMap<&str, &str> = lines
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let (hash, name) = l.split_once("  ").expect("CHECKSUMS line without two spaces");
            (name, hash)
        })
        .collect();
    let mut have = BTreeMap::new();
    walk(&root, &root, &mut have);

    let mut problems = Vec::new();
    for (name, hash) in &want {
        match have.get(*name) {
            Some(path) if sha256_hex(path) != *hash => problems.push(format!("changed: {name}")),
            Some(_) => {}
            None => problems.push(format!("missing: {name}")),
        }
    }
    for name in have.keys() {
        if !want.contains_key(name.as_str()) {
            problems.push(format!("extra: {name}"));
        }
    }
    assert!(
        problems.is_empty(),
        "docs/format differs from {} — re-sync it from thorncue-spec, don't edit it here:\n{}",
        header.trim_start_matches("# ").split(". Generated").next().unwrap(),
        problems.join("\n")
    );
}
