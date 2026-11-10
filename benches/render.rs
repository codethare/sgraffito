use std::hint::black_box;
use std::time::Instant;

use sgraffito::canvas::{
    Board, MOTION_QUALITY, Overlay, Rect, Stroke, TextBuffer, TextItem, TextOverlay, Tool, Toolbar,
    ToolbarAction, View,
};
use sgraffito::render::{FrameCache, Renderer, toolbar_bounds, transient_bounds, transient_damage};
use sgraffito::sketch;

const WARMUP: usize = 8;
const SAMPLES: usize = 30;

/// The optimisation level the release profile declares. The benchmark runs under whichever profile
/// `cargo bench` built it with, so an environment override of that profile is invisible here — the
/// figures themselves are the evidence in that case.
fn opt_level() -> String {
    let manifest = std::fs::read_to_string("Cargo.toml").unwrap_or_default();
    let mut in_release = false;
    for line in manifest.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.starts_with('[') {
            in_release = line == "[profile.release]";
        } else if in_release
            && let Some((key, value)) = line.split_once('=')
            && key.trim() == "opt-level"
        {
            return value.trim().trim_matches('"').to_string();
        }
    }
    "unknown".to_string()
}

fn line(y: f32, x0: f32, x1: f32, width: f32) -> Stroke {
    Stroke {
        color: "#e01b24".into(),
        width,
        points: vec![[x0, y], [x1, y]],
    }
}

fn document(stroke_count: usize, width: f32, height: f32) -> Board {
    let strokes = (0..stroke_count)
        .map(|i| {
            let y = height * (i as f32 + 1.0) / (stroke_count + 1) as f32;
            line(y, width * 0.1, width * 0.9, 3.0)
        })
        .collect();
    Board {
        strokes,
        texts: vec![TextItem {
            x: width * 0.1,
            y: height * 0.12,
            color: "#ffffff".into(),
            size: 24.0,
            text: "benchmark note".into(),
        }],
        shapes: vec![],
        notes: vec![],
    }
}

fn live_stroke(width: f32, height: f32) -> Stroke {
    Stroke {
        color: "#3584e4".into(),
        width: 3.0,
        points: vec![[width * 0.2, height * 0.3], [width * 0.24, height * 0.34]],
    }
}

fn text_overlay(width: f32, height: f32, preedit: &str) -> Overlay {
    Overlay {
        text: Some(TextOverlay {
            item: TextItem {
                x: width * 0.1,
                y: height * 0.2,
                color: "#ffffff".into(),
                size: 24.0,
                text: "benchmark".into(),
            },
            buffer: TextBuffer::new("benchmark"),
            index: None,
            note: None,
            preedit: preedit.into(),
        }),
        ..Default::default()
    }
}

#[allow(clippy::too_many_arguments)]
fn measure(
    name: &str,
    logical_width: u32,
    logical_height: u32,
    scale: f32,
    view: View,
    annotations: &Board,
    overlay: &Overlay,
    damage: Option<Rect>,
    cold_raster: bool,
) {
    measure_at(
        name,
        logical_width,
        logical_height,
        scale,
        view,
        annotations,
        overlay,
        damage,
        cold_raster,
        1.0,
    );
}

/// The same measurement, with the document rasterised at `quality` of the surface's own pixels: what
/// a canvas moving faster than `SETTLE_SPEED` is drawn at.
#[allow(clippy::too_many_arguments)]
fn measure_at(
    name: &str,
    logical_width: u32,
    logical_height: u32,
    scale: f32,
    view: View,
    annotations: &Board,
    overlay: &Overlay,
    damage: Option<Rect>,
    cold_raster: bool,
    quality: f32,
) {
    let width = (logical_width as f32 * scale) as u32;
    let height = (logical_height as f32 * scale) as u32;
    let mut buffer = vec![0u8; (width * height * 4) as usize];
    let mut renderer = Renderer::new();
    let mut cache = FrameCache::default();

    // A warm whole-surface frame only exists after a localized frame has built the raster — a
    // whole-surface frame does not maintain one — so this case is set up the way the daemon reaches
    // it, instead of pretending a document drawn from scratch is the copy that case measures.
    if !cold_raster && damage.is_none() {
        render_frame(
            &mut renderer,
            &mut cache,
            &mut buffer,
            width,
            height,
            scale,
            annotations,
            overlay,
            Some(Rect {
                x: 0.0,
                y: 0.0,
                w: 1.0,
                h: 1.0,
            }),
            view,
            quality,
        );
    }

    for _ in 0..WARMUP {
        if cold_raster {
            cache.invalidate();
        }
        render_frame(
            &mut renderer,
            &mut cache,
            &mut buffer,
            width,
            height,
            scale,
            annotations,
            overlay,
            damage,
            view,
            quality,
        );
    }

    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        if cold_raster {
            cache.invalidate();
        }
        let start = Instant::now();
        render_frame(
            &mut renderer,
            &mut cache,
            &mut buffer,
            width,
            height,
            scale,
            annotations,
            overlay,
            damage,
            view,
            quality,
        );
        samples.push(start.elapsed().as_nanos() as u64);
    }
    black_box(&buffer);

    samples.sort_unstable();
    let median = samples[samples.len() / 2] as f64 / 1_000.0;
    let p95 = samples[(samples.len() * 95 / 100).min(samples.len() - 1)] as f64 / 1_000.0;
    println!(
        "{name}: median={median:.1}us p95={p95:.1}us logical={logical_width}x{logical_height} scale={scale:.1} buffer={width}x{height} strokes={} texts={} damage={} toolbar={} text={} raster={} view={:.2}@{:.0},{:.0} quality={quality:.2} opt={}",
        annotations.strokes.len(),
        annotations.texts.len(),
        damage.is_some(),
        overlay.toolbar.is_some(),
        overlay.text.is_some(),
        if cold_raster { "cold" } else { "warm" },
        view.zoom,
        view.offset[0],
        view.offset[1],
        opt_level(),
    );
}

#[allow(clippy::too_many_arguments)]
fn render_frame(
    renderer: &mut Renderer,
    cache: &mut FrameCache,
    buffer: &mut [u8],
    width: u32,
    height: u32,
    scale: f32,
    annotations: &Board,
    overlay: &Overlay,
    damage: Option<Rect>,
    view: View,
    quality: f32,
) {
    renderer.render_with_cache(
        buffer,
        width,
        height,
        scale,
        quality,
        view,
        annotations,
        overlay,
        damage,
        cache,
        &[],
    );
}

/// The rail with the pen armed and no block under the pointer, fully open.
fn rail_state() -> Toolbar {
    Toolbar {
        more: false,
        boards_open: false,
        boards: Vec::new(),
        shared: false,
        scrim: true,
        tool: Tool::Pen,
        color: "#e01b24",
        size: 3.0,
        hover: None,
        grow: 1.0,
    }
}

/// What the hand-drawn style costs before any pixel is touched: the wander for every stroke in a dense
/// document. Path generation happens in the raster build, so this is the part of the style that is not
/// a rasterisation cost, and the figure is what says whether caching the commands would buy anything.
fn sketch_cost(strokes: &[Stroke]) {
    let generate = || {
        let mut segments = 0usize;
        for s in strokes {
            segments += black_box(sketch::stroke(&s.points, s.width, s.seed())).len();
        }
        black_box(segments)
    };
    for _ in 0..WARMUP {
        generate();
    }
    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let start = Instant::now();
        generate();
        samples.push(start.elapsed().as_nanos() as u64);
    }
    samples.sort_unstable();
    let median = samples[samples.len() / 2] as f64 / 1_000.0;
    let p95 = samples[(samples.len() * 95 / 100).min(samples.len() - 1)] as f64 / 1_000.0;
    println!(
        "sketch-commands: median={median:.1}us p95={p95:.1}us strokes={} (path generation only, no pixels)",
        strokes.len()
    );
}

fn main() {
    let sparse = document(30, 3840.0, 2160.0);
    let small = document(30, 1920.0, 1080.0);
    let dense = document(200, 3840.0, 2160.0);
    let toolbar = Overlay {
        toolbar: Some(rail_state()),
        ..Default::default()
    };
    let drag = Overlay {
        stroke: Some(live_stroke(3840.0, 2160.0)),
        ..Default::default()
    };
    let drag_damage = transient_damage(None, transient_bounds(&drag), None);
    measure(
        "sparse-drag",
        3840,
        2160,
        1.0,
        View::default(),
        &sparse,
        &drag,
        Some(drag_damage),
        false,
    );
    // The drag that used to grow its box into the drawing and cost a whole-surface frame.
    measure(
        "dense-drag-damaged",
        3840,
        2160,
        1.0,
        View::default(),
        &dense,
        &drag,
        Some(drag_damage),
        false,
    );
    measure(
        "dense-full",
        3840,
        2160,
        1.0,
        View::default(),
        &dense,
        &drag,
        None,
        false,
    );
    // A frame that only copies the raster: no overlay, so nothing is drawn into it and the style has
    // no work to do at all. `dense-full` above copies *and* draws a live stroke.
    measure(
        "copy-only",
        3840,
        2160,
        1.0,
        View::default(),
        &dense,
        &Overlay::default(),
        None,
        false,
    );
    sketch_cost(&dense.strokes);
    measure(
        "rail-full",
        3840,
        2160,
        1.0,
        View::default(),
        &sparse,
        &toolbar,
        None,
        false,
    );
    // A whole-surface frame that follows a document change: the document is rasterised again.
    measure(
        "rail-rebuild",
        3840,
        2160,
        1.0,
        View::default(),
        &sparse,
        &toolbar,
        None,
        true,
    );
    // The widest capsule the rail can open, the shape the pop-out animation draws every frame.
    let hovered = Overlay {
        toolbar: Some(Toolbar {
            hover: Some(ToolbarAction::Tool(Tool::Text)),
            ..rail_state()
        }),
        ..Default::default()
    };
    measure(
        "rail-hover-full",
        3840,
        2160,
        1.0,
        View::default(),
        &sparse,
        &hovered,
        None,
        false,
    );
    // A frame of that animation: only the rail's box is damaged.
    measure(
        "rail-hover-damaged",
        3840,
        2160,
        1.0,
        View::default(),
        &sparse,
        &hovered,
        Some(toolbar_bounds(3840.0, 2160.0)),
        false,
    );
    measure(
        "text-edit-full",
        1920,
        1080,
        1.0,
        View::default(),
        &small,
        &text_overlay(1920.0, 1080.0, ""),
        None,
        false,
    );
    // A keystroke: the box of the text being edited is the only thing damaged.
    let typing = text_overlay(1920.0, 1080.0, "");
    let typing_damage = typing
        .text
        .as_ref()
        .expect("text overlay")
        .item
        .paint_bounds();
    measure(
        "text-edit-damaged",
        1920,
        1080,
        1.0,
        View::default(),
        &small,
        &typing,
        Some(typing_damage),
        false,
    );
    // The frame that reports a committed stroke: the document is rasterised again, but only the
    // stroke's own box is filled, so this is the one frame a change still costs.
    // A pan or a zoom rebuilds the whole document raster, because the raster is screen-space and a
    // view change moves every pixel of it: this is that frame, over a dense drawing, and it is the
    // cost of moving the canvas at all.
    measure(
        "pan-cold-dense",
        3840,
        2160,
        1.0,
        View::default(),
        &dense,
        &Overlay::default(),
        None,
        true,
    );
    // The frame a pan or a zoom produces, at both sizes the change is held to: a cold raster, a
    // whole-surface damage and a view that is not the identity, over a dense document.
    for (w, h) in [(2560, 1440), (3840, 2160)] {
        let dense = document(200, w as f32, h as f32);
        measure(
            "view-change",
            w,
            h,
            1.0,
            View {
                zoom: 1.0,
                offset: [-200.0, -120.0],
            },
            &dense,
            &Overlay::default(),
            None,
            true,
        );
    }
    for (w, h) in [(2560, 1440), (3840, 2160)] {
        let dense = document(200, w as f32, h as f32);
        measure_at(
            "view-change-coarse",
            w,
            h,
            1.0,
            View {
                zoom: 1.0,
                offset: [-200.0, -120.0],
            },
            &dense,
            &Overlay::default(),
            None,
            true,
            MOTION_QUALITY,
        );
    }
    measure(
        "dense-commit",
        3840,
        2160,
        1.0,
        View::default(),
        &dense,
        &Overlay::default(),
        Some(drag_damage),
        true,
    );
    measure(
        "text-edit-rebuild",
        1920,
        1080,
        1.0,
        View::default(),
        &small,
        &text_overlay(1920.0, 1080.0, ""),
        None,
        true,
    );
    measure(
        "preedit-full",
        1920,
        1080,
        1.0,
        View::default(),
        &small,
        &text_overlay(1920.0, 1080.0, "ni"),
        None,
        false,
    );
    measure(
        "scale2-full",
        1920,
        1080,
        2.0,
        View::default(),
        &small,
        &Overlay::default(),
        None,
        false,
    );
}
