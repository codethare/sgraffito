use sgraffito::canvas::{
    Doc, Hint, OutputAnnotations, Overlay, Rect, Stroke, TextBuffer, TextItem, TextOverlay,
};
use sgraffito::render::{DamageRegion, Renderer, damage_box, transient_bounds};

const W: u32 = 200;
const H: u32 = 100;

fn buffer(w: u32, h: u32) -> Vec<u8> {
    vec![0u8; (w * h * 4) as usize]
}

fn alpha(buf: &[u8], w: u32, x: u32, y: u32) -> u8 {
    buf[((y * w + x) * 4 + 3) as usize]
}

fn ink(buf: &[u8]) -> usize {
    buf.as_chunks::<4>().0.iter().filter(|p| p[3] != 0).count()
}

fn ann(strokes: Vec<Stroke>, texts: Vec<TextItem>) -> OutputAnnotations {
    OutputAnnotations { strokes, texts }
}

fn line(y: f32, x0: f32, x1: f32) -> Stroke {
    Stroke {
        color: "#e01b24".into(),
        width: 4.0,
        points: vec![[x0, y], [x1, y]],
    }
}

#[test]
fn empty_doc_leaves_buffer_transparent() {
    let mut buf = buffer(W, H);
    Renderer::new().render(
        &mut buf,
        W,
        H,
        1.0,
        &OutputAnnotations::default(),
        &Overlay::default(),
        None,
    );
    assert_eq!(ink(&buf), 0);
}

#[test]
fn committed_stroke_draws_ink_on_the_line_only() {
    let mut buf = buffer(W, H);
    let a = ann(vec![line(50.0, 20.0, 180.0)], vec![]);
    Renderer::new().render(&mut buf, W, H, 1.0, &a, &Overlay::default(), None);
    assert!(alpha(&buf, W, 100, 50) > 0);
    assert_eq!(alpha(&buf, W, 100, 10), 0);
    assert_eq!(alpha(&buf, W, 100, 90), 0);
}

#[test]
fn scale_multiplies_coordinates() {
    let (w, h) = (W * 2, H * 2);
    let mut buf = buffer(w, h);
    let a = ann(vec![line(50.0, 20.0, 180.0)], vec![]);
    Renderer::new().render(&mut buf, w, h, 2.0, &a, &Overlay::default(), None);
    assert!(alpha(&buf, w, 200, 100) > 0);
    assert_eq!(alpha(&buf, w, 200, 50), 0);
}

#[test]
fn text_item_draws_ink() {
    let mut buf = buffer(W, H);
    let a = ann(
        vec![],
        vec![TextItem {
            x: 10.0,
            y: 10.0,
            color: "#ffffff".into(),
            size: 32.0,
            text: "H".into(),
        }],
    );
    Renderer::new().render(&mut buf, W, H, 1.0, &a, &Overlay::default(), None);
    assert!(ink(&buf) > 0);
    assert_eq!(alpha(&buf, W, 190, 90), 0);
}

#[test]
fn overlay_in_progress_stroke_is_drawn() {
    let mut buf = buffer(W, H);
    let overlay = Overlay {
        stroke: Some(line(30.0, 10.0, 60.0)),
        ..Default::default()
    };
    Renderer::new().render(
        &mut buf,
        W,
        H,
        1.0,
        &OutputAnnotations::default(),
        &overlay,
        None,
    );
    assert!(alpha(&buf, W, 35, 30) > 0);
}

#[test]
fn overlay_eraser_marker_is_drawn() {
    let mut buf = buffer(W, H);
    let overlay = Overlay {
        eraser: Some([150.0, 20.0]),
        ..Default::default()
    };
    Renderer::new().render(
        &mut buf,
        W,
        H,
        1.0,
        &OutputAnnotations::default(),
        &overlay,
        None,
    );
    assert!(ink(&buf) > 0);
    assert_eq!(alpha(&buf, W, 100, 80), 0);
}

#[test]
fn overlay_text_box_draws_text_and_cursor() {
    let mut buf = buffer(W, H);
    let overlay = Overlay {
        text: Some(TextOverlay {
            item: TextItem {
                x: 10.0,
                y: 10.0,
                color: "#ffffff".into(),
                size: 32.0,
                text: "ab".into(),
            },
            buffer: TextBuffer::new("ab"),
            index: None,
            preedit: String::new(),
        }),
        ..Default::default()
    };
    Renderer::new().render(
        &mut buf,
        W,
        H,
        1.0,
        &OutputAnnotations::default(),
        &overlay,
        None,
    );
    assert!(ink(&buf) > 0);
}

#[test]
fn preedit_renders_more_ink_than_committed_text_alone() {
    let item = TextItem {
        x: 10.0,
        y: 10.0,
        color: "#ffffff".into(),
        size: 32.0,
        text: "ab".into(),
    };
    let mut plain = buffer(W, H);
    let mut with_preedit = buffer(W, H);
    let make = |preedit: &str| Overlay {
        text: Some(TextOverlay {
            item: item.clone(),
            buffer: TextBuffer::new("ab"),
            index: None,
            preedit: preedit.into(),
        }),
        ..Default::default()
    };
    Renderer::new().render(
        &mut plain,
        W,
        H,
        1.0,
        &OutputAnnotations::default(),
        &make(""),
        None,
    );
    Renderer::new().render(
        &mut with_preedit,
        W,
        H,
        1.0,
        &OutputAnnotations::default(),
        &make("ni"),
        None,
    );
    assert!(
        ink(&with_preedit) > ink(&plain),
        "preedit should add ink: {} vs {}",
        ink(&with_preedit),
        ink(&plain)
    );
}

#[test]
fn stroke_is_smoothed_through_the_sample_midpoints() {
    let mut buf = buffer(120, 80);
    let points = vec![[10.0, 60.0], [40.0, 20.0], [70.0, 60.0], [100.0, 20.0]];
    let a = ann(
        vec![Stroke {
            color: "#e01b24".into(),
            width: 4.0,
            points,
        }],
        vec![],
    );
    Renderer::new().render(&mut buf, 120, 80, 1.0, &a, &Overlay::default(), None);
    // The ends are still exactly on the first and last sample, with round caps.
    assert!(alpha(&buf, 120, 10, 60) > 0);
    assert!(alpha(&buf, 120, 100, 20) > 0);
    // The interior samples are control points of the curve, not points on it: a polyline
    // would paint both of these, a smoothed stroke cuts the corner instead.
    assert_eq!(alpha(&buf, 120, 40, 20), 0);
    assert_eq!(alpha(&buf, 120, 70, 60), 0);
}

#[test]
fn single_sample_stroke_is_a_dot() {
    let mut buf = buffer(W, H);
    let a = ann(
        vec![Stroke {
            color: "#e01b24".into(),
            width: 4.0,
            points: vec![[60.0, 40.0]],
        }],
        vec![],
    );
    Renderer::new().render(&mut buf, W, H, 1.0, &a, &Overlay::default(), None);
    assert!(alpha(&buf, W, 60, 40) > 0);
    assert_eq!(alpha(&buf, W, 60, 50), 0);
}

/// One frame exactly as the daemon composes it: clear the damaged region, draw, then
/// swap the R and B bytes of that region.
fn frame(
    buf: &mut [u8],
    w: u32,
    h: u32,
    ann: &OutputAnnotations,
    overlay: &Overlay,
    damage: Option<Rect>,
) {
    let region = match damage {
        None => DamageRegion::All,
        Some(d) => DamageRegion::from_logical(d, 1.0, w, h),
    };
    region.clear(buf, w, h);
    Renderer::new().render(buf, w, h, 1.0, ann, overlay, damage);
    region.swap_rb(buf, w, h);
}

fn dot(x: f32, y: f32) -> Overlay {
    Overlay {
        stroke: Some(Stroke {
            color: "#f6d32d".into(),
            width: 3.0,
            points: vec![[x, y]],
        }),
        ..Default::default()
    }
}

#[test]
fn hint_draws_ink_only_when_it_is_present() {
    let doc = OutputAnnotations::default();
    let with = Overlay {
        hint: Some(Hint {
            tool: "text",
            color: "#33d17a",
            size: 18.0,
        }),
        ..Default::default()
    };
    let mut buf = buffer(640, 120);
    Renderer::new().render(&mut buf, 640, 120, 1.0, &doc, &with, None);
    assert!(ink(&buf) > 0);

    let mut plain = buffer(640, 120);
    Renderer::new().render(&mut plain, 640, 120, 1.0, &doc, &Overlay::default(), None);
    assert_eq!(ink(&plain), 0);
}

#[test]
fn hint_is_a_centred_translucent_capsule() {
    let (w, h) = (640u32, 120u32);
    let mut buf = buffer(w, h);
    let overlay = Overlay {
        hint: Some(Hint {
            tool: "pen",
            color: "#33d17a",
            size: 3.0,
        }),
        ..Default::default()
    };
    Renderer::new().render(
        &mut buf,
        w,
        h,
        1.0,
        &OutputAnnotations::default(),
        &overlay,
        None,
    );
    // The capsule lives in the top strip and is centred: the row through its middle is
    // painted in the middle of the surface and empty at the far left edge.
    let painted: Vec<u32> = (0..w).filter(|x| alpha(&buf, w, *x, 33) > 0).collect();
    assert!(!painted.is_empty());
    let (left, right) = (*painted.first().unwrap(), *painted.last().unwrap());
    assert!(
        (left + right).abs_diff(w - 1) <= 2,
        "pill {left}..{right} is not centred on {w}"
    );
    assert_eq!(alpha(&buf, w, 2, 33), 0);
    // The fill is translucent, so the wallpaper still reads through the capsule.
    let fill = alpha(&buf, w, left + 4, 33);
    assert!(fill > 0 && fill < 255, "fill alpha {fill}");
}

#[test]
fn the_eraser_cannot_reach_the_hint() {
    // The hint lives in the transient overlay, never in the document, so a click at its
    // position has nothing to delete — including the colour swatch at the hint's origin.
    let mut doc = OutputAnnotations::default();
    assert!(!doc.erase(16.0, 16.0));
    assert!(!doc.erase(40.0, 22.0));
}

#[test]
fn damage_region_clears_and_swaps_only_its_rectangle() {
    let (w, h) = (4u32, 2u32);
    let mut buf = vec![9u8; (w * h * 4) as usize];
    let region = DamageRegion::from_logical(
        Rect {
            x: 1.0,
            y: 0.0,
            w: 1.0,
            h: 2.0,
        },
        1.0,
        w,
        h,
    );
    region.clear(&mut buf, w, h);
    for y in 0..h {
        for x in 0..w {
            let v = buf[((y * w + x) * 4) as usize];
            assert_eq!(v, if x == 1 { 0 } else { 9 }, "pixel {x},{y}");
        }
    }
    // A rectangle that runs off the surface is clipped to it.
    assert_eq!(
        DamageRegion::from_logical(
            Rect {
                x: -5.0,
                y: -5.0,
                w: 100.0,
                h: 100.0
            },
            1.0,
            w,
            h
        ),
        DamageRegion::Rect {
            x0: 0,
            y0: 0,
            x1: 4,
            y1: 2
        }
    );

    let mut buf = vec![0u8; (w * h * 4) as usize];
    let pixel = 4usize; // pixel (1, 0) of a 4-wide surface
    buf[pixel..pixel + 4].copy_from_slice(&[1, 2, 3, 4]);
    DamageRegion::Rect {
        x0: 1,
        y0: 0,
        x1: 2,
        y1: 1,
    }
    .swap_rb(&mut buf, w, h);
    assert_eq!(&buf[pixel..pixel + 4], &[3, 2, 1, 4]);
    // The pixel just outside the region keeps its bytes untouched.
    assert_eq!(&buf[pixel + 4..pixel + 8], &[0, 0, 0, 0]);
}

#[test]
fn damage_box_grows_to_contain_the_element_it_overlaps() {
    let a = ann(vec![line(50.0, 20.0, 180.0)], vec![]);
    let transient = Rect {
        x: 100.0,
        y: 40.0,
        w: 10.0,
        h: 10.0,
    };
    let grown = damage_box(&a, &Overlay::default(), transient, W as f32);
    // The line's box is 20..180 x 47..53 plus half the width and the anti-aliasing edge, and
    // the line is drawn whole, so the damage box has to reach past both of its ends: outside
    // the box those pixels would keep already-swapped bytes.
    assert!(grown.x <= 17.0 && grown.x + grown.w >= 183.0, "{grown:?}");
    assert!(grown.y <= 40.0 && grown.y + grown.h >= 53.0, "{grown:?}");
    // An element that does not overlap leaves the box alone.
    let far = Rect {
        x: 300.0,
        y: 300.0,
        w: 10.0,
        h: 10.0,
    };
    assert_eq!(damage_box(&a, &Overlay::default(), far, W as f32), far);
}

#[test]
fn bounding_box_frame_is_pixel_identical_to_a_whole_surface_frame() {
    let doc = ann(
        vec![line(50.0, 20.0, 180.0), line(70.0, 20.0, 180.0)],
        vec![TextItem {
            x: 120.0,
            y: 60.0,
            color: "#ffffff".into(),
            size: 18.0,
            text: "note".into(),
        }],
    );
    let first = dot(100.0, 45.0);
    let second = dot(104.0, 48.0);
    let damage = damage_box(
        &doc,
        &second,
        transient_bounds(&first)
            .unwrap()
            .union(transient_bounds(&second).unwrap()),
        W as f32,
    );
    // Both buffers start from the same frame, then get the second frame in the two ways.
    let mut whole = buffer(W, H);
    frame(&mut whole, W, H, &doc, &first, None);
    let mut boxed = whole.clone();
    frame(&mut whole, W, H, &doc, &second, None);
    frame(&mut boxed, W, H, &doc, &second, Some(damage));
    assert_eq!(whole, boxed);
}

#[test]
fn a_hint_does_not_widen_a_drag_far_from_it() {
    let overlay = Overlay {
        hint: Some(Hint {
            tool: "pen",
            color: "#ffffff",
            size: 3.0,
        }),
        ..Default::default()
    };
    let transient = Rect {
        x: 100.0,
        y: 300.0,
        w: 10.0,
        h: 10.0,
    };
    // The hint sits in the top strip; a drag far below it must not drag it along.
    assert_eq!(
        damage_box(&OutputAnnotations::default(), &overlay, transient, 1920.0),
        transient
    );
}

#[test]
fn bounding_box_frame_with_a_hint_matches_a_whole_surface_frame() {
    // The reserve has to hold the whole capsule: a frame that damages only the box around a
    // drag into the hint must still paint every capsule pixel the whole-surface frame paints.
    let (w, h) = (1024u32, 200u32);
    let hint = Hint {
        tool: "text",
        color: "#33d17a",
        size: 72.0,
    };
    let first = Overlay {
        hint: Some(hint),
        ..dot(500.0, 20.0)
    };
    let second = Overlay {
        hint: Some(hint),
        ..dot(504.0, 24.0)
    };
    let damage = damage_box(
        &OutputAnnotations::default(),
        &second,
        transient_bounds(&first)
            .unwrap()
            .union(transient_bounds(&second).unwrap()),
        w as f32,
    );
    let mut whole = buffer(w, h);
    frame(
        &mut whole,
        w,
        h,
        &OutputAnnotations::default(),
        &first,
        None,
    );
    let mut boxed = whole.clone();
    frame(
        &mut whole,
        w,
        h,
        &OutputAnnotations::default(),
        &second,
        None,
    );
    frame(
        &mut boxed,
        w,
        h,
        &OutputAnnotations::default(),
        &second,
        Some(damage),
    );
    assert_eq!(whole, boxed);
}

#[test]
fn doc_strokes_of_other_outputs_are_not_drawn() {
    let mut buf = buffer(W, H);
    let mut doc = Doc::default();
    doc.outputs
        .insert("eDP-1".into(), ann(vec![line(50.0, 20.0, 180.0)], vec![]));
    // Rendering an output reads only that output's bucket; an empty bucket means no annotations
    Renderer::new().render(
        &mut buf,
        W,
        H,
        1.0,
        &OutputAnnotations::default(),
        &Overlay::default(),
        None,
    );
    assert_eq!(ink(&buf), 0);
}
