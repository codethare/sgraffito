use std::hint::black_box;
use std::time::Instant;

use sgraffito::canvas::{
    Hint, OutputAnnotations, Overlay, Rect, Stroke, TextBuffer, TextItem, TextOverlay,
};
use sgraffito::render::{DamageRegion, Renderer, damage_box, transient_bounds};

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

fn measure(
    name: &str,
    logical_width: u32,
    logical_height: u32,
    scale: f32,
    annotations: &OutputAnnotations,
    overlay: &Overlay,
    damage: Option<Rect>,
) {
    let width = (logical_width as f32 * scale) as u32;
    let height = (logical_height as f32 * scale) as u32;
    let mut buffer = vec![0u8; (width * height * 4) as usize];
    let region = damage.map(|d| DamageRegion::from_logical(d, scale, width, height));
    let mut renderer = Renderer::new();

    for _ in 0..WARMUP {
        render_frame(
            &mut renderer,
            &mut buffer,
            width,
            height,
            scale,
            annotations,
            overlay,
            damage,
            region,
        );
    }

    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let start = Instant::now();
        render_frame(
            &mut renderer,
            &mut buffer,
            width,
            height,
            scale,
            annotations,
            overlay,
            damage,
            region,
        );
        samples.push(start.elapsed().as_nanos() as u64);
    }
    black_box(&buffer);

    samples.sort_unstable();
    let median = samples[samples.len() / 2] as f64 / 1_000.0;
    let p95 = samples[(samples.len() * 95 / 100).min(samples.len() - 1)] as f64 / 1_000.0;
    println!(
        "{name}: median={median:.1}us p95={p95:.1}us logical={logical_width}x{logical_height} scale={scale:.1} buffer={width}x{height} strokes={} texts={} damage={} hint={} text={}",
        annotations.strokes.len(),
        annotations.texts.len(),
        region.is_some(),
        overlay.hint.is_some(),
        overlay.text.is_some(),
    );
}

#[allow(clippy::too_many_arguments)]
fn render_frame(
    renderer: &mut Renderer,
    buffer: &mut [u8],
    width: u32,
    height: u32,
    scale: f32,
    annotations: &OutputAnnotations,
    overlay: &Overlay,
    damage: Option<Rect>,
    region: Option<DamageRegion>,
) {
    let region = region.unwrap_or(DamageRegion::All);
    region.clear(buffer, width, height);
    renderer.render(buffer, width, height, scale, annotations, overlay, damage);
}

fn main() {
    let sparse = document(30, 3840.0, 2160.0);
    let small = document(30, 1920.0, 1080.0);
    let dense = document(200, 3840.0, 2160.0);
    let hint = Overlay {
        hint: Some(Hint {
            tool: "pen",
            color: "#e01b24",
            size: 3.0,
        }),
        ..Default::default()
    };
    let drag = Overlay {
        stroke: Some(live_stroke(3840.0, 2160.0)),
        ..Default::default()
    };
    let drag_damage = damage_box(
        &sparse,
        &drag,
        transient_bounds(&drag).expect("drag has bounds"),
        3840.0,
    );
    measure(
        "sparse-drag",
        3840,
        2160,
        1.0,
        &sparse,
        &drag,
        Some(drag_damage),
    );
    measure("dense-drag", 3840, 2160, 1.0, &dense, &drag, None);
    measure("hint-full", 3840, 2160, 1.0, &sparse, &hint, None);
    measure(
        "text-edit-full",
        1920,
        1080,
        1.0,
        &small,
        &text_overlay(1920.0, 1080.0, ""),
        None,
    );
    measure(
        "preedit-full",
        1920,
        1080,
        1.0,
        &small,
        &text_overlay(1920.0, 1080.0, "ni"),
        None,
    );
    measure(
        "scale2-full",
        1920,
        1080,
        2.0,
        &small,
        &Overlay::default(),
        None,
    );
}
