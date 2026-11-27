use sgraffito::canvas::{
    Board, ERASER_THRESHOLD, ItemId, LinearHandle, PALETTE, Rect, SEED_MAX_CHARS, Shape, ShapeKind,
    Stroke, TextItem, View, linear_handle_at, seed_of, selected_linear,
};
use sgraffito::sketch::SKETCH_SLACK;

fn line(y: f32, x0: f32, x1: f32) -> Stroke {
    Stroke {
        color: "#e01b24".into(),
        width: 3.0,
        points: vec![[x0, y], [x1, y]],
        attach: [None, None],
    }
}

fn ann(strokes: Vec<Stroke>, texts: Vec<TextItem>) -> Board {
    Board {
        strokes,
        texts,
        shapes: vec![],
        notes: vec![],
    }
}

fn text(x: f32, y: f32, s: &str) -> TextItem {
    TextItem {
        container: None,
        x,
        y,
        color: "#ffffff".into(),
        size: 18.0,
        text: s.into(),
        link: None,
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
fn stroke_bounds_cover_the_width_and_its_paint_box_adds_the_wander() {
    let s = Stroke {
        color: "#ffffff".into(),
        width: 4.0,
        points: vec![[10.0, 20.0], [30.0, 20.0]],
        attach: [None, None],
    };
    // Half the stroke width and one logical pixel for the anti-aliasing edge: the geometry the user
    // drew, which is what a selection frame and a marquee measure.
    let slack = 2.0 + 1.0;
    let exact = Rect {
        x: 10.0 - slack,
        y: 20.0 - slack,
        w: 20.0 + 2.0 * slack,
        h: 2.0 * slack,
    };
    assert_eq!(s.bounds(), Some(exact));
    // The box a damaged region is cut from is the bigger one: the hand-drawn style reaches past it.
    assert_eq!(s.paint_bounds(), Some(exact.grown(SKETCH_SLACK)));
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
            x: 5.0 - slack,
            y: 5.0 - slack,
            w: 2.0 * slack,
            h: 2.0 * slack
        })
    );
}

#[test]
fn the_wander_is_room_for_painting_and_not_for_hitting() {
    // The bounds grew so a damaged region repaints the whole mark. The eraser must not grow with
    // them: what a mark looks like never changes what it is.
    let line = Stroke {
        color: "#ffffff".into(),
        width: 2.0,
        points: vec![[0.0, 0.0], [100.0, 0.0]],
        attach: [None, None],
    };
    let a = Board {
        strokes: vec![line],
        texts: vec![],
        shapes: vec![],
        notes: vec![],
    };
    assert_eq!(a.hit(50.0, 0.0, ERASER_THRESHOLD), Some(ItemId::Stroke(0)));
    assert_eq!(a.hit(50.0, ERASER_THRESHOLD + 1.0, ERASER_THRESHOLD), None);
    assert_eq!(
        a.hit(50.0, SKETCH_SLACK.floor(), ERASER_THRESHOLD),
        None,
        "the drawn wander is not a target"
    );
}

// --- selection: the frame, the marquee and the three gestures -------------------------------

use sgraffito::canvas::{
    Gesture, Grab, Handle, Press, SELECT_HANDLE_HIT, SELECT_ROTATE_GAP, Selection, Transform,
    grab_at, item_bounds, marquee_select, rotate_handle, selection_bounds, selection_frame,
    selection_press, toggle_selection,
};

fn stroke_at(x: f32, y: f32) -> Stroke {
    Stroke {
        color: "#e01b24".into(),
        width: 2.0,
        points: vec![[x, y], [x + 10.0, y]],
        attach: [None, None],
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
        attach: [None, None],
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
        attach: [None, None],
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

fn shape(kind: ShapeKind, from: [f32; 2], to: [f32; 2]) -> Shape {
    Shape {
        roundness: None,
        points: Vec::new(),
        kind,
        color: "#e01b24".into(),
        width: 3.0,
        from,
        to,
        rotate: 0.0,
        id: 0,
        attach: [None, None],
        fill: None,
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
        roundness: None,
        points: Vec::new(),
        width: 1.0,
        fill: None,
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
    let mut a = Board {
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
    // Every edge allows for half the pen, a pixel of anti-aliasing and the head's spread; the wander
    // is the paint box's, not the geometry's.
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
    let a = Board {
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

fn close(a: [f32; 2], b: [f32; 2]) -> bool {
    (a[0] - b[0]).abs() < 1e-3 && (a[1] - b[1]).abs() < 1e-3
}

#[test]
fn a_turn_visibly_rotates_every_closed_shape() {
    use std::f32::consts::FRAC_PI_2;
    for kind in [ShapeKind::Circle, ShapeKind::Rect, ShapeKind::Diamond] {
        let original = shape(kind, [100.0, 100.0], [200.0, 150.0]);
        let pivot = original.centre();
        let turned = Transform::Turn {
            pivot,
            radians: FRAC_PI_2,
        }
        .shape(&original);

        assert!((turned.rotate - FRAC_PI_2).abs() < 1e-6);
        assert_eq!(
            (turned.from, turned.to),
            (original.from, original.to),
            "{kind:?} kept its local box, so it kept its size"
        );
        assert!(
            close(turned.centre(), pivot),
            "{kind:?} turned about its own centre"
        );
        assert_ne!(
            turned.outline(),
            original.outline(),
            "{kind:?} draws a different outline turned"
        );
        // The first outline point swings about the centre: the drawn geometry really moved.
        let expected = match kind {
            ShapeKind::Circle => [150.0, 175.0],
            ShapeKind::Rect => [175.0, 75.0],
            _ => [175.0, 125.0],
        };
        assert!(
            close(turned.outline()[0], expected),
            "{kind:?} outline starts at {:?}",
            turned.outline()[0]
        );
    }
}

#[test]
fn a_turn_swings_a_shape_around_a_pivot_outside_it() {
    use std::f32::consts::FRAC_PI_2;
    let original = shape(ShapeKind::Rect, [100.0, 100.0], [200.0, 150.0]);
    let turned = Transform::Turn {
        pivot: [0.0, 0.0],
        radians: FRAC_PI_2,
    }
    .shape(&original);
    // The centre (150, 125) turns a quarter turn about the origin.
    assert!(close(turned.centre(), [-125.0, 150.0]));
    // A rigid turn keeps the box's own extents, so the shape is not squashed by the bounding box.
    let before = original.box_geometry().1;
    let after = turned.box_geometry().1;
    assert!(
        close(before, after),
        "half-extents {:?} became {:?}",
        before,
        after
    );
    assert_eq!(turned.outline().len(), original.outline().len());
}

#[test]
fn a_turned_shape_is_hit_on_the_outline_the_user_sees() {
    use std::f32::consts::FRAC_PI_2;
    let original = shape(ShapeKind::Rect, [0.0, 0.0], [100.0, 50.0]);
    let turned = Transform::Turn {
        pivot: original.centre(),
        radians: FRAC_PI_2,
    }
    .shape(&original);

    // The old top-left corner is now the turned top-right corner.
    assert!(
        close(turned.outline()[0], [75.0, -25.0]),
        "corner at {:?}",
        turned.outline()[0]
    );
    assert!(
        turned.distance_squared(75.0, -25.0) <= 1.0,
        "the drawn corner is a hit"
    );
    assert!(
        original.distance_squared(50.0, 0.0) <= 1.0,
        "the unrotated top edge is still a hit"
    );
    assert!(
        turned.distance_squared(50.0, 0.0) > ERASER_THRESHOLD.powi(2),
        "the middle of the old top edge is inside the turned rectangle, not on it"
    );
    // A point above the turned top edge snaps to the edge that is actually drawn.
    let nearest = turned.nearest_boundary([50.0, -40.0]).unwrap();
    assert!(close(nearest, [50.0, -25.0]), "nearest {nearest:?}");
}

#[test]
fn a_connector_follows_a_turned_shape_and_keeps_its_local_attachment() {
    use std::f32::consts::FRAC_PI_2;
    let mut board = Board::default();
    board.add_shape(shape(ShapeKind::Rect, [0.0, 0.0], [100.0, 100.0]));
    let target = board.shapes[0].id;
    board.add_shape(Shape {
        roundness: None,
        points: Vec::new(),
        kind: ShapeKind::Line,
        color: "#000000".into(),
        width: 2.0,
        from: [50.0, 0.0],
        to: [200.0, 0.0],
        rotate: 0.0,
        id: 0,
        attach: [
            Some(sgraffito::canvas::Attachment {
                shape: target,
                at: [0.0, -1.0],
            }),
            None,
        ],
        fill: None,
    });
    assert_eq!(board.shapes[1].from, [50.0, 0.0]);

    let turned = Transform::Turn {
        pivot: [50.0, 50.0],
        radians: FRAC_PI_2,
    }
    .shape(&board.shapes[0]);
    board.shapes[0] = turned;
    board.sync_connectors();

    let moved = board.shapes[1].from;
    assert!(close(moved, [100.0, 50.0]), "the end followed to {moved:?}");
    let attachment = board.shapes[0].attachment_at(moved);
    assert_eq!(attachment.shape, target);
    assert!(
        close(attachment.at, [0.0, -1.0]),
        "the same local point is kept: {:?}",
        attachment.at
    );
}

#[test]
fn a_turn_turns_a_lines_endpoints_and_leaves_no_angle_on_it() {
    use std::f32::consts::FRAC_PI_2;
    for kind in [ShapeKind::Line, ShapeKind::Arrow] {
        let line = shape(kind, [10.0, 0.0], [20.0, 0.0]);
        let turned = Transform::Turn {
            pivot: [0.0, 0.0],
            radians: FRAC_PI_2,
        }
        .shape(&line);
        assert!(close(turned.from, [0.0, 10.0]), "{kind:?} from");
        assert!(close(turned.to, [0.0, 20.0]), "{kind:?} to");
        assert_eq!(turned.rotate, 0.0, "{kind:?} has no second rotation");
        assert_eq!(turned.turn_angle(), 0.0);
    }
}

#[test]
fn a_turned_shape_widens_the_frame_and_the_rotation_handle_stays_above_it() {
    use std::f32::consts::FRAC_PI_2;
    let mut board = Board {
        strokes: vec![],
        texts: vec![],
        shapes: vec![shape(ShapeKind::Rect, [80.0, 80.0], [180.0, 130.0])],
        notes: vec![],
    };
    let held = [ItemId::Shape(0)];
    let before = selection_frame(&board, &held, None).unwrap();
    board.shapes[0] = Transform::Turn {
        pivot: board.shapes[0].centre(),
        radians: FRAC_PI_2,
    }
    .shape(&board.shapes[0]);
    let after = selection_frame(&board, &held, None).unwrap();

    assert!(after.h > before.h, "the turned shape is taller");
    assert!(after.w < before.w, "the turned shape is narrower");
    let knob = rotate_handle(after, SELECT_ROTATE_GAP);
    assert!((knob[0] - after.centre()[0]).abs() < 1e-4);
    assert_eq!(knob[1], after.y - SELECT_ROTATE_GAP);
    assert_eq!(
        grab_at(after, knob[0], knob[1], SELECT_HANDLE_HIT),
        Some(Grab::Rotate)
    );
}

#[test]
fn deleting_a_selection_removes_exactly_it() {
    // The bug this guards against: removing an earlier element shifts every later one, so a
    // selection holding two adjacent strokes would remove the wrong pair and leave one of them drawn.
    let mut a = Board {
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

use sgraffito::canvas::{ZOOM_MAX, ZOOM_MIN, ZOOM_STEP};

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
        selection_press(&a, &held, view, corner, ERASER_THRESHOLD, false),
        Press::Handle(Grab::Handle(Handle::Nw))
    );
    // …and not where the canvas says it is, which is a different place on screen.
    let canvas_corner = Handle::Nw.point(frame);
    assert_ne!(
        selection_press(&a, &held, view, canvas_corner, ERASER_THRESHOLD, false),
        Press::Handle(Grab::Handle(Handle::Nw)),
        "a handle was grabbed at its canvas position instead of where it is drawn"
    );

    // A press on an element the selection does not hold takes that element, asked in canvas space: the
    // stroke's own point is at a different screen position, and asking there must find it.
    let on_other = screen(stroke_at(400.0, 300.0).points[0]);
    assert_eq!(
        selection_press(&a, &held, view, on_other, ERASER_THRESHOLD, false),
        Press::Element(ItemId::Stroke(1))
    );

    // Empty space outside the frame sweeps a marquee, and the marquee is a **canvas** point: that is
    // what the elements it will catch and the frame it is drawn in are measured in.
    let far = [1200.0, 900.0];
    match selection_press(&a, &held, view, far, ERASER_THRESHOLD, false) {
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
    let mut a = Board {
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
    let a = Board {
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
    let mut a = Board {
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
        a.strokes[0]
            .paint_bounds()
            .unwrap()
            .union(a.notes[0].bounds())
    );
    // The other two kinds are reached the same way, and their indices are left alone.
    let more = a
        .recolour(&[ItemId::Text(0), ItemId::Shape(0)], PALETTE[2])
        .expect("the other two kinds");
    assert_eq!(a.texts[0].color, PALETTE[2]);
    assert_eq!(a.shapes[0].color, PALETTE[2]);
    assert_eq!(
        more,
        a.texts[0].paint_bounds().union(a.shapes[0].paint_bounds())
    );
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
fn a_fill_recolours_only_the_closed_shapes_it_holds() {
    use sgraffito::canvas::{Note, ShapeKind};
    let mut a = Board {
        strokes: vec![line(40.0, 20.0, 60.0)],
        texts: vec![text(20.0, 80.0, "hello")],
        shapes: vec![
            shape(ShapeKind::Rect, [10.0, 120.0], [60.0, 160.0]),
            shape(ShapeKind::Line, [10.0, 200.0], [60.0, 240.0]),
        ],
        notes: vec![Note::new(100.0, 20.0, PALETTE[1], "2026/10/14".into())],
    };
    let held = [
        ItemId::Shape(0),
        ItemId::Shape(1),
        ItemId::Stroke(0),
        ItemId::Text(0),
        ItemId::Note(0),
    ];
    let before = (
        a.strokes[0].color.clone(),
        a.texts[0].color.clone(),
        a.notes[0].color.clone(),
    );
    let box_ = a
        .recolour_fill(&held, Some(PALETTE[2]))
        .expect("a fill was applied");
    assert_eq!(a.shapes[0].fill.as_deref(), Some(PALETTE[2]));
    assert_eq!(a.shapes[1].fill, None, "a line has no inside");
    assert_eq!(
        (
            a.strokes[0].color.clone(),
            a.texts[0].color.clone(),
            a.notes[0].color.clone()
        ),
        before,
        "a fill is not a stroke colour"
    );
    assert_eq!(box_, a.shapes[0].paint_bounds());
    // No fill clears it again, and the box is still the closed shape's.
    let cleared = a.recolour_fill(&held, None).expect("a fill was cleared");
    assert_eq!(a.shapes[0].fill, None);
    assert_eq!(cleared, a.shapes[0].paint_bounds());
    // Nothing held, an id that names no element, or a fill a shape already carries changes nothing at
    // all — an unchanged pick must not record a step that undoes nothing.
    assert_eq!(a.recolour_fill(&[], Some(PALETTE[0])), None);
    assert_eq!(a.recolour_fill(&[ItemId::Shape(9)], Some(PALETTE[0])), None);
    assert_eq!(
        a.recolour_fill(&held, None),
        None,
        "a clear of an empty inside"
    );
}

/// The byte offset of a character index, for slicing a preview by characters the way the board
/// counts them.
fn char_bytes(s: &str, chars: usize) -> usize {
    s.char_indices()
        .nth(chars)
        .map_or(s.len(), |(byte, _)| byte)
}

#[test]
fn a_search_finds_every_word_on_the_board_and_names_the_match() {
    use sgraffito::canvas::Note;
    let mut a = Board {
        texts: vec![
            text(20.0, 80.0, "buy Milk and eggs"),
            text(20.0, 200.0, "nothing here"),
        ],
        notes: vec![{
            let mut note = Note::new(100.0, 20.0, PALETTE[1], "2026/10/14".into());
            note.text = "milk first".into();
            note
        }],
        ..Board::default()
    };
    let hits = a.search("MILK");
    // Texts come before notes, and only the elements that hold the word are listed.
    assert_eq!(hits.len(), 2, "{hits:?}");
    assert_eq!(hits[0].item, ItemId::Text(0));
    assert_eq!(hits[1].item, ItemId::Note(0));
    // `at` and `len` name the matched characters of the preview, case-insensitively.
    let preview = hits[0].preview.clone();
    let (at, len) = (hits[0].at, hits[0].len);
    assert_eq!(len, 4);
    assert_eq!(
        &preview[char_bytes(&preview, at)..char_bytes(&preview, at + len)],
        "Milk"
    );
    // A query that matches nothing, and the empty query, list nothing.
    assert!(a.search("yogurt").is_empty());
    assert!(a.search("").is_empty());
    // The preview stays short: a long line is clipped around the match with an ellipsis.
    a.texts[0].text = format!("{}milk{}", "word ".repeat(40), " tail".repeat(40));
    let hits = a.search("milk");
    assert!(hits[0].preview.chars().count() < 80, "{}", hits[0].preview);
    assert!(hits[0].preview.contains('…'), "{}", hits[0].preview);
    let preview = hits[0].preview.clone();
    let (at, len) = (hits[0].at, hits[0].len);
    assert_eq!(
        &preview[char_bytes(&preview, at)..char_bytes(&preview, at + len)],
        "milk"
    );
}

#[test]
fn a_copy_leaves_the_original_in_place_and_keeps_every_index() {
    use sgraffito::canvas::{Note, ShapeKind};
    let mut a = Board {
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
            .paint_bounds()
            .unwrap()
            .union(a.shapes[1].paint_bounds())
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
    let a = Board {
        strokes: vec![line(10.0, 0.0, 5.0), line(20.0, 0.0, 5.0)],
        texts: vec![text(0.0, 0.0, "one")],
        shapes: vec![sgraffito::canvas::Shape {
            kind: sgraffito::canvas::ShapeKind::Rect,
            color: "#e01b24".into(),
            width: 3.0,
            from: [0.0, 0.0],
            to: [10.0, 10.0],
            points: Vec::new(),
            roundness: None,
            rotate: 0.0,
            id: 0,
            attach: [None, None],
            fill: None,
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
    let a = Board {
        strokes: vec![line(10.0, 0.0, 5.0), line(20.0, 0.0, 5.0)],
        texts: vec![],
        shapes: vec![Shape {
            kind: ShapeKind::Line,
            color: "#e01b24".into(),
            width: 3.0,
            from: [0.0, 0.0],
            to: [10.0, 10.0],
            points: Vec::new(),
            roundness: None,
            rotate: 0.0,
            id: 0,
            attach: [None, None],
            fill: None,
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
    let empty = Board::default();
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

#[test]
fn the_selection_frame_is_the_stored_geometry_and_not_the_ink() {
    // The hand-drawn style reaches outside an element's geometry, but the frame is what the handles
    // hang on and what a marquee measures, so it stays the stored box: a frame that grew with the
    // wander would put its handles where a resize is not.
    let stroke = Stroke {
        color: "#e01b24".into(),
        width: 4.0,
        points: vec![[10.0, 20.0], [110.0, 20.0]],
        attach: [None, None],
    };
    let a = Board {
        strokes: vec![stroke.clone()],
        texts: vec![],
        shapes: vec![],
        notes: vec![],
    };
    let exact = Rect {
        x: 7.0,
        y: 17.0,
        w: 106.0,
        h: 6.0,
    };
    assert_eq!(selection_frame(&a, &[ItemId::Stroke(0)], None), Some(exact));
    // A marquee takes an element by that same box, so a mark whose ink overhangs the marquee is
    // still caught by the geometry the user drew.
    assert_eq!(marquee_select(&a, exact), vec![ItemId::Stroke(0)]);
    // The box a damaged region is cut from is the bigger one.
    assert_eq!(
        stroke.paint_bounds(),
        Some(exact.grown(SKETCH_SLACK)),
        "the paint box has to hold the wander"
    );
}

#[test]
fn a_dot_is_not_painted_beyond_its_own_disc() {
    // A one-sample stroke is never sketched, so its paint box is its bounds.
    let dot = Stroke {
        color: "#ffffff".into(),
        width: 4.0,
        points: vec![[5.0, 5.0]],
        attach: [None, None],
    };
    assert_eq!(dot.paint_bounds(), dot.bounds());
}

#[test]
fn a_zoom_eases_to_its_target_about_its_anchor() {
    use sgraffito::canvas::{ZOOM_SECONDS, Zoom};

    let mut view = View {
        zoom: 1.0,
        offset: [30.0, -10.0],
    };
    let anchor = [640.0, 360.0];
    let canvas = view.to_canvas(anchor);
    let mut zoom = Zoom::request(
        view,
        view.zoom,
        canvas,
        anchor,
        ZOOM_STEP * ZOOM_STEP * ZOOM_STEP,
    );
    let target = zoom.target;
    assert!(target > view.zoom, "the fixture does not zoom in");

    // A frame's worth of transition at a time, which is what the daemon's tick does.
    let mut seen = vec![view.zoom];
    let mut running = true;
    let mut frames = 0;
    while running && frames < 200 {
        running = zoom.step(&mut view, 0.016);
        seen.push(view.zoom);
        frames += 1;
        let under = view.to_screen(canvas);
        assert!(
            (under[0] - anchor[0]).abs() < 0.01 && (under[1] - anchor[1]).abs() < 0.01,
            "the anchored point moved to {under:?}"
        );
    }
    assert!(!running, "the transition never landed");
    assert_eq!(view.zoom, target, "the zoom stopped short of its target");
    assert_eq!(
        frames,
        ((ZOOM_SECONDS / 0.016).ceil()) as i32,
        "it took another time than the limit"
    );
    // Monotone: an eased zoom never goes back and forth on its way to the target.
    for pair in seen.windows(2) {
        assert!(
            pair[1] >= pair[0],
            "the zoom went backwards: {} then {}",
            pair[0],
            pair[1]
        );
    }
}

#[test]
fn a_burst_of_zoom_requests_lands_on_its_last_target() {
    use sgraffito::canvas::Zoom;

    let mut view = View::default();
    let anchor = [400.0, 300.0];
    let canvas = view.to_canvas(anchor);
    // Three notches in one burst: each request is measured from where the zoom is heading, so they
    // compound, and the transition starts from where the view visibly is.
    let mut zoom = None;
    for _ in 0..3 {
        let base = zoom.map_or(view.zoom, |z: Zoom| z.target);
        let mut request = Zoom::request(view, base, canvas, anchor, ZOOM_STEP);
        request.step(&mut view, 0.016);
        zoom = Some(request);
    }
    let mut zoom = zoom.expect("a burst left no transition");
    let mut running = true;
    while running {
        running = zoom.step(&mut view, 0.016);
    }
    let expected = ZOOM_STEP * ZOOM_STEP * ZOOM_STEP;
    assert!(
        (view.zoom - expected).abs() < 0.0001,
        "the burst landed on {} instead of {expected}",
        view.zoom
    );
}

#[test]
fn the_rung_follows_the_speed_of_the_pointer_and_settles_back() {
    use sgraffito::canvas::{MOTION_QUALITY, Motion, SETTLE_SPEED};

    // A fast drag: a hundred logical pixels every frame is well past the settle speed.
    let mut fast = Motion::default();
    for _ in 0..6 {
        fast.moved(100.0, 0.016);
    }
    assert!(
        fast.speed() > SETTLE_SPEED,
        "a fast drag measured {} pt/s",
        fast.speed()
    );
    assert_eq!(fast.quality(), MOTION_QUALITY);

    // A slow adjustment: two pixels a frame never gets there.
    let mut slow = Motion::default();
    for _ in 0..6 {
        slow.moved(2.0, 0.016);
    }
    assert_eq!(slow.quality(), 1.0, "a slow adjustment went coarse");

    // The pointer stopped: the speed decays and the drawing is crisp again without another event.
    let mut frames = 0;
    while fast.quality() < 1.0 && frames < 100 {
        fast.idle(0.016);
        frames += 1;
    }
    assert_eq!(fast.quality(), 1.0, "the rung never came back");
    assert!(
        frames * 16 <= 200,
        "it took {frames} frames to come back, which is not a settle"
    );
}

/// A shift press toggles membership and never replaces the selection, and it keeps the order the
/// other elements were in.
#[test]
fn toggling_an_element_in_and_out_returns_the_list() {
    let held = vec![ItemId::Stroke(0), ItemId::Text(1)];
    let added = toggle_selection(&held, ItemId::Stroke(2));
    assert_eq!(
        added,
        vec![ItemId::Stroke(0), ItemId::Text(1), ItemId::Stroke(2)]
    );
    assert_eq!(toggle_selection(&added, ItemId::Stroke(2)), held);
    assert_eq!(
        toggle_selection(&held, ItemId::Stroke(0)),
        vec![ItemId::Text(1)]
    );
    assert_eq!(
        toggle_selection(&[ItemId::Text(1)], ItemId::Text(1)),
        vec![]
    );
}

/// With `Shift` held, an element under the pointer is a toggle, empty space is nothing, and a handle is
/// still a handle — the order the press is decided in is what keeps a proportional resize reachable.
#[test]
fn a_shift_press_toggles_and_a_handle_still_wins() {
    // The held stroke is long enough that a press on it can be away from every handle of its frame.
    let long = Stroke {
        color: "#e01b24".into(),
        width: 2.0,
        points: vec![[0.0, 0.0], [400.0, 0.0]],
        attach: [None, None],
    };
    let a = ann(vec![long, stroke_at(400.0, 300.0)], vec![]);
    let held = vec![ItemId::Stroke(0)];
    let view = View::default();
    let on_held = view.to_screen([100.0, 0.0]);
    let on_other = view.to_screen([400.0, 300.0]);
    assert_eq!(
        selection_press(&a, &held, view, on_other, ERASER_THRESHOLD, true),
        Press::Toggle(ItemId::Stroke(1))
    );
    assert_eq!(
        selection_press(&a, &held, view, on_held, ERASER_THRESHOLD, true),
        Press::Toggle(ItemId::Stroke(0)),
        "a held element toggles out instead of dragging the selection"
    );
    assert_eq!(
        selection_press(&a, &held, view, [1200.0, 900.0], ERASER_THRESHOLD, true),
        Press::Inert,
        "a shift press on empty space started something"
    );
    let frame = selection_frame(&a, &held, None).unwrap();
    assert_eq!(
        selection_press(
            &a,
            &held,
            view,
            view.to_screen(Handle::Nw.point(frame)),
            ERASER_THRESHOLD,
            true
        ),
        Press::Handle(Grab::Handle(Handle::Nw)),
        "shift took the handle's press"
    );
}

/// Inside the frame but on no element, a shift press changes nothing where a plain press drags the whole
/// selection; `Shift` must not turn the frame's interior into a drag.
#[test]
fn a_shift_press_inside_the_frame_starts_nothing() {
    let long = |y: f32| Stroke {
        color: "#e01b24".into(),
        width: 2.0,
        points: vec![[0.0, y], [400.0, y]],
        attach: [None, None],
    };
    let a = ann(vec![long(0.0), long(200.0)], vec![]);
    let held = vec![ItemId::Stroke(0), ItemId::Stroke(1)];
    let view = View::default();
    let inside = view.to_screen([200.0, 100.0]);
    assert_eq!(
        selection_press(&a, &held, view, inside, ERASER_THRESHOLD, true),
        Press::Inert,
        "a shift press inside the frame started something"
    );
    assert_eq!(
        selection_press(&a, &held, view, inside, ERASER_THRESHOLD, false),
        Press::Move,
        "a plain press inside the frame no longer drags the selection"
    );
}

/// The drawing snap also follows freehand polylines and, as a last resort, the grid. Content wins
/// over a grid intersection that is closer, and a closed shape still carries its attachment.
#[test]
fn drawing_snaps_to_polylines_grid_and_closed_shapes() {
    use sgraffito::canvas::{GRID_SPACING, GridSnap, ShapeKind};
    let grid = GridSnap {
        step: GRID_SPACING,
        offset: [0.0, 0.0],
    };

    let mut board = Board {
        strokes: vec![Stroke {
            color: "#e01b24".into(),
            width: 3.0,
            points: vec![[0.0, 0.0], [100.0, 0.0]],
            attach: [None, None],
        }],
        ..Default::default()
    };
    let on_polyline = board
        .snap_drawing([50.0, 5.0], 12.0, Some(grid))
        .expect("the freehand polyline did not snap");
    assert_eq!(on_polyline.point, [50.0, 0.0]);
    assert_eq!(on_polyline.attachment, None);

    // A three-sample stroke is drawn as a quadratic through its next midpoint; the snap follows
    // that curve rather than the straight chord, so it lands above y=0.
    let curved = Board {
        strokes: vec![Stroke {
            color: "#e01b24".into(),
            width: 3.0,
            points: vec![[0.0, 0.0], [100.0, 0.0], [100.0, 100.0]],
            attach: [None, None],
        }],
        ..Default::default()
    }
    .snap_drawing([50.0, 5.0], 12.0, None)
    .expect("the curved stroke did not snap");
    assert!(curved.point[1] > 1.0 && curved.point[1] < 8.0);

    let on_grid = Board::default()
        .snap_drawing([25.0, 23.0], 12.0, Some(grid))
        .expect("the grid did not snap");
    assert_eq!(on_grid.point, [24.0, 24.0]);
    assert_eq!(on_grid.attachment, None);
    assert!(
        Board::default()
            .snap_drawing([25.0, 23.0], 12.0, None)
            .is_none(),
        "an empty board without a grid snapped to nothing"
    );
    let dots = GridSnap {
        step: GRID_SPACING,
        offset: [GRID_SPACING / 2.0; 2],
    };
    let on_dot = Board::default()
        .snap_drawing([13.0, 11.0], 12.0, Some(dots))
        .expect("the dot lattice did not snap");
    assert_eq!(on_dot.point, [12.0, 12.0]);

    board.add_shape(shape(ShapeKind::Rect, [200.0, 0.0], [300.0, 100.0]));
    let on_shape = board
        .snap_drawing([195.0, 5.0], 12.0, Some(grid))
        .expect("the shape did not snap");
    assert!(on_shape.attachment.is_some());
    assert_eq!(on_shape.point, [200.0, 5.0]);
}

#[test]
fn a_connector_snaps_to_a_shape_and_follows_both_endpoints() {
    use sgraffito::canvas::{Shape, ShapeKind};
    let mut board = Board::default();
    board.add_shape(shape(ShapeKind::Rect, [0.0, 0.0], [100.0, 100.0]));
    board.add_shape(shape(ShapeKind::Circle, [300.0, 0.0], [400.0, 100.0]));
    let (left, _) = board.snap([-5.0, 50.0], 12.0).expect("no snap to the rect");
    let (right, _) = board
        .snap([405.0, 50.0], 12.0)
        .expect("no snap to the circle");
    assert_eq!(left.at, [-1.0, 0.0]);
    assert_eq!(right.at, [1.0, 0.0]);

    let connector = Shape {
        roundness: None,
        points: Vec::new(),
        kind: ShapeKind::Line,
        color: "#000000".into(),
        width: 2.0,
        from: [0.0, 50.0],
        to: [400.0, 50.0],
        rotate: 0.0,
        id: 0,
        attach: [Some(left), Some(right)],
        fill: None,
    };
    board.add_shape(connector);
    let connector_id = board.shapes[2].id;

    // Move the right-hand shape: only the attached end moves with it, both ends stay on their
    // outlines, and the free geometry of the line is unchanged.
    board.shapes[1].from = [380.0, 100.0];
    board.shapes[1].to = [500.0, 200.0];
    board.sync_connectors();
    assert_eq!(board.shapes[2].from, [0.0, 50.0]);
    assert_eq!(board.shapes[2].to, [500.0, 150.0]);
    assert_eq!(
        board.shapes[2].attach,
        [Some(left), Some(right)],
        "the connector lost its attachment"
    );
    assert_eq!(board.shapes[2].id, connector_id);

    // A preview computes the same geometry without touching the document.
    let preview = board.connector_preview(
        &[ItemId::Shape(1)],
        sgraffito::canvas::Transform::Translate { dx: 10.0, dy: 0.0 },
        &[],
    );
    let live = preview
        .iter()
        .find_map(|connector| match connector {
            sgraffito::canvas::LiveConnector::Shape(id, shape) if *id == ItemId::Shape(2) => {
                Some(shape)
            }
            _ => None,
        })
        .expect("the connector is not in the preview");
    assert_eq!(live.to, [510.0, 150.0]);
}

#[test]
fn a_shape_id_is_unique_and_connectors_are_remapped_on_a_copy() {
    use sgraffito::canvas::{Shape, ShapeKind};
    let mut board = Board::default();
    board.add_shape(shape(ShapeKind::Rect, [0.0, 0.0], [100.0, 100.0]));
    let target = board.shapes[0].id;
    board.add_shape(Shape {
        roundness: None,
        points: Vec::new(),
        kind: ShapeKind::Line,
        color: "#000000".into(),
        width: 2.0,
        from: [0.0, 50.0],
        to: [100.0, 50.0],
        rotate: 0.0,
        id: 0,
        attach: [
            Some(sgraffito::canvas::Attachment {
                shape: target,
                at: [-1.0, 0.0],
            }),
            None,
        ],
        fill: None,
    });
    let items = [ItemId::Shape(0), ItemId::Shape(1)];
    let (copies, _) = board.duplicate(&items);
    let copied_shape = match copies[0] {
        ItemId::Shape(index) => board.shapes[index].id,
        _ => unreachable!(),
    };
    assert_ne!(copied_shape, target, "the copy reused the original id");
    let copied_line = match copies[1] {
        ItemId::Shape(index) => &board.shapes[index],
        _ => unreachable!(),
    };
    assert_eq!(
        copied_line.attach[0].map(|attachment| attachment.shape),
        Some(copied_shape),
        "the copied connector points at the original shape"
    );

    // Loading a legacy file gives every zero-id shape a unique id.
    let mut legacy = Board::default();
    legacy
        .shapes
        .push(shape(ShapeKind::Rect, [0.0, 0.0], [10.0, 10.0]));
    legacy
        .shapes
        .push(shape(ShapeKind::Circle, [20.0, 0.0], [30.0, 10.0]));
    legacy.ensure_shape_ids();
    assert!(legacy.shapes.iter().all(|shape| shape.id != 0));
    assert_ne!(legacy.shapes[0].id, legacy.shapes[1].id);
}

#[test]
fn a_moved_shape_snaps_to_a_free_connector_end() {
    use sgraffito::canvas::{ReverseSnap, Shape, ShapeKind, Transform};
    let mut board = Board::default();
    board.add_shape(shape(ShapeKind::Rect, [0.0, 0.0], [100.0, 100.0]));
    let shape_id = board.shapes[0].id;
    board.add_shape(Shape {
        roundness: None,
        points: Vec::new(),
        kind: ShapeKind::Line,
        color: "#000000".into(),
        width: 2.0,
        from: [100.0, 50.0],
        to: [300.0, 50.0],
        rotate: 0.0,
        id: 0,
        attach: [None, None],
        fill: None,
    });
    // The free end sits on the rect's right edge. A 10 px drag puts the end just inside the
    // outline, within the magnetic radius: the shape is pulled back onto the end and the link is
    // recorded.
    let snap: ReverseSnap = board
        .reverse_snap(
            &[ItemId::Shape(0)],
            Transform::Translate { dx: 10.0, dy: 0.0 },
            12.0,
        )
        .expect("the free end did not attract the shape");
    assert_eq!(snap.connector, ItemId::Shape(1));
    assert_eq!(snap.end, 0);
    assert_eq!(snap.attachment.shape, shape_id);
    assert_eq!(snap.attachment.at, [1.0, 0.0]);
    assert_eq!(snap.transform, Transform::Translate { dx: 0.0, dy: 0.0 });

    // Applying that link is what the release does: the connector end stays exactly on the edge.
    board.shapes[0] = snap.transform.shape(&board.shapes[0]);
    board.shapes[1].attach[snap.end] = Some(snap.attachment);
    board.sync_connectors();
    assert_eq!(board.shapes[1].from, [100.0, 50.0]);
}

#[test]
fn one_shot_drawing_tools_hand_the_pointer_back_to_select() {
    use sgraffito::canvas::Tool;
    for tool in [
        Tool::Pen,
        Tool::Text,
        Tool::Circle,
        Tool::Rect,
        Tool::Diamond,
        Tool::Line,
        Tool::Arrow,
    ] {
        assert!(
            tool.hands_to_select(),
            "{tool:?} should hand back to select"
        );
    }
    for tool in [Tool::Eraser, Tool::Selection, Tool::Hand, Tool::Note] {
        assert!(!tool.hands_to_select(), "{tool:?} should keep its tool");
    }
}

// --- a closed shape is picked on its own ------------------------------------------------

#[test]
fn a_press_on_a_closed_shape_picks_that_shape_and_nothing_it_encloses() {
    // What a shape holds is not a group: a press on the outline is a press on the outline. Otherwise a
    // boundary drawn around a branch would drag the whole branch whenever the branch was moved, and a
    // resize of one rectangle would resize everything drawn inside it.
    let mut board = Board::default();
    board.add_shape(shape(ShapeKind::Rect, [0.0, 0.0], [300.0, 300.0]));
    board.add_shape(shape(ShapeKind::Rect, [100.0, 100.0], [200.0, 200.0]));
    for at in [[0.0, 150.0], [150.0, 0.0], [300.0, 150.0], [150.0, 300.0]] {
        let press = selection_press(&board, &[], View::default(), at, ERASER_THRESHOLD, false);
        assert_eq!(
            press,
            Press::Element(ItemId::Shape(0)),
            "a press at {at:?} picked more than the shape"
        );
    }
    // A press on the inner shape picks the inner shape, and only it.
    let press = selection_press(
        &board,
        &[],
        View::default(),
        [100.0, 150.0],
        ERASER_THRESHOLD,
        false,
    );
    assert_eq!(press, Press::Element(ItemId::Shape(1)));
    // A marquee still takes everything it wholly contains: that is what a group is drawn with.
    let caught = marquee_select(
        &board,
        Rect {
            x: -10.0,
            y: -10.0,
            w: 320.0,
            h: 320.0,
        },
    );
    assert_eq!(caught.len(), 2, "{caught:?}");
}

#[test]
fn an_empty_press_inside_a_closed_shape_picks_that_shape() {
    let mut board = Board::default();
    board.add_shape(shape(ShapeKind::Rect, [0.0, 0.0], [200.0, 200.0]));
    let press = selection_press(
        &board,
        &[],
        View::default(),
        [100.0, 100.0],
        ERASER_THRESHOLD,
        false,
    );
    assert_eq!(press, Press::Element(ItemId::Shape(0)));
}

#[test]
fn the_text_frame_moves_with_a_live_translation() {
    use sgraffito::canvas::selection_frame_with;
    let mut board = Board::default();
    board.texts.push(text(10.0, 20.0, "hi"));
    let measured = [(
        0usize,
        Rect {
            x: 8.0,
            y: 18.0,
            w: 30.0,
            h: 20.0,
        },
    )];
    let frame = selection_frame_with(
        &board,
        &[ItemId::Text(0)],
        Some(Transform::Translate { dx: 40.0, dy: 50.0 }),
        &measured,
    )
    .expect("the text has a frame");
    assert_eq!(
        frame,
        Rect {
            x: 48.0,
            y: 68.0,
            w: 30.0,
            h: 20.0
        },
        "the measured frame stayed at the document origin"
    );
}

/// The bucket's hit test is the shape's inside, not its outline: a circle misses a corner of its
/// box, a diamond misses the same corner too, while a rectangle keeps it.
#[test]
fn a_closed_shape_contains_its_interior() {
    let circle = shape(ShapeKind::Circle, [100.0, 100.0], [200.0, 200.0]);
    assert!(circle.contains([150.0, 150.0]));
    assert!(!circle.contains([102.0, 102.0]));
    let rect = shape(ShapeKind::Rect, [100.0, 100.0], [200.0, 200.0]);
    assert!(rect.contains([102.0, 102.0]));
    let diamond = shape(ShapeKind::Diamond, [100.0, 100.0], [200.0, 200.0]);
    assert!(diamond.contains([150.0, 150.0]));
    assert!(!diamond.contains([110.0, 110.0]));
    let line = shape(ShapeKind::Line, [100.0, 100.0], [200.0, 200.0]);
    assert!(!line.contains([150.0, 150.0]));
}

// --- affine matrices and boundary bindings ---------------------------------------------------

#[test]
fn an_affine_matrix_maps_points_composes_and_inverts() {
    use std::f32::consts::FRAC_PI_2;

    use sgraffito::canvas::Affine;

    let p = [3.0, 4.0];
    assert!(Affine::IDENTITY.is_identity());
    assert_eq!(Affine::IDENTITY.apply(p), p);
    assert_eq!(Affine::IDENTITY.determinant(), 1.0);
    assert!(Affine::IDENTITY.inverse().unwrap().is_identity());

    let moved = Affine::translation(10.0, -5.0);
    assert!(!moved.is_identity());
    assert_eq!(moved.apply(p), [13.0, -1.0]);
    assert_eq!(moved.determinant(), 1.0);
    assert!(close(moved.inverse().unwrap().apply(moved.apply(p)), p));

    let scaled = Affine::scaling([1.0, 2.0], 2.0, 4.0);
    assert_eq!(scaled.apply([1.0, 2.0]), [1.0, 2.0]);
    assert_eq!(scaled.apply([2.0, 3.0]), [3.0, 6.0]);
    assert_eq!(scaled.determinant(), 8.0);
    assert!(close(scaled.inverse().unwrap().apply(scaled.apply(p)), p));

    let turned = Affine::rotation([0.0, 0.0], FRAC_PI_2);
    assert!(close(turned.apply([2.0, 0.0]), [0.0, 2.0]));
    assert_eq!(turned.determinant(), 1.0);

    // Collapsing the plane has no inverse.
    assert!(Affine::scaling([0.0, 0.0], 0.0, 1.0).inverse().is_none());
    assert!(Affine::scaling([0.0, 0.0], 0.0, 1.0).determinant().abs() < f32::EPSILON);
}

#[test]
fn composition_order_is_explicit_and_deterministic() {
    use std::f32::consts::FRAC_PI_2;

    use sgraffito::canvas::{Affine, Transform};

    let p = [3.0, 4.0];
    let turn = Affine::rotation([0.0, 0.0], FRAC_PI_2);
    let scale = Affine::scaling([0.0, 0.0], 2.0, 3.0);

    // `then` reads left to right: the receiver runs first.
    let turn_then_scale = turn.then(scale);
    assert!(close(turn_then_scale.apply(p), scale.apply(turn.apply(p))));
    // The matrix product reads right to left, so `scale * turn` is the same map.
    assert!(close((scale * turn).apply(p), turn_then_scale.apply(p)));
    assert!(close((turn * scale).apply(p), scale.then(turn).apply(p)));
    // The two orders are genuinely different maps.
    assert!(!close(turn_then_scale.apply(p), scale.then(turn).apply(p)));
    // The composed map has the product determinant and inverts as a whole.
    assert!((turn_then_scale.determinant() - 6.0).abs() < 1e-4);
    let back = turn_then_scale.inverse().unwrap();
    assert!(close(back.apply(turn_then_scale.apply(p)), p));

    // The enum's `then` follows the same order as its matrices.
    let moved = Transform::Translate { dx: 10.0, dy: -2.0 };
    let turned = Transform::Turn {
        pivot: [1.0, 2.0],
        radians: FRAC_PI_2,
    };
    let composed = moved.then(turned);
    assert!(close(composed.apply(p), turned.point(moved.point(p))));
    assert!(close(
        composed.apply(p),
        (turned.matrix() * moved.matrix()).apply(p)
    ));
    assert!(close(
        composed.apply(p),
        moved.matrix().then(turned.matrix()).apply(p)
    ));
}

#[test]
fn a_rotated_binding_stays_on_the_outline_it_draws() {
    use std::f32::consts::FRAC_PI_3;

    use sgraffito::canvas::{Attachment, Transform};

    for kind in [ShapeKind::Circle, ShapeKind::Rect, ShapeKind::Diamond] {
        let mut base = shape(kind, [40.0, -10.0], [240.0, 110.0]);
        base.id = 9;
        let turned = Transform::Turn {
            pivot: [10.0, 20.0],
            radians: FRAC_PI_3,
        }
        .shape(&base);
        for at in [[1.0, 1.0], [-1.0, 0.4], [0.3, -1.0]] {
            let point = turned.attachment_point(Attachment {
                shape: turned.id,
                at,
            });
            let on_outline = turned.nearest_boundary(point).expect("no outline");
            assert!(
                close(point, on_outline),
                "{kind:?} at {at:?} landed at {point:?}, nearest outline point {on_outline:?}"
            );
            // Binding the world point back and mapping it again is a fixed point.
            let again = turned.attachment_point(turned.attachment_at(point));
            assert!(
                close(point, again),
                "{kind:?} at {at:?} drifted: {point:?} -> {again:?}"
            );
        }
    }
}

#[test]
fn a_box_corner_binding_is_pulled_onto_a_circle_or_diamond() {
    use sgraffito::canvas::Attachment;

    let circle = shape(ShapeKind::Circle, [0.0, 0.0], [100.0, 100.0]);
    let corner = circle.attachment_point(Attachment {
        shape: circle.id,
        at: [1.0, 1.0],
    });
    let radius = (corner[0] - 50.0).hypot(corner[1] - 50.0);
    assert!((radius - 50.0).abs() < 1e-3, "circle corner at {corner:?}");
    assert!(close(circle.nearest_boundary(corner).unwrap(), corner));

    let diamond = shape(ShapeKind::Diamond, [0.0, 0.0], [100.0, 100.0]);
    let corner = diamond.attachment_point(Attachment {
        shape: diamond.id,
        at: [1.0, 1.0],
    });
    assert!(close(corner, [75.0, 75.0]), "diamond corner at {corner:?}");
    assert!(close(diamond.nearest_boundary(corner).unwrap(), corner));
}

#[test]
fn a_connector_end_can_route_toward_another_point() {
    use std::f32::consts::FRAC_PI_2;

    use sgraffito::canvas::Transform;

    let circle = shape(ShapeKind::Circle, [0.0, 0.0], [100.0, 100.0]);
    let right = circle.attachment_toward([500.0, 50.0]).unwrap();
    assert!(close(right.at, [1.0, 0.0]), "aimed at {:?}", right.at);
    assert!(close(circle.attachment_point(right), [100.0, 50.0]));

    let down = circle.attachment_toward([50.0, 500.0]).unwrap();
    assert!(close(circle.attachment_point(down), [50.0, 100.0]));

    let turned = Transform::Turn {
        pivot: [50.0, 50.0],
        radians: FRAC_PI_2,
    }
    .shape(&circle);
    let bound = turned.attachment_toward([50.0, 500.0]).unwrap();
    // The aim is a world point, so the route still lands on the world-down side; only the stored
    // local coordinates turn with the shape.
    assert!(
        close(turned.attachment_point(bound), [50.0, 100.0]),
        "the route did not follow the world aim"
    );

    let line = shape(ShapeKind::Line, [0.0, 0.0], [10.0, 10.0]);
    assert!(line.attachment_toward([1.0, 1.0]).is_none());
}

#[test]
fn a_collapsed_box_keeps_its_binding_on_the_segment_it_draws() {
    use sgraffito::canvas::Transform;

    // Zero height: the outline is the segment from (0, 50) to (100, 50).
    let flat = shape(ShapeKind::Rect, [0.0, 50.0], [100.0, 50.0]);
    let nearest = flat.nearest_boundary([40.0, 90.0]).unwrap();
    assert_eq!(nearest, [40.0, 50.0]);
    let attachment = flat.attachment_at(nearest);
    assert_eq!(attachment.at, [-0.2, 0.0]);
    assert!(close(flat.attachment_point(attachment), nearest));

    // Collapsed to a point: every route reports that point.
    let dot = shape(ShapeKind::Circle, [10.0, 10.0], [10.0, 10.0]);
    assert_eq!(dot.nearest_boundary([99.0, 99.0]).unwrap(), [10.0, 10.0]);
    let attachment = dot.attachment_at([10.0, 10.0]);
    assert_eq!(attachment.at, [0.0, 0.0]);
    assert_eq!(dot.attachment_point(attachment), [10.0, 10.0]);

    // A turned flat rectangle still lands on the segment it draws.
    let turned = Transform::Turn {
        pivot: [50.0, 50.0],
        radians: std::f32::consts::FRAC_PI_2,
    }
    .shape(&flat);
    assert!(close(
        turned.nearest_boundary([100.0, 60.0]).unwrap(),
        [50.0, 60.0]
    ));
    let attachment = turned.attachment_at([50.0, 60.0]);
    assert!(close(turned.attachment_point(attachment), [50.0, 60.0]));
}

#[test]
fn an_attached_connector_stays_on_the_outline_through_moves_scales_turns_and_copies() {
    use sgraffito::canvas::{Attachment, Shape, Transform};

    let mut board = Board::default();
    board.add_shape(shape(ShapeKind::Circle, [0.0, 0.0], [100.0, 100.0]));
    let target = board.shapes[0].id;
    board.add_shape(Shape {
        roundness: None,
        points: Vec::new(),
        kind: ShapeKind::Line,
        color: "#000000".into(),
        width: 2.0,
        from: [50.0, 50.0],
        to: [300.0, 50.0],
        rotate: 0.0,
        id: 0,
        attach: [
            Some(Attachment {
                shape: target,
                at: [1.0, -1.0],
            }),
            None,
        ],
        fill: None,
    });

    let on_outline = |board: &Board| {
        let (circle, line) = (&board.shapes[0], &board.shapes[1]);
        let nearest = circle.nearest_boundary(line.from).expect("no outline");
        assert!(
            close(line.from, nearest),
            "the connector end {:?} left the outline (nearest {nearest:?})",
            line.from
        );
        let (centre, half) = circle.box_geometry();
        assert!(
            (half[0] - half[1]).abs() < 1e-3,
            "the fixture stays a circle: {half:?}"
        );
        let (dx, dy) = (line.from[0] - centre[0], line.from[1] - centre[1]);
        assert!(
            (dx.hypot(dy) - half[0]).abs() < 1e-2,
            "the end is not on the circle's rim"
        );
    };

    board.sync_connectors();
    on_outline(&board);
    assert_eq!(
        board.shapes[1].attach[0].unwrap().at,
        [1.0, -1.0],
        "sync keeps the local direction"
    );

    for transform in [
        Transform::Translate {
            dx: 40.0,
            dy: -15.0,
        },
        Transform::Turn {
            pivot: [0.0, 0.0],
            radians: 0.7,
        },
        Transform::Scale {
            anchor: [20.0, 30.0],
            sx: 1.7,
            sy: 1.7,
        },
    ] {
        board.shapes[0] = transform.shape(&board.shapes[0]);
        board.sync_connectors();
        on_outline(&board);
    }

    // A copy gets a fresh shape id, a re-pointed attachment and the same rim binding.
    let (copies, _) = board.duplicate(&[ItemId::Shape(0), ItemId::Shape(1)]);
    board.sync_connectors();
    let copied_circle = match copies[0] {
        ItemId::Shape(index) => &board.shapes[index],
        _ => unreachable!(),
    };
    let copied_line = match copies[1] {
        ItemId::Shape(index) => &board.shapes[index],
        _ => unreachable!(),
    };
    assert_eq!(
        copied_line.attach[0].map(|attachment| attachment.shape),
        Some(copied_circle.id),
        "the copied connector still points at the original circle"
    );
    let nearest = copied_circle.nearest_boundary(copied_line.from).unwrap();
    assert!(close(copied_line.from, nearest));
}

#[test]
fn a_reverse_snap_to_a_circle_puts_the_joint_on_the_rim() {
    use sgraffito::canvas::{Attachment, Shape, Transform};

    let mut board = Board::default();
    board.add_shape(shape(ShapeKind::Circle, [0.0, 0.0], [100.0, 100.0]));
    let target = board.shapes[0].id;
    board.add_shape(Shape {
        roundness: None,
        points: Vec::new(),
        kind: ShapeKind::Line,
        color: "#000000".into(),
        width: 2.0,
        from: [100.0, 25.0],
        to: [300.0, 25.0],
        rotate: 0.0,
        id: 0,
        attach: [None, None],
        fill: None,
    });

    let snap = board
        .reverse_snap(
            &[ItemId::Shape(0)],
            Transform::Translate { dx: 2.0, dy: 0.0 },
            12.0,
        )
        .expect("the free end did not attract the circle");
    assert_eq!(snap.connector, ItemId::Shape(1));
    assert_eq!(snap.end, 0);
    assert_eq!(snap.attachment.shape, target);

    // What the release does: move the shape, write the link, then seat both ends.
    board.shapes[0] = snap.transform.shape(&board.shapes[0]);
    board.shapes[1].attach[snap.end] = Some(Attachment {
        shape: snap.attachment.shape,
        at: snap.attachment.at,
    });
    board.sync_connectors();
    let end = board.shapes[1].from;
    let nearest = board.shapes[0].nearest_boundary(end).unwrap();
    assert!(close(end, nearest), "the joint is not on the rim: {end:?}");
}

#[test]
fn a_bent_line_is_hit_and_bounded_through_its_interior_vertex() {
    let mut bent = shape(ShapeKind::Line, [0.0, 0.0], [100.0, 0.0]);
    bent.points.push([50.0, 60.0]);
    assert!(!bent.is_round());
    bent.roundness = Some(2);
    assert!(bent.is_round());

    assert_eq!(bent.linear_vertex_count(), 3);
    assert_eq!(bent.linear_vertex(1), Some([50.0, 60.0]));
    assert!(bent.distance_squared(50.0, 60.0) < 1e-4);
    assert!(
        bent.paint_bounds().contains(50.0, 60.0),
        "the bend escaped the shape's paint bounds: {:?}",
        bent.paint_bounds()
    );
    assert!(
        !bent.is_empty(),
        "a line with a real bend was treated as an empty drag"
    );

    let plain = shape(ShapeKind::Line, [0.0, 0.0], [0.0, 0.0]);
    assert!(plain.is_empty());
}

#[test]
fn a_midpoint_press_inserts_a_persistent_vertex() {
    let mut line = shape(ShapeKind::Line, [0.0, 0.0], [100.0, 0.0]);
    assert_eq!(line.linear_midpoints(), vec![[50.0, 0.0]]);

    let vertex = line
        .insert_linear_vertex(0, line.linear_midpoints()[0])
        .expect("the midpoint did not insert");
    assert_eq!(vertex, 1);
    assert_eq!(line.points, vec![[50.0, 0.0]]);
    assert_eq!(line.linear_vertex_count(), 3);

    assert!(line.set_linear_vertex(vertex, [50.0, 30.0]));
    assert_eq!(line.linear_vertex(vertex), Some([50.0, 30.0]));
    assert_eq!(line.points, vec![[50.0, 30.0]]);
    assert_eq!(line.from, [0.0, 0.0]);
    assert_eq!(line.to, [100.0, 0.0]);
}

#[test]
fn a_bound_label_is_removed_with_its_container_and_copied_with_it() {
    let mut board = Board::default();
    let mut rect = shape(ShapeKind::Rect, [0.0, 0.0], [200.0, 100.0]);
    rect.id = 7;
    board.shapes.push(rect);
    board.texts.push(TextItem {
        container: Some(7),
        ..text(40.0, 30.0, "label")
    });

    assert_eq!(board.bound_text_index(7), Some(0));
    assert_eq!(board.bound_text(7).map(|t| t.text.as_str()), Some("label"));
    assert_eq!(board.text_wrap(&board.texts[0]), Some(184.0));
    assert!(
        board.texts[0].container == Some(7),
        "a bound label was selected as a second object"
    );

    let copy = board.copied(&[ItemId::Shape(0)]);
    assert_eq!(copy.texts.len(), 1, "the clipboard left the label behind");
    assert_eq!(copy.texts[0].container, Some(7));

    let mut target = Board::default();
    let (_, _) = target.append(&copy);
    assert_eq!(target.texts.len(), 1);
    assert_eq!(target.texts[0].container, Some(target.shapes[0].id));

    board.remove(&[ItemId::Shape(0)]);
    assert!(
        board.texts.is_empty(),
        "removing the container left its label behind"
    );
}

#[test]
fn a_duplicate_clone_is_offset_and_keeps_its_bound_label() {
    let mut board = Board::default();
    let mut rect = shape(ShapeKind::Rect, [10.0, 20.0], [210.0, 120.0]);
    rect.id = 9;
    board.shapes.push(rect);
    board.texts.push(TextItem {
        container: Some(9),
        ..text(50.0, 50.0, "label")
    });

    let copy = board.copied(&[ItemId::Shape(0)]).shifted(12.0, 12.0);
    let (ids, _) = board.append(&copy);
    assert_eq!(ids.len(), 2, "the shape and its label were not cloned");
    assert_eq!(board.shapes.len(), 2);
    assert_eq!(board.texts.len(), 2);
    assert_eq!(board.shapes[1].from, [22.0, 32.0]);
    assert_eq!(board.shapes[1].to, [222.0, 132.0]);
    assert_eq!(board.texts[1].x, 62.0);
    assert_eq!(board.texts[1].y, 62.0);
    assert_eq!(board.texts[1].container, Some(board.shapes[1].id));
}

#[test]
fn a_line_label_sits_on_the_midpoint_measured_along_the_line() {
    let mut line = shape(ShapeKind::Line, [0.0, 0.0], [200.0, 0.0]);
    line.points.push([100.0, 100.0]);
    // The three segments have equal length; the middle of the whole polyline is the bend.
    assert!(
        (line.label_point()[0] - 100.0).abs() < 1e-3
            && (line.label_point()[1] - 100.0).abs() < 1e-3,
        "the label point is not the polyline midpoint: {:?}",
        line.label_point()
    );
}

#[test]
fn selection_exposes_endpoints_while_edit_exposes_every_vertex() {
    let mut line = shape(ShapeKind::Line, [0.0, 0.0], [100.0, 0.0]);
    line.insert_linear_vertex(0, [50.0, 0.0]);
    let view = View::default();

    assert_eq!(
        linear_handle_at(&line, view, [0.0, 0.0], false, None),
        Some(LinearHandle::Vertex(0))
    );
    assert_eq!(
        linear_handle_at(&line, view, [100.0, 0.0], false, None),
        Some(LinearHandle::Vertex(2))
    );
    // The inserted vertex is hidden in selection mode, where only endpoints and midpoints are
    // grabbable; edit mode promotes it to a full vertex handle.
    assert_eq!(
        linear_handle_at(&line, view, [50.0, 0.0], false, None),
        None
    );
    assert_eq!(
        linear_handle_at(&line, view, [50.0, 0.0], true, None),
        Some(LinearHandle::Vertex(1))
    );
    assert_eq!(
        linear_handle_at(&line, view, [25.0, 0.0], false, None),
        Some(LinearHandle::Midpoint(0))
    );
    assert_eq!(
        linear_handle_at(&line, view, [25.0, 0.0], true, None),
        None,
        "edit mode hides midpoint hints until a vertex is dragged"
    );
    assert_eq!(
        linear_handle_at(&line, view, [25.0, 0.0], true, Some(1)),
        Some(LinearHandle::Midpoint(0)),
        "the hint beside the dragged vertex is grabbable"
    );

    let board = Board {
        shapes: vec![line],
        ..Board::default()
    };
    assert_eq!(selected_linear(&board, &[ItemId::Shape(0)]), Some(0));
    assert_eq!(
        selected_linear(&board, &[ItemId::Shape(0), ItemId::Shape(1)]),
        None
    );
}

#[test]
fn a_seed_names_the_words_it_holds_and_how_to_remove_them() {
    // A note with its own text and a labelled shape: the seed carries the words, the place, and the
    // handles that take both away again. A label is named with its shape, never as a text of its own.
    let shape = Shape {
        id: 7,
        ..shape(ShapeKind::Rect, [200.0, 200.0], [300.0, 260.0])
    };
    let mut a = ann(
        vec![],
        vec![
            text(10.0, 10.0, "Robot"),
            TextItem {
                container: Some(7),
                ..text(210.0, 220.0, "Sensors")
            },
        ],
    );
    a.shapes.push(shape);
    let seed = seed_of(&a, &[ItemId::Text(0), ItemId::Shape(0)]).unwrap();
    assert_eq!(seed.text, "Robot / Sensors");
    assert_eq!(seed.shapes, vec![7]);
    assert_eq!(seed.texts, vec![(0, "Robot".to_string())]);
    assert!(seed.frame.w > 0.0 && seed.frame.h > 0.0, "{:?}", seed.frame);
    // Nothing held, nothing to expand.
    assert!(seed_of(&a, &[]).is_none());
}

#[test]
fn a_seed_stops_naming_words_it_cannot_carry() {
    // A long selection reaches the prompt as much of itself as fits, never as a truncated word.
    let long = "x".repeat(SEED_MAX_CHARS - 1);
    let a = ann(vec![], vec![text(0.0, 0.0, &long), text(0.0, 40.0, "tail")]);
    let seed = seed_of(&a, &[ItemId::Text(0), ItemId::Text(1)]).unwrap();
    assert!(seed.text.len() <= SEED_MAX_CHARS, "{}", seed.text.len());
    assert!(!seed.text.contains("tail") || seed.text.len() <= SEED_MAX_CHARS);
}
