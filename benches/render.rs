use std::hint::black_box;
use std::time::Instant;

use sgraffito::canvas::{
    OutputAnnotations, Overlay, Rect, Stroke, TextBuffer, TextItem, TextOverlay, Tool, Toolbar,
    ToolbarAction,
};
use sgraffito::render::{FrameCache, Renderer, toolbar_bounds, transient_bounds, transient_damage};

const WARMUP: usize = 8;
const SAMPLES: usize = 30;

fn line(y: f32, x0: f32, x1: f32, width: f32) -> Stroke {
    Stroke {
        color: "#e01b24".into(),
        width,
        points: vec![[x0, y], [x1, y]],
    }
}

fn document(stroke_count: usize, width: f32, height: f32) -> OutputAnnotations {
    let strokes = (0..stroke_count)
        .map(|i| {
            let y = height * (i as f32 + 1.0) / (stroke_count + 1) as f32;
            line(y, width * 0.1, width * 0.9, 3.0)
        })
        .collect();
    OutputAnnotations {
        strokes,
        texts: vec![TextItem {
            x: width * 0.1,
            y: height * 0.12,
            color: "#ffffff".into(),
            size: 24.0,
            text: "benchmark note".into(),
        }],
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
    annotations: &OutputAnnotations,
    overlay: &Overlay,
    damage: Option<Rect>,
    cold_raster: bool,
) {
    let width = (logical_width as f32 * scale) as u32;
    let height = (logical_height as f32 * scale) as u32;
    let mut buffer = vec![0u8; (width * height * 4) as usize];
    let mut renderer = Renderer::new();
    let mut cache = FrameCache::default();

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
        );
        samples.push(start.elapsed().as_nanos() as u64);
    }
    black_box(&buffer);

    samples.sort_unstable();
    let median = samples[samples.len() / 2] as f64 / 1_000.0;
    let p95 = samples[(samples.len() * 95 / 100).min(samples.len() - 1)] as f64 / 1_000.0;
    println!(
        "{name}: median={median:.1}us p95={p95:.1}us logical={logical_width}x{logical_height} scale={scale:.1} buffer={width}x{height} strokes={} texts={} damage={} toolbar={} text={} raster={}",
        annotations.strokes.len(),
        annotations.texts.len(),
        damage.is_some(),
        overlay.toolbar.is_some(),
        overlay.text.is_some(),
        if cold_raster { "cold" } else { "warm" },
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
    annotations: &OutputAnnotations,
    overlay: &Overlay,
    damage: Option<Rect>,
) {
    renderer.render_with_cache(
        buffer,
        width,
        height,
        scale,
        annotations,
        overlay,
        damage,
        cache,
        None,
    );
}

/// The rail with the pen armed and no block under the pointer, fully open.
fn rail_state() -> Toolbar {
    Toolbar {
        tool: Tool::Pen,
        color: "#e01b24",
        size: 3.0,
        hover: None,
        grow: 1.0,
    }
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
        &dense,
        &drag,
        Some(drag_damage),
        false,
    );
    measure("dense-full", 3840, 2160, 1.0, &dense, &drag, None, false);
    measure("rail-full", 3840, 2160, 1.0, &sparse, &toolbar, None, false);
    // A whole-surface frame that follows a document change: the document is rasterised again.
    measure(
        "rail-rebuild",
        3840,
        2160,
        1.0,
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
        &small,
        &typing,
        Some(typing_damage),
        false,
    );
    // The frame that reports a committed stroke: the document is rasterised again, but only the
    // stroke's own box is filled, so this is the one frame a change still costs.
    measure(
        "dense-commit",
        3840,
        2160,
        1.0,
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
        &small,
        &Overlay::default(),
        None,
        false,
    );
}
