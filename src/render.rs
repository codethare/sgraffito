//! Frame composition: tiny-skia draws strokes and the eraser marker, cosmic-text draws text.
//!
//! `buf` is a tiny-skia premultiplied RGBA buffer already sized as logical size x scale.

use std::borrow::Cow;

use cosmic_text::{Attrs, Buffer, Color, FontSystem, Metrics, Shaping, SwashCache, Wrap};
use tiny_skia::{
    Color as SkColor, LineCap, LineJoin, Paint, PathBuilder, Pixmap, PixmapMut,
    PremultipliedColorU8, Stroke as SkStroke, Transform,
};
use wayland_client::protocol::wl_shm::Format;

use crate::canvas::{
    Handle, ItemId, NOTE_DATE_SIZE, NOTE_RADIUS, Note, OutputAnnotations, Overlay, PALETTE, Rect,
    SELECT_HANDLE, SELECT_ROTATE_GAP, Shape, ShapeKind, Stroke, TextItem, TextOverlay, Tool,
    Toolbar, ToolbarAction, View, parse_hex, rect_of, rotate_handle, selection_frame,
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
/// One size for every piece of text a block carries — its label, and the size readout's number:
/// the HIG's 13 pt macOS control size, so they share a size and a baseline instead of being matched
/// by eye. An icon is geometry rather than text and is not sized by this.
const TOOLBAR_TEXT_SIZE: f32 = 13.0;
/// How far the block and the meaning of a stretched block are kept apart.
const TOOLBAR_LABEL_GAP: f32 = 6.0;
/// Padding after a label, so text never touches the capsule's edge.
const TOOLBAR_LABEL_PAD: f32 = 12.0;
/// Stroke width of an icon and the inset between it and the edge of its block, in logical pixels.
/// An icon is a picture, so it is drawn heavier than the interface's hairline: one device pixel of
/// a 30 pt square does not read as a picture at scale 1.
const ICON_STROKE: f32 = 1.5;
const ICON_INSET: f32 = 7.0;
/// Widest stretched block the damage accounting reserves room for, and the ceiling a label has
/// to fit: the width of a block depends on the font, so the damage reserves this strip instead
/// of the drawn rail. The strip is damaged whenever the rail changes, and joins a transient
/// damage box that reaches it.
const TOOLBAR_RESERVE: f32 = 224.0;
/// Height of the whole column: twenty-one blocks, seventeen gaps inside the groups and three between
/// them, all fixed, so the column's height never depends on the font, unlike the width a stretched block
/// takes. The drawer is a second column and adds nothing to this.
const TOOLBAR_COLUMN: f32 =
    2.0 * TOOLBAR_PAD + 21.0 * TOOLBAR_BLOCK + 17.0 * TOOLBAR_GAP + 3.0 * TOOLBAR_GROUP_GAP;
/// How far the drawer's column starts from the rail's.
const TOOLBAR_DRAWER_X: f32 = TOOLBAR_BLOCK + TOOLBAR_GAP;
const TOOLBAR_CACHE_CAP: usize = 16;
/// `#rrggbbaa`. The block fill stands in for a HUD material: a `wl_shm` layer surface cannot be
/// blurred, so there is translucency but no real vibrancy. The alpha is the opacity that keeps
/// a secondary label above 4.5:1 over any wallpaper; darken it rather than lighten it.
const TOOLBAR_BLOCK_FILL: &str = "#1c1c1ce6";
const TOOLBAR_BLOCK_EDGE: &str = "#ffffff26";
/// macOS system accent, used for the armed tool's block and the active colour's ring.
const TOOLBAR_ACCENT: &str = "#0a84ffff";
/// `#rrggbbaa`. The scrim an edit-mode frame is painted on: black at 90% opacity over the whole
/// output, under the annotations and the rail, so what is behind the surface — the wallpaper and the
/// user's other windows — is dimmed and the content on the surface is what can be read. Locked mode
/// paints none of it: that surface sits below ordinary windows, so a scrim there would dim the
/// wallpaper the annotations are read against and hide nothing.
const SCRIM_FILL: &str = "#000000e6";
/// The scrim's alpha, for the tests that have to tell content from the scrim a frame is painted on.
pub const SCRIM_ALPHA: u8 = 0xCC;
/// `#rrggbbaa`. The plate the rail sits on: black at 90% opacity, behind every block so the gaps
/// between them do not show the wallpaper at full strength and the rail reads as one control over
/// bright content drawn under it. Over the edit-mode scrim it reads as 99% black against the
/// scrim's 90%.
const TOOLBAR_PLATE_FILL: &str = "#000000e6";
/// The colour of whatever a block draws on itself: an icon, or the size readout's number.
const TOOLBAR_FOREGROUND: &str = "#ffffffee";
const TOOLBAR_LABEL_TEXT: &str = "#ebebf59e";
const TOOLBAR_SWATCH_EDGE: &str = "#ffffff4d";
const UNDERLINE_HEIGHT: f32 = 1.5;
const CURSOR_WIDTH: f32 = 2.0;
const LINE_HEIGHT_SCALE: f32 = 1.2;

/// What a block shows on its face. Exactly one of these is set: an icon of what the control does, a
/// colour, or a value. A colour chip and the size readout are not functions, so they carry no icon.
#[derive(Clone)]
enum Mark {
    Icon(Icon),
    /// Palette index of a colour chip.
    Swatch(usize),
    /// A value as a bare number.
    Value(String),
}

/// The rail's icon set. Every one is drawn as geometry, never as a glyph: a symbol font is a
/// coverage gamble that fails as a tofu box on a machine whose fonts differ from this one.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Icon {
    Pen,
    Eraser,
    Text,
    Selection,
    Circle,
    Rect,
    Diamond,
    Line,
    Arrow,
    Smaller,
    Larger,
    Lock,
    Hand,
    Note,
    More,
    Scrim,
}

/// What a canvas coordinate has to be multiplied by to become a device pixel: the output's scale, and
/// the view. One value for both, because every draw site needs both and passing them separately is how
/// they drift apart.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Screen {
    pub scale: f32,
    pub view: View,
}

impl Screen {
    /// A canvas point in device pixels.
    fn point(self, p: [f32; 2]) -> [f32; 2] {
        let s = self.view.to_screen(p);
        [s[0] * self.scale, s[1] * self.scale]
    }

    /// A canvas length in device pixels: a pen width, a text size, an element's box.
    fn len(self, l: f32) -> f32 {
        l * self.view.zoom * self.scale
    }

    /// A **screen** rectangle in device pixels: the zoom is already in it, which is what the
    /// selection frame carries because its handles are interface-sized.
    fn ui_rect(self, r: Rect) -> Rect {
        Rect {
            x: r.x * self.scale,
            y: r.y * self.scale,
            w: r.w * self.scale,
            h: r.h * self.scale,
        }
    }

    /// A canvas rectangle in device pixels.
    fn rect(self, r: Rect) -> Rect {
        let origin = self.point([r.x, r.y]);
        Rect {
            x: origin[0],
            y: origin[1],
            w: self.len(r.w),
            h: self.len(r.h),
        }
    }

    /// A length that belongs to the interface rather than to the canvas — a handle, a hairline — in
    /// device pixels. It ignores the zoom, so a control is the same size at every zoom.
    fn ui(self, l: f32) -> f32 {
        l * self.scale
    }
}

/// One block of the rail: what it shows when the pointer is elsewhere, what it says when the
/// pointer is on it, and what clicking it does.
#[derive(Clone)]
struct Block {
    mark: Mark,
    /// 0 for the rail, 1 for the drawer beside it. The drawer is a second column, so a control in it
    /// needs no layout machinery of its own.
    column: u8,
    /// What the stretched block says: what the control does, and the key that does the same thing.
    /// Built by [`labelled`], so an action cannot be added without its text naming its key.
    label: String,
    /// `None` for the size readout between `[` and `]`: drawn, but the neighbouring block owns
    /// the presses on its row.
    action: Option<ToolbarAction>,
    /// The armed tool's block, or the active colour's swatch: drawn with the accent.
    active: bool,
    /// Gap under the block, in column coordinates.
    gap: f32,
}

/// The key a control applies, as it is written in a stretched block's text.
fn key_of(action: ToolbarAction) -> &'static str {
    match action {
        ToolbarAction::Tool(tool) => tool.key(),
        ToolbarAction::More => "M",
        ToolbarAction::Scrim => "B",
        ToolbarAction::Color(index) => ["1", "2", "3", "4", "5"].get(index).copied().unwrap_or("?"),
        ToolbarAction::SizeStep(dir) => {
            if dir < 0.0 {
                "["
            } else {
                "]"
            }
        }
        ToolbarAction::Lock => "Esc",
    }
}

/// What a block says when the pointer is on it, ending with the key that applies the same thing.
fn labelled(action: ToolbarAction, what: &str) -> String {
    format!("{what} ({})", key_of(action))
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
            mark: Mark::Swatch(index),
            column: 0,
            label: labelled(
                ToolbarAction::Color(index),
                &format!("colour {}", index + 1),
            ),
            action: Some(ToolbarAction::Color(index)),
            active: *color == toolbar.color,
            gap: TOOLBAR_GAP,
        })
        .collect();
    blocks[PALETTE.len() - 1].gap = TOOLBAR_GROUP_GAP;
    for (tool, icon, label) in [
        (Tool::Pen, Icon::Pen, "pen"),
        (Tool::Eraser, Icon::Eraser, "eraser"),
        (Tool::Text, Icon::Text, "text: click to place"),
        (Tool::Selection, Icon::Selection, "select: drag to move"),
        (Tool::Circle, Icon::Circle, "circle"),
        (Tool::Rect, Icon::Rect, "rectangle"),
        (Tool::Diamond, Icon::Diamond, "diamond"),
        (Tool::Line, Icon::Line, "line"),
        (Tool::Arrow, Icon::Arrow, "arrow"),
        (Tool::Hand, Icon::Hand, "hand: drag to pan"),
        (Tool::Note, Icon::Note, "note"),
    ] {
        blocks.push(Block {
            mark: Mark::Icon(icon),
            column: 0,
            label: labelled(ToolbarAction::Tool(tool), label),
            action: Some(ToolbarAction::Tool(tool)),
            active: toolbar.tool == tool,
            gap: TOOLBAR_GAP,
        });
    }
    let last = blocks.len() - 1;
    blocks[last].gap = TOOLBAR_GROUP_GAP;
    blocks.push(Block {
        mark: Mark::Icon(Icon::Smaller),
        column: 0,
        label: labelled(ToolbarAction::SizeStep(-1.0), &format!("smaller {axis}")),
        action: Some(ToolbarAction::SizeStep(-1.0)),
        active: false,
        gap: TOOLBAR_GAP,
    });
    blocks.push(Block {
        mark: Mark::Value(format!("{:.0}", toolbar.size)),
        column: 0,
        // Drawn, but no action owns it, so it never stretches to say anything.
        label: String::new(),
        action: None,
        active: false,
        gap: TOOLBAR_GAP,
    });
    blocks.push(Block {
        mark: Mark::Icon(Icon::Larger),
        column: 0,
        label: labelled(ToolbarAction::SizeStep(1.0), &format!("larger {axis}")),
        action: Some(ToolbarAction::SizeStep(1.0)),
        active: false,
        gap: TOOLBAR_GROUP_GAP,
    });
    // The block that opens the drawer sits at the foot of the rail and before `Esc`, so the rail's
    // order and its "Esc is last" property are unchanged.
    blocks.push(Block {
        mark: Mark::Icon(Icon::More),
        column: 0,
        label: labelled(ToolbarAction::More, "more tools"),
        action: Some(ToolbarAction::More),
        active: toolbar.more,
        gap: TOOLBAR_GAP,
    });
    blocks.push(Block {
        mark: Mark::Icon(Icon::Lock),
        column: 0,
        label: labelled(ToolbarAction::Lock, "cancel, then lock"),
        action: Some(ToolbarAction::Lock),
        active: false,
        gap: 0.0,
    });
    // The drawer: a second column for the controls that are not used often. Its own height is whatever
    // its residents need, so the rail's height is the same whether it is open or closed.
    if toolbar.more {
        blocks.push(Block {
            mark: Mark::Icon(Icon::Scrim),
            column: 1,
            label: labelled(
                ToolbarAction::Scrim,
                if toolbar.scrim {
                    "backdrop: on"
                } else {
                    "backdrop: off"
                },
            ),
            action: Some(ToolbarAction::Scrim),
            active: toolbar.scrim,
            gap: 0.0,
        });
    }
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
}

/// Everything one frame reuses: the shaped lines of the committed text, and the committed
/// document itself, rasterised once. The daemon holds one per output; a caller that only renders
/// a single frame can let [`Renderer::render`] build a throwaway one.
#[derive(Default)]
pub struct FrameCache {
    text: TextLayoutCache,
    base: BaseLayer,
}

/// The committed document of one output, rasterised into a pixmap that every frame fills from.
/// A frame therefore paints only the overlays, and its cost follows what changed rather than how
/// much is drawn. The raster is rebuilt when the output's pixel size, its scale, or the text box
/// under edit changes, and whenever the document changes. See [`TextLayoutCache`].
#[derive(Default)]
pub(crate) struct BaseLayer {
    pixmap: Option<Pixmap>,
    key: Option<BaseKey>,
}

#[derive(Clone, PartialEq)]
struct BaseKey {
    width: u32,
    height: u32,
    scale: f32,
    /// The elements the raster has to leave out: the text box under edit, and the elements a live
    /// selection gesture is dragging. The overlay draws each of them instead, so a frame paints an
    /// element exactly once, in the position it is live at.
    skip: Vec<ItemId>,
    /// The view is part of the key because the raster is screen-space: a pan or a zoom moves every
    /// pixel of it, so the frame that follows has to rebuild it.
    view: View,
    /// Whether the raster's background is the edit-mode scrim. A mode switch recreates every surface
    /// and so every cache, but the key says it anyway: a raster that had to be trusted to be rebuilt
    /// would be one change away from being stale.
    scrim: bool,
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
    }

    fn sync(&mut self, text_len: usize, scale: f32) {
        if self.scale != scale {
            self.invalidate();
            self.scale = scale;
        }
        if self.entries.len() != text_len {
            self.entries.resize_with(text_len, || None);
        }
    }

    fn matches(&self, index: usize, text: &str, size: f32) -> bool {
        self.entries
            .get(index)
            .and_then(Option::as_ref)
            .is_some_and(|entry| entry.text == text && entry.size == size)
    }

    /// `size` is the size the view puts on screen, not the item's own: a shaped line is only reusable
    /// at the same zoom as well as the same text.
    fn insert(&mut self, index: usize, text: &str, size: f32, layout: PreparedText) {
        if index >= self.entries.len() {
            self.entries.resize_with(index + 1, || None);
        }
        self.entries[index] = Some(TextCacheEntry {
            text: text.to_string(),
            size,
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

impl FrameCache {
    /// Drop both caches; the next frame rebuilds them from the document. The daemon calls this
    /// whenever the document changes, an output is resized or rescaled, or the output's bucket
    /// changes.
    pub fn invalidate(&mut self) {
        self.text.invalidate();
        self.base.invalidate();
    }
}

impl BaseLayer {
    /// Drop the raster; the next frame rebuilds it from the document.
    pub(crate) fn invalidate(&mut self) {
        self.key = None;
    }

    fn matches(&self, key: &BaseKey) -> bool {
        self.key.as_ref() == Some(key)
    }

    /// Rasterise the document, reusing the pixmap where it can. `false` when no pixmap could be
    /// allocated, in which case the caller draws the document straight into the buffer.
    fn rasterise(&mut self, key: BaseKey, draw: impl FnOnce(&mut PixmapMut)) -> bool {
        let reuse = self
            .pixmap
            .as_ref()
            .is_some_and(|pm| pm.width() == key.width && pm.height() == key.height);
        if !reuse {
            match Pixmap::new(key.width, key.height) {
                Some(pixmap) => self.pixmap = Some(pixmap),
                None => {
                    self.key = None;
                    return false;
                }
            }
        }
        let Some(pixmap) = self.pixmap.as_mut() else {
            return false;
        };
        // The background is the drawing closure's business: the raster and the frame that draws the
        // document straight into the buffer both have to paint the same one, scrim included.
        draw(&mut pixmap.as_mut());
        self.key = Some(key);
        true
    }

    /// Fill the frame's region with the raster's pixels. The raster holds tiny-skia's
    /// premultiplied RGBA and so does the buffer until the caller swaps R and B for the
    /// `ARGB8888` fallback, so this is a row-wise copy.
    fn fill(&self, buf: &mut [u8], region: DamageRegion, width: u32, height: u32) {
        let Some(pixmap) = &self.pixmap else {
            return;
        };
        let (x0, y0, x1, y1) = region.rows(width, height);
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let src = pixmap.data();
        for y in y0..y1 {
            let (start, end) = (
                ((y * width + x0) * 4) as usize,
                ((y * width + x1) * 4) as usize,
            );
            buf[start..end].copy_from_slice(&src[start..end]);
        }
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
    /// One frame with a throwaway cache, so the document is rasterised again: the baseline a
    /// caller that renders once wants, and what a frame that follows a document change costs.
    pub fn render(
        &mut self,
        buf: &mut [u8],
        width: u32,
        height: u32,
        scale: f32,
        view: View,
        ann: &OutputAnnotations,
        overlay: &Overlay,
        damage: Option<Rect>,
    ) {
        let mut render_cache = FrameCache::default();
        self.render_with_cache(
            buf,
            width,
            height,
            scale,
            view,
            ann,
            overlay,
            damage,
            &mut render_cache,
            &[],
        );
    }

    /// One frame, reusing `cache` across frames: the daemon's path, and the one the benchmark
    /// measures. The committed document is filled in from the cache rather than drawn again, so a
    /// frame costs what its damage region costs.
    #[allow(clippy::too_many_arguments)]
    pub fn render_with_cache(
        &mut self,
        buf: &mut [u8],
        width: u32,
        height: u32,
        scale: f32,
        view: View,
        ann: &OutputAnnotations,
        overlay: &Overlay,
        damage: Option<Rect>,
        cache: &mut FrameCache,
        skip: &[ItemId],
    ) {
        let FrameCache { text, base } = cache;
        let screen = Screen { scale, view };
        text.sync(ann.texts.len(), screen.len(1.0));
        let region = match damage {
            None => DamageRegion::All,
            Some(d) => DamageRegion::from_logical(d, scale, width, height),
        };
        // The scrim an edit-mode frame is painted on, and nothing at all while locked or while it has
        // been switched off. The overlay's rail is `Some` exactly in edit mode — `App::current_toolbar`
        // decides that — and it carries whether the backdrop is on, so the two are one read.
        let scrim = overlay
            .toolbar
            .as_ref()
            .is_some_and(|toolbar| toolbar.scrim)
            .then(|| parse_hex(SCRIM_FILL));
        // The committed document comes from the raster. Only a frame whose raster cannot be
        // allocated draws it into the buffer itself.
        let key = BaseKey {
            width,
            height,
            scale,
            view,
            skip: skip.to_vec(),
            scrim: scrim.is_some(),
        };
        let mut draw_document = false;
        if base.matches(&key) {
            base.fill(buf, region, width, height);
        } else {
            let renderer = &mut *self;
            let built = base.rasterise(key, |target| {
                renderer.draw_document(target, ann, screen, text, skip, scrim)
            });
            if built {
                base.fill(buf, region, width, height);
            } else {
                region.clear(buf, width, height);
                draw_document = true;
            }
        }
        let Some(mut pixmap) = PixmapMut::from_bytes(buf, width, height) else {
            return;
        };
        if draw_document {
            self.draw_document(&mut pixmap, ann, screen, text, skip, scrim);
        }
        if let Some(s) = &overlay.stroke {
            draw_stroke(&mut pixmap, s, screen);
        }
        if let Some(s) = &overlay.shape {
            draw_shape(&mut pixmap, s, screen);
        }
        if let Some(note) = &overlay.note {
            self.draw_note(&mut pixmap, note, screen, None);
        }
        if let Some(t) = &overlay.text {
            match t.note.and_then(|i| ann.notes.get(i)) {
                // A note being typed in: its square and its date are content, so the whole note is
                // drawn with the live text on it rather than the text alone.
                Some(note) => self.draw_note(&mut pixmap, note, screen, Some(t)),
                None => self.draw_text(&mut pixmap, &t.item, screen, Some(t), None, None),
            }
        }
        // The elements a live gesture is dragging, at the position the gesture puts them. They are
        // the same copies the release writes into the document, so the preview cannot lie.
        if let Some(selection) = &overlay.selection
            && let Some(transform) = selection.transform
        {
            for id in &selection.items {
                match id {
                    ItemId::Stroke(i) => {
                        if let Some(s) = ann.strokes.get(*i) {
                            draw_stroke(&mut pixmap, &transform.stroke(s), screen);
                        }
                    }
                    ItemId::Text(i) => {
                        if let Some(item) = ann.texts.get(*i) {
                            self.draw_text(
                                &mut pixmap,
                                &transform.text(item),
                                screen,
                                None,
                                None,
                                None,
                            );
                        }
                    }
                    ItemId::Shape(i) => {
                        if let Some(s) = ann.shapes.get(*i) {
                            draw_shape(&mut pixmap, &transform.shape(s), screen);
                        }
                    }
                    ItemId::Note(i) => {
                        if let Some(n) = ann.notes.get(*i) {
                            self.draw_note(&mut pixmap, &transform.note(n), screen, None);
                        }
                    }
                }
            }
        }
        if let Some(frame) = overlay
            .selection
            .as_ref()
            .and_then(|s| selection_frame(ann, &s.items, s.transform))
        {
            draw_selection(&mut pixmap, view.screen_rect(frame), screen);
        }
        if let Some(marquee) = overlay.marquee {
            draw_marquee(&mut pixmap, marquee, screen);
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
        if let Some(at) = overlay.eraser {
            // The marker is what the pointer aims with, so its size is a screen length even though
            // the point it sits on is the canvas point it will erase.
            let p = screen.point(at);
            draw_eraser_marker(&mut pixmap, p, screen.ui(1.0));
        }
    }

    /// Draw the committed document into `pixmap`: the raster build, and the fallback for a frame
    /// whose raster could not be allocated. The shaped-line cache is filled here, once per
    /// document change, so a frame that reuses the raster shapes no text at all. `scrim` is the
    /// background the document is drawn on: the whole pixmap is filled with it first, so this is the
    /// one place a frame's background comes from.
    fn draw_document(
        &mut self,
        pixmap: &mut PixmapMut,
        ann: &OutputAnnotations,
        screen: Screen,
        text: &mut TextLayoutCache,
        skip: &[ItemId],
        scrim: Option<[u8; 4]>,
    ) {
        pixmap.fill(match scrim {
            Some(c) => SkColor::from_rgba8(c[0], c[1], c[2], c[3]),
            None => SkColor::TRANSPARENT,
        });
        for (index, s) in ann.strokes.iter().enumerate() {
            if skip.contains(&ItemId::Stroke(index)) {
                continue;
            }
            draw_stroke(pixmap, s, screen);
        }
        for (index, s) in ann.shapes.iter().enumerate() {
            if skip.contains(&ItemId::Shape(index)) {
                continue;
            }
            draw_shape(pixmap, s, screen);
        }
        for (index, item) in ann.texts.iter().enumerate() {
            // The box under edit is the overlay's, and a box a gesture is dragging is drawn with
            // the live gesture: the raster must not hold either, because the live one is what the
            // frame paints over it. A live text that has become shorter would otherwise leave the
            // committed glyphs behind.
            if skip.contains(&ItemId::Text(index)) {
                continue;
            }
            let size = screen.len(item.size);
            if !text.matches(index, &item.text, size) {
                let layout = self.prepare_text(&item.text, item, screen, None);
                text.insert(index, &item.text, size, layout);
            }
            if let Some(layout) = text.get_mut(index) {
                self.draw_prepared_text(pixmap, item, screen, layout, None);
            }
        }
        // Notes last: a note is what the user reads the drawing against, so nothing a later tool draws
        // can bury it, and the pointer hits what it can see.
        for (index, note) in ann.notes.iter().enumerate() {
            if skip.contains(&ItemId::Note(index)) {
                continue;
            }
            self.draw_note(pixmap, note, screen, None);
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
        // The plate the whole rail sits on: the block column, inset `TOOLBAR_PAD` from the blocks on
        // all four sides — which is exactly the column's own extent, since the column already carries
        // that padding at each end. Its width is the block column and not the room a stretched capsule
        // may take, so a capsule reaching past it is drawn over the wallpaper as its own block, rather
        // than the plate appearing to grow.
        round_rect(
            pixmap,
            Rect {
                x: (ox - TOOLBAR_PAD) * scale,
                y: oy * scale,
                w: (TOOLBAR_BLOCK + 2.0 * TOOLBAR_PAD) * scale,
                h: TOOLBAR_COLUMN * scale,
            },
            TOOLBAR_RADIUS * scale,
            parse_hex(TOOLBAR_PLATE_FILL),
            None,
        );
        // How far the block under the pointer has opened. The layout is settled, so a frame of the
        // transition only moves the capsule's trailing edge and the text that edge uncovers.
        let grow = toolbar.grow.clamp(0.0, 1.0);
        for control in &pill.controls {
            let width = if control.stretched {
                TOOLBAR_BLOCK + (control.width - TOOLBAR_BLOCK) * grow
            } else {
                control.width
            };
            // A block of the drawer is offset a column to the right of the rail's.
            let column_x = control.block.column as f32 * TOOLBAR_DRAWER_X;
            let block = Rect {
                x: (ox + column_x) * scale,
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
            let square = Rect {
                y: block.y,
                x: block.x,
                w: TOOLBAR_BLOCK * scale,
                h: TOOLBAR_BLOCK * scale,
            };
            match &control.block.mark {
                Mark::Swatch(index) => {
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
                        square,
                        TOOLBAR_RADIUS * scale,
                        parse_hex(PALETTE[*index]),
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
                }
                mark => {
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
                    // What the block draws stays where it is and the capsule grows to its right,
                    // so the block reads as the same square, stretched. An icon or the readout's
                    // number is centred in the square, never in the stretched rectangle.
                    match mark {
                        Mark::Icon(icon) => {
                            draw_icon(pixmap, *icon, square, scale, TOOLBAR_FOREGROUND)
                        }
                        Mark::Value(value) => {
                            let width = self.text_width(value, TOOLBAR_TEXT_SIZE);
                            let item = toolbar_text(
                                ox + (TOOLBAR_BLOCK - width) / 2.0,
                                oy + control.y,
                                TOOLBAR_BLOCK,
                                TOOLBAR_TEXT_SIZE,
                                TOOLBAR_FOREGROUND,
                                value,
                            );
                            self.draw_text(
                                pixmap,
                                &item,
                                Screen {
                                    scale,
                                    view: View::default(),
                                },
                                None,
                                None,
                                None,
                            );
                        }
                        Mark::Swatch(_) => {}
                    }
                }
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
                self.draw_text(
                    pixmap,
                    &item,
                    Screen {
                        scale,
                        view: View::default(),
                    },
                    None,
                    Some(ox + width),
                    None,
                );
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
        // One row counter per column: the drawer is a second column, so a block in it starts at the
        // top of its own column and leaves the rail's height alone.
        let mut y = [TOOLBAR_PAD; 2];
        for block in toolbar_blocks(toolbar) {
            let column = block.column as usize;
            let x = block.column as f32 * TOOLBAR_DRAWER_X;
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
                        x,
                        y: y[column],
                        w: width,
                        h: TOOLBAR_BLOCK,
                    },
                    action,
                ));
            }
            controls.push(Control {
                block,
                y: y[column],
                width,
                stretched: width > TOOLBAR_BLOCK,
            });
            y[column] += TOOLBAR_BLOCK + gap;
        }
        debug_assert_eq!(y[0] + TOOLBAR_PAD, TOOLBAR_COLUMN);
        // The rail consumes every press on a block, and the rows between its own blocks belong to a
        // neighbour as well: a press on the size readout or in a gap has to step something rather than
        // start a stroke under the rail. The drawer's column is not filled that way: it is only as tall
        // as its controls, so a press beside one is the canvas's. The rail's controls are built before
        // the drawer's, so they are the leading rows of `hits`.
        let rail_rows = hits.iter().take_while(|(r, _)| r.x == 0.0).count();
        if let Some((first, _)) = hits.first_mut() {
            first.h += first.y;
            first.y = 0.0;
        }
        for index in 0..rail_rows.saturating_sub(1) {
            hits[index].0.h = hits[index + 1].0.y - hits[index].0.y;
        }
        if rail_rows > 0 {
            hits[rail_rows - 1].0.h = TOOLBAR_COLUMN - hits[rail_rows - 1].0.y;
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

    /// Shapes `text` at the size the view puts on screen. The prepared box comes back in **canvas**
    /// coordinates, because that is what a click and the selection frame measure; a caller that needs
    /// it on screen maps it through the view.
    /// `wrap` is the width the text may occupy, in canvas units, or `None` for text that is clipped
    /// rather than wrapped. A note passes its inner width; a text box passes nothing, so its long lines
    /// run to the output's edge as they always have.
    fn prepare_text(
        &mut self,
        text: &str,
        item: &TextItem,
        screen: Screen,
        wrap: Option<f32>,
    ) -> PreparedText {
        let size = screen.len(item.size);
        let mut buffer = Buffer::new_empty(Metrics::new(size, size * LINE_HEIGHT_SCALE));
        match wrap {
            Some(width) => {
                buffer.set_size(Some(screen.len(width)), None);
                // Word wrapping, with the glyph as the fallback for a token wider than the box: a URL
                // must not be drawn out of a note's side.
                buffer.set_wrap(Wrap::WordOrGlyph);
            }
            None => buffer.set_size(None, None),
        }
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
            // The prepared layout is in device pixels at the effective size; the box a click and the
            // selection measure is in canvas units.
            w: max_right / screen.len(1.0),
            h: max_bottom / screen.len(1.0),
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

    /// The region a text box's content occupies, which is what a click that re-enters it is
    /// tested against.
    pub fn text_edit_bounds(&mut self, text: &str, item: &TextItem, screen: Screen) -> Rect {
        self.prepare_text(text, item, screen, None)
            .paint_bounds
            .grown(CURSOR_WIDTH)
    }

    /// Draw a note: its square, the text on it, and the day it was made. `edit` is the note being
    /// typed in, whose live text and caret stand in for the committed text.
    fn draw_note(
        &mut self,
        pixmap: &mut PixmapMut,
        note: &Note,
        screen: Screen,
        edit: Option<&TextOverlay>,
    ) {
        let square = screen.rect(note.bounds());
        round_rect(
            pixmap,
            square,
            screen.len(NOTE_RADIUS),
            parse_hex(&note.color),
            Some((parse_hex(note.ink()), screen.ui(TOOLBAR_HAIRLINE))),
        );
        // The live text is the overlay's: a note that has become shorter must not leave the committed
        // glyphs behind, so the caller leaves the note out of the raster while it is being edited.
        let item = note.text_item();
        self.draw_text(pixmap, &item, screen, edit, None, Some(note.text_box().w));
        let date = toolbar_text(
            0.0,
            0.0,
            NOTE_DATE_SIZE * LINE_HEIGHT_SCALE,
            NOTE_DATE_SIZE,
            note.ink(),
            &note.created,
        );
        // Bottom-right, measured rather than estimated.
        let width = self.text_width(&note.created, NOTE_DATE_SIZE);
        let box_ = note.date_box();
        let origin = screen.point([box_.x + box_.w - width, box_.y]);
        let item = TextItem {
            x: origin[0] / screen.scale,
            y: origin[1] / screen.scale,
            ..date
        };
        self.draw_text(
            pixmap,
            &item,
            Screen {
                scale: screen.scale,
                view: View::default(),
            },
            None,
            None,
            None,
        );
    }

    /// The height `text` needs when it wraps at `note`'s inner width, in canvas units. The daemon grows
    /// the square by this as the text is typed, which is what keeps the text inside the note without the
    /// pure layer having to lay text out.
    pub fn wrapped_text_height(&mut self, text: &str, note: &Note, screen: Screen) -> f32 {
        let item = note.text_item();
        self.prepare_text(text, &item, screen, Some(note.text_box().w))
            .paint_bounds
            .h
    }

    /// `reveal` is the logical x a progress animation has uncovered: glyphs to its right are
    /// left out, so the text appears as the shape around it grows.
    fn draw_prepared_text(
        &mut self,
        pixmap: &mut PixmapMut,
        item: &TextItem,
        screen: Screen,
        prepared: &mut PreparedText,
        reveal: Option<f32>,
    ) {
        let origin = screen.point([item.x, item.y]);
        let (ox, oy) = (origin[0], origin[1]);
        let c = parse_hex(&item.color);
        let color = Color::rgba(c[0], c[1], c[2], c[3]);
        let (clip_w, clip_h) = (pixmap.width(), pixmap.height());
        let reveal = reveal.map(|x| screen.point([x, 0.0])[0]);
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

    #[allow(clippy::too_many_arguments)]
    fn draw_text(
        &mut self,
        pixmap: &mut PixmapMut,
        item: &TextItem,
        screen: Screen,
        edit: Option<&TextOverlay>,
        reveal: Option<f32>,
        wrap: Option<f32>,
    ) {
        let (committed, preedit, caret) = match edit {
            Some(e) => (
                e.buffer.text.as_str(),
                e.preedit.as_str(),
                Some(e.buffer.cursor_bytes()),
            ),
            None => (item.text.as_str(), "", None),
        };
        // A text box has no width to wrap at, so its text uses explicit newlines only and a long line
        // runs on. A note passes its inner width, and then the layout wraps.
        // The preedit renders where the caret is, so the text after the caret stays after it.
        let display = match caret.filter(|_| !preedit.is_empty()) {
            Some(at) => Cow::Owned(format!(
                "{}{}{}",
                &committed[..at],
                preedit,
                &committed[at..]
            )),
            None => Cow::Borrowed(committed),
        };
        if display.is_empty() && edit.is_none() {
            return;
        }
        let mut prepared = self.prepare_text(display.as_ref(), item, screen, wrap);
        let geometry = caret_geometry(&prepared, caret.unwrap_or(committed.len()), preedit.len());
        self.draw_prepared_text(pixmap, item, screen, &mut prepared, reveal);

        if edit.is_some() {
            let size = screen.len(item.size);
            let c = parse_hex(&item.color);
            let color = Color::rgba(c[0], c[1], c[2], c[3]);
            let origin = screen.point([item.x, item.y]);
            let (ox, oy) = (origin[0], origin[1]);
            if let Some((x0, x1)) = geometry.preedit {
                fill(
                    pixmap,
                    ox + x0,
                    oy + (geometry.baseline + size * 0.15),
                    (x1 - x0).max(0.0),
                    screen.ui(UNDERLINE_HEIGHT),
                    color,
                );
            }
            // The caret is the line box the text sits in, so it matches the typed text instead
            // of floating at a hardcoded fraction of the font size (the empty-line baseline and
            // a real glyph's baseline are not the same fraction).
            fill(
                pixmap,
                ox + geometry.x,
                oy + geometry.top,
                screen.ui(CURSOR_WIDTH),
                geometry.height,
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

/// Damage box of a transient frame: the union of the element's previous and current position,
/// plus the rail's box when the rail is what changed. It stays this small because a frame paints
/// no document element — the ones under the box come back from the raster — so it never has to
/// grow to cover them the way `damage_box` used to.
pub fn transient_damage(last: Option<Rect>, now: Option<Rect>, rail: Option<Rect>) -> Rect {
    let mut box_ = match (last, now) {
        (Some(a), Some(b)) => a.union(b),
        (Some(a), None) => a,
        (None, Some(b)) => b,
        (None, None) => Rect {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
        },
    };
    if let Some(rail) = rail {
        box_ = box_.union(rail);
    }
    box_
}

/// Bounding box of the transient overlay elements of a frame, in logical pixels: the
/// in-progress stroke, the shape being dragged out, the eraser marker and the marquee. `None` when
/// the overlay draws none of them. The selection's own box needs the document, so the caller adds
/// it.
pub fn transient_bounds(overlay: &Overlay) -> Option<Rect> {
    let stroke = overlay.stroke.as_ref().and_then(Stroke::bounds);
    let shape = overlay.shape.as_ref().map(Shape::bounds);
    let note = overlay.note.as_ref().map(Note::bounds);
    let eraser = overlay.eraser.map(|[x, y]| {
        Rect {
            x,
            y,
            w: 0.0,
            h: 0.0,
        }
        .grown(ERASER_MARKER_RADIUS + 2.0)
    });
    let marquee = overlay.marquee.map(|m| m.grown(SELECT_HANDLE));
    [stroke, shape, note, eraser, marquee]
        .into_iter()
        .flatten()
        .reduce(Rect::union)
}

/// The selection frame and its handles, in logical pixels: one hairline and accent-filled
/// capsules, so the interface gains no shape and no colour it did not already have.
fn draw_selection(pixmap: &mut PixmapMut, frame: Rect, screen: Screen) {
    let c = parse_hex(TOOLBAR_ACCENT);
    let mut paint = Paint::default();
    paint.set_color_rgba8(c[0], c[1], c[2], c[3]);
    paint.anti_alias = true;
    // The frame's place comes from the canvas and its weight from the interface, so the handles stay
    // the same size to aim at at every zoom.
    let hairline = SkStroke {
        width: screen.ui(TOOLBAR_HAIRLINE),
        ..Default::default()
    };
    if let Some(path) = rounded_rect_path(screen.ui_rect(frame), 0.0) {
        pixmap.stroke_path(&path, &paint, &hairline, Transform::identity(), None);
    }
    // The stem is what makes the rotation handle read as part of the frame rather than as a dot
    // that floats above it.
    let middle_top = [frame.x + frame.w / 2.0, frame.y];
    let knob = rotate_handle(frame, SELECT_ROTATE_GAP);
    let mut stem = PathBuilder::new();
    stem.move_to(middle_top[0] * screen.scale, middle_top[1] * screen.scale);
    stem.line_to(knob[0] * screen.scale, knob[1] * screen.scale);
    if let Some(path) = stem.finish() {
        pixmap.stroke_path(&path, &paint, &hairline, Transform::identity(), None);
    }
    for point in Handle::ALL.iter().map(|h| h.point(frame)).chain([knob]) {
        let mut dot = PathBuilder::new();
        let point = [point[0] * screen.scale, point[1] * screen.scale];
        dot.push_circle(point[0], point[1], screen.ui(SELECT_HANDLE / 2.0));
        if let Some(path) = dot.finish() {
            pixmap.fill_path(
                &path,
                &paint,
                tiny_skia::FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
    }
}

/// The marquee of a selection drag: the same hairline, with no fill of its own.
fn draw_marquee(pixmap: &mut PixmapMut, marquee: Rect, screen: Screen) {
    let Some(path) = rounded_rect_path(screen.rect(marquee), 0.0) else {
        return;
    };
    let c = parse_hex(TOOLBAR_ACCENT);
    let mut paint = Paint::default();
    paint.set_color_rgba8(c[0], c[1], c[2], c[3]);
    paint.anti_alias = true;
    let hairline = SkStroke {
        width: screen.ui(TOOLBAR_HAIRLINE),
        ..Default::default()
    };
    pixmap.stroke_path(&path, &paint, &hairline, Transform::identity(), None);
}

/// Whether two toolbar states lay out the same rail. The transition progress is not part of the
/// layout: a block's final width is settled before the pop-out opens, so its frames reuse the
/// measured layout instead of measuring the label again.
fn same_layout(a: &Toolbar, b: &Toolbar) -> bool {
    // Everything that decides what blocks exist and how wide they are. `grow` is deliberately not here:
    // the transition progress does not change the layout, only what the edge has uncovered. `more` and
    // `scrim` are: one adds a block and the other changes a label, so leaving either out would reuse the
    // previous layout and make the drawer's control unreachable.
    a.tool == b.tool
        && a.color == b.color
        && a.size == b.size
        && a.hover == b.hover
        && a.more == b.more
        && a.scrim == b.scrim
}

/// One block of the rail as laid out: where it sits in the column, how wide it is drawn, and
/// whether the pointer stretched it into a capsule.
#[derive(Clone)]
struct Control {
    block: Block,
    /// Top of the block, in its column's coordinates.
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
        // The plate reaches a pad outside the blocks horizontally and starts at the column's own top,
        // which already carries that padding — and the hairline edge is stroked around a path, so it
        // reaches one logical pixel past. The box therefore holds every pixel the rail paints: any less
        // and part of the plate would fall outside a frame that repaints only the rail, keeping stale
        // pixels behind.
        x: x - TOOLBAR_PAD - TOOLBAR_HAIRLINE,
        y: y - TOOLBAR_HAIRLINE,
        w: (width - 2.0 * TOOLBAR_MARGIN).clamp(0.0, TOOLBAR_RESERVE)
            + 2.0 * (TOOLBAR_PAD + TOOLBAR_HAIRLINE),
        h: TOOLBAR_COLUMN + 2.0 * TOOLBAR_HAIRLINE,
    }
}

/// Where the caret and the preedit sit in a shaped line, in logical pixels relative to the
/// text's own origin.
struct CaretGeometry {
    /// Left edge of the caret.
    x: f32,
    /// Top and height of the line box the caret spans, and its baseline.
    top: f32,
    height: f32,
    baseline: f32,
    /// Start and end of the preedit's underline, when there is a preedit.
    preedit: Option<(f32, f32)>,
}

/// Place the caret at a byte offset in the shaped text, and the preedit that starts there. The
/// offset belongs to one of the lines cosmic-text laid out; an offset past every glyph of a line
/// is that line's end, which is where a caret with nothing after it belongs. The last line is the
/// fallback, so a caret at the end of the text lands exactly where it used to always be drawn.
fn caret_geometry(prepared: &PreparedText, at: usize, preedit_len: usize) -> CaretGeometry {
    let mut geometry = CaretGeometry {
        x: prepared.line_w,
        top: prepared.line_top,
        height: prepared.line_height,
        baseline: prepared.baseline,
        preedit: None,
    };
    // The caret sits after the preedit while one is being composed, which is where the text
    // being composed grows from; the underline covers the preedit itself.
    let caret_at = at + preedit_len;
    let mut placed = false;
    // The lines are laid out in order, so their byte offsets accumulate.
    let mut line_start = 0;
    for run in prepared.buffer.layout_runs() {
        let line_end = line_start + run.text.len();
        if !placed && caret_at >= line_start && caret_at <= line_end {
            placed = true;
            geometry.x = run
                .glyphs
                .iter()
                .find(|glyph| glyph.start >= caret_at)
                .map_or(run.line_w, |glyph| glyph.x);
            geometry.top = run.line_top;
            geometry.height = run.line_height;
            geometry.baseline = run.line_y;
        }
        if preedit_len > 0 {
            for glyph in run
                .glyphs
                .iter()
                .filter(|glyph| glyph.start >= at && glyph.start < at + preedit_len)
            {
                let (x0, x1) = (glyph.x, glyph.x + glyph.w);
                geometry.preedit = Some(match geometry.preedit {
                    Some((a, b)) => (a.min(x0), b.max(x1)),
                    None => (x0, x1),
                });
            }
        }
        line_start = line_end + 1;
    }
    geometry
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

/// An icon, drawn inside the square of its block: geometry, in the block's foreground colour, in the
/// interface's own stroke language. The shape tools' icons call the code their shapes are drawn by,
/// from the same two points, so an icon cannot drift from what its tool draws.
fn draw_icon(pixmap: &mut PixmapMut, icon: Icon, square: Rect, scale: f32, color: &str) {
    let c = parse_hex(color);
    let mut paint = Paint::default();
    paint.set_color_rgba8(c[0], c[1], c[2], c[3]);
    paint.anti_alias = true;
    let box_ = square.grown(-ICON_INSET * scale);
    if box_.w <= 0.0 || box_.h <= 0.0 {
        return;
    }
    let (x, y, w, h) = (box_.x, box_.y, box_.w, box_.h);
    let stroke = SkStroke {
        width: ICON_STROKE * scale,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Default::default()
    };
    // A stroke or a shape is drawn in logical pixels by the code that draws the real one, so the
    // icon is the same drawing, only smaller. An icon is interface, not canvas: it takes the output's
    // scale and no view, so it stays the same size however the drawing is zoomed.
    let icon_screen = Screen {
        scale,
        view: View::default(),
    };
    let l = Rect {
        x: x / scale,
        y: y / scale,
        w: w / scale,
        h: h / scale,
    };
    if let Some(kind) = match icon {
        Icon::Circle => Some(ShapeKind::Circle),
        Icon::Rect => Some(ShapeKind::Rect),
        Icon::Diamond => Some(ShapeKind::Diamond),
        Icon::Line => Some(ShapeKind::Line),
        Icon::Arrow => Some(ShapeKind::Arrow),
        _ => None,
    } {
        // A line runs corner to corner; a closed shape fills the box it is given.
        let (from, to) = match kind {
            ShapeKind::Line | ShapeKind::Arrow => ([l.x, l.y + l.h], [l.x + l.w, l.y]),
            _ => ([l.x, l.y], [l.x + l.w, l.y + l.h]),
        };
        draw_shape(
            pixmap,
            &Shape {
                kind,
                color: color.into(),
                width: ICON_STROKE,
                from,
                to,
            },
            icon_screen,
        );
        return;
    }
    if icon == Icon::More {
        // Three dots: the macOS way of saying "there is more here".
        for at in [0.3, 0.5, 0.7] {
            let mut dot = PathBuilder::new();
            dot.push_circle(x + w * at, y + h / 2.0, ICON_STROKE * 0.9 * scale);
            if let Some(path) = dot.finish() {
                pixmap.fill_path(
                    &path,
                    &paint,
                    tiny_skia::FillRule::Winding,
                    Transform::identity(),
                    None,
                );
            }
        }
        return;
    }
    if icon == Icon::Scrim {
        // A square, half of it solid: what the backdrop does, in one glyph.
        if let Some(square) = rounded_rect_path(box_, 2.0 * scale) {
            pixmap.stroke_path(&square, &paint, &stroke, Transform::identity(), None);
            let mut half = PathBuilder::new();
            half.move_to(x, y + h * 0.5);
            half.line_to(x + w, y + h * 0.5);
            if let Some(path) = half.finish() {
                pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
            }
            let mut solid = PathBuilder::new();
            solid.move_to(x + w * 0.25, y + h * 0.25);
            solid.line_to(x + w * 0.75, y + h * 0.25);
            solid.line_to(x + w * 0.75, y + h * 0.75);
            solid.line_to(x + w * 0.25, y + h * 0.75);
            solid.close();
            if let Some(path) = solid.finish() {
                pixmap.fill_path(
                    &path,
                    &paint,
                    tiny_skia::FillRule::Winding,
                    Transform::identity(),
                    None,
                );
            }
        }
        return;
    }
    if icon == Icon::Note {
        // A note: a rounded square with two lines of text on it. `fraction` is where the line sits
        // down the icon: naming it `y` would shadow the box's own top and draw at the pixmap's corner.
        for (fraction, width) in [(0.32, 0.64), (0.5, 0.48)] {
            let mut line = PathBuilder::new();
            line.move_to(x + w * 0.18, y + h * fraction);
            line.line_to(x + w * (0.18 + width), y + h * fraction);
            if let Some(path) = line.finish() {
                pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
            }
        }
        if let Some(square) = rounded_rect_path(box_, 3.0 * scale) {
            pixmap.stroke_path(&square, &paint, &stroke, Transform::identity(), None);
        }
        return;
    }
    if icon == Icon::Hand {
        // A palm with three fingers and a thumb: a hand, drawn in the same stroke language as the
        // rest of the set.
        let mut hand = PathBuilder::new();
        for at in [0.28, 0.5, 0.72] {
            hand.move_to(x + w * at, y + h * 0.42);
            hand.line_to(x + w * at, y + h * 0.95);
        }
        hand.move_to(x + w * 0.06, y + h * 0.62);
        hand.line_to(x + w * 0.3, y + h * 0.5);
        hand.move_to(x + w * 0.28, y + h * 0.42);
        hand.line_to(x + w * 0.28, y + h * 0.2);
        hand.move_to(x + w * 0.5, y + h * 0.42);
        hand.line_to(x + w * 0.5, y + h * 0.1);
        hand.move_to(x + w * 0.72, y + h * 0.42);
        hand.line_to(x + w * 0.72, y + h * 0.22);
        hand.move_to(x + w * 0.28, y + h * 0.95);
        hand.line_to(x + w * 0.72, y + h * 0.95);
        if let Some(path) = hand.finish() {
            pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
        }
        return;
    }
    if icon == Icon::Pen {
        draw_stroke(
            pixmap,
            &Stroke {
                color: color.into(),
                width: ICON_STROKE,
                points: vec![
                    [l.x + l.w * 0.05, l.y + l.h * 0.8],
                    [l.x + l.w * 0.35, l.y + l.h * 0.25],
                    [l.x + l.w * 0.65, l.y + l.h * 0.75],
                    [l.x + l.w * 0.95, l.y + l.h * 0.2],
                ],
            },
            icon_screen,
        );
        return;
    }
    let mut path = PathBuilder::new();
    let mut filled: Vec<tiny_skia::Path> = Vec::new();
    match icon {
        Icon::Eraser => {
            if let Some(rounded) = rounded_rect_path(box_, 4.0 * scale) {
                path.push_path(&rounded);
            }
        }
        Icon::Text => {
            path.move_to(x, y + h * 0.15);
            path.line_to(x + w, y + h * 0.15);
            path.move_to(x + w / 2.0, y + h * 0.15);
            path.line_to(x + w / 2.0, y + h);
        }
        Icon::Selection => {
            // The marquee's own look: a hairline frame with a grip at each corner.
            if let Some(frame) = rounded_rect_path(box_, 0.0) {
                let thin = SkStroke {
                    width: TOOLBAR_HAIRLINE * scale,
                    ..Default::default()
                };
                pixmap.stroke_path(&frame, &paint, &thin, Transform::identity(), None);
            }
            for corner in [[x, y], [x + w, y], [x + w, y + h], [x, y + h]] {
                let mut dot = PathBuilder::new();
                dot.push_circle(corner[0], corner[1], ICON_STROKE * scale);
                if let Some(dot) = dot.finish() {
                    filled.push(dot);
                }
            }
        }
        Icon::Smaller => {
            path.move_to(x + w * 0.2, y + h / 2.0);
            path.line_to(x + w * 0.8, y + h / 2.0);
        }
        Icon::Larger => {
            path.move_to(x + w * 0.2, y + h / 2.0);
            path.line_to(x + w * 0.8, y + h / 2.0);
            path.move_to(x + w / 2.0, y + h * 0.2);
            path.line_to(x + w / 2.0, y + h * 0.8);
        }
        Icon::Lock => {
            // The body, and the shackle over it: the block's end state, not its first act.
            if let Some(body) = rounded_rect_path(
                Rect {
                    x: x + w * 0.12,
                    y: y + h * 0.45,
                    w: w * 0.76,
                    h: h * 0.55,
                },
                2.0 * scale,
            ) {
                path.push_path(&body);
            }
            let (arm, top, arc) = (w * 0.3, h * 0.3, h * 0.1);
            path.move_to(x + arm, y + h * 0.45);
            path.line_to(x + arm, y + top);
            path.cubic_to(x + arm, y + arc, x + w - arm, y + arc, x + w - arm, y + top);
            path.line_to(x + w - arm, y + h * 0.45);
        }
        // Drawn by the branches above.
        Icon::Pen
        | Icon::More
        | Icon::Scrim
        | Icon::Note
        | Icon::Hand
        | Icon::Circle
        | Icon::Rect
        | Icon::Diamond
        | Icon::Line
        | Icon::Arrow => return,
    }
    if let Some(path) = path.finish() {
        pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    }
    for path in filled {
        pixmap.fill_path(
            &path,
            &paint,
            tiny_skia::FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
}

/// This crate's rectangle as the one tiny-skia takes, which rejects a negative or non-finite one.
fn sk_rect(r: Rect) -> Option<tiny_skia::Rect> {
    tiny_skia::Rect::from_xywh(r.x, r.y, r.w, r.h)
}

/// A shape, in device pixels: its outline stroked with its own pen width, and the head of an arrow
/// filled, because a stroked triangle would read as a hollow head rather than as an arrow.
fn draw_shape(pixmap: &mut PixmapMut, s: &Shape, screen: Screen) {
    if s.is_empty() {
        return;
    }
    let c = parse_hex(&s.color);
    let mut paint = Paint::default();
    paint.set_color_rgba8(c[0], c[1], c[2], c[3]);
    paint.anti_alias = true;
    let stroke = SkStroke {
        width: screen.len(s.width),
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Default::default()
    };
    let from = screen.point(s.from);
    let to = screen.point(s.to);
    let box_ = rect_of(from, to);
    match s.kind {
        ShapeKind::Circle | ShapeKind::Rect | ShapeKind::Diamond => {
            let Some(rect) = sk_rect(box_) else {
                return;
            };
            let mut pb = PathBuilder::new();
            match s.kind {
                ShapeKind::Circle => pb.push_oval(rect),
                ShapeKind::Rect => pb.push_rect(rect),
                _ => {
                    let (cx, cy) = (box_.x + box_.w / 2.0, box_.y + box_.h / 2.0);
                    pb.move_to(cx, box_.y);
                    pb.line_to(box_.x + box_.w, cy);
                    pb.line_to(cx, box_.y + box_.h);
                    pb.line_to(box_.x, cy);
                    pb.close();
                }
            }
            if let Some(path) = pb.finish() {
                pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
            }
        }
        ShapeKind::Line | ShapeKind::Arrow => {
            let mut pb = PathBuilder::new();
            pb.move_to(from[0], from[1]);
            pb.line_to(to[0], to[1]);
            if let Some(path) = pb.finish() {
                pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
            }
            if s.kind != ShapeKind::Arrow {
                return;
            }
            let (dx, dy) = (to[0] - from[0], to[1] - from[1]);
            let len = (dx * dx + dy * dy).sqrt();
            if len <= f32::EPSILON {
                return;
            }
            let (ux, uy) = (dx / len, dy / len);
            let head = screen.len(s.head_len());
            let half = screen.len(s.head_half());
            let base = [to[0] - ux * head, to[1] - uy * head];
            let (px, py) = (-uy * half, ux * half);
            let mut head_path = PathBuilder::new();
            head_path.move_to(to[0], to[1]);
            head_path.line_to(base[0] + px, base[1] + py);
            head_path.line_to(base[0] - px, base[1] - py);
            head_path.close();
            if let Some(path) = head_path.finish() {
                pixmap.fill_path(
                    &path,
                    &paint,
                    tiny_skia::FillRule::Winding,
                    Transform::identity(),
                    None,
                );
            }
        }
    }
}

fn draw_stroke(pixmap: &mut PixmapMut, s: &Stroke, screen: Screen) {
    let c = parse_hex(&s.color);
    let mut paint = Paint::default();
    paint.set_color_rgba8(c[0], c[1], c[2], c[3]);
    paint.anti_alias = true;
    let width = screen.len(s.width);
    let transform = Transform::identity();

    match s.points.as_slice() {
        [] => {}
        [only] => {
            let mut pb = PathBuilder::new();
            let p = screen.point(*only);
            pb.push_circle(p[0], p[1], width / 2.0);
            if let Some(path) = pb.finish() {
                pixmap.fill_path(&path, &paint, tiny_skia::FillRule::Winding, transform, None);
            }
        }
        points => {
            let Some(path) = smoothed_path(points, screen) else {
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
pub fn smoothed_path(points: &[[f32; 2]], screen: Screen) -> Option<tiny_skia::Path> {
    let (first, rest) = points.split_first()?;
    let (fx, fy) = {
        let p = screen.point(*first);
        (p[0], p[1])
    };
    let mut pb = PathBuilder::new();
    pb.move_to(fx, fy);
    for w in rest.windows(2) {
        let a = screen.point(w[0]);
        let mid = screen.point([(w[0][0] + w[1][0]) / 2.0, (w[0][1] + w[1][1]) / 2.0]);
        pb.quad_to(a[0], a[1], mid[0], mid[1]);
    }
    if let Some(last) = rest.last() {
        let p = screen.point(*last);
        pb.line_to(p[0], p[1]);
    }
    pb.finish()
}

/// `at` is in device pixels and `scale` is the output's, so the marker keeps its size at any zoom.
fn draw_eraser_marker(pixmap: &mut PixmapMut, at: [f32; 2], scale: f32) {
    let mut pb = PathBuilder::new();
    pb.push_circle(at[0], at[1], ERASER_MARKER_RADIUS * scale);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The identity view at a given output scale, for the tests that only care about the document.
    fn screen(scale: f32) -> Screen {
        Screen {
            scale,
            view: View::default(),
        }
    }
    use crate::canvas::{Selection, TextBuffer, Transform};

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
        cache: &mut FrameCache,
        annotations: &OutputAnnotations,
        buffer: &mut [u8],
    ) {
        renderer.render_with_cache(
            buffer,
            200,
            100,
            1.0,
            View::default(),
            annotations,
            &Overlay::default(),
            None,
            cache,
            &[],
        );
    }

    /// A document with a stroke and a text item, so a frame has something of both kinds to fill
    /// in from the cache.
    fn document() -> OutputAnnotations {
        OutputAnnotations {
            strokes: vec![Stroke {
                color: "#e01b24".into(),
                width: 4.0,
                points: vec![[10.0, 50.0], [190.0, 60.0]],
            }],
            texts: vec![text_item("cached text")],
            shapes: vec![],
            notes: vec![],
        }
    }

    #[test]
    fn warm_text_cache_matches_cold_text_cache() {
        let mut renderer = Renderer::new();
        let mut cache = FrameCache::default();
        let annotations = OutputAnnotations {
            strokes: vec![],
            texts: vec![text_item("cached text")],
            shapes: vec![],
            notes: vec![],
        };
        let mut cold = vec![0; 200 * 100 * 4];
        let mut warm = vec![0; 200 * 100 * 4];
        render_cached(&mut renderer, &mut cache, &annotations, &mut cold);
        render_cached(&mut renderer, &mut cache, &annotations, &mut warm);
        assert_eq!(cold, warm);
    }

    #[test]
    fn the_document_raster_is_reused_until_it_is_invalidated() {
        // What a frame costs stands or falls on this: the second frame must fill from the raster
        // instead of rasterising the document again.
        let mut renderer = Renderer::new();
        let mut cache = FrameCache::default();
        let annotations = document();
        let mut first = vec![0; 200 * 100 * 4];
        render_cached(&mut renderer, &mut cache, &annotations, &mut first);
        assert!(cache.base.matches(&BaseKey {
            width: 200,
            height: 100,
            scale: 1.0,
            view: View::default(),
            skip: Vec::new(),
            scrim: false,
        }));

        let mut second = vec![0; 200 * 100 * 4];
        render_cached(&mut renderer, &mut cache, &annotations, &mut second);
        assert_eq!(first, second, "a reused raster draws the same pixels");

        // Invalidating drops it, and the frame that follows is built from the document again.
        cache.invalidate();
        assert!(cache.base.key.is_none());
        let mut rebuilt = vec![0; 200 * 100 * 4];
        render_cached(&mut renderer, &mut cache, &annotations, &mut rebuilt);
        assert_eq!(first, rebuilt, "a rebuilt raster draws the same pixels");
    }

    #[test]
    fn a_changed_document_reaches_the_next_frame_after_invalidation() {
        let mut renderer = Renderer::new();
        let mut cache = FrameCache::default();
        let before = document();
        let mut stale = vec![0; 200 * 100 * 4];
        render_cached(&mut renderer, &mut cache, &before, &mut stale);

        let mut after = before.clone();
        after.texts[0].text = "after".into();
        cache.invalidate();
        let mut warm = vec![0; 200 * 100 * 4];
        render_cached(&mut renderer, &mut cache, &after, &mut warm);
        let mut cold = vec![0; 200 * 100 * 4];
        let mut cold_cache = FrameCache::default();
        render_cached(&mut renderer, &mut cold_cache, &after, &mut cold);
        assert_eq!(cold, warm);
        assert_ne!(stale, warm, "the old document may not survive the change");
    }

    #[test]
    fn the_box_under_edit_is_left_out_of_the_raster() {
        // The live text is the overlay's, so the committed text has to be absent from the raster:
        // a live text that has become shorter would otherwise leave glyphs behind.
        let mut renderer = Renderer::new();
        let mut cache = FrameCache::default();
        let annotations = document();
        let mut with_item = vec![0; 200 * 100 * 4];
        render_cached(&mut renderer, &mut cache, &annotations, &mut with_item);
        let mut without_item = vec![0; 200 * 100 * 4];
        renderer.render_with_cache(
            &mut without_item,
            200,
            100,
            1.0,
            View::default(),
            &annotations,
            &Overlay::default(),
            None,
            &mut cache,
            &[crate::canvas::ItemId::Text(0)],
        );
        assert_ne!(
            with_item, without_item,
            "the edited item's pixels are not in the raster"
        );
        // The stroke that is not under edit survives in both.
        assert!(without_item.chunks(4).any(|p| p[3] != 0));
    }

    #[test]
    fn a_changed_text_item_is_reshaped_without_invalidating_the_layout_cache() {
        // The layout cache compares the text and the size itself, so the shaped lines survive a
        // change; it is the document raster that has to be dropped, which is what
        // `App::doc_changed` does for the whole output.
        let mut renderer = Renderer::new();
        let mut cache = FrameCache::default();
        let mut annotations = OutputAnnotations {
            strokes: vec![],
            texts: vec![text_item("before")],
            shapes: vec![],
            notes: vec![],
        };
        let mut first = vec![0; 200 * 100 * 4];
        render_cached(&mut renderer, &mut cache, &annotations, &mut first);
        annotations.texts[0].text = "after".into();
        cache.base.invalidate();
        let mut warm = vec![0; 200 * 100 * 4];
        render_cached(&mut renderer, &mut cache, &annotations, &mut warm);
        let mut cold = vec![0; 200 * 100 * 4];
        let mut fresh_cache = FrameCache::default();
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
                more: false,
                scrim: true,
            }),
            ..Default::default()
        };
        let mut cold = vec![0; 640 * 400 * 4];
        renderer.render(
            &mut cold,
            640,
            400,
            1.0,
            View::default(),
            &annotations,
            &overlay,
            None,
        );
        let mut warm = vec![0; 640 * 400 * 4];
        renderer.render(
            &mut warm,
            640,
            400,
            1.0,
            View::default(),
            &annotations,
            &overlay,
            None,
        );
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
                note: None,
                preedit: String::new(),
            }),
            ..Default::default()
        };
        let bounds = renderer.text_edit_bounds("new text", &item, screen(1.0));
        let mut expected = vec![0; 200 * 100 * 4];
        renderer.render(
            &mut expected,
            200,
            100,
            1.0,
            View::default(),
            &annotations,
            &overlay,
            None,
        );
        let mut actual = vec![0; 200 * 100 * 4];
        let mut cache = FrameCache::default();
        renderer.render_with_cache(
            &mut actual,
            200,
            100,
            1.0,
            View::default(),
            &annotations,
            &overlay,
            Some(bounds),
            &mut cache,
            &[],
        );
        assert_eq!(actual, expected);
    }

    #[test]
    fn localized_text_edit_skips_the_stale_document_text() {
        let mut renderer = Renderer::new();
        let old_document = OutputAnnotations {
            strokes: vec![],
            texts: vec![text_item("before")],
            shapes: vec![],
            notes: vec![],
        };
        let mut new_document = old_document.clone();
        new_document.texts[0].text = "after".into();
        let edit = TextOverlay {
            item: old_document.texts[0].clone(),
            buffer: TextBuffer::new("after"),
            index: Some(0),
            note: None,
            preedit: String::new(),
        };
        let overlay = Overlay {
            text: Some(edit),
            ..Default::default()
        };
        let old_bounds = renderer.text_edit_bounds("before", &old_document.texts[0], screen(1.0));
        let new_bounds = renderer.text_edit_bounds("after", &new_document.texts[0], screen(1.0));
        let damage = old_bounds.union(new_bounds);

        let mut old_frame = vec![0; 200 * 100 * 4];
        renderer.render(
            &mut old_frame,
            200,
            100,
            1.0,
            View::default(),
            &old_document,
            &Overlay::default(),
            None,
        );
        let expected_overlay = Overlay {
            text: Some(TextOverlay {
                item: new_document.texts[0].clone(),
                buffer: TextBuffer::new("after"),
                index: None,
                note: None,
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
            View::default(),
            &OutputAnnotations::default(),
            &expected_overlay,
            None,
        );

        let mut actual = old_frame;
        let mut cache = FrameCache::default();
        renderer.render_with_cache(
            &mut actual,
            200,
            100,
            1.0,
            View::default(),
            &old_document,
            &overlay,
            Some(damage),
            &mut cache,
            &[crate::canvas::ItemId::Text(0)],
        );
        assert_eq!(actual, expected);
    }

    #[test]
    fn a_live_gesture_draws_its_element_once() {
        // The raster leaves the held element out and the overlay draws it with the live
        // transform, so a dragged element is painted exactly once. A frame drawn that way has to
        // be pixel-identical to the same frame drawn from a document that already holds the moved
        // element and nothing live: twice the ink, or none, would show up as a difference.
        let stored = document();
        let held = [ItemId::Stroke(0)];
        let transform = Transform::Translate {
            dx: 40.0,
            dy: -10.0,
        };
        let mut renderer = Renderer::new();
        let live = Overlay {
            selection: Some(Selection {
                items: held.to_vec(),
                transform: Some(transform),
            }),
            ..Default::default()
        };
        let mut dragged = vec![0; 200 * 100 * 4];
        renderer.render_with_cache(
            &mut dragged,
            200,
            100,
            1.0,
            View::default(),
            &stored,
            &live,
            None,
            &mut FrameCache::default(),
            &held,
        );

        let mut moved = stored.clone();
        moved.strokes[0] = transform.stroke(&stored.strokes[0]);
        let settled = Overlay {
            selection: Some(Selection {
                items: held.to_vec(),
                transform: None,
            }),
            ..Default::default()
        };
        let mut committed = vec![0; 200 * 100 * 4];
        renderer.render_with_cache(
            &mut committed,
            200,
            100,
            1.0,
            View::default(),
            &moved,
            &settled,
            None,
            &mut FrameCache::default(),
            &[],
        );
        assert_eq!(dragged, committed);
    }

    #[test]
    fn a_live_shape_is_drawn_once_and_not_by_the_raster() {
        // A shape being dragged out is the overlay's, so it is left out of the raster and drawn
        // live. Painting it twice, or not at all, would show up against the same frame drawn from
        // a document that already holds it.
        let held = [ItemId::Shape(0)];
        let shape = Shape {
            kind: ShapeKind::Circle,
            color: "#e01b24".into(),
            width: 3.0,
            from: [40.0, 20.0],
            to: [160.0, 80.0],
        };
        let mut renderer = Renderer::new();
        let stored = OutputAnnotations {
            strokes: vec![],
            texts: vec![],
            shapes: vec![shape.clone()],
            notes: vec![],
        };
        let live = Overlay {
            shape: Some(shape.clone()),
            ..Default::default()
        };
        let mut transient = vec![0; 200 * 100 * 4];
        renderer.render_with_cache(
            &mut transient,
            200,
            100,
            1.0,
            View::default(),
            &stored,
            &live,
            None,
            &mut FrameCache::default(),
            &held,
        );
        let settled = Overlay::default();
        let mut committed = vec![0; 200 * 100 * 4];
        renderer.render_with_cache(
            &mut committed,
            200,
            100,
            1.0,
            View::default(),
            &stored,
            &settled,
            None,
            &mut FrameCache::default(),
            &[],
        );
        assert_eq!(transient, committed);

        // The raster holds no shape while the gesture holds it: a frame that draws only the
        // raster is blank where the shape is.
        let mut cache = FrameCache::default();
        let mut only_raster = vec![0; 200 * 100 * 4];
        renderer.render_with_cache(
            &mut only_raster,
            200,
            100,
            1.0,
            View::default(),
            &stored,
            &settled,
            None,
            &mut cache,
            &held,
        );
        assert_eq!(
            only_raster.iter().filter(|b| **b != 0).count(),
            0,
            "a skipped shape is absent from the raster"
        );

        // And the live shape is what the damage box covers, head and all.
        let box_ = transient_bounds(&live).expect("a live shape has a box");
        assert!(box_.contains(40.0, 50.0) && box_.contains(160.0, 50.0));
    }

    /// Brightness of one pixel: an icon is drawn in the block's foreground colour, which is near
    /// white, while a block's own fill and its hairline edge are dark or blue.
    fn near_white(buf: &[u8], w: u32, x: u32, y: u32) -> bool {
        let i = ((y * w + x) * 4) as usize;
        buf[i] > 150 && buf[i + 1] > 150 && buf[i + 2] > 150
    }

    fn rail_state() -> Toolbar {
        Toolbar {
            tool: Tool::Pen,
            color: PALETTE[0],
            size: 3.0,
            hover: None,
            grow: 1.0,
            more: false,
            scrim: true,
        }
    }

    #[test]
    fn every_block_draws_an_icon_in_its_square() {
        // Every function's block shows a picture of what it does, at scale 1 and at scale 2: a
        // 30 pt block with a 1.5 pt stroke has to carry ink at both.
        let rail = rail_state();
        for scale in [1.0, 2.0] {
            // Logical size, and the device buffer the renderer is given for it.
            let (w, h) = (640.0, 1600.0);
            let (wd, hd) = ((w * scale) as u32, (h * scale) as u32);
            let mut renderer = Renderer::new();
            let pill = renderer.cached_toolbar_pill(&rail, w).unwrap();
            let mut buf = vec![0; (wd * hd * 4) as usize];
            renderer.render(
                &mut buf,
                wd,
                hd,
                scale,
                View::default(),
                &OutputAnnotations::default(),
                &Overlay {
                    toolbar: Some(rail),
                    ..Default::default()
                },
                None,
            );
            let (ox, oy) = toolbar_origin(h);
            let mut icons = 0;
            for control in &pill.controls {
                let Mark::Icon(icon) = control.block.mark else {
                    continue;
                };
                icons += 1;
                // The inside of the square, so neither the block's edge nor the cap of its
                // rounded corner can be mistaken for the icon.
                let (x0, y0) = (
                    ((ox + 3.0) * scale) as u32,
                    ((oy + control.y + 3.0) * scale) as u32,
                );
                let (x1, y1) = (
                    ((ox + TOOLBAR_BLOCK - 3.0) * scale) as u32,
                    ((oy + control.y + TOOLBAR_BLOCK - 3.0) * scale) as u32,
                );
                let ink = (y0..y1)
                    .flat_map(|y| (x0..x1).map(move |x| (x, y)))
                    .filter(|(x, y)| near_white(&buf, wd, *x, *y))
                    .count();
                assert!(ink > 0, "{icon:?} drew nothing at scale {scale}");
            }
            assert_eq!(
                icons, 15,
                "every function has an icon, and only functions do"
            );
        }
    }

    #[test]
    fn every_block_states_the_key_that_applies_it() {
        // A block's face carries no key letter any more, so the key is what its stretched text
        // ends with — and that key has to be one this daemon actually handles.
        use crate::input::{
            color_for_keysym, panel_action_for_keysym, size_step_for_keysym, tool_for_keysym,
        };
        use smithay_client_toolkit::seat::keyboard::Keysym;
        let keysym_of = |key: &str| match key {
            "[" => Some(Keysym::bracketleft),
            "]" => Some(Keysym::bracketright),
            "1" => Some(Keysym::_1),
            "2" => Some(Keysym::_2),
            "3" => Some(Keysym::_3),
            "4" => Some(Keysym::_4),
            "5" => Some(Keysym::_5),
            "Esc" => Some(Keysym::Escape),
            "P" => Some(Keysym::P),
            "E" => Some(Keysym::E),
            "T" => Some(Keysym::T),
            "S" => Some(Keysym::S),
            "O" => Some(Keysym::O),
            "R" => Some(Keysym::R),
            "D" => Some(Keysym::D),
            "L" => Some(Keysym::L),
            "A" => Some(Keysym::A),
            "H" => Some(Keysym::H),
            "N" => Some(Keysym::N),
            "M" => Some(Keysym::M),
            _ => None,
        };
        let blocks = toolbar_blocks(&rail_state());
        assert_eq!(blocks.len(), 21);
        let mut stated = 0;
        for block in &blocks {
            let Some(action) = block.action else {
                // The size readout: it applies nothing, so it says nothing and shows a value.
                assert!(block.label.is_empty());
                assert!(matches!(block.mark, Mark::Value(_)));
                continue;
            };
            let key = key_of(action);
            assert!(
                block.label.ends_with(&format!("({key})")),
                "'{}' does not end with its key ({key})",
                block.label
            );
            stated += 1;
            let keysym = keysym_of(key).unwrap_or_else(|| panic!("'{key}' is not a key we draw"));
            let applies = match action {
                ToolbarAction::Tool(tool) => tool_for_keysym(keysym) == Some(tool),
                ToolbarAction::Color(index) => color_for_keysym(keysym) == Some(index),
                ToolbarAction::SizeStep(dir) => {
                    size_step_for_keysym(keysym).is_some_and(|step| step == dir)
                }
                ToolbarAction::Lock => keysym == Keysym::Escape,
                ToolbarAction::More => panel_action_for_keysym(keysym) == Some(ToolbarAction::More),
                ToolbarAction::Scrim => {
                    panel_action_for_keysym(keysym) == Some(ToolbarAction::Scrim)
                }
            };
            assert!(applies, "'{}' names a key that does not do it", block.label);
        }
        // Twenty blocks apply something: five colours, eleven tools, two size steps, the drawer's
        // control and lock. The backdrop's switch is in the drawer, which is closed in this rail.
        assert_eq!(stated, 20);
    }
}
