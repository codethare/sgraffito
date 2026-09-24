//! Frame composition: tiny-skia draws strokes and the eraser marker, cosmic-text draws text.
//!
//! `buf` is a tiny-skia premultiplied RGBA buffer already sized as logical size x scale.

use cosmic_text::{Attrs, Buffer, Color, FontSystem, Metrics, Shaping, SwashCache};
use tiny_skia::{
    LineCap, LineJoin, Paint, PathBuilder, PixmapMut, PremultipliedColorU8, Stroke as SkStroke,
    Transform,
};
use wayland_client::protocol::wl_shm::Format;

use crate::canvas::{Hint, OutputAnnotations, Overlay, Rect, Stroke, TextItem, TextOverlay};
/// Radius of the eraser marker circle, in logical pixels.
const ERASER_MARKER_RADIUS: f32 = 8.0;
/// The edit-mode hint: a capsule in the macOS HUD idiom, built around the 13 pt control size
/// the HIG gives as the macOS default. All of it is logical pixels.
const HINT_TOP: f32 = 16.0;
const HINT_PAD: [f32; 2] = [12.0, 6.0];
const HINT_HEIGHT: f32 = 34.0;
/// A capsule is a rectangle whose corner radius is half its height.
const HINT_RADIUS: f32 = HINT_HEIGHT / 2.0;
const HINT_HAIRLINE: f32 = 1.0;
const HINT_LABEL_SIZE: f32 = 13.0;
const HINT_KEY_SIZE: f32 = 12.0;
const HINT_KEY_MIN: f32 = 8.0;
const HINT_KEY_PAD: f32 = 7.0;
const HINT_KEY_HEIGHT: f32 = 22.0;
const HINT_KEY_RADIUS: f32 = 6.0;
const HINT_GAP: f32 = 7.0;
const HINT_ITEM_GAP: f32 = 14.0;
const HINT_WELL: f32 = 16.0;
/// Widest capsule the damage accounting reserves room for. It keeps the reserved box a
/// centred strip instead of the full width of the output: a full-width strip unioned with a
/// grown drawing box covers nearly the whole surface and the fast path disappears. The size
/// key and its value grew the capsule from ~508 to ~707 px, hence the reserve.
const HINT_RESERVE: f32 = 720.0;
const HINT_WELL_RADIUS: f32 = 4.0;
/// `#rrggbbaa`. The pill fill stands in for a HUD material: a `wl_shm` layer surface cannot be
/// blurred, so there is translucency but no real vibrancy.
const HINT_PILL_FILL: &str = "#1c1c1ec7";
const HINT_PILL_EDGE: &str = "#ffffff1f";
const HINT_KEY_FILL: &str = "#ffffff26";
const HINT_KEY_EDGE: &str = "#ffffff2e";
/// macOS system accent, used for the active tool's keycap.
const HINT_ACCENT: &str = "#0a84ffff";
const HINT_KEY_TEXT: &str = "#ffffffee";
const HINT_LABEL_TEXT: &str = "#ebebf59e";
const HINT_LABEL_ACTIVE: &str = "#fffffff2";
const HINT_WELL_EDGE: &str = "#ffffff4d";
/// The keys and what they do, in the order the capsule shows them. The size key is
/// contextual: it adjusts the pen width, or the text size while the text tool is active.
fn hint_items(hint: &Hint) -> Vec<(&'static str, String)> {
    let size_name = if hint.tool == "text" { "size" } else { "width" };
    vec![
        ("1-5", "colour".into()),
        ("P", "pen".into()),
        ("E", "eraser".into()),
        ("T", "text: click to place".into()),
        ("[ ]", format!("{size_name} {:.0}", hint.size)),
        ("Esc", "end text, again locks".into()),
    ]
}
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
            // The hint is static content: only a tool, colour, size or mode change repaints it,
            // and those force a whole-surface frame. A bounding-box frame that does not reach it
            // must leave it alone — those bytes are outside the damage region and still hold
            // the previous frame.
            let box_ = hint_bounds(pixmap.width() as f32 / scale);
            if damage.is_none_or(|d| box_.intersects(d)) {
                self.draw_hint(&mut pixmap, hint, scale);
            }
        }
        if let Some([x, y]) = overlay.eraser {
            draw_eraser_marker(&mut pixmap, x * scale, y * scale, scale);
        }
    }

    /// The edit-mode affordance. macOS idiom: a capsule, keys drawn as keycaps and their
    /// meaning as secondary label text, with the active tool's keycap in the system accent.
    fn draw_hint(&mut self, pixmap: &mut PixmapMut, hint: &Hint, scale: f32) {
        let surface = pixmap.width() as f32 / scale;
        // The descriptions are dropped first when the capsule would not fit: the keycaps
        // alone still teach the shortcuts.
        let Some(pill) = self
            .hint_pill(hint, surface, true)
            .or_else(|| self.hint_pill(hint, surface, false))
        else {
            return;
        };
        round_rect(
            pixmap,
            Rect {
                x: pill.x * scale,
                y: HINT_TOP * scale,
                w: pill.width * scale,
                h: HINT_HEIGHT * scale,
            },
            HINT_RADIUS * scale,
            parse_hex(HINT_PILL_FILL),
            Some((parse_hex(HINT_PILL_EDGE), HINT_HAIRLINE * scale)),
        );
        for (piece, x, w) in &pill.pieces {
            match piece {
                Piece::Well => round_rect(
                    pixmap,
                    Rect {
                        x: (pill.x + x) * scale,
                        y: (HINT_TOP + (HINT_HEIGHT - HINT_WELL) / 2.0) * scale,
                        w: HINT_WELL * scale,
                        h: HINT_WELL * scale,
                    },
                    HINT_WELL_RADIUS * scale,
                    parse_hex(hint.color),
                    Some((parse_hex(HINT_WELL_EDGE), HINT_HAIRLINE * scale)),
                ),
                Piece::Key(text, active) => {
                    let top = HINT_TOP + (HINT_HEIGHT - HINT_KEY_HEIGHT) / 2.0;
                    let (fill, edge) = if *active {
                        (HINT_ACCENT, HINT_ACCENT)
                    } else {
                        (HINT_KEY_FILL, HINT_KEY_EDGE)
                    };
                    round_rect(
                        pixmap,
                        Rect {
                            x: (pill.x + x) * scale,
                            y: top * scale,
                            w: w * scale,
                            h: HINT_KEY_HEIGHT * scale,
                        },
                        HINT_KEY_RADIUS * scale,
                        parse_hex(fill),
                        Some((parse_hex(edge), HINT_HAIRLINE * scale)),
                    );
                    // Centre the glyph in its keycap.
                    let glyph = self.text_width(text, HINT_KEY_SIZE);
                    let item = hint_text(
                        pill.x + x + (w - glyph) / 2.0,
                        top,
                        HINT_KEY_HEIGHT,
                        HINT_KEY_SIZE,
                        HINT_KEY_TEXT,
                        text,
                    );
                    self.draw_text(pixmap, &item, scale, None);
                }
                Piece::Label(text, active) => {
                    let item = hint_text(
                        pill.x + x,
                        HINT_TOP,
                        HINT_HEIGHT,
                        HINT_LABEL_SIZE,
                        if *active {
                            HINT_LABEL_ACTIVE
                        } else {
                            HINT_LABEL_TEXT
                        },
                        text,
                    );
                    self.draw_text(pixmap, &item, scale, None);
                }
            }
        }
    }

    /// Lay the capsule out from left to right, measuring each keycap and label. `None` when
    /// the result would not fit on the surface.
    fn hint_pill(&mut self, hint: &Hint, surface: f32, labels: bool) -> Option<HintPill> {
        let active = active_key(hint.tool);
        let items = hint_items(hint);
        let mut pieces = vec![(Piece::Well, HINT_PAD[0], HINT_WELL)];
        let mut cursor = HINT_PAD[0] + HINT_WELL + HINT_ITEM_GAP;
        for (i, (key, label)) in items.iter().enumerate() {
            let is_active = *key == active;
            let width = self.text_width(key, HINT_KEY_SIZE).max(HINT_KEY_MIN) + 2.0 * HINT_KEY_PAD;
            pieces.push((Piece::Key(key, is_active), cursor, width));
            cursor += width;
            if labels {
                cursor += HINT_GAP;
                let width = self.text_width(label, HINT_LABEL_SIZE);
                pieces.push((Piece::Label(label.clone(), is_active), cursor, width));
                cursor += width;
            }
            if i + 1 < items.len() {
                cursor += HINT_ITEM_GAP;
            }
        }
        let width = cursor + HINT_PAD[0];
        if width > surface.min(HINT_RESERVE) {
            return None;
        }
        Some(HintPill {
            x: (surface - width) / 2.0,
            width,
            pieces,
        })
    }

    /// Width of one line of text, in logical pixels. The shaped buffer is thrown away: the
    /// hint is a handful of short strings and it is only drawn while editing.
    fn text_width(&mut self, text: &str, size: f32) -> f32 {
        let mut buffer = Buffer::new(
            &mut self.font_system,
            Metrics::new(size, size * LINE_HEIGHT_SCALE),
        );
        buffer.set_size(None, None);
        buffer.set_text(text, &Attrs::new(), Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.font_system, false);
        buffer.layout_runs().map(|r| r.line_w).fold(0.0, f32::max)
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

        // Pre-scan the layout for the preedit start, and for the caret box and baseline of the
        // last line: the cursor is always at the end, so that is the line it is drawn on.
        let (mut line_w, mut baseline) = (0.0f32, size * 0.8);
        let (mut line_top, mut line_height) = (0.0f32, size * LINE_HEIGHT_SCALE);
        let mut preedit_x0 = None;
        for run in buffer.layout_runs() {
            line_w = run.line_w;
            baseline = run.line_y;
            line_top = run.line_top;
            line_height = run.line_height;
            for g in run.glyphs {
                if g.start >= committed.len() && preedit_x0.is_none() {
                    preedit_x0 = Some(g.x);
                }
            }
        }

        let (ox, oy) = (item.x * scale, item.y * scale);
        let c = parse_hex(&item.color);
        let color = Color::rgba(c[0], c[1], c[2], c[3]);
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
            // The caret is the line box the text sits in, so it matches the typed text instead
            // of floating at a hardcoded fraction of the font size (the empty-line baseline and
            // a real glyph's baseline are not the same fraction).
            fill(
                pixmap,
                ox + line_w,
                oy + line_top,
                CURSOR_WIDTH * scale,
                line_height,
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

    /// Swaps the R and B bytes inside the region, turning a tiny-skia premultiplied RGBA
    /// buffer into `wl_shm` ARGB8888 (B, G, R, A on little endian). Only the `Argb8888`
    /// fallback of [`buffer_format`] needs it; either way the swapped bytes are single-use
    /// until the region is cleared again.
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

/// The `wl_shm` format to allocate buffers in, and whether the frame's bytes then need the
/// R/B swap. On little endian `Abgr8888` is R, G, B, A — the order tiny-skia writes
/// (premultiplied) — so it needs no swap; the mandatory `Argb8888` is B, G, R, A and does.
pub fn buffer_format(formats: &[Format]) -> (Format, bool) {
    if formats.contains(&Format::Abgr8888) {
        (Format::Abgr8888, false)
    } else {
        (Format::Argb8888, true)
    }
}

/// Damage box for a bounding-box frame: `from` grown until it contains every element the
/// renderer will paint inside it. Painting outside the box is not allowed, because those
/// bytes still hold the previous frame's pixels; `render` culls the elements left outside,
/// which is why they may keep those pixels.
///
/// ponytail: growing can cascade into a large box when the drag touches a long stroke over a
/// dense drawing, and then the frame costs nearly a whole-surface one. That is the honest
/// price of redrawing that stroke where the box overlaps it.
pub fn damage_box(ann: &OutputAnnotations, overlay: &Overlay, from: Rect, width: f32) -> Rect {
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
        // The hint is only repainted when the box grows into it, so the growth has to cover
        // everything the hint paints. Reserving its box unconditionally would union a top strip
        // with a drawing box far below it and swallow nearly the whole surface; the hint itself
        // only changes on a tool, colour or mode switch, and those force a whole-surface frame.
        if overlay.hint.is_some() {
            let hint = hint_bounds(width);
            if hint.intersects(grown) {
                grown = grown.union(hint);
            }
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

/// The keycap of the tool the hint reports as the active one.
fn active_key(tool: &str) -> &str {
    match tool {
        "pen" => "P",
        "eraser" => "E",
        "text" => "T",
        _ => "",
    }
}

/// One laid-out element of the hint.
#[derive(Clone)]
enum Piece {
    /// The active colour; macOS calls this a colour well.
    Well,
    /// A keycap: the glyph and whether it is the active tool's key.
    Key(&'static str, bool),
    /// The meaning of the keycap before it.
    Label(String, bool),
}

/// A laid-out hint capsule: where it sits and what to paint, in logical pixels.
struct HintPill {
    x: f32,
    width: f32,
    pieces: Vec<(Piece, f32, f32)>,
}

/// A label vertically centred in a `height`-tall box whose top edge is `top`.
fn hint_text(x: f32, top: f32, height: f32, size: f32, color: &str, text: &str) -> TextItem {
    TextItem {
        x,
        y: top + (height - size * LINE_HEIGHT_SCALE) / 2.0,
        color: color.into(),
        size,
        text: text.into(),
    }
}

/// Everything the hint paints. The capsule is centred and its width depends on the font, so
/// the damage accounting reserves a centred box of the widest capsule `hint_pill` will build.
fn hint_bounds(width: f32) -> Rect {
    let reserved = HINT_RESERVE.min(width);
    Rect {
        x: (width - reserved) / 2.0,
        y: HINT_TOP - HINT_HAIRLINE,
        w: reserved,
        h: HINT_HEIGHT + 2.0 * HINT_HAIRLINE,
    }
}

/// A rounded rectangle with a fill and an optional hairline edge, in device pixels. The
/// corners are cubic approximations of a circle, so a radius of half the height is a real
/// capsule.
fn round_rect(
    pixmap: &mut PixmapMut,
    box_: Rect,
    radius: f32,
    fill: [u8; 4],
    edge: Option<([u8; 4], f32)>,
) {
    let Some(path) = rounded_rect_path(box_, radius) else {
        return;
    };
    let mut paint = Paint::default();
    paint.set_color_rgba8(fill[0], fill[1], fill[2], fill[3]);
    paint.anti_alias = true;
    pixmap.fill_path(
        &path,
        &paint,
        tiny_skia::FillRule::Winding,
        Transform::identity(),
        None,
    );
    if let Some((edge, width)) = edge {
        let mut paint = Paint::default();
        paint.set_color_rgba8(edge[0], edge[1], edge[2], edge[3]);
        paint.anti_alias = true;
        let stroke = SkStroke {
            width,
            line_cap: LineCap::Round,
            ..Default::default()
        };
        pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    }
}

/// Path of a rounded rectangle. tiny-skia has no rounded-rect helper, and quadratic corners
/// are visibly too flat at a capsule's radius, so the corners are cubic circle segments.
fn rounded_rect_path(box_: Rect, radius: f32) -> Option<tiny_skia::Path> {
    let Rect { x, y, w, h } = box_;
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let r = radius.min(w / 2.0).min(h / 2.0);
    // Cubic control-point distance that approximates a quarter circle.
    let k = r * 0.552_285;
    let (right, bottom) = (x + w, y + h);
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(right - r, y);
    pb.cubic_to(right - r + k, y, right, y + r - k, right, y + r);
    pb.line_to(right, bottom - r);
    pb.cubic_to(
        right,
        bottom - r + k,
        right - r + k,
        bottom,
        right - r,
        bottom,
    );
    pb.line_to(x + r, bottom);
    pb.cubic_to(x + r - k, bottom, x, bottom - r + k, x, bottom - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish()
}

fn draw_stroke(pixmap: &mut PixmapMut, s: &Stroke, scale: f32) {
    let c = parse_hex(&s.color);
    let mut paint = Paint::default();
    paint.set_color_rgba8(c[0], c[1], c[2], c[3]);
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

/// `#rrggbb` or `#rrggbbaa` to RGBA bytes.
fn parse_hex(s: &str) -> [u8; 4] {
    let hex = s.trim_start_matches('#');
    let v = u32::from_str_radix(hex, 16).unwrap_or(0xff_ffff);
    match hex.len() {
        8 => [(v >> 24) as u8, (v >> 16) as u8, (v >> 8) as u8, v as u8],
        6 => [(v >> 16) as u8, (v >> 8) as u8, v as u8, 255],
        _ => [255, 255, 255, 255],
    }
}
