use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sgraffito::canvas::{Board, Doc, Shape, ShapeKind, Stroke, TextItem};
use sgraffito::store::{self, Store};

fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sgraffito-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn sample() -> Doc {
    let mut doc = Doc::default();
    doc.boards.insert(
        "eDP-1".into(),
        Board {
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
fn saved_file_has_version_boards_and_the_picked_one() {
    let dir = tmpdir("schema");
    let p = dir.join("annotations.json");
    let mut doc = sample();
    doc.active = "eDP-1".into();
    store::save(&p, &doc).unwrap();
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    assert_eq!(v["version"], 4);
    assert_eq!(v["active"], "eDP-1");
    assert_eq!(v["boards"]["eDP-1"]["strokes"][0]["points"][0][0], 12.0);
    assert_eq!(v["boards"]["eDP-1"]["texts"][0]["text"], "买牛奶"); // CJK survives the round trip
    // A shape is two points and a kind, however large it would be drawn: no samples, no box.
    let shape = &v["boards"]["eDP-1"]["shapes"][0];
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
    assert_eq!(doc.boards["eDP-1"].strokes.len(), 1, "the strokes load");
    assert!(
        doc.boards["eDP-1"].shapes.is_empty(),
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
    assert_eq!(back.boards["eDP-1"].shapes, sample().boards["eDP-1"].shapes);
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

/// A board created without a name takes the smallest number no board already holds, so creating boards
/// never collides with one loaded from the file.
#[test]
fn a_new_board_takes_the_first_free_number() {
    let mut doc = Doc::default();
    doc.boards.insert("Board 1".into(), Board::default());
    doc.boards.insert("Board 3".into(), Board::default());
    assert_eq!(doc.new_board(), "Board 2");
    assert!(doc.boards.contains_key("Board 2"));
    assert_eq!(doc.new_board(), "Board 4");
}

/// The board picked last is what a screen that appears afterwards lands on: shutting the laptop and
/// connecting a monitor has to show the drawing that was picked, not an empty one.
#[test]
fn a_new_output_adopts_the_board_picked_last() {
    let mut doc = Doc::default();
    doc.new_board();
    let office = doc.new_board();
    doc.active = office.clone();
    assert_eq!(doc.adopt("eDP-1"), office);
}

/// A file written before boards existed names each bucket after its output, so the name match is what
/// puts a two-screen setup back exactly as it was.
#[test]
fn a_migrated_document_puts_each_screen_back_on_its_own_board() {
    let mut doc = Doc::default();
    doc.boards.insert("eDP-1".into(), Board::default());
    doc.boards.insert("DP-2".into(), Board::default());
    assert_eq!(doc.active, "");
    assert_eq!(doc.adopt("DP-2"), "DP-2");
    assert_eq!(doc.adopt("eDP-1"), "eDP-1");
}

/// An output always gets a board, and adoption never writes the picked board, or the second screen
/// would inherit the first one's drawing.
#[test]
fn an_output_always_gets_a_board() {
    let mut doc = Doc::default();
    assert_eq!(doc.adopt("eDP-1"), "Board 1");
    assert!(doc.boards.contains_key("Board 1"));
    assert_eq!(doc.active, "");

    let mut doc = Doc::default();
    doc.boards.insert("zeta".into(), Board::default());
    assert_eq!(doc.adopt("eDP-1"), "zeta");
    assert_eq!(doc.active, "");

    let mut doc = Doc::default();
    doc.boards.insert("a".into(), Board::default());
    doc.active = "gone".into();
    assert_eq!(doc.adopt("eDP-1"), "a");
}

/// The file an earlier release wrote keys its annotations by output name. Those become boards of the
/// same name, so nothing is lost and no screen is emptied by the upgrade.
#[test]
fn a_version_3_file_becomes_boards() {
    let dir = tmpdir("v3-boards");
    let p = dir.join("annotations.json");
    std::fs::write(
        &p,
        r##"{"version":3,"outputs":{"eDP-1":{"strokes":[{"color":"#e01b24","width":3.0,"points":[[1.0,2.0]]}],"texts":[]},"DP-2":{"notes":[]}}}"##,
    )
    .unwrap();
    let mut doc = store::load(&p);
    assert_eq!(doc.boards["eDP-1"].strokes.len(), 1, "the strokes load");
    assert!(doc.boards.contains_key("DP-2"), "the other output's board");
    assert_eq!(doc.active, "", "nothing was picked in a version 3 file");
    assert!(
        !dir.join("annotations.json.bak").exists(),
        "it is not backed up"
    );
    assert_eq!(doc.adopt("DP-2"), "DP-2", "each screen keeps its own board");

    // The next save is the current version.
    store::save(&p, &doc).unwrap();
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    assert_eq!(v["version"], 4);
    assert!(v["boards"]["eDP-1"].is_object());
}

/// Which board was picked is content of the file: it is what a screen connected tomorrow lands on.
#[test]
fn the_picked_board_survives_the_round_trip() {
    let dir = tmpdir("active");
    let p = dir.join("annotations.json");
    let mut doc = Doc::default();
    doc.new_board();
    let office = doc.new_board();
    doc.active = office.clone();
    store::save(&p, &doc).unwrap();
    let mut back = store::load(&p);
    assert_eq!(back.active, office);
    assert_eq!(back.adopt("eDP-1"), office);
}

/// A name that arrives after the surface was made re-runs the same rule, so a board picked last still wins
/// over the board named after the output: otherwise a migrated user who picked `office` would be put back
/// on the old per-output board on every start-up.
#[test]
fn a_late_name_does_not_override_the_board_picked_last() {
    let mut doc = Doc::default();
    doc.boards.insert("eDP-1".into(), Board::default());
    doc.boards.insert("DP-2".into(), Board::default());
    // Nothing picked yet: the name match is the migration, and it moves the output.
    assert_eq!(doc.readopt("DP-2", false, "eDP-1"), Some("DP-2".into()));
    assert_eq!(doc.readopt("eDP-1", false, "eDP-1"), None, "already there");
    // Picked: the picked board wins, every time.
    doc.active = "eDP-1".into();
    assert_eq!(doc.readopt("DP-2", false, "eDP-1"), None);
    // And a board chosen by hand is never moved, whatever `active` says.
    doc.active = "DP-2".into();
    assert_eq!(doc.readopt("DP-2", true, "eDP-1"), None);
}

/// Switching "every screen" on adopts the focused output's board, else the one picked last, else the only
/// board there is — the order the design names, because a key binding while locked has no focused output.
#[test]
fn the_shared_board_is_the_focused_one_then_the_picked_one() {
    let mut doc = Doc::default();
    doc.boards.insert("eDP-1".into(), Board::default());
    doc.boards.insert("office".into(), Board::default());
    assert_eq!(
        doc.shared_board(Some("office"), Some("eDP-1")),
        Some("office".into())
    );
    doc.active = "eDP-1".into();
    assert_eq!(
        doc.shared_board(None, Some("office")),
        Some("eDP-1".into()),
        "the board picked last comes before an arbitrary output"
    );
    doc.active = "gone".into();
    assert_eq!(
        doc.shared_board(None, Some("office")),
        Some("office".into()),
        "with nothing picked, any board beats none"
    );
    assert_eq!(Doc::default().shared_board(None, None), None);
}

/// A board can be removed, and the board that takes its place is the first one the document still has,
/// so which board a screen lands on after a deletion is the document's decision, not the pointer's.
#[test]
fn deleting_a_board_names_the_first_remaining_board() {
    let mut doc = Doc::default();
    doc.boards.insert("Board 1".into(), Board::default());
    doc.boards.insert("Board 3".into(), Board::default());
    doc.boards.insert("office".into(), Board::default());
    assert_eq!(doc.delete_board("Board 3"), Some("Board 1".to_string()));
    assert!(!doc.boards.contains_key("Board 3"));
    assert!(doc.boards.contains_key("Board 1"));
    assert!(doc.boards.contains_key("office"));
}

/// A document with no boards is nowhere to draw, so the last one is refused rather than removed.
#[test]
fn the_only_board_cannot_be_deleted() {
    let mut doc = Doc::default();
    doc.boards.insert("office".into(), Board::default());
    assert_eq!(doc.delete_board("office"), None);
    assert!(doc.boards.contains_key("office"));
}

/// A name the document does not hold changes nothing: the row a click lands on and the document can
/// disagree by a frame, and that must not empty a board or the map.
#[test]
fn deleting_a_board_that_is_not_there_changes_nothing() {
    let mut doc = Doc::default();
    doc.boards.insert("Board 1".into(), Board::default());
    doc.boards.insert("office".into(), Board::default());
    let before = doc.clone();
    assert_eq!(doc.delete_board("gone"), None);
    assert_eq!(doc, before);
}

/// The board picked last must not be the one just deleted: a screen that appears afterwards lands on
/// the board an output adopted, and that name has to exist.
#[test]
fn deleting_the_picked_board_moves_the_pick_to_the_one_that_takes_its_place() {
    let mut doc = Doc::default();
    doc.boards.insert("Board 1".into(), Board::default());
    doc.boards.insert("office".into(), Board::default());
    doc.active = "office".into();
    assert_eq!(doc.delete_board("office"), Some("Board 1".to_string()));
    assert_eq!(doc.active, "Board 1");
}
