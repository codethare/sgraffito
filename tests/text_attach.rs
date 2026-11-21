//! Text attachment: snapping to a drawing's ink, following the drawing, and surviving a copy or a
//! round trip through the file.

use std::path::PathBuf;

use sgraffito::canvas::{
    Board, ItemId, Shape, ShapeKind, Stroke, TEXT_SNAP_GAP, TextAttachment, TextItem, TextTarget,
    Transform,
};
use sgraffito::sketch::SKETCH_SLACK;
use sgraffito::store;

fn text(x: f32, y: f32, value: &str) -> TextItem {
    TextItem {
        x,
        y,
        color: "#ffffff".into(),
        size: 18.0,
        text: value.into(),
        link: None,
        attach: None,
    }
}

fn stroke(points: Vec<[f32; 2]>, id: u64) -> Stroke {
    Stroke {
        color: "#e01b24".into(),
        width: 3.0,
        points,
        id,
        attach: [None, None],
    }
}

fn shape(kind: ShapeKind, from: [f32; 2], to: [f32; 2], id: u64) -> Shape {
    Shape {
        kind,
        color: "#e01b24".into(),
        width: 3.0,
        from,
        to,
        rotate: 0.0,
        id,
        attach: [None, None],
    }
}

fn board(strokes: Vec<Stroke>, texts: Vec<TextItem>, shapes: Vec<Shape>) -> Board {
    Board {
        strokes,
        texts,
        shapes,
        notes: vec![],
    }
}

fn ink(width: f32) -> f32 {
    width / 2.0 + SKETCH_SLACK + TEXT_SNAP_GAP
}

#[test]
fn a_text_dragged_to_a_rectangle_lands_just_inside_its_border() {
    let mut board = board(
        vec![],
        vec![text(-10.0, 40.0, "hi")],
        vec![shape(ShapeKind::Rect, [0.0, 0.0], [200.0, 100.0], 1)],
    );
    let translation = Transform::Translate { dx: 0.0, dy: 0.0 };
    let snap = board
        .snap_text(&[ItemId::Text(0)], translation, 12.0)
        .expect("the text meets the rectangle");
    assert_eq!(snap.target, TextTarget::Shape(1));

    board.texts[0] = snap.transform.text(&board.texts[0]);
    let bounds = board.texts[0].bounds();
    // The box's near side sits the ink's reach plus the visual gap inside the border.
    assert!(
        (bounds.x - ink(3.0)).abs() < 0.01,
        "left inset was {}",
        bounds.x
    );
    assert!(bounds.y >= 0.0 && bounds.y + bounds.h <= 100.0);
}

#[test]
fn a_text_dragged_to_a_circle_lands_inside_it() {
    let mut board = board(
        vec![],
        vec![text(2.0, 40.0, "hi")],
        vec![shape(ShapeKind::Circle, [0.0, 0.0], [200.0, 100.0], 4)],
    );
    let snap = board
        .snap_text(
            &[ItemId::Text(0)],
            Transform::Translate { dx: 0.0, dy: 0.0 },
            12.0,
        )
        .expect("the text meets the circle");
    assert_eq!(snap.target, TextTarget::Shape(4));
    board.texts[0] = snap.transform.text(&board.texts[0]);
    let bounds = board.texts[0].bounds();
    assert!(bounds.x >= 0.0 && bounds.x + bounds.w <= 200.0);
    assert!(bounds.y >= 0.0 && bounds.y + bounds.h <= 100.0);
    assert!(bounds.x < 30.0, "the box stayed near the approached border");
}

#[test]
fn a_text_dragged_to_a_line_stays_on_the_side_it_approached() {
    let mut board = board(
        vec![],
        vec![text(40.0, 70.0, "hi"), text(40.0, 110.0, "lo")],
        vec![shape(ShapeKind::Line, [0.0, 100.0], [200.0, 100.0], 9)],
    );
    // Each box is near the line from its own side; separate snap calls keep them independent so
    // the assertion can name the side.
    let above = board
        .snap_text(
            &[ItemId::Text(0)],
            Transform::Translate { dx: 0.0, dy: 0.0 },
            12.0,
        )
        .expect("the upper text meets the line");
    assert_eq!(above.target, TextTarget::Shape(9));
    board.texts[0] = above.transform.text(&board.texts[0]);
    let above_bounds = board.texts[0].bounds();
    let clear = ink(3.0);
    assert!(
        (100.0 - (above_bounds.y + above_bounds.h) - clear).abs() < 0.01,
        "the upper box was {} from the ink",
        100.0 - (above_bounds.y + above_bounds.h)
    );

    let below = board
        .snap_text(
            &[ItemId::Text(1)],
            Transform::Translate { dx: 0.0, dy: 0.0 },
            12.0,
        )
        .expect("the lower text meets the line");
    board.texts[1] = below.transform.text(&board.texts[1]);
    let below_bounds = board.texts[1].bounds();
    assert!(
        (below_bounds.y - 100.0 - clear).abs() < 0.01,
        "the lower box was {} from the ink",
        below_bounds.y - 100.0
    );
}

#[test]
fn a_text_dragged_to_a_pen_stroke_stays_beside_its_ink() {
    let mut board = board(
        vec![stroke(vec![[0.0, 100.0], [200.0, 100.0]], 6)],
        vec![text(40.0, 70.0, "hi")],
        vec![],
    );
    let snap = board
        .snap_text(
            &[ItemId::Text(0)],
            Transform::Translate { dx: 0.0, dy: 0.0 },
            12.0,
        )
        .expect("the text meets the stroke");
    assert_eq!(snap.target, TextTarget::Stroke(6));
    board.texts[0] = snap.transform.text(&board.texts[0]);
    let bounds = board.texts[0].bounds();
    assert!(
        (100.0 - (bounds.y + bounds.h) - ink(3.0)).abs() < 0.01,
        "the box did not rest beside the stroke"
    );
}

#[test]
fn an_attachment_survives_save_and_load() {
    let mut board = board(
        vec![stroke(vec![[0.0, 100.0], [200.0, 100.0]], 6)],
        vec![text(40.0, 70.0, "hi")],
        vec![],
    );
    board.texts[0].attach = Some(TextAttachment {
        target: TextTarget::Stroke(6),
        offset: [-20.0, -10.0],
    });
    board.sync_attached_texts();
    let mut doc = sgraffito::canvas::Doc::default();
    doc.boards.insert("eDP-1".into(), board.clone());

    let dir = tmpdir("text-attach");
    let path = dir.join("annotations.json");
    store::save(&path, &doc).unwrap();
    let loaded = store::load(&path);
    assert_eq!(store::VERSION, 7);
    let loaded = &loaded.boards["eDP-1"];
    assert_eq!(loaded.texts[0].attach, board.texts[0].attach);
    assert_eq!(loaded.strokes[0].id, 6);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn old_files_without_ids_or_attachments_still_load() {
    let dir = tmpdir("text-legacy");
    let path = dir.join("annotations.json");
    let json = r##"{
        "version": 6,
        "boards": {
            "eDP-1": {
                "strokes": [{"color": "#e01b24", "width": 3.0, "points": [[0, 0], [10, 10]]}],
                "texts": [{"x": 1.0, "y": 2.0, "color": "#ffffff", "size": 18.0, "text": "old"}]
            }
        },
        "active": "eDP-1"
    }"##;
    std::fs::write(&path, json).unwrap();
    let doc = store::load(&path);
    let board = &doc.boards["eDP-1"];
    assert_eq!(board.strokes[0].id, 1);
    assert_eq!(board.texts[0].attach, None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn moving_a_target_moves_its_attached_text() {
    let mut board = board(
        vec![],
        vec![text(10.0, 10.0, "hi")],
        vec![shape(ShapeKind::Rect, [0.0, 0.0], [100.0, 50.0], 2)],
    );
    board.texts[0].attach = Some(TextAttachment {
        target: TextTarget::Shape(2),
        offset: [10.0, -20.0],
    });
    board.sync_attached_texts();
    let before = board.texts[0].x;

    let moved = Transform::Translate { dx: 25.0, dy: 10.0 };
    board.shapes[0] = moved.shape(&board.shapes[0]);
    board.sync_attached_texts();
    assert_eq!(board.texts[0].x, before + 25.0);
    assert_eq!(board.texts[0].y, 15.0);
}

#[test]
fn copying_a_target_remaps_the_copied_text_attachment() {
    let mut board = board(
        vec![stroke(vec![[0.0, 0.0], [50.0, 0.0]], 3)],
        vec![text(5.0, 5.0, "hi")],
        vec![shape(ShapeKind::Rect, [0.0, 50.0], [50.0, 100.0], 7)],
    );
    board.texts[0].attach = Some(TextAttachment {
        target: TextTarget::Stroke(3),
        offset: [5.0, 5.0],
    });
    let copies = board.duplicate(&[ItemId::Text(0), ItemId::Stroke(0), ItemId::Shape(0)]);
    assert_eq!(copies.0.len(), 3);
    let copied_text = board.texts[1].clone();
    assert_eq!(copied_text.attach.unwrap().target, TextTarget::Stroke(4));
    assert_eq!(board.texts[0].attach.unwrap().target, TextTarget::Stroke(3));
    assert_eq!(board.strokes[1].id, 4);
    assert_eq!(board.shapes[1].id, 8);
}

#[test]
fn appending_remaps_a_text_attachment_onto_the_copied_target() {
    let mut source = board(
        vec![stroke(vec![[0.0, 0.0], [50.0, 0.0]], 3)],
        vec![text(5.0, 5.0, "hi")],
        vec![],
    );
    source.texts[0].attach = Some(TextAttachment {
        target: TextTarget::Stroke(3),
        offset: [5.0, 5.0],
    });
    let mut target = Board::default();
    target.append(&source);
    assert_eq!(target.strokes[0].id, 1);
    assert_eq!(
        target.texts[0].attach.unwrap().target,
        TextTarget::Stroke(1)
    );
}

#[test]
fn deleting_a_target_clears_the_attachment() {
    let mut board = board(
        vec![stroke(vec![[0.0, 0.0], [50.0, 0.0]], 3)],
        vec![text(5.0, 5.0, "hi"), text(5.0, 5.0, "ho")],
        vec![shape(ShapeKind::Rect, [0.0, 50.0], [50.0, 100.0], 7)],
    );
    board.texts[0].attach = Some(TextAttachment {
        target: TextTarget::Stroke(3),
        offset: [5.0, 5.0],
    });
    board.texts[1].attach = Some(TextAttachment {
        target: TextTarget::Shape(7),
        offset: [5.0, 5.0],
    });
    board.remove(&[ItemId::Stroke(0)]);
    assert_eq!(board.texts[0].attach, None);
    board.remove(&[ItemId::Shape(0)]);
    assert_eq!(board.texts[1].attach, None);
}

#[test]
fn strokes_get_unique_board_local_ids() {
    let mut board = Board::default();
    board.add_stroke(stroke(vec![[0.0, 0.0]], 0));
    board.add_stroke(stroke(vec![[1.0, 1.0]], 1));
    assert_eq!(board.strokes[0].id, 1);
    assert_eq!(board.strokes[1].id, 2);
    board.strokes[0].id = 0;
    board.ensure_stroke_ids();
    assert_eq!(board.strokes[0].id, 3);
}

fn tmpdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "sgraffito-text-attach-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn text_snaps_inside_a_rotated_border() {
    let mut turned = shape(ShapeKind::Rect, [0.0, 0.0], [200.0, 100.0], 7);
    turned.rotate = std::f32::consts::FRAC_PI_2;
    let mut board = board(vec![], vec![text(158.0, 40.0, "hi")], vec![turned]);
    let snap = board
        .snap_text(
            &[ItemId::Text(0)],
            Transform::Translate { dx: 0.0, dy: 0.0 },
            12.0,
        )
        .expect("the turned border did not attract the text");
    assert_eq!(snap.target, TextTarget::Shape(7));
    board.texts[0] = snap.transform.text(&board.texts[0]);
    let bounds = board.texts[0].bounds();
    // The rectangle's local 200x100 box, turned a quarter turn about (100, 50), occupies
    // x 50..150 and y -50..150. The snapped box must be wholly inside that.
    assert!(
        bounds.x >= 50.0
            && bounds.x + bounds.w <= 150.0
            && bounds.y >= -50.0
            && bounds.y + bounds.h <= 150.0,
        "the snapped text crossed the turned border: {bounds:?}"
    );
}
