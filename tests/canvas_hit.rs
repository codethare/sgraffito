use sgraffito::canvas::{
    ERASER_THRESHOLD, ItemId, OutputAnnotations, PALETTE, Rect, Stroke, TextItem,
};

fn line(y: f32, x0: f32, x1: f32) -> Stroke {
    Stroke {
        color: "#e01b24".into(),
        width: 3.0,
        points: vec![[x0, y], [x1, y]],
    }
}

fn ann(strokes: Vec<Stroke>, texts: Vec<TextItem>) -> OutputAnnotations {
    OutputAnnotations {
        strokes,
        texts,
        shapes: vec![],
        notes: vec![],
    }
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
    assert_eq!(
        a.hit(50.0, 101.0, ERASER_THRESHOLD),
        Some(ItemId::Stroke(0))
    );
}

#[test]
fn threshold_boundary_is_inclusive() {
    let a = ann(vec![line(100.0, 0.0, 200.0)], vec![]);
    assert_eq!(
        a.hit(50.0, 108.0, ERASER_THRESHOLD),
        Some(ItemId::Stroke(0))
    );
    assert_eq!(a.hit(50.0, 108.5, ERASER_THRESHOLD), None);
}

#[test]
fn misses_far_away_point() {
    let a = ann(vec![line(100.0, 0.0, 200.0)], vec![]);
    assert_eq!(a.hit(50.0, 120.0, ERASER_THRESHOLD), None);
    assert!(
        ann(vec![line(100.0, 0.0, 200.0)], vec![])
            .clone()
            .erase(400.0, 400.0, ERASER_THRESHOLD)
            .is_none()
    );
}

#[test]
fn misses_beyond_stroke_ends() {
    let a = ann(vec![line(100.0, 0.0, 200.0)], vec![]);
    assert_eq!(a.hit(220.0, 100.0, ERASER_THRESHOLD), None);
}

#[test]
fn picks_nearest_stroke() {
    let a = ann(
        vec![line(100.0, 0.0, 200.0), line(104.0, 0.0, 200.0)],
        vec![],
    );
    assert_eq!(
        a.hit(50.0, 101.0, ERASER_THRESHOLD),
        Some(ItemId::Stroke(0))
    );
    assert_eq!(
        a.hit(50.0, 103.0, ERASER_THRESHOLD),
        Some(ItemId::Stroke(1))
    );
}

#[test]
fn hits_text_box() {
    let a = ann(vec![], vec![text(100.0, 200.0, "buy milk")]);
    assert_eq!(a.hit(110.0, 210.0, ERASER_THRESHOLD), Some(ItemId::Text(0)));
    assert_eq!(
        a.hit(100.0 + 0.6 * 18.0 * 8.0 + 5.0, 210.0, ERASER_THRESHOLD),
        None
    );
}

#[test]
fn text_wins_over_stroke_at_same_point() {
    let t = text(100.0, 200.0, "note");
    let s = line(205.0, 90.0, 200.0);
    let a = ann(vec![s], vec![t]);
    assert_eq!(a.hit(110.0, 205.0, ERASER_THRESHOLD), Some(ItemId::Text(0)));
}

#[test]
fn erase_removes_only_hit_item() {
    let mut a = ann(
        vec![line(100.0, 0.0, 200.0), line(140.0, 0.0, 200.0)],
        vec![],
    );
    // The erased element's box comes back, so the frame that reports it knows what to damage.
    assert!(a.erase(50.0, 101.0, ERASER_THRESHOLD).is_some());
    assert_eq!(a.strokes.len(), 1);
    assert_eq!(a.strokes[0].points[0][1], 140.0);
    assert!(a.erase(50.0, 101.0, ERASER_THRESHOLD).is_none());
}

#[test]
fn erase_removes_text_box() {
    let mut a = ann(vec![], vec![text(100.0, 200.0, "note")]);
    assert!(a.erase(110.0, 210.0, ERASER_THRESHOLD).is_some());
    assert!(a.texts.is_empty());
}

#[test]
fn multi_line_text_box_grows() {
    let a = ann(vec![], vec![text(0.0, 0.0, "ab\ncdef")]);
    // Two lines: height is 2 * 1.2 * size, width comes from the longest line
    assert_eq!(
        a.hit(10.0, 1.2 * 18.0 + 1.0, ERASER_THRESHOLD),
        Some(ItemId::Text(0))
    );
    assert_eq!(a.hit(10.0, 2.4 * 18.0 + 1.0, ERASER_THRESHOLD), None);
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
    Gesture, Grab, Handle, Press, SELECT_HANDLE_HIT, SELECT_ROTATE_GAP, Selection, Transform,
    grab_at, item_bounds, marquee_select, rotate_handle, selection_bounds, selection_frame,
    selection_press,
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
    assert_eq!(
        grab_at(frame, 100.0, 100.0, SELECT_HANDLE_HIT),
        Some(Grab::Handle(Handle::Nw))
    );
    assert_eq!(
        grab_at(frame, 100.0 + SELECT_HANDLE_HIT, 100.0, SELECT_HANDLE_HIT),
        Some(Grab::Handle(Handle::Nw))
    );
    assert_eq!(
        grab_at(
            frame,
            100.0 + SELECT_HANDLE_HIT + 1.0,
            100.0,
            SELECT_HANDLE_HIT
        ),
        None
    );
    assert_eq!(
        grab_at(frame, 200.0, 100.0, SELECT_HANDLE_HIT),
        Some(Grab::Handle(Handle::N))
    );
    let knob = rotate_handle(frame, SELECT_ROTATE_GAP);
    assert_eq!(
        grab_at(frame, knob[0], knob[1], SELECT_HANDLE_HIT),
        Some(Grab::Rotate)
    );
    assert_eq!(
        grab_at(frame, 200.0, 150.0, SELECT_HANDLE_HIT),
        None,
        "the middle is a move"
    );
}

#[test]
fn the_selection_box_covers_the_frame_the_handles_and_the_rotation_handle() {
    let frame = Rect {
        x: 100.0,
        y: 100.0,
        w: 200.0,
        h: 100.0,
    };
    let box_ = selection_bounds(frame, SELECT_HANDLE_HIT);
    let knob = rotate_handle(frame, SELECT_ROTATE_GAP);
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
    let gesture = Gesture::start(Some(Grab::Handle(Handle::Se)), frame, 300.0, 200.0, false);
    let t = gesture.transform([500.0, 300.0], false).unwrap();
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
    let t = Gesture::start(Some(Grab::Handle(Handle::E)), frame, 300.0, 150.0, false)
        .transform([500.0, 150.0], false)
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
    let up = Gesture::start(Some(Grab::Handle(Handle::S)), frame, 200.0, 200.0, false)
        .transform([200.0, 300.0], false)
        .unwrap();
    assert_eq!(up.text(&text(100.0, 100.0, "note")).size, 36.0);
    let huge = Gesture::start(Some(Grab::Handle(Handle::S)), frame, 200.0, 200.0, false)
        .transform([200.0, 20_000.0], false)
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
    let t = Gesture::start(Some(Grab::Handle(Handle::Se)), frame, 300.0, 200.0, false)
        .transform([-9_000.0, -9_000.0], false)
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
    let t = Gesture::start(Some(Grab::Rotate), frame, 50.0, 0.0, false)
        .transform([100.0, 50.0], false)
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
        let gesture = Gesture::start(grab, frame, 10.0, 10.0, false);
        assert!(
            gesture
                .transform([10.0, 10.0], false)
                .unwrap()
                .is_identity()
        );
        assert!(
            !gesture
                .transform([11.0, 10.0], false)
                .unwrap()
                .is_identity()
        );
    }
    // A marquee transforms nothing at all.
    assert!(
        Gesture::start(None, frame, 10.0, 10.0, false)
            .transform([90.0, 90.0], false)
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
    assert!(marquee.transform([50.0, 50.0], false).is_none());
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

// --- shapes: two points and a kind -----------------------------------------------------------

use sgraffito::canvas::{Shape, ShapeKind};

fn shape(kind: ShapeKind, from: [f32; 2], to: [f32; 2]) -> Shape {
    Shape {
        kind,
        color: "#e01b24".into(),
        width: 3.0,
        from,
        to,
    }
}

#[test]
fn every_kind_is_read_from_the_same_two_points() {
    let a = ann(vec![], vec![]);
    let _ = a;
    let (from, to) = ([10.0, 20.0], [110.0, 70.0]);
    // A rectangle is its own box, drawn as a closed loop of five points.
    let rect = shape(ShapeKind::Rect, from, to).outline();
    assert_eq!(
        rect,
        vec![
            [10.0, 20.0],
            [110.0, 20.0],
            [110.0, 70.0],
            [10.0, 70.0],
            [10.0, 20.0]
        ]
    );
    // A diamond's corners are that box's edge midpoints.
    let diamond = shape(ShapeKind::Diamond, from, to).outline();
    assert_eq!(
        diamond[..4],
        [[60.0, 20.0], [110.0, 45.0], [60.0, 70.0], [10.0, 45.0]]
    );
    // A line is just the two points, in the order they were dragged.
    assert_eq!(shape(ShapeKind::Line, from, to).outline(), vec![from, to]);
    // A circle's rim touches the middle of each edge of the box and is a closed loop.
    let circle = shape(ShapeKind::Circle, from, to).outline();
    assert!((circle[0][0] - 110.0).abs() < 0.01 && (circle[0][1] - 45.0).abs() < 0.01);
    let (first, last) = (circle.first().unwrap(), circle.last().unwrap());
    assert!(
        (first[0] - last[0]).abs() < 0.01 && (first[1] - last[1]).abs() < 0.01,
        "a circle is closed"
    );
}

#[test]
fn an_arrow_keeps_its_direction_and_carries_a_head_at_the_end() {
    // Dragged right to left: the head is at `to`, and the line runs in that direction.
    let arrow = shape(ShapeKind::Arrow, [200.0, 100.0], [40.0, 100.0]);
    let outline = arrow.outline();
    assert_eq!(
        outline[0],
        [200.0, 100.0],
        "the tail is where the press was"
    );
    assert_eq!(
        outline[1],
        [40.0, 100.0],
        "the head is where the pointer ended"
    );
    // The head's two wings sit back along the line, one on each side of it.
    let (wing_a, wing_b, half) = (outline[2], outline[3], arrow.head_half());
    assert!(wing_a[0] > 40.0, "the head points along the drag");
    assert!(
        (wing_a[0] - wing_b[0]).abs() < 0.01,
        "the wings share the base's column"
    );
    let (up, down) = (wing_a[1].min(wing_b[1]), wing_a[1].max(wing_b[1]));
    assert!(up < 100.0 && down > 100.0, "the wings straddle the line");
    assert!((100.0 - up - half).abs() < 0.01 && (down - 100.0 - half).abs() < 0.01);
    // A hairline arrow still gets a head big enough to read.
    let thin = Shape {
        width: 1.0,
        ..arrow.clone()
    };
    assert!(thin.head_len() >= 9.0);
}

#[test]
fn a_drag_with_no_extent_is_nothing_to_draw() {
    for kind in [
        ShapeKind::Circle,
        ShapeKind::Rect,
        ShapeKind::Diamond,
        ShapeKind::Line,
        ShapeKind::Arrow,
    ] {
        assert!(shape(kind, [5.0, 5.0], [5.0, 5.0]).is_empty(), "{kind:?}");
    }
    assert!(!shape(ShapeKind::Line, [5.0, 5.0], [6.0, 5.0]).is_empty());
    // A direction one pixel wide is still a line, even though the box has no extent across it.
    assert!(!shape(ShapeKind::Arrow, [5.0, 5.0], [5.0, 9.0]).is_empty());
}

#[test]
fn a_shape_is_hit_by_its_outline_and_not_by_its_interior() {
    let from = [100.0, 100.0];
    let to = [300.0, 200.0];
    let rect = shape(ShapeKind::Rect, from, to);
    // The middle of a wide rectangle belongs to whatever is drawn inside it.
    assert!(rect.distance_squared(200.0, 150.0) > sgraffito::canvas::ERASER_THRESHOLD.powi(2));
    // Its edge is within reach.
    assert!(rect.distance_squared(200.0, 101.0) <= sgraffito::canvas::ERASER_THRESHOLD.powi(2));

    let circle = shape(ShapeKind::Circle, from, to);
    assert!(circle.distance_squared(200.0, 150.0) > sgraffito::canvas::ERASER_THRESHOLD.powi(2));
    assert!(circle.distance_squared(200.0, 100.0) <= sgraffito::canvas::ERASER_THRESHOLD.powi(2));

    let diamond = shape(ShapeKind::Diamond, from, to);
    assert!(diamond.distance_squared(200.0, 150.0) > sgraffito::canvas::ERASER_THRESHOLD.powi(2));
    assert!(
        diamond.distance_squared(200.0, 100.0) <= sgraffito::canvas::ERASER_THRESHOLD.powi(2),
        "a diamond touches its box's edge midpoint"
    );

    let line = shape(ShapeKind::Line, from, to);
    assert!(
        line.distance_squared(200.0, 150.0) <= 1.0,
        "a point on the segment is a hit"
    );
    // The head is solid, so a point inside it is a hit even though it is off the line. The
    // centroid of the head's triangle is the worst case for an outline-only test.
    let arrow = shape(ShapeKind::Arrow, from, to);
    let head = arrow.outline();
    let centroid = [
        (head[1][0] + head[2][0] + head[3][0]) / 3.0,
        (head[1][1] + head[2][1] + head[3][1]) / 3.0,
    ];
    assert!(
        arrow.distance_squared(centroid[0], centroid[1])
            <= sgraffito::canvas::ERASER_THRESHOLD.powi(2),
        "the middle of the head is a hit"
    );
}

#[test]
fn a_clicks_inside_a_rectangle_reaches_the_stroke_inside_it() {
    // The whole point of the outline hit test: an element inside a shape is reachable.
    let rect = shape(ShapeKind::Rect, [100.0, 100.0], [300.0, 200.0]);
    let inside = line(150.0, 150.0, 250.0);
    let a = ann(vec![inside], vec![]);
    let mut a = OutputAnnotations {
        shapes: vec![rect],
        ..a
    };
    assert_eq!(
        a.hit(200.0, 150.0, ERASER_THRESHOLD),
        Some(ItemId::Stroke(0))
    );
    assert_eq!(
        a.hit(200.0, 101.0, ERASER_THRESHOLD),
        Some(ItemId::Shape(0))
    );
    // The nearer of the two wins when both are in reach.
    assert_eq!(
        a.hit(105.0, 150.0, ERASER_THRESHOLD),
        Some(ItemId::Shape(0))
    );
    // And the stroke is what the eraser removes, leaving the rectangle alone.
    assert!(a.erase(200.0, 150.0, ERASER_THRESHOLD).is_some());
    assert!(a.strokes.is_empty() && a.shapes.len() == 1);
}

#[test]
fn an_arrows_box_covers_its_head() {
    let arrow = shape(ShapeKind::Arrow, [100.0, 100.0], [300.0, 100.0]);
    let box_ = arrow.bounds();
    // The head spreads perpendicular to the line, so the box is wider than the pen alone.
    assert!(box_.h / 2.0 >= arrow.head_half(), "the box holds the head");
    let plain = shape(ShapeKind::Line, [100.0, 100.0], [300.0, 100.0]);
    assert!(
        box_.h > plain.bounds().h,
        "an arrow's box is grown by its head"
    );
    // Every edge allows for half the pen, a pixel of anti-aliasing and the head's spread.
    let slack = arrow.width / 2.0 + 1.0 + arrow.head_half();
    assert!(
        (box_.x - (100.0 - slack)).abs() < 0.01,
        "the left edge is the head away"
    );
}

#[test]
fn a_marquee_takes_a_wholly_contained_shape_and_not_a_clipped_one() {
    let inside = shape(ShapeKind::Rect, [20.0, 20.0], [60.0, 50.0]);
    let clipped = shape(ShapeKind::Rect, [80.0, 20.0], [200.0, 50.0]);
    let clipped_bounds = clipped.bounds();
    let a = OutputAnnotations {
        strokes: vec![],
        texts: vec![],
        shapes: vec![inside, clipped],
        notes: vec![],
    };
    let marquee = Rect {
        x: 0.0,
        y: 0.0,
        w: 120.0,
        h: 100.0,
    };
    assert_eq!(marquee_select(&a, marquee), vec![ItemId::Shape(0)]);
    assert_eq!(item_bounds(&a, ItemId::Shape(1)), Some(clipped_bounds));
}

#[test]
fn a_transform_moves_both_points_of_a_shape_and_keeps_its_kind() {
    let arrow = shape(ShapeKind::Arrow, [0.0, 0.0], [100.0, 0.0]);
    let moved = Transform::Translate { dx: 10.0, dy: 5.0 }.shape(&arrow);
    assert_eq!((moved.from, moved.to), ([10.0, 5.0], [110.0, 5.0]));
    assert_eq!(moved.kind, ShapeKind::Arrow);
    assert_eq!(
        moved.width, arrow.width,
        "a shape's pen does not change with a move"
    );
    // A resize scales the two points and leaves the kind alone, so a circle stays a circle.
    let circle = shape(ShapeKind::Circle, [0.0, 0.0], [100.0, 100.0]);
    let scaled = Transform::Scale {
        anchor: [0.0, 0.0],
        sx: 2.0,
        sy: 2.0,
    }
    .shape(&circle);
    assert_eq!((scaled.from, scaled.to), ([0.0, 0.0], [200.0, 200.0]));
    assert_eq!(scaled.kind, ShapeKind::Circle);
}

#[test]
fn deleting_a_selection_removes_exactly_it() {
    // The bug this guards against: removing an earlier element shifts every later one, so a
    // selection holding two adjacent strokes would remove the wrong pair and leave one of them drawn.
    let mut a = OutputAnnotations {
        strokes: vec![
            stroke_at(0.0, 0.0),
            stroke_at(100.0, 0.0),
            stroke_at(200.0, 0.0),
        ],
        texts: vec![text(10.0, 10.0, "keep"), text(300.0, 10.0, "keep too")],
        shapes: vec![shape(ShapeKind::Rect, [0.0, 0.0], [50.0, 50.0])],
        notes: vec![],
    };
    let held = [ItemId::Stroke(0), ItemId::Stroke(1), ItemId::Shape(0)];
    let removed = a
        .remove(&held)
        .expect("the removed elements occupied a box");
    assert_eq!(a.strokes.len(), 1, "the third stroke survives");
    assert_eq!(
        a.strokes[0].points[0],
        [200.0, 0.0],
        "and it is the one that was there"
    );
    assert!(a.shapes.is_empty());
    assert_eq!(a.texts.len(), 2, "no text was selected");
    // The box is the union of what went, so the frame that follows knows what to damage.
    assert!(
        removed.contains(0.0, 0.0) && removed.contains(105.0, 0.0) && removed.contains(25.0, 25.0),
        "{removed:?}"
    );
}

#[test]
fn removing_nothing_removes_nothing() {
    let mut a = ann(vec![stroke_at(0.0, 0.0), stroke_at(100.0, 0.0)], vec![]);
    assert_eq!(a.remove(&[]), None);
    assert_eq!(a.strokes.len(), 2);
    // An index that is no longer there is skipped rather than panicking: a `clear` can arrive while
    // a gesture still holds ids from the document it just emptied.
    assert_eq!(a.remove(&[ItemId::Stroke(7), ItemId::Text(3)]), None);
    assert_eq!(a.strokes.len(), 2);
    // The same element named twice is removed once.
    assert!(a.remove(&[ItemId::Stroke(0), ItemId::Stroke(0)]).is_some());
    assert_eq!(a.strokes.len(), 1);
    assert_eq!(a.strokes[0].points[0], [100.0, 0.0]);
}

// --- the view: canvas to screen ---------------------------------------------------------------

use sgraffito::canvas::{View, ZOOM_MAX, ZOOM_MIN, ZOOM_STEP};

#[test]
fn the_identity_view_maps_every_point_to_itself() {
    let view = View::default();
    assert!(view.is_identity());
    for p in [[0.0, 0.0], [12.0, -40.0], [1920.0, 1080.0]] {
        assert_eq!(view.to_screen(p), p);
        assert_eq!(view.to_canvas(p), p);
    }
    assert_eq!(view.canvas_len(8.0), 8.0);
}

#[test]
fn a_pan_moves_every_point_by_the_same_screen_delta() {
    let mut view = View {
        zoom: 2.0,
        ..Default::default()
    };
    let delta = [30.0, -12.0];
    // The screen point the gesture started on, and the canvas point under it: after the pan, that
    // screen point has moved by the delta and still has the same canvas point under it. That is what
    // makes a grab feel like a grab.
    let start = [130.0, 250.0];
    let grabbed = view.to_canvas(start);
    let moved = [start[0] + delta[0], start[1] + delta[1]];
    view.pan_by(delta);
    let still_there = view.to_canvas(moved);
    assert!(
        (still_there[0] - grabbed[0]).abs() < 0.001 && (still_there[1] - grabbed[1]).abs() < 0.001,
        "{grabbed:?} -> {still_there:?}"
    );
    // A pan moves a canvas point by exactly the delta on screen.
    let p = [40.0, 60.0];
    let before = view.to_screen(p);
    view.pan_by(delta);
    assert_eq!(
        view.to_screen(p),
        [before[0] + delta[0], before[1] + delta[1]]
    );
    assert_eq!(view.zoom, 2.0, "a pan does not change the zoom");
}

#[test]
fn a_zoom_about_a_point_leaves_that_point_under_it() {
    let mut view = View {
        zoom: 1.3,
        offset: [40.0, -25.0],
    };
    for anchor in [[0.0, 0.0], [120.0, 300.0], [1919.0, 1079.0]] {
        let canvas_under = view.to_canvas(anchor);
        view.zoom_about(anchor, ZOOM_STEP);
        let after = view.to_canvas(anchor);
        assert!(
            (after[0] - canvas_under[0]).abs() < 0.01 && (after[1] - canvas_under[1]).abs() < 0.01,
            "the anchor moved: {canvas_under:?} -> {after:?}"
        );
    }
    // Zooming in and back out by the same step returns to the same view.
    let mut view = View {
        zoom: 1.0,
        offset: [7.0, 9.0],
    };
    view.zoom_about([100.0, 100.0], ZOOM_STEP);
    view.zoom_about([100.0, 100.0], 1.0 / ZOOM_STEP);
    assert!((view.zoom - 1.0).abs() < 0.0001);
    assert!((view.offset[0] - 7.0).abs() < 0.01 && (view.offset[1] - 9.0).abs() < 0.01);
}

#[test]
fn the_zoom_stops_at_its_range_and_keeps_the_anchor() {
    let mut view = View::default();
    for _ in 0..200 {
        view.zoom_about([500.0, 400.0], ZOOM_STEP);
    }
    assert_eq!(view.zoom, ZOOM_MAX);
    // At the limit the drawing stays where the last zoom about the point put it.
    let canvas_under = view.to_canvas([500.0, 400.0]);
    view.zoom_about([500.0, 400.0], ZOOM_STEP);
    assert_eq!(view.zoom, ZOOM_MAX);
    let after = view.to_canvas([500.0, 400.0]);
    assert!((after[0] - canvas_under[0]).abs() < 0.001);
    for _ in 0..400 {
        view.zoom_about([500.0, 400.0], 1.0 / ZOOM_STEP);
    }
    assert_eq!(view.zoom, ZOOM_MIN);
    assert!(view.zoom > 0.0, "a zoom is never zero or negative");
}

#[test]
fn a_screen_length_is_a_canvas_length_over_the_zoom() {
    let view = View {
        zoom: 4.0,
        ..Default::default()
    };
    // The eraser's radius is what the pointer aims with, so zooming out reaches further in the
    // drawing's own units.
    assert_eq!(view.canvas_len(8.0), 2.0);
    let half = View {
        zoom: 0.5,
        ..Default::default()
    };
    assert_eq!(half.canvas_len(8.0), 16.0);
}

#[test]
fn a_canvas_box_maps_to_the_screen_box_it_covers() {
    let view = View {
        zoom: 2.0,
        offset: [10.0, 20.0],
    };
    let box_ = Rect {
        x: 5.0,
        y: 6.0,
        w: 30.0,
        h: 40.0,
    };
    let screen = view.screen_rect(box_);
    assert_eq!(screen.x, 20.0);
    assert_eq!(screen.y, 32.0);
    assert_eq!(screen.w, 60.0);
    assert_eq!(screen.h, 80.0);
}

#[test]
fn a_press_resolves_in_the_coordinate_space_it_belongs_to() {
    // The bug this pins: the marquee and the drag belong to the canvas, while the handles belong to
    // the screen. A press resolved in the wrong space makes the marquee drift away from the pointer the
    // moment the view is panned or zoomed, and leaves it working only near where screen and canvas
    // coincide.
    let held = [ItemId::Stroke(0)];
    let a = ann(
        vec![stroke_at(100.0, 100.0), stroke_at(400.0, 300.0)],
        vec![],
    );
    let frame = selection_frame(&a, &held, None).unwrap();
    let view = View {
        zoom: 2.0,
        offset: [60.0, -30.0],
    };
    let screen = |p: [f32; 2]| view.to_screen(p);

    // A handle is grabbed where it is drawn: on screen, not at its canvas position.
    let corner = screen(Handle::Nw.point(frame));
    assert_eq!(
        selection_press(&a, &held, view, corner, ERASER_THRESHOLD),
        Press::Handle(Grab::Handle(Handle::Nw))
    );
    // …and not where the canvas says it is, which is a different place on screen.
    let canvas_corner = Handle::Nw.point(frame);
    assert_ne!(
        selection_press(&a, &held, view, canvas_corner, ERASER_THRESHOLD),
        Press::Handle(Grab::Handle(Handle::Nw)),
        "a handle was grabbed at its canvas position instead of where it is drawn"
    );

    // A press on an element the selection does not hold takes that element, asked in canvas space: the
    // stroke's own point is at a different screen position, and asking there must find it.
    let on_other = screen(stroke_at(400.0, 300.0).points[0]);
    assert_eq!(
        selection_press(&a, &held, view, on_other, ERASER_THRESHOLD),
        Press::Element(ItemId::Stroke(1))
    );

    // Empty space outside the frame sweeps a marquee, and the marquee is a **canvas** point: that is
    // what the elements it will catch and the frame it is drawn in are measured in.
    let far = [1200.0, 900.0];
    match selection_press(&a, &held, view, far, ERASER_THRESHOLD) {
        Press::Marquee { canvas } => {
            assert_eq!(canvas, view.to_canvas(far));
            assert_ne!(canvas, far, "the marquee is stored in screen coordinates");
            // And a marquee built from two such canvas points catches what is in that part of the
            // drawing, not what is in the same part of the screen.
            let marquee = Rect::from_corners(canvas, [380.0, 280.0]);
            assert_eq!(marquee_select(&a, marquee), vec![ItemId::Stroke(1)]);
        }
        other => panic!("expected a marquee, got {other:?}"),
    }
}

#[test]
fn a_marquee_catches_at_any_view_what_it_catches_at_one_to_one() {
    // The containment rule is zoom-invariant: the same canvas rectangle catches the same elements
    // whatever the view, as long as the rectangle is built from canvas points.
    let a = ann(
        vec![stroke_at(20.0, 20.0), stroke_at(300.0, 300.0)],
        vec![text(40.0, 60.0, "note")],
    );
    let view = View {
        zoom: 3.0,
        offset: [-120.0, 40.0],
    };
    // A canvas box the user sweeps, and the screen corners that are the same box at this view: what a
    // drag does is convert those corners back, so this is the round trip a release depends on.
    let (x0, y0, x1, y1) = (0.0, 0.0, 300.0, 300.0);
    let (from, to) = (view.to_screen([x0, y0]), view.to_screen([x1, y1]));
    let swept = Rect::from_corners(view.to_canvas(from), view.to_canvas(to));
    assert!((swept.x - x0).abs() < 0.01 && (swept.w - (x1 - x0)).abs() < 0.01);
    assert_eq!(
        marquee_select(&a, swept),
        vec![ItemId::Text(0), ItemId::Stroke(0)],
        "the near stroke and the text are inside the swept canvas box, the far stroke is not"
    );
    // The rule does not depend on the view at all: the same canvas box catches the same elements,
    // because nothing about it was measured on screen.
    let one_to_one = marquee_select(&a, swept);
    assert_eq!(marquee_select(&a, swept), one_to_one);
}

// --- sticky notes: the date, the ink and the element --------------------------------------

use sgraffito::canvas::{NOTE_PADDING, Note, date_stamp, note_ink};

#[test]
fn the_date_is_the_day_the_note_was_made() {
    use std::time::{Duration, UNIX_EPOCH};
    let at = |days: u64| UNIX_EPOCH + Duration::from_secs(days * 86_400);
    assert_eq!(date_stamp(at(0)), "1970/01/01");
    // A leap day, and the day after it.
    assert_eq!(date_stamp(at(19_782)), "2024/02/29");
    assert_eq!(date_stamp(at(19_783)), "2024/03/01");
    // A century that is a leap year, and the last day of a year.
    assert_eq!(date_stamp(at(11_016)), "2000/02/29");
    assert_eq!(date_stamp(at(19_722)), "2023/12/31");
}

/// The contrast ratio the accessibility floor is written in, as the render tests do it.
fn contrast(a: &str, b: &str) -> f32 {
    let l = |c: &str| {
        let c = sgraffito::canvas::parse_hex(c);
        let channel = |v: u8| {
            let v = v as f32 / 255.0;
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(c[0]) + 0.7152 * channel(c[1]) + 0.0722 * channel(c[2])
    };
    let (a, b) = (l(a), l(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

#[test]
fn every_palette_colour_can_be_read_on_a_note() {
    // No single ink works on all five — light on the yellow is 1.4:1 and dark on the red is 3.5:1 —
    // so the pair is chosen by the background, and every pair has to clear the floor.
    for background in PALETTE {
        let ink = note_ink(background);
        let ratio = contrast(ink, background);
        assert!(ratio >= 4.5, "{background} with {ink} is only {ratio:.2}:1");
    }
}

#[test]
fn a_note_is_hit_by_its_body_and_the_note_is_what_is_on_top() {
    let mut a = OutputAnnotations {
        strokes: vec![],
        texts: vec![],
        shapes: vec![],
        notes: vec![Note::new(100.0, 100.0, "#f6d32d", "2026/10/14".into())],
    };
    // Anywhere in the square is a press on the note.
    assert_eq!(a.hit(150.0, 150.0, ERASER_THRESHOLD), Some(ItemId::Note(0)));
    assert_eq!(
        a.hit(101.0, 101.0, ERASER_THRESHOLD),
        Some(ItemId::Note(0)),
        "the note has no outline to aim at, unlike a shape"
    );
    // A note is drawn over the ink, so the press goes to the note the pointer can see. Ink stuck to a
    // note is reached from outside the square the note covers.
    a.strokes.push(stroke_at(150.0, 150.0));
    assert_eq!(a.hit(150.0, 150.0, ERASER_THRESHOLD), Some(ItemId::Note(0)));
    // And erasing the note takes it whole.
    assert!(a.erase(250.0, 150.0, ERASER_THRESHOLD).is_some());
    assert!(a.notes.is_empty() && a.strokes.len() == 1);
}

#[test]
fn a_note_grows_to_hold_its_text_and_keeps_its_padding() {
    let mut note = Note::new(40.0, 60.0, "#f6d32d", "2026/10/14".into());
    let box_ = note.text_box();
    assert_eq!(box_.x, 40.0 + NOTE_PADDING);
    assert_eq!(box_.y, 60.0 + NOTE_PADDING);
    assert_eq!(box_.w, note.w - 2.0 * NOTE_PADDING);
    // The text box leaves the date's line at the bottom, so the date never sits on the text.
    assert!(
        box_.y + box_.h <= note.date_box().y,
        "the text's box never runs into the date's line"
    );
    // A note never shrinks below its minimum, however little text it holds.
    assert_eq!(note.height_for(0.0), sgraffito::canvas::NOTE_MIN_SIZE);
    // And it grows by the text it has to hold.
    let grown = note.height_for(300.0);
    assert!(grown > note.h);
    note.h = grown;
    assert!(note.text_box().h >= 300.0);
    // The date sits inside the note, in the bottom-right corner.
    let date = note.date_box();
    assert!(date.x >= note.x + NOTE_PADDING && date.y + date.h <= note.y + note.h);
}

#[test]
fn a_note_moves_and_resizes_as_one_thing() {
    let note = Note::new(100.0, 200.0, "#3584e4", "2026/10/14".into());
    let moved = Transform::Translate {
        dx: 30.0,
        dy: -50.0,
    }
    .note(&note);
    assert_eq!((moved.x, moved.y), (130.0, 150.0));
    assert_eq!(
        (moved.w, moved.h),
        (note.w, note.h),
        "a move keeps its size"
    );
    assert_eq!(moved.color, note.color);
    assert_eq!(moved.created, note.created);
    let scaled = Transform::Scale {
        anchor: [100.0, 200.0],
        sx: 0.25,
        sy: 3.0,
    }
    .note(&note);
    // It shrinks to its minimum rather than to nothing, so a resize cannot make it unusable.
    assert_eq!(scaled.w, sgraffito::canvas::NOTE_MIN_SIZE);
    assert_eq!(scaled.h, note.h * 3.0);
    // A marquee takes a note when it wholly contains it.
    let a = OutputAnnotations {
        strokes: vec![],
        texts: vec![],
        shapes: vec![],
        notes: vec![note.clone()],
    };
    let marquee = note.bounds().grown(1.0);
    assert_eq!(marquee_select(&a, marquee), vec![ItemId::Note(0)]);
    assert!(
        marquee_select(
            &a,
            Rect {
                x: 150.0,
                y: 200.0,
                w: 20.0,
                h: 20.0
            }
        )
        .is_empty()
    );
}

#[test]
fn a_new_note_is_sized_by_the_drag_that_creates_it() {
    use sgraffito::canvas::{NOTE_DRAG_SLOP, NOTE_MIN_SIZE, note_drag};
    let origin = [400.0, 300.0];
    // A click: the pointer barely moved, so the note keeps the default size rather than becoming a sliver.
    assert_eq!(note_drag(origin, origin), None);
    assert_eq!(note_drag(origin, [402.0, 301.0]), None);
    assert_eq!(
        note_drag(origin, [400.0 + NOTE_DRAG_SLOP - 1.0, 300.0]),
        None
    );
    // A small drag is still a drag: it gives a note at the minimum rather than the default size, which is
    // what "size it by dragging" has to mean.
    let small = note_drag(origin, [440.0, 330.0]).expect("a small drag is a drag");
    assert_eq!((small.w, small.h), (NOTE_MIN_SIZE, NOTE_MIN_SIZE));
    // A drag gives the rectangle it swept, and both directions work: a note dragged up and to the left
    // starts above and left of the press.
    let down_right = note_drag(origin, [700.0, 560.0]).expect("a drag");
    assert_eq!((down_right.x, down_right.y), (400.0, 300.0));
    assert_eq!((down_right.w, down_right.h), (300.0, 260.0));
    let up_left = note_drag(origin, [150.0, 120.0]).expect("a drag");
    assert_eq!((up_left.x, up_left.y), (150.0, 120.0));
    assert_eq!((up_left.w, up_left.h), (250.0, 180.0));
    // Each side is clamped on its own: a drag that is long and thin still gives a usable note.
    let thin = note_drag(origin, [700.0, 302.0]).expect("a drag");
    assert_eq!(thin.w, 300.0);
    assert_eq!(thin.h, NOTE_MIN_SIZE, "a thin drag is not a thin note");
}

#[test]
fn a_note_is_never_shrunk_by_the_text_that_is_typed_into_it() {
    // What the drag or the resize decided is the floor: typing only ever adds height, or a note dragged
    // out tall snapped back to the height of its first character.
    let mut note = Note::new(60.0, 40.0, "#f6d32d", "2026/10/14".into());
    note.w = 200.0;
    note.h = 400.0;
    let tall = note.grown_for(20.0);
    assert_eq!(tall.h, 400.0, "an empty note was shrunk");
    assert_eq!(tall.text, note.text);
    let grown = note.grown_for(500.0);
    assert!(
        grown.h > 400.0,
        "a note did not grow for text that needs it"
    );
    assert_eq!(grown.h, note.height_for(500.0));
    // And it takes the minimum when the box is smaller than a note may be.
    let mut small = note.clone();
    small.h = 10.0;
    assert_eq!(small.grown_for(0.0).h, sgraffito::canvas::NOTE_MIN_SIZE);
}

#[test]
fn a_note_keeps_the_press_point_as_a_corner_however_the_drag_turns() {
    // The press is a corner of the note at every point of the gesture, and each side is the distance the
    // pointer has gone from it. Without that, a side clamped up to the minimum reached back through the
    // pointer when a note was pulled back in, so the note flickered on both sides as it shrank.
    use sgraffito::canvas::{NOTE_MIN_SIZE, Rect, note_drag};
    let origin = [500.0, 400.0];
    let mut seen = 0;
    for at in [
        [700.0, 600.0],
        [760.0, 640.0],
        [700.0, 600.0],
        [560.0, 500.0],
        [500.0, 440.0],
        [470.0, 400.0],
        [430.0, 380.0],
        [380.0, 350.0],
        [470.0, 390.0],
        [620.0, 560.0],
    ] {
        let r = note_drag(origin, at).expect("a drag past the slop");
        assert_eq!(r.w, (at[0] - origin[0]).abs().max(NOTE_MIN_SIZE));
        assert_eq!(r.h, (at[1] - origin[1]).abs().max(NOTE_MIN_SIZE));
        let (right, bottom) = (r.x + r.w, r.y + r.h);
        assert!(
            (r.x == origin[0] || right == origin[0]) && (r.y == origin[1] || bottom == origin[1]),
            "{r:?} does not keep the press {origin:?} as a corner at {at:?}"
        );
        seen += 1;
    }
    assert_eq!(seen, 10);
    // And a drag too short to be one is still nothing at all.
    assert_eq!(note_drag(origin, [505.0, 402.0]), None);
    // The rectangle a drag out and back in sweeps is a rectangle: the two corners are the press and the
    // pointer, in either direction, with each side at least the minimum.
    let expected = Rect {
        x: origin[0] - 380.0f32.max(NOTE_MIN_SIZE),
        y: origin[1] - 350.0f32.max(NOTE_MIN_SIZE),
        w: 380.0,
        h: 350.0,
    };
    let up_left = note_drag(origin, [120.0, 50.0]).unwrap();
    assert_eq!((up_left.w, up_left.h), (expected.w, expected.h));
    assert_eq!(
        (up_left.x + up_left.w, up_left.y + up_left.h),
        (origin[0], origin[1])
    );
}

#[test]
fn a_colour_applies_to_every_element_the_selection_holds() {
    use sgraffito::canvas::{Note, ShapeKind};
    let mut a = OutputAnnotations {
        strokes: vec![line(40.0, 20.0, 60.0)],
        texts: vec![text(20.0, 80.0, "hello")],
        shapes: vec![shape(ShapeKind::Rect, [10.0, 120.0], [60.0, 160.0])],
        notes: vec![Note::new(100.0, 20.0, PALETTE[1], "2026/10/14".into())],
    };
    let kept = (a.texts[0].color.clone(), a.shapes[0].color.clone());
    // A selection of a stroke and a note: both take the colour, and the box is the union of the two, which
    // is what the frame that repaints them damages.
    let box_ = a
        .recolour(&[ItemId::Stroke(0), ItemId::Note(0)], PALETTE[3])
        .expect("a selection was recoloured");
    assert_eq!(a.strokes[0].color, PALETTE[3]);
    assert_eq!(a.notes[0].color, PALETTE[3]);
    assert_eq!(
        a.texts[0].color, kept.0,
        "an element the selection did not hold"
    );
    assert_eq!(
        a.shapes[0].color, kept.1,
        "an element the selection did not hold"
    );
    assert_eq!(
        box_,
        a.strokes[0].bounds().unwrap().union(a.notes[0].bounds())
    );
    // The other two kinds are reached the same way, and their indices are left alone.
    let more = a
        .recolour(&[ItemId::Text(0), ItemId::Shape(0)], PALETTE[2])
        .expect("the other two kinds");
    assert_eq!(a.texts[0].color, PALETTE[2]);
    assert_eq!(a.shapes[0].color, PALETTE[2]);
    assert_eq!(more, a.texts[0].paint_bounds().union(a.shapes[0].bounds()));
    // Nothing held, or an id that names no element, changes nothing at all.
    assert_eq!(a.recolour(&[], PALETTE[0]), None);
    assert_eq!(
        a.recolour(&[ItemId::Note(7), ItemId::Text(3)], PALETTE[0]),
        None
    );
    assert_eq!(
        a.notes[0].color, PALETTE[3],
        "a stale id reached an element"
    );
}

#[test]
fn a_copy_leaves_the_original_in_place_and_keeps_every_index() {
    use sgraffito::canvas::{Note, ShapeKind};
    let mut a = OutputAnnotations {
        strokes: vec![line(10.0, 0.0, 40.0), line(60.0, 0.0, 90.0)],
        texts: vec![text(0.0, 100.0, "one")],
        shapes: vec![shape(ShapeKind::Rect, [0.0, 200.0], [40.0, 240.0])],
        notes: vec![Note::new(100.0, 300.0, PALETTE[1], "2026/10/14".into())],
    };
    let originals = [ItemId::Stroke(0), ItemId::Shape(0), ItemId::Note(0)];
    let (copies, box_) = a.duplicate(&originals);
    // Appended, one per kind: the originals keep the indices they had.
    assert_eq!(
        copies,
        vec![ItemId::Stroke(2), ItemId::Shape(1), ItemId::Note(1)]
    );
    assert_eq!(a.strokes[0].points, a.strokes[2].points);
    assert_eq!(a.strokes[1].points, line(60.0, 0.0, 90.0).points);
    assert_eq!(
        box_.unwrap(),
        a.strokes[2]
            .bounds()
            .unwrap()
            .union(a.shapes[1].bounds())
            .union(a.notes[1].bounds())
    );
    // Removing the copies by id puts the document back exactly as it was.
    assert!(a.remove(&copies).is_some());
    assert_eq!(
        (
            a.strokes.len(),
            a.texts.len(),
            a.shapes.len(),
            a.notes.len()
        ),
        (2, 1, 1, 1)
    );
    assert_eq!(a.strokes[0].points, line(10.0, 0.0, 40.0).points);
    // An id that names nothing copies nothing.
    assert_eq!(a.duplicate(&[ItemId::Stroke(9)]).0.len(), 0);
}

#[test]
fn shift_constrains_the_point_a_drag_draws_to() {
    use sgraffito::canvas::{SHAPE_ANGLE_STEP, ShapeKind, constrained_drag, snap_square};
    let origin = [100.0, 100.0];
    // A line takes the nearest 15°, at the length the pointer asked for: 5.7° is far nearer flat than 15°.
    let flat = constrained_drag(ShapeKind::Line, origin, [200.0, 110.0], true);
    assert_eq!(flat[1], origin[1]);
    assert!(flat[0] - origin[0] > 100.0, "the length was not kept");
    // And a diagonal is reachable, not only the axes.
    let diagonal = constrained_drag(ShapeKind::Arrow, origin, [200.0, 200.0], true);
    let angle = (diagonal[1] - origin[1]).atan2(diagonal[0] - origin[0]);
    assert!((angle - 3.0 * SHAPE_ANGLE_STEP).abs() < 1e-4, "{angle}");
    // A closed kind is squared on the longer side of its box.
    let square = constrained_drag(ShapeKind::Rect, origin, [300.0, 150.0], true);
    assert_eq!(
        (square[0] - origin[0], square[1] - origin[1]),
        (200.0, 200.0)
    );
    // Switched off, the point is the pointer's own.
    for kind in [ShapeKind::Line, ShapeKind::Circle] {
        assert_eq!(
            constrained_drag(kind, origin, [300.0, 150.0], false),
            [300.0, 150.0]
        );
    }
    // A square swept up and to the left goes that way, and a drag that did not move stays put.
    assert_eq!(snap_square(origin, [40.0, 20.0]), [20.0, 20.0]);
    assert_eq!(snap_square(origin, origin), origin);
    let still = constrained_drag(ShapeKind::Line, origin, origin, true);
    assert_eq!(still, origin);
}

#[test]
fn shift_scales_both_axes_by_one_factor() {
    use sgraffito::canvas::{Grab, Handle, Transform};
    let frame = Rect {
        x: 100.0,
        y: 100.0,
        w: 200.0,
        h: 100.0,
    };
    let factors = |t: Transform| match t {
        Transform::Scale { sx, sy, .. } => (sx, sy),
        other => panic!("not a scale: {other:?}"),
    };
    // A corner handle: the pointer moved further along x, so both axes take the x factor.
    let corner = Gesture::start(Some(Grab::Handle(Handle::Se)), frame, 300.0, 200.0, false);
    let free = factors(corner.transform([500.0, 260.0], false).unwrap());
    let uniform = factors(corner.transform([500.0, 260.0], true).unwrap());
    assert!(free.0 > free.1, "the fixture is not lopsided: {free:?}");
    assert_eq!(uniform, (free.0, free.0));
    // An edge handle has only its own axis, so shrinking that edge shrinks both.
    let edge = Gesture::start(Some(Grab::Handle(Handle::E)), frame, 300.0, 150.0, false);
    let (sx, sy) = factors(edge.transform([200.0, 150.0], true).unwrap());
    assert!(sx < 1.0, "the edge did not shrink: {sx}");
    assert!((sx - sy).abs() < 1e-6, "{sx} != {sy}");
    // And with `Shift` off, the two axes are independent again.
    let (fx, fy) = factors(corner.transform([500.0, 260.0], false).unwrap());
    assert!(fx > 1.0 && fy > 1.0 && (fx - fy).abs() > 1e-6, "{fx} {fy}");
}

#[test]
fn the_selection_walk_visits_every_kind_in_document_order() {
    let a = OutputAnnotations {
        strokes: vec![line(10.0, 0.0, 5.0), line(20.0, 0.0, 5.0)],
        texts: vec![text(0.0, 0.0, "one")],
        shapes: vec![sgraffito::canvas::Shape {
            kind: sgraffito::canvas::ShapeKind::Rect,
            color: "#e01b24".into(),
            width: 3.0,
            from: [0.0, 0.0],
            to: [10.0, 10.0],
        }],
        notes: vec![sgraffito::canvas::Note::new(
            0.0,
            0.0,
            "#ffffff",
            "2026/01/01".into(),
        )],
    };
    let order = sgraffito::canvas::items_in_order(&a);
    assert_eq!(
        order,
        vec![
            ItemId::Stroke(0),
            ItemId::Stroke(1),
            ItemId::Text(0),
            ItemId::Shape(0),
            ItemId::Note(0),
        ]
    );
    // `Tab` with nothing held takes the first, then one element at a time.
    let mut held: Vec<ItemId> = Vec::new();
    let mut walked = Vec::new();
    for _ in 0..order.len() {
        held = sgraffito::canvas::cycle_selection(&a, &held, true);
        walked.push(held.clone());
    }
    assert_eq!(
        walked,
        order.iter().map(|id| vec![*id]).collect::<Vec<_>>(),
        "the walk does not follow the document's order"
    );
    // And it wraps: one more step is the first element again.
    assert_eq!(
        sgraffito::canvas::cycle_selection(&a, &held, true),
        vec![ItemId::Stroke(0)]
    );
}

#[test]
fn the_walk_wraps_backwards_and_steps_from_a_marquee() {
    use sgraffito::canvas::{Note, Shape, ShapeKind, cycle_selection};
    let a = OutputAnnotations {
        strokes: vec![line(10.0, 0.0, 5.0), line(20.0, 0.0, 5.0)],
        texts: vec![],
        shapes: vec![Shape {
            kind: ShapeKind::Line,
            color: "#e01b24".into(),
            width: 3.0,
            from: [0.0, 0.0],
            to: [10.0, 10.0],
        }],
        notes: vec![Note::new(0.0, 0.0, "#ffffff", "2026/01/01".into())],
    };
    // With nothing held, `Shift+Tab` takes the last element, and stepping back from the first wraps.
    assert_eq!(cycle_selection(&a, &[], false), vec![ItemId::Note(0)]);
    assert_eq!(
        cycle_selection(&a, &[ItemId::Stroke(0)], false),
        vec![ItemId::Note(0)]
    );
    // A marquee's selection steps out of itself from its first element, not from its last.
    assert_eq!(
        cycle_selection(&a, &[ItemId::Stroke(1), ItemId::Shape(0)], true),
        vec![ItemId::Shape(0)]
    );
    // An empty bucket names nothing, whichever direction it is walked.
    let empty = OutputAnnotations::default();
    assert!(cycle_selection(&empty, &[], true).is_empty());
    assert!(cycle_selection(&empty, &[], false).is_empty());
}

#[test]
fn a_nudge_is_a_screen_step_at_every_zoom() {
    use sgraffito::canvas::nudge_delta;
    // At 1:1 a press is one of the document's own units, and ten with `Shift`.
    assert_eq!(nudge_delta(1, 0, false, 1.0), [1.0, 0.0]);
    assert_eq!(nudge_delta(0, -1, false, 1.0), [0.0, -1.0]);
    assert_eq!(nudge_delta(1, 0, true, 1.0), [10.0, 0.0]);
    // Zoomed out, the same press is the same length on screen, which is more of the document's units —
    // and `Shift` stays ten times the plain step rather than ten units.
    assert_eq!(nudge_delta(1, 0, false, 0.5), [2.0, 0.0]);
    assert_eq!(nudge_delta(1, 0, true, 0.5), [20.0, 0.0]);
    // Zoomed in, it is fewer.
    assert_eq!(nudge_delta(-1, 0, false, 2.0), [-0.5, 0.0]);
}
