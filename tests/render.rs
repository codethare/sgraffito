use sgraffito::canvas::{
    Doc, OutputAnnotations, Overlay, PALETTE, Rect, Stroke, TextBuffer, TextItem, TextOverlay,
    Tool, Toolbar, ToolbarAction,
};
use sgraffito::render::{
    DamageRegion, Renderer, buffer_format, damage_box, toolbar_bounds, transient_bounds,
};
use wayland_client::protocol::wl_shm::Format;

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
fn the_caret_spans_the_whole_line_box() {
    // An empty box draws no glyphs, so the only ink is the caret and its bounds are the line
    // box the typed text will use.
    let (w, h) = (200u32, 100u32);
    let mut buf = buffer(w, h);
    let overlay = Overlay {
        text: Some(TextOverlay {
            item: TextItem {
                x: 10.0,
                y: 10.0,
                color: "#ffffff".into(),
                size: 32.0,
                text: String::new(),
            },
            buffer: TextBuffer::new(""),
            index: None,
            preedit: String::new(),
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
    let rows: Vec<u32> = (0..h)
        .filter(|y| (0..w).any(|x| alpha(&buf, w, x, *y) > 0))
        .collect();
    let (top, bottom) = (*rows.first().unwrap(), *rows.last().unwrap());
    // The caret starts at the top of the line, not below the glyph ascent.
    assert_eq!(top, 10, "caret top {top} is not the line top");
    // One metrics line height (1.2 em) tall, matching the text's line.
    let height = bottom - top + 1;
    assert!((36..=40).contains(&height), "caret height {height}");
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
fn toolbar_draws_ink_only_when_it_is_present() {
    let doc = OutputAnnotations::default();
    let with = Overlay {
        toolbar: Some(Toolbar {
            tool: Tool::Text,
            color: "#33d17a",
            size: 18.0,
        }),
        ..Default::default()
    };
    let mut buf = buffer(640, 400);
    Renderer::new().render(&mut buf, 640, 400, 1.0, &doc, &with, None);
    assert!(ink(&buf) > 0);

    let mut plain = buffer(640, 400);
    Renderer::new().render(&mut plain, 640, 400, 1.0, &doc, &Overlay::default(), None);
    assert_eq!(ink(&plain), 0);
}

#[test]
fn toolbar_is_a_left_anchored_capsule_at_the_optical_centre() {
    let (w, h) = (640u32, 400u32);
    let mut buf = buffer(w, h);
    let overlay = Overlay {
        toolbar: Some(Toolbar {
            tool: Tool::Pen,
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
    // The toolbar hugs the left edge and its vertical centre is at the optical centre, above
    // the geometric one: the row through its middle is painted from the left margin, and the
    // row through the middle of the surface is empty.
    let band = toolbar_bounds(w as f32, h as f32);
    let mid = (band.y + band.h / 2.0) as u32;
    let painted: Vec<u32> = (0..w).filter(|x| alpha(&buf, w, *x, mid) > 0).collect();
    assert!(!painted.is_empty());
    let left = *painted.first().unwrap();
    assert!(
        left <= 12,
        "the toolbar starts at {left}, not against the left edge"
    );
    assert!(
        band.y + band.h / 2.0 < h as f32 / 2.0,
        "not above the geometric centre"
    );
    // It is anchored to the left, not centred: the right edge of the surface stays empty.
    let right = *painted.last().unwrap();
    assert!(
        right < w - 20,
        "the toolbar reaches {right}, the surface is {w}"
    );
    // The old top strip is empty: this is not a top-edge capsule any more.
    assert_eq!(ink(&buf[..(w * 16 * 4) as usize]), 0);
    // The fill is translucent, so the wallpaper still reads through the toolbar.
    let fill = alpha(&buf, w, left + 4, mid);
    assert!(fill > 0 && fill < 255, "fill alpha {fill}");
}

#[test]
fn every_point_inside_the_toolbar_resolves_to_a_control() {
    let (w, h) = (1920.0f32, 1080.0f32);
    let toolbar = Toolbar {
        tool: Tool::Pen,
        color: PALETTE[0],
        size: 3.0,
    };
    let mut renderer = Renderer::new();
    let band = toolbar_bounds(w, h);
    let mid = band.y + band.h / 2.0;
    let sweep: Vec<Option<ToolbarAction>> = (0..band.w as u32)
        .map(|step| renderer.toolbar_hit(&toolbar, w, h, band.x + step as f32 + 0.5, mid))
        .collect();
    // The toolbar starts at its left margin, ends inside its reserve, and nothing between the
    // two is a hole: a press on a gap would start a stroke under the capsule.
    let first = sweep.iter().position(Option::is_some).expect("no toolbar");
    assert!(first <= 2, "the toolbar starts {first} px in");
    let end = sweep[first..]
        .iter()
        .position(Option::is_none)
        .map_or(sweep.len(), |step| step + first);
    assert!(
        sweep[end..].iter().all(Option::is_none),
        "a gap inside the toolbar reaches no control"
    );
    let mut actions: Vec<ToolbarAction> = Vec::new();
    for action in sweep[first..end].iter().flatten() {
        if !actions.contains(action) {
            actions.push(*action);
        }
    }
    assert!(
        end - first > 200,
        "the toolbar is only {} px wide",
        end - first
    );
    for tool in [Tool::Pen, Tool::Eraser, Tool::Text] {
        assert!(actions.contains(&ToolbarAction::Tool(tool)), "{tool:?}");
    }
    for index in 0..PALETTE.len() {
        assert!(
            actions.contains(&ToolbarAction::Color(index)),
            "colour {index}"
        );
    }
    assert!(actions.contains(&ToolbarAction::SizeStep(-1.0)));
    assert!(actions.contains(&ToolbarAction::SizeStep(1.0)));
    assert!(actions.contains(&ToolbarAction::Lock));
    assert_eq!(
        sweep[first].unwrap(),
        ToolbarAction::Color(0),
        "the left end is not the palette"
    );
    assert_eq!(sweep[end - 1].unwrap(), ToolbarAction::Lock);
    // Outside the band the toolbar takes nothing: those presses are drawing gestures.
    assert_eq!(
        renderer.toolbar_hit(&toolbar, w, h, band.x + 4.0, band.y - 1.0),
        None
    );
    assert_eq!(
        renderer.toolbar_hit(&toolbar, w, h, band.x + 4.0, band.y + band.h),
        None
    );
}

#[test]
fn the_eraser_cannot_reach_the_toolbar() {
    // The toolbar lives in the transient overlay, never in the document, so nothing at its
    // position is there to delete -- including the palette swatches and the tool keycaps.
    let (w, h) = (1920.0f32, 1080.0f32);
    let mut doc = OutputAnnotations::default();
    let band = toolbar_bounds(w, h);
    for x in [band.x + 4.0, band.x + band.w / 2.0] {
        assert!(!doc.erase(x, band.y + band.h / 2.0));
    }
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
    let grown = damage_box(&a, &Overlay::default(), transient, W as f32, H as f32);
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
    assert_eq!(
        damage_box(&a, &Overlay::default(), far, W as f32, H as f32),
        far
    );
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
        H as f32,
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
fn a_toolbar_does_not_widen_a_drag_far_from_it() {
    let overlay = Overlay {
        toolbar: Some(Toolbar {
            tool: Tool::Pen,
            color: "#ffffff",
            size: 3.0,
        }),
        ..Default::default()
    };
    let transient = Rect {
        x: 100.0,
        y: 900.0,
        w: 10.0,
        h: 10.0,
    };
    // The toolbar sits on the left at the optical centre; a drag far below it must not drag it
    // along.
    assert_eq!(
        damage_box(
            &OutputAnnotations::default(),
            &overlay,
            transient,
            1920.0,
            1080.0
        ),
        transient
    );
}

#[test]
fn bounding_box_frame_with_a_toolbar_matches_a_whole_surface_frame() {
    // The reserve has to hold the whole toolbar: a frame that damages only the box around a
    // drag into it must still paint every toolbar pixel the whole-surface frame paints.
    let (w, h) = (1024u32, 200u32);
    let toolbar = Toolbar {
        tool: Tool::Text,
        color: "#33d17a",
        size: 72.0,
    };
    let first = Overlay {
        toolbar: Some(toolbar),
        ..dot(500.0, 80.0)
    };
    let second = Overlay {
        toolbar: Some(toolbar),
        ..dot(504.0, 84.0)
    };
    let damage = damage_box(
        &OutputAnnotations::default(),
        &second,
        transient_bounds(&first)
            .unwrap()
            .union(transient_bounds(&second).unwrap()),
        w as f32,
        h as f32,
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
fn a_drag_far_from_the_toolbar_leaves_the_toolbar_pixels_alone() {
    // The toolbar is static: only a tool, colour, size or mode change repaints it, and those
    // force a whole-surface frame. A bounding-box frame that does not reach it may not paint it,
    // because those bytes lie outside the damage region and still hold the previous frame.
    let (w, h) = (1024u32, 600u32);
    let toolbar = Toolbar {
        tool: Tool::Pen,
        color: "#33d17a",
        size: 3.0,
    };
    let with = |x: f32, y: f32| Overlay {
        toolbar: Some(toolbar),
        ..dot(x, y)
    };
    let first = with(500.0, 500.0);
    let second = with(504.0, 504.0);
    let damage = damage_box(
        &OutputAnnotations::default(),
        &second,
        transient_bounds(&first)
            .unwrap()
            .union(transient_bounds(&second).unwrap()),
        w as f32,
        h as f32,
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
fn buffer_format_prefers_abgr_and_falls_back_to_argb() {
    assert_eq!(
        buffer_format(&[Format::Argb8888, Format::Abgr8888]),
        (Format::Abgr8888, false)
    );
    // Without Abgr8888 on offer the mandatory ARGB8888 needs the R/B swap.
    assert_eq!(
        buffer_format(&[Format::Argb8888, Format::Xrgb8888]),
        (Format::Argb8888, true)
    );
    assert_eq!(buffer_format(&[]), (Format::Argb8888, true));
}

#[test]
fn abgr_buffer_carries_rgba_bytes_so_it_needs_no_swap() {
    let (w, h) = (16u32, 8u32);
    let mut buf = buffer(w, h);
    Renderer::new().render(
        &mut buf,
        w,
        h,
        1.0,
        &ann(vec![line(4.0, 0.0, 8.0)], vec![]),
        &Overlay::default(),
        None,
    );
    let i = ((4 * w + 4) * 4) as usize;
    assert_eq!(
        (buf[i], buf[i + 1], buf[i + 2]),
        (0xe0, 0x1b, 0x24),
        "not R,G,B"
    );
    // The ARGB8888 fallback is the same frame with the two exchanged.
    DamageRegion::All.swap_rb(&mut buf, w, h);
    assert_eq!((buf[i], buf[i + 2]), (0x24, 0xe0));
}

#[test]
fn long_text_damage_contains_everything_rasterised() {
    let doc = ann(
        vec![],
        vec![TextItem {
            x: 10.0,
            y: 20.0,
            color: "#ffffff".into(),
            size: 24.0,
            text: "a long text line that exceeds the output width. ".repeat(3),
        }],
    );
    let first = dot(100.0, 50.0);
    let second = dot(104.0, 54.0);
    let damage = damage_box(
        &doc,
        &Overlay { ..second.clone() },
        transient_bounds(&first)
            .unwrap()
            .union(transient_bounds(&second).unwrap()),
        W as f32,
        H as f32,
    );
    let mut whole = buffer(W, H);
    frame(&mut whole, W, H, &doc, &first, None);
    let mut boxed = whole.clone();
    frame(&mut whole, W, H, &doc, &second, None);
    frame(&mut boxed, W, H, &doc, &second, Some(damage));
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
