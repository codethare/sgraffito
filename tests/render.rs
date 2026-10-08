use sgraffito::canvas::{
    Doc, OutputAnnotations, Overlay, PALETTE, Rect, Stroke, TextBuffer, TextItem, TextOverlay,
    Tool, Toolbar, ToolbarAction,
};
use sgraffito::render::{
    DamageRegion, Renderer, buffer_format, toolbar_bounds, transient_bounds, transient_damage,
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

/// One frame exactly as the daemon composes it: the renderer fills the damaged region from the
/// document cache, draws the overlays, then swaps the R and B bytes of that region.
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
    Renderer::new().render(buf, w, h, 1.0, ann, overlay, damage);
    region.swap_rb(buf, w, h);
}

/// A toolbar state for the tests, with the pop-out fully open.
fn toolbar(tool: Tool, color: &'static str, size: f32, hover: Option<ToolbarAction>) -> Toolbar {
    Toolbar {
        tool,
        color,
        size,
        hover,
        grow: 1.0,
    }
}

/// The rail with the pen armed and no block stretched.
fn rail() -> Toolbar {
    toolbar(Tool::Pen, PALETTE[0], 3.0, None)
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
        toolbar: Some(toolbar(Tool::Text, "#33d17a", 18.0, None)),
        ..Default::default()
    };
    let mut buf = buffer(640, 900);
    Renderer::new().render(&mut buf, 640, 900, 1.0, &doc, &with, None);
    assert!(ink(&buf) > 0);

    let mut plain = buffer(640, 900);
    Renderer::new().render(&mut plain, 640, 900, 1.0, &doc, &Overlay::default(), None);
    assert_eq!(ink(&plain), 0);
}

#[test]
fn toolbar_is_a_left_edge_rail_at_the_optical_centre() {
    let (w, h) = (640u32, 900u32);
    let mut buf = buffer(w, h);
    let overlay = Overlay {
        toolbar: Some(toolbar(Tool::Pen, "#33d17a", 3.0, None)),
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
    // The rail hangs on the left edge and fills its box top to bottom: column x 8..44 holds a
    // block on every row of it.
    let band = toolbar_bounds(w as f32, h as f32);
    let column: Vec<u32> = (0..h).filter(|y| alpha(&buf, w, 20, *y) > 0).collect();
    assert!(!column.is_empty());
    let (top, bottom) = (*column.first().unwrap(), *column.last().unwrap());
    assert!(
        top as f32 >= band.y
            && top as f32 <= band.y + 8.0
            && bottom as f32 <= band.y + band.h
            && bottom as f32 >= band.y + band.h - 8.0,
        "the rail is painted {top}..{bottom}, its box is {band:?}"
    );
    // Its vertical centre is at the optical centre, above the geometric one.
    let centre = (top + bottom) as f32 / 2.0;
    let optical = h as f32 * 0.45;
    assert!(
        (centre - optical).abs() <= 8.0,
        "the rail is centred on {centre}, not on the optical centre {optical}"
    );
    assert!(centre < h as f32 / 2.0, "not above the geometric centre");
    // Only the rail is painted: the rest of its band, and the top strip, are empty.
    assert_eq!(ink(&buf[..(w * 16 * 4) as usize]), 0);
    assert_eq!(alpha(&buf, w, 200, (band.y + band.h / 2.0) as u32), 0);
    // A block fill is translucent, so the wallpaper still reads through the rail. The top block
    // is a colour chip, which is opaque; the bottom one is a key.
    let fill = alpha(&buf, w, 20, (band.y + band.h - 21.0) as u32);
    assert!(fill > 0 && fill < 255, "fill alpha {fill}");
}

/// Whether a pixel carries text rather than a block fill: the label is much lighter than the
/// translucent dark fill, so a plain threshold separates them.
fn bright(buf: &[u8], w: u32, x: u32, y: u32) -> bool {
    let i = ((y * w + x) * 4) as usize;
    buf[i] as u32 + buf[i + 1] as u32 + buf[i + 2] as u32 > 260
}

/// The y of the row whose block has this action, found by sweeping the rail.
fn row_of(
    renderer: &mut Renderer,
    toolbar: &Toolbar,
    w: f32,
    h: f32,
    action: ToolbarAction,
) -> f32 {
    let band = toolbar_bounds(w, h);
    (0..band.h as u32)
        .map(|step| band.y + step as f32 + 0.5)
        .find(|y| renderer.toolbar_hit(toolbar, w, h, band.x + 30.0, *y) == Some(action))
        .expect("the rail has no such block")
}

/// One hit test per row of the rail, at `x` in column coordinates.
fn sweep_rail(
    renderer: &mut Renderer,
    toolbar: &Toolbar,
    w: f32,
    h: f32,
    column_x: f32,
) -> Vec<Option<ToolbarAction>> {
    let band = toolbar_bounds(w, h);
    // `toolbar_bounds` starts one hairline left of the rail.
    let x = band.x + 1.0 + column_x;
    (0..band.h as u32)
        .map(|step| renderer.toolbar_hit(toolbar, w, h, x, band.y + step as f32 + 0.5))
        .collect()
}

#[test]
fn every_block_of_the_rail_is_reachable_and_no_row_is_a_gap() {
    let (w, h) = (1920.0f32, 1080.0f32);
    let rail = rail();
    let mut renderer = Renderer::new();
    let sweep = sweep_rail(&mut renderer, &rail, w, h, 20.0);
    // Only the hairline rows at either end of the box reach no block; nothing between them does,
    // or a press on a gap would start a stroke under the rail.
    let first = sweep.iter().position(Option::is_some).expect("no rail");
    assert!(first <= 2, "the rail starts {first} rows in");
    let end = sweep[first..]
        .iter()
        .position(Option::is_none)
        .map_or(sweep.len(), |step| step + first);
    assert!(
        sweep[end..].iter().all(Option::is_none),
        "a row inside the rail reaches no block"
    );
    let mut actions: Vec<ToolbarAction> = Vec::new();
    for action in sweep[first..end].iter().flatten() {
        if actions.last() != Some(action) {
            actions.push(*action);
        }
    }
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
    assert_eq!(
        actions[0],
        ToolbarAction::Color(0),
        "the palette is not on top"
    );
    assert_eq!(
        *actions.last().unwrap(),
        ToolbarAction::Lock,
        "Esc is not last"
    );
    // Eleven clickable blocks: the size readout is drawn but owned by `[` above it.
    assert_eq!(actions.len(), 11, "a drawn block cannot be hit");
    // Outside the rail the toolbar takes nothing: those presses are drawing gestures.
    let band = toolbar_bounds(w, h);
    assert_eq!(
        renderer.toolbar_hit(&rail, w, h, band.x + 20.0, band.y - 2.0),
        None
    );
    assert_eq!(
        renderer.toolbar_hit(&rail, w, h, band.x + 20.0, band.y + band.h + 2.0),
        None
    );
    assert_eq!(
        renderer.toolbar_hit(&rail, w, h, band.x - 2.0, band.y + 30.0),
        None
    );
    // To the right of an unstretched block is canvas, not toolbar.
    assert_eq!(
        renderer.toolbar_hit(&rail, w, h, band.x + 80.0, band.y + 30.0),
        None
    );
}

#[test]
fn a_hovered_block_stretches_to_say_what_it_does() {
    let (w, h) = (1920u32, 1080u32);
    let (fw, fh) = (w as f32, h as f32);
    let rail = rail();
    let mut renderer = Renderer::new();
    let band = toolbar_bounds(fw, fh);
    // The top block is the first colour. `inside` is on the square, `beyond` in the room the
    // capsule it unfurls would take, both on that block's own row.
    let row = band.y + 20.0;
    let (inside, beyond) = (band.x + 22.0, band.x + 70.0);
    assert_eq!(
        renderer.toolbar_hit(&rail, fw, fh, inside, row),
        Some(ToolbarAction::Color(0))
    );
    assert_eq!(renderer.toolbar_hit(&rail, fw, fh, beyond, row), None);

    let hovered = Toolbar {
        hover: Some(ToolbarAction::Color(0)),
        ..rail
    };
    // Stretched, the block owns the capsule it unfurled: a press there is still the toolbar's,
    // which is what keeps the pointer on the block while it reads the meaning.
    assert_eq!(
        renderer.toolbar_hit(&hovered, fw, fh, inside, row),
        Some(ToolbarAction::Color(0))
    );
    assert_eq!(
        renderer.toolbar_hit(&hovered, fw, fh, beyond, row),
        Some(ToolbarAction::Color(0))
    );
    assert_eq!(
        renderer.toolbar_hit(&hovered, fw, fh, beyond, row + 200.0),
        None,
        "the stretch is only on the block's own row"
    );
    // A block only stretches while its meaning fits what the damage accounting reserves.
    let narrow = Toolbar {
        hover: Some(ToolbarAction::Color(0)),
        ..rail
    };
    assert_eq!(
        renderer.toolbar_hit(&narrow, fw, fh, beyond, row),
        Some(ToolbarAction::Color(0)),
        "the label did not fit a 4K output"
    );
    assert_eq!(
        renderer.toolbar_hit(&narrow, 60.0, fh, beyond, row),
        None,
        "a block stretched past the room reserved for it"
    );

    // Stretching paints the meaning: the square alone has no ink where the label goes.
    let draw = |hover| {
        let mut buf = buffer(w, h);
        let overlay = Overlay {
            toolbar: Some(Toolbar { hover, ..rail }),
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
        buf
    };
    let (square, capsule) = (draw(None), draw(Some(ToolbarAction::Color(0))));
    let strip = |buf: &[u8]| {
        (0..h)
            .flat_map(|y| (50..200).map(move |x| (x, y)))
            .filter(|(x, y)| alpha(buf, w, *x, *y) > 0)
            .count()
    };
    assert!(
        strip(&capsule) > strip(&square),
        "the block did not stretch"
    );
    let outside = |buf: &[u8]| {
        buf.as_chunks::<4>()
            .0
            .iter()
            .enumerate()
            .filter(|(_, pixel)| pixel[3] != 0)
            .filter(|(index, _)| {
                let (x, y) = ((*index as u32) % w, (*index as u32) / w);
                x as f32 > band.x + band.w || y as f32 > band.y + band.h
            })
            .count()
    };
    assert_eq!(
        outside(&capsule),
        0,
        "the stretched block paints outside the box the damage model reserves"
    );
}

#[test]
fn the_eraser_cannot_reach_the_toolbar() {
    // The toolbar lives in the transient overlay, never in the document, so nothing at its
    // position is there to delete -- including the palette swatches and the tool blocks.
    let (w, h) = (1920.0f32, 1080.0f32);
    let mut doc = OutputAnnotations::default();
    let band = toolbar_bounds(w, h);
    for y in [band.y + 20.0, band.y + band.h / 2.0] {
        assert!(doc.erase(band.x + 20.0, y).is_none());
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
fn the_damage_box_is_the_element_that_changed() {
    // A transient frame paints no document element, so its box never grows: the strokes under it
    // come back from the document cache. This is the rule that used to make a drag on a dense
    // drawing cost nearly a whole-surface frame.
    let here = Rect {
        x: 100.0,
        y: 40.0,
        w: 10.0,
        h: 10.0,
    };
    let moved = Rect {
        x: 140.0,
        y: 60.0,
        w: 10.0,
        h: 10.0,
    };
    assert_eq!(transient_damage(None, Some(here), None), here);
    assert_eq!(
        transient_damage(Some(here), Some(here), None),
        here,
        "standing still repaints one box"
    );
    assert_eq!(
        transient_damage(Some(here), Some(moved), None),
        here.union(moved),
        "moving on has to erase the box it left"
    );
    // The rail joins the box only when the rail is what changed: a drag far from it leaves it
    // alone, and a drag that reaches it repaints it whole.
    let rail_box = toolbar_bounds(1024.0, 600.0);
    assert_eq!(transient_damage(None, Some(here), None), here);
    assert_eq!(
        transient_damage(None, Some(here), Some(rail_box)),
        here.union(rail_box)
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
    let damage = transient_damage(transient_bounds(&first), transient_bounds(&second), None);
    // Both buffers start from the same frame, then get the second frame in the two ways.
    let mut whole = buffer(W, H);
    frame(&mut whole, W, H, &doc, &first, None);
    let mut boxed = whole.clone();
    frame(&mut whole, W, H, &doc, &second, None);
    frame(&mut boxed, W, H, &doc, &second, Some(damage));
    assert_eq!(whole, boxed);
}

#[test]
fn a_small_box_over_a_dense_document_keeps_the_document_pixels() {
    // The case that used to collapse into a whole-surface frame: a drag over a drawing whose
    // strokes all cross the drag's box. Every stroke under the box comes back from the document
    // cache, so a box the size of the drag is enough.
    let strokes = (0..60)
        .map(|i| line(10.0 + i as f32 * 1.4, 5.0, 195.0))
        .collect();
    let doc = ann(strokes, vec![]);
    let first = dot(100.0, 45.0);
    let second = dot(104.0, 48.0);
    let damage = transient_damage(transient_bounds(&first), transient_bounds(&second), None);
    let mut whole = buffer(W, H);
    frame(&mut whole, W, H, &doc, &first, None);
    let mut boxed = whole.clone();
    frame(&mut whole, W, H, &doc, &second, None);
    frame(&mut boxed, W, H, &doc, &second, Some(damage));
    assert_eq!(whole, boxed);
}

#[test]
fn bounding_box_frame_with_a_toolbar_matches_a_whole_surface_frame() {
    // The reserve has to hold the whole toolbar: a frame that damages only the box around a
    // drag into it must still paint every toolbar pixel the whole-surface frame paints.
    let (w, h) = (1024u32, 600u32);
    let rail = rail();
    // On the rail's own row: the damage box has to grow over the whole rail and repaint it.
    let first = Overlay {
        toolbar: Some(rail),
        ..dot(140.0, 300.0)
    };
    let second = Overlay {
        toolbar: Some(rail),
        ..dot(144.0, 304.0)
    };
    let damage = transient_damage(
        transient_bounds(&first),
        transient_bounds(&second),
        Some(toolbar_bounds(w as f32, h as f32)),
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
    let rail = rail();
    let with = |x: f32, y: f32| Overlay {
        toolbar: Some(rail),
        ..dot(x, y)
    };
    let first = with(500.0, 500.0);
    let second = with(504.0, 504.0);
    let damage = transient_damage(transient_bounds(&first), transient_bounds(&second), None);
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
    let damage = transient_damage(transient_bounds(&first), transient_bounds(&second), None);
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

/// One frame of the rail with the given hover and pop-out progress.
fn rail_frame(w: u32, h: u32, wheel: &Toolbar, hover: Option<ToolbarAction>, grow: f32) -> Vec<u8> {
    let mut buf = buffer(w, h);
    let overlay = Overlay {
        toolbar: Some(Toolbar {
            hover,
            grow,
            ..*wheel
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
    buf
}

#[test]
fn the_capsule_keeps_room_around_its_label() {
    // The metrics CLAUDE.md fixes for a control: 6 pt between a block and its label, and 12 pt
    // of padding after a label, so the text never touches the capsule's edge.
    const GAP: f32 = 6.0;
    const PAD: f32 = 12.0;
    let (w, h) = (1920u32, 1080u32);
    let (fw, fh) = (w as f32, h as f32);
    let mut renderer = Renderer::new();
    let wheel = rail();
    let text = ToolbarAction::Tool(Tool::Text);
    let row = (row_of(&mut renderer, &wheel, fw, fh, text) + 15.0) as u32;
    let buf = rail_frame(w, h, &wheel, Some(text), 1.0);
    let ox = toolbar_bounds(fw, fh).x + 1.0;
    let painted: Vec<u32> = (0..w).filter(|x| alpha(&buf, w, *x, row) > 0).collect();
    let glyphs: Vec<u32> = (0..w).filter(|x| bright(&buf, w, *x, row)).collect();
    let (edge, label_end) = (
        *painted.last().unwrap() as f32,
        *glyphs.last().unwrap() as f32,
    );
    let label_start = *glyphs
        .iter()
        .find(|x| (**x as f32) > ox + 30.0)
        .expect("the label was not drawn") as f32;
    let block_end = ox + 30.0;
    assert!(
        label_start - block_end >= GAP - 1.0,
        "only {:.1} pt between the block and its label",
        label_start - block_end
    );
    assert!(
        edge - label_end >= PAD - 2.0,
        "only {:.1} pt of padding after the label",
        edge - label_end
    );
    // The glyph stays where it was in the square, so the block reads as stretched, not moved.
    assert!(
        glyphs[0] as f32 > ox && (glyphs[0] as f32) < block_end,
        "the glyph left its block"
    );
}

#[test]
fn the_pop_out_edge_uncovers_the_label() {
    let (w, h) = (1920u32, 1080u32);
    let (fw, fh) = (w as f32, h as f32);
    let mut renderer = Renderer::new();
    let wheel = rail();
    let text = ToolbarAction::Tool(Tool::Text);
    let row = (row_of(&mut renderer, &wheel, fw, fh, text) + 15.0) as u32;
    let edge = |buf: &[u8]| (0..w).rfind(|x| alpha(buf, w, *x, row) > 0).unwrap_or(0);
    let ox = toolbar_bounds(fw, fh).x + 1.0;
    let label_end = |buf: &[u8]| {
        (0..w)
            .rev()
            .find(|x| *x as f32 > ox + 30.0 && bright(buf, w, *x, row))
    };
    let (closed, half, open) = (
        rail_frame(w, h, &wheel, Some(text), 0.0),
        rail_frame(w, h, &wheel, Some(text), 0.5),
        rail_frame(w, h, &wheel, Some(text), 1.0),
    );
    // A block with the pointer on it opens from its own square outwards, and the label appears
    // only where the edge has already passed.
    assert!(
        edge(&closed) < edge(&half) && edge(&half) < edge(&open),
        "the capsule edge went nowhere: {} {} {}",
        edge(&closed),
        edge(&half),
        edge(&open)
    );
    assert!(
        label_end(&closed).is_none(),
        "the label was drawn while closed"
    );
    assert!(
        label_end(&half) < label_end(&open),
        "the label never revealed"
    );
    // The target does not move with the pop-out: the pointer stays on the block, and the room the
    // capsule opens into is already its own, so the hover cannot flicker while it opens.
    for grow in [0.0, 0.5, 1.0] {
        let wheel_open = Toolbar {
            hover: Some(text),
            grow,
            ..wheel
        };
        assert_eq!(
            renderer.toolbar_hit(&wheel_open, fw, fh, ox + 100.0, row as f32),
            Some(text),
            "the block lost the pointer at grow {grow}"
        );
    }
}

#[test]
fn the_pop_out_eases_from_nothing_to_open() {
    use sgraffito::app::{ease_out, hover_grow};
    use std::time::{Duration, Instant};
    let now = Instant::now();
    // No block under the pointer: nothing is drawn stretched, so its progress is complete.
    assert_eq!(hover_grow(None, now), 1.0);
    assert_eq!(hover_grow(Some(now), now), 0.0);
    // Ease-out: past halfway at half the time, and settled once the time is up.
    let half = hover_grow(Some(now - Duration::from_millis(70)), now);
    assert!(half > 0.5 && half < 1.0, "half time gives {half}");
    assert_eq!(hover_grow(Some(now - Duration::from_millis(140)), now), 1.0);
    assert_eq!(hover_grow(Some(now - Duration::from_secs(9)), now), 1.0);
    assert_eq!(ease_out(0.0), 0.0);
    assert_eq!(ease_out(1.0), 1.0);
    assert_eq!(ease_out(-3.0), 0.0);
    assert_eq!(ease_out(7.0), 1.0);
}

#[test]
fn a_label_keeps_aa_contrast_over_any_wallpaper() {
    // The buffer holds one layer, so a pixel over a black wallpaper is the pixel itself and over
    // a white one it is the pixel blended with white. Both have to keep secondary label text
    // above 4.5:1 against the block it sits on.
    fn over(pixel: &[u8], backdrop: f32) -> [f32; 3] {
        let a = pixel[3] as f32 / 255.0;
        [0, 1, 2].map(|i| pixel[i] as f32 + (1.0 - a) * backdrop)
    }
    fn luminance(c: [f32; 3]) -> f32 {
        let channel = |v: f32| {
            let v = v / 255.0;
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(c[0]) + 0.7152 * channel(c[1]) + 0.0722 * channel(c[2])
    }
    fn ratio(a: [f32; 3], b: [f32; 3]) -> f32 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    let (w, h) = (1920u32, 1080u32);
    let (fw, fh) = (w as f32, h as f32);
    let mut renderer = Renderer::new();
    let wheel = rail();
    let text = ToolbarAction::Tool(Tool::Text);
    let row = (row_of(&mut renderer, &wheel, fw, fh, text) + 15.0) as u32;
    let buf = rail_frame(w, h, &wheel, Some(text), 1.0);
    let pixel = |x: u32| -> [u8; 4] {
        let i = ((row * w + x) * 4) as usize;
        [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
    };
    let edge = (0..w).rfind(|x| alpha(&buf, w, *x, row) > 0).unwrap();
    let fill = pixel(edge - 4);
    let glyph = pixel(
        (0..w)
            .rfind(|x| bright(&buf, w, *x, row))
            .expect("no label"),
    );

    for backdrop in [0.0, 128.0, 255.0] {
        let label = ratio(over(&glyph, backdrop), over(&fill, backdrop));
        assert!(
            label >= 4.5,
            "label contrast {label:.2}:1 over a {backdrop} backdrop"
        );
    }
}

/// A frame of one text box being edited, with the given caret position and preedit.
fn edit_frame(w: u32, h: u32, text: &str, cursor: usize, preedit: &str) -> Vec<u8> {
    let mut buf = buffer(w, h);
    let overlay = Overlay {
        text: Some(TextOverlay {
            item: TextItem {
                x: 10.0,
                y: 10.0,
                color: "#ffffff".into(),
                size: 24.0,
                text: text.into(),
            },
            buffer: TextBuffer {
                text: text.into(),
                cursor,
            },
            index: None,
            preedit: preedit.into(),
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
    buf
}

/// Rightmost painted column of a row band.
fn rightmost(buf: &[u8], w: u32, y0: u32, y1: u32) -> u32 {
    (y0..y1)
        .flat_map(|y| (0..w).filter(move |x| alpha(buf, w, *x, y) > 0))
        .max()
        .unwrap_or(0)
}

#[test]
fn the_caret_moves_with_the_cursor() {
    let (w, h) = (200u32, 100u32);
    let (y0, y1) = (10, 10 + 29);
    // "a " ends in a space, so the ink of the text stops at "a": anything past it is the caret.
    let at_end = edit_frame(w, h, "a ", 2, "");
    let at_start = edit_frame(w, h, "a ", 0, "");
    let (end, start) = (
        rightmost(&at_end, w, y0, y1),
        rightmost(&at_start, w, y0, y1),
    );
    assert!(
        end > start + 4,
        "the caret did not move with the cursor: {start} vs {end}"
    );
    // A caret at the start also paints the text's left edge, so nothing is lost either way.
    assert!(alpha(&at_start, w, 10, 20) > 0, "no caret at the start");
}

#[test]
fn the_preedit_renders_at_the_cursor() {
    let (w, h) = (200u32, 100u32);
    let (y0, y1) = (10, 10 + 29);
    // The preedit is underlined, so its row is the widest run of ink in the line box.
    let underline_start = |buf: &[u8]| {
        let mut widest = (0usize, 0u32);
        for y in y0..y1 {
            let painted: Vec<u32> = (0..w).filter(|x| alpha(buf, w, *x, y) > 0).collect();
            if painted.len() > widest.0 {
                widest = (painted.len(), painted[0]);
            }
        }
        widest.1
    };
    let middle = underline_start(&edit_frame(w, h, "ab", 1, "XX"));
    let end = underline_start(&edit_frame(w, h, "ab", 2, "XX"));
    assert!(
        middle < end,
        "the preedit did not render where the caret is: {middle} vs {end}"
    );
    assert!(middle > 10, "the preedit rendered before the text");
}

#[test]
fn a_cjk_click_lands_inside_the_measured_box() {
    // The click that re-enters a box is tested against what the renderer measures, because the
    // font-size estimate the eraser uses is 0.6 em per character: too narrow for full-width
    // scripts, which is why a click on such a glyph used to start a new box instead.
    let item = TextItem {
        x: 10.0,
        y: 10.0,
        color: "#ffffff".into(),
        size: 24.0,
        text: "买牛奶".into(),
    };
    let mut renderer = Renderer::new();
    let measured = renderer.text_edit_bounds(&item.text, &item, 1.0);
    let (w, h) = (200u32, 100u32);
    let mut buf = buffer(w, h);
    renderer.render(
        &mut buf,
        w,
        h,
        1.0,
        &ann(vec![], vec![item.clone()]),
        &Overlay::default(),
        None,
    );
    let painted: Vec<(u32, u32)> = (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .filter(|(x, y)| alpha(&buf, w, *x, *y) > 0)
        .collect();
    assert!(!painted.is_empty());
    for (x, y) in painted {
        assert!(
            measured.contains(x as f32 + 0.5, y as f32 + 0.5),
            "painted pixel {x},{y} is outside the box the click test uses: {measured:?}"
        );
    }
    assert!(
        item.bounds().w < measured.w,
        "the estimate is not narrower here, so this checks nothing"
    );
}
