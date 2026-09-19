use sgraffito::canvas::{ItemId, OutputAnnotations, Stroke, TextItem};

fn line(y: f32, x0: f32, x1: f32) -> Stroke {
    Stroke {
        color: "#e01b24".into(),
        width: 3.0,
        points: vec![[x0, y], [x1, y]],
    }
}

fn ann(strokes: Vec<Stroke>, texts: Vec<TextItem>) -> OutputAnnotations {
    OutputAnnotations { strokes, texts }
}

fn text(x: f32, y: f32, s: &str) -> TextItem {
    TextItem {
        x,
        y,
        color: "#ffffff".into(),
        size: 18.0,
        text: s.into(),
    }
}

#[test]
fn hits_stroke_within_threshold() {
    let a = ann(vec![line(100.0, 0.0, 200.0)], vec![]);
    assert_eq!(a.hit(50.0, 101.0), Some(ItemId::Stroke(0)));
}

#[test]
fn threshold_boundary_is_inclusive() {
    let a = ann(vec![line(100.0, 0.0, 200.0)], vec![]);
    assert_eq!(a.hit(50.0, 108.0), Some(ItemId::Stroke(0)));
    assert_eq!(a.hit(50.0, 108.5), None);
}

#[test]
fn misses_far_away_point() {
    let a = ann(vec![line(100.0, 0.0, 200.0)], vec![]);
    assert_eq!(a.hit(50.0, 120.0), None);
    assert!(
        !ann(vec![line(100.0, 0.0, 200.0)], vec![])
            .clone()
            .erase(400.0, 400.0)
    );
}

#[test]
fn misses_beyond_stroke_ends() {
    let a = ann(vec![line(100.0, 0.0, 200.0)], vec![]);
    assert_eq!(a.hit(220.0, 100.0), None);
}

#[test]
fn picks_nearest_stroke() {
    let a = ann(
        vec![line(100.0, 0.0, 200.0), line(104.0, 0.0, 200.0)],
        vec![],
    );
    assert_eq!(a.hit(50.0, 101.0), Some(ItemId::Stroke(0)));
    assert_eq!(a.hit(50.0, 103.0), Some(ItemId::Stroke(1)));
}

#[test]
fn hits_text_box() {
    let a = ann(vec![], vec![text(100.0, 200.0, "buy milk")]);
    assert_eq!(a.hit(110.0, 210.0), Some(ItemId::Text(0)));
    assert_eq!(a.hit(100.0 + 0.6 * 18.0 * 8.0 + 5.0, 210.0), None);
}

#[test]
fn text_wins_over_stroke_at_same_point() {
    let t = text(100.0, 200.0, "note");
    let s = line(205.0, 90.0, 200.0);
    let a = ann(vec![s], vec![t]);
    assert_eq!(a.hit(110.0, 205.0), Some(ItemId::Text(0)));
}

#[test]
fn erase_removes_only_hit_item() {
    let mut a = ann(
        vec![line(100.0, 0.0, 200.0), line(140.0, 0.0, 200.0)],
        vec![],
    );
    assert!(a.erase(50.0, 101.0));
    assert_eq!(a.strokes.len(), 1);
    assert_eq!(a.strokes[0].points[0][1], 140.0);
    assert!(!a.erase(50.0, 101.0));
}

#[test]
fn erase_removes_text_box() {
    let mut a = ann(vec![], vec![text(100.0, 200.0, "note")]);
    assert!(a.erase(110.0, 210.0));
    assert!(a.texts.is_empty());
}

#[test]
fn multi_line_text_box_grows() {
    let a = ann(vec![], vec![text(0.0, 0.0, "ab\ncdef")]);
    // Two lines: height is 2 * 1.2 * size, width comes from the longest line
    assert_eq!(a.hit(10.0, 1.2 * 18.0 + 1.0), Some(ItemId::Text(0)));
    assert_eq!(a.hit(10.0, 2.4 * 18.0 + 1.0), None);
}
