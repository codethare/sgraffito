use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sgraffito::canvas::{
    Board, Doc, ItemId, Note, PEN_WIDTH_MAX, Shape, ShapeKind, Stroke, TEXT_SIZE_MAX, TextItem,
    View, link_source, stepped_size,
};
use sgraffito::store::{self, Store};

fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sgraffito-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The board record with `name`, wherever its encoded file name put it.
fn board_record(dir: &Path, name: &str) -> serde_json::Value {
    let boards = dir.join("boards");
    let mut names: Vec<String> = std::fs::read_dir(&boards)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    for file in names {
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(boards.join(&file)).unwrap()).unwrap();
        if value["name"] == name {
            return value;
        }
    }
    panic!("{name:?} has no board record in {boards:?}");
}

fn board_records(dir: &Path) -> Vec<serde_json::Value> {
    let boards = dir.join("boards");
    let mut names: Vec<String> = std::fs::read_dir(&boards)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".json"))
        .collect();
    names.sort();
    names
        .into_iter()
        .map(|file| {
            serde_json::from_str(&std::fs::read_to_string(boards.join(file)).unwrap()).unwrap()
        })
        .collect()
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
                attach: [None, None],
            }],
            texts: vec![TextItem {
                x: 100.0,
                y: 200.0,
                color: "#ffffff".into(),
                size: 18.0,
                text: "买牛奶".into(), // CJK on purpose: multi-byte content must survive
                link: None,
                container: None,
            }],
            shapes: vec![Shape {
                kind: ShapeKind::Arrow,
                color: "#3584e4".into(),
                width: 3.0,
                from: [30.0, 400.0],
                to: [180.0, 260.0],
                points: Vec::new(),
                roundness: None,
                rotate: 0.0,
                id: 1,
                attach: [None, None],
                fill: None,
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
fn the_manifest_names_the_boards_and_the_record_holds_one() {
    let dir = tmpdir("schema");
    let p = dir.join("annotations.json");
    let mut doc = sample();
    doc.active = "eDP-1".into();
    store::save(&p, &doc).unwrap();
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    assert_eq!(v["version"], 8);
    assert_eq!(v["active"], "eDP-1");
    assert_eq!(v["boards"], serde_json::json!(["eDP-1"]));
    assert!(
        v["boards"]["eDP-1"].is_null(),
        "the manifest must not carry the drawing"
    );

    let record = board_record(&dir, "eDP-1");
    assert_eq!(record["version"], 8);
    let board = &record["board"];
    assert_eq!(board["strokes"][0]["points"][0][0], 12.0);
    assert_eq!(board["texts"][0]["text"], "买牛奶"); // CJK survives the round trip
    // A shape is two points and a kind, however large it would be drawn: no samples, no box.
    let shape = &board["shapes"][0];
    assert_eq!(shape["kind"], "arrow");
    assert_eq!(shape["from"], serde_json::json!([30.0, 400.0]));
    assert_eq!(shape["to"], serde_json::json!([180.0, 260.0]));
    assert_eq!(
        shape.as_object().unwrap().len(),
        7,
        "kind, color, width, from, to, id, attach"
    );
}

/// Every board has its own record, so one board's save cannot rewrite another board's drawing.
#[test]
fn each_board_has_its_own_file() {
    let dir = tmpdir("one-file-per-board");
    let p = dir.join("annotations.json");
    let mut doc = sample();
    doc.boards.insert("second".into(), Board::default());
    doc.active = "second".into();
    store::save(&p, &doc).unwrap();

    let records = board_records(&dir);
    assert_eq!(records.len(), 2);
    assert!(
        records.iter().any(|record| record["name"] == "eDP-1"),
        "the first board has no record"
    );
    assert!(
        records.iter().any(|record| record["name"] == "second"),
        "the second board has no record"
    );
    assert_eq!(store::load(&p), doc);
}

/// There is no board-count ceiling: every board gets its own record and the manifest lists them all.
#[test]
fn many_boards_round_trip_through_their_own_files() {
    let dir = tmpdir("many-boards");
    let p = dir.join("annotations.json");
    let mut doc = Doc::default();
    for i in 0..64 {
        doc.boards.insert(format!("board {i}"), Board::default());
    }
    doc.active = "board 42".into();
    store::save(&p, &doc).unwrap();
    assert_eq!(board_records(&dir).len(), 64);
    let back = store::load(&p);
    assert_eq!(back.boards.len(), 64);
    assert_eq!(back.active, "board 42");
}

/// A board that is renamed or deleted loses its old record; a new board gains one.
#[test]
fn board_records_follow_rename_and_deletion() {
    let dir = tmpdir("board-records-move");
    let p = dir.join("annotations.json");
    let mut doc = sample();
    doc.boards.insert("second".into(), Board::default());
    store::save(&p, &doc).unwrap();
    assert_eq!(board_records(&dir).len(), 2);

    assert!(doc.rename_board("second", "renamed"));
    doc.delete_board("eDP-1").unwrap();
    store::save(&p, &doc).unwrap();
    let records = board_records(&dir);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["name"], "renamed");
    assert_eq!(store::load(&p), doc);
}

/// A board name may contain path separators and non-ASCII text; its file name is encoded, not
/// interpreted as a directory.
#[test]
fn a_board_name_is_safe_as_a_file_name() {
    let dir = tmpdir("board-name");
    let p = dir.join("annotations.json");
    let name = "a/b % 你好";
    let mut doc = Doc::default();
    doc.boards.insert(name.into(), Board::default());
    doc.active = name.into();
    store::save(&p, &doc).unwrap();
    assert_eq!(store::load(&p).boards[name].strokes.len(), 0);
    assert_eq!(board_records(&dir)[0]["name"], name);
    assert_eq!(
        std::fs::read_dir(dir.join("boards")).unwrap().count(),
        1,
        "the encoded name created more than one record"
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
fn a_turned_shape_survives_the_round_trip_and_an_old_file_loads_upright() {
    let dir = tmpdir("shape-rotation");
    let p = dir.join("annotations.json");
    let mut doc = sample();
    let angle = std::f32::consts::FRAC_PI_4;
    doc.boards.get_mut("eDP-1").unwrap().shapes[0].rotate = angle;
    store::save(&p, &doc).unwrap();

    let saved = board_record(&dir, "eDP-1")["board"]["shapes"][0]["rotate"]
        .as_f64()
        .unwrap();
    assert!((saved as f32 - angle).abs() < 1e-6, "the angle is written");
    let loaded = store::load(&p).boards["eDP-1"].shapes[0].rotate;
    assert!((loaded - angle).abs() < 1e-6, "the angle round-trips");

    // A file from before rotation existed has no key at all; it loads with the shape upright.
    std::fs::write(
        &p,
        r##"{"version":6,"boards":{"old":{"shapes":[{"kind":"rect","color":"#ffffff","width":2.0,"from":[0.0,0.0],"to":[10.0,10.0]}]}}}"##,
    )
    .unwrap();
    let old = store::load(&p);
    assert_eq!(old.boards["old"].shapes[0].rotate, 0.0);
    assert_eq!(old.boards["old"].shapes[0].outline()[0], [0.0, 0.0]);
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

/// The manifest is only an index. Losing it must not lose the board records beside it.
#[test]
fn a_corrupt_manifest_recovers_the_board_records() {
    let dir = tmpdir("recover-manifest");
    let p = dir.join("annotations.json");
    let doc = sample();
    store::save(&p, &doc).unwrap();
    std::fs::write(&p, "{ not json").unwrap();
    let recovered = store::load(&p);
    assert_eq!(recovered.boards, doc.boards);
    assert!(dir.join("annotations.json.bak").exists());
}

#[test]
fn a_missing_manifest_recovers_the_board_records() {
    let dir = tmpdir("recover-missing-manifest");
    let p = dir.join("annotations.json");
    let doc = sample();
    store::save(&p, &doc).unwrap();
    std::fs::remove_file(&p).unwrap();
    let recovered = store::load(&p);
    assert_eq!(recovered.boards, doc.boards);
    assert_eq!(recovered.active, "", "no index means no picked board");
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
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec!["annotations.json".to_string(), "boards".to_string()]
    );
    assert_eq!(
        std::fs::read_dir(dir.join("boards")).unwrap().count(),
        0,
        "the removed board's record survived"
    );
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

    // Loading an old single file already splits it into one board record per board.
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    assert_eq!(v["version"], 8);
    assert_eq!(v["boards"], serde_json::json!(["DP-2", "eDP-1"]));
    assert!(board_record(&dir, "eDP-1")["board"].is_object());
    assert!(board_record(&dir, "DP-2")["board"].is_object());
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

/// A size step walks what the selection holds: a stroke's and a shape's width by the pen step, a text
/// box's size by the text step, and a note not at all — its type size is its own.
#[test]
fn stepping_a_selection_steps_its_widths_and_text_sizes() {
    let mut board = Board {
        strokes: vec![Stroke {
            color: "#e01b24".into(),
            width: 4.0,
            points: vec![[10.0, 10.0], [110.0, 10.0]],
            attach: [None, None],
        }],
        texts: vec![TextItem {
            x: 10.0,
            y: 40.0,
            color: "#ffffff".into(),
            size: 18.0,
            text: "hi".into(),
            link: None,
            container: None,
        }],
        shapes: vec![Shape {
            kind: ShapeKind::Rect,
            color: "#33d17a".into(),
            width: 6.0,
            from: [10.0, 80.0],
            to: [110.0, 140.0],
            points: Vec::new(),
            roundness: None,
            rotate: 0.0,
            id: 0,
            attach: [None, None],
            fill: None,
        }],
        notes: vec![Note {
            x: 10.0,
            y: 160.0,
            w: 100.0,
            h: 100.0,
            color: "#f6d32d".into(),
            text: "hold".into(),
            created: "2026/11/12".into(),
        }],
    };
    let note = board.notes[0].clone();
    let items = [
        ItemId::Stroke(0),
        ItemId::Text(0),
        ItemId::Shape(0),
        ItemId::Note(0),
    ];
    let box_ = board
        .step_size(&items, 1.0)
        .expect("a step that changes something reports its box");
    assert_eq!(board.strokes[0].width, 5.0);
    assert_eq!(board.shapes[0].width, 7.0);
    assert_eq!(board.texts[0].size, 20.0);
    assert_eq!(board.notes[0], note, "a note changed its type size");
    // The box covers the ink the step produced, or the frame leaves the thicker edge behind.
    let after = board.texts[0].paint_bounds();
    assert!(box_.contains(after.x, after.y));
    assert!(box_.contains(after.x + after.w, after.y + after.h));
}

/// Nothing to step is not a change: every held item already at its bound leaves the board equal and
/// reports no box, so a press there cannot dirty the file or repaint anything.
#[test]
fn a_size_step_at_the_bound_writes_nothing() {
    let mut board = Board {
        strokes: vec![Stroke {
            color: "#e01b24".into(),
            width: PEN_WIDTH_MAX,
            points: vec![[10.0, 10.0], [110.0, 10.0]],
            attach: [None, None],
        }],
        texts: vec![TextItem {
            x: 10.0,
            y: 40.0,
            color: "#ffffff".into(),
            size: TEXT_SIZE_MAX,
            text: "hi".into(),
            link: None,
            container: None,
        }],
        ..Default::default()
    };
    let before = board.clone();
    assert_eq!(
        board.step_size(&[ItemId::Stroke(0), ItemId::Text(0)], 1.0),
        None
    );
    assert_eq!(board, before);
}

/// The step is a fixed amount on either side and stops at the range's ends.
#[test]
fn the_size_step_clamps_to_its_range() {
    assert_eq!(stepped_size(3.0, 1.0, 1.0, 1.0, 16.0), 4.0);
    assert_eq!(stepped_size(3.0, -1.0, 1.0, 1.0, 16.0), 2.0);
    assert_eq!(stepped_size(1.0, -1.0, 1.0, 1.0, 16.0), 1.0);
    assert_eq!(stepped_size(16.0, 1.0, 1.0, 1.0, 16.0), 16.0);
    assert_eq!(stepped_size(72.0, 1.0, 2.0, 8.0, 72.0), 72.0);
}

/// A shrinking step reports the ink it left, not only the ink it produced: the old, wider edge is what a
/// frame would otherwise leave behind.
#[test]
fn a_shrinking_step_reports_the_ink_it_left() {
    let mut board = Board {
        strokes: vec![Stroke {
            color: "#e01b24".into(),
            width: 5.0,
            points: vec![[10.0, 10.0], [110.0, 10.0]],
            attach: [None, None],
        }],
        texts: vec![TextItem {
            x: 10.0,
            y: 40.0,
            color: "#ffffff".into(),
            size: 20.0,
            text: "hi".into(),
            link: None,
            container: None,
        }],
        ..Default::default()
    };
    let (stroke, text) = (
        board.strokes[0].paint_bounds(),
        board.texts[0].paint_bounds(),
    );
    let box_ = board
        .step_size(&[ItemId::Stroke(0), ItemId::Text(0)], -1.0)
        .expect("a shrink is a change");
    assert_eq!(board.strokes[0].width, 4.0);
    assert_eq!(board.texts[0].size, 18.0);
    for old in [stroke, Some(text)].into_iter().flatten() {
        assert!(box_.contains(old.x, old.y));
        assert!(box_.contains(old.x + old.w, old.y + old.h));
    }
}

/// When only some of what is held can step, the box is exactly the ink of the items that did.
#[test]
fn a_mixed_selection_reports_only_what_changed() {
    let mut board = Board {
        strokes: vec![Stroke {
            color: "#e01b24".into(),
            width: PEN_WIDTH_MAX,
            points: vec![[10.0, 10.0], [110.0, 10.0]],
            attach: [None, None],
        }],
        texts: vec![TextItem {
            x: 10.0,
            y: 40.0,
            color: "#ffffff".into(),
            size: 18.0,
            text: "hi".into(),
            link: None,
            container: None,
        }],
        ..Default::default()
    };
    let before = board.texts[0].paint_bounds();
    let box_ = board
        .step_size(&[ItemId::Stroke(0), ItemId::Text(0)], 1.0)
        .expect("the text box can still grow");
    let after = board.texts[0].paint_bounds();
    assert_eq!(
        board.strokes[0].width, PEN_WIDTH_MAX,
        "a stroke at its bound stepped"
    );
    assert_eq!(board.texts[0].size, 20.0);
    assert_eq!(
        box_,
        before.union(after),
        "the box is not the text's ink alone"
    );
}

/// A rename moves the drawing with the name: the board's content and the picked board follow it, and
/// nothing is left under the old key.
#[test]
fn renaming_a_board_moves_its_drawing() {
    let mut doc = Doc::default();
    doc.boards.insert("laptop".into(), Board::default());
    doc.boards.insert("monitor".into(), Board::default());
    doc.boards.get_mut("laptop").unwrap().notes.push(Note::new(
        0.0,
        0.0,
        "#ffffff",
        "2026/11/17".into(),
    ));
    doc.active = "laptop".into();
    assert!(doc.rename_board("laptop", "left"));
    assert_eq!(doc.active, "left");
    assert_eq!(doc.boards["left"].notes.len(), 1);
    assert!(!doc.boards.contains_key("laptop"));
}

/// A name the document cannot hold is refused rather than half-applied: empty, taken, or not a board.
#[test]
fn a_rename_refuses_a_name_it_cannot_take() {
    let mut doc = Doc::default();
    doc.boards.insert("laptop".into(), Board::default());
    doc.boards.insert("monitor".into(), Board::default());
    let before = doc.clone();
    for bad in ["", "monitor"] {
        assert!(!doc.rename_board("laptop", bad), "{bad:?} was taken");
        assert_eq!(doc, before, "{bad:?} left the document changed");
    }
    assert!(!doc.rename_board("desk", "drawing"));
    assert_eq!(doc, before);
}

/// A rename can move a board's row: the list is ordered by name, so a press resolved after a rename
/// commit would act on a different board than the one it landed on.
#[test]
fn a_rename_can_move_a_board_row() {
    let mut doc = Doc::default();
    doc.boards.insert("a".into(), Board::default());
    doc.boards.insert("z".into(), Board::default());
    assert_eq!(doc.names().cloned().collect::<Vec<_>>(), ["a", "z"]);
    assert!(doc.rename_board("a", "zz"));
    assert_eq!(doc.names().cloned().collect::<Vec<_>>(), ["z", "zz"]);
}

/// A link is the text's, and it is written: a version 4 file has none, and a round trip at the
/// current version keeps the link.
#[test]
fn a_version_4_file_loads_and_a_link_survives_the_round_trip() {
    let dir = tmpdir("links-roundtrip");
    let p = dir.join("annotations.json");
    std::fs::write(
        &p,
        r##"{"version":4,"boards":{"a":{"texts":[{"x":8,"y":8,"color":"#ffffff","size":18,"text":"next"}]}},"active":"a"}"##,
    )
    .unwrap();
    let mut doc = store::load(&p);
    assert_eq!(doc.boards["a"].texts[0].link, None);
    doc.boards.get_mut("a").unwrap().texts[0].link = Some("b".into());
    doc.boards.insert("b".into(), Board::default());
    store::save(&p, &doc).unwrap();
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    assert_eq!(saved["version"], 8);
    assert_eq!(store::load(&p).boards["a"].texts[0].link, Some("b".into()));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A link names a board by name, so deletion clears every link that named it and a rename moves
/// them to the new name.
#[test]
fn links_follow_a_board_rename_and_are_dropped_with_its_deletion() {
    let mut doc = Doc::default();
    for name in ["a", "b", "c"] {
        doc.boards.insert(name.into(), Board::default());
    }
    doc.boards.get_mut("a").unwrap().texts.push(TextItem {
        container: None,
        x: 0.0,
        y: 0.0,
        color: "#ffffff".into(),
        size: 18.0,
        text: "go".into(),
        link: Some("c".into()),
    });
    assert!(doc.rename_board("c", "d"));
    assert_eq!(doc.boards["a"].texts[0].link, Some("d".into()));
    doc.delete_board("d");
    assert_eq!(
        doc.boards["a"].texts[0].link, None,
        "a link to a gone board survived"
    );
}

/// Moving elements removes them from the source and appends copies to the target; links are content
/// and travel with their text.
#[test]
fn moving_elements_leaves_the_source_board_without_them() {
    let mut doc = Doc::default();
    doc.boards.insert("a".into(), Board::default());
    doc.boards.insert("b".into(), Board::default());
    let board = doc.boards.get_mut("a").unwrap();
    board.strokes.push(Stroke {
        color: "#ffffff".into(),
        width: 3.0,
        points: vec![[0.0, 0.0], [10.0, 10.0]],
        attach: [None, None],
    });
    board.texts.push(TextItem {
        container: None,
        x: 0.0,
        y: 0.0,
        color: "#ffffff".into(),
        size: 18.0,
        text: "go".into(),
        link: Some("b".into()),
    });
    let moved = doc.move_items("a", "b", &[ItemId::Stroke(0), ItemId::Text(0)]);
    assert!(moved.is_some());
    assert!(
        doc.boards["a"].strokes.is_empty(),
        "the source kept a stroke"
    );
    assert!(doc.boards["a"].texts.is_empty(), "the source kept a text");
    assert_eq!(doc.boards["b"].strokes.len(), 1);
    assert_eq!(doc.boards["b"].texts.len(), 1);
    assert_eq!(doc.boards["b"].texts[0].link, Some("b".into()));
    assert_eq!(doc.move_items("a", "a", &[ItemId::Stroke(0)]), None);
}

/// Only one text box is a link source, and following a link keeps the zoom and centres the point.
#[test]
fn a_link_source_is_one_text_box_and_the_jump_centres_it() {
    let board = Board {
        texts: vec![
            TextItem {
                x: 0.0,
                y: 0.0,
                color: "#fff".into(),
                size: 18.0,
                text: "go".into(),
                link: Some("b".into()),
                container: None,
            },
            TextItem {
                x: 0.0,
                y: 0.0,
                color: "#fff".into(),
                size: 18.0,
                text: "no".into(),
                link: None,
                container: None,
            },
        ],
        ..Board::default()
    };
    assert_eq!(link_source(&board, &[ItemId::Text(0)]), Some(0));
    assert_eq!(link_source(&board, &[ItemId::Text(1)]), Some(1));
    assert_eq!(link_source(&board, &[]), None);
    assert_eq!(
        link_source(&board, &[ItemId::Text(0), ItemId::Text(1)]),
        None
    );
    let view = View {
        zoom: 2.0,
        offset: [10.0, 20.0],
    }
    .centred_on((800.0, 600.0), [100.0, 50.0]);
    assert_eq!(view.zoom, 2.0);
    assert_eq!(view.to_screen([100.0, 50.0]), [400.0, 300.0]);
}

/// Shape ids and connector attachments are document data: a save and load keeps both, so a connector
/// is still attached after the daemon restarts.
#[test]
fn connector_attachments_survive_the_round_trip() {
    use sgraffito::canvas::{Attachment, Shape, ShapeKind};
    let dir = tmpdir("connector-roundtrip");
    let path = dir.join("annotations.json");
    let mut doc = Doc::default();
    let mut board = Board::default();
    board.add_shape(Shape {
        roundness: None,
        points: Vec::new(),
        kind: ShapeKind::Rect,
        color: "#ffffff".into(),
        width: 3.0,
        from: [0.0, 0.0],
        to: [100.0, 100.0],
        rotate: 0.0,
        id: 0,
        attach: [None, None],
        fill: None,
    });
    let target = board.shapes[0].id;
    board.add_shape(Shape {
        roundness: None,
        points: Vec::new(),
        kind: ShapeKind::Line,
        color: "#ffffff".into(),
        width: 3.0,
        from: [0.0, 50.0],
        to: [200.0, 50.0],
        rotate: 0.0,
        id: 0,
        attach: [
            Some(Attachment {
                shape: target,
                at: [-1.0, 0.0],
            }),
            None,
        ],
        fill: None,
    });
    doc.boards.insert("a".into(), board);
    store::save(&path, &doc).unwrap();
    let loaded = store::load(&path);
    let board = &loaded.boards["a"];
    assert_eq!(board.shapes[0].id, target);
    assert_eq!(
        board.shapes[1].attach[0].map(|attachment| attachment.shape),
        Some(target)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_version_seven_file_loads_and_is_saved_back_as_eight() {
    let dir = tmpdir("version-seven");
    let p = dir.join("annotations.json");
    std::fs::write(
        &p,
        r##"{"version":7,"boards":{"kept":{"strokes":[{"color":"#e01b24","width":3,"points":[[0,0]],"id":9,"attach":[null,null]}],"texts":[{"x":5,"y":6,"color":"#ffffff","size":18,"text":"kept","attach":{"target":{"stroke":9},"offset":[1,2]}}],"shapes":[]}},"active":"kept"}"##,
    )
    .unwrap();
    let doc = store::load(&p);
    assert_eq!(doc.active, "kept");
    assert_eq!(doc.boards["kept"].texts[0].text, "kept");
    assert!(
        !dir.join("annotations.json.bak").exists(),
        "a readable newer file was treated as unusable"
    );

    store::save(&p, &doc).unwrap();
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    assert_eq!(v["version"], 8);
    let board = &board_record(&dir, "kept")["board"];
    assert!(
        board["strokes"][0].get("id").is_none(),
        "the removed stroke id survived the downgrade"
    );
    assert!(
        board["texts"][0].get("attach").is_none(),
        "the removed text attachment survived the downgrade"
    );
    assert_eq!(board["texts"][0]["text"], "kept");
}

#[test]
fn a_missing_main_file_recovers_a_readable_backup() {
    let dir = tmpdir("recover-backup");
    let p = dir.join("annotations.json");
    let bak = p.with_extension("json.bak");
    std::fs::write(
        &bak,
        r##"{"version":7,"boards":{"kept":{"strokes":[],"texts":[{"x":5,"y":6,"color":"#ffffff","size":18,"text":"recovered"}],"shapes":[]}},"active":"kept"}"##,
    )
    .unwrap();
    let doc = store::load(&p);
    assert_eq!(doc.boards["kept"].texts[0].text, "recovered");
    assert!(p.exists(), "the recovered document was not put back");
    assert!(bak.exists(), "recovery consumed the backup");
}

#[test]
fn a_corrupt_backup_is_left_alone_when_the_main_file_is_missing() {
    let dir = tmpdir("bad-backup");
    let p = dir.join("annotations.json");
    let bak = p.with_extension("json.bak");
    std::fs::write(&bak, "{ not json").unwrap();
    assert_eq!(store::load(&p), Doc::default());
    assert!(!p.exists());
    assert!(bak.exists());
}
