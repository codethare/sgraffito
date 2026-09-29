//! Frame composition: tiny-skia draws strokes and the eraser marker, cosmic-text draws text.
//!
//! `buf` is a tiny-skia premultiplied RGBA buffer already sized as logical size x scale.

use std::borrow::Cow;

use cosmic_text::{Attrs, Buffer, Color, FontSystem, Metrics, Shaping, SwashCache};
use tiny_skia::{
    LineCap, LineJoin, Paint, PathBuilder, PixmapMut, PremultipliedColorU8, Stroke as SkStroke,
    Transform,
};
use wayland_client::protocol::wl_shm::Format;

use crate::canvas::{
    OutputAnnotations, Overlay, PALETTE, Rect, Stroke, TextItem, TextOverlay, Tool, Toolbar,
    ToolbarAction,
};
/// Radius of the eraser marker circle, in logical pixels.
const ERASER_MARKER_RADIUS: f32 = 8.0;
/// The edit-mode toolbar: a vertical rail of square blocks against the left edge, in the macOS
/// HUD idiom, built around the 13 pt control size the HIG gives as the macOS default. All of it
/// is logical pixels.
///
/// The rail hugs the left edge (the side measured fastest for toolbar targets) with its vertical
/// centre at the optical centre, roughly 5% above the geometric centre, where the gaze rests.
const TOOLBAR_MARGIN: f32 = 8.0;
const TOOLBAR_CENTRE: f32 = 0.45;
/// Side of one block: the rail is a column of these, above the HIG's minimum pointer target.
const TOOLBAR_BLOCK: f32 = 30.0;
/// Between two blocks of the same group.
const TOOLBAR_GAP: f32 = 4.0;
/// Between two groups: the palette, the tools, the size control, `Esc`.
const TOOLBAR_GROUP_GAP: f32 = 10.0;
/// Around the whole column.
const TOOLBAR_PAD: f32 = 6.0;
const TOOLBAR_RADIUS: f32 = 8.0;
const TOOLBAR_HAIRLINE: f32 = 1.0;
/// One size for the glyph and for the label beside it: the HIG's 13 pt macOS control size, so
/// the two share a size and a baseline instead of being matched by eye.
const TOOLBAR_TEXT_SIZE: f32 = 13.0;
/// How far the block and the meaning of a stretched block are kept apart.
const TOOLBAR_LABEL_GAP: f32 = 6.0;
/// Padding after a label, so text never touches the capsule's edge.
const TOOLBAR_LABEL_PAD: f32 = 12.0;
/// Widest stretched block the damage accounting reserves room for, and the ceiling a label has
/// to fit: the width of a block depends on the font, so `damage_box` reserves this strip instead
/// of the drawn rail. The strip only joins a damage box that reaches it.
const TOOLBAR_RESERVE: f32 = 224.0;
/// Height of the whole column: twelve blocks, eight gaps inside the groups and three between
/// them, all fixed, so the column's height never depends on the font, unlike the width a
/// stretched block takes.
const TOOLBAR_COLUMN: f32 =
    2.0 * TOOLBAR_PAD + 12.0 * TOOLBAR_BLOCK + 8.0 * TOOLBAR_GAP + 3.0 * TOOLBAR_GROUP_GAP;
const TOOLBAR_CACHE_CAP: usize = 16;
/// `#rrggbbaa`. The block fill stands in for a HUD material: a `wl_shm` layer surface cannot be
/// blurred, so there is translucency but no real vibrancy. The alpha is the opacity that keeps
/// a secondary label above 4.5:1 over any wallpaper; darken it rather than lighten it.
const TOOLBAR_BLOCK_FILL: &str = "#1c1c1ce6";
const TOOLBAR_BLOCK_EDGE: &str = "#ffffff26";
/// macOS system accent, used for the armed tool's block and the active colour's ring.
const TOOLBAR_ACCENT: &str = "#0a84ffff";
const TOOLBAR_KEY_TEXT: &str = "#ffffffee";
const TOOLBAR_LABEL_TEXT: &str = "#ebebf59e";
const TOOLBAR_SWATCH_EDGE: &str = "#ffffff4d";
const UNDERLINE_HEIGHT: f32 = 1.5;
const CURSOR_WIDTH: f32 = 2.0;
const LINE_HEIGHT_SCALE: f32 = 1.2;

/// One block of the rail: what it shows when the pointer is elsewhere, what it says when the
/// pointer is on it, and what clicking it does.
#[derive(Clone)]
struct Block {
    /// What the square shows while it is not stretched: a key, or the size as a number. A colour
    /// swatch has no glyph; its block is the colour.
    glyph: String,
    /// What the stretched block says: what the control does.
    label: String,
    /// `None` for the size readout between `[` and `]`: drawn, but the neighbouring block owns
    /// the presses on its row.
    action: Option<ToolbarAction>,
    /// The armed tool's block, or the active colour's swatch: drawn with the accent.
    active: bool,
    /// Palette index when the block is a colour swatch.
    swatch: Option<usize>,
    /// Gap under the block, in column coordinates.
    gap: f32,
}

/// The blocks of the rail, top to bottom: the palette, the tools, the size control and `Esc`.
fn toolbar_blocks(toolbar: &Toolbar) -> Vec<Block> {
    let axis = if toolbar.tool == Tool::Text {
        "size"
    } else {
        "width"
    };
    let mut blocks: Vec<Block> = PALETTE
        .iter()
        .enumerate()
        .map(|(index, color)| Block {
            glyph: String::new(),
            label: format!("colour {}", index + 1),
            action: Some(ToolbarAction::Color(index)),
            active: *color == toolbar.color,
            swatch: Some(index),
            gap: TOOLBAR_GAP,
        })
        .collect();
    blocks[PALETTE.len() - 1].gap = TOOLBAR_GROUP_GAP;
    for (tool, glyph, label) in [
        (Tool::Pen, "P", "pen"),
        (Tool::Eraser, "E", "eraser"),
        (Tool::Text, "T", "text: click to place"),
    ] {
        blocks.push(Block {
            glyph: glyph.into(),
            label: label.into(),
            action: Some(ToolbarAction::Tool(tool)),
            active: toolbar.tool == tool,
            swatch: None,
            gap: TOOLBAR_GAP,
        });
    }
    let last = blocks.len() - 1;
    blocks[last].gap = TOOLBAR_GROUP_GAP;
    blocks.push(Block {
        glyph: "[".into(),
        label: format!("smaller {axis}"),
        action: Some(ToolbarAction::SizeStep(-1.0)),
        active: false,
        swatch: None,
        gap: TOOLBAR_GAP,
    });
    blocks.push(Block {
        glyph: format!("{:.0}", toolbar.size),
        // Drawn, but no action owns it, so it never stretches to say anything.
        label: String::new(),
        action: None,
        active: false,
        swatch: None,
        gap: TOOLBAR_GAP,
    });
    blocks.push(Block {
        glyph: "]".into(),
        label: format!("larger {axis}"),
        action: Some(ToolbarAction::SizeStep(1.0)),
        active: false,
        swatch: None,
        gap: TOOLBAR_GROUP_GAP,
    });
    blocks.push(Block {
        glyph: "Esc".into(),
        label: "end text, then lock".into(),
        action: Some(ToolbarAction::Lock),
        active: false,
        swatch: None,
        gap: 0.0,
    });
    blocks
}

pub struct Renderer {
    font_system: FontSystem,
    cache: SwashCache,
    toolbar_cache: Vec<CachedToolbar>,
}

#[derive(Default)]
pub(crate) struct TextLayoutCache {
    scale: f32,
    entries: Vec<Option<TextCacheEntry>>,
    stroke_bounds: Vec<Option<Rect>>,
}

struct TextCacheEntry {
    text: String,
    size: f32,
    layout: PreparedText,
}

struct PreparedText {
    buffer: Buffer,
    paint_bounds: Rect,
    line_w: f32,
    baseline: f32,
    line_top: f32,
    line_height: f32,
}

#[derive(Clone)]
struct CachedToolbar {
    toolbar: Toolbar,
    surface: f32,
    pill: ToolbarPill,
}

impl TextLayoutCache {
    pub(crate) fn invalidate(&mut self) {
        self.scale = 0.0;
        self.entries.clear();
        self.stroke_bounds.clear();
    }

    fn sync(&mut self, stroke_len: usize, text_len: usize, scale: f32) {
        if self.scale != scale {
            self.invalidate();
            self.scale = scale;
        }
        if self.entries.len() != text_len {
            self.entries.resize_with(text_len, || None);
        }
        if self.stroke_bounds.len() != stroke_len {
            self.stroke_bounds.resize_with(stroke_len, || None);
        }
    }

    fn stroke_bounds(&mut self, index: usize, stroke: &Stroke) -> Option<Rect> {
        if let Some(bounds) = self.stroke_bounds.get(index).and_then(Option::as_ref) {
            return Some(*bounds);
        }
        let bounds = stroke.bounds();
        if index < self.stroke_bounds.len() {
            self.stroke_bounds[index] = bounds;
        }
        bounds
    }

    fn matches(&self, index: usize, item: &TextItem) -> bool {
        self.entries
            .get(index)
            .and_then(Option::as_ref)
            .is_some_and(|entry| entry.text == item.text && entry.size == item.size)
    }

    fn bounds(&self, index: usize) -> Option<Rect> {
        self.entries
            .get(index)
            .and_then(Option::as_ref)
            .map(|entry| entry.layout.paint_bounds)
    }

    fn text_bounds(&self, index: usize, item: &TextItem) -> Option<Rect> {
        self.entries
            .get(index)
            .and_then(Option::as_ref)
            .filter(|entry| entry.text == item.text && entry.size == item.size)
            .map(|entry| entry.layout.paint_bounds)
    }

    fn insert(&mut self, index: usize, item: &TextItem, layout: PreparedText) {
        if index >= self.entries.len() {
            self.entries.resize_with(index + 1, || None);
        }
        self.entries[index] = Some(TextCacheEntry {
            text: item.text.clone(),
            size: item.size,
            layout,
        });
    }

    fn get_mut(&mut self, index: usize) -> Option<&mut PreparedText> {
        self.entries
            .get_mut(index)
            .and_then(Option::as_mut)
            .map(|entry| &mut entry.layout)
    }
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
            toolbar_cache: Vec::new(),
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
        let mut cache = TextLayoutCache::default();
        self.render_with_cache(
            buf, width, height, scale, ann, overlay, damage, &mut cache, None,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render_with_cache(
        &mut self,
        buf: &mut [u8],
        width: u32,
        height: u32,
        scale: f32,
        ann: &OutputAnnotations,
        overlay: &Overlay,
        damage: Option<Rect>,
        text_cache: &mut TextLayoutCache,
        skip_text: Option<usize>,
    ) {
        let Some(mut pixmap) = PixmapMut::from_bytes(buf, width, height) else {
            return;
        };
        text_cache.sync(ann.strokes.len(), ann.texts.len(), scale);
        for (index, s) in ann.strokes.iter().enumerate() {
            // Strokes outside the damaged box already hold the right pixels in this buffer.
            if let Some(damage) = damage
                && !text_cache
                    .stroke_bounds(index, s)
                    .is_some_and(|b| b.intersects(damage))
            {
                continue;
            }
            draw_stroke(&mut pixmap, s, scale);
        }
        for (index, item) in ann.texts.iter().enumerate() {
            if skip_text == Some(index) {
                continue;
            }
            let cache_valid = text_cache.matches(index, item);
            let bounds = if cache_valid {
                text_cache.bounds(index)
            } else {
                Some(item.paint_bounds())
            };
            if let Some(damage) = damage
                && !bounds.is_some_and(|bounds| bounds.intersects(damage))
            {
                continue;
            }
            if !cache_valid {
                let layout = self.prepare_text(&item.text, item, scale);
                text_cache.insert(index, item, layout);
            }
            let Some(bounds) = text_cache.bounds(index) else {
                continue;
            };
            if let Some(damage) = damage
                && !bounds.intersects(damage)
            {
                continue;
            }
            if let Some(layout) = text_cache.get_mut(index) {
                self.draw_prepared_text(&mut pixmap, item, scale, layout, None);
            }
        }
        if let Some(s) = &overlay.stroke {
            draw_stroke(&mut pixmap, s, scale);
        }
        if let Some(t) = &overlay.text {
            self.draw_text(&mut pixmap, &t.item, scale, Some(t), None);
        }
        if let Some(toolbar) = &overlay.toolbar {
            // The toolbar is static content: only a tool, colour, size or mode change repaints
            // it, and those force a whole-surface frame. A bounding-box frame that does not
            // reach it must leave it alone: those bytes are outside the damage region and
            // still hold the previous frame.
            let (w, h) = (
                pixmap.width() as f32 / scale,
                pixmap.height() as f32 / scale,
            );
            let box_ = toolbar_bounds(w, h);
            if damage.is_none_or(|d| box_.intersects(d)) {
                self.draw_toolbar(&mut pixmap, toolbar, scale);
            }
        }
        if let Some([x, y]) = overlay.eraser {
            draw_eraser_marker(&mut pixmap, x * scale, y * scale, scale);
        }
    }

    /// The edit-mode toolbar. macOS idiom: a rail of translucent blocks, the armed tool's block
    /// and the active colour's swatch in the system accent. The block under the pointer stretches
    /// into a capsule that says what it does; every block is also a target, so the rectangles the
    /// press path asks for are laid out with the blocks drawn here.
    fn draw_toolbar(&mut self, pixmap: &mut PixmapMut, toolbar: &Toolbar, scale: f32) {
        let Some(pill) = self.cached_toolbar_pill(toolbar, pixmap.width() as f32 / scale) else {
            return;
        };
        let (ox, oy) = toolbar_origin(pixmap.height() as f32 / scale);
        // How far the block under the pointer has opened. The layout is settled, so a frame of the
        // transition only moves the capsule's trailing edge and the text that edge uncovers.
        let grow = toolbar.grow.clamp(0.0, 1.0);
        for control in &pill.controls {
            let width = if control.stretched {
                TOOLBAR_BLOCK + (control.width - TOOLBAR_BLOCK) * grow
            } else {
                control.width
            };
            let block = Rect {
                x: ox * scale,
                y: (oy + control.y) * scale,
                w: width * scale,
                h: TOOLBAR_BLOCK * scale,
            };
            // A stretched block is a capsule; a square block is a rounded square.
            let radius = if control.stretched {
                TOOLBAR_BLOCK / 2.0
            } else {
                TOOLBAR_RADIUS
            } * scale;
            if let Some(index) = control.block.swatch {
                // The colour chip keeps its place; the capsule unfurls around it.
                if control.stretched {
                    round_rect(
                        pixmap,
                        block,
                        radius,
                        parse_hex(TOOLBAR_BLOCK_FILL),
                        Some((parse_hex(TOOLBAR_BLOCK_EDGE), TOOLBAR_HAIRLINE * scale)),
                    );
                }
                round_rect(
                    pixmap,
                    Rect {
                        y: block.y,
                        x: block.x,
                        w: TOOLBAR_BLOCK * scale,
                        h: TOOLBAR_BLOCK * scale,
                    },
                    TOOLBAR_RADIUS * scale,
                    parse_hex(PALETTE[index]),
                    Some((
                        parse_hex(if control.block.active {
                            TOOLBAR_ACCENT
                        } else {
                            TOOLBAR_SWATCH_EDGE
                        }),
                        if control.block.active {
                            2.0
                        } else {
                            TOOLBAR_HAIRLINE
                        } * scale,
                    )),
                );
            } else {
                let (fill, edge) = if control.block.active {
                    (TOOLBAR_ACCENT, TOOLBAR_ACCENT)
                } else {
                    (TOOLBAR_BLOCK_FILL, TOOLBAR_BLOCK_EDGE)
                };
                round_rect(
                    pixmap,
                    block,
                    radius,
                    parse_hex(fill),
                    Some((parse_hex(edge), TOOLBAR_HAIRLINE * scale)),
                );
                // The glyph stays where it is and the capsule grows to its right, so the block
                // reads as the same square, stretched.
                let glyph = self.text_width(&control.block.glyph, TOOLBAR_TEXT_SIZE);
                let item = toolbar_text(
                    ox + (TOOLBAR_BLOCK - glyph) / 2.0,
                    oy + control.y,
                    TOOLBAR_BLOCK,
                    TOOLBAR_TEXT_SIZE,
                    TOOLBAR_KEY_TEXT,
                    &control.block.glyph,
                );
                self.draw_text(pixmap, &item, scale, None, None);
            }
            if control.stretched && width > TOOLBAR_BLOCK + TOOLBAR_LABEL_GAP {
                let item = toolbar_text(
                    ox + TOOLBAR_BLOCK + TOOLBAR_LABEL_GAP,
                    oy + control.y,
                    TOOLBAR_BLOCK,
                    TOOLBAR_TEXT_SIZE,
                    TOOLBAR_LABEL_TEXT,
                    &control.block.label,
                );
                // The label is uncovered by the capsule growing over it: a glyph appears once
                // the edge has passed it, and at rest the edge is past its trailing padding.
                self.draw_text(pixmap, &item, scale, None, Some(ox + width));
            }
        }
    }

    /// The toolbar block under a pointer position, in logical pixels, or `None` outside the rail.
    /// The layout is the one the frame drew, so a block's rectangle is the rectangle it paints --
    /// including the capsule a stretched block has become.
    pub fn toolbar_hit(
        &mut self,
        toolbar: &Toolbar,
        width: f32,
        height: f32,
        x: f32,
        y: f32,
    ) -> Option<ToolbarAction> {
        let (ox, oy) = toolbar_origin(height);
        let (column_x, column_y) = (x - ox, y - oy);
        if column_x < 0.0 || column_y < 0.0 || column_y >= TOOLBAR_COLUMN {
            return None;
        }
        let pill = self.cached_toolbar_pill(toolbar, width)?;
        pill.hits
            .iter()
            .find(|(hit, _)| {
                column_x >= hit.x
                    && column_x < hit.x + hit.w
                    && column_y >= hit.y
                    && column_y < hit.y + hit.h
            })
            .map(|(_, action)| *action)
    }

    fn cached_toolbar_pill(&mut self, toolbar: &Toolbar, surface: f32) -> Option<ToolbarPill> {
        if let Some(cached) = self
            .toolbar_cache
            .iter()
            .find(|cached| same_layout(&cached.toolbar, toolbar) && cached.surface == surface)
        {
            return Some(cached.pill.clone());
        }
        let pill = self.toolbar_pill(toolbar, surface);
        if self.toolbar_cache.len() == TOOLBAR_CACHE_CAP {
            self.toolbar_cache.remove(0);
        }
        self.toolbar_cache.push(CachedToolbar {
            toolbar: *toolbar,
            surface,
            pill: pill.clone(),
        });
        Some(pill)
    }

    /// Lay the rail out top to bottom. Only the block under the pointer is measured: every other
    /// block is a square, and a block only stretches when its meaning fits the room the damage
    /// accounting reserves for the rail.
    fn toolbar_pill(&mut self, toolbar: &Toolbar, surface: f32) -> ToolbarPill {
        let ceiling = (surface - 2.0 * TOOLBAR_MARGIN).min(TOOLBAR_RESERVE);
        let mut controls = Vec::new();
        let mut hits = Vec::new();
        let mut y = TOOLBAR_PAD;
        for block in toolbar_blocks(toolbar) {
            let gap = block.gap;
            let hovered = block.action.is_some() && block.action == toolbar.hover;
            // The capsule is [block][gap][label][padding], so the text never touches its edge.
            let label = hovered.then(|| self.text_width(&block.label, TOOLBAR_TEXT_SIZE));
            let width = match label {
                Some(label)
                    if TOOLBAR_BLOCK + TOOLBAR_LABEL_GAP + label + TOOLBAR_LABEL_PAD <= ceiling =>
                {
                    TOOLBAR_BLOCK + TOOLBAR_LABEL_GAP + label + TOOLBAR_LABEL_PAD
                }
                _ => TOOLBAR_BLOCK,
            };
            if let Some(action) = block.action {
                hits.push((
                    Rect {
                        x: 0.0,
                        y,
                        w: width,
                        h: TOOLBAR_BLOCK,
                    },
                    action,
                ));
            }
            controls.push(Control {
                block,
                y,
                width,
                stretched: width > TOOLBAR_BLOCK,
            });
            y += TOOLBAR_BLOCK + gap;
        }
        debug_assert_eq!(y + TOOLBAR_PAD, TOOLBAR_COLUMN);
        // The rail consumes every press on a block, and the rows between blocks belong to a
        // neighbour as well: a press on the size readout or in a gap has to step something
        // rather than start a stroke under the rail.
        if let Some((first, _)) = hits.first_mut() {
            first.h += first.y;
            first.y = 0.0;
        }
        for index in 0..hits.len().saturating_sub(1) {
            hits[index].0.h = hits[index + 1].0.y - hits[index].0.y;
        }
        if let Some((last, _)) = hits.last_mut() {
            last.h = TOOLBAR_COLUMN - last.y;
        }
        ToolbarPill { controls, hits }
    }

    /// Width of one line of text, in logical pixels. The shaped buffer is thrown away: the
    /// toolbar is a handful of short strings and it is only drawn while editing.
    fn text_width(&mut self, text: &str, size: f32) -> f32 {
        let mut buffer = Buffer::new_empty(Metrics::new(size, size * LINE_HEIGHT_SCALE));
        buffer.set_text(text, &Attrs::new(), Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.font_system, false);
        buffer.layout_runs().map(|r| r.line_w).fold(0.0, f32::max)
    }

    fn prepare_text(&mut self, text: &str, item: &TextItem, scale: f32) -> PreparedText {
        let size = item.size * scale;
        let mut buffer = Buffer::new_empty(Metrics::new(size, size * LINE_HEIGHT_SCALE));
        buffer.set_size(None, None);
        buffer.set_text(text, &Attrs::new(), Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.font_system, false);

        let mut line_w = 0.0;
        let mut baseline = size * 0.8;
        let mut line_top = 0.0;
        let mut line_height = size * LINE_HEIGHT_SCALE;
        let mut max_right: f32 = 0.0;
        let mut max_bottom: f32 = line_height;
        for run in buffer.layout_runs() {
            line_w = run.line_w;
            baseline = run.line_y;
            line_top = run.line_top;
            line_height = run.line_height;
            max_right = max_right.max(run.line_w);
            max_bottom = max_bottom.max(run.line_top + run.line_height);
        }
        let paint_bounds = Rect {
            x: item.x,
            y: item.y,
            w: max_right / scale,
            h: max_bottom / scale,
        }
        .grown(1.0);
        PreparedText {
            buffer,
            paint_bounds,
            line_w,
            baseline,
            line_top,
            line_height,
        }
    }

    pub(crate) fn text_edit_bounds(&mut self, text: &str, item: &TextItem, scale: f32) -> Rect {
        self.prepare_text(text, item, scale)
            .paint_bounds
            .grown(CURSOR_WIDTH)
    }

    /// `reveal` is the logical x a progress animation has uncovered: glyphs to its right are
    /// left out, so the text appears as the shape around it grows.
    fn draw_prepared_text(
        &mut self,
        pixmap: &mut PixmapMut,
        item: &TextItem,
        scale: f32,
        prepared: &mut PreparedText,
        reveal: Option<f32>,
    ) {
        let (ox, oy) = (item.x * scale, item.y * scale);
        let c = parse_hex(&item.color);
        let color = Color::rgba(c[0], c[1], c[2], c[3]);
        let (clip_w, clip_h) = (pixmap.width(), pixmap.height());
        let reveal = reveal.map(|x| x * scale);
        prepared.buffer.draw(
            &mut self.font_system,
            &mut self.cache,
            color,
            |x, y, w, h, c| {
                if reveal.is_some_and(|reveal| ox + x as f32 > reveal) {
                    return;
                }
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
    }

    fn draw_text(
        &mut self,
        pixmap: &mut PixmapMut,
        item: &TextItem,
        scale: f32,
        edit: Option<&TextOverlay>,
        reveal: Option<f32>,
    ) {
        let (committed, preedit) = match edit {
            Some(e) => (e.buffer.text.as_str(), e.preedit.as_str()),
            None => (item.text.as_str(), ""),
        };
        // Text uses explicit newlines only. The output clips long lines instead of wrapping
        // them behind the damage calculator's back.
        let display = if preedit.is_empty() {
            Cow::Borrowed(committed)
        } else {
            Cow::Owned(format!("{committed}{preedit}"))
        };
        if display.is_empty() && edit.is_none() {
            return;
        }
        let mut prepared = self.prepare_text(display.as_ref(), item, scale);
        let mut preedit_x0 = None;
        if !preedit.is_empty() {
            for run in prepared.buffer.layout_runs() {
                for glyph in run.glyphs {
                    if glyph.start >= committed.len() && preedit_x0.is_none() {
                        preedit_x0 = Some(glyph.x);
                    }
                }
            }
        }
        self.draw_prepared_text(pixmap, item, scale, &mut prepared, reveal);

        if let Some(e) = edit {
            let size = item.size * scale;
            let color = {
                let c = parse_hex(&item.color);
                Color::rgba(c[0], c[1], c[2], c[3])
            };
            let (ox, oy) = (item.x * scale, item.y * scale);
            if !e.preedit.is_empty() {
                let x0 = ox + preedit_x0.unwrap_or(prepared.line_w);
                fill(
                    pixmap,
                    x0,
                    oy + prepared.baseline + size * 0.15,
                    (ox + prepared.line_w - x0).max(0.0),
                    UNDERLINE_HEIGHT * scale,
                    color,
                );
            }
            // The caret is the line box the text sits in, so it matches the typed text instead
            // of floating at a hardcoded fraction of the font size (the empty-line baseline and
            // a real glyph's baseline are not the same fraction).
            fill(
                pixmap,
                ox + prepared.line_w,
                oy + prepared.line_top,
                CURSOR_WIDTH * scale,
                prepared.line_height,
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
pub fn damage_box(
    ann: &OutputAnnotations,
    overlay: &Overlay,
    from: Rect,
    width: f32,
    height: f32,
) -> Rect {
    let mut cache = TextLayoutCache::default();
    damage_box_with_cache(ann, overlay, from, width, height, 1.0, &mut cache)
}

pub(crate) fn damage_box_with_cache(
    ann: &OutputAnnotations,
    overlay: &Overlay,
    from: Rect,
    width: f32,
    height: f32,
    scale: f32,
    cache: &mut TextLayoutCache,
) -> Rect {
    cache.sync(ann.strokes.len(), ann.texts.len(), scale);
    let mut damage = from;
    loop {
        let mut grown = damage;
        for (index, stroke) in ann.strokes.iter().enumerate() {
            if let Some(bounds) = cache.stroke_bounds(index, stroke)
                && bounds.intersects(grown)
            {
                grown = grown.union(bounds);
            }
        }
        for (index, text) in ann.texts.iter().enumerate() {
            let bounds = cache
                .text_bounds(index, text)
                .unwrap_or_else(|| text.paint_bounds());
            if bounds.intersects(grown) {
                grown = grown.union(bounds);
            }
        }
        if let Some(t) = &overlay.text {
            let b = t.item.paint_bounds();
            if b.intersects(grown) {
                grown = grown.union(b);
            }
        }
        // The toolbar is only repainted when the box grows into it, so the growth has to cover
        // everything the toolbar paints. Reserving its box unconditionally would union a left
        // strip with a drawing box far from it and swallow nearly the whole surface; the toolbar
        // itself only changes on a tool, colour or mode switch, and those force a whole-surface
        // frame.
        if overlay.toolbar.is_some() {
            let toolbar = toolbar_bounds(width, height);
            if toolbar.intersects(grown) {
                grown = grown.union(toolbar);
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

/// Whether two toolbar states lay out the same rail. The transition progress is not part of the
/// layout: a block's final width is settled before the pop-out opens, so its frames reuse the
/// measured layout instead of measuring the label again.
fn same_layout(a: &Toolbar, b: &Toolbar) -> bool {
    a.tool == b.tool && a.color == b.color && a.size == b.size && a.hover == b.hover
}

/// One block of the rail as laid out: where it sits in the column, how wide it is drawn, and
/// whether the pointer stretched it into a capsule.
#[derive(Clone)]
struct Control {
    block: Block,
    /// Top of the block, in column coordinates.
    y: f32,
    width: f32,
    stretched: bool,
}

/// A laid-out rail: what to paint and what to hit, in logical pixels relative to the rail's own
/// origin at its top-left corner.
#[derive(Clone)]
struct ToolbarPill {
    controls: Vec<Control>,
    hits: Vec<(Rect, ToolbarAction)>,
}

/// A label vertically centred in a `height`-tall box whose top edge is `top`.
fn toolbar_text(x: f32, top: f32, height: f32, size: f32, color: &str, text: &str) -> TextItem {
    TextItem {
        x,
        y: top + (height - size * LINE_HEIGHT_SCALE) / 2.0,
        color: color.into(),
        size,
        text: text.into(),
    }
}

/// Where the rail sits on an output of this logical height: `TOOLBAR_MARGIN` from the left edge,
/// its vertical centre at the optical centre. Short outputs keep the top margin rather than
/// pushing the rail off the screen.
fn toolbar_origin(height: f32) -> (f32, f32) {
    (
        TOOLBAR_MARGIN,
        (height * TOOLBAR_CENTRE - TOOLBAR_COLUMN / 2.0).max(TOOLBAR_MARGIN),
    )
}

/// Everything the rail paints. A stretched block's width depends on the font, so the damage
/// accounting reserves the widest block `toolbar_pill` will build, as a strip on the left of the
/// output; the strip only joins a damage box that reaches it.
pub fn toolbar_bounds(width: f32, height: f32) -> Rect {
    let (x, y) = toolbar_origin(height);
    Rect {
        // The hairline edge is stroked around the path, so it reaches one logical pixel past
        // the block itself.
        x: x - TOOLBAR_HAIRLINE,
        y: y - TOOLBAR_HAIRLINE,
        w: (width - 2.0 * TOOLBAR_MARGIN).clamp(0.0, TOOLBAR_RESERVE) + 2.0 * TOOLBAR_HAIRLINE,
        h: TOOLBAR_COLUMN + 2.0 * TOOLBAR_HAIRLINE,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::TextBuffer;

    fn text_item(text: &str) -> TextItem {
        TextItem {
            x: 10.0,
            y: 10.0,
            color: "#ffffff".into(),
            size: 24.0,
            text: text.into(),
        }
    }

    fn render_cached(
        renderer: &mut Renderer,
        cache: &mut TextLayoutCache,
        annotations: &OutputAnnotations,
        buffer: &mut [u8],
    ) {
        renderer.render_with_cache(
            buffer,
            200,
            100,
            1.0,
            annotations,
            &Overlay::default(),
            None,
            cache,
            None,
        );
    }

    #[test]
    fn warm_text_cache_matches_cold_text_cache() {
        let mut renderer = Renderer::new();
        let mut cache = TextLayoutCache::default();
        let annotations = OutputAnnotations {
            strokes: vec![],
            texts: vec![text_item("cached text")],
        };
        let mut cold = vec![0; 200 * 100 * 4];
        let mut warm = vec![0; 200 * 100 * 4];
        render_cached(&mut renderer, &mut cache, &annotations, &mut cold);
        render_cached(&mut renderer, &mut cache, &annotations, &mut warm);
        assert_eq!(cold, warm);
    }

    #[test]
    fn text_cache_notices_changed_text_without_external_invalidation() {
        let mut renderer = Renderer::new();
        let mut cache = TextLayoutCache::default();
        let mut annotations = OutputAnnotations {
            strokes: vec![],
            texts: vec![text_item("before")],
        };
        let mut first = vec![0; 200 * 100 * 4];
        render_cached(&mut renderer, &mut cache, &annotations, &mut first);
        annotations.texts[0].text = "after".into();
        let mut warm = vec![0; 200 * 100 * 4];
        render_cached(&mut renderer, &mut cache, &annotations, &mut warm);
        let mut cold = vec![0; 200 * 100 * 4];
        let mut fresh_cache = TextLayoutCache::default();
        render_cached(&mut renderer, &mut fresh_cache, &annotations, &mut cold);
        assert_eq!(cold, warm);
    }

    #[test]
    fn warm_toolbar_cache_matches_cold_toolbar_render() {
        let mut renderer = Renderer::new();
        let annotations = OutputAnnotations::default();
        let overlay = Overlay {
            toolbar: Some(Toolbar {
                tool: Tool::Pen,
                color: "#e01b24",
                size: 3.0,
                hover: None,
                grow: 1.0,
            }),
            ..Default::default()
        };
        let mut cold = vec![0; 640 * 400 * 4];
        renderer.render(&mut cold, 640, 400, 1.0, &annotations, &overlay, None);
        let mut warm = vec![0; 640 * 400 * 4];
        renderer.render(&mut warm, 640, 400, 1.0, &annotations, &overlay, None);
        assert_eq!(cold, warm);
    }

    #[test]
    fn localized_new_text_uses_the_same_safe_damage_path() {
        let mut renderer = Renderer::new();
        let annotations = OutputAnnotations::default();
        let item = text_item("new text");
        let overlay = Overlay {
            text: Some(TextOverlay {
                item: item.clone(),
                buffer: TextBuffer::new("new text"),
                index: None,
                preedit: String::new(),
            }),
            ..Default::default()
        };
        let bounds = renderer.text_edit_bounds("new text", &item, 1.0);
        let mut expected = vec![0; 200 * 100 * 4];
        renderer.render(&mut expected, 200, 100, 1.0, &annotations, &overlay, None);
        let mut actual = vec![0; 200 * 100 * 4];
        let mut cache = TextLayoutCache::default();
        renderer.render_with_cache(
            &mut actual,
            200,
            100,
            1.0,
            &annotations,
            &overlay,
            Some(bounds),
            &mut cache,
            None,
        );
        assert_eq!(actual, expected);
    }

    #[test]
    fn cached_damage_box_matches_uncached_damage_box() {
        let annotations = OutputAnnotations {
            strokes: vec![Stroke {
                color: "#ffffff".into(),
                width: 3.0,
                points: vec![[20.0, 50.0], [180.0, 50.0]],
            }],
            texts: vec![text_item("cached text")],
        };
        let overlay = Overlay::default();
        let from = Rect {
            x: 100.0,
            y: 48.0,
            w: 4.0,
            h: 4.0,
        };
        let expected = damage_box(&annotations, &overlay, from, 200.0, 100.0);
        let mut cache = TextLayoutCache::default();
        let actual =
            damage_box_with_cache(&annotations, &overlay, from, 200.0, 100.0, 1.0, &mut cache);
        assert_eq!(actual, expected);
    }

    #[test]
    fn localized_text_edit_skips_the_stale_document_text() {
        let mut renderer = Renderer::new();
        let old_document = OutputAnnotations {
            strokes: vec![],
            texts: vec![text_item("before")],
        };
        let mut new_document = old_document.clone();
        new_document.texts[0].text = "after".into();
        let edit = TextOverlay {
            item: old_document.texts[0].clone(),
            buffer: TextBuffer::new("after"),
            index: Some(0),
            preedit: String::new(),
        };
        let overlay = Overlay {
            text: Some(edit),
            ..Default::default()
        };
        let old_bounds = renderer.text_edit_bounds("before", &old_document.texts[0], 1.0);
        let new_bounds = renderer.text_edit_bounds("after", &new_document.texts[0], 1.0);
        let damage = old_bounds.union(new_bounds);

        let mut old_frame = vec![0; 200 * 100 * 4];
        renderer.render(
            &mut old_frame,
            200,
            100,
            1.0,
            &old_document,
            &Overlay::default(),
            None,
        );
        let expected_overlay = Overlay {
            text: Some(TextOverlay {
                item: new_document.texts[0].clone(),
                buffer: TextBuffer::new("after"),
                index: None,
                preedit: String::new(),
            }),
            ..Default::default()
        };
        let mut expected = vec![0; 200 * 100 * 4];
        renderer.render(
            &mut expected,
            200,
            100,
            1.0,
            &OutputAnnotations::default(),
            &expected_overlay,
            None,
        );

        let mut actual = old_frame;
        DamageRegion::from_logical(damage, 1.0, 200, 100).clear(&mut actual, 200, 100);
        let mut cache = TextLayoutCache::default();
        renderer.render_with_cache(
            &mut actual,
            200,
            100,
            1.0,
            &old_document,
            &overlay,
            Some(damage),
            &mut cache,
            Some(0),
        );
        assert_eq!(actual, expected);
    }
}
