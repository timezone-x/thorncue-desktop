//! Audio export (FORMAT.md section 7): numbers, names, collisions, folders, the cue list, the
//! zip, and importing one (the spec's `fixtures/audio-export/`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tqs::{
    AudioExportOptions, AudioZip, CueChoice, Limits, Manifest, PackageKind, cue_list_csv,
    cue_number, plan_paths, safe_name, zip_name,
};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/format/fixtures")
}

fn cue(id: &str, ty: &str, name: &str, group: Option<&str>) -> Value {
    let mut c = json!({"id": id, "type": ty, "name": name, "asset": null, "duration": 2.5});
    if let Some(g) = group {
        c["group"] = json!(g);
    }
    c
}

fn show(name: &str, cues: Vec<Value>, groups: Vec<Value>) -> Manifest {
    let v = json!({
        "format": "thorncue.show", "format_version": 1,
        "generator": {"name": "test", "version": "1"}, "exported_at": "2026-11-19T00:00:00Z",
        "show": {"name": name, "author": "", "notes": "", "source": null},
        "assets": {}, "cues": cues, "groups": groups,
    });
    Manifest::from_slice(&serde_json::to_vec(&v).unwrap())
        .unwrap()
        .0
}

/// `n` audio cues called "Cue 1"... with no groups.
fn plain(n: usize) -> Manifest {
    show(
        "Plain",
        (1..=n)
            .map(|i| cue(&format!("c{i}"), "audio", &format!("Cue {i}"), None))
            .collect(),
        vec![],
    )
}

fn all_have(ext: &'static str) -> impl Fn(usize) -> Option<String> {
    move |_| Some(ext.to_string())
}

#[test]
fn numbers_are_padded_to_the_show_s_cue_count() {
    assert_eq!(cue_number(2, 9, true), "Q3");
    assert_eq!(cue_number(2, 24, true), "Q03");
    assert_eq!(cue_number(2, 120, true), "Q003");
    assert_eq!(cue_number(2, 120, false), "Q3");
    assert_eq!(cue_number(9, 10, true), "Q10");

    // Always the number in the whole show, even exporting only some cues.
    let m = plain(24);
    let o = AudioExportOptions::default();
    let (paths, _) = plan_paths(&m, &o, &[2, 20], &all_have("wav"));
    assert_eq!(paths[&2], "Q03 Cue 3.wav");
    assert_eq!(paths[&20], "Q21 Cue 21.wav");
}

#[test]
fn the_file_name_rule() {
    assert_eq!(
        safe_name(r#"Storm: "big" <one>/a\b*c?d|e"#, "Q1", 100),
        "Storm big oneabcde"
    );
    assert_eq!(
        safe_name("  Rain\t\non\u{a0}the   roof ..", "Q1", 100),
        "Rain on the roof"
    );
    assert_eq!(safe_name("bell\u{7}\u{1f}\u{7f}", "Q1", 100), "bell");
    assert_eq!(safe_name("...", "Q4", 100), "Q4");
    assert_eq!(safe_name("", "Group", 60), "Group");
    assert_eq!(safe_name("CON", "Q1", 100), "CON_");
    assert_eq!(safe_name("lpt9", "Q1", 100), "lpt9_");
    assert_eq!(safe_name("com10", "Q1", 100), "com10");
    let long = "a".repeat(150);
    assert_eq!(safe_name(&long, "Q1", 100).chars().count(), 100);
    assert_eq!(safe_name(&long, "Group", 60).chars().count(), 60);
    // Cut, then trimmed again: a dot at the cut goes.
    assert_eq!(
        safe_name(&format!("{}.b", "a".repeat(99)), "Q1", 100),
        "a".repeat(99)
    );
}

#[test]
fn collisions_folders_and_reserved_names() {
    let m = show(
        "Storm night",
        vec![
            cue("a", "audio", "Storm", None),
            cue("b", "audio", "storm", None),
            cue("c", "audio", "Storm", Some("g1")),
            cue("d", "audio", "Storm", Some("g2")),
            cue("e", "audio", "cue-list", None),
            cue("f", "audio", "Storm night", None),
            cue("g", "silence", "Wait", None),
        ],
        vec![
            json!({"id": "g1", "name": "Storm.wav", "color": "#1f9e9a"}),
            json!({"id": "g2", "name": "storm.WAV", "color": "#c98a14"}),
        ],
    );
    let o = AudioExportOptions {
        numbers: false,
        folders: true,
        tqs: true,
        ..Default::default()
    };
    let picked: Vec<usize> = (0..7).collect();
    let has = |i: usize| (m.cues[i].cue_type == tqs::CueType::Audio).then(|| "wav".to_string());
    let (paths, tqs_name) = plan_paths(&m, &o, &picked, &has);
    assert_eq!(tqs_name, "Storm night.tqs");
    assert_eq!(paths[&0], "Storm.wav");
    assert_eq!(paths[&1], "storm (2).wav", "case-insensitive");
    // Folders share the top level with files: the folder "Storm.wav" is taken by Q1's file.
    assert_eq!(
        paths[&2], "Storm.wav (2)/Storm.wav",
        "own folder, own names"
    );
    assert_eq!(
        paths[&3], "storm.WAV (3)/Storm.wav",
        "second group with the same name"
    );
    assert_eq!(paths[&4], "cue-list.wav", "only cue-list.csv is reserved");
    assert_eq!(paths[&5], "Storm night.wav", ".tqs reserved, not .wav");
    assert!(!paths.contains_key(&6), "silence has no file");

    // Without folders everything is at the top level; numbers keep names apart.
    let o = AudioExportOptions::default();
    let (paths, _) = plan_paths(&m, &o, &picked, &has);
    assert_eq!(paths[&2], "Q3 Storm.wav");
    // The cue list reserves its name first.
    let m2 = show("S", vec![cue("a", "audio", "cue-list", None)], vec![]);
    let o = AudioExportOptions {
        numbers: false,
        ..Default::default()
    };
    let (p, _) = plan_paths(&m2, &o, &[0], &|_| Some("csv".into()));
    assert_eq!(p[&0], "cue-list (2).csv");
}

#[test]
fn zip_names() {
    let m = show(
        "Storm / night",
        vec![
            cue("a", "audio", "x", Some("g1")),
            cue("b", "audio", "y", None),
            cue("c", "audio", "z", None),
        ],
        vec![json!({"id": "g1", "name": "Act 1", "color": "#1f9e9a"})],
    );
    assert_eq!(
        zip_name(&m, &CueChoice::All, &[0, 1, 2]),
        "Storm night audio.zip"
    );
    assert_eq!(
        zip_name(&m, &CueChoice::Selected(vec![]), &[1, 2]),
        "Storm night - Q2 to Q3 audio.zip"
    );
    assert_eq!(
        zip_name(&m, &CueChoice::Selected(vec![]), &[1]),
        "Storm night - Q2 audio.zip"
    );
    assert_eq!(
        zip_name(&m, &CueChoice::Group("g1".into()), &[0]),
        "Storm night - Act 1 audio.zip"
    );
    assert_eq!(tqs::single_file_name(&m, 1, "mp3"), "Storm night Q2 y.mp3");
}

#[test]
fn the_cue_list() {
    let mut cues = vec![
        cue("a", "audio", "=SUM(A1)", Some("g1")),
        cue("b", "silence", "Wait, then \"go\"", None),
        cue("c", "fadeout", "Fade", None),
        cue("d", "audio", "Missing", None),
    ];
    cues[0]["volume"] = json!(1.256);
    cues[0]["fade_in"] = json!(1.5);
    cues[0]["fade_out"] = json!(0.25);
    cues[0]["pre_wait"] = json!(0.1);
    cues[0]["loop"] = json!(true);
    cues[1]["continue"] = json!("after_end");
    cues[1]["duration"] = json!(2.0);
    let m = show(
        "S",
        cues,
        vec![json!({"id": "g1", "name": "-Act", "color": "#1f9e9a"})],
    );
    let paths = HashMap::from([(0usize, "Act/Q1 =SUM(A1).wav".to_string())]);
    let lengths = HashMap::from([(0usize, 12.3456)]);
    let csv = String::from_utf8(cue_list_csv(&m, &[0, 1, 2, 3], &paths, &lengths)).unwrap();
    let csv = csv.strip_prefix('\u{feff}').expect("BOM");
    let lines: Vec<&str> = csv.split("\r\n").collect();
    assert_eq!(
        lines[0],
        "number,name,type,file,group,volume %,fade in,fade out,pre-wait,loop,auto-continue,length"
    );
    assert_eq!(
        lines[1],
        "Q1,'=SUM(A1),Audio,Act/Q1 =SUM(A1).wav,'-Act,126,1.5,0.25,0.1,yes (file plays once),no,12.346"
    );
    assert_eq!(
        lines[2],
        "Q2,\"Wait, then \"\"go\"\"\",Silence,,,,,,0,,yes,2"
    );
    assert_eq!(lines[3], "Q3,Fade,Fade-out,,,,,,0,,no,2.5");
    assert_eq!(
        lines[4], "Q4,Missing,Audio,,,100,0,0,0,no,no,",
        "no audio: empty length"
    );
    assert_eq!(lines[5], "", "ends with CRLF");
    assert_eq!(tqs::seconds(0.0), "0");
    assert_eq!(tqs::seconds(2.10), "2.1");
}

#[test]
fn the_zip_layout() {
    let dir = tempfile::tempdir().unwrap();
    let wav = dir.path().join("x.wav");
    std::fs::write(&wav, b"RIFF....fake").unwrap();
    let out = dir.path().join("Show audio.zip");
    let mut z = AudioZip::create(&out).unwrap();
    z.add_cue_list(b"\xef\xbb\xbfnumber\r\n").unwrap();
    assert!(z.add_file("Q1 Café.wav", &wav, &mut || true).unwrap());
    assert!(z.add_file("Act 1/Q2 Rain.wav", &wav, &mut || true).unwrap());
    z.finish().unwrap();

    let mut zip = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap();
    assert_eq!(zip.comment(), b"thorncue.audio-export");
    let names: Vec<String> = (0..zip.len())
        .map(|i| zip.by_index_raw(i).unwrap().name().unwrap().to_string())
        .collect();
    assert_eq!(names, ["cue-list.csv", "Q1 Café.wav", "Act 1/Q2 Rain.wav"]);
    let csv = zip.by_index_raw(0).unwrap();
    assert_eq!(csv.compression(), zip::CompressionMethod::Deflated);
    drop(csv);
    for i in 1..3 {
        assert_eq!(
            zip.by_index_raw(i).unwrap().compression(),
            zip::CompressionMethod::Stored
        );
    }
    // Nothing left behind but the zip and the source.
    let mut left: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into())
        .collect();
    left.sort();
    assert_eq!(left, ["Show audio.zip", "x.wav"]);
    // The language-encoding flag (bit 11) on the non-ASCII name.
    let raw = std::fs::read(&out).unwrap();
    let name = "Q1 Café.wav".as_bytes();
    let at = raw.windows(name.len()).position(|w| w == name).unwrap();
    let flags = u16::from_le_bytes([raw[at - 30 + 6], raw[at - 30 + 7]]);
    assert_ne!(flags & 0x0800, 0, "UTF-8 flag");
}

#[test]
fn a_stopped_export_leaves_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let wav = dir.path().join("x.wav");
    std::fs::write(&wav, vec![0u8; 3 << 20]).unwrap();
    let out = dir.path().join("Show audio.zip");
    let mut z = AudioZip::create(&out).unwrap();
    let mut chunks = 0;
    assert!(
        !z.add_file("x.wav", &wav, &mut || {
            chunks += 1;
            chunks < 2
        })
        .unwrap()
    );
    drop(z);
    let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
    assert_eq!(left.len(), 1, "only the source: no zip, no temp file");
}

/// fixtures/audio-export/index.json: recognised before the legacy test; the .tqs inside is
/// imported, or the 7.5 refusal.
#[test]
fn audio_export_fixtures() {
    let dir = fixtures().join("audio-export");
    let index: Value =
        serde_json::from_slice(&std::fs::read(dir.join("index.json")).unwrap()).unwrap();
    let cases = index.as_array().unwrap();
    assert_eq!(cases.len(), 4);
    for case in cases {
        let file = dir.join(case["file"].as_str().unwrap());
        assert_eq!(
            tqs::detect_kind(&file).unwrap(),
            PackageKind::AudioExport,
            "{file:?}"
        );
        let tmp = tempfile::tempdir().unwrap();
        let inner = tmp.path().join("inner.tqs");
        let r = tqs::extract_audio_export_tqs(&file, &inner, Limits::default());
        match case["expect"].as_str().unwrap() {
            "import-inner-tqs" => {
                r.unwrap();
                let (m, _) =
                    tqs::read_package(&inner, &tmp.path().join("show"), Limits::default()).unwrap();
                let (want, _) = tqs::read_package(
                    &fixtures().join("valid/01-minimal.tqs"),
                    &tmp.path().join("want"),
                    Limits::default(),
                )
                .unwrap();
                assert_eq!(m.cues, want.cues);
                assert_eq!(m.show.name, want.show.name);
            }
            "reject-audio-export" => {
                let e = r.unwrap_err().to_string();
                assert_eq!(e, tqs::AUDIO_EXPORT_REFUSED, "{file:?}");
            }
            other => panic!("unknown expectation {other}"),
        }
    }
}

#[test]
fn shows_and_legacy_zips_are_not_audio_exports() {
    assert_eq!(
        tqs::detect_kind(&fixtures().join("valid/01-minimal.tqs")).unwrap(),
        PackageKind::Show
    );
    assert_eq!(
        tqs::detect_kind(&fixtures().join("legacy/web-export-STAGE-4821.zip")).unwrap(),
        PackageKind::LegacyWebExport
    );
}

/// 7.5: where cues are expected (.tqc/.tqg), an audio export says what it is.
#[test]
fn an_audio_export_offered_as_cues_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let err = tqs::read_package_of(
        &fixtures().join("audio-export/with-tqs.zip"),
        &tmp.path().join("x"),
        Limits::default(),
        &[tqs::Kind::Cues, tqs::Kind::Group],
        &mut |_| {},
    )
    .unwrap_err();
    assert_eq!(err.to_string(), tqs::AUDIO_EXPORT_FOR_CUES);
}
