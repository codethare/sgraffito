//! Frame composition: tiny-skia draws strokes and the eraser marker, cosmic-text draws text.
//!
//! `buf` is a tiny-skia premultiplied RGBA buffer already sized as logical size x scale.

use cosmic_text::{Attrs, Buffer, Color, FontSystem, Metrics, Shaping, SwashCache};
use tiny_skia::{
    LineCap, LineJoin, Paint, PathBuilder, PixmapMut, PremultipliedColorU8, Stroke as SkStroke,
    Transform,
};

use crate::canvas::{Hint, OutputAnnotations, Overlay, Rect, Stroke, TextItem, TextOverlay};

/// Radius of the eraser marker circle, in logical pixels.
const ERASER_MARKER_RADIUS: f32 = 8.0;
/// Position, size and text of the edit-mode hint, in logical pixels.
const HINT_ORIGIN: [f32; 2] = [16.0, 16.0];
const HINT_SIZE: f32 = 14.0;
const HINT_SWATCH: f32 = 11.0;
const HINT_GAP: f32 = 7.0;
const HINT_COLOR: &str = "#ffffff";
/// Everything the hint has to say beyond the active tool and the colour swatch.
const HINT_KEYS: &str = "P pen · E eraser · T text: click to place · 1-5 colour · Esc lock";
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

    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        buf: &mut [u8],
        width: u32,
        height: u32,
        scale: f32,
        ann: &OutputAnnotations,
        overlay: &Overlay,
        damage: Option<Rect>,
    ) {
        let Some(mut pixmap) = PixmapMut::from_bytes(buf, width, height) else {
            return;
        };
        for s in &ann.strokes {
            // Strokes outside the damaged box already hold the right pixels in this buffer.
            if let Some(damage) = damage
                && !s.bounds().is_some_and(|b| b.intersects(damage))
            {
                continue;
            }
            draw_stroke(&mut pixmap, s, scale);
        }
        for t in &ann.texts {
            // Text is culled by its conservative paint box, which `damage_box` has grown the
            // damage box to contain, so a drawn item is always inside the region being swapped.
            if let Some(damage) = damage
                && !t.paint_bounds().intersects(damage)
            {
                continue;
            }
            self.draw_text(&mut pixmap, t, scale, None);
        }
        if let Some(s) = &overlay.stroke {
            draw_stroke(&mut pixmap, s, scale);
        }
        if let Some(t) = &overlay.text {
            self.draw_text(&mut pixmap, &t.item, scale, Some(t));
        }
        if let Some(hint) = &overlay.hint {
            self.draw_hint(&mut pixmap, hint, scale);
        }
        if let Some([x, y]) = overlay.eraser {
            draw_eraser_marker(&mut pixmap, x * scale, y * scale, scale);
        }
    }

    /// The edit-mode affordance: a swatch of the active colour plus the key list, with the
    /// active tool in brackets so it is never ambiguous which one is armed.
    fn draw_hint(&mut self, pixmap: &mut PixmapMut, hint: &Hint, scale: f32) {
        let (r, g, b) = parse_hex(hint.color);
        fill(
            pixmap,
            HINT_ORIGIN[0] * scale,
            HINT_ORIGIN[1] * scale,
            HINT_SWATCH * scale,
            HINT_SWATCH * scale,
            Color::rgba(r, g, b, 255),
        );
        let item = hint_item(hint);
        self.draw_text(pixmap, &item, scale, None);
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

/// Region of a frame that has to be touched. Anything outside a `Rect` already holds
/// the pixels of the frame before it, which is only true for the buffer the previous
/// frame was drawn into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DamageRegion {
    /// Every pixel of the surface.
    All,
    /// One half-open device rectangle, `[x0, x1) x [y0, y1)`.
    Rect { x0: u32, y0: u32, x1: u32, y1: u32 },
}

impl DamageRegion {
    /// The device region a logical damage rectangle covers, clipped to the surface.
    pub fn from_logical(damage: Rect, scale: f32, w: u32, h: u32) -> Self {
        let clamp = |v: f32, max: u32| v.max(0.0).min(max as f32) as u32;
        DamageRegion::Rect {
            x0: clamp(damage.x * scale, w),
            y0: clamp(damage.y * scale, h),
            x1: clamp((damage.x + damage.w) * scale, w),
            y1: clamp((damage.y + damage.h) * scale, h),
        }
    }

    /// The region clipped to a `w` x `h` surface, as `[x0, x1) x [y0, y1)`.
    fn rows(self, w: u32, h: u32) -> (u32, u32, u32, u32) {
        match self {
            DamageRegion::All => (0, 0, w, h),
            DamageRegion::Rect { x0, y0, x1, y1 } => (x0, y0, x1.min(w), y1.min(h)),
        }
    }

    /// Clears the region to transparent.
    pub fn clear(self, buf: &mut [u8], w: u32, h: u32) {
        let (x0, y0, x1, y1) = self.rows(w, h);
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        if let DamageRegion::All = self {
            // Row-by-row fills cost a little more than one memset over the whole buffer.
            buf.fill(0);
            return;
        }
        for y in y0..y1 {
            let (start, end) = (((y * w + x0) * 4) as usize, ((y * w + x1) * 4) as usize);
            buf[start..end].fill(0);
        }
    }

    /// Swaps the R and B bytes inside the region. tiny-skia is premultiplied RGBA and
    /// `wl_shm` ARGB8888 is BGRA byte order on little endian, so this is mandatory and
    /// makes the region single-use until it is cleared again.
    pub fn swap_rb(self, buf: &mut [u8], w: u32, h: u32) {
        if let DamageRegion::All = self {
            // Measured: one flat pass is ~45% faster than the same swap row by row.
            for c in buf.as_chunks_mut::<4>().0 {
                c.swap(0, 2);
            }
            return;
        }
        let (x0, y0, x1, y1) = self.rows(w, h);
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        for y in y0..y1 {
            let (start, end) = (((y * w + x0) * 4) as usize, ((y * w + x1) * 4) as usize);
            for c in buf[start..end].as_chunks_mut::<4>().0 {
                c.swap(0, 2);
            }
        }
    }
}

/// Damage box for a bounding-box frame: `from` grown until it contains every element the
/// renderer will paint inside it. Painting outside the box is not allowed, because those
/// bytes still hold the previous frame's already-swapped pixels; `render` culls the
/// elements left outside, which is why they may keep those pixels.
///
/// ponytail: growing can cascade into a large box when the drag touches a long stroke over a
/// dense drawing, and then the frame costs nearly a whole-surface one. That is the honest
/// price of redrawing that stroke where the box overlaps it.
pub fn damage_box(ann: &OutputAnnotations, overlay: &Overlay, from: Rect) -> Rect {
    let mut damage = from;
    loop {
        let mut grown = damage;
        for s in &ann.strokes {
            if let Some(b) = s.bounds()
                && b.intersects(grown)
            {
                grown = grown.union(b);
            }
        }
        for t in &ann.texts {
            let b = t.paint_bounds();
            if b.intersects(grown) {
                grown = grown.union(b);
            }
        }
        if let Some(t) = &overlay.text {
            let b = t.item.paint_bounds();
            if b.intersects(grown) {
                grown = grown.union(b);
            }
        }
        // The hint is painted every frame, so it always has to be inside the box.
        if let Some(hint) = &overlay.hint {
            grown = grown.union(hint_bounds(hint));
        }
        if grown == damage {
            return damage;
        }
        damage = grown;
    }
}

/// Bounding box of the transient overlay elements of a frame, in logical pixels: the
/// in-progress stroke and the eraser marker. `None` when the overlay draws neither.
pub fn transient_bounds(overlay: &Overlay) -> Option<Rect> {
    let stroke = overlay.stroke.as_ref().and_then(Stroke::bounds);
    let eraser = overlay.eraser.map(|[x, y]| {
        Rect {
            x,
            y,
            w: 0.0,
            h: 0.0,
        }
        .grown(ERASER_MARKER_RADIUS + 2.0)
    });
    match (stroke, eraser) {
        (Some(a), Some(b)) => Some(a.union(b)),
        (a, b) => a.or(b),
    }
}

/// The text line of the hint. `draw_hint` and the damage accounting share it so they can
/// never disagree about where the hint is.
fn hint_item(hint: &Hint) -> TextItem {
    TextItem {
        x: HINT_ORIGIN[0] + HINT_SWATCH + HINT_GAP,
        y: HINT_ORIGIN[1],
        color: HINT_COLOR.into(),
        size: HINT_SIZE,
        text: format!("[{}]  {HINT_KEYS}", hint.tool),
    }
}

/// Everything the hint paints.
fn hint_bounds(hint: &Hint) -> Rect {
    Rect {
        x: HINT_ORIGIN[0],
        y: HINT_ORIGIN[1],
        w: HINT_SWATCH,
        h: HINT_SWATCH,
    }
    .union(hint_item(hint).paint_bounds())
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
            let Some(path) = smoothed_path(points, scale) else {
                return;
            };
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

/// The path a stroke is drawn along: a quadratic curve through the midpoint of every
/// pair of consecutive samples, with the interior samples as control points, so a slow
/// drag does not render as a chain of straight segments. The stored samples are never
/// modified. Two samples are a straight line; the caller draws a single sample as a dot.
pub fn smoothed_path(points: &[[f32; 2]], scale: f32) -> Option<tiny_skia::Path> {
    let (first, rest) = points.split_first()?;
    let mut pb = PathBuilder::new();
    pb.move_to(first[0] * scale, first[1] * scale);
    for w in rest.windows(2) {
        pb.quad_to(
            w[0][0] * scale,
            w[0][1] * scale,
            (w[0][0] + w[1][0]) / 2.0 * scale,
            (w[0][1] + w[1][1]) / 2.0 * scale,
        );
    }
    if let Some(last) = rest.last() {
        pb.line_to(last[0] * scale, last[1] * scale);
    }
    pb.finish()
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
