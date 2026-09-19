//! Frame composition: tiny-skia draws strokes and the eraser marker, cosmic-text draws text.
//!
//! `buf` is a tiny-skia premultiplied RGBA buffer already sized as logical size x scale.

use cosmic_text::{Attrs, Buffer, Color, FontSystem, Metrics, Shaping, SwashCache};
use tiny_skia::{
    LineCap, LineJoin, Paint, PathBuilder, PixmapMut, PremultipliedColorU8, Stroke as SkStroke,
    Transform,
};

use crate::canvas::{OutputAnnotations, Overlay, Stroke, TextItem, TextOverlay};

/// Radius of the eraser marker circle, in logical pixels.
const ERASER_MARKER_RADIUS: f32 = 8.0;
const UNDERLINE_HEIGHT: f32 = 1.5;
const CURSOR_WIDTH: f32 = 2.0;
const LINE_HEIGHT_SCALE: f32 = 1.2;

pub struct Renderer {
    font_system: FontSystem,
    cache: SwashCache,
}

impl Default for Renderer {
    fn default() -> Self {
        Self::new()
    }
}

impl Renderer {
    pub fn new() -> Self {
        Self {
            font_system: FontSystem::new(),
            cache: SwashCache::new(),
        }
    }

    pub fn render(
        &mut self,
        buf: &mut [u8],
        width: u32,
        height: u32,
        scale: f32,
        ann: &OutputAnnotations,
        overlay: &Overlay,
    ) {
        let Some(mut pixmap) = PixmapMut::from_bytes(buf, width, height) else {
            return;
        };
        for s in &ann.strokes {
            draw_stroke(&mut pixmap, s, scale);
        }
        for t in &ann.texts {
            self.draw_text(&mut pixmap, t, scale, None);
        }
        if let Some(s) = &overlay.stroke {
            draw_stroke(&mut pixmap, s, scale);
        }
        if let Some(t) = &overlay.text {
            self.draw_text(&mut pixmap, &t.item, scale, Some(t));
        }
        if let Some([x, y]) = overlay.eraser {
            draw_eraser_marker(&mut pixmap, x * scale, y * scale, scale);
        }
    }

    fn draw_text(
        &mut self,
        pixmap: &mut PixmapMut,
        item: &TextItem,
        scale: f32,
        edit: Option<&TextOverlay>,
    ) {
        let size = item.size * scale;
        let mut buffer = Buffer::new(
            &mut self.font_system,
            Metrics::new(size, size * LINE_HEIGHT_SCALE),
        );
        buffer.set_size(Some(pixmap.width() as f32), None);

        let (committed, preedit) = match edit {
            Some(e) => (e.buffer.text.as_str(), e.preedit.as_str()),
            None => (item.text.as_str(), ""),
        };
        let display = format!("{committed}{preedit}");
        if display.is_empty() && edit.is_none() {
            return;
        }
        buffer.set_text(&display, &Attrs::new(), Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.font_system, false);

        // Pre-scan the layout for the preedit start, the last line width and the baseline.
        let (mut line_w, mut baseline) = (0.0f32, size * 0.8);
        let mut preedit_x0 = None;
        for run in buffer.layout_runs() {
            line_w = run.line_w;
            baseline = run.line_y;
            for g in run.glyphs {
                if g.start >= committed.len() && preedit_x0.is_none() {
                    preedit_x0 = Some(g.x);
                }
            }
        }

        let (ox, oy) = (item.x * scale, item.y * scale);
        let (r, g, b) = parse_hex(&item.color);
        let color = Color::rgba(r, g, b, 255);
        let (clip_w, clip_h) = (pixmap.width(), pixmap.height());
        buffer.draw(
            &mut self.font_system,
            &mut self.cache,
            color,
            |x, y, w, h, c| {
                blend(
                    pixmap,
                    clip_w,
                    clip_h,
                    ox as i32 + x,
                    oy as i32 + y,
                    w,
                    h,
                    c,
                )
            },
        );

        if let Some(e) = edit {
            let line_top = oy + baseline - size * 0.8;
            if !e.preedit.is_empty() {
                let x0 = ox + preedit_x0.unwrap_or(line_w);
                fill(
                    pixmap,
                    x0,
                    oy + baseline + size * 0.15,
                    (ox + line_w - x0).max(0.0),
                    UNDERLINE_HEIGHT * scale,
                    color,
                );
            }
            fill(
                pixmap,
                ox + line_w,
                line_top,
                CURSOR_WIDTH * scale,
                size * LINE_HEIGHT_SCALE,
                color,
            );
        }
    }
}

fn draw_stroke(pixmap: &mut PixmapMut, s: &Stroke, scale: f32) {
    let (r, g, b) = parse_hex(&s.color);
    let mut paint = Paint::default();
    paint.set_color_rgba8(r, g, b, 255);
    paint.anti_alias = true;
    let width = s.width * scale;
    let transform = Transform::identity();

    match s.points.as_slice() {
        [] => {}
        [only] => {
            let mut pb = PathBuilder::new();
            pb.push_circle(only[0] * scale, only[1] * scale, width / 2.0);
            if let Some(path) = pb.finish() {
                pixmap.fill_path(&path, &paint, tiny_skia::FillRule::Winding, transform, None);
            }
        }
        points => {
            let mut pb = PathBuilder::new();
            pb.move_to(points[0][0] * scale, points[0][1] * scale);
            for p in &points[1..] {
                pb.line_to(p[0] * scale, p[1] * scale);
            }
            let Some(path) = pb.finish() else { return };
            let stroke = SkStroke {
                width,
                line_cap: LineCap::Round,
                line_join: LineJoin::Round,
                ..Default::default()
            };
            pixmap.stroke_path(&path, &paint, &stroke, transform, None);
        }
    }
}

fn draw_eraser_marker(pixmap: &mut PixmapMut, x: f32, y: f32, scale: f32) {
    let mut pb = PathBuilder::new();
    pb.push_circle(x, y, ERASER_MARKER_RADIUS * scale);
    let Some(path) = pb.finish() else { return };
    let mut paint = Paint::default();
    paint.set_color_rgba8(255, 255, 255, 220);
    let stroke = SkStroke {
        width: 2.0 * scale,
        line_cap: LineCap::Round,
        ..Default::default()
    };
    pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
}

/// Blend `Color` (alpha included) into a premultiplied buffer using src-over.
#[allow(clippy::too_many_arguments)]
fn blend(
    pixmap: &mut PixmapMut,
    clip_w: u32,
    clip_h: u32,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    c: Color,
) {
    let a = c.a() as f32 / 255.0;
    if a <= 0.0 {
        return;
    }
    let inv = 1.0 - a;
    for py in y..y + h as i32 {
        for px in x..x + w as i32 {
            if px < 0 || py < 0 || px >= clip_w as i32 || py >= clip_h as i32 {
                continue;
            }
            let idx = (py * clip_w as i32 + px) as usize;
            let dst = pixmap.pixels_mut()[idx];
            let out_a = (c.a() as f32 + dst.alpha() as f32 * inv)
                .round()
                .clamp(0.0, 255.0) as u8;
            // A source channel contributes s * a in premultiplied space; clamp to alpha to keep the invariant.
            let ch = |s: u8, d: u8| {
                (s as f32 * a + d as f32 * inv)
                    .round()
                    .clamp(0.0, out_a as f32) as u8
            };
            let out = PremultipliedColorU8::from_rgba(
                ch(c.r(), dst.red()),
                ch(c.g(), dst.green()),
                ch(c.b(), dst.blue()),
                out_a,
            )
            .unwrap();
            pixmap.pixels_mut()[idx] = out;
        }
    }
}

/// Fill a solid rectangle (cursor, underline).
fn fill(pixmap: &mut PixmapMut, x: f32, y: f32, w: f32, h: f32, color: Color) {
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let Some(rect) = tiny_skia::Rect::from_xywh(x, y, w, h) else {
        return;
    };
    let mut pb = PathBuilder::new();
    pb.push_rect(rect);
    let Some(path) = pb.finish() else { return };
    let mut paint = Paint::default();
    paint.set_color_rgba8(color.r(), color.g(), color.b(), color.a());
    pixmap.fill_path(
        &path,
        &paint,
        tiny_skia::FillRule::Winding,
        Transform::identity(),
        None,
    );
}

fn parse_hex(s: &str) -> (u8, u8, u8) {
    let hex = s.trim_start_matches('#');
    if hex.len() != 6 {
        return (255, 255, 255);
    }
    let v = u32::from_str_radix(hex, 16).unwrap_or(0xff_ffff);
    ((v >> 16) as u8, (v >> 8) as u8, v as u8)
}
