use sgraffito::canvas::{ItemId, OutputAnnotations, Rect, Stroke, TextItem};

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
        ann(vec![line(100.0, 0.0, 200.0)], vec![])
            .clone()
            .erase(400.0, 400.0)
            .is_none()
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
    // The erased element's box comes back, so the frame that reports it knows what to damage.
    assert!(a.erase(50.0, 101.0).is_some());
    assert_eq!(a.strokes.len(), 1);
    assert_eq!(a.strokes[0].points[0][1], 140.0);
    assert!(a.erase(50.0, 101.0).is_none());
}

#[test]
fn erase_removes_text_box() {
    let mut a = ann(vec![], vec![text(100.0, 200.0, "note")]);
    assert!(a.erase(110.0, 210.0).is_some());
    assert!(a.texts.is_empty());
}

#[test]
fn multi_line_text_box_grows() {
    let a = ann(vec![], vec![text(0.0, 0.0, "ab\ncdef")]);
    // Two lines: height is 2 * 1.2 * size, width comes from the longest line
    assert_eq!(a.hit(10.0, 1.2 * 18.0 + 1.0), Some(ItemId::Text(0)));
    assert_eq!(a.hit(10.0, 2.4 * 18.0 + 1.0), None);
}

#[test]
fn rect_union_and_intersection() {
    let a = Rect {
        x: 0.0,
        y: 0.0,
        w: 10.0,
        h: 10.0,
    };
    let b = Rect {
        x: 20.0,
        y: 5.0,
        w: 10.0,
        h: 10.0,
    };
    assert_eq!(
        a.union(b),
        Rect {
            x: 0.0,
            y: 0.0,
            w: 30.0,
            h: 15.0
        }
    );
    assert!(!a.intersects(b));
    assert!(a.intersects(Rect {
        x: 5.0,
        y: 5.0,
        w: 1.0,
        h: 1.0
    }));
    // Touching edges are not an overlap, so culling never keeps a box that only abuts.
    assert!(!a.intersects(Rect {
        x: 10.0,
        y: 0.0,
        w: 5.0,
        h: 5.0
    }));
}

#[test]
fn stroke_bounds_cover_the_width_and_handle_the_empty_and_single_case() {
    let s = Stroke {
        color: "#ffffff".into(),
        width: 4.0,
        points: vec![[10.0, 20.0], [30.0, 20.0]],
    };
    // Half the stroke width plus one logical pixel for the anti-aliasing edge.
    assert_eq!(
        s.bounds(),
        Some(Rect {
            x: 7.0,
            y: 17.0,
            w: 26.0,
            h: 6.0
        })
    );
    let empty = Stroke {
        points: vec![],
        ..s.clone()
    };
    assert_eq!(empty.bounds(), None);
    // A single sample renders as a dot, so its box is a square and not empty.
    let dot = Stroke {
        points: vec![[5.0, 5.0]],
        ..s
    };
    assert_eq!(
        dot.bounds(),
        Some(Rect {
            x: 2.0,
            y: 2.0,
            w: 6.0,
            h: 6.0
        })
    );
}

// --- selection: the frame, the marquee and the three gestures -------------------------------

use sgraffito::canvas::{
    Gesture, Grab, Handle, SELECT_HANDLE_HIT, Selection, Transform, grab_at, item_bounds,
    marquee_select, rotate_handle, selection_bounds, selection_frame,
};

fn stroke_at(x: f32, y: f32) -> Stroke {
    Stroke {
        color: "#e01b24".into(),
        width: 2.0,
        points: vec![[x, y], [x + 10.0, y]],
    }
}

#[test]
fn the_frame_is_the_union_of_what_is_held() {
    let a = ann(
        vec![stroke_at(0.0, 0.0), stroke_at(100.0, 100.0)],
        vec![text(50.0, 50.0, "note")],
    );
    let one = selection_frame(&a, &[ItemId::Stroke(0)], None).unwrap();
    assert_eq!(one, item_bounds(&a, ItemId::Stroke(0)).unwrap());
    let both = selection_frame(&a, &[ItemId::Stroke(0), ItemId::Stroke(1)], None).unwrap();
    assert_eq!(both.union(one), both, "the union covers each of them");
    let with_text = selection_frame(&a, &[ItemId::Stroke(0), ItemId::Text(0)], None).unwrap();
    assert!(with_text.x + with_text.w >= a.texts[0].paint_bounds().x + a.texts[0].paint_bounds().w);
    assert!(selection_frame(&a, &[], None).is_none());
}

#[test]
fn the_frame_follows_a_live_transform() {
    let a = ann(vec![stroke_at(0.0, 0.0)], vec![]);
    let held = [ItemId::Stroke(0)];
    let before = selection_frame(&a, &held, None).unwrap();
    let moved =
        selection_frame(&a, &held, Some(Transform::Translate { dx: 40.0, dy: 25.0 })).unwrap();
    assert_eq!(moved.x, before.x + 40.0);
    assert_eq!(moved.y, before.y + 25.0);
}

#[test]
fn the_marquee_takes_only_what_it_wholly_contains() {
    let a = ann(
        vec![
            stroke_at(20.0, 20.0), // wholly inside
            Stroke {
                points: vec![[90.0, 20.0], [200.0, 20.0]],
                ..stroke_at(90.0, 20.0)
            }, // crosses the right edge
        ],
        vec![text(25.0, 60.0, "in"), text(300.0, 60.0, "out")],
    );
    let marquee = Rect {
        x: 0.0,
        y: 0.0,
        w: 120.0,
        h: 100.0,
    };
    assert_eq!(
        marquee_select(&a, marquee),
        vec![ItemId::Text(0), ItemId::Stroke(0)]
    );
}

#[test]
fn a_marquee_dragged_up_and_to_the_left_is_the_same_rectangle() {
    let a = ann(vec![stroke_at(20.0, 20.0)], vec![]);
    let down_right = Rect::from_corners([10.0, 10.0], [100.0, 100.0]);
    let up_left = Rect::from_corners([100.0, 100.0], [10.0, 10.0]);
    assert_eq!(down_right, up_left);
    assert!(down_right.w > 0.0 && down_right.h > 0.0);
    assert_eq!(marquee_select(&a, down_right), marquee_select(&a, up_left));
}

#[test]
fn a_handle_is_grabbed_within_its_target_and_a_handle_wins() {
    let frame = Rect {
        x: 100.0,
        y: 100.0,
        w: 200.0,
        h: 100.0,
    };
    // The element under a corner is not what the press resolves to: the handle is.
    assert_eq!(grab_at(frame, 100.0, 100.0), Some(Grab::Handle(Handle::Nw)));
    assert_eq!(
        grab_at(frame, 100.0 + SELECT_HANDLE_HIT, 100.0),
        Some(Grab::Handle(Handle::Nw))
    );
    assert_eq!(grab_at(frame, 100.0 + SELECT_HANDLE_HIT + 1.0, 100.0), None);
    assert_eq!(grab_at(frame, 200.0, 100.0), Some(Grab::Handle(Handle::N)));
    let knob = rotate_handle(frame);
    assert_eq!(grab_at(frame, knob[0], knob[1]), Some(Grab::Rotate));
    assert_eq!(grab_at(frame, 200.0, 150.0), None, "the middle is a move");
}

#[test]
fn the_selection_box_covers_the_frame_the_handles_and_the_rotation_handle() {
    let frame = Rect {
        x: 100.0,
        y: 100.0,
        w: 200.0,
        h: 100.0,
    };
    let box_ = selection_bounds(frame);
    let knob = rotate_handle(frame);
    assert!(
        box_.contains(knob[0], knob[1]),
        "the rotation handle is in it"
    );
    for handle in Handle::ALL {
        let [x, y] = handle.point(frame);
        assert!(box_.contains(x, y), "{handle:?} is in it");
    }
    assert!(
        box_.contains(frame.x + SELECT_HANDLE_HIT, frame.y + SELECT_HANDLE_HIT),
        "the frame's edge is in it"
    );
}

#[test]
fn a_move_offsets_every_point_and_the_origin_of_a_text_box() {
    let s = stroke_at(10.0, 10.0);
    let t = text(10.0, 10.0, "note");
    let m = Transform::Translate { dx: 12.0, dy: -6.0 };
    assert_eq!(m.stroke(&s).points, vec![[22.0, 4.0], [32.0, 4.0]]);
    assert_eq!((m.text(&t).x, m.text(&t).y), (22.0, 4.0));
    assert_eq!(m.text(&t).size, t.size, "a move does not resize the text");
    assert!(Transform::Translate { dx: 0.0, dy: 0.0 }.is_identity());
    assert!(!m.is_identity());
}

#[test]
fn a_corner_resize_keeps_the_opposite_corner_still() {
    let frame = Rect {
        x: 100.0,
        y: 100.0,
        w: 200.0,
        h: 100.0,
    };
    // Press on the south-east corner and drag it to twice the distance from the north-west one.
    let gesture = Gesture::start(Some(Grab::Handle(Handle::Se)), frame, 300.0, 200.0);
    let t = gesture.transform([500.0, 300.0]).unwrap();
    let s = Stroke {
        color: "#ffffff".into(),
        width: 2.0,
        points: vec![[100.0, 100.0], [300.0, 200.0]],
    };
    let scaled = t.stroke(&s);
    assert_eq!(scaled.points[0], [100.0, 100.0], "the anchor did not move");
    assert_eq!(
        scaled.points[1],
        [500.0, 300.0],
        "the handle tracked the pointer"
    );
    assert_eq!(scaled.width, s.width, "the pen does not get fat");
}

#[test]
fn an_edge_resize_scales_one_axis_and_grows_the_text() {
    let frame = Rect {
        x: 100.0,
        y: 100.0,
        w: 200.0,
        h: 100.0,
    };
    let t = Gesture::start(Some(Grab::Handle(Handle::E)), frame, 300.0, 150.0)
        .transform([500.0, 150.0])
        .unwrap();
    let s = stroke_at(100.0, 120.0);
    let scaled = t.stroke(&s);
    assert_eq!(scaled.points[0], [100.0, 120.0], "the left edge stayed put");
    assert_eq!(scaled.points[1], [120.0, 120.0], "the width doubled");
    assert_eq!(
        t.text(&text(100.0, 100.0, "note")).size,
        18.0,
        "one axis only"
    );

    // A vertical resize is what grows a text box, and it stays within the size bounds.
    let up = Gesture::start(Some(Grab::Handle(Handle::S)), frame, 200.0, 200.0)
        .transform([200.0, 300.0])
        .unwrap();
    assert_eq!(up.text(&text(100.0, 100.0, "note")).size, 36.0);
    let huge = Gesture::start(Some(Grab::Handle(Handle::S)), frame, 200.0, 200.0)
        .transform([200.0, 20_000.0])
        .unwrap();
    assert_eq!(huge.text(&text(100.0, 100.0, "note")).size, 72.0);
}

#[test]
fn a_frame_cannot_be_collapsed_or_mirrored() {
    let frame = Rect {
        x: 100.0,
        y: 100.0,
        w: 200.0,
        h: 100.0,
    };
    // Dragged far past the anchor: the factor stays positive and above the floor, so the elements
    // are still recoverable and nothing is mirrored.
    let t = Gesture::start(Some(Grab::Handle(Handle::Se)), frame, 300.0, 200.0)
        .transform([-9_000.0, -9_000.0])
        .unwrap();
    let scaled = t.stroke(&stroke_at(110.0, 110.0));
    assert!(scaled.points[0][0] < 110.0 && scaled.points[0][1] < 110.0);
    assert!(
        scaled.points[1][0] > scaled.points[0][0],
        "the extent is still positive"
    );
}

#[test]
fn a_rotation_turns_the_points_about_the_frames_centre_and_moves_the_text_upright() {
    let frame = Rect {
        x: 0.0,
        y: 0.0,
        w: 100.0,
        h: 100.0,
    };
    // A quarter turn: the press is above the centre, the pointer to its right.
    let t = Gesture::start(Some(Grab::Rotate), frame, 50.0, 0.0)
        .transform([100.0, 50.0])
        .unwrap();
    let turned = t.stroke(&Stroke {
        color: "#ffffff".into(),
        width: 2.0,
        points: vec![[50.0, 0.0], [50.0, 50.0]],
    });
    assert!((turned.points[0][0] - 100.0).abs() < 0.001);
    assert!((turned.points[0][1] - 50.0).abs() < 0.001);
    assert!((turned.points[1][0] - 50.0).abs() < 0.001);
    assert!((turned.points[1][1] - 50.0).abs() < 0.001);
    // A text box has no angle to turn, so it orbits the centre with its glyphs upright.
    let moved = t.text(&text(50.0, 0.0, "note"));
    assert!((moved.x - 100.0).abs() < 0.001 && (moved.y - 50.0).abs() < 0.001);
    assert_eq!(moved.size, 18.0);
    assert_eq!(turned.color, "#ffffff");
}

#[test]
fn a_gesture_that_ends_where_it_started_changes_nothing() {
    let frame = Rect {
        x: 0.0,
        y: 0.0,
        w: 100.0,
        h: 100.0,
    };
    for grab in [None, Some(Grab::Handle(Handle::Nw)), Some(Grab::Rotate)] {
        let gesture = Gesture::start(grab, frame, 10.0, 10.0);
        assert!(gesture.transform([10.0, 10.0]).unwrap().is_identity());
        assert!(!gesture.transform([11.0, 10.0]).unwrap().is_identity());
    }
    // A marquee transforms nothing at all.
    assert!(
        Gesture::start(None, frame, 10.0, 10.0)
            .transform([90.0, 90.0])
            .is_some()
    );
}

#[test]
fn a_marquee_gesture_transforms_nothing() {
    let selection = Selection {
        items: vec![ItemId::Stroke(0)],
        transform: None,
    };
    assert!(
        selection.live().is_empty(),
        "a resting selection is not live"
    );
    let dragged = Selection {
        transform: Some(Transform::Translate { dx: 1.0, dy: 0.0 }),
        ..selection.clone()
    };
    assert_eq!(dragged.live(), &[ItemId::Stroke(0)]);
    let marquee = Gesture::Marquee { origin: [0.0, 0.0] };
    assert!(marquee.transform([50.0, 50.0]).is_none());
}

#[test]
fn the_gap_between_the_elements_of_a_selection_is_inside_its_frame() {
    // What makes a cluster grabbable by its box: a press between two held strokes has to land
    // inside the frame, on empty space that belongs to neither of them.
    let a = ann(vec![stroke_at(0.0, 0.0), stroke_at(160.0, 100.0)], vec![]);
    let held = [ItemId::Stroke(0), ItemId::Stroke(1)];
    let frame = selection_frame(&a, &held, None).unwrap();
    let (gap, outside) = ([80.0, 50.0], [-20.0, 50.0]);
    assert!(
        !item_bounds(&a, ItemId::Stroke(0))
            .unwrap()
            .contains(gap[0], gap[1])
    );
    assert!(
        !item_bounds(&a, ItemId::Stroke(1))
            .unwrap()
            .contains(gap[0], gap[1])
    );
    assert!(
        frame.contains(gap[0], gap[1]),
        "the gap is inside the frame"
    );
    assert!(
        !frame.contains(outside[0], outside[1]),
        "the frame has an edge"
    );
    // With nothing held there is no frame, so a press there can only start a marquee.
    assert!(selection_frame(&a, &[], None).is_none());
}
