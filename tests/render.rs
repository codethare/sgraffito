use sgraffito::canvas::{
    Board, BoardRow, Doc, ERASER_THRESHOLD, ItemId, Note, Overlay, PALETTE, Rect, Selection, Shape,
    ShapeKind, Stroke, TextBuffer, TextItem, TextOverlay, Tool, Toolbar, ToolbarAction, Transform,
    View,
};
use sgraffito::render::{
    DAMAGE_SLACK, DamageRegion, FrameCache, Renderer, Screen, buffer_format, rail_column,
    toolbar_bounds, transient_bounds, transient_damage,
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

/// The alpha of the scrim an edit-mode frame is painted on: the surface dims what is behind it, so
/// no pixel of a rail frame is transparent.
const SCRIM: u8 = 0xe6;

/// Whether a pixel carries content rather than only the scrim the whole frame sits on. Every
/// assertion that used to ask "is anything painted here" has to ask this instead, or it would be
/// satisfied by the scrim everywhere and would stop testing anything.
fn content(buf: &[u8], w: u32, x: u32, y: u32) -> bool {
    alpha(buf, w, x, y) > SCRIM
}

fn ink(buf: &[u8]) -> usize {
    buf.as_chunks::<4>().0.iter().filter(|p| p[3] != 0).count()
}

fn ann(strokes: Vec<Stroke>, texts: Vec<TextItem>) -> Board {
    Board {
        strokes,
        texts,
        shapes: vec![],
        notes: vec![],
    }
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
        View::default(),
        &Board::default(),
        &Overlay::default(),
        None,
    );
    assert_eq!(ink(&buf), 0);
}

#[test]
fn committed_stroke_draws_ink_on_the_line_only() {
    let mut buf = buffer(W, H);
    let a = ann(vec![line(50.0, 20.0, 180.0)], vec![]);
    Renderer::new().render(
        &mut buf,
        W,
        H,
        1.0,
        View::default(),
        &a,
        &Overlay::default(),
        None,
    );
    assert!(alpha(&buf, W, 100, 50) > 0);
    assert_eq!(alpha(&buf, W, 100, 10), 0);
    assert_eq!(alpha(&buf, W, 100, 90), 0);
}

#[test]
fn scale_multiplies_coordinates() {
    let (w, h) = (W * 2, H * 2);
    let mut buf = buffer(w, h);
    let a = ann(vec![line(50.0, 20.0, 180.0)], vec![]);
    Renderer::new().render(
        &mut buf,
        w,
        h,
        2.0,
        View::default(),
        &a,
        &Overlay::default(),
        None,
    );
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
    Renderer::new().render(
        &mut buf,
        W,
        H,
        1.0,
        View::default(),
        &a,
        &Overlay::default(),
        None,
    );
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
        View::default(),
        &Board::default(),
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
        View::default(),
        &Board::default(),
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
            note: None,
            preedit: String::new(),
        }),
        ..Default::default()
    };
    Renderer::new().render(
        &mut buf,
        W,
        H,
        1.0,
        View::default(),
        &Board::default(),
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
            note: None,
            preedit: String::new(),
        }),
        ..Default::default()
    };
    Renderer::new().render(
        &mut buf,
        w,
        h,
        1.0,
        View::default(),
        &Board::default(),
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
            note: None,
            preedit: preedit.into(),
        }),
        ..Default::default()
    };
    Renderer::new().render(
        &mut plain,
        W,
        H,
        1.0,
        View::default(),
        &Board::default(),
        &make(""),
        None,
    );
    Renderer::new().render(
        &mut with_preedit,
        W,
        H,
        1.0,
        View::default(),
        &Board::default(),
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
    Renderer::new().render(
        &mut buf,
        120,
        80,
        1.0,
        View::default(),
        &a,
        &Overlay::default(),
        None,
    );
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
    Renderer::new().render(
        &mut buf,
        W,
        H,
        1.0,
        View::default(),
        &a,
        &Overlay::default(),
        None,
    );
    assert!(alpha(&buf, W, 60, 40) > 0);
    assert_eq!(alpha(&buf, W, 60, 50), 0);
}

/// One frame exactly as the daemon composes it: the renderer fills the damaged region from the
/// document cache, draws the overlays, then swaps the R and B bytes of that region.
fn frame(buf: &mut [u8], w: u32, h: u32, ann: &Board, overlay: &Overlay, damage: Option<Rect>) {
    let region = match damage {
        None => DamageRegion::All,
        Some(d) => DamageRegion::from_logical(d, 1.0, w, h),
    };
    Renderer::new().render(buf, w, h, 1.0, View::default(), ann, overlay, damage);
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
        more: false,
        boards_open: false,
        boards: Vec::new(),
        shared: false,
        scrim: true,
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
    let doc = Board::default();
    let with = Overlay {
        toolbar: Some(toolbar(Tool::Text, "#33d17a", 18.0, None)),
        ..Default::default()
    };
    let mut buf = buffer(640, 900);
    Renderer::new().render(&mut buf, 640, 900, 1.0, View::default(), &doc, &with, None);
    assert!(ink(&buf) > 0);

    let mut plain = buffer(640, 900);
    Renderer::new().render(
        &mut plain,
        640,
        900,
        1.0,
        View::default(),
        &doc,
        &Overlay::default(),
        None,
    );
    assert_eq!(ink(&plain), 0);
}

#[test]
fn toolbar_is_a_left_edge_rail_at_the_optical_centre() {
    // Tall enough that the rail is centred rather than pinned: the column is about 780 logical pixels,
    // so 900 no longer leaves the margin above it that the optical centre needs.
    let (w, h) = (640u32, 1000u32);
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
        View::default(),
        &Board::default(),
        &overlay,
        None,
    );
    // The rail hangs on the left edge and fills its box top to bottom: column x 8..44 holds a
    // block on every row of it.
    let band = toolbar_bounds(&rail(), w as f32, h as f32);
    let column: Vec<u32> = (0..h).filter(|y| content(&buf, w, 20, *y)).collect();
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
    // Only the rail carries content: the rest of its band, and the top strip, are the scrim.
    let top_strip_ink = (0..16)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .filter(|(x, y)| content(&buf, w, *x, *y))
        .count();
    assert_eq!(top_strip_ink, 0, "something is painted in the top strip");
    assert!(!content(&buf, w, 200, (band.y + band.h / 2.0) as u32));
    // The rail is essentially opaque in edit mode: the scrim is under everything and the plate is
    // under the blocks, so the block fill's own translucency can no longer be read off a frame. What
    // keeps a label readable over any wallpaper is
    // `a_label_keeps_aa_contrast_over_any_wallpaper`, which composites the label over that fill.
    assert!(
        content(&buf, w, 20, (band.y + band.h - 21.0) as u32),
        "the bottom block is not painted"
    );
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
    let band = toolbar_bounds(toolbar, w, h);
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
    let band = toolbar_bounds(toolbar, w, h);
    // `toolbar_bounds` starts one hairline left of the rail.
    let x = band.x + 1.0 + column_x;
    (0..band.h as u32)
        .map(|step| renderer.toolbar_hit(toolbar, w, h, x, band.y + step as f32 + 0.5))
        .collect()
}

#[test]
fn every_block_of_the_rail_is_reachable_and_no_row_is_a_gap() {
    const HAIRLINE: f32 = 1.0;
    let (w, h) = (1920.0f32, 1080.0f32);
    let rail = rail();
    let mut renderer = Renderer::new();
    let sweep = sweep_rail(&mut renderer, &rail, w, h, 20.0);
    // Only the hairline rows at either end of the box reach no block: the box is the plate, which is
    // the column the hit test covers, plus that hairline. Nothing between them is a gap, or a press on
    // a gap between two blocks would start a stroke under the rail.
    let first = sweep.iter().position(Option::is_some).expect("no rail");
    assert_eq!(first, HAIRLINE as usize, "the rail starts {first} rows in");
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
    for tool in [
        Tool::Pen,
        Tool::Eraser,
        Tool::Text,
        Tool::Selection,
        Tool::Circle,
        Tool::Rect,
        Tool::Diamond,
        Tool::Line,
        Tool::Arrow,
        Tool::Hand,
        Tool::Note,
    ] {
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
    // Twenty-one clickable blocks: five colours, eleven tools, `[`, `]`, the boards' block, the drawer's
    // block and `Esc`. The size readout is drawn between the two size keys but owned by `[`, so it is not
    // one of them.
    assert_eq!(actions.len(), 21, "a drawn block cannot be hit");
    // Outside the rail the toolbar takes nothing: those presses are drawing gestures.
    let band = toolbar_bounds(&rail, w, h);
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
    let band = toolbar_bounds(&rail, fw, fh);
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
        ..rail.clone()
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
        ..rail.clone()
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
            toolbar: Some(Toolbar {
                hover,
                ..rail.clone()
            }),
            ..Default::default()
        };
        Renderer::new().render(
            &mut buf,
            w,
            h,
            1.0,
            View::default(),
            &Board::default(),
            &overlay,
            None,
        );
        buf
    };
    let (square, capsule) = (draw(None), draw(Some(ToolbarAction::Color(0))));
    let strip = |buf: &[u8]| {
        (0..h)
            .flat_map(|y| (50..200).map(move |x| (x, y)))
            .filter(|(x, y)| content(buf, w, *x, *y))
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
            .filter(|(_, pixel)| pixel[3] > SCRIM)
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
    let mut doc = Board::default();
    let band = toolbar_bounds(&rail(), w, h);
    for y in [band.y + 20.0, band.y + band.h / 2.0] {
        assert!(doc.erase(band.x + 20.0, y, ERASER_THRESHOLD).is_none());
    }
}

#[test]
fn damage_region_clears_and_swaps_only_its_rectangle() {
    let (w, h) = (4u32, 2u32);
    let mut buf = vec![9u8; (w * h * 4) as usize];
    let region = DamageRegion::from_logical(
        Rect {
            x: 2.0,
            y: 0.0,
            w: 1.0,
            h: 2.0,
        },
        1.0,
        w,
        h,
    );
    region.clear(&mut buf, w, h);
    // The box, plus `DAMAGE_SLACK` on each side: the hairline an element is drawn with is centred on its
    // own edge, so half of it is outside the box the change reports.
    let (x0, x1) = ((2.0 - DAMAGE_SLACK) as u32, (3.0 + DAMAGE_SLACK) as u32);
    for y in 0..h {
        for x in 0..w {
            let v = buf[((y * w + x) * 4) as usize];
            let inside = x >= x0 && x < x1;
            assert_eq!(v, if inside { 0 } else { 9 }, "pixel {x},{y}");
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
    let rail_box = toolbar_bounds(&rail(), 1024.0, 600.0);
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
        toolbar: Some(rail.clone()),
        ..dot(140.0, 300.0)
    };
    let second = Overlay {
        toolbar: Some(rail.clone()),
        ..dot(144.0, 304.0)
    };
    let damage = transient_damage(
        transient_bounds(&first),
        transient_bounds(&second),
        Some(toolbar_bounds(&rail, w as f32, h as f32)),
    );
    let mut whole = buffer(w, h);
    frame(&mut whole, w, h, &Board::default(), &first, None);
    let mut boxed = whole.clone();
    frame(&mut whole, w, h, &Board::default(), &second, None);
    frame(&mut boxed, w, h, &Board::default(), &second, Some(damage));
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
        toolbar: Some(rail.clone()),
        ..dot(x, y)
    };
    let first = with(500.0, 500.0);
    let second = with(504.0, 504.0);
    let damage = transient_damage(transient_bounds(&first), transient_bounds(&second), None);
    let mut whole = buffer(w, h);
    frame(&mut whole, w, h, &Board::default(), &first, None);
    let mut boxed = whole.clone();
    frame(&mut whole, w, h, &Board::default(), &second, None);
    frame(&mut boxed, w, h, &Board::default(), &second, Some(damage));
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
        View::default(),
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
    doc.boards
        .insert("eDP-1".into(), ann(vec![line(50.0, 20.0, 180.0)], vec![]));
    // Rendering an output reads only that output's bucket; an empty bucket means no annotations
    Renderer::new().render(
        &mut buf,
        W,
        H,
        1.0,
        View::default(),
        &Board::default(),
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
            ..wheel.clone()
        }),
        ..Default::default()
    };
    Renderer::new().render(
        &mut buf,
        w,
        h,
        1.0,
        View::default(),
        &Board::default(),
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
    let ox = toolbar_bounds(&wheel, fw, fh).x + 1.0;
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
    let edge = |buf: &[u8]| (0..w).rfind(|x| content(buf, w, *x, row)).unwrap_or(0);
    let ox = toolbar_bounds(&wheel, fw, fh).x + 1.0;
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
            ..wheel.clone()
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
    // The capsule's own edge, so the fill sampled below is the block the label sits on rather than
    // the scrim the rest of the frame is painted on.
    let edge = (0..w).rfind(|x| content(&buf, w, *x, row)).unwrap();
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
            note: None,
            preedit: preedit.into(),
        }),
        ..Default::default()
    };
    Renderer::new().render(
        &mut buf,
        w,
        h,
        1.0,
        View::default(),
        &Board::default(),
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
    let measured = renderer.text_edit_bounds(
        &item.text,
        &item,
        Screen {
            scale: 1.0,
            view: View::default(),
        },
    );
    let (w, h) = (200u32, 100u32);
    let mut buf = buffer(w, h);
    renderer.render(
        &mut buf,
        w,
        h,
        1.0,
        View::default(),
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

#[test]
fn the_rail_folds_to_fit_a_short_output_and_is_pinned_only_below_the_foot() {
    // The rail has twenty-two blocks, about 780 logical pixels, and the palette, the tools and the size group
    // fold into the drawer until the foot — boards, more, Esc — fits. So a short output loses none of the
    // controls and none of the rail: the column is only ever longer than the output when even the foot cannot
    // fit, and then its top is the margin rather than the whole rail sliding off the top.
    let rail = rail();
    let (w, tall) = (640u32, 1600u32);
    let roomy = toolbar_bounds(&rail, w as f32, tall as f32);
    assert!(
        roomy.y > 8.0,
        "a tall output centres the rail instead of pinning it: {roomy:?}"
    );

    let (short_w, short_h) = (640u32, 500u32);
    let short = toolbar_bounds(&rail, short_w as f32, short_h as f32);
    assert!(
        rail_column(&rail, short_h as f32) < rail_column(&rail, tall as f32),
        "a short output's rail is as tall as a tall one's"
    );
    assert!(
        short.y + short.h <= short_h as f32,
        "the folded rail is still taller than the output: {short:?}"
    );
    assert!(
        short.h < roomy.h,
        "the rail did not fold: {short:?} against {roomy:?}"
    );

    // Below the foot's own height there is nothing left to fold, and the rail is pinned.
    let (tiny_w, tiny_h) = (640u32, 100u32);
    let pinned = toolbar_bounds(&rail, tiny_w as f32, tiny_h as f32);
    assert_eq!(
        pinned.y,
        8.0 - 1.0,
        "the top is the margin, hairline included"
    );
    assert!(
        pinned.y + pinned.h > tiny_h as f32,
        "the rail is taller than the room the output has for it"
    );

    // What is drawn agrees with that box, and on the short output the foot is inside the output.
    let mut buf = buffer(short_w, short_h);
    let overlay = Overlay {
        toolbar: Some(toolbar(Tool::Pen, "#33d17a", 3.0, None)),
        ..Default::default()
    };
    Renderer::new().render(
        &mut buf,
        short_w,
        short_h,
        1.0,
        View::default(),
        &Board::default(),
        &overlay,
        None,
    );
    let column: Vec<u32> = (0..short_h)
        .filter(|y| content(&buf, short_w, 20, *y))
        .collect();
    let (top, bottom) = (
        *column.first().expect("the rail is painted"),
        *column.last().expect("the rail is painted"),
    );
    assert!(
        top as f32 >= short.y && top as f32 <= short.y + 8.0,
        "the rail starts where its box does: {top} against {short:?}"
    );
    assert!(
        (bottom as f32) < short_h as f32 - 8.0,
        "the foot of the rail is off the output: {bottom} of {short_h}"
    );
}

#[test]
fn the_rail_sits_on_a_plate() {
    // The plate is what makes the rail read as one control over any wallpaper: black at 90%
    // opacity, behind every block, inset by TOOLBAR_PAD, and inside the box the rail declares as
    // its own — a plate's margin outside that box would be left unrepainted by a rail-only frame.
    const PAD: f32 = 6.0;
    const BLOCK: f32 = 30.0;
    const HAIRLINE: f32 = 1.0;
    let (w, h) = (640u32, 900u32);
    let mut buf = buffer(w, h);
    let mut renderer = Renderer::new();
    let rail = rail();
    let overlay = Overlay {
        toolbar: Some(rail.clone()),
        ..Default::default()
    };
    renderer.render(
        &mut buf,
        w,
        h,
        1.0,
        View::default(),
        &Board::default(),
        &overlay,
        None,
    );
    let band = toolbar_bounds(&rail, w as f32, h as f32);
    // The plate reaches a pad left of the blocks and starts at their column's top; the declared box
    // is that plus the hairline.
    let (ox, oy) = (band.x + PAD + HAIRLINE, band.y + HAIRLINE);
    let (left, top) = (ox - PAD, oy);
    assert_eq!(
        (band.x, band.y),
        (left - HAIRLINE, top - HAIRLINE),
        "the declared box does not hold the plate"
    );

    // The plate's own margin is painted: this is the strip that has to be inside the damage box,
    // because a frame that repaints only the rail is what draws it.
    // Sampled in the middle of the top margin and of the left margin, because the plate's rounded
    // corners are cut away and a corner pixel would prove nothing.
    assert!(
        content(&buf, w, (ox + BLOCK / 2.0) as u32, (top + 1.0) as u32),
        "no plate in its top margin"
    );
    assert!(
        content(&buf, w, (left + 1.0) as u32, (oy + band.h / 2.0) as u32),
        "no plate in its left margin"
    );

    // A gap between two blocks is the plate rather than the wallpaper: painted, and black.
    let gap_y = (oy + 5.0 * BLOCK + 4.0 * 4.0 + 10.0 + 2.0) as u32;
    let (x0, x1) = ((ox + 2.0) as u32, (ox + BLOCK - 2.0) as u32);
    let mut brightest = 0;
    for x in x0..x1 {
        assert!(
            alpha(&buf, w, x, gap_y) > 0,
            "the gap shows the wallpaper at {x}"
        );
        let i = ((gap_y * w + x) * 4) as usize;
        brightest = brightest.max(buf[i] as u32 + buf[i + 1] as u32 + buf[i + 2] as u32);
    }
    assert!(brightest <= 40, "the plate is not black: {brightest}");

    // The plate is no wider than the block column while a capsule reached past it: the widest
    // block the rail can open is the text tool's, and it is drawn over the wallpaper beyond the
    // plate rather than the plate appearing to grow with it.
    let hole = ToolbarAction::Tool(Tool::Text);
    let stretched = toolbar(Tool::Pen, PALETTE[0], 3.0, Some(hole));
    let mut renderer = Renderer::new();
    let row = row_of(&mut renderer, &stretched, w as f32, h as f32, hole);
    let mut wide = buffer(w, h);
    renderer.render(
        &mut wide,
        w,
        h,
        1.0,
        View::default(),
        &Board::default(),
        &Overlay {
            toolbar: Some(stretched),
            ..Default::default()
        },
        None,
    );
    let plate_right = ox + BLOCK + PAD;
    let beyond = (plate_right + 6.0) as u32;
    // In the plate's own margin, nothing is painted past the plate: it did not grow.
    assert!(
        !content(&wide, w, beyond, (top + 1.0) as u32),
        "the plate grew to the width of the capsule"
    );
    // In the capsule's own row the same column does carry ink, which is the capsule and not the plate.
    // `row_of` answers in absolute coordinates, like the box it sweeps.
    assert!(
        content(&wide, w, beyond, (row + BLOCK / 2.0) as u32),
        "the capsule did not reach past the plate"
    );
}

#[test]
fn an_edit_mode_frame_is_painted_on_a_scrim() {
    // The scrim is what dims the windows behind the surface while editing: the whole output carries
    // it, under the annotations and the rail. Locked mode paints none of it, because that surface
    // sits under other windows and would dim only the wallpaper behind the annotations.
    const LOCKED_MESSAGE: &str = "locked mode paints no scrim";
    let (w, h) = (640u32, 900u32);
    let mut editor = Renderer::new();
    let rail = rail();
    let edit = Overlay {
        toolbar: Some(rail.clone()),
        ..Default::default()
    };
    let locked = Overlay::default();
    let mut editing = buffer(w, h);
    editor.render(
        &mut editing,
        w,
        h,
        1.0,
        View::default(),
        &Board::default(),
        &edit,
        None,
    );
    let mut idle = buffer(w, h);
    editor.render(
        &mut idle,
        w,
        h,
        1.0,
        View::default(),
        &Board::default(),
        &locked,
        None,
    );

    // Every corner of the output, and the far side of it, away from the rail: the scrim and nothing
    // else.
    for (x, y) in [
        (1u32, 1u32),
        (w - 2, 1),
        (1, h - 2),
        (w - 2, h - 2),
        (w / 2, h / 2),
        (w - 40, h / 3),
    ] {
        assert_eq!(alpha(&editing, w, x, y), SCRIM, "no scrim at {x},{y}");
        assert_eq!(alpha(&idle, w, x, y), 0, "{LOCKED_MESSAGE} at {x},{y}");
    }
    let band = toolbar_bounds(&rail, w as f32, h as f32);
    assert!(content(&editing, w, (band.x + 20.0) as u32, 200u32));
}

#[test]
fn a_cached_frame_keeps_the_scrim_after_a_document_change() {
    // The scrim is the raster's background, so the frame that rebuilds the raster after a document
    // change has to paint it again: a frame left transparent where the document is empty would show
    // the windows the scrim exists to dim.
    let (w, h) = (400u32, 300u32);
    let mut renderer = Renderer::new();
    let mut cache = FrameCache::default();
    let rail = rail();
    let overlay = Overlay {
        toolbar: Some(rail.clone()),
        ..Default::default()
    };
    let before = Board::default();
    let mut after = before.clone();
    after.strokes = vec![Stroke {
        color: "#e01b24".into(),
        width: 4.0,
        points: vec![[20.0, 20.0], [120.0, 60.0]],
    }];
    let mut first = buffer(w, h);
    renderer.render_with_cache(
        &mut first,
        w,
        h,
        1.0,
        1.0,
        View::default(),
        &before,
        &overlay,
        None,
        &mut cache,
        &[],
    );
    cache.invalidate();
    let mut second = buffer(w, h);
    renderer.render_with_cache(
        &mut second,
        w,
        h,
        1.0,
        1.0,
        View::default(),
        &after,
        &overlay,
        None,
        &mut cache,
        &[],
    );
    // A fresh render of the new document, so the two paths are compared rather than assumed equal.
    let mut fresh = buffer(w, h);
    Renderer::new().render(
        &mut fresh,
        w,
        h,
        1.0,
        View::default(),
        &after,
        &overlay,
        None,
    );
    assert_eq!(second, fresh);
    let (x, y) = (w - 20, h - 20);
    assert_eq!(
        alpha(&second, w, x, y),
        SCRIM,
        "the rebuilt raster lost the scrim"
    );
}

#[test]
fn a_panned_and_zoomed_frame_puts_the_drawing_where_the_view_says() {
    // The whole point of the view: a canvas point lands at the screen point the transform names, and
    // the rail stays exactly where it was, because it is a control rather than part of the drawing.
    let (w, h) = (400u32, 300u32);
    let doc = Board {
        strokes: vec![Stroke {
            color: "#e01b24".into(),
            width: 4.0,
            points: vec![[100.0, 100.0], [140.0, 100.0]],
        }],
        texts: vec![],
        shapes: vec![],
        notes: vec![],
    };
    let view = View {
        zoom: 2.0,
        offset: [30.0, -10.0],
    };
    let rail = rail();
    let overlay = Overlay {
        toolbar: Some(rail.clone()),
        ..Default::default()
    };
    let mut buf = buffer(w, h);
    Renderer::new().render(&mut buf, w, h, 1.0, view, &doc, &overlay, None);
    // The stroke's canvas point (100,100) is at (230,190) on screen: red there, and not where it was.
    let red_at = |x: u32, y: u32| {
        let i = ((y * w + x) * 4) as usize;
        buf[i] > 120 && buf[i + 1] < 80 && buf[i + 2] < 80
    };
    assert!(red_at(230, 190), "the stroke is not where the view puts it");
    assert!(
        !red_at(100, 100),
        "the stroke is still where the canvas had it"
    );
    // The rail is in the same place it is at 1:1: the block column's first row is painted there.
    let (ox, oy) = (
        toolbar_bounds(&rail, w as f32, h as f32).x + 6.0 + 1.0,
        toolbar_bounds(&rail, w as f32, h as f32).y + 1.0,
    );
    assert!(
        content(&buf, w, (ox + 15.0) as u32, (oy + 20.0) as u32),
        "the rail moved with the view"
    );
}

#[test]
fn the_default_view_draws_what_no_view_drew() {
    // A frame at 1:1 with no pan has to be the frame this program drew before the view existed,
    // down to the byte: that is what keeps this change from moving a pixel it does not mean to.
    let (w, h) = (400u32, 300u32);
    let doc = Board {
        strokes: vec![Stroke {
            color: "#e01b24".into(),
            width: 4.0,
            points: vec![[40.0, 40.0], [180.0, 90.0]],
        }],
        texts: vec![TextItem {
            x: 40.0,
            y: 140.0,
            color: "#ffffff".into(),
            size: 20.0,
            text: "milk".into(),
        }],
        shapes: vec![Shape {
            kind: ShapeKind::Circle,
            color: "#33d17a".into(),
            width: 3.0,
            from: [200.0, 150.0],
            to: [300.0, 250.0],
        }],
        notes: vec![],
    };
    let overlay = Overlay {
        toolbar: Some(rail()),
        ..Default::default()
    };
    let mut plain = buffer(w, h);
    Renderer::new().render(&mut plain, w, h, 1.0, View::default(), &doc, &overlay, None);
    let mut cached = buffer(w, h);
    Renderer::new().render_with_cache(
        &mut cached,
        w,
        h,
        1.0,
        1.0,
        View::default(),
        &doc,
        &overlay,
        None,
        &mut FrameCache::default(),
        &[],
    );
    assert_eq!(plain, cached);
}

#[test]
fn a_note_draws_its_text_inside_its_square_and_nowhere_else() {
    // The user's rule for a sticky note: the text never leaves the square, it wraps instead. Comparing
    // the same note with and without text shows exactly where the text went, whatever colours it uses.
    let (w, h) = (600u32, 400u32);
    let view = View::default();
    let text = "buy milk and bread and eggs and coffee and tea and rice and beans and more";
    let mut note = Note::new(60.0, 40.0, PALETTE[1], "2026/10/14".into());
    note.w = 200.0;
    let mut renderer = Renderer::new();
    // Grow the square the way the daemon does, then render it.
    let needed = renderer.wrapped_text_height(text, &note, Screen { scale: 1.0, view });
    note.h = note.height_for(needed);
    note.text = text.into();
    let mut empty = note.clone();
    empty.text = String::new();
    empty.h = note.h;
    let draw = |note: &Note| {
        let ann = Board {
            strokes: vec![],
            texts: vec![],
            shapes: vec![],
            notes: vec![note.clone()],
        };
        let mut buf = buffer(w, h);
        Renderer::new().render(&mut buf, w, h, 1.0, view, &ann, &Overlay::default(), None);
        buf
    };
    let (with, without) = (draw(&note), draw(&empty));
    let square = note.bounds().grown(1.5);
    let mut outside = 0;
    for (i, (a, b)) in with.chunks(4).zip(without.chunks(4)).enumerate() {
        if a == b {
            continue;
        }
        let (x, y) = ((i as u32 % w) as f32, (i as u32 / w) as f32);
        if !square.contains(x, y) {
            outside += 1;
        }
    }
    assert_eq!(outside, 0, "the note's text was drawn outside its square");

    // Wrapping means the text needs more than one line: a wide note is shorter than a narrow one with
    // the same words, and both are taller than a single line.
    let mut narrow = note.clone();
    narrow.w = 90.0;
    let mut wide = note.clone();
    wide.w = 400.0;
    let mut renderer = Renderer::new();
    let line = renderer.wrapped_text_height("one line", &note, Screen { scale: 1.0, view });
    let tall = renderer.wrapped_text_height(text, &narrow, Screen { scale: 1.0, view });
    let flat = renderer.wrapped_text_height(text, &wide, Screen { scale: 1.0, view });
    assert!(
        tall > flat,
        "a narrow note wraps into more lines: {tall} vs {flat}"
    );
    assert!(flat > line, "the same words need more than one line");

    // The square is really painted: its middle is the note's colour and not the scrim behind it.
    let i = (((note.y + note.h / 2.0) as u32 * w) + (note.x + note.w - 4.0) as u32) as usize * 4;
    let note_colour = sgraffito::canvas::parse_hex(&note.color);
    assert!(
        with[i] > note_colour[0].saturating_sub(40),
        "the note's fill is missing: {:?}",
        &with[i..i + 4]
    );
}

#[test]
fn the_drawer_opens_beside_the_rail_without_moving_it() {
    // The point of a drawer: a control can live somewhere other than the rail, and the rail does not
    // grow to hold it. Its height, its order and its own box are the same either way.
    let (w, h) = (1920u32, 1080u32);
    let (fw, fh) = (w as f32, h as f32);
    let mut renderer = Renderer::new();
    let closed = rail();
    let open = Toolbar {
        more: true,
        ..closed.clone()
    };
    // The rail's height and its declared box are unchanged by the drawer.
    let (a, b) = (toolbar_bounds(&open, fw, fh), toolbar_bounds(&open, fw, fh));
    assert_eq!(a.h, b.h);
    // The rail's own blocks are where they were: the block that opens the drawer is at the foot of the
    // rail, and `Esc` is still last.
    let sweep = sweep_rail(&mut renderer, &closed, fw, fh, 20.0);
    let mut actions: Vec<ToolbarAction> = Vec::new();
    for action in sweep.iter().flatten() {
        if actions.last() != Some(action) {
            actions.push(*action);
        }
    }
    assert_eq!(
        *actions.last().unwrap(),
        ToolbarAction::Lock,
        "Esc is still last"
    );
    assert!(
        actions.contains(&ToolbarAction::More),
        "the drawer's own block is on the rail"
    );
    assert!(
        !actions.contains(&ToolbarAction::Scrim),
        "the backdrop's switch is not on the rail while the drawer is closed"
    );

    // Open, the switch is a row of the panel in the second column, and exactly one row of that panel is
    // level with the block that opened it — the row the corridor leads into, so the pointer can walk from
    // the block straight onto the list.
    let band = toolbar_bounds(&open, fw, fh);
    let column_x = band.x + 1.0 + 30.0 + 4.0 + 15.0;
    let mut more_row = None;
    for step in 1..band.h as u32 {
        let y = band.y + step as f32;
        if renderer.toolbar_hit(&open, fw, fh, band.x + 1.0 + 15.0, y) == Some(ToolbarAction::More)
        {
            more_row = Some(y);
            break;
        }
    }
    let more_row = more_row.expect("the more block is on the rail");
    let panel_row = (1..band.h as u32)
        .map(|step| band.y + step as f32)
        .find(|y| renderer.toolbar_hit(&open, fw, fh, column_x, *y) == Some(ToolbarAction::Scrim))
        .expect("the panel's row is not reachable at all");
    assert!(
        (more_row..more_row + 30.0).contains(&panel_row),
        "the panel's row {panel_row} is not level with the block's {more_row}"
    );
    // The corridor reaches exactly to the panel's edge: one step left of the panel's first column, on the
    // block's own row, is still the block, so the pointer travels from the block into the panel without
    // crossing a gap that owns nothing and would close it. Both ends come from the same layout.
    let panel_left = (1..band.w as u32)
        .map(|step| band.x + step as f32)
        .find(|x| renderer.toolbar_hit(&open, fw, fh, *x, panel_row) == Some(ToolbarAction::Scrim))
        .expect("the panel's left edge");
    assert_eq!(
        renderer.toolbar_hit(&open, fw, fh, panel_left - 1.0, more_row + 5.0),
        Some(ToolbarAction::More)
    );
    // Nothing of the drawer is at the top of its column: the list hangs off the block, not the rail.
    assert_eq!(
        renderer.toolbar_hit(&open, fw, fh, column_x, band.y + 1.0 + 15.0),
        None
    );
    // A press in the same row of the rail is the rail's own block, not the drawer's.
    assert_eq!(
        renderer.toolbar_hit(&open, fw, fh, band.x + 1.0 + 15.0, more_row),
        Some(ToolbarAction::More)
    );
    // And a row below the drawer's only control is canvas, not a control: the drawer's column is only
    // as tall as what it holds.
    assert_eq!(
        renderer.toolbar_hit(&open, fw, fh, column_x, band.y + 1.0 + 200.0),
        None
    );
}

#[test]
fn the_backdrop_can_be_switched_off_and_on() {
    // A preference about reading, not content: switching it changes every pixel of the surface and
    // nothing in the document, and the raster cannot hold the state it was switched away from.
    let (w, h) = (400u32, 300u32);
    let mut renderer = Renderer::new();
    let on = Overlay {
        toolbar: Some(rail()),
        ..Default::default()
    };
    let off = Overlay {
        toolbar: Some(Toolbar {
            scrim: false,
            ..rail()
        }),
        ..Default::default()
    };
    let render = |overlay: &Overlay| {
        let mut buf = buffer(w, h);
        Renderer::new().render(
            &mut buf,
            w,
            h,
            1.0,
            View::default(),
            &Board::default(),
            overlay,
            None,
        );
        buf
    };
    let (with, without) = (render(&on), render(&off));
    // Away from the rail, the frame with it is the scrim and the frame without it is transparent.
    let (x, y) = (w - 20, h - 20);
    assert_eq!(alpha(&with, w, x, y), SCRIM, "the backdrop is not painted");
    assert_eq!(
        alpha(&without, w, x, y),
        0,
        "the backdrop was painted anyway"
    );
    // The rail itself is drawn either way, so it is not the backdrop that makes it visible.
    let band = toolbar_bounds(&rail(), w as f32, h as f32);
    assert!(content(
        &with,
        w,
        (band.x + 20.0) as u32,
        (band.y + 20.0) as u32
    ));
    assert!(content(
        &without,
        w,
        (band.x + 20.0) as u32,
        (band.y + 20.0) as u32
    ));

    // The raster is keyed by the backdrop, so a frame that switches it cannot reuse the other state.
    let mut cache = FrameCache::default();
    let mut first = buffer(w, h);
    renderer.render_with_cache(
        &mut first,
        w,
        h,
        1.0,
        1.0,
        View::default(),
        &Board::default(),
        &on,
        None,
        &mut cache,
        &[],
    );
    let mut second = buffer(w, h);
    renderer.render_with_cache(
        &mut second,
        w,
        h,
        1.0,
        1.0,
        View::default(),
        &Board::default(),
        &off,
        None,
        &mut cache,
        &[],
    );
    assert_eq!(
        second, without,
        "the switched frame did not match a fresh one"
    );
    assert_eq!(alpha(&second, w, x, y), 0);
}

#[test]
fn a_note_being_typed_in_is_drawn_wrapped_while_the_typing_goes_on() {
    // The live path, which is the one the user is in front of: the note under edit is drawn by the overlay
    // with the buffer's text wrapped at its own width, and it is left out of the raster so the text that
    // was committed before cannot show through it.
    let (w, h) = (600u32, 500u32);
    let view = View::default();
    let screen = Screen { scale: 1.0, view };
    let long = "buy milk and bread and eggs and coffee and tea and rice and beans";
    let mut note = Note::new(60.0, 40.0, PALETTE[1], "2026/10/14".into());
    note.w = 200.0;
    let mut renderer = Renderer::new();
    let needed = renderer.wrapped_text_height(long, &note, screen);
    note.h = note.height_for(needed);
    let mut empty = note.clone();
    empty.h = note.h;
    // The note as the document holds it while the typing is going on: it still has no text, because the
    // text lives in the overlay's buffer until the edit ends.
    let doc = |note: &Note| Board {
        strokes: vec![],
        texts: vec![],
        shapes: vec![],
        notes: vec![note.clone()],
    };
    let live = Overlay {
        text: Some(TextOverlay {
            item: note.text_item(),
            buffer: TextBuffer::new(long),
            index: None,
            note: Some(0),
            preedit: String::new(),
        }),
        ..Default::default()
    };
    let draw = |ann: &Board, overlay: &Overlay| {
        let mut buf = buffer(w, h);
        Renderer::new().render(&mut buf, w, h, 1.0, view, ann, overlay, None);
        buf
    };
    let (typed, blank) = (
        draw(&doc(&empty), &live),
        draw(&doc(&empty), &Overlay::default()),
    );
    // Whatever the live text draws, it is inside the note's square — the same rule as the committed text,
    // so a note cannot look right only after `Esc`.
    let square = note.bounds().grown(1.5);
    let mut outside = 0;
    for (i, (a, b)) in typed.chunks(4).zip(blank.chunks(4)).enumerate() {
        if a == b {
            continue;
        }
        let (x, y) = ((i as u32 % w) as f32, (i as u32 / w) as f32);
        if !square.contains(x, y) {
            outside += 1;
        }
    }
    assert_eq!(outside, 0, "the live text was drawn outside the note");

    // And the note under edit is not in the raster: a frame drawn from the raster alone has its square
    // blank, so the overlay's copy is the only one on screen.
    let mut cache = FrameCache::default();
    let mut only_raster = buffer(w, h);
    renderer.render_with_cache(
        &mut only_raster,
        w,
        h,
        1.0,
        1.0,
        view,
        &doc(&empty),
        &Overlay::default(),
        None,
        &mut cache,
        &[ItemId::Note(0)],
    );
    let empty_frame = draw(&Board::default(), &Overlay::default());
    assert_eq!(
        only_raster, empty_frame,
        "the note under edit was left in the raster"
    );
}

#[test]
fn a_note_is_drawn_over_the_ink_that_crosses_it() {
    // A note is what the drawing is read against, so no tool can bury it: a stroke across a note is
    // under it in the frame, which is also why the note is what the pointer hits there.
    let (w, h) = (600u32, 500u32);
    let view = View::default();
    let note = Note::new(80.0, 60.0, PALETTE[1], "2026/10/14".into());
    let stroke = Stroke {
        color: PALETTE[0].to_string(),
        width: 6.0,
        points: vec![[40.0, 110.0], [300.0, 110.0]],
    };
    let with_ink = Board {
        strokes: vec![stroke],
        texts: vec![],
        shapes: vec![],
        notes: vec![note.clone()],
    };
    let mut inkless = with_ink.clone();
    inkless.strokes.clear();
    let draw = |ann: &Board| {
        let mut buf = buffer(w, h);
        Renderer::new().render(&mut buf, w, h, 1.0, view, ann, &Overlay::default(), None);
        buf
    };
    let (ink, bare) = (draw(&with_ink), draw(&inkless));
    // The stroke reaches out of the note on both sides and that is the only place it may be seen: inside
    // the square the two frames agree, so the note is over the ink rather than the ink over the note.
    let mut seen = 0;
    // Inset by a pixel: the note's own edge is anti-aliased, so its outermost pixels may hold some of the
    // ink under them. Deeper inside, the note covers.
    let b = note.bounds();
    let square = Rect {
        x: b.x + 2.5,
        y: b.y + 2.5,
        w: b.w - 5.0,
        h: b.h - 5.0,
    };
    for (i, (a, b)) in ink.chunks(4).zip(bare.chunks(4)).enumerate() {
        if a == b {
            continue;
        }
        let (x, y) = ((i as u32 % w) as f32, (i as u32 / w) as f32);
        assert!(
            !square.contains(x, y),
            "the ink was drawn over the note at {x},{y}"
        );
        seen += 1;
    }
    assert!(seen > 0, "the stroke was not drawn at all");
}

#[test]
fn the_date_stays_inside_the_note_at_every_zoom() {
    // The date is drawn at the interface's own size and does not zoom, but it is placed at the note's
    // canvas-space bottom-right corner. Its measured width was mixed with that canvas box, so at any zoom
    // but 1:1 it walked out of the note and hung off its edge.
    let (w, h) = (900u32, 700u32);
    let mut bands: Vec<f32> = Vec::new();
    for zoom in [0.6f32, 1.0, 1.75, 2.5] {
        let view = View {
            zoom,
            offset: [0.0, 0.0],
        };
        let note = Note::new(80.0, 60.0, PALETTE[1], "2026/10/14".into());
        let mut dateless = note.clone();
        dateless.created = String::new();
        let with_date = Board {
            strokes: vec![],
            texts: vec![],
            shapes: vec![],
            notes: vec![note.clone()],
        };
        let mut without = with_date.clone();
        without.notes = vec![dateless];
        let draw = |ann: &Board| {
            let mut buf = buffer(w, h);
            Renderer::new().render(&mut buf, w, h, 1.0, view, ann, &Overlay::default(), None);
            buf
        };
        let (dated, bare) = (draw(&with_date), draw(&without));
        let square = view.screen_rect(note.bounds());
        let mut seen = 0;
        let mut band = (f32::MAX, 0.0f32);
        for (i, (a, b)) in dated.chunks(4).zip(bare.chunks(4)).enumerate() {
            if a == b {
                continue;
            }
            let (x, y) = ((i as u32 % w) as f32, (i as u32 / w) as f32);
            assert!(
                square.contains(x, y),
                "the date at {x},{y} is outside the note at zoom {zoom}"
            );
            band = (band.0.min(y), band.1.max(y));
            seen += 1;
        }
        assert!(seen > 0, "the date was not drawn at all at zoom {zoom}");
        bands.push(band.1 - band.0);
    }
    // And the date is the size of the note it belongs to: twice the zoom, about twice the glyph band.
    assert!(
        bands[3] > bands[1] * 2.0 && bands[0] < bands[1],
        "the date does not follow the zoom: {bands:?}"
    );
}

#[test]
fn a_note_being_dragged_out_is_filled_and_not_only_outlined() {
    let (w, h) = (600u32, 500u32);
    let view = View::default();
    let note = Note::new(80.0, 60.0, PALETTE[1], "2026/10/14".into());
    let overlay = Overlay {
        note: Some(note.clone()),
        ..Default::default()
    };
    let mut buf = buffer(w, h);
    Renderer::new().render(&mut buf, w, h, 1.0, view, &Board::default(), &overlay, None);
    let px = |x: u32, y: u32| {
        let p = buf.chunks(4).nth((y * w + x) as usize).unwrap();
        (p[0], p[1], p[2], p[3])
    };
    let fill = sgraffito::canvas::parse_hex(&note.color);
    assert_eq!(
        px(170, 120),
        (fill[0], fill[1], fill[2], fill[3]),
        "the live note has no fill"
    );
    assert_eq!(px(5, 5).3, 0, "the live note painted outside itself");
}

#[test]
fn a_note_dragged_left_leaves_no_trail_in_its_damage_box() {
    // The frame that follows a note's move repaints the union of the two boxes the note occupied, and the
    // rest of the surface keeps the previous frame's pixels. If that union misses any of the old note, the
    // old pixels survive as a trail — which is what dragging a note leftwards showed.
    let (w, h) = (900u32, 700u32);
    for zoom in [1.0f32, 2.0] {
        let view = View {
            zoom,
            offset: [0.0, 0.0],
        };
        let at = |x: f32| Overlay {
            note: Some(Note::new(x, 60.0, PALETTE[1], "2026/10/14".into())),
            ..Default::default()
        };
        let (far, near) = (at(500.0), at(120.0));
        let box_ = |overlay: &Overlay| transient_bounds(overlay).map(|b| view.screen_rect(b));
        let damage = transient_damage(box_(&far), box_(&near), None);
        let mut cache = FrameCache::default();
        let mut renderer = Renderer::new();
        let mut buf = buffer(w, h);
        renderer.render_with_cache(
            &mut buf,
            w,
            h,
            1.0,
            1.0,
            view,
            &Board::default(),
            &far,
            None,
            &mut cache,
            &[],
        );
        renderer.render_with_cache(
            &mut buf,
            w,
            h,
            1.0,
            1.0,
            view,
            &Board::default(),
            &near,
            Some(damage),
            &mut cache,
            &[],
        );
        let mut full = buffer(w, h);
        Renderer::new().render(&mut full, w, h, 1.0, view, &Board::default(), &near, None);
        let mut spots: Vec<(u32, u32)> = Vec::new();
        for (i, (a, b)) in buf.chunks(4).zip(full.chunks(4)).enumerate() {
            if a != b {
                spots.push(((i as u32) % w, (i as u32) / w));
            }
        }
        assert!(
            spots.is_empty(),
            "the note left a trail at zoom {zoom}: {spots:?}"
        );
    }
}

#[test]
fn a_note_dragged_out_and_back_leaves_no_trail() {
    // The whole gesture, frame by frame into one buffer, each frame damaging the union of the box the note
    // was in and the box it is in now — the app's rule. Dragging out and then pulling back in (so the note
    // shrinks, and the corner the sweep turns about changes when the pointer crosses the press) showed a
    // trail on both sides.
    let (w, h) = (900u32, 700u32);
    let view = View::default();
    let origin = [500.0f32, 400.0f32];
    let path: [[f32; 2]; 9] = [
        [700.0, 600.0],
        [760.0, 640.0],
        [700.0, 600.0],
        [560.0, 500.0],
        [500.0, 420.0],
        [430.0, 380.0],
        [380.0, 350.0],
        [470.0, 390.0],
        [620.0, 560.0],
    ];
    let mut last: Option<Rect> = None;
    let mut cache = FrameCache::default();
    let mut renderer = Renderer::new();
    let mut buf = buffer(w, h);
    let mut last_overlay = Overlay::default();
    for at in path {
        let Some(swept) = sgraffito::canvas::note_drag(origin, at) else {
            continue;
        };
        let mut note = Note::new(0.0, 0.0, PALETTE[1], "2026/10/14".into());
        note.x = swept.x;
        note.y = swept.y;
        note.w = swept.w;
        note.h = swept.h;
        let overlay = Overlay {
            note: Some(note.clone()),
            ..Default::default()
        };
        let now = transient_bounds(&overlay).map(|b| view.screen_rect(b));
        let damage = transient_damage(last, now, None);
        renderer.render_with_cache(
            &mut buf,
            w,
            h,
            1.0,
            1.0,
            view,
            &Board::default(),
            &overlay,
            Some(damage),
            &mut cache,
            &[],
        );
        last = now;
        last_overlay = overlay;
    }
    let mut full = buffer(w, h);
    Renderer::new().render(
        &mut full,
        w,
        h,
        1.0,
        view,
        &Board::default(),
        &last_overlay,
        None,
    );
    let spots: Vec<(u32, u32)> = buf
        .chunks(4)
        .zip(full.chunks(4))
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(i, _)| ((i as u32) % w, (i as u32) / w))
        .collect();
    assert!(
        spots.is_empty(),
        "the note trailed {} pixels, first {:?} last {:?}",
        spots.len(),
        spots.first(),
        spots.last()
    );
}

#[test]
fn switching_the_backdrop_repaints_the_whole_surface() {
    // The scrim is the raster's own background, so a frame that follows the switch has to be a
    // whole-surface one: a partial frame would leave every pixel outside its box with the backing the
    // surface had before, which is what "only the area around the rail changed" was.
    let (w, h) = (600u32, 500u32);
    let view = View::default();
    let rail = |scrim: bool| Toolbar {
        scrim,
        ..toolbar(Tool::Pen, PALETTE[0], 3.0, None)
    };
    let draw = |buf: &mut [u8], cache: &mut FrameCache, scrim: bool| {
        Renderer::new().render_with_cache(
            buf,
            w,
            h,
            1.0,
            1.0,
            view,
            &Board::default(),
            &Overlay {
                toolbar: Some(rail(scrim)),
                ..Default::default()
            },
            None,
            cache,
            &[],
        );
    };
    let mut cache = FrameCache::default();
    let mut buf = buffer(w, h);
    draw(&mut buf, &mut cache, true);
    let alpha = |b: &[u8], x: u32, y: u32| b[((y * w + x) * 4 + 3) as usize];
    // The scrim is black at 90%, so a pixel on it is nearly opaque everywhere, well away from the rail.
    assert!(alpha(&buf, 400, 400) > 200, "the scrim is not painted");
    draw(&mut buf, &mut cache, false);
    for (x, y) in [(400, 400), (400, 100), (550, 480), (200, 250)] {
        assert_eq!(
            alpha(&buf, x, y),
            0,
            "the scrim's own pixels at {x},{y} were not repainted"
        );
    }
}

#[test]
fn hovering_the_more_block_paints_the_drawer_list_inside_the_rails_box() {
    // Tall enough for the whole rail and the legend under it: the `more` block is near its bottom.
    let (w, h) = (600u32, 900u32);
    let view = View::default();
    let rail = |hover: Option<ToolbarAction>| toolbar(Tool::Pen, PALETTE[0], 3.0, hover);
    let draw = |t: Toolbar| {
        let mut buf = buffer(w, h);
        Renderer::new().render(
            &mut buf,
            w,
            h,
            1.0,
            view,
            &Board::default(),
            &Overlay {
                toolbar: Some(t),
                ..Default::default()
            },
            None,
        );
        buf
    };
    let (hovered, bare) = (draw(rail(Some(ToolbarAction::More))), draw(rail(None)));
    // Everything the legend adds is inside the box the rail declares, which is what a rail-only frame
    // repaints: a line outside it would be left behind the moment the pointer moved on.
    let box_ = toolbar_bounds(&rail(None), w as f32, h as f32);
    let mut painted = 0;
    let mut rows = (f32::MAX, 0.0f32);
    for (i, (a, b)) in hovered.chunks(4).zip(bare.chunks(4)).enumerate() {
        if a == b {
            continue;
        }
        let (x, y) = ((i as u32 % w) as f32, (i as u32 / w) as f32);
        assert!(
            box_.contains(x, y),
            "the drawer list painted at {x},{y}, outside the rail's box"
        );
        rows = (rows.0.min(y), rows.1.max(y));
        painted += 1;
    }
    assert!(painted > 0, "nothing was drawn for the drawer list");
    // The capsule and its own label are one block tall (30), so anything taller than that is the second
    // line the legend adds under them.
    assert!(
        rows.1 - rows.0 > 40.0,
        "nothing was drawn under the capsule: {rows:?}"
    );
}

#[test]
fn a_row_of_the_panel_can_be_clicked_with_the_pointer_on_the_block_that_opened_it() {
    // The regression the panel exists to fix: in the shipped build every press along the panel's row was
    // read as a press on `more`, because that block's hover capsule is up to 200 pt wide and the rail's hit
    // boxes are the ones tested first — so the panel's only control could not be clicked at all, and
    // clicking it closed the menu instead.
    let (w, h) = (900u32, 820u32);
    let toolbar = Toolbar {
        tool: Tool::Pen,
        color: PALETTE[0],
        size: 3.0,
        hover: Some(ToolbarAction::More),
        grow: 1.0,
        more: true,
        boards_open: false,
        boards: Vec::new(),
        shared: false,
        scrim: true,
    };
    let mut renderer = Renderer::new();
    let band = toolbar_bounds(&rail(), w as f32, h as f32);
    let hit = |renderer: &mut Renderer, x: f32, y: f32| {
        renderer.toolbar_hit(&toolbar, w as f32, h as f32, band.x + x, band.y + y)
    };
    // The panel's rows, found the way the pointer finds them: in its own column. The row nearest the block is
    // the last one when the panel grows up, which it does from a block near the foot of the rail.
    let rows: Vec<f32> = (1..band.h as u32)
        .map(|step| band.y + step as f32)
        .filter(|y| hit(&mut renderer, 60.0, *y).is_some())
        .collect();
    let (first_row, last_row) = (
        *rows.first().expect("the panel's row is not reachable"),
        *rows.last().expect("the panel's row is not reachable"),
    );
    // The panel's control answers on a row, and the block's own strip covers the rail's column beside it.
    assert_eq!(
        hit(&mut renderer, 60.0, last_row - 5.0),
        Some(ToolbarAction::Shared)
    );
    assert_eq!(
        hit(&mut renderer, 20.0, last_row - 5.0),
        Some(ToolbarAction::More)
    );
    // And down the panel's own column, from the first row to the last, no point belongs to nothing: the gap
    // between two rows belongs to the row above it, so moving between them is staying on the list.
    for y in (first_row as u32)..(last_row as u32) {
        assert!(
            hit(&mut renderer, 60.0, y as f32 + 0.5).is_some(),
            "the panel has a hole at {y}"
        );
    }
}

// --- the hand-drawn style ---------------------------------------------------------------------

/// The box the inked pixels occupy, as `(min_x, min_y, max_x, max_y)` in device pixels with `scale`
/// 1.0. `None` when nothing was painted.
fn inked_box(buf: &[u8], w: u32, h: u32) -> Option<(u32, u32, u32, u32)> {
    let mut out: Option<(u32, u32, u32, u32)> = None;
    for y in 0..h {
        for x in 0..w {
            if alpha(buf, w, x, y) > 0 {
                out = Some(match out {
                    None => (x, y, x, y),
                    Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                });
            }
        }
    }
    out
}

#[test]
fn a_stroke_is_drawn_as_a_hand_drawn_mark() {
    let (w, h) = (200u32, 100u32);
    let doc = ann(vec![line(50.0, 20.0, 180.0)], vec![]);
    let mut buf = buffer(w, h);
    frame(&mut buf, w, h, &doc, &Overlay::default(), None);
    let (_, top, _, bottom) = inked_box(&buf, w, h).expect("the stroke painted something");
    // The pen is four logical pixels wide, so the exact mark reaches two pixels either side of the
    // stored line plus one of anti-aliasing. A hand-drawn trace wanders past that, and both traces
    // cross, so the mark is wider than the pen.
    assert!(
        50.0 - top as f32 > 2.5 || bottom as f32 - 50.0 > 2.5,
        "the mark was the pen's exact width: top {top} bottom {bottom}"
    );
}

#[test]
fn a_shape_is_drawn_as_a_hand_drawn_outline() {
    let (w, h) = (240u32, 160u32);
    let doc = Board {
        strokes: vec![],
        texts: vec![],
        shapes: vec![Shape {
            kind: ShapeKind::Rect,
            color: "#3584e4".into(),
            width: 3.0,
            from: [60.0, 50.0],
            to: [180.0, 120.0],
        }],
        notes: vec![],
    };
    let mut buf = buffer(w, h);
    frame(&mut buf, w, h, &doc, &Overlay::default(), None);
    let (left, top, right, bottom) = inked_box(&buf, w, h).expect("the shape painted something");
    // Half the pen plus anti-aliasing is the exact rectangle's reach; a bowed trace leaves that box.
    let exact = 1.5 + 1.0;
    assert!(
        left as f32 <= 60.0 - exact - 0.5
            || top as f32 <= 50.0 - exact - 0.5
            || right as f32 >= 180.0 + exact + 0.5
            || bottom as f32 >= 120.0 + exact + 0.5,
        "the outline was the exact rectangle: {left},{top}..{right},{bottom}"
    );
}

/// One shape, alone on the surface, drawn exactly as the daemon draws a committed document.
fn sole_shape(shape: Shape) -> Board {
    Board {
        strokes: vec![],
        texts: vec![],
        shapes: vec![shape],
        notes: vec![],
    }
}

fn rect_shape(from: [f32; 2], to: [f32; 2]) -> Shape {
    Shape {
        kind: ShapeKind::Rect,
        color: "#3584e4".into(),
        width: 3.0,
        from,
        to,
    }
}

#[test]
fn sketched_ink_stays_inside_the_paint_box() {
    let (w, h) = (320u32, 320u32);
    let mut cases: Vec<(Board, Rect)> = vec![
        (
            ann(vec![line(150.0, 40.0, 280.0)], vec![]),
            line(150.0, 40.0, 280.0).paint_bounds().unwrap(),
        ),
        (
            sole_shape(rect_shape([80.0, 80.0], [220.0, 200.0])),
            rect_shape([80.0, 80.0], [220.0, 200.0]).paint_bounds(),
        ),
    ];
    for kind in [
        ShapeKind::Circle,
        ShapeKind::Diamond,
        ShapeKind::Line,
        ShapeKind::Arrow,
    ] {
        let shape = Shape {
            kind,
            ..rect_shape([80.0, 80.0], [220.0, 200.0])
        };
        let bounds = shape.paint_bounds();
        cases.push((sole_shape(shape), bounds));
    }
    for (doc, bounds) in cases {
        let mut buf = buffer(w, h);
        frame(&mut buf, w, h, &doc, &Overlay::default(), None);
        let (left, top, right, bottom) = inked_box(&buf, w, h).expect("the element painted");
        assert!(
            left as f32 >= bounds.x - 1.0
                && top as f32 >= bounds.y - 1.0
                && right as f32 <= bounds.x + bounds.w + 1.0
                && bottom as f32 <= bounds.y + bounds.h + 1.0,
            "{left},{top}..{right},{bottom} escaped {bounds:?}"
        );
    }
}

#[test]
fn a_dragged_shape_keeps_the_mark_it_had() {
    // The gesture draws the held element at the transform the pointer is dragging, and its wander must
    // come from the element it holds rather than from the moved copy: a mark that re-rolled every
    // motion event would shimmer under the hand.
    let (w, h) = (360u32, 220u32);
    let doc = sole_shape(rect_shape([60.0, 60.0], [180.0, 150.0]));
    let dragged = |dx: f32| {
        // Exactly the daemon's frame: the held element is left out of the document pass and drawn
        // once, at the gesture's transform.
        let selection = Selection {
            items: vec![ItemId::Shape(0)],
            transform: Some(Transform::Translate { dx, dy: 0.0 }),
        };
        let overlay = Overlay {
            selection: Some(selection.clone()),
            ..Default::default()
        };
        let mut buf = buffer(w, h);
        let mut cache = FrameCache::default();
        Renderer::new().render_with_cache(
            &mut buf,
            w,
            h,
            1.0,
            1.0,
            View::default(),
            &doc,
            &overlay,
            None,
            &mut cache,
            selection.live(),
        );
        buf
    };
    let near = dragged(10.0);
    let far = dragged(40.0);
    let shift = 30u32;
    for y in 0..h {
        for x in 0..w - shift {
            assert_eq!(
                alpha(&near, w, x, y),
                alpha(&far, w, x + shift, y),
                "the mark re-rolled at {x},{y}"
            );
        }
    }
}

#[test]
fn a_moved_sketched_shape_leaves_no_trail() {
    // The daemon damages the union of the box an element left and the box it went to. If the drawn
    // mark reached past those boxes, the outer edge of where it used to be would survive the frame.
    let (w, h) = (400u32, 300u32);
    // A heavy pen, because a light one's wander is inside what the half-width already allows for:
    // the point is the ink that only the style puts past the box.
    let heavy = |from: [f32; 2], to: [f32; 2]| Shape {
        width: 8.0,
        ..rect_shape(from, to)
    };
    let before = heavy([60.0, 60.0], [200.0, 160.0]);
    let after = heavy([140.0, 130.0], [280.0, 230.0]);
    let mut buf = buffer(w, h);
    let damage = before.paint_bounds().union(after.paint_bounds());
    let bounds = after.paint_bounds();
    frame(
        &mut buf,
        w,
        h,
        &sole_shape(before),
        &Overlay::default(),
        None,
    );
    frame(
        &mut buf,
        w,
        h,
        &sole_shape(after),
        &Overlay::default(),
        Some(damage),
    );
    let (left, top, right, bottom) = inked_box(&buf, w, h).expect("the shape painted");
    assert!(
        left as f32 >= bounds.x - 1.0
            && top as f32 >= bounds.y - 1.0
            && right as f32 <= bounds.x + bounds.w + 1.0
            && bottom as f32 <= bounds.y + bounds.h + 1.0,
        "{left},{top}..{right},{bottom} kept ink from where the shape was, inside {bounds:?}"
    );
}

#[test]
fn a_damaged_frame_of_a_sketched_document_matches_a_whole_surface_one() {
    let (w, h) = (300u32, 240u32);
    let doc = Board {
        strokes: vec![line(120.0, 30.0, 270.0)],
        texts: vec![],
        shapes: vec![rect_shape([50.0, 50.0], [150.0, 120.0])],
        notes: vec![],
    };
    let mut whole = buffer(w, h);
    frame(&mut whole, w, h, &doc, &Overlay::default(), None);
    let damage = doc.shapes[0]
        .paint_bounds()
        .union(doc.strokes[0].paint_bounds().unwrap());
    let mut damaged = buffer(w, h);
    frame(&mut damaged, w, h, &doc, &Overlay::default(), Some(damage));
    let x0 = damage.x as u32;
    let x1 = (damage.x + damage.w).ceil() as u32;
    let y0 = damage.y as u32;
    let y1 = (damage.y + damage.h).ceil() as u32;
    for y in y0..y1.min(h) {
        for x in x0..x1.min(w) {
            assert_eq!(
                alpha(&whole, w, x, y),
                alpha(&damaged, w, x, y),
                "the localized frame disagreed at {x},{y}"
            );
        }
    }
}

#[test]
fn a_second_renderer_draws_the_same_sketch() {
    // The style is derived from the element, not from a stream that runs across frames, so a restart
    // cannot change a drawing.
    let (w, h) = (300u32, 200u32);
    let doc = Board {
        strokes: vec![line(60.0, 30.0, 260.0)],
        texts: vec![],
        shapes: vec![rect_shape([40.0, 90.0], [180.0, 170.0])],
        notes: vec![],
    };
    let mut first = buffer(w, h);
    Renderer::new().render(
        &mut first,
        w,
        h,
        1.0,
        View::default(),
        &doc,
        &Overlay::default(),
        None,
    );
    let mut second = buffer(w, h);
    Renderer::new().render(
        &mut second,
        w,
        h,
        1.0,
        View::default(),
        &doc,
        &Overlay::default(),
        None,
    );
    assert_eq!(first, second);
}

/// The RGB a pixel carries, ignoring its alpha — what the eye reads on an opaque overlay.
fn rgb(buf: &[u8], w: u32, x: u32, y: u32) -> (u8, u8, u8) {
    let i = ((y * w + x) * 4) as usize;
    (buf[i], buf[i + 1], buf[i + 2])
}

fn near(a: (u8, u8, u8), b: (u8, u8, u8), tolerance: u8) -> bool {
    a.0.abs_diff(b.0) <= tolerance
        && a.1.abs_diff(b.1) <= tolerance
        && a.2.abs_diff(b.2) <= tolerance
}

#[test]
fn a_note_keeps_its_exact_square() {
    // A note is what the drawing is read against, so its square is a figure: a wobbling edge under
    // wrapped text is not a mark, it is a bug. The edge is measured against the box, column by column.
    let (w, h) = (400u32, 360u32);
    let note = Note {
        x: 100.0,
        y: 80.0,
        w: 200.0,
        h: 160.0,
        color: "#f6d32d".into(),
        text: "buy milk".into(),
        created: "2026/10/14".into(),
    };
    let doc = Board {
        strokes: vec![line(20.0, 20.0, 380.0)],
        texts: vec![],
        shapes: vec![rect_shape([260.0, 240.0], [380.0, 340.0])],
        notes: vec![note.clone()],
    };
    let mut buf = buffer(w, h);
    frame(&mut buf, w, h, &doc, &Overlay::default(), None);
    // Inside the rounded corners, every column of the top edge starts on the same row: the note's
    // boundary is a straight line, not a trace that wanders by a pixel or two.
    let top_of = |x: u32| {
        (60..h)
            .find(|y| alpha(&buf, w, x, *y) > 0)
            .expect("the note painted its column")
    };
    let reference = top_of(200);
    for x in 116..284u32 {
        assert_eq!(
            top_of(x),
            reference,
            "the note's top edge wobbled at column {x}"
        );
    }
}

#[test]
fn the_interface_over_a_sketched_document_is_exact() {
    // The rail, the selection frame, its handles, the marquee and the eraser cursor are interface, so
    // they are drawn the same over a sketched document as over a bare one. The comparison is by the
    // pixels they own: the accent and the near-white of a handle, which no mark shares.
    const ACCENT: (u8, u8, u8) = (10, 132, 255);
    const WHITE: (u8, u8, u8) = (255, 255, 255);
    let (w, h) = (400u32, 300u32);
    let shape = Shape {
        color: "#e01b24".into(),
        ..rect_shape([80.0, 90.0], [240.0, 200.0])
    };
    let marked = |bare: bool| {
        let mut doc = sole_shape(shape.clone());
        if !bare {
            doc.strokes = vec![line(40.0, 260.0, 360.0)];
            doc.shapes.push(Shape {
                kind: ShapeKind::Circle,
                ..rect_shape([260.0, 40.0], [370.0, 140.0])
            });
        }
        doc
    };
    let overlay = Overlay {
        selection: Some(Selection {
            items: vec![ItemId::Shape(0)],
            transform: None,
        }),
        marquee: Some(Rect {
            x: 300.0,
            y: 210.0,
            w: 70.0,
            h: 60.0,
        }),
        eraser: Some([60.0, 40.0]),
        ..Default::default()
    };
    let owned = |doc: &Board| {
        // Straight from the renderer, without the swap a shm buffer gets, so the accent's own bytes
        // are the ones compared.
        let mut buf = buffer(w, h);
        Renderer::new().render(&mut buf, w, h, 1.0, View::default(), doc, &overlay, None);
        let mut out = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let c = rgb(&buf, w, x, y);
                if alpha(&buf, w, x, y) == 255 && (near(c, ACCENT, 6) || near(c, WHITE, 6)) {
                    out.push((x, y, c));
                }
            }
        }
        out
    };
    let bare = owned(&marked(true));
    let busy = owned(&marked(false));
    assert!(!bare.is_empty(), "the selection frame painted nothing");
    assert_eq!(bare, busy, "the interface moved over a sketched document");
}

#[test]
fn removing_an_element_does_not_redraw_the_others() {
    // The seed comes from the element's own geometry, never from where it sits in its bucket, so
    // erasing one element cannot change how the ones next to it are drawn.
    let (w, h) = (400u32, 220u32);
    let first = line(50.0, 40.0, 360.0);
    let second = line(140.0, 40.0, 360.0);
    let render = |doc: &Board| {
        let mut buf = buffer(w, h);
        Renderer::new().render(
            &mut buf,
            w,
            h,
            1.0,
            View::default(),
            doc,
            &Overlay::default(),
            None,
        );
        buf
    };
    let both = render(&ann(vec![first.clone(), second.clone()], vec![]));
    let alone = render(&ann(vec![second.clone()], vec![]));
    let box_ = second.paint_bounds().unwrap();
    for y in box_.y as u32..(box_.y + box_.h) as u32 {
        for x in box_.x as u32..(box_.x + box_.w) as u32 {
            assert_eq!(
                alpha(&both, w, x, y),
                alpha(&alone, w, x, y),
                "the second stroke was redrawn at {x},{y}"
            );
        }
    }
}

/// How much of an element's ink lies beyond the exact outline it stores, as a fraction of its ink.
/// A figure drawn exactly scores ~0; a hand-drawn trace puts a good share of its ink outside.
fn outside_the_outline(doc: &Board, outline: &[[f32; 2]], reach: f32, w: u32, h: u32) -> f32 {
    let mut buf = buffer(w, h);
    frame(&mut buf, w, h, doc, &Overlay::default(), None);
    let (mut ink, mut out) = (0usize, 0usize);
    for y in 0..h {
        for x in 0..w {
            if alpha(&buf, w, x, y) == 0 {
                continue;
            }
            ink += 1;
            let p = [x as f32 + 0.5, y as f32 + 0.5];
            let nearest = outline
                .windows(2)
                .map(|seg| {
                    let (a, b) = (seg[0], seg[1]);
                    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
                    let len2 = dx * dx + dy * dy;
                    let t = if len2 <= f32::EPSILON {
                        0.0
                    } else {
                        (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2).clamp(0.0, 1.0)
                    };
                    let (qx, qy) = (a[0] + dx * t, a[1] + dy * t);
                    ((p[0] - qx).powi(2) + (p[1] - qy).powi(2)).sqrt()
                })
                .fold(f32::INFINITY, f32::min);
            if nearest > reach {
                out += 1;
            }
        }
    }
    out as f32 / ink.max(1) as f32
}

#[test]
fn every_shape_kind_puts_ink_outside_its_own_outline() {
    // The single-`||` guard could be satisfied by one hand-drawn edge on one kind. This asks each kind
    // on its own for a real share of its ink beyond the outline it stores.
    let (w, h) = (360u32, 300u32);
    for kind in [
        ShapeKind::Rect,
        ShapeKind::Circle,
        ShapeKind::Diamond,
        ShapeKind::Line,
        ShapeKind::Arrow,
    ] {
        let shape = Shape {
            kind,
            width: 3.0,
            ..rect_shape([70.0, 70.0], [290.0, 230.0])
        };
        let reach = shape.width / 2.0 + 1.0;
        let share = outside_the_outline(&sole_shape(shape.clone()), &shape.outline(), reach, w, h);
        eprintln!("DEBUG {kind:?} {:.1}% outside", share * 100.0);
        assert!(
            share > 0.05,
            "{kind:?} put only {:.1}% of its ink outside its own outline",
            share * 100.0
        );
    }
}

/// One frame drawn through a cache that outlives it, which is the daemon's path: the commit is
/// skipped, so every assertion below is about pixels and about what the cache holds afterwards.
#[allow(clippy::too_many_arguments)]
fn cached(
    renderer: &mut Renderer,
    cache: &mut FrameCache,
    buf: &mut [u8],
    w: u32,
    h: u32,
    view: View,
    doc: &Board,
    damage: Option<Rect>,
) {
    renderer.render_with_cache(
        buf,
        w,
        h,
        1.0,
        1.0,
        view,
        doc,
        &Overlay::default(),
        damage,
        cache,
        &[],
    );
}

/// A document with a stroke and a text box, so a frame has one of each to draw.
fn cached_document() -> Board {
    Board {
        strokes: vec![line(50.0, 20.0, 180.0)],
        texts: vec![TextItem {
            x: 20.0,
            y: 20.0,
            color: "#ffffff".into(),
            size: 18.0,
            text: "cached".into(),
        }],
        shapes: vec![],
        notes: vec![],
    }
}

#[test]
fn a_whole_surface_frame_draws_the_document_and_leaves_no_raster() {
    // The frame writes the buffer once for the background and once for the content: maintaining the
    // raster as well would be a third whole-surface write, so it is left absent instead.
    let (w, h) = (W, H);
    let mut renderer = Renderer::new();
    let mut cache = FrameCache::default();
    let doc = cached_document();
    let mut first = buffer(w, h);
    cached(
        &mut renderer,
        &mut cache,
        &mut first,
        w,
        h,
        View::default(),
        &doc,
        None,
    );
    assert!(
        !cache.base_valid(),
        "a whole-surface frame kept a raster it did not need"
    );

    // And it still draws the document: the second whole-surface frame is byte-identical.
    let mut second = buffer(w, h);
    cached(
        &mut renderer,
        &mut cache,
        &mut second,
        w,
        h,
        View::default(),
        &doc,
        None,
    );
    assert_eq!(first, second, "the same document drew differently");
    assert!(ink(&first) > 0, "the document painted nothing");
}

#[test]
fn the_first_localized_frame_after_a_view_change_rebuilds_the_raster() {
    let (w, h) = (W, H);
    let mut renderer = Renderer::new();
    let mut cache = FrameCache::default();
    let doc = cached_document();
    let mut whole = buffer(w, h);
    cached(
        &mut renderer,
        &mut cache,
        &mut whole,
        w,
        h,
        View::default(),
        &doc,
        None,
    );

    // A damage box this frame must not paint outside of: everything else keeps the sentinel.
    let damage = Rect {
        x: 10.0,
        y: 10.0,
        w: 80.0,
        h: 60.0,
    };
    let mut buf = vec![0x5a; (w * h * 4) as usize];
    cached(
        &mut renderer,
        &mut cache,
        &mut buf,
        w,
        h,
        View::default(),
        &doc,
        Some(damage),
    );
    assert!(
        cache.base_valid(),
        "the localized frame left no raster to fill from"
    );
    let region = DamageRegion::from_logical(damage, 1.0, w, h);
    let inside = |x: u32, y: u32| match region {
        DamageRegion::All => true,
        DamageRegion::Rect { x0, y0, x1, y1 } => x >= x0 && x < x1 && y >= y0 && y < y1,
    };
    for y in 0..h {
        for x in 0..w {
            if !inside(x, y) {
                assert_eq!(
                    alpha(&buf, w, x, y),
                    0x5a,
                    "the localized frame painted outside its damage at {x},{y}"
                );
            }
        }
    }
    // Inside the box it agrees with a whole-surface frame of the same document.
    for y in damage.y as u32..(damage.y + damage.h) as u32 {
        for x in damage.x as u32..(damage.x + damage.w) as u32 {
            assert_eq!(
                alpha(&whole, w, x, y),
                alpha(&buf, w, x, y),
                "the rebuilt raster disagreed at {x},{y}"
            );
        }
    }
}

#[test]
fn a_warm_raster_is_copied_for_a_whole_surface_frame_and_stays_valid() {
    // Copying a warm raster is one pass over the pixels where drawing the document is a fill plus
    // every element, so a whole-surface frame takes the copy and keeps the raster.
    let (w, h) = (W, H);
    let mut renderer = Renderer::new();
    let mut cache = FrameCache::default();
    let doc = cached_document();
    let damage = Rect {
        x: 10.0,
        y: 10.0,
        w: 40.0,
        h: 40.0,
    };
    let mut primed = buffer(w, h);
    cached(
        &mut renderer,
        &mut cache,
        &mut primed,
        w,
        h,
        View::default(),
        &doc,
        Some(damage),
    );
    assert!(cache.base_valid(), "the priming frame built no raster");

    let mut copied = buffer(w, h);
    cached(
        &mut renderer,
        &mut cache,
        &mut copied,
        w,
        h,
        View::default(),
        &doc,
        None,
    );
    assert!(cache.base_valid(), "the copy dropped the raster it used");

    let mut drawn = buffer(w, h);
    let mut cold = FrameCache::default();
    cached(
        &mut renderer,
        &mut cold,
        &mut drawn,
        w,
        h,
        View::default(),
        &doc,
        None,
    );
    assert_eq!(copied, drawn, "the copy disagreed with the document itself");
}

#[test]
fn a_localized_frame_rebuilds_the_raster_for_the_view_it_paints() {
    // The raster is screen-space, so a view change makes it stale: the localized frame that follows
    // must rebuild it for the new view instead of filling from the old one.
    let (w, h) = (W, H);
    let mut renderer = Renderer::new();
    let mut cache = FrameCache::default();
    let doc = cached_document();
    let moved = View {
        zoom: 1.0,
        offset: [-30.0, -20.0],
    };
    let mut before = buffer(w, h);
    cached(
        &mut renderer,
        &mut cache,
        &mut before,
        w,
        h,
        View::default(),
        &doc,
        None,
    );

    let damage = Rect {
        x: 0.0,
        y: 0.0,
        w: 120.0,
        h: 80.0,
    };
    let mut localized = buffer(w, h);
    cached(
        &mut renderer,
        &mut cache,
        &mut localized,
        w,
        h,
        moved,
        &doc,
        Some(damage),
    );
    let mut whole = buffer(w, h);
    cached(
        &mut renderer,
        &mut cache,
        &mut whole,
        w,
        h,
        moved,
        &doc,
        None,
    );
    for y in 0..80u32 {
        for x in 0..120u32 {
            assert_eq!(
                alpha(&whole, w, x, y),
                alpha(&localized, w, x, y),
                "the localized frame used the previous view at {x},{y}"
            );
        }
    }
}

/// The rung: a coarse frame is the same drawing, drawn at a fraction of the pixels and blitted up, so
/// it is the same ink in the same place and slightly softer. What must not change is where it is.
#[test]
fn a_coarse_frame_keeps_the_drawing_where_the_full_one_puts_it() {
    use sgraffito::canvas::MOTION_QUALITY;

    let (w, h) = (300u32, 240u32);
    let doc = Board {
        strokes: vec![line(120.0, 30.0, 270.0)],
        texts: vec![],
        shapes: vec![rect_shape([50.0, 50.0], [150.0, 120.0])],
        notes: vec![],
    };
    let render = |quality: f32| {
        let mut buf = buffer(w, h);
        let mut renderer = Renderer::new();
        let mut cache = FrameCache::default();
        renderer.render_with_cache(
            &mut buf,
            w,
            h,
            1.0,
            quality,
            View::default(),
            &doc,
            &Overlay::default(),
            None,
            &mut cache,
            &[],
        );
        (buf, cache.base_valid())
    };
    let (full, full_valid) = render(1.0);
    let (coarse, coarse_valid) = render(MOTION_QUALITY);
    assert!(
        !full_valid && !coarse_valid,
        "a whole-surface frame kept a raster"
    );

    let whole = inked_box(&full, w, h).expect("the full frame painted");
    let soft = inked_box(&coarse, w, h).expect("the coarse frame painted");
    // The blit spreads an edge by at most a pixel at 0.5, so the drawing may not move further than
    // that in any direction: a coarse frame is a softer copy, never a displaced one.
    for (a, b) in [(whole.0, soft.0), (whole.1, soft.1)] {
        assert!(
            a.abs_diff(b) <= 2,
            "the coarse frame moved the drawing's edge: {whole:?} against {soft:?}"
        );
    }
    for (a, b) in [(whole.2, soft.2), (whole.3, soft.3)] {
        assert!(
            a.abs_diff(b) <= 2,
            "the coarse frame moved the drawing's edge: {whole:?} against {soft:?}"
        );
    }
}

#[test]
fn a_full_resolution_frame_after_a_coarse_one_is_crisp_again() {
    use sgraffito::canvas::MOTION_QUALITY;

    let (w, h) = (300u32, 240u32);
    let doc = Board {
        strokes: vec![line(120.0, 30.0, 270.0)],
        texts: vec![],
        shapes: vec![],
        notes: vec![],
    };
    let mut renderer = Renderer::new();
    let mut cache = FrameCache::default();
    let mut coarse = buffer(w, h);
    renderer.render_with_cache(
        &mut coarse,
        w,
        h,
        1.0,
        MOTION_QUALITY,
        View::default(),
        &doc,
        &Overlay::default(),
        None,
        &mut cache,
        &[],
    );
    let mut landed = buffer(w, h);
    renderer.render_with_cache(
        &mut landed,
        w,
        h,
        1.0,
        1.0,
        View::default(),
        &doc,
        &Overlay::default(),
        None,
        &mut cache,
        &[],
    );
    // The frame the pointer stopped on is the one a renderer that never went coarse would draw.
    let mut reference = buffer(w, h);
    let mut cold = FrameCache::default();
    renderer.render_with_cache(
        &mut reference,
        w,
        h,
        1.0,
        1.0,
        View::default(),
        &doc,
        &Overlay::default(),
        None,
        &mut cold,
        &[],
    );
    assert_eq!(landed, reference, "the settle frame kept the coarse pixels");
    assert_ne!(coarse, landed, "the coarse frame drew at full resolution");
}

/// A rail with the boards panel open on the given boards, as the daemon builds it for one output.
fn boards_rail(boards: &[(&str, bool)]) -> Toolbar {
    Toolbar {
        boards_open: true,
        boards: boards
            .iter()
            .map(|(name, current)| BoardRow {
                name: (*name).to_string(),
                current: *current,
            })
            .collect(),
        ..toolbar(Tool::Pen, PALETTE[0], 3.0, None)
    }
}

/// The panel is the document's boards and nothing else: one row per board, the one this output shows
/// marked, then the row that deletes and the row that creates one.
#[test]
fn the_boards_panel_states_the_documents_boards() {
    let (w, h) = (900.0f32, 1000.0f32);
    let mut renderer = Renderer::new();
    let rail = boards_rail(&[("Board 1", false), ("office", true)]);
    // Sweeping the panel's column top to bottom gives its rows in the order they are drawn.
    let sweep = sweep_rail(&mut renderer, &rail, w, h, 40.0);
    let mut rows: Vec<ToolbarAction> = Vec::new();
    for action in sweep.into_iter().flatten() {
        if !rows.contains(&action) {
            rows.push(action);
        }
    }
    assert_eq!(
        rows,
        vec![
            ToolbarAction::Board(0),
            ToolbarAction::Board(1),
            ToolbarAction::DeleteBoard,
            ToolbarAction::NewBoard
        ],
        "the panel's rows are not its boards, the row that deletes one and the row that creates one"
    );
}

/// A document with one board has nothing to delete, so the row is absent rather than inert: a control
/// that does nothing is a control nobody can tell from one that does.
#[test]
fn a_panel_with_one_board_has_no_delete_row() {
    let (w, h) = (900.0f32, 1000.0f32);
    let mut renderer = Renderer::new();
    let rail = boards_rail(&[("Board 1", true)]);
    let sweep = sweep_rail(&mut renderer, &rail, w, h, 40.0);
    let mut rows: Vec<ToolbarAction> = Vec::new();
    for action in sweep.into_iter().flatten() {
        if !rows.contains(&action) {
            rows.push(action);
        }
    }
    assert_eq!(rows, vec![ToolbarAction::Board(0), ToolbarAction::NewBoard]);
}

/// A row of the panel is hit as the board it lists, and the board on screen is the one the panel marks.
#[test]
fn a_board_row_is_hit_as_that_board() {
    let (w, h) = (900.0f32, 1000.0f32);
    let mut renderer = Renderer::new();
    let rail = boards_rail(&[("Board 1", false), ("office", true)]);
    let index = |renderer: &mut Renderer, action: ToolbarAction| {
        let sweep = sweep_rail(renderer, &rail, w, h, 40.0);
        sweep.iter().position(|hit| *hit == Some(action))
    };
    let (first, second) = (
        index(&mut renderer, ToolbarAction::Board(0)).expect("the first board's row"),
        index(&mut renderer, ToolbarAction::Board(1)).expect("the second board's row"),
    );
    assert!(second > first, "the rows are not in the document's order");
}

/// The corridor: on its own row the block owns the strip as far as the panel's column, and a row of the
/// panel owns its own column, so no rail box is tested first over a row and takes it. The vertical gap
/// between the block's band and the rows is not covered here — it is the drawer's, inherited, and it is
/// what a pointer crossing it exactly can close the panel on.
#[test]
fn the_boards_block_owns_its_row_as_far_as_the_panel_and_a_row_owns_its_column() {
    let (w, h) = (900.0f32, 1000.0f32);
    let mut renderer = Renderer::new();
    let rail = boards_rail(&[("Board 1", true), ("office", false)]);
    let band = toolbar_bounds(&rail, w, h);
    // The rail's own origin: the box starts one hairline and one pad left of the block column.
    let base = band.x + 1.0 + 6.0;
    // The rail's own column for the block, the panel's column for its rows.
    let rail_sweep = sweep_rail(&mut renderer, &rail, w, h, 20.0);
    let panel_sweep = sweep_rail(&mut renderer, &rail, w, h, 40.0);
    let row_of = |sweep: &[Option<ToolbarAction>], action: ToolbarAction| {
        sweep
            .iter()
            .position(|hit| *hit == Some(action))
            .unwrap_or_else(|| panic!("{action:?} is not on the rail")) as f32
    };
    let (block_y, first_row_y) = (
        band.y + row_of(&rail_sweep, ToolbarAction::Boards) + 0.5,
        band.y + row_of(&panel_sweep, ToolbarAction::Board(0)) + 0.5,
    );
    // The corridor: the block's own row reaches exactly to the panel's column, so the pointer is on the
    // block until it is on the panel's own column.
    for x in 0..32 {
        assert_eq!(
            renderer.toolbar_hit(&rail, w, h, base + x as f32, block_y),
            Some(ToolbarAction::Boards),
            "the corridor has a hole at {x} on the block's row"
        );
    }
    // And the row is still the row once the pointer is in the panel's own column.
    for x in 34..60 {
        assert_eq!(
            renderer.toolbar_hit(&rail, w, h, base + x as f32, first_row_y),
            Some(ToolbarAction::Board(0)),
            "the row was taken from the panel at {x}"
        );
    }
}

/// The row level with the `boards` block is where the corridor leads the pointer into the list, so the
/// row that deletes a board is never there: a panel tall enough to grow upwards puts its last row on
/// that band, and one click from it destroys a board with no undo behind it.
#[test]
fn the_row_the_pointer_walks_into_is_not_the_one_that_deletes() {
    let (w, h) = (900.0f32, 600.0f32);
    let mut renderer = Renderer::new();
    let rows = [("Board 1", false), ("office", true)];
    let open = boards_rail(&rows);
    let shut = Toolbar {
        boards_open: false,
        ..boards_rail(&rows)
    };
    let row = |sweep: &[Option<ToolbarAction>], action: ToolbarAction| {
        sweep
            .iter()
            .position(|hit| *hit == Some(action))
            .unwrap_or_else(|| panic!("{action:?} is not on the rail"))
    };
    // The block's own row is taken with the panel shut: open, the corridor answers for the whole
    // strip beside the panel instead, so the rail's sweep no longer shows the block's band.
    let block_y = row(
        &sweep_rail(&mut renderer, &shut, w, h, 20.0),
        ToolbarAction::Boards,
    ) + 8;
    let panel = sweep_rail(&mut renderer, &open, w, h, 40.0);
    assert!(
        panel.contains(&Some(ToolbarAction::DeleteBoard)),
        "the panel has no row that deletes a board"
    );
    assert_eq!(
        panel[block_y],
        Some(ToolbarAction::NewBoard),
        "the row level with the `boards` block is not the row that creates a board"
    );
}

/// The panel is inside the box the rail declares, or a frame that repaints only the rail leaves its
/// pixels behind.
#[test]
fn the_boards_panel_paints_inside_the_rails_box() {
    let (w, h) = (900u32, 1000u32);
    let view = View::default();
    let draw = |rail: &Toolbar| {
        let mut buf = buffer(w, h);
        Renderer::new().render(
            &mut buf,
            w,
            h,
            1.0,
            view,
            &Board::default(),
            &Overlay {
                toolbar: Some(rail.clone()),
                ..Default::default()
            },
            None,
        );
        buf
    };
    let open = draw(&boards_rail(&[("Board 1", true), ("office", false)]));
    let shut = draw(&Toolbar {
        boards_open: false,
        ..boards_rail(&[("Board 1", true), ("office", false)])
    });
    let box_ = toolbar_bounds(&boards_rail(&[("Board 1", true)]), w as f32, h as f32);
    let mut painted = 0;
    for (i, (a, b)) in open.chunks(4).zip(shut.chunks(4)).enumerate() {
        if a == b {
            continue;
        }
        let (x, y) = ((i as u32 % w) as f32, (i as u32 / w) as f32);
        assert!(
            box_.contains(x, y),
            "the panel painted at {x},{y}, outside the box"
        );
        painted += 1;
    }
    assert!(painted > 0, "nothing was drawn for the panel");
}

/// A menu nobody can walk into is not a menu. Moving the pointer out of the block that opened a panel and
/// into the panel must never cross a point that belongs to nothing, and moving between two of its rows must
/// not either: the first closes the panel, the second does the same one row in.
#[test]
fn the_pointer_can_walk_from_the_block_into_its_panel_and_between_its_rows() {
    let (w, h) = (900.0f32, 1000.0f32);
    let mut renderer = Renderer::new();
    let rail = boards_rail(&[("Board 1", true), ("office", false)]);
    let band = toolbar_bounds(&rail, w, h);
    // The rail's own origin: the box starts one hairline and one pad left of the block column.
    let base = band.x + 1.0 + 6.0;
    let block_row = sweep_rail(&mut renderer, &rail, w, h, 20.0)
        .iter()
        .position(|hit| *hit == Some(ToolbarAction::Boards))
        .expect("the boards block") as f32;
    // The middle of the block's own band, and the panel's column.
    let y = band.y + block_row + 15.0;
    // The panel's own column: a block is 30 wide and the panel starts one gap past it.
    let inside_panel = base + 34.0 + 10.0;
    // 1. Sideways out of the block: the row that shares its band takes over where the block's box ends.
    for x in 0..60 {
        assert!(
            renderer
                .toolbar_hit(&rail, w, h, base + x as f32, y)
                .is_some(),
            "nothing is hit {x} past the rail's edge on the block's own row, so the panel closes"
        );
    }
    // 2. And through the panel's own column, row after row, with no hole between them.
    let hits: Vec<Option<ToolbarAction>> = (0..160)
        .map(|dy| renderer.toolbar_hit(&rail, w, h, inside_panel, y - 80.0 + dy as f32))
        .collect();
    let first = hits
        .iter()
        .position(Option::is_some)
        .expect("no row at all");
    let last = hits
        .iter()
        .rposition(Option::is_some)
        .expect("no row at all");
    for (i, hit) in hits.iter().enumerate().take(last + 1).skip(first) {
        assert!(
            hit.is_some(),
            "the panel has a hole at {} in its column, so moving between two rows closes it",
            i as f32 - 80.0
        );
    }
}

/// A tool that folded is reached through the drawer: it is a row of the panel like any other, and the rail
/// on that output simply does not have it. Before the fold, this output's rail ran past the bottom of the
/// screen and the note tool was off it — unreachable, not folded.
#[test]
fn a_folded_tool_is_reached_through_the_drawer() {
    let (w, h) = (900.0f32, 500.0f32);
    let mut renderer = Renderer::new();
    let mut rail = toolbar(Tool::Pen, PALETTE[0], 3.0, None);
    rail.more = true;
    let hit = |renderer: &mut Renderer, x: f32, y: f32| renderer.toolbar_hit(&rail, w, h, x, y);
    // The note's tool is not on this output's rail, but it is a row of its drawer.
    let column_x = toolbar_bounds(&rail, w, h).x + 1.0 + 40.0;
    let found = (0..h as u32)
        .map(|y| y as f32 + 0.5)
        .any(|y| hit(&mut renderer, column_x, y) == Some(ToolbarAction::Tool(Tool::Note)));
    assert!(found, "the folded note tool is in no drawer row");
    // And the rail's own column holds no block at all for it.
    let rail_x = toolbar_bounds(&rail, w, h).x + 1.0 + 15.0;
    assert!(
        !(0..h as u32)
            .map(|y| y as f32 + 0.5)
            .any(|y| hit(&mut renderer, rail_x, y) == Some(ToolbarAction::Tool(Tool::Note))),
        "the note is on the rail and in the drawer at once"
    );
}

/// A short output folds its whole overflow into the drawer, and that drawer is the one thing in this layout
/// that can outgrow both the rail and the output: nineteen folded controls would be a twenty-one-row panel,
/// which on a 400-pixel output is a panel taller than the screen. Every row it draws must therefore be inside
/// the output, inside the box the rail declares for damage accounting, and hit as its own control — a row that
/// is painted but not clickable, or clickable but never repainted, is worse than one that is not there.
#[test]
fn a_panel_draws_only_rows_that_are_inside_the_output_and_clickable() {
    let (w, h) = (900u32, 400u32);
    let mut rail = toolbar(Tool::Pen, PALETTE[0], 3.0, None);
    rail.more = true;
    // No backdrop: with it on every pixel of the surface is the scrim and the test would measure that.
    rail.scrim = false;
    let mut buf = buffer(w, h);
    Renderer::new().render(
        &mut buf,
        w,
        h,
        1.0,
        View::default(),
        &Board::default(),
        &Overlay {
            toolbar: Some(rail.clone()),
            ..Default::default()
        },
        None,
    );
    let band = toolbar_bounds(&rail, w as f32, h as f32);
    assert!(
        band.y + band.h <= h as f32,
        "the box itself leaves the output: {band:?}"
    );
    let mut renderer = Renderer::new();
    // The panel's own column, one block right of the rail's.
    let column_x = band.x + 1.0 + 40.0;
    let painted: Vec<u32> = (0..h)
        .filter(|y| (50..300).any(|x| alpha(&buf, w, x, *y) > 0))
        .collect();
    let (first, last) = (
        *painted.first().expect("the drawer drew nothing") as f32,
        *painted.last().expect("the drawer drew nothing") as f32,
    );
    assert!(
        last < band.y + band.h,
        "the panel paints down to {last}, past its box ({band:?})"
    );
    // The plate carries `TOOLBAR_PAD` above its first row and below its last, and those pixels belong to no
    // row: everything between them is a row, gap included.
    for y in (first as u32 + 6)..(last as u32 - 6) {
        assert!(
            renderer
                .toolbar_hit(&rail, w as f32, h as f32, column_x, y as f32 + 0.5)
                .is_some(),
            "the panel paints {y} and nothing is hit there"
        );
    }
}

/// The corridor is the block's strip in the rail's column, as tall as the panel it opened, and the rows answer
/// from the panel's own column: the pointer leaves the block at whatever row it likes on its way into the list.
/// Drifting on the way there used to step into a neighbouring rail block, which reads as leaving the menu, and
/// the list closed before the pointer had arrived.
#[test]
fn the_corridor_covers_the_panels_whole_span_not_just_the_blocks_row() {
    let (w, h) = (900.0f32, 1000.0f32);
    let mut renderer = Renderer::new();
    let rail = boards_rail(&[("Board 1", true), ("office", false)]);
    let band = toolbar_bounds(&rail, w, h);
    let (rail_x, row_x) = (band.x + 1.0 + 15.0, band.x + 1.0 + 40.0);
    // The panel's own span, found the way the pointer finds it: its rows in the second column.
    let rows: Vec<f32> = (0..h as u32)
        .map(|y| y as f32 + 0.5)
        .filter(|y| renderer.toolbar_hit(&rail, w, h, row_x, *y).is_some())
        .collect();
    let (top, bottom) = (
        *rows.first().expect("no rows"),
        *rows.last().expect("no rows"),
    );
    assert!(
        bottom - top > 60.0,
        "the panel is one row tall: {top}..{bottom}"
    );
    // Every point of the rail's column beside the panel is the block that opened it, and every point of the
    // panel's column is one of its rows: nothing between the two is nobody's.
    for y in (top as u32 + 1)..(bottom as u32 - 1) {
        assert_eq!(
            renderer.toolbar_hit(&rail, w, h, rail_x, y as f32 + 0.5),
            Some(ToolbarAction::Boards),
            "the rail's column at {y} is not the block that opened the panel"
        );
        assert!(
            renderer
                .toolbar_hit(&rail, w, h, row_x, y as f32 + 0.5)
                .is_some(),
            "the panel's column at {y} belongs to no row"
        );
    }
}

/// A panel has no plate behind it: the rows are the control, and a dark rectangle behind the list was a second
/// surface to read through. So the panel's column is the wallpaper between the rows and above the first one.
#[test]
fn a_panel_paints_no_plate_behind_its_rows() {
    let (w, h) = (900u32, 1000u32);
    let mut rail = boards_rail(&[("Board 1", true), ("office", false)]);
    rail.scrim = false;
    let mut buf = buffer(w, h);
    Renderer::new().render(
        &mut buf,
        w,
        h,
        1.0,
        View::default(),
        &Board::default(),
        &Overlay {
            toolbar: Some(rail.clone()),
            ..Default::default()
        },
        None,
    );
    let mut renderer = Renderer::new();
    let band = toolbar_bounds(&rail, w as f32, h as f32);
    // One point for the hit test, one inside a row's capsule for the pixels.
    let (x, probe) = (band.x + 1.0 + 40.0, band.x + 1.0 + 55.0);
    let row = (0..h)
        .map(|y| y as f32 + 0.5)
        .find(|y| {
            renderer
                .toolbar_hit(&rail, w as f32, h as f32, x, *y)
                .is_some()
        })
        .expect("the panel has no rows");
    // Two pixels above the first row, and the gap between two rows: both were the plate's before.
    for y in [row - 2.0, row + 32.0] {
        assert_eq!(
            alpha(&buf, w, probe as u32, y as u32),
            0,
            "the panel painted a plate at y={y}"
        );
    }
    // The rows themselves are still painted, so this is not a test of an empty panel.
    assert!(alpha(&buf, w, probe as u32, row as u32 + 10) > 0);
}
