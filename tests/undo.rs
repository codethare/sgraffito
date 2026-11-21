//! Document undo: a snapshot is taken before a change, and undo restores it without recording
//! itself.

use std::collections::BTreeMap;

use sgraffito::app::{UNDO_LIMIT, UndoHistory};
use sgraffito::canvas::{Board, Doc, Stroke};

fn stroke(y: f32) -> Stroke {
    Stroke {
        color: "#e01b24".into(),
        width: 3.0,
        points: vec![[0.0, y], [100.0, y]],
        attach: [None, None],
    }
}

fn doc_with(name: &str, strokes: Vec<Stroke>) -> Doc {
    Doc {
        boards: BTreeMap::from([(
            name.to_string(),
            Board {
                strokes,
                ..Board::default()
            },
        )]),
        active: name.to_string(),
    }
}

#[test]
fn drawing_then_undo_puts_the_board_back() {
    let mut doc = doc_with("Board 1", Vec::new());
    let mut undo = UndoHistory::new();
    undo.checkpoint(&doc);
    doc.boards
        .get_mut("Board 1")
        .unwrap()
        .strokes
        .push(stroke(10.0));

    assert_eq!(undo.len(), 1);
    assert_eq!(undo.undo(&doc), Some(doc_with("Board 1", Vec::new())));
    // Undo takes a snapshot out but never puts one in, so a second undo has nothing to do.
    assert!(undo.is_empty());
    assert_eq!(undo.undo(&doc), None);
}

#[test]
fn transform_then_undo_restores_the_geometry() {
    let mut doc = doc_with("Board 1", vec![stroke(10.0)]);
    let before = doc.clone();
    let mut undo = UndoHistory::new();
    undo.checkpoint(&doc);

    let moved = doc.boards.get_mut("Board 1").unwrap().strokes[0].clone();
    doc.boards.get_mut("Board 1").unwrap().strokes[0].points = vec![[20.0, 60.0], [120.0, 60.0]];
    assert_ne!(doc, before);
    assert_ne!(doc.boards["Board 1"].strokes[0], moved);

    let restored = undo.undo(&doc).unwrap();
    assert_eq!(restored, before);
    doc = restored;
    // The restored stroke is the one that was there before the transform, not the moved copy.
    assert_eq!(doc.boards["Board 1"].strokes[0], stroke(10.0));
}

#[test]
fn deleting_a_board_then_undo_brings_it_back() {
    let mut doc = Doc {
        boards: BTreeMap::from([
            ("Board 1".to_string(), Board::default()),
            (
                "Board 2".to_string(),
                Board {
                    strokes: vec![stroke(20.0)],
                    ..Board::default()
                },
            ),
        ]),
        active: "Board 2".to_string(),
    };
    let before = doc.clone();
    let mut undo = UndoHistory::new();
    undo.checkpoint(&doc);

    assert_eq!(doc.delete_board("Board 2").as_deref(), Some("Board 1"));
    let restored = undo.undo(&doc).unwrap();
    assert_eq!(restored, before);
    doc = restored;
    assert_eq!(doc, before);
}

#[test]
fn renaming_a_board_then_undo_brings_the_old_name_back() {
    let mut doc = doc_with("Board 1", vec![stroke(10.0)]);
    let before = doc.clone();
    let mut undo = UndoHistory::new();
    undo.checkpoint(&doc);

    assert!(doc.rename_board("Board 1", "office"));
    assert!(doc.boards.contains_key("office"));
    let restored = undo.undo(&doc).unwrap();
    assert_eq!(restored, before);
    doc = restored;
    assert_eq!(doc, before);
    assert!(doc.boards.contains_key("Board 1"));
    assert!(!doc.boards.contains_key("office"));
}

#[test]
fn nothing_changed_records_nothing_and_undo_is_a_no_op() {
    let doc = doc_with("Board 1", Vec::new());
    let mut undo = UndoHistory::new();

    // An empty history has nothing to restore and does not become dirty or repaint.
    assert!(undo.is_empty());
    assert_eq!(undo.undo(&doc), None);

    // A change that was a no-op is not a step: recording it compares the two documents.
    undo.record(doc.clone(), &doc);
    assert!(undo.is_empty());

    // A gesture or an edit that is taken back or ended without typing leaves nothing behind.
    undo.begin_gesture(&doc);
    undo.abandon_gesture();
    undo.begin_edit(&doc);
    undo.commit_edit(&doc);
    assert!(undo.is_empty());
}

#[test]
fn history_is_bounded_and_drops_the_oldest_snapshot() {
    let mut doc = doc_with("Board 1", Vec::new());
    let mut undo = UndoHistory::new();
    for i in 0..UNDO_LIMIT + 10 {
        let before = doc.clone();
        doc.boards
            .get_mut("Board 1")
            .unwrap()
            .strokes
            .push(stroke(i as f32));
        undo.record(before, &doc);
    }
    assert_eq!(undo.len(), UNDO_LIMIT);

    // The newest snapshot is the one just before the last stroke; every remaining one is older.
    assert_eq!(
        undo.undo(&doc).unwrap().boards["Board 1"].strokes.len(),
        UNDO_LIMIT + 9
    );
    for _ in 1..UNDO_LIMIT {
        assert!(undo.undo(&doc).is_some());
    }
    assert!(undo.undo(&doc).is_none());
}

#[test]
fn one_gesture_is_one_undo_step() {
    let mut doc = doc_with("Board 1", Vec::new());
    let empty = doc.clone();
    let mut undo = UndoHistory::new();

    // An `Alt`-drag makes a copy and then moves it, but both are the gesture that started at the
    // press: its single snapshot must bring back the board as the press found it.
    undo.begin_gesture(&doc);
    doc.boards
        .get_mut("Board 1")
        .unwrap()
        .strokes
        .push(stroke(10.0));
    doc.boards.get_mut("Board 1").unwrap().strokes[0].points = vec![[40.0, 40.0], [140.0, 40.0]];
    undo.commit_gesture(&doc);
    // A second commit inside the same gesture must not add another step.
    undo.commit_gesture(&doc);

    assert_eq!(undo.len(), 1);
    assert_eq!(undo.undo(&doc), Some(empty));
    assert!(undo.is_empty());
}

#[test]
fn one_typing_session_is_one_undo_step() {
    let mut doc = doc_with("Board 1", Vec::new());
    let empty = doc.clone();
    let mut undo = UndoHistory::new();

    undo.begin_edit(&doc);
    // Grow the document through several keystrokes, as `grow_note` and the commit do.
    for _ in 0..5 {
        doc.boards
            .get_mut("Board 1")
            .unwrap()
            .strokes
            .push(stroke(1.0));
    }
    undo.commit_edit(&doc);
    // Ending an edit with nothing typed is not a step either.
    undo.commit_edit(&doc);

    assert_eq!(undo.len(), 1);
    assert_eq!(undo.undo(&doc), Some(empty));
    assert!(undo.is_empty());
}

#[test]
fn undo_then_redo_puts_the_change_back() {
    let mut doc = doc_with("Board 1", Vec::new());
    let empty = doc.clone();
    let mut undo = UndoHistory::new();
    undo.checkpoint(&doc);
    doc.boards
        .get_mut("Board 1")
        .unwrap()
        .strokes
        .push(stroke(10.0));
    let drawn = doc.clone();

    assert!(undo.can_undo());
    assert!(!undo.can_redo());
    let restored = undo.undo(&doc).unwrap();
    assert_eq!(restored, empty);
    assert!(!undo.can_undo());
    assert!(undo.can_redo());
    assert_eq!(undo.redo_len(), 1);

    let redone = undo.redo(&restored).unwrap();
    assert_eq!(redone, drawn);
    // The change is back on the undo stack, and there is nothing left to redo.
    assert!(undo.can_undo());
    assert!(!undo.can_redo());
    assert_eq!(undo.len(), 1);
}

#[test]
fn a_new_change_clears_the_redo_stack() {
    let mut doc = doc_with("Board 1", Vec::new());
    let mut undo = UndoHistory::new();
    undo.checkpoint(&doc);
    doc.boards
        .get_mut("Board 1")
        .unwrap()
        .strokes
        .push(stroke(10.0));
    let restored = undo.undo(&doc).unwrap();
    assert!(undo.can_redo());

    // A new edit from the undone state is a new branch: the change that was undone is no longer
    // ahead of the document, so redo must not bring it back.
    let before = restored.clone();
    doc = restored;
    doc.boards
        .get_mut("Board 1")
        .unwrap()
        .strokes
        .push(stroke(20.0));
    undo.record(before, &doc);

    assert!(!undo.can_redo());
    assert_eq!(undo.redo(&doc), None);
    assert_eq!(undo.len(), 1);

    // A checkpoint is a new change too, even when nothing has been recorded from it yet.
    let restored = undo.undo(&doc).unwrap();
    assert!(undo.can_redo());
    undo.checkpoint(&restored);
    assert!(!undo.can_redo());
}

#[test]
fn redo_with_nothing_to_redo_is_a_no_op() {
    let doc = doc_with("Board 1", Vec::new());
    let mut undo = UndoHistory::new();

    assert!(!undo.can_redo());
    assert_eq!(undo.redo(&doc), None);
    assert!(undo.is_empty());
    assert!(!undo.can_undo());
}

#[test]
fn bounded_undo_does_not_make_redo_unbounded() {
    let mut doc = doc_with("Board 1", Vec::new());
    let mut undo = UndoHistory::new();
    for i in 0..UNDO_LIMIT + 10 {
        let before = doc.clone();
        doc.boards
            .get_mut("Board 1")
            .unwrap()
            .strokes
            .push(stroke(i as f32));
        undo.record(before, &doc);
    }

    // Every undo that is possible moves one document onto the redo stack, so redo cannot outgrow
    // the bounded undo stack.
    let mut current = doc;
    let mut undone = 0;
    while let Some(previous) = undo.undo(&current) {
        current = previous;
        undone += 1;
    }
    assert_eq!(undone, UNDO_LIMIT);
    assert_eq!(undo.redo_len(), UNDO_LIMIT);
    assert!(!undo.can_undo());

    // Redoing them one at a time returns the same bounded number of steps to undo.
    while let Some(next) = undo.redo(&current) {
        current = next;
    }
    assert_eq!(undo.redo_len(), 0);
    assert_eq!(undo.len(), UNDO_LIMIT);
}

#[test]
fn redo_does_not_itself_create_history() {
    let mut doc = doc_with("Board 1", Vec::new());
    let empty = doc.clone();
    let mut undo = UndoHistory::new();
    undo.checkpoint(&doc);
    doc.boards
        .get_mut("Board 1")
        .unwrap()
        .strokes
        .push(stroke(10.0));
    let drawn = doc.clone();

    let restored = undo.undo(&doc).unwrap();
    assert_eq!(undo.len(), 0);
    assert_eq!(undo.redo_len(), 1);

    let redone = undo.redo(&restored).unwrap();
    assert_eq!(redone, drawn);
    // Redo returns the document it replaced to undo and adds no redo entry of its own.
    assert_eq!(undo.len(), 1);
    assert_eq!(undo.redo_len(), 0);
    assert_eq!(undo.undo(&redone), Some(empty));
}

#[test]
fn an_abandoned_gesture_leaves_redo_alone() {
    let mut doc = doc_with("Board 1", Vec::new());
    let mut undo = UndoHistory::new();
    undo.checkpoint(&doc);
    doc.boards
        .get_mut("Board 1")
        .unwrap()
        .strokes
        .push(stroke(10.0));
    let restored = undo.undo(&doc).unwrap();
    assert!(undo.can_redo());

    // A gesture that never lands changed nothing, so it is not a new branch.
    undo.begin_gesture(&restored);
    undo.abandon_gesture();
    assert!(undo.can_redo());

    // A gesture that does land is a new change, and it drops the branch.
    undo.begin_gesture(&restored);
    let mut moved = restored.clone();
    moved
        .boards
        .get_mut("Board 1")
        .unwrap()
        .strokes
        .push(stroke(30.0));
    undo.commit_gesture(&moved);
    assert!(!undo.can_redo());
}
