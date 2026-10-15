use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sgraffito::canvas::{Doc, OutputAnnotations, Shape, ShapeKind, Stroke, TextItem};
use sgraffito::store::{self, Store};

fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sgraffito-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn sample() -> Doc {
    let mut doc = Doc::default();
    doc.outputs.insert(
        "eDP-1".into(),
        OutputAnnotations {
            strokes: vec![Stroke {
                color: "#e01b24".into(),
                width: 3.0,
                points: vec![[12.0, 40.0], [14.0, 44.0]],
            }],
            texts: vec![TextItem {
                x: 100.0,
                y: 200.0,
                color: "#ffffff".into(),
                size: 18.0,
                text: "买牛奶".into(), // CJK on purpose: multi-byte content must survive
            }],
            shapes: vec![Shape {
                kind: ShapeKind::Arrow,
                color: "#3584e4".into(),
                width: 3.0,
                from: [30.0, 400.0],
                to: [180.0, 260.0],
            }],
            notes: vec![],
        },
    );
    doc
}

#[test]
fn path_follows_xdg_data_home() {
    assert_eq!(
        store::path_in(Some("/xdg"), Some("/home/u")),
        Some(Path::new("/xdg/sgraffito/annotations.json").to_path_buf())
    );
}

#[test]
fn path_falls_back_to_home_local_share() {
    assert_eq!(
        store::path_in(None, Some("/home/u")),
        Some(Path::new("/home/u/.local/share/sgraffito/annotations.json").to_path_buf())
    );
    assert_eq!(store::path_in(None, None), None);
}

#[test]
fn round_trip_preserves_everything() {
    let dir = tmpdir("roundtrip");
    let p = dir.join("annotations.json");
    store::save(&p, &sample()).unwrap();
    assert_eq!(store::load(&p), sample());
}

#[test]
fn saved_file_has_version_and_output_buckets() {
    let dir = tmpdir("schema");
    let p = dir.join("annotations.json");
    store::save(&p, &sample()).unwrap();
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    assert_eq!(v["version"], 3);
    assert_eq!(v["outputs"]["eDP-1"]["strokes"][0]["points"][0][0], 12.0);
    assert_eq!(v["outputs"]["eDP-1"]["texts"][0]["text"], "买牛奶"); // CJK survives the round trip
    // A shape is two points and a kind, however large it would be drawn: no samples, no box.
    let shape = &v["outputs"]["eDP-1"]["shapes"][0];
    assert_eq!(shape["kind"], "arrow");
    assert_eq!(shape["from"], serde_json::json!([30.0, 400.0]));
    assert_eq!(shape["to"], serde_json::json!([180.0, 260.0]));
    assert_eq!(
        shape.as_object().unwrap().len(),
        5,
        "kind, color, width, from, to"
    );
}

#[test]
fn a_version_1_file_loads_and_is_not_backed_up() {
    let dir = tmpdir("v1");
    let p = dir.join("annotations.json");
    // What an earlier release wrote: no `shapes` key at all.
    std::fs::write(
        &p,
        r##"{"version":1,"outputs":{"eDP-1":{"strokes":[{"color":"#e01b24","width":3.0,"points":[[1.0,2.0]]}],"texts":[]}}}"##,
    )
    .unwrap();
    let doc = store::load(&p);
    assert_eq!(doc.outputs["eDP-1"].strokes.len(), 1, "the strokes load");
    assert!(
        doc.outputs["eDP-1"].shapes.is_empty(),
        "no shape is invented"
    );
    assert!(
        !dir.join("annotations.json.bak").exists(),
        "it is not backed up"
    );
}

#[test]
fn a_shape_survives_a_round_trip() {
    let dir = tmpdir("shapes");
    let p = dir.join("annotations.json");
    store::save(&p, &sample()).unwrap();
    let back = store::load(&p);
    assert_eq!(
        back.outputs["eDP-1"].shapes,
        sample().outputs["eDP-1"].shapes
    );
}

#[test]
fn missing_file_loads_empty_without_error() {
    let dir = tmpdir("missing");
    let p = dir.join("annotations.json");
    assert_eq!(store::load(&p), Doc::default());
    assert!(!dir.join("annotations.json.bak").exists());
}

#[test]
fn corrupt_file_is_backed_up_and_starts_empty() {
    let dir = tmpdir("corrupt");
    let p = dir.join("annotations.json");
    std::fs::write(&p, "{ not json").unwrap();
    assert_eq!(store::load(&p), Doc::default());
    assert_eq!(
        std::fs::read_to_string(dir.join("annotations.json.bak")).unwrap(),
        "{ not json"
    );
}

#[test]
fn unsupported_version_is_backed_up() {
    let dir = tmpdir("version");
    let p = dir.join("annotations.json");
    std::fs::write(&p, r#"{"version":99,"outputs":{}}"#).unwrap();
    assert_eq!(store::load(&p), Doc::default());
    assert!(dir.join("annotations.json.bak").exists());
}

#[test]
fn save_leaves_no_temp_files_and_is_repeatable() {
    let dir = tmpdir("atomic");
    let p = dir.join("annotations.json");
    store::save(&p, &sample()).unwrap();
    store::save(&p, &Doc::default()).unwrap();
    let names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["annotations.json".to_string()]);
    assert_eq!(store::load(&p), Doc::default());
}

#[test]
fn save_creates_missing_directories() {
    let dir = tmpdir("mkdir");
    let p = dir.join("nested/deeper/annotations.json");
    store::save(&p, &sample()).unwrap();
    assert_eq!(store::load(&p), sample());
}

#[test]
fn debounce_holds_for_one_second() {
    let dir = tmpdir("debounce");
    let mut s = Store::new(dir.join("annotations.json"));
    let t0 = Instant::now();
    s.mark_dirty(t0);
    assert!(!s.due(t0));
    assert!(!s.due(t0 + Duration::from_millis(999)));
    assert!(s.due(t0 + Duration::from_millis(1000)));
}

#[test]
fn flush_writes_and_clears_pending() {
    let dir = tmpdir("flush");
    let p = dir.join("annotations.json");
    let mut s = Store::new(p.clone());
    let t0 = Instant::now();
    s.mark_dirty(t0);
    s.flush(&sample()).unwrap();
    assert!(!s.due(t0 + Duration::from_secs(10)));
    assert_eq!(store::load(&p), sample());
}

#[test]
fn failed_flush_stays_pending_for_retry() {
    let dir = tmpdir("failed-flush");
    let blocker = dir.join("not-a-directory");
    std::fs::write(&blocker, "not a directory").unwrap();
    let mut s = Store::new(blocker.join("annotations.json"));
    let t0 = Instant::now();
    s.mark_dirty(t0);
    assert!(s.flush(&sample()).is_err());
    assert!(s.due(Instant::now() + Duration::from_secs(2)));
}
