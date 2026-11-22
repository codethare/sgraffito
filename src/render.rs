//! Frame composition: tiny-skia draws strokes and the eraser marker, cosmic-text draws text.
//!
//! `buf` is a tiny-skia premultiplied RGBA buffer already sized as logical size x scale.

use std::borrow::Cow;

use cosmic_text::{Attrs, Buffer, Color, Cursor, FontSystem, Metrics, Shaping, SwashCache, Wrap};
use tiny_skia::{
    Color as SkColor, LineCap, LineJoin, Paint, PathBuilder, Pixmap, PixmapMut,
    PremultipliedColorU8, Stroke as SkStroke, Transform,
};
use wayland_client::protocol::wl_shm::Format;

use crate::canvas::{
    Backdrop, Board, Handle, ItemId, LiveConnector, NOTE_DATE_SIZE, NOTE_RADIUS, Note, Overlay,
    Rect, SELECT_HANDLE, SELECT_ROTATE_GAP, Shape, ShapeKind, Stroke, TextItem, TextOverlay, Tool,
    Toolbar, ToolbarAction, View, note_ink, parse_hex, rect_of, rotate_handle,
    selection_frame_with,
};
use crate::config;
use crate::config::{CardPosition, ToolbarPosition};
use crate::sketch::{self, Seg};
/// Radius of the eraser marker circle, in logical pixels.
const ERASER_MARKER_RADIUS: f32 = 8.0;
/// The edit-mode toolbar: a rail of square blocks docked to one edge of the output, in the macOS
/// HUD idiom, built around the 13 pt control size the HIG gives as the macOS default. All of it
/// is logical pixels.
///
/// A left or right edge gives a vertical column of blocks; a top or bottom edge gives a horizontal
/// row. The blocks themselves stay upright — an icon or a label is never rotated — and every panel
/// opens across the rail toward the interior. The default is the left edge (the side measured
/// fastest for toolbar targets), with its vertical centre at the optical centre, roughly 5% above
/// the geometric centre, where the gaze rests.
const TOOLBAR_MARGIN: f32 = 18.0;
const TOOLBAR_CENTRE: f32 = 0.45;
/// Side of one block: the rail is a column or row of these, above the HIG's minimum pointer target.
const TOOLBAR_BLOCK: f32 = 30.0;
/// Between two blocks of the same group.
const TOOLBAR_GAP: f32 = 4.0;
/// Between two groups: the palette, the tools, the size control, `Esc`.
const TOOLBAR_GROUP_GAP: f32 = 10.0;
/// Around the whole column.
const TOOLBAR_PAD: f32 = 6.0;
const TOOLBAR_RADIUS: f32 = 8.0;
const TOOLBAR_HAIRLINE: f32 = 1.0;
/// The move button's width and height, in logical pixels. The width fits its label with the same
/// padding the rail's capsules use.
const MOVE_BUTTON_W: f32 = 150.0;
const MOVE_BUTTON_H: f32 = 30.0;
/// One size for every piece of text a block carries — its label, and the size readout's number:
/// the HIG's 13 pt macOS control size, so they share a size and a baseline instead of being matched
/// by eye. An icon is geometry rather than text and is not sized by this.
const TOOLBAR_TEXT_SIZE: f32 = 13.0;
/// The key a block carries on its face: 7 pt is the smallest size that still reads at scale 1, and
/// the badge is inset into the corner the icon leaves free rather than centred over the picture.
const BADGE_TEXT_SIZE: f32 = 7.0;
/// How far the badge is inset from the square's left and bottom edges, so the glyph sits inside the
/// rounded corner instead of touching the hairline.
const BADGE_INSET: f32 = 3.0;
/// The box a badged block draws its icon in: smaller than an unbadged icon's, and moved up and to
/// the right, because the badge owns the bottom-left corner. The two boxes never touch, so a badge
/// cannot be mistaken for part of the icon.
const BADGE_ICON_SIDE: f32 = 13.0;
const BADGE_ICON_TOP: f32 = 3.0;
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
/// damage box that reaches it. It also holds the line the `more` block shows while the pointer is
/// on it, which names the drawer's controls — two of them now, so the room is the first control's
/// phrase plus the second's, and a phrase that does not fit is not drawn rather than drawn outside
/// the box.
const TOOLBAR_RESERVE: f32 = 320.0;
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
/// A 24-point cell, the common dot-grid spacing in drawing and note products (and three 8-point
/// steps); the line is one logical pixel, the dot two.
const BACKDROP_SPACING: f32 = 24.0;
const BACKDROP_LINE: f32 = 1.0;
const BACKDROP_DOT: f32 = 2.0;
/// Mid-grey at partial opacity: visible over a light wallpaper and over a dark one, without
/// pretending to be content.
const BACKDROP_GRID_COLOR: &str = "#80808066";
const BACKDROP_DOT_COLOR: &str = "#80808099";
/// `#rrggbbaa`. The plate the rail sits on: black at 90% opacity, behind every block so the gaps
/// between them do not show the wallpaper at full strength and the rail reads as one control over
/// bright content drawn under it. Over the edit-mode scrim it reads as 99% black against the
/// scrim's 90%.
const TOOLBAR_PLATE_FILL: &str = "#000000e6";
/// The colour of whatever a block draws on itself: an icon, or the size readout's number.
const TOOLBAR_FOREGROUND: &str = "#ffffffee";
const TOOLBAR_LABEL_TEXT: &str = "#ebebf59e";
const TOOLBAR_SWATCH_EDGE: &str = "#ffffff4d";
/// The wash a hovered button of a board's row takes. White rather than the accent: on the row of the
/// board this screen shows the capsule is accent-filled, and an accent ring there is invisible.
const TOOLBAR_HOVER_FILL: &str = "#ffffff3d";
const TOOLBAR_CARD_EDGE: &str = "#ffffff59";
const TOOLBAR_CARD_SHADOW: &str = "#00000073";
const TOOLBAR_SHADOW_REACH: f32 = 2.0;
/// How far inside its cell a hovered button's outline sits, so it never touches the row's edge.
const TOOLBAR_HOVER_INSET: f32 = 2.0;
const UNDERLINE_HEIGHT: f32 = 1.5;
const CURSOR_WIDTH: f32 = 2.0;
const LINE_HEIGHT_SCALE: f32 = 1.2;

/// What a block shows on its face. Exactly one of these is set: an icon of what the control does, a
/// colour, or a value. A colour chip and the size readout are not functions, so they carry no icon.
#[derive(Debug, Clone, PartialEq)]
enum Mark {
    Icon(Icon),
    /// Palette index of a colour chip.
    Swatch(usize),
    /// A value as a bare number.
    Value(String),
    /// Nothing on its face: a row of names has nothing to show but the name, and a repeated glyph beside
    /// every one of them is an icon the reader has to identify before they can read the list.
    None,
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
    Cross,
    Pencil,
    Lock,
    Hand,
    Note,
    More,
    Scrim,
    /// The boards panel's block, and a board in its list.
    Board,
    /// `new board`.
    Plus,
    /// Every screen sharing one board.
    Shared,
    /// The link row: two chain links.
    Link,
    /// A clipboard entry.
    Clipboard,
    /// Undo and redo arrows for the history cards.
    Undo,
    Redo,
    /// No backdrop: an empty square.
    Clear,
    /// A square grid.
    Grid,
    /// A dot matrix.
    Dots,
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
    /// The control a press applies. Every block has one, the width readout included: its action opens
    /// the list of steps rather than stepping.
    action: Option<ToolbarAction>,
    /// The armed tool's block, or the active colour's swatch: drawn with the accent.
    active: bool,
    /// Gap under the block, in column coordinates.
    gap: f32,
}

/// The key a control applies, as it is written in a stretched block's text.
fn key_of(action: ToolbarAction) -> Option<&'static str> {
    Some(match action {
        ToolbarAction::Tool(tool) => tool.key(),
        ToolbarAction::More => "M",
        ToolbarAction::Scrim => "B",
        // The panel's own rows are picks out of the list the block opens, so they name that block's key.
        ToolbarAction::Boards
        | ToolbarAction::Board(_)
        | ToolbarAction::NewBoard
        | ToolbarAction::AskDeleteBoard(_)
        | ToolbarAction::DeleteBoard(_)
        | ToolbarAction::RenameBoard(_) => "C",
        ToolbarAction::CancelDeleteBoard => return None,
        ToolbarAction::Shared => "F",
        // No single letter: `Ctrl+Shift+V` opens the history, and the row says so in the drawer.
        ToolbarAction::Clipboard => "Ctrl+Shift+V",
        ToolbarAction::Link => return None,
        ToolbarAction::Clip(_) => return None,
        ToolbarAction::Backdrop(_) => return None,
        ToolbarAction::Color(index) => ["1", "2", "3", "4", "5"].get(index).copied().unwrap_or("?"),
        ToolbarAction::SizeStep(dir) => {
            if dir < 0.0 {
                "["
            } else {
                "]"
            }
        }
        ToolbarAction::Lock => "Esc",
        // No block shows these yet; if one does, its text names the key that does the same thing.
        ToolbarAction::Undo => "Ctrl+Z",
        ToolbarAction::Redo => "Ctrl+Shift+Z",
        // The width readout has no key: hovering it is the way in, and the steps it opens carry `[` and `]`.
        ToolbarAction::Size => return None,
    })
}

/// What a block says when the pointer is on it, ending with the key that applies the same thing.
fn labelled(action: ToolbarAction, what: &str) -> String {
    match key_of(action) {
        Some(key) => format!("{what} ({key})"),
        None => what.to_string(),
    }
}

/// The key a block shows on its face: its **primary** shortcut, and only when that shortcut is a
/// single key. A board row names the key that opened its list rather than a key of its own, and a
/// chord such as `Ctrl+Shift+V` or an alternate binding is no face mark — the stretched capsule
/// still states it in full, which is where every key, primary or alternate, is taught.
fn badge_of(action: ToolbarAction) -> Option<&'static str> {
    if matches!(
        action,
        ToolbarAction::Board(_)
            | ToolbarAction::NewBoard
            | ToolbarAction::AskDeleteBoard(_)
            | ToolbarAction::DeleteBoard(_)
            | ToolbarAction::RenameBoard(_)
    ) {
        return None;
    }
    let key = key_of(action)?;
    (!key.contains('+')).then_some(key)
}

/// Where a block's text starts, in logical screen coordinates, given the capsule it sits in. Text
/// always reads left to right: it starts after the block's own square, and on a right-docked rail,
/// where the square is the capsule's right end, it ends before it instead.
fn label_origin(
    position: crate::config::ToolbarPosition,
    capsule: Rect,
    bare: bool,
    text_width: f32,
) -> f32 {
    let pad = if bare {
        TOOLBAR_LABEL_PAD
    } else {
        TOOLBAR_BLOCK + TOOLBAR_LABEL_GAP
    };
    if position == crate::config::ToolbarPosition::Right {
        capsule.x + capsule.w - pad - text_width
    } else {
        capsule.x + pad
    }
}

/// A three-quarter-block rule between the rail's families: colour chips, the tools, and `more tools`.
const RAIL_SEPARATOR: f32 = TOOLBAR_BLOCK * 0.75;

fn separates(previous: Option<ToolbarAction>, current: Option<ToolbarAction>) -> bool {
    matches!(
        (previous, current),
        (Some(ToolbarAction::Color(_)), Some(action)) if !matches!(action, ToolbarAction::Color(_))
    ) || matches!(current, Some(ToolbarAction::More))
}

fn draw_rail_separators(
    pixmap: &mut PixmapMut,
    pill: &ToolbarPill,
    origin: (f32, f32),
    position: ToolbarPosition,
    scale: f32,
) {
    let mut previous: Option<Option<ToolbarAction>> = None;
    for control in pill.controls.iter().filter(|c| c.block.column == 0) {
        if let Some(previous_action) = previous {
            let boundary = separates(previous_action, control.block.action);
            if boundary {
                // Measure from the following button, so the colour rule and the `more tools` rule
                // keep the same clearance even if their surrounding gaps differ.
                let along = control.along - TOOLBAR_GROUP_GAP / 2.0;
                let across = TOOLBAR_BLOCK / 2.0;
                let (x, y) = position.point(origin, along, across);
                let (x, y) = (x * scale, y * scale);
                let c = parse_hex(TOOLBAR_CARD_EDGE);
                let color = Color::rgba(c[0], c[1], c[2], c[3]);
                let thin = scale.max(1.0);
                if position.horizontal() {
                    fill(
                        pixmap,
                        x - thin / 2.0,
                        y - RAIL_SEPARATOR * scale / 2.0,
                        thin,
                        RAIL_SEPARATOR * scale,
                        color,
                    );
                } else {
                    fill(
                        pixmap,
                        x - RAIL_SEPARATOR * scale / 2.0,
                        y - thin / 2.0,
                        RAIL_SEPARATOR * scale,
                        thin,
                        color,
                    );
                }
            }
        }
        previous = Some(control.block.action);
    }
}

/// The controls that fill the rail from the top and fold into the drawer when there is no room for them: the
/// palette, the tools and the size group, in their own order.
fn rail_candidates(toolbar: &Toolbar) -> Vec<Block> {
    let colors = &config::current().colors;
    let mut blocks: Vec<Block> = colors
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
            active: color == &toolbar.color,
            gap: TOOLBAR_GAP,
        })
        .collect();
    blocks[colors.len() - 1].gap = TOOLBAR_GROUP_GAP;
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
        mark: Mark::Value(format!("{:.0}", toolbar.size)),
        column: 0,
        // A control with no label: its value is its face, and a click opens the steps' own list.
        label: String::new(),
        action: Some(ToolbarAction::Size),
        active: false,
        gap: TOOLBAR_GROUP_GAP,
    });
    blocks
}

/// The foot of the rail: the way into the panels and the way out of edit mode. It is always on the rail
/// whatever the output's height, because a short output must still be able to reach its boards and leave.
fn foot_blocks() -> Vec<Block> {
    vec![
        // Boards are their own group: what is drawn on the wallpaper is one thing, which document it belongs
        // to another, so the block that lists them sits apart from the tools rather than among them. No state
        // mark while its panel is open, for the same reason `more` takes none: the panel is that state, and a
        // second signal would put two accented blocks on the screen at once.
        Block {
            mark: Mark::Icon(Icon::Board),
            column: 0,
            label: labelled(ToolbarAction::Boards, "boards"),
            action: Some(ToolbarAction::Boards),
            active: false,
            gap: TOOLBAR_GROUP_GAP,
        },
        // The block that opens the drawer sits at the foot of the rail and before `Esc`, so the rail's order
        // and its "Esc is last" property are unchanged. No state mark while the panel is open: the accent
        // means the item that is on in the list it belongs to — the armed tool, the active colour, and inside
        // the panel the control that is on — while a block that has opened a list is not armed at all.
        Block {
            mark: Mark::Icon(Icon::More),
            column: 0,
            label: labelled(ToolbarAction::More, "more tools"),
            action: Some(ToolbarAction::More),
            active: false,
            gap: TOOLBAR_GAP,
        },
        Block {
            mark: Mark::Icon(Icon::Lock),
            column: 0,
            label: labelled(ToolbarAction::Lock, "cancel, then lock"),
            action: Some(ToolbarAction::Lock),
            active: false,
            gap: 0.0,
        },
    ]
}

/// The height of a sequence of blocks: the pads, every block, and the gap under it — the last block's gap is
/// the column's own bottom padding, so it is not counted twice.
fn column_of(blocks: &[Block]) -> f32 {
    2.0 * TOOLBAR_PAD
        + blocks
            .iter()
            .map(|block| TOOLBAR_BLOCK + block.gap)
            .sum::<f32>()
}

/// The blocks that fit on an output of this height, and the ones that do not. The foot always stays; the
/// candidates fill from the top while they fit, and a block that does not fit is a row of the drawer instead —
/// the rail has no scrolling and no way to reach what hangs past the edge of the output.
fn fitted_blocks(toolbar: &Toolbar, surface: f32) -> (Vec<Block>, Vec<Block>) {
    let room = (surface - 2.0 * TOOLBAR_MARGIN).max(0.0);
    let foot = foot_blocks();
    let foot_h: f32 = foot
        .iter()
        .map(|block| TOOLBAR_BLOCK + block.gap)
        .sum::<f32>();
    let mut rail: Vec<Block> = Vec::new();
    let mut overflow: Vec<Block> = Vec::new();
    let mut used = 2.0 * TOOLBAR_PAD + foot_h;
    for block in rail_candidates(toolbar) {
        let next = used + TOOLBAR_BLOCK + block.gap;
        if overflow.is_empty() && next <= room {
            used = next;
            rail.push(block);
        } else {
            overflow.push(block);
        }
    }
    rail.extend(foot);
    (rail, overflow)
}

/// How many rows of a panel fit an output. A row past the edge can be neither clicked nor repainted, and a
/// short output folds the rail's overflow into a drawer that is easily taller than the rail itself, so the
/// panel keeps the rows nearest its block and the rest stay on their keys.
fn panel_room(surface: f32) -> usize {
    let usable = (surface - 2.0 * TOOLBAR_MARGIN - 2.0 * TOOLBAR_PAD).max(0.0);
    ((usable / (TOOLBAR_BLOCK + TOOLBAR_GAP)) as usize).max(1)
}

/// The row of the block that opened the panel, in the rail's own coordinates: walking the column is how the
/// panel is anchored, and both the frame and the box the rail declares have to walk it the same way.
fn owner_row(rail: &[Block], owner: ToolbarAction) -> Option<f32> {
    let mut y = TOOLBAR_PAD;
    for block in rail {
        if block.action == Some(owner) {
            return Some(y);
        }
        y += TOOLBAR_BLOCK + block.gap;
    }
    None
}

/// A rectangle in the rail's own coordinates: `along` runs down a left/right rail or across a
/// top/bottom one, and `across` runs from the rail's edge toward the interior of the output. Keeping
/// the layout in these terms is what lets one set of block positions serve all four edges.
#[derive(Debug, Clone, Copy)]
struct RailRect {
    along: f32,
    across: f32,
    along_extent: f32,
    across_extent: f32,
}

impl ToolbarPosition {
    /// The output dimension the rail is laid out along: the height of a side rail, the width of one
    /// at the top or bottom.
    fn along_surface(self, surface: (f32, f32)) -> f32 {
        if self.horizontal() {
            surface.0
        } else {
            surface.1
        }
    }

    /// The output dimension across the rail: the width of a side rail, the height of one at the
    /// top or bottom. Panels stack their rows across the rail, so this is the room they have.
    fn across_surface(self, surface: (f32, f32)) -> f32 {
        if self.horizontal() {
            surface.1
        } else {
            surface.0
        }
    }

    /// Where the rail's local origin — the start of its blocks, at the inner face of its plate —
    /// sits on the output. The rail's centre is at the optical centre of the edge it is docked to.
    fn origin(self, surface: (f32, f32), column: f32) -> (f32, f32) {
        // The plate reaches one pad outside the blocks, so keeping the plate's outer edge at
        // TOOLBAR_MARGIN means moving the origin in by that pad. The rail then floats instead of
        // sitting flush against the output.
        let edge = TOOLBAR_MARGIN + TOOLBAR_PAD;
        if self.horizontal() {
            (
                (surface.0 * TOOLBAR_CENTRE - column / 2.0).max(edge),
                match self {
                    ToolbarPosition::Top => edge,
                    _ => surface.1 - edge,
                },
            )
        } else {
            (
                match self {
                    ToolbarPosition::Left => edge,
                    _ => surface.0 - edge,
                },
                (surface.1 * TOOLBAR_CENTRE - column / 2.0).max(edge),
            )
        }
    }

    /// A local rectangle on the output. The across axis always points inward, so the same layout
    /// opens toward the interior on every edge.
    fn place(self, origin: (f32, f32), r: RailRect) -> Rect {
        match self {
            ToolbarPosition::Left => Rect {
                x: origin.0 + r.across,
                y: origin.1 + r.along,
                w: r.across_extent,
                h: r.along_extent,
            },
            ToolbarPosition::Right => Rect {
                x: origin.0 - r.across - r.across_extent,
                y: origin.1 + r.along,
                w: r.across_extent,
                h: r.along_extent,
            },
            ToolbarPosition::Top => Rect {
                x: origin.0 + r.along,
                y: origin.1 + r.across,
                w: r.along_extent,
                h: r.across_extent,
            },
            ToolbarPosition::Bottom => Rect {
                x: origin.0 + r.along,
                y: origin.1 - r.across - r.across_extent,
                w: r.along_extent,
                h: r.across_extent,
            },
        }
    }

    /// A local point on the output.
    fn point(self, origin: (f32, f32), along: f32, across: f32) -> (f32, f32) {
        match self {
            ToolbarPosition::Left => (origin.0 + across, origin.1 + along),
            ToolbarPosition::Right => (origin.0 - across, origin.1 + along),
            ToolbarPosition::Top => (origin.0 + along, origin.1 + across),
            ToolbarPosition::Bottom => (origin.0 + along, origin.1 - across),
        }
    }

    /// A pointer position in the rail's local coordinates.
    fn to_local(self, origin: (f32, f32), x: f32, y: f32) -> (f32, f32) {
        match self {
            ToolbarPosition::Left => (y - origin.1, x - origin.0),
            ToolbarPosition::Right => (y - origin.1, origin.0 - x),
            ToolbarPosition::Top => (x - origin.0, y - origin.1),
            ToolbarPosition::Bottom => (x - origin.0, origin.1 - y),
        }
    }
}

/// The rail and its open panel on one output, decided once: the fitted column, the panel's own rows and plate,
/// and the extent the two reach together — what the frame paints, what the hit test accepts and what a frame
/// that repaints only the rail damages. Pure: no text is measured here, so a damage box can be worked out
/// without drawing, and the frame and the box cannot disagree about how much there is.
struct RailLayout {
    /// The rail's own blocks: the candidates that fit, then the foot.
    rail: Vec<Block>,
    /// The controls the rail had no room for, in the rail's order.
    overflow: Vec<Block>,
    /// Length of the rail's own column, along the rail's axis.
    column: f32,
    /// The open panel's owner, its rows and its box, when one is open.
    panel: Option<(ToolbarAction, Vec<Block>, RailRect)>,
    /// How far the rail and its panel reach along the rail's axis.
    reach: f32,
}

fn rail_layout(toolbar: &Toolbar, surface: (f32, f32), position: ToolbarPosition) -> RailLayout {
    let along_surface = position.along_surface(surface);
    let (rail, overflow) = fitted_blocks(toolbar, along_surface);
    let column = column_of(&rail);
    let panel_surface = if position.horizontal() {
        position.across_surface(surface)
    } else {
        along_surface
    };
    let Some((owner, rows)) = open_panel(toolbar, &overflow, panel_room(panel_surface)) else {
        return RailLayout {
            rail,
            overflow,
            column,
            panel: None,
            reach: column,
        };
    };
    let rows_h = rows
        .iter()
        .map(|block| TOOLBAR_BLOCK + block.gap)
        .sum::<f32>()
        - rows.first().map_or(0.0, |block| block.gap);
    let owner_along = owner_row(&rail, owner).unwrap_or(TOOLBAR_PAD);
    // One row is level with the block that opened the panel and the rest grow away from it: down the
    // column while they fit, up when they do not. A top or bottom rail drops its menu from the block
    // itself, with each row ending under it, so the walk from the block into the list is direct.
    let along = if position.horizontal() || owner_along + rows_h <= column - TOOLBAR_PAD {
        owner_along
    } else {
        (owner_along + TOOLBAR_BLOCK - rows_h).max(TOOLBAR_PAD)
    };
    let plate = if position.horizontal() {
        // A top or bottom rail's panel drops down (or rises, from the bottom edge) as a menu: its
        // rows stack across the rail, each right-aligned under the block that opened it and with the
        // room a stretched block may take along the rail. The strip is kept inside the output.
        let origin = position.origin(surface, column);
        let room = (along_surface - TOOLBAR_MARGIN - origin.0).max(0.0);
        let width = room.min(TOOLBAR_RESERVE);
        let start = (along + TOOLBAR_BLOCK - width)
            .max(0.0)
            .min((room - width).max(0.0));
        RailRect {
            along: start,
            across: TOOLBAR_DRAWER_X - TOOLBAR_PAD,
            along_extent: width,
            across_extent: rows_h + 2.0 * TOOLBAR_PAD,
        }
    } else {
        RailRect {
            along: along - TOOLBAR_PAD,
            across: TOOLBAR_DRAWER_X - TOOLBAR_PAD,
            along_extent: rows_h + 2.0 * TOOLBAR_PAD,
            across_extent: TOOLBAR_RESERVE - TOOLBAR_DRAWER_X + TOOLBAR_PAD,
        }
    };
    let reach = (plate.along + plate.along_extent).max(column);
    // A top or bottom rail's capsule grows along the rail, past its own last block; the hit test's
    // early bound has to reach as far as a capsule may.
    let reach = if position.horizontal() {
        reach.max(column + TOOLBAR_RESERVE)
    } else {
        reach
    };
    RailLayout {
        rail,
        overflow,
        column,
        panel: Some((owner, rows, plate)),
        reach,
    }
}

/// The rail's column on an output of this length: what the fitted blocks need. The plate, the hit
/// boxes, the panels anchored to the foot and the box a rail-only frame damages are all placed from this
/// one number. `surface` is the dimension the rail runs along: the output's height for a side rail,
/// its width for one at the top or bottom.
pub fn rail_column(toolbar: &Toolbar, surface: f32) -> f32 {
    let position = config::current().toolbar;
    let output = if position.horizontal() {
        (surface, 0.0)
    } else {
        (0.0, surface)
    };
    rail_layout(toolbar, output, position).column
}

/// The rail's blocks and its open panel's rows, in the order they are laid out: the list the tests walk, and the
/// shape the layout gives a frame.
#[cfg(test)]
fn toolbar_blocks(toolbar: &Toolbar, surface: f32) -> Vec<Block> {
    let position = config::current().toolbar;
    let output = if position.horizontal() {
        (surface, 0.0)
    } else {
        (0.0, surface)
    };
    let layout = rail_layout(toolbar, output, position);
    let mut blocks = layout.rail;
    if let Some((_, rows, _)) = layout.panel {
        blocks.extend(rows);
    }
    blocks
}

/// The controls the drawer holds: the rail has no room for them, so they live in the list the `more`
/// block opens. This is the one list of who they are — `drawer_blocks` is what they look like — so the
/// rules that open and close the drawer cannot disagree with what it shows.
pub const DRAWER: [ToolbarAction; 4] = [
    ToolbarAction::Scrim,
    ToolbarAction::Shared,
    ToolbarAction::Link,
    ToolbarAction::Clipboard,
];

/// How many boards the panel lists before the rest are only reachable from the command line. The rail
/// declares a fixed box for damage accounting and a frame that repaints only the rail has to repaint the
/// whole panel inside it, so the panel cannot grow with the document.
pub const MAX_BOARD_ROWS: usize = 8;

/// The panel that is open, and the block that opened it: one panel at a time, so one owner and one list.
fn open_panel(
    toolbar: &Toolbar,
    overflow: &[Block],
    room: usize,
) -> Option<(ToolbarAction, Vec<Block>)> {
    let rows = if toolbar.boards_open {
        Some((ToolbarAction::Boards, board_rows(toolbar)))
    } else if toolbar.size_open {
        Some((ToolbarAction::Size, size_rows(toolbar)))
    } else if toolbar.clipboard_open {
        Some((ToolbarAction::More, clip_rows(toolbar)))
    } else if toolbar.more {
        Some((ToolbarAction::More, drawer_blocks(toolbar, overflow)))
    } else {
        None
    };
    rows.map(|(owner, mut rows)| {
        // The rows nearest the block are the ones worth keeping: the row level with it is the last when the
        // panel grows up and the first when it grows down, so the tail is what goes.
        rows.truncate(room.max(1));
        (owner, rows)
    })
}

/// Whether `action` belongs to a panel: a pick out of a list. What a pick *closes* is
/// [`closes_panel_on_pick`]'s decision — a width step is the one pick that leaves its list open.
pub fn picks_from_panel(action: ToolbarAction) -> bool {
    DRAWER.contains(&action)
        || matches!(
            action,
            ToolbarAction::Board(_)
                | ToolbarAction::NewBoard
                | ToolbarAction::AskDeleteBoard(_)
                | ToolbarAction::DeleteBoard(_)
                | ToolbarAction::CancelDeleteBoard
                | ToolbarAction::RenameBoard(_)
                | ToolbarAction::SizeStep(_)
                | ToolbarAction::Clip(_)
                | ToolbarAction::Backdrop(_)
        )
}

/// Whether the open panel stays open with the pointer on `hover`: on the block that opened it, or on one
/// of its own rows. The drawer is a menu the pointer opens by walking into it, so anywhere else — including
/// off the rail entirely — closes it. The boards and width lists are opened by a click or a hover and
/// dismissed by their own rules — a pick that closes them, a press elsewhere, `Esc` — so no hover state
/// closes them.
pub fn panel_stays_open(hover: Option<ToolbarAction>, owner: Option<ToolbarAction>) -> bool {
    matches!(
        owner,
        Some(
            ToolbarAction::Boards
                | ToolbarAction::Size
                | ToolbarAction::Clipboard
                | ToolbarAction::More
        )
    ) || match owner {
        Some(owner) => hover == Some(owner) || hover.is_some_and(picks_from_panel),
        None => false,
    }
}

/// Whether a pick out of a panel closes it: every pick does, except a width step, which is a command you
/// may want to repeat. `picks_from_panel` still says what belongs to a panel, for the walkway and for a
/// press on a row that must not be read as a press outside it.
pub fn closes_panel_on_pick(action: ToolbarAction) -> bool {
    // A width step and a rename work on what the panel holds rather than choosing from it, so they
    // leave it open. The delete question and its `no` do too: both replace or restore the row itself.
    picks_from_panel(action)
        && !matches!(
            action,
            ToolbarAction::SizeStep(_)
                | ToolbarAction::AskDeleteBoard(_)
                | ToolbarAction::CancelDeleteBoard
                | ToolbarAction::RenameBoard(_)
                | ToolbarAction::Link
                | ToolbarAction::Clipboard
                | ToolbarAction::Scrim
                | ToolbarAction::Backdrop(_)
        )
}

/// Whether a press on a rail control dismisses the open panel: a press on the panel's own block or on one
/// of its rows belongs to the panel, and anything else is a press outside it, which a menu closes on.
pub fn press_dismisses_panel(hit: Option<ToolbarAction>, owner: Option<ToolbarAction>) -> bool {
    match (hit, owner) {
        (Some(hit), Some(owner)) => hit != owner && !picks_from_panel(hit),
        _ => false,
    }
}

/// The drawer's residents: the controls the rail has no room for. Their own height is what the drawer's
/// column is, and this one list is what the drawer shows, what the `more` block says is in there, and what
/// the rules that open and close it are built from.
fn drawer_blocks(toolbar: &Toolbar, overflow: &[Block]) -> Vec<Block> {
    let mut rows: Vec<Block> = DRAWER
        .iter()
        .filter_map(|action| match action {
            ToolbarAction::Scrim => Some(Block {
                mark: Mark::Icon(Icon::Scrim),
                column: 1,
                // Open, the row is the anchor of the four mode buttons beside it; its name would
                // cover them, and the active option already says which mode is on.
                label: if toolbar.more {
                    String::new()
                } else {
                    labelled(*action, &format!("backdrop: {}", toolbar.backdrop.name()))
                },
                action: Some(*action),
                active: false,
                gap: TOOLBAR_GAP,
            }),
            ToolbarAction::Shared => Some(Block {
                mark: Mark::Icon(Icon::Shared),
                column: 1,
                label: labelled(
                    *action,
                    if toolbar.shared {
                        "boards: every screen"
                    } else {
                        "boards: this screen"
                    },
                ),
                action: Some(*action),
                active: toolbar.shared,
                gap: TOOLBAR_GAP,
            }),
            ToolbarAction::Link => Some(Block {
                mark: Mark::Icon(Icon::Link),
                column: 1,
                label: labelled(*action, "link text to board"),
                action: Some(*action),
                active: false,
                gap: TOOLBAR_GAP,
            }),
            ToolbarAction::Clipboard => Some(Block {
                mark: Mark::Icon(Icon::Clipboard),
                column: 1,
                label: labelled(*action, "clipboard history"),
                action: Some(*action),
                active: false,
                gap: TOOLBAR_GAP,
            }),
            _ => None,
        })
        .collect();
    // Then the controls the rail had no room for, in the rail's own order: they are ordinary blocks, so they
    // draw, stretch, are hit and state their keys exactly like the rest of the drawer.
    rows.extend(overflow.iter().cloned().map(|mut block| {
        block.column = 1;
        block
    }));
    rows
}

/// What a row draws where its name goes: the buffer while it is being edited.
fn row_label<'a>(toolbar: &'a Toolbar, row: &'a Block) -> &'a str {
    if let (Some(edit), Some(ToolbarAction::Board(index))) = (&toolbar.rename, row.action)
        && edit.index == index
    {
        return &edit.text;
    }
    &row.label
}

/// Whether the row at `index` is currently asking whether to delete its board.
fn board_confirms(toolbar: &Toolbar, index: usize) -> bool {
    toolbar
        .boards
        .get(index)
        .is_some_and(|board| toolbar.delete_confirm.as_deref() == Some(board.name.as_str()))
}

/// Whether a board's row shows its buttons, as `(delete, rename)`: the pointer is on the row or on one
/// of them, the document has a board to delete, and the row is not the one being renamed. A row asking
/// about a deletion always shows both slots, which hold `no` and `yes` instead of the icons.
fn board_actions(toolbar: &Toolbar, index: usize) -> Option<(bool, bool)> {
    if board_confirms(toolbar, index) {
        return Some((true, true));
    }
    if toolbar
        .rename
        .as_ref()
        .is_some_and(|edit| edit.index == index)
    {
        return None;
    }
    let on_it = matches!(
        toolbar.hover,
        Some(
            ToolbarAction::Board(i)
                | ToolbarAction::AskDeleteBoard(i)
                | ToolbarAction::DeleteBoard(i)
                | ToolbarAction::RenameBoard(i)
        ) if i == index
    );
    on_it.then_some((toolbar.boards.len() > 1, true))
}

/// The boards panel's rows: one per board stating its name, the board this output shows accent-filled,
/// then `new board` and `delete <name>`. A board row says its name and no key: every one of them is
/// picked from the list the `boards` block opened, so a key on the row would repeat the block's own.
fn board_rows(toolbar: &Toolbar) -> Vec<Block> {
    let mut rows: Vec<Block> = toolbar
        .boards
        .iter()
        .take(MAX_BOARD_ROWS)
        .enumerate()
        .map(|(index, board)| Block {
            mark: Mark::None,
            column: 1,
            label: if board_confirms(toolbar, index) {
                format!("delete {}?", board.name)
            } else if toolbar.target.is_some() {
                format!("→ {}", board.name)
            } else {
                board.name.clone()
            },
            action: Some(ToolbarAction::Board(index)),
            active: board.current,
            gap: TOOLBAR_GAP,
        })
        .collect();
    // `new board` is the last row: a panel with more rows than the column holds grows upwards, which puts
    // the last row level with the block, where the corridor leads the pointer into the list.
    rows.push(Block {
        mark: Mark::Icon(Icon::Plus),
        column: 1,
        label: "new board".into(),
        action: Some(ToolbarAction::NewBoard),
        active: false,
        gap: TOOLBAR_GAP,
    });
    rows
}

/// The width list's rows: the two steps, smaller first, so the panel reads in the order the rail did.
fn size_rows(toolbar: &Toolbar) -> Vec<Block> {
    let axis = if toolbar.tool == Tool::Text {
        "size"
    } else {
        "width"
    };
    [(-1.0, "smaller"), (1.0, "larger")]
        .into_iter()
        .map(|(dir, word)| Block {
            mark: Mark::Icon(if dir < 0.0 {
                Icon::Smaller
            } else {
                Icon::Larger
            }),
            column: 1,
            label: labelled(ToolbarAction::SizeStep(dir), &format!("{word} {axis}")),
            action: Some(ToolbarAction::SizeStep(dir)),
            active: false,
            gap: TOOLBAR_GAP,
        })
        .collect()
}

/// The clipboard history's rows, newest first. Every row is a pick: a press pastes that entry and
/// closes the panel.
fn clip_rows(toolbar: &Toolbar) -> Vec<Block> {
    toolbar
        .clips
        .iter()
        .enumerate()
        .map(|(index, clip)| {
            let label = if clip.preview.is_empty() {
                format!("{}: {} items", index + 1, clip.items)
            } else {
                format!("{}: {} items · {}", index + 1, clip.items, clip.preview)
            };
            Block {
                mark: Mark::Icon(Icon::Clipboard),
                column: 1,
                label,
                action: Some(ToolbarAction::Clip(index)),
                active: false,
                gap: TOOLBAR_GAP,
            }
        })
        .collect()
}

/// What the `more` block reports while the pointer is on it: what is in the drawer, or `None` while the
/// drawer is open, because then its controls are on screen and each is named by its own block.
fn drawer_legend(toolbar: &Toolbar, overflow: &[Block]) -> Option<String> {
    (!toolbar.more).then(|| {
        drawer_blocks(toolbar, overflow)
            .iter()
            .map(|block| {
                block
                    .action
                    .and_then(drawer_caption)
                    .map(str::to_string)
                    .unwrap_or_else(|| block.label.clone())
            })
            .collect::<Vec<_>>()
            .join(" · ")
    })
}

/// The one-word name the `more` block's legend gives a drawer control. The rows themselves state
/// the full label and key; the legend only has the one line under the capsule and has to fit it.
fn drawer_caption(action: ToolbarAction) -> Option<&'static str> {
    Some(match action {
        ToolbarAction::Scrim => "backdrop",
        ToolbarAction::Shared => "boards",
        ToolbarAction::Link => "link",
        ToolbarAction::Clipboard => "clipboard",
        _ => return None,
    })
}

/// Which side a growing capsule uncovers its label from. A side rail's capsule opens away from
/// its edge, and so does the text; on a right-docked rail both open leftwards, and the glyphs
/// have to disappear past the capsule's left edge rather than its right.
#[derive(Debug, Clone, Copy)]
enum Reveal {
    /// Show glyphs left of the edge: the capsule opens to the right.
    After(f32),
    /// Show glyphs right of the edge: the capsule opens to the left.
    Before(f32),
}

pub struct Renderer {
    font_system: FontSystem,
    cache: SwashCache,
    toolbar_cache: Vec<CachedToolbar>,
}

/// Clear `pixmap` and paint the chosen backdrop. `scale` is the output's device scale and `view`
/// is the canvas view, so the grid and dots pan and zoom with the drawing: they are the canvas's
/// paper, not a fixed screen texture.
fn paint_backdrop(pixmap: &mut PixmapMut, mode: Backdrop, scale: f32, view: View) {
    pixmap.fill(SkColor::TRANSPARENT);
    match mode {
        Backdrop::Transparent => {}
        Backdrop::Black => {
            let c = parse_hex(SCRIM_FILL);
            pixmap.fill(SkColor::from_rgba8(c[0], c[1], c[2], c[3]));
        }
        Backdrop::Grid => {
            let c = parse_hex(BACKDROP_GRID_COLOR);
            let color = Color::rgba(c[0], c[1], c[2], c[3]);
            let (w, h) = (pixmap.width() as f32, pixmap.height() as f32);
            let spacing = (BACKDROP_SPACING * view.zoom * scale).max(1.0);
            let line = (BACKDROP_LINE * scale).max(1.0);
            let phase = |offset: f32| (offset * scale).rem_euclid(spacing);
            let mut x = phase(view.offset[0]);
            while x < w {
                fill(pixmap, x, 0.0, line, h, color);
                x += spacing;
            }
            let mut y = phase(view.offset[1]);
            while y < h {
                fill(pixmap, 0.0, y, w, line, color);
                y += spacing;
            }
        }
        Backdrop::Dots => {
            let c = parse_hex(BACKDROP_DOT_COLOR);
            let color = Color::rgba(c[0], c[1], c[2], c[3]);
            let (w, h) = (pixmap.width() as f32, pixmap.height() as f32);
            let spacing = (BACKDROP_SPACING * view.zoom * scale).max(1.0);
            let dot = (BACKDROP_DOT * scale).max(1.0);
            // A dot sits at the middle of each canvas cell, so its phase carries the half-cell.
            let phase = |offset: f32| (offset * scale + spacing / 2.0).rem_euclid(spacing);
            let mut y = phase(view.offset[1]);
            while y < h {
                let mut x = phase(view.offset[0]);
                while x < w {
                    fill(pixmap, x - dot / 2.0, y - dot / 2.0, dot, dot, color);
                    x += spacing;
                }
                y += spacing;
            }
        }
    }
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
    /// What a frame drawn at a coarser rung is rasterised into before it is blitted up. It is reused
    /// across a gesture rather than allocated per frame, and it is never the cached raster: a soft
    /// document is only ever produced by a frame that repaints the whole surface anyway.
    coarse: Option<Pixmap>,
}

/// The committed document of one output, rasterised into a pixmap that every frame fills from.
/// A frame therefore paints only the overlays, and its cost follows what changed rather than how
/// much is drawn. The raster is rebuilt by the first frame that needs a document-only base — a
/// localized frame — and a frame whose damage covers the surface does not maintain it: it draws the
/// document into the published buffer and leaves the raster absent. See [`TextLayoutCache`].
#[derive(Default)]
pub(crate) struct BaseLayer {
    pixmap: Option<Pixmap>,
    key: Option<BaseKey>,
}

#[derive(Debug, Clone, PartialEq)]
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
    backdrop: Backdrop,
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
    surface: (f32, f32),
    position: crate::config::ToolbarPosition,
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

    /// Whether a document raster valid for the last frame is held. A whole-surface frame leaves it
    /// absent — it draws the document into the published buffer instead — so this is what says
    /// whether the next localized frame has anything to fill from.
    pub fn base_valid(&self) -> bool {
        self.base.key.is_some()
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
        ann: &Board,
        overlay: &Overlay,
        damage: Option<Rect>,
    ) {
        let mut render_cache = FrameCache::default();
        self.render_with_cache(
            buf,
            width,
            height,
            scale,
            1.0,
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
        // The fraction of the surface's own pixels the document is rasterised at, `1.0` for full
        // resolution. It never reaches the cache key: a coarser frame only ever happens on the path
        // that would rebuild the raster anyway, and it leaves the raster absent.
        quality: f32,
        view: View,
        ann: &Board,
        overlay: &Overlay,
        damage: Option<Rect>,
        cache: &mut FrameCache,
        skip: &[ItemId],
    ) {
        let FrameCache { text, base, coarse } = cache;
        let screen = Screen { scale, view };
        text.sync(ann.texts.len(), screen.len(1.0));
        let region = match damage {
            None => DamageRegion::All,
            Some(d) => DamageRegion::from_logical(d, scale, width, height),
        };
        // What an edit-mode frame is painted on. The overlay's rail is `Some` exactly in edit mode —
        // `App::current_toolbar` decides that — and it carries the chosen backdrop.
        let backdrop = overlay
            .toolbar
            .as_ref()
            .map_or(Backdrop::Transparent, |toolbar| toolbar.backdrop);
        // The committed document comes from the raster. Only a frame whose raster cannot be
        // allocated draws it into the buffer itself.
        let key = BaseKey {
            width,
            height,
            scale,
            view,
            skip: skip.to_vec(),
            backdrop,
        };
        let mut draw_document = false;
        if base.matches(&key) {
            // A warm raster is one pass over the pixels whatever the damage is, so a whole-surface
            // frame copies it rather than drawing the document again.
            base.fill(buf, region, width, height);
        } else if matches!(region, DamageRegion::All) {
            // A whole-surface frame writes every pixel of the buffer anyway, so the document is drawn
            // straight into it and the raster is left absent: keeping the raster would be a third
            // whole-surface write per frame. The next frame that needs a document-only base — any
            // localized damage — rebuilds it then, for its own view.
            base.invalidate();
            draw_document = true;
        } else {
            let renderer = &mut *self;
            let built = base.rasterise(key, |target| {
                renderer.draw_document(target, ann, screen, text, skip, backdrop)
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
            if quality < 1.0 {
                self.draw_document_coarse(
                    &mut pixmap,
                    coarse,
                    ann,
                    screen,
                    text,
                    skip,
                    backdrop,
                    quality,
                );
            } else {
                self.draw_document(&mut pixmap, ann, screen, text, skip, backdrop);
            }
        }
        if let Some(s) = &overlay.stroke {
            draw_stroke(&mut pixmap, s, screen, Ink::Sketch(s.seed()));
        }
        if let Some(s) = &overlay.shape {
            draw_shape(&mut pixmap, s, screen, Ink::Sketch(s.seed()));
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
                            // The seed is the held element's, not the transformed copy's: a mark
                            // that re-rolled as the pointer moved would shimmer under the drag.
                            draw_stroke(
                                &mut pixmap,
                                &transform.stroke(s),
                                screen,
                                Ink::Sketch(s.seed()),
                            );
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
                            draw_shape(
                                &mut pixmap,
                                &transform.shape(s),
                                screen,
                                Ink::Sketch(s.seed()),
                            );
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
        for connector in &overlay.connectors {
            match connector {
                LiveConnector::Stroke(_, stroke) => {
                    draw_stroke(&mut pixmap, stroke, screen, Ink::Sketch(stroke.seed()));
                }
                LiveConnector::Shape(_, shape) => {
                    draw_shape(&mut pixmap, shape, screen, Ink::Sketch(shape.seed()));
                }
            }
        }
        draw_snap_hints(&mut pixmap, ann, overlay, screen);
        // The frame is drawn on the measured text, not on the damage estimate, so its trailing edge
        // sits on the last glyph instead of past it.
        let selection_boxes = overlay.selection.as_ref().map(|s| {
            (
                s.items.clone(),
                s.transform,
                self.text_boxes(ann, &s.items, screen),
            )
        });
        if let Some(frame) = selection_boxes
            .as_ref()
            .and_then(|(items, transform, boxes)| {
                selection_frame_with(ann, items, *transform, boxes)
            })
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
            let box_ = toolbar_bounds(toolbar, w, h);
            if damage.is_none_or(|d| box_.intersects(d)) {
                self.draw_toolbar(&mut pixmap, toolbar, scale);
            }
            let cards = cards_bounds((w, h), config::current().cards);
            if damage.is_none_or(|d| cards.intersects(d)) {
                self.draw_cards(&mut pixmap, toolbar, (w, h), scale, config::current().cards);
            }
        }
        // Interface on the canvas: only while something is held, so a press on it can be told from
        // a press on the drawing by the same rule that says the button is there.
        if overlay.selection.is_some() && overlay.toolbar.is_some() {
            let (w, h) = (
                pixmap.width() as f32 / scale,
                pixmap.height() as f32 / scale,
            );
            let box_ = move_button_bounds((w, h));
            round_rect(
                &mut pixmap,
                Rect {
                    x: box_.x * scale,
                    y: box_.y * scale,
                    w: box_.w * scale,
                    h: box_.h * scale,
                },
                (MOVE_BUTTON_H / 2.0) * scale,
                parse_hex(TOOLBAR_BLOCK_FILL),
                Some((parse_hex(TOOLBAR_BLOCK_EDGE), TOOLBAR_HAIRLINE * scale)),
            );
            let label = "move to board";
            let width = self.text_width(label, TOOLBAR_TEXT_SIZE);
            let item = toolbar_text(
                box_.x + (box_.w - width) / 2.0,
                box_.y,
                box_.h,
                TOOLBAR_TEXT_SIZE,
                TOOLBAR_LABEL_TEXT,
                label,
            );
            self.draw_text(
                &mut pixmap,
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
        if let Some(at) = overlay.eraser {
            // The marker is what the pointer aims with, so its size is a screen length even though
            // the point it sits on is the canvas point it will erase.
            let p = screen.point(at);
            draw_eraser_marker(&mut pixmap, p, screen.ui(1.0));
        }
    }

    /// Draw the committed document into `pixmap`: the raster build, and the fallback for a frame
    /// whose raster could not be allocated. The shaped-line cache is filled here, once per
    /// document change, so a frame that reuses the raster shapes no text at all. `backdrop` is the
    /// background the document is drawn on: the whole pixmap is cleared and the pattern painted
    /// first, so this is the one place a frame's background comes from.
    fn draw_document(
        &mut self,
        pixmap: &mut PixmapMut,
        ann: &Board,
        screen: Screen,
        text: &mut TextLayoutCache,
        skip: &[ItemId],
        backdrop: Backdrop,
    ) {
        paint_backdrop(pixmap, backdrop, screen.scale, screen.view);
        for (index, s) in ann.strokes.iter().enumerate() {
            if skip.contains(&ItemId::Stroke(index)) {
                continue;
            }
            draw_stroke(pixmap, s, screen, Ink::Sketch(s.seed()));
        }
        for (index, s) in ann.shapes.iter().enumerate() {
            if skip.contains(&ItemId::Shape(index)) {
                continue;
            }
            draw_shape(pixmap, s, screen, Ink::Sketch(s.seed()));
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

    /// Draw the document at a coarser rung and scale it up into `target`, which is what a moving
    /// canvas spends instead of a full-resolution document: every pixel is laid down once, over
    /// `quality` of the area.
    ///
    /// The scaling is a replication of whole pixels by an integer factor rather than a resample,
    /// because tiny-skia has no fast path for transforming an image at all: filling the surface with a
    /// scaled `Pattern`, or `draw_pixmap` with a scale transform, costs 70 to 215 ms at 3840x2160 on
    /// this machine, against 6 ms for copying the rows out by hand. A source pixel is therefore
    /// repeated `factor` times across and `factor` times down, which is coarse and never blurry, and
    /// the anti-aliasing the document already carries comes with it.
    #[allow(clippy::too_many_arguments)]
    fn draw_document_coarse(
        &mut self,
        target: &mut PixmapMut,
        coarse: &mut Option<Pixmap>,
        ann: &Board,
        screen: Screen,
        text: &mut TextLayoutCache,
        skip: &[ItemId],
        backdrop: Backdrop,
        quality: f32,
    ) {
        // The rung is a whole-pixel factor: any fraction is rounded, and a factor of one is not a
        // rung at all.
        let factor = (1.0 / quality.max(f32::EPSILON)).round().max(1.0) as u32;
        let (tw, th) = (target.width(), target.height());
        if factor < 2 {
            self.draw_document(target, ann, screen, text, skip, backdrop);
            return;
        }
        let (w, h) = (tw.div_ceil(factor), th.div_ceil(factor));
        if !coarse
            .as_ref()
            .is_some_and(|pm| pm.width() == w && pm.height() == h)
        {
            *coarse = Pixmap::new(w, h);
        }
        let Some(pixmap) = coarse.as_mut() else {
            // No pixmap to spare: the frame is drawn at full resolution rather than left stale.
            self.draw_document(target, ann, screen, text, skip, backdrop);
            return;
        };
        // The same document on a smaller surface: the *view* is what shrinks, once — a device pixel is
        // `screen.point`, which is the view's screen position times the output's scale, so dividing
        // both would draw the document at a quarter of its size rather than a half.
        let coarse_screen = Screen {
            scale: screen.scale,
            view: View {
                zoom: screen.view.zoom / factor as f32,
                offset: [
                    screen.view.offset[0] / factor as f32,
                    screen.view.offset[1] / factor as f32,
                ],
            },
        };
        self.draw_document(
            &mut pixmap.as_mut(),
            ann,
            coarse_screen,
            text,
            skip,
            backdrop,
        );

        let (cw, ch) = (pixmap.width() as usize, pixmap.height() as usize);
        let stride = tw as usize * 4;
        let factor = factor as usize;
        let mut row = vec![0u8; stride];
        let data = pixmap.data();
        let out = target.data_mut();
        for dy in 0..th as usize {
            let sy = (dy / factor).min(ch - 1);
            let source = &data[sy * cw * 4..(sy + 1) * cw * 4];
            // One source pixel at a time, written `factor` times across. The inner loop is a runtime
            // length, which is why the doubling it actually uses has its own arm: a bounded copy per
            // destination pixel measured four times as long as one store per source pixel.
            let pixels = source.as_chunks::<4>().0;
            if factor == 2 {
                for (chunk, pixel) in row.as_chunks_mut::<8>().0.iter_mut().zip(pixels) {
                    chunk[..4].copy_from_slice(pixel);
                    chunk[4..].copy_from_slice(pixel);
                }
                // An odd destination width leaves one pixel past the paired chunks.
                if tw as usize % 2 == 1
                    && let (Some(pixel), Some(last)) =
                        (pixels.last(), row.as_chunks_mut::<4>().0.last_mut())
                {
                    last.copy_from_slice(pixel);
                }
            } else {
                let mut dx = 0;
                for pixel in pixels {
                    for _ in 0..factor {
                        if dx == tw as usize {
                            break;
                        }
                        row[dx * 4..dx * 4 + 4].copy_from_slice(pixel);
                        dx += 1;
                    }
                }
            }
            out[dy * stride..(dy + 1) * stride].copy_from_slice(&row);
        }
    }

    /// The edit-mode toolbar. macOS idiom: a rail of translucent blocks, the armed tool's block
    /// and the active colour's swatch in the system accent. The block under the pointer stretches
    /// into a capsule that says what it does; every block is also a target, so the rectangles the
    /// press path asks for are laid out with the blocks drawn here.
    fn draw_toolbar(&mut self, pixmap: &mut PixmapMut, toolbar: &Toolbar, scale: f32) {
        let surface = (
            pixmap.width() as f32 / scale,
            pixmap.height() as f32 / scale,
        );
        let position = config::current().toolbar;
        let Some(pill) = self.cached_toolbar_pill(toolbar, surface, position) else {
            return;
        };
        let origin = position.origin(surface, pill.column);
        let local = |r: RailRect| position.place(origin, r);
        let device = |r: RailRect| {
            let r = local(r);
            Rect {
                x: r.x * scale,
                y: r.y * scale,
                w: r.w * scale,
                h: r.h * scale,
            }
        };
        // The plate the whole rail sits on: the block column, inset `TOOLBAR_PAD` from the blocks on
        // all four sides — which is exactly the column's own extent, since the column already carries
        // that padding at each end. Its width is the block column and not the room a stretched capsule
        // may take, so a capsule reaching past it is drawn over the wallpaper as its own block, rather
        // than the plate appearing to grow.
        let plate = device(RailRect {
            along: 0.0,
            across: -TOOLBAR_PAD,
            along_extent: pill.column,
            across_extent: TOOLBAR_BLOCK + 2.0 * TOOLBAR_PAD,
        });
        round_rect(
            pixmap,
            plate,
            TOOLBAR_RADIUS * scale,
            parse_hex(TOOLBAR_PLATE_FILL),
            Some((parse_hex(TOOLBAR_CARD_EDGE), TOOLBAR_HAIRLINE * scale)),
        );
        draw_card_shadow(pixmap, plate, TOOLBAR_RADIUS * scale, scale);
        // The line through the middle of the backdrop options: the four modes are one family, and
        // the line says so without a second plate. It is drawn behind the buttons.
        if toolbar.more
            && let Some(scrim) = pill
                .controls
                .iter()
                .find(|control| control.block.action == Some(ToolbarAction::Scrim))
                .map(|control| (control.along, control.across))
        {
            let c = parse_hex(TOOLBAR_BLOCK_EDGE);
            let last = 2.0 + (Backdrop::ALL.len() - 1) as f32;
            let (start, end) = if position.horizontal() {
                (
                    position.point(
                        origin,
                        scrim.0 + TOOLBAR_BLOCK,
                        scrim.1 + TOOLBAR_BLOCK / 2.0,
                    ),
                    position.point(
                        origin,
                        scrim.0
                            + (Backdrop::ALL.len() as f32) * TOOLBAR_DRAWER_X
                            + TOOLBAR_BLOCK / 2.0,
                        scrim.1 + TOOLBAR_BLOCK / 2.0,
                    ),
                )
            } else {
                (
                    position.point(
                        origin,
                        scrim.0 + TOOLBAR_BLOCK / 2.0,
                        TOOLBAR_DRAWER_X + TOOLBAR_BLOCK,
                    ),
                    position.point(
                        origin,
                        scrim.0 + TOOLBAR_BLOCK / 2.0,
                        last * TOOLBAR_DRAWER_X + TOOLBAR_BLOCK / 2.0,
                    ),
                )
            };
            let (x0, y0) = (start.0 * scale, start.1 * scale);
            let (x1, y1) = (end.0 * scale, end.1 * scale);
            let (dx, dy) = ((x1 - x0).abs(), (y1 - y0).abs());
            let thin = scale.max(1.0);
            fill(
                pixmap,
                x0.min(x1) - if dx < 0.01 { thin / 2.0 } else { 0.0 },
                y0.min(y1) - if dy < 0.01 { thin / 2.0 } else { 0.0 },
                dx.max(thin),
                dy.max(thin),
                Color::rgba(c[0], c[1], c[2], c[3]),
            );
        }
        draw_rail_separators(pixmap, &pill, origin, position, scale);
        // How far the block under the pointer has opened. The layout is settled, so a frame of the
        // transition only moves the capsule's trailing edge and the text that edge uncovers.
        let grow = toolbar.grow.clamp(0.0, 1.0);
        // Two passes: settled squares first, then every pop-out — a hovered capsule and a panel's
        // rows — so a pop-out covers the buttons it grows across instead of being cut by whichever
        // control happens to come later in the layout.
        for layer in [false, true] {
            for control in &pill.controls {
                let on_top = control.stretched || control.block.column == 1;
                if on_top != layer {
                    continue;
                }
                // A rail block's capsule opens under the pointer; a panel's rows open once, when the panel
                // does. Both grow from the block square, and neither moves while it opens.
                let opening = if control.block.column == 0 {
                    grow
                } else {
                    toolbar.panel_grow.clamp(0.0, 1.0)
                };
                let full = if position.horizontal() {
                    control.along_extent
                } else {
                    control.across_extent
                };
                let extent = if control.stretched {
                    TOOLBAR_BLOCK + (full - TOOLBAR_BLOCK) * opening
                } else {
                    full
                };
                // A block of the drawer is offset a column inward from the rail's.
                let block_local = RailRect {
                    along: control.along,
                    across: control.across,
                    along_extent: if position.horizontal() {
                        extent
                    } else {
                        TOOLBAR_BLOCK
                    },
                    across_extent: if position.horizontal() {
                        TOOLBAR_BLOCK
                    } else {
                        extent
                    },
                };
                let block = device(block_local);
                let capsule = local(block_local);
                // A stretched block is a capsule; a square block is a rounded square.
                let radius = if control.stretched {
                    TOOLBAR_BLOCK / 2.0
                } else {
                    TOOLBAR_RADIUS
                } * scale;
                let square_local = position.place(
                    origin,
                    RailRect {
                        along: control.along,
                        across: control.across,
                        along_extent: TOOLBAR_BLOCK,
                        across_extent: TOOLBAR_BLOCK,
                    },
                );
                let square = Rect {
                    x: square_local.x * scale,
                    y: square_local.y * scale,
                    w: TOOLBAR_BLOCK * scale,
                    h: TOOLBAR_BLOCK * scale,
                };
                let hovered =
                    control.block.action.is_some() && control.block.action == toolbar.hover;
                // The key on the block's face. It is the primary shortcut only; the capsule the block
                // opens into states the full key, an alternate or a chord included.
                let badge = control.block.action.and_then(badge_of);
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
                        if hovered {
                            round_rect(
                                pixmap,
                                square.grown(2.0 * scale),
                                TOOLBAR_RADIUS * scale,
                                parse_hex(TOOLBAR_HOVER_FILL),
                                None,
                            );
                        }
                        round_rect(
                            pixmap,
                            square,
                            TOOLBAR_RADIUS * scale,
                            parse_hex(&config::current().colors[*index]),
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
                        // The chip is the colour itself, so the badge takes the ink a note on that
                        // colour would: no single foreground reads on all five palette entries.
                        if let Some(key) = badge {
                            let ink = note_ink(&config::current().colors[*index]);
                            self.draw_badge(pixmap, square, scale, key, ink);
                        }
                    }
                    mark => {
                        // The rail is one card: its buttons share the plate and paint no square of
                        // their own. A pop-out still needs a surface over the plate, and an active
                        // tool still needs its accent. Panel rows keep their own capsules.
                        if control.block.column != 0 {
                            let (fill, edge) = if control.block.active {
                                (TOOLBAR_ACCENT, TOOLBAR_ACCENT)
                            } else if hovered {
                                (TOOLBAR_HOVER_FILL, TOOLBAR_CARD_EDGE)
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
                        } else if control.stretched {
                            round_rect(pixmap, block, radius, parse_hex(TOOLBAR_BLOCK_FILL), None);
                        } else if control.block.active {
                            round_rect(pixmap, block, radius, parse_hex(TOOLBAR_ACCENT), None);
                        } else if hovered {
                            round_rect(pixmap, block, radius, parse_hex(TOOLBAR_HOVER_FILL), None);
                        }
                        if hovered
                            && !control.block.active
                            && control.block.column == 0
                            && control.stretched
                        {
                            round_rect(
                                pixmap,
                                square,
                                TOOLBAR_RADIUS * scale,
                                parse_hex(TOOLBAR_HOVER_FILL),
                                None,
                            );
                        }
                        // What the block draws stays where it is and the capsule grows to its right,
                        // so the block reads as the same square, stretched. An icon or the readout's
                        // number is centred in the square, never in the stretched rectangle.
                        match mark {
                            Mark::Icon(icon) => {
                                draw_icon(
                                    pixmap,
                                    *icon,
                                    square,
                                    scale,
                                    TOOLBAR_FOREGROUND,
                                    badge.is_some(),
                                );
                                if let Some(key) = badge {
                                    self.draw_badge(pixmap, square, scale, key, TOOLBAR_FOREGROUND);
                                }
                            }
                            Mark::Value(value) => {
                                let width = self.text_width(value, TOOLBAR_TEXT_SIZE);
                                let item = toolbar_text(
                                    square_local.x + (TOOLBAR_BLOCK - width) / 2.0,
                                    square_local.y,
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
                            Mark::Swatch(_) | Mark::None => {}
                        }
                    }
                }
                let bare = control.block.mark == Mark::None;
                // A board's row shows its two buttons at its right end while the pointer is on it: the
                // name is clipped where they begin, and each button is a block-sized square of its own.
                let buttons = match control.block.action {
                    Some(ToolbarAction::Board(index)) => board_actions(toolbar, index),
                    _ => None,
                };
                let buttons_w = match buttons {
                    Some((true, _)) => 2.0 * TOOLBAR_BLOCK,
                    Some((false, _)) => TOOLBAR_BLOCK,
                    None => 0.0,
                };
                let label = row_label(toolbar, &control.block);
                let button_rect = |slot: f32| {
                    let local = if position.horizontal() {
                        RailRect {
                            along: control.along + extent - slot * TOOLBAR_BLOCK,
                            across: control.across,
                            along_extent: TOOLBAR_BLOCK,
                            across_extent: TOOLBAR_BLOCK,
                        }
                    } else {
                        RailRect {
                            along: control.along,
                            across: control.across + extent - slot * TOOLBAR_BLOCK,
                            along_extent: TOOLBAR_BLOCK,
                            across_extent: TOOLBAR_BLOCK,
                        }
                    };
                    device(local)
                };
                if (control.stretched || bare) && !label.is_empty() {
                    let text_width = self.text_width(label, TOOLBAR_TEXT_SIZE);
                    let item = toolbar_text(
                        label_origin(position, capsule, bare, text_width),
                        capsule.y,
                        TOOLBAR_BLOCK,
                        TOOLBAR_TEXT_SIZE,
                        TOOLBAR_LABEL_TEXT,
                        label,
                    );
                    // The label is uncovered by the capsule growing over it: a glyph appears once
                    // the edge has passed it, and at rest the edge is past its trailing padding.
                    let reveal = if position == ToolbarPosition::Right {
                        Reveal::Before(capsule.x + buttons_w)
                    } else {
                        Reveal::After(capsule.x + capsule.w - buttons_w)
                    };
                    self.draw_text_with(
                        pixmap,
                        &item,
                        Screen {
                            scale,
                            view: View::default(),
                        },
                        None,
                        Some(reveal),
                        None,
                    );
                }
                if let Some((delete, _)) = buttons {
                    let index = match control.block.action {
                        Some(ToolbarAction::Board(index)) => index,
                        _ => 0,
                    };
                    if board_confirms(toolbar, index) {
                        let actions = [
                            ("no", ToolbarAction::CancelDeleteBoard),
                            ("yes", ToolbarAction::DeleteBoard(index)),
                        ];
                        for (number, (label, action)) in actions.iter().enumerate() {
                            let slot = (actions.len() - number) as f32;
                            let square = button_rect(slot);
                            self.draw_board_button(
                                pixmap,
                                toolbar.hover == Some(*action),
                                square,
                                scale,
                                label,
                            );
                        }
                    } else {
                        let mut actions = Vec::new();
                        if delete {
                            actions.push((Icon::Cross, ToolbarAction::AskDeleteBoard(index)));
                        }
                        actions.push((Icon::Pencil, ToolbarAction::RenameBoard(index)));
                        for (number, (icon, action)) in actions.iter().enumerate() {
                            let slot = (actions.len() - number) as f32;
                            let square = button_rect(slot);
                            // The button under the pointer states its own shape: a wash inside a hairline,
                            // inset far enough that the outline never meets the row's own rounded edge.
                            if toolbar.hover == Some(*action) {
                                let ring = square.grown(-TOOLBAR_HOVER_INSET * scale);
                                round_rect(
                                    pixmap,
                                    ring,
                                    (TOOLBAR_RADIUS - TOOLBAR_HOVER_INSET) * scale,
                                    parse_hex(TOOLBAR_HOVER_FILL),
                                    Some((parse_hex(TOOLBAR_FOREGROUND), TOOLBAR_HAIRLINE * scale)),
                                );
                            }
                            draw_icon(pixmap, *icon, square, scale, TOOLBAR_LABEL_TEXT, false);
                        }
                    }
                }
                // The caret sits after the buffer's text, and the preedit after the caret: a name is
                // typed in its row the way a text box is typed in the canvas.
                if let (Some(edit), Some(ToolbarAction::Board(index))) =
                    (&toolbar.rename, control.block.action)
                    && edit.index == index
                {
                    let typed: String = edit.text.chars().take(edit.cursor).collect();
                    let text_width = self.text_width(label, TOOLBAR_TEXT_SIZE);
                    let x = label_origin(position, capsule, bare, text_width)
                        + self.text_width(&typed, TOOLBAR_TEXT_SIZE);
                    if edit.caret {
                        let caret = Rect {
                            x: x * scale,
                            y: (capsule.y + TOOLBAR_BLOCK * 0.2) * scale,
                            w: TOOLBAR_HAIRLINE.max(1.0) * scale,
                            h: TOOLBAR_BLOCK * 0.6 * scale,
                        };
                        round_rect(pixmap, caret, 0.0, parse_hex(TOOLBAR_ACCENT), None);
                    }
                    if !edit.preedit.is_empty() {
                        let item = toolbar_text(
                            x + TOOLBAR_HAIRLINE,
                            capsule.y,
                            TOOLBAR_BLOCK,
                            TOOLBAR_TEXT_SIZE,
                            TOOLBAR_FOREGROUND,
                            &edit.preedit,
                        );
                        let reveal = if position == ToolbarPosition::Right {
                            Reveal::Before(capsule.x)
                        } else {
                            Reveal::After(capsule.x + capsule.w)
                        };
                        self.draw_text_with(
                            pixmap,
                            &item,
                            Screen {
                                scale,
                                view: View::default(),
                            },
                            None,
                            Some(reveal),
                            None,
                        );
                    }
                }
                // The `more` block states what the drawer holds, on a line under its own label: a control
                // nobody can find is a control nobody uses. It goes in the drawer's own column, which is
                // empty while the drawer is closed, and it is not drawn while the drawer is open — then the
                // controls are on screen, each named by its own block.
                if control.stretched
                && control.block.action == Some(ToolbarAction::More)
                && let Some(legend) = drawer_legend(toolbar, &pill.overflow)
                // The legend is not a block's label, so it carries the same ceiling by hand: a line wider
                // than the room the rail declares would be left behind by a frame that repaints only the
                // rail, which is what the box is for.
                && self.text_width(&legend, TOOLBAR_TEXT_SIZE)
                    <= toolbar_ceiling(pixmap.width() as f32 / scale) - TOOLBAR_DRAWER_X
                {
                    let anchor = position.point(
                        origin,
                        control.along + TOOLBAR_BLOCK + TOOLBAR_LABEL_GAP,
                        TOOLBAR_DRAWER_X,
                    );
                    let width = self.text_width(&legend, TOOLBAR_TEXT_SIZE);
                    let x = if position == ToolbarPosition::Right {
                        anchor.0 - width
                    } else {
                        anchor.0
                    };
                    let item = toolbar_text(
                        x,
                        anchor.1,
                        TOOLBAR_BLOCK,
                        TOOLBAR_TEXT_SIZE,
                        TOOLBAR_LABEL_TEXT,
                        &legend,
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
            }
        }
    }

    /// The zoom percentage and the two history cards, on the edge opposite the rail. They are
    /// separate from the rail's cached layout: no block owns them and they never widen a capsule.
    fn draw_cards(
        &mut self,
        pixmap: &mut PixmapMut,
        toolbar: &Toolbar,
        surface: (f32, f32),
        scale: f32,
        position: CardPosition,
    ) {
        let cards = cards_layout(surface, position);
        let square = |r: Rect| Rect {
            x: r.x * scale,
            y: r.y * scale,
            w: r.w * scale,
            h: r.h * scale,
        };
        let zoom = square(cards.zoom);
        round_rect(
            pixmap,
            zoom,
            TOOLBAR_RADIUS * scale,
            parse_hex(TOOLBAR_BLOCK_FILL),
            Some((parse_hex(TOOLBAR_CARD_EDGE), TOOLBAR_HAIRLINE * scale)),
        );
        draw_card_shadow(pixmap, zoom, TOOLBAR_RADIUS * scale, scale);
        let percent = format!("{}%", (toolbar.zoom * 100.0).round() as i32);
        let text_w = self.text_width(&percent, TOOLBAR_TEXT_SIZE);
        let text = toolbar_text(
            zoom.x / scale + (cards.zoom.w - text_w) / 2.0,
            zoom.y / scale,
            cards.zoom.h,
            TOOLBAR_TEXT_SIZE,
            TOOLBAR_FOREGROUND,
            &percent,
        );
        self.draw_text(
            pixmap,
            &text,
            Screen {
                scale,
                view: View::default(),
            },
            None,
            None,
            None,
        );
        let history = square(cards.history);
        round_rect(
            pixmap,
            history,
            TOOLBAR_RADIUS * scale,
            parse_hex(TOOLBAR_BLOCK_FILL),
            Some((parse_hex(TOOLBAR_CARD_EDGE), TOOLBAR_HAIRLINE * scale)),
        );
        for (rect, action, enabled, icon, badge) in [
            (
                cards.undo,
                ToolbarAction::Undo,
                toolbar.can_undo,
                Icon::Undo,
                "Z",
            ),
            (
                cards.redo,
                ToolbarAction::Redo,
                toolbar.can_redo,
                Icon::Redo,
                "⇧Z",
            ),
        ] {
            let box_ = square(rect);
            let hover = toolbar.hover == Some(action);
            if hover && enabled {
                let ring = box_.grown(-TOOLBAR_HOVER_INSET * scale);
                round_rect(
                    pixmap,
                    ring,
                    (TOOLBAR_RADIUS - TOOLBAR_HOVER_INSET) * scale,
                    parse_hex(TOOLBAR_HOVER_FILL),
                    Some((parse_hex(TOOLBAR_FOREGROUND), TOOLBAR_HAIRLINE * scale)),
                );
            }
            let ink = if enabled {
                TOOLBAR_FOREGROUND
            } else {
                TOOLBAR_LABEL_TEXT
            };
            let icon_box = Rect {
                x: box_.x + (box_.w - 20.0 * scale) / 2.0,
                y: box_.y + 2.0 * scale,
                w: 20.0 * scale,
                h: 20.0 * scale,
            };
            draw_icon(pixmap, icon, icon_box, scale, ink, false);
            self.draw_badge(pixmap, box_, scale, badge, ink);
        }
        let hint = match toolbar.hover {
            Some(ToolbarAction::Undo) if toolbar.can_undo => Some((cards.history, "undo (Ctrl+Z)")),
            Some(ToolbarAction::Redo) if toolbar.can_redo => {
                Some((cards.history, "redo (Ctrl+Shift+Z)"))
            }
            _ => None,
        };
        if let Some((rect, label)) = hint {
            let width = self.text_width(label, TOOLBAR_TEXT_SIZE) + 2.0 * TOOLBAR_LABEL_PAD;
            let x = (rect.x + rect.w / 2.0 - width / 2.0)
                .clamp(TOOLBAR_MARGIN, surface.0 - width - TOOLBAR_MARGIN);
            let y = if cards.position.top() {
                rect.y + rect.h + TOOLBAR_GAP
            } else {
                rect.y - TOOLBAR_GAP - TOOLBAR_BLOCK
            };
            let capsule = Rect {
                x: x * scale,
                y: y * scale,
                w: width * scale,
                h: TOOLBAR_BLOCK * scale,
            };
            round_rect(
                pixmap,
                capsule,
                TOOLBAR_RADIUS * scale,
                parse_hex(TOOLBAR_BLOCK_FILL),
                Some((parse_hex(TOOLBAR_BLOCK_EDGE), TOOLBAR_HAIRLINE * scale)),
            );
            let text = toolbar_text(
                x + TOOLBAR_LABEL_PAD,
                y,
                TOOLBAR_BLOCK,
                TOOLBAR_TEXT_SIZE,
                TOOLBAR_LABEL_TEXT,
                label,
            );
            self.draw_text(
                pixmap,
                &text,
                Screen {
                    scale,
                    view: View::default(),
                },
                None,
                None,
                None,
            );
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
        self.toolbar_hit_at(config::current().toolbar, toolbar, width, height, x, y)
    }

    /// The toolbar block under a pointer position for a rail docked to `position`. The public
    /// [`toolbar_hit`] uses the configured edge; this is the one the edge tests drive directly.
    fn toolbar_hit_at(
        &mut self,
        position: ToolbarPosition,
        toolbar: &Toolbar,
        width: f32,
        height: f32,
        x: f32,
        y: f32,
    ) -> Option<ToolbarAction> {
        let cards = cards_layout((width, height), config::current().cards);
        if let Some(action) = [
            (cards.undo, ToolbarAction::Undo, toolbar.can_undo),
            (cards.redo, ToolbarAction::Redo, toolbar.can_redo),
        ]
        .into_iter()
        .find(|(rect, _, enabled)| {
            *enabled && x >= rect.x && x < rect.x + rect.w && y >= rect.y && y < rect.y + rect.h
        })
        .map(|(_, action, _)| action)
        {
            return Some(action);
        }
        let pill = self.cached_toolbar_pill(toolbar, (width, height), position)?;
        let origin = position.origin((width, height), pill.column);
        let (along, across) = position.to_local(origin, x, y);
        if along < 0.0 || across < 0.0 || along >= pill.reach {
            return None;
        }
        let inside = |r: &RailRect| {
            along >= r.along
                && along < r.along + r.along_extent
                && across >= r.across
                && across < r.across + r.across_extent
        };
        // The corridor answers before the rail's own rows: it is the way from the block into the list.
        if let Some((corridor, owner)) = pill.corridor
            && inside(&corridor)
        {
            return Some(owner);
        }
        if let Some(action) = pill
            .hits
            .iter()
            .find(|(hit, _)| inside(hit))
            .map(|(_, action)| *action)
        {
            return Some(action);
        }
        None
    }

    fn cached_toolbar_pill(
        &mut self,
        toolbar: &Toolbar,
        surface: (f32, f32),
        position: crate::config::ToolbarPosition,
    ) -> Option<ToolbarPill> {
        if let Some(cached) = self.toolbar_cache.iter().find(|cached| {
            same_layout(&cached.toolbar, toolbar)
                && cached.surface == surface
                && cached.position == position
        }) {
            return Some(cached.pill.clone());
        }
        let pill = self.toolbar_pill(toolbar, surface, position);
        if self.toolbar_cache.len() == TOOLBAR_CACHE_CAP {
            self.toolbar_cache.remove(0);
        }
        self.toolbar_cache.push(CachedToolbar {
            toolbar: toolbar.clone(),
            surface,
            position,
            pill: pill.clone(),
        });
        Some(pill)
    }

    /// Lay the rail out top to bottom. Only the block under the pointer is measured: every other
    /// block is a square, and a block only stretches when its meaning fits the room the damage
    /// accounting reserves for the rail.
    fn toolbar_pill(
        &mut self,
        toolbar: &Toolbar,
        surface: (f32, f32),
        position: crate::config::ToolbarPosition,
    ) -> ToolbarPill {
        let ceiling = toolbar_ceiling(surface.0);
        let layout = rail_layout(toolbar, surface, position);
        let RailLayout {
            rail,
            overflow,
            column,
            panel,
            reach,
        } = layout;
        let origin = position.origin(surface, column);
        let mut controls = Vec::new();
        let mut hits = Vec::new();
        // The drawer is the `more` block's own list, so its column starts on that block's row and grows
        // upwards when the rows below would not hold it — the way a menu at the bottom of a window opens
        // upwards. Starting at the top of the column instead left a control the rail has no room for
        // sitting far from the block that owns it, with nothing saying the two belong together.
        let owner = panel.as_ref().map(|(owner, _, _)| *owner);
        // The panel's rows and its plate both come from the layout, where one row is level with the block that
        // opened it and the rest grow away from it. Placing them here as well is how the rows and the plate
        // drifted apart by a row.
        let plate = panel.as_ref().map(|(_, _, plate)| *plate);
        // One along counter per column: a side rail stacks its blocks down the column, and the drawer
        // is a second column that leaves the rail's length alone. A top or bottom rail stacks the
        // panel's rows across itself instead, below (or above) the block that opened them.
        let mut along = [TOOLBAR_PAD; 2];
        let mut panel_across = plate.map_or(TOOLBAR_PAD, |plate| plate.across + TOOLBAR_PAD);
        let blocks: Vec<Block> = rail
            .iter()
            .cloned()
            .chain(
                panel
                    .as_ref()
                    .map(|(_, rows, _)| rows.clone())
                    .unwrap_or_default(),
            )
            .collect();
        for block in blocks {
            let column = block.column as usize;
            if column == 1
                && let Some(plate) = plate
                && !position.horizontal()
            {
                along[column] = along[column].max(plate.along + TOOLBAR_PAD);
            }
            let (row, across) = if position.horizontal() && block.column > 0 {
                (
                    plate.map_or(TOOLBAR_PAD, |plate| plate.along + TOOLBAR_PAD),
                    panel_across,
                )
            } else {
                (along[column], block.column as f32 * TOOLBAR_DRAWER_X)
            };
            let gap = block.gap;
            // The block that owns the open panel stays a square: the list it opened says what it is,
            // and a caption growing over it would push against the list's own rows.
            let hovered =
                block.action.is_some() && block.action == toolbar.hover && block.action != owner;
            // The capsule is [block][gap][label][padding], so the text never touches its edge. A row of the
            // drawer states its name and key whether or not the pointer is on it: a menu row is read and
            // chosen between, and a row that has to be hovered before it can be read has to be probed.
            let labelled = hovered || block.column == 1;
            let bare = block.mark == Mark::None;
            // A row being renamed draws its buffer, so the row is measured from the text it draws:
            // measuring the old name would clip a longer one at the capsule's edge.
            let text = row_label(toolbar, &block);
            let label =
                (labelled && !text.is_empty()).then(|| self.text_width(text, TOOLBAR_TEXT_SIZE));
            // A row of the drawer starts a column to the right, so it has that much less of the reserve to
            // reach into: the reserve is the room a control may take from the rail's own edge, and
            // `toolbar_bounds` — what a frame that repaints only the rail damages — is that wide. A top
            // or bottom rail spells its capsule along the rail, so its room is what is left of the
            // output from the block's own start.
            let ceiling = if position.horizontal() {
                if block.column > 0 {
                    plate.map_or(ceiling, |plate| {
                        ceiling.min(plate.along_extent - 2.0 * TOOLBAR_LABEL_PAD)
                    })
                } else {
                    let room = surface.0 - TOOLBAR_MARGIN - (origin.0 + row);
                    ceiling.min(room.max(0.0))
                }
            } else if block.column == 0 {
                ceiling
            } else {
                ceiling - TOOLBAR_DRAWER_X
            };
            let mut width = match label {
                // A row of names has no square, so its capsule is the text and a pad on each side: the
                // text is centred in the control rather than pushed against one end of it.
                Some(label) if bare && label + 2.0 * TOOLBAR_LABEL_PAD <= ceiling => {
                    label + 2.0 * TOOLBAR_LABEL_PAD
                }
                Some(label)
                    if TOOLBAR_BLOCK + TOOLBAR_LABEL_GAP + label + TOOLBAR_LABEL_PAD <= ceiling =>
                {
                    TOOLBAR_BLOCK + TOOLBAR_LABEL_GAP + label + TOOLBAR_LABEL_PAD
                }
                _ => TOOLBAR_BLOCK,
            };
            // A board's row makes room for its two buttons while they are up: the name is drawn to the
            // left of them, and the row's own hit stops where they start.
            let buttons = match block.action {
                Some(ToolbarAction::Board(index)) => board_actions(toolbar, index),
                _ => None,
            };
            // One board has nothing to delete, so its row makes room for its rename alone.
            let button_w = match buttons {
                Some((true, _)) => 2.0 * TOOLBAR_BLOCK,
                Some((false, _)) => TOOLBAR_BLOCK,
                None => 0.0,
            };
            width = (width + button_w).min(ceiling);
            // A top or bottom rail stacks panel rows below (or above) the rail, so a row that would
            // fall past the output's interior is a row nobody could see or click.
            if position.horizontal() && block.column > 0 {
                if across + TOOLBAR_BLOCK > surface.1 - TOOLBAR_MARGIN {
                    continue;
                }
                if width < TOOLBAR_BLOCK {
                    continue;
                }
            }
            // A menu row on a top or bottom rail ends under the block that opened it, so the walk
            // down from that block lands on the first row rather than beside it.
            let row = if position.horizontal() && block.column > 0 {
                plate.map_or(row, |plate| {
                    plate.along + plate.along_extent - TOOLBAR_LABEL_PAD - width
                })
            } else {
                row
            };
            let (along_extent, across_extent) = if position.horizontal() {
                (width, TOOLBAR_BLOCK)
            } else {
                (TOOLBAR_BLOCK, width)
            };
            if let Some(action) = block.action {
                // The corridor: while the panel is open, the block that opens it owns the row as far as the
                // panel's own edge — so the pointer can travel from the block into the list without
                // crossing a region that owns nothing and would close the panel on the way. It stops at
                // that edge because a hit box reaching into the panel would win over the panel's rows:
                // the rail's boxes are the ones tested first.
                let (hit_along, hit_across) = if owner == Some(action) {
                    (TOOLBAR_BLOCK, TOOLBAR_DRAWER_X)
                } else if buttons.is_some() {
                    if position.horizontal() {
                        (along_extent - button_w, across_extent)
                    } else {
                        (along_extent, across_extent - button_w)
                    }
                } else {
                    (along_extent, across_extent)
                };
                hits.push((
                    RailRect {
                        along: row,
                        across,
                        along_extent: hit_along,
                        across_extent: hit_across,
                    },
                    action,
                ));
            }
            controls.push(Control {
                block,
                along: row,
                across,
                along_extent,
                across_extent,
                stretched: width > TOOLBAR_BLOCK,
            });
            if position.horizontal() && column > 0 {
                panel_across = across + TOOLBAR_BLOCK + gap;
            } else {
                along[column] = row + TOOLBAR_BLOCK + gap;
            }
        }
        debug_assert_eq!(along[0] + TOOLBAR_PAD, column); // The rail consumes every press on a block, and the rows between its own blocks belong to a
        // neighbour as well: a press on the size readout or in a gap has to step something rather than
        // start a stroke under the rail. The drawer's column is not filled that way: it is only as tall
        // as its controls, so a press beside one is the canvas's. The rail's controls are built before
        // the drawer's, so they are the leading rows of `hits`.
        let rail_rows = hits.iter().take_while(|(r, _)| r.across == 0.0).count();
        if let Some((first, _)) = hits.first_mut() {
            first.along_extent = first.along_extent.max(first.along);
            first.along = 0.0;
        }
        for index in 0..rail_rows.saturating_sub(1) {
            let next = hits[index + 1].0.along;
            hits[index].0.along_extent = hits[index].0.along_extent.max(next - hits[index].0.along);
        }
        if rail_rows > 0 {
            hits[rail_rows - 1].0.along_extent = hits[rail_rows - 1]
                .0
                .along_extent
                .max(column - hits[rail_rows - 1].0.along);
        }
        // The panel's rows, by the same rule: the four pixels between two rows belong to the row above
        // them, so moving from one to the next is staying on the list. Only between its own first and
        // last row, though — the strip beside the rail at any other height is the canvas's, and the
        // block level with a row is the rail's, whose box is tested first. A side rail stacks its rows
        // down the column and fills the gap along the rail; a top or bottom rail stacks them down the
        // output instead, so the gap is filled across it.
        if position.horizontal() {
            for index in rail_rows..hits.len().saturating_sub(1) {
                let next = hits[index + 1].0.across;
                hits[index].0.across_extent =
                    hits[index].0.across_extent.max(next - hits[index].0.across);
            }
        } else {
            for index in rail_rows..hits.len().saturating_sub(1) {
                hits[index].0.along_extent = hits[index + 1].0.along - hits[index].0.along;
            }
        }
        // A board's two buttons answer before the row they sit on: their rects are inside the row's
        // box, past the name, and a hit is the first rect that contains the point. They are appended
        // after the gap-filling above, which walks the rows in order.
        for control in &controls {
            let Some(ToolbarAction::Board(index)) = control.block.action else {
                continue;
            };
            let Some((delete, _)) = board_actions(toolbar, index) else {
                continue;
            };
            // A button takes the strip its row was given, so the gap between two rows belongs to the
            // row above and the pointer can walk a column of buttons without falling out of it.
            let row_h = hits
                .iter()
                .rev()
                .find(|(_, action)| Some(*action) == control.block.action)
                .map_or(TOOLBAR_BLOCK, |(rect, _)| rect.along_extent);
            let mut actions = Vec::new();
            if board_confirms(toolbar, index) {
                actions.push(ToolbarAction::CancelDeleteBoard);
                actions.push(ToolbarAction::DeleteBoard(index));
            } else {
                if delete {
                    actions.push(ToolbarAction::AskDeleteBoard(index));
                }
                actions.push(ToolbarAction::RenameBoard(index));
            }
            for (number, action) in actions.iter().enumerate() {
                let slot = (actions.len() - number) as f32;
                let rect = if position.horizontal() {
                    // The capsule grows along the rail, so its buttons sit at its trailing end.
                    RailRect {
                        along: control.along + control.along_extent - slot * TOOLBAR_BLOCK,
                        across: control.across,
                        along_extent: TOOLBAR_BLOCK,
                        across_extent: TOOLBAR_BLOCK,
                    }
                } else {
                    // The capsule grows across the rail, so its buttons sit at the inward end, and
                    // take the row's gap so a column of them stays contiguous.
                    RailRect {
                        along: control.along,
                        across: control.across + control.across_extent - slot * TOOLBAR_BLOCK,
                        along_extent: row_h,
                        across_extent: TOOLBAR_BLOCK,
                    }
                };
                hits.push((rect, *action));
            }
        }
        // The backdrop row expands sideways: four mode buttons level with the `backdrop` row,
        // starting one drawer-column to its right. On a top or bottom rail they are the same row
        // stepped across the rail instead. The drawer's plate already covers them, and their vertical
        // position is the row's, not a column walk.
        if toolbar.more
            && let Some(scrim) = controls
                .iter()
                .find(|control| control.block.action == Some(ToolbarAction::Scrim))
                .map(|control| (control.along, control.across))
        {
            for (index, mode) in Backdrop::ALL.iter().enumerate() {
                let column = 2 + index as u8;
                let action = ToolbarAction::Backdrop(*mode);
                let icon = match mode {
                    Backdrop::Transparent => Icon::Clear,
                    Backdrop::Grid => Icon::Grid,
                    Backdrop::Dots => Icon::Dots,
                    Backdrop::Black => Icon::Scrim,
                };
                // A side rail puts the four options beside the backdrop row; a top or bottom rail,
                // whose panel rows already point inward, puts them after the row's own square along
                // the rail. Either way they are one strip with the row they belong to.
                let (along, across) = if position.horizontal() {
                    (scrim.0 + (index as f32 + 1.0) * TOOLBAR_DRAWER_X, scrim.1)
                } else {
                    (scrim.0, column as f32 * TOOLBAR_DRAWER_X)
                };
                controls.push(Control {
                    block: Block {
                        mark: Mark::Icon(icon),
                        column,
                        label: String::new(),
                        action: Some(action),
                        active: toolbar.backdrop == *mode,
                        gap: 0.0,
                    },
                    along,
                    across,
                    along_extent: TOOLBAR_BLOCK,
                    across_extent: TOOLBAR_BLOCK,
                    stretched: false,
                });
                hits.push((
                    RailRect {
                        along,
                        across,
                        along_extent: TOOLBAR_BLOCK,
                        across_extent: TOOLBAR_BLOCK,
                    },
                    action,
                ));
            }
        }
        // The walkway into a panel. The drawer and the boards list own the strip beside the rail, from
        // their first row to their last — a block level with a row answers for the list, which is how the
        // pointer walks into a tall list from any height. The width list hangs off the readout, whose rows
        // reach the boards block just below it, so a full strip would shadow that block; it needs only the
        // gap beside the rail's blocks, and those keep their own press.
        let corridor_across = match owner {
            Some(ToolbarAction::Size) => TOOLBAR_BLOCK,
            _ => 0.0,
        };
        let corridor = match (plate, owner) {
            (Some(plate), Some(owner)) => Some((
                RailRect {
                    along: plate.along,
                    across: corridor_across,
                    along_extent: plate.along_extent,
                    across_extent: TOOLBAR_DRAWER_X - corridor_across,
                },
                owner,
            )),
            _ => None,
        };
        ToolbarPill {
            controls,
            hits,
            corridor,
            column,
            reach,
            overflow,
        }
    }

    /// Width of one line of text, in logical pixels. The shaped buffer is thrown away: the
    /// toolbar is a handful of short strings and it is only drawn while editing.
    fn text_width(&mut self, text: &str, size: f32) -> f32 {
        let mut buffer = Buffer::new_empty(Metrics::new(size, size * LINE_HEIGHT_SCALE));
        buffer.set_text(text, &Attrs::new(), Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.font_system, false);
        buffer.layout_runs().map(|r| r.line_w).fold(0.0, f32::max)
    }

    /// The key a block carries on its face: the primary shortcut, in the bottom-left corner of its
    /// square. It stays inside the square while the capsule opens, so it never collides with the
    /// label the capsule uncovers.
    fn draw_badge(
        &mut self,
        pixmap: &mut PixmapMut,
        square: Rect,
        scale: f32,
        key: &str,
        color: &str,
    ) {
        let width = self.text_width(key, BADGE_TEXT_SIZE);
        let box_ = badge_box(square, scale, width);
        let item = toolbar_text(
            box_.x / scale,
            box_.y / scale,
            box_.h / scale,
            BADGE_TEXT_SIZE,
            color,
            key,
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

    /// One word button in a board row: the confirmation's `no` and `yes`. It uses the same hover ring
    /// and square as the icon buttons; the word is centred in the square instead of an icon.
    fn draw_board_button(
        &mut self,
        pixmap: &mut PixmapMut,
        hover: bool,
        square: Rect,
        scale: f32,
        label: &str,
    ) {
        if hover {
            let ring = square.grown(-TOOLBAR_HOVER_INSET * scale);
            round_rect(
                pixmap,
                ring,
                (TOOLBAR_RADIUS - TOOLBAR_HOVER_INSET) * scale,
                parse_hex(TOOLBAR_HOVER_FILL),
                Some((parse_hex(TOOLBAR_FOREGROUND), TOOLBAR_HAIRLINE * scale)),
            );
        }
        let width = self.text_width(label, TOOLBAR_TEXT_SIZE);
        let item = toolbar_text(
            square.x / scale + (TOOLBAR_BLOCK - width) / 2.0,
            square.y / scale,
            TOOLBAR_BLOCK,
            TOOLBAR_TEXT_SIZE,
            TOOLBAR_FOREGROUND,
            label,
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
    /// The measured box of every text in `items`, in canvas coordinates: the selection frame, its
    /// handles and the press that grabs them use these, so a frame sits on the words rather than on
    /// the damage estimate, which is deliberately wider.
    pub fn text_boxes(
        &mut self,
        ann: &Board,
        items: &[ItemId],
        screen: Screen,
    ) -> Vec<(usize, Rect)> {
        items
            .iter()
            .filter_map(|id| match id {
                ItemId::Text(i) => Some(*i),
                _ => None,
            })
            .filter_map(|i| {
                let text = ann.texts.get(i)?;
                Some((i, self.text_edit_bounds(&text.text, text, screen)))
            })
            .collect()
    }

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
        let border = config::current().border;
        round_rect(
            pixmap,
            square,
            screen.len(NOTE_RADIUS),
            parse_hex(&note.color),
            (border > 0.0).then(|| (parse_hex(note.ink()), screen.ui(border))),
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
        // Bottom-right, measured rather than estimated. The date is part of the note, so it is measured
        // and placed in canvas units and drawn through the view: it is the same size as the square it
        // sits in at every zoom, and a fixed interface size would have left it the same size in a
        // square that had grown, and hanging outside one that had shrunk.
        let width = self.text_width(&note.created, NOTE_DATE_SIZE);
        let box_ = note.date_box();
        let item = TextItem {
            x: box_.x + box_.w - width,
            y: box_.y,
            link: None,
            ..date
        };
        self.draw_text(pixmap, &item, screen, None, None, None);
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

    /// `reveal` is the logical x a progress animation has uncovered: glyphs on its far side are
    /// left out, so the text appears as the shape around it grows.
    fn draw_prepared_text(
        &mut self,
        pixmap: &mut PixmapMut,
        item: &TextItem,
        screen: Screen,
        prepared: &mut PreparedText,
        reveal: Option<Reveal>,
    ) {
        let origin = screen.point([item.x, item.y]);
        let (ox, oy) = (origin[0], origin[1]);
        let c = parse_hex(&item.color);
        let color = Color::rgba(c[0], c[1], c[2], c[3]);
        let (clip_w, clip_h) = (pixmap.width(), pixmap.height());
        let reveal = reveal.map(|reveal| match reveal {
            Reveal::After(x) => Reveal::After(screen.point([x, 0.0])[0]),
            Reveal::Before(x) => Reveal::Before(screen.point([x, 0.0])[0]),
        });
        prepared.buffer.draw(
            &mut self.font_system,
            &mut self.cache,
            color,
            |x, y, w, h, c| {
                let hidden = match reveal {
                    Some(Reveal::After(reveal)) => ox + x as f32 > reveal,
                    Some(Reveal::Before(reveal)) => ox + x as f32 + (w as f32) < reveal,
                    None => false,
                };
                if hidden {
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
        // A link is visible without hovering it: one accent hairline under the measured line.
        if item.link.is_some() {
            let size = screen.len(item.size);
            let c = parse_hex(TOOLBAR_ACCENT);
            fill(
                pixmap,
                ox,
                oy + prepared.baseline + size * 0.15,
                prepared.line_w,
                screen.ui(UNDERLINE_HEIGHT),
                Color::rgba(c[0], c[1], c[2], c[3]),
            );
        }
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
        self.draw_text_with(pixmap, item, screen, edit, reveal.map(Reveal::After), wrap);
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_text_with(
        &mut self,
        pixmap: &mut PixmapMut,
        item: &TextItem,
        screen: Screen,
        edit: Option<&TextOverlay>,
        reveal: Option<Reveal>,
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

/// How far a damage region is grown past the box it was reported for, in logical pixels: enough to hold
/// the half of a hairline that is drawn outside the box it belongs to.
pub const DAMAGE_SLACK: f32 = 1.0;

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
        // An element reports the box it occupies, and the hairline it is drawn with is centred on that
        // box's edge, so half of the stroke falls outside it. A region that stopped at the box left that
        // ring of the previous frame behind wherever the element had moved away from: dragging a note
        // leftwards trailed the old note's outline, one pixel of it, at the edge the box excluded.
        let damage = damage.grown(DAMAGE_SLACK);
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
    let stroke = overlay.stroke.as_ref().and_then(Stroke::paint_bounds);
    let shape = overlay.shape.as_ref().map(Shape::paint_bounds);
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
    let connectors = overlay
        .connectors
        .iter()
        .filter_map(|connector| match connector {
            LiveConnector::Stroke(_, stroke) => stroke.paint_bounds(),
            LiveConnector::Shape(_, shape) => Some(shape.paint_bounds()),
        });
    [stroke, shape, note, eraser, marquee]
        .into_iter()
        .flatten()
        .chain(connectors)
        .reduce(Rect::union)
}

/// Radius of a solid connector anchor, in logical pixels.
const ANCHOR_RADIUS: f32 = 3.5;

/// The shapes an active connector is attached to, for the accent outline that says "this end is
/// magnetic". Selection drags only show anchors, not target outlines.
fn snap_target_ids(overlay: &Overlay) -> Vec<u64> {
    let mut ids = Vec::new();
    let mut add = |attachment: Option<crate::canvas::Attachment>| {
        if let Some(attachment) = attachment
            && !ids.contains(&attachment.shape)
        {
            ids.push(attachment.shape);
        }
    };
    if let Some(shape) = &overlay.shape
        && matches!(shape.kind, ShapeKind::Line | ShapeKind::Arrow)
    {
        add(shape.attach[0]);
        add(shape.attach[1]);
    }
    if let Some(stroke) = &overlay.stroke {
        add(stroke.attach[0]);
        add(stroke.attach[1]);
    }
    ids
}

/// Every attached endpoint on screen: the connector being drawn, and the live copies a selection
/// drag publishes. A solid dot sits on each, so an attached end can be told from a free one.
fn snap_anchor_points(overlay: &Overlay) -> Vec<[f32; 2]> {
    let mut points = Vec::new();
    let mut add = |attachment: Option<crate::canvas::Attachment>, point: [f32; 2]| {
        if attachment.is_some() {
            points.push(point);
        }
    };
    if let Some(shape) = &overlay.shape
        && matches!(shape.kind, ShapeKind::Line | ShapeKind::Arrow)
    {
        add(shape.attach[0], shape.from);
        add(shape.attach[1], shape.to);
    }
    if let Some(stroke) = &overlay.stroke
        && !stroke.points.is_empty()
    {
        add(stroke.attach[0], stroke.points[0]);
        if let Some(last) = stroke.points.last() {
            add(stroke.attach[1], *last);
        }
    }
    for connector in &overlay.connectors {
        match connector {
            LiveConnector::Shape(_, shape) => {
                add(shape.attach[0], shape.from);
                add(shape.attach[1], shape.to);
            }
            LiveConnector::Stroke(_, stroke) if !stroke.points.is_empty() => {
                add(stroke.attach[0], stroke.points[0]);
                if let Some(last) = stroke.points.last() {
                    add(stroke.attach[1], *last);
                }
            }
            LiveConnector::Stroke(_, _) => {}
        }
    }
    points
}

/// The canvas box the snap highlight paints: the target outlines plus the anchor dots. The frame
/// that removes a snap has to damage it, like any other transient overlay.
pub fn snap_hint_bounds(ann: &Board, overlay: &Overlay) -> Option<Rect> {
    let mut box_: Option<Rect> = None;
    for id in snap_target_ids(overlay) {
        if let Some(shape) = ann.shapes.iter().find(|shape| shape.id == id) {
            let bounds = shape.paint_bounds().grown(ANCHOR_RADIUS);
            box_ = Some(box_.map_or(bounds, |box_| box_.union(bounds)));
        }
    }
    for point in snap_anchor_points(overlay) {
        let anchor = Rect {
            x: point[0],
            y: point[1],
            w: 0.0,
            h: 0.0,
        }
        .grown(ANCHOR_RADIUS + 2.0);
        box_ = Some(box_.map_or(anchor, |box_| box_.union(anchor)));
    }
    box_
}

/// Paint the magnetic state: accent outline on the target shapes, solid accent dots on the
/// attached ends.
fn draw_snap_hints(pixmap: &mut PixmapMut, ann: &Board, overlay: &Overlay, screen: Screen) {
    for id in snap_target_ids(overlay) {
        let Some(shape) = ann.shapes.iter().find(|shape| shape.id == id) else {
            continue;
        };
        let mut highlight = shape.clone();
        highlight.color = TOOLBAR_ACCENT.to_string();
        highlight.width = 2.0;
        draw_shape(pixmap, &highlight, screen, Ink::Precise);
    }
    let c = parse_hex(TOOLBAR_ACCENT);
    let mut paint = Paint::default();
    paint.set_color_rgba8(c[0], c[1], c[2], c[3]);
    paint.anti_alias = true;
    for point in snap_anchor_points(overlay) {
        let at = screen.point(point);
        let mut dot = PathBuilder::new();
        dot.push_circle(at[0], at[1], screen.ui(ANCHOR_RADIUS));
        if let Some(dot) = dot.finish() {
            pixmap.fill_path(
                &dot,
                &paint,
                tiny_skia::FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
    }
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
        && a.boards_open == b.boards_open
        && a.size_open == b.size_open
        && a.clipboard_open == b.clipboard_open
        && a.clips == b.clips
        && a.target == b.target
        && a.rename == b.rename
        && a.delete_confirm == b.delete_confirm
        && a.boards == b.boards
        && a.shared == b.shared
        && a.backdrop == b.backdrop
}

/// One block of the rail as laid out: where it sits along the rail and across it, and the extent it
/// occupies in each direction. A stretched rail block grows across a side rail and along a top or
/// bottom one, because its text always reads left to right.
#[derive(Clone)]
struct Control {
    block: Block,
    /// Start of the block along the rail's axis.
    along: f32,
    /// Start of the block across the rail, toward the interior.
    across: f32,
    /// Extent along the rail: `TOOLBAR_BLOCK` for a row of a side rail, the capsule's width for a
    /// block of a top or bottom one.
    along_extent: f32,
    /// Extent across the rail: the capsule's width for a side rail, `TOOLBAR_BLOCK` otherwise.
    across_extent: f32,
    stretched: bool,
}

/// A laid-out rail: what to paint and what to hit, in logical pixels in the rail's own coordinates,
/// where `along` runs down or across the rail and `across` runs toward the interior.
#[derive(Clone)]
struct ToolbarPill {
    controls: Vec<Control>,
    hits: Vec<(RailRect, ToolbarAction)>,
    /// Waiting to answer first: while a panel is open, the strip of the rail's column beside it belongs to the
    /// block that opened it. The pointer leaves the block at whatever row it likes on its way into the list,
    /// and a step into a neighbouring block would read as leaving the menu. It is a strip of its own rather
    /// than a wider box on the block, because the rail's rows answer in column order and a later row would win.
    corridor: Option<(RailRect, ToolbarAction)>,
    /// Length of the rail's own column on this output, which the plate and the origin are placed from.
    column: f32,
    /// Everything the rail and its open panel reach along the rail's axis: what the frame paints, what
    /// the hit test accepts and what a rail-only frame damages.
    reach: f32,
    /// The controls the rail had no room for: the drawer lists them after its own rows.
    overflow: Vec<Block>,
}

/// A label vertically centred in a `height`-tall box whose top edge is `top`.
fn toolbar_text(x: f32, top: f32, height: f32, size: f32, color: &str, text: &str) -> TextItem {
    TextItem {
        x,
        y: top + (height - size * LINE_HEIGHT_SCALE) / 2.0,
        color: color.into(),
        size,
        text: text.into(),
        link: None,
    }
}

/// A soft inner shadow along the card edge. It stays inside the card's own box, so the damage
/// accounting and the hit box remain exactly the card's geometry.
fn draw_card_shadow(pixmap: &mut PixmapMut, rect: Rect, radius: f32, scale: f32) {
    let inset = TOOLBAR_SHADOW_REACH * scale;
    round_rect(
        pixmap,
        rect.grown(-inset),
        (radius - inset).max(0.0),
        [0, 0, 0, 0],
        Some((parse_hex(TOOLBAR_CARD_SHADOW), 2.0 * inset)),
    );
}

/// The room a stretched block may take, from the rail's own edge: bounded by the output's width and by the
/// reserve the damage accounting holds, so nothing the rail draws reaches past the box it declares.
fn toolbar_ceiling(surface: f32) -> f32 {
    (surface - 2.0 * TOOLBAR_MARGIN).min(TOOLBAR_RESERVE)
}

/// One zoom readout and one history card. The history card holds both buttons, so it keeps the
/// same width and background as the percentage.
#[derive(Clone, Copy)]
struct CardsLayout {
    zoom: Rect,
    history: Rect,
    undo: Rect,
    redo: Rect,
    position: CardPosition,
}

const CARD_W: f32 = 64.0;
const CARD_H: f32 = TOOLBAR_BLOCK;

fn cards_layout(surface: (f32, f32), position: CardPosition) -> CardsLayout {
    let gap = TOOLBAR_GAP;
    let total = 2.0 * CARD_W + gap;
    let x = if position.left() {
        TOOLBAR_MARGIN
    } else if position.centre() {
        ((surface.0 - total) / 2.0).max(TOOLBAR_MARGIN)
    } else {
        (surface.0 - TOOLBAR_MARGIN - total).max(TOOLBAR_MARGIN)
    };
    let y = if position.top() {
        TOOLBAR_MARGIN
    } else {
        (surface.1 - TOOLBAR_MARGIN - CARD_H).max(0.0)
    };
    let history = Rect {
        x: x + CARD_W + gap,
        y,
        w: CARD_W,
        h: CARD_H,
    };
    let half = CARD_W / 2.0;
    CardsLayout {
        zoom: Rect {
            x,
            y,
            w: CARD_W,
            h: CARD_H,
        },
        history,
        undo: Rect {
            x: history.x,
            y,
            w: half,
            h: CARD_H,
        },
        redo: Rect {
            x: history.x + half,
            y,
            w: half,
            h: CARD_H,
        },
        position,
    }
}

/// The area the cards and a hover hint can paint. The hint opens away from the nearest screen edge.
pub fn cards_bounds(surface: (f32, f32), position: CardPosition) -> Rect {
    let cards = cards_layout(surface, position);
    let cards_box = cards.zoom.union(cards.history).grown(TOOLBAR_SHADOW_REACH);
    if position.top() {
        Rect {
            h: cards_box.h + TOOLBAR_BLOCK + TOOLBAR_GAP,
            ..cards_box
        }
    } else {
        Rect {
            y: cards_box.y - TOOLBAR_BLOCK - TOOLBAR_GAP,
            h: cards_box.h + TOOLBAR_BLOCK + TOOLBAR_GAP,
            ..cards_box
        }
    }
}

/// Everything the rail paints. A stretched block's width depends on the font, so the damage
/// accounting reserves the widest block `toolbar_pill` will build, as a strip on the output's edge
/// the rail is docked to; the strip only joins a damage box that reaches it.
pub fn toolbar_bounds(toolbar: &Toolbar, width: f32, height: f32) -> Rect {
    toolbar_bounds_at(config::current().toolbar, toolbar, width, height)
}

/// The damage box for a rail docked to `position`. The public [`toolbar_bounds`] uses the
/// configured edge; this is the one the edge tests drive directly.
fn toolbar_bounds_at(
    position: ToolbarPosition,
    toolbar: &Toolbar,
    width: f32,
    height: f32,
) -> Rect {
    let surface = (width, height);
    let layout = rail_layout(toolbar, surface, position);
    let origin = position.origin(surface, layout.column);
    // The plate reaches a pad outside the blocks and the hairline is stroked around a path, so it
    // reaches one logical pixel past. The box therefore holds every pixel the rail paints: any less
    // and part of the plate would fall outside a frame that repaints only the rail, keeping stale
    // pixels behind.
    let outer = TOOLBAR_PAD + TOOLBAR_HAIRLINE;
    let strip = toolbar_ceiling(width) + 2.0 * outer;
    if position.horizontal() {
        let strip = strip.min(height);
        let reach = layout
            .reach
            .max(layout.column + TOOLBAR_RESERVE)
            .min((width - origin.0).max(0.0));
        let y = match position {
            ToolbarPosition::Top => origin.1 - outer,
            _ => origin.1 - strip + outer,
        }
        .clamp(0.0, (height - strip).max(0.0));
        Rect {
            x: origin.0 - TOOLBAR_HAIRLINE,
            y,
            w: reach + 2.0 * TOOLBAR_HAIRLINE,
            h: strip,
        }
    } else {
        Rect {
            x: match position {
                ToolbarPosition::Left => origin.0 - outer,
                _ => origin.0 - toolbar_ceiling(width) - outer,
            },
            y: origin.1 - TOOLBAR_HAIRLINE,
            w: strip,
            h: layout.reach + 2.0 * TOOLBAR_HAIRLINE,
        }
    }
}

/// The move-to-board button: a capsule at the bottom centre of the canvas, present only while a
/// selection is held. Logical pixels, like the rail.
pub fn move_button_bounds(size: (f32, f32)) -> Rect {
    Rect {
        x: size.0 / 2.0 - MOVE_BUTTON_W / 2.0,
        y: size.1 - TOOLBAR_MARGIN - MOVE_BUTTON_H,
        w: MOVE_BUTTON_W,
        h: MOVE_BUTTON_H,
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
/// offset is resolved to a paragraph and a byte within it before cosmic-text is asked for a
/// position: glyph offsets inside a `LayoutRun` are line-local, so comparing them with a whole-text
/// offset put every caret on the last line.
fn caret_geometry(prepared: &PreparedText, at: usize, preedit_len: usize) -> CaretGeometry {
    let mut geometry = CaretGeometry {
        x: prepared.line_w,
        top: prepared.line_top,
        height: prepared.line_height,
        baseline: prepared.baseline,
        preedit: None,
    };
    // The buffer's lines are the paragraphs the newlines split; `at` is a byte into the whole text.
    let mut line = 0;
    let mut index = at;
    while let Some(buffer_line) = prepared.buffer.lines.get(line) {
        let len = buffer_line.text().len();
        if index <= len {
            break;
        }
        index = index.saturating_sub(len + 1);
        line += 1;
    }
    let cursor = Cursor::new(line, index);
    let mut found = false;
    for run in prepared.buffer.layout_runs() {
        if run.line_i != line {
            continue;
        }
        // A wrapped paragraph has several runs; only the run that holds the cursor answers.
        let Some(x) = run.cursor_position(&cursor) else {
            continue;
        };
        geometry.x = x;
        geometry.top = run.line_top;
        geometry.height = run.line_height;
        geometry.baseline = run.line_y;
        found = true;
        if preedit_len > 0 {
            for glyph in run
                .glyphs
                .iter()
                .filter(|glyph| glyph.start >= index && glyph.start < index + preedit_len)
            {
                let (x0, x1) = (glyph.x, glyph.x + glyph.w);
                geometry.preedit = Some(match geometry.preedit {
                    Some((a, b)) => (a.min(x0), b.max(x1)),
                    None => (x0, x1),
                });
            }
        }
        break;
    }
    if !found && let Some(run) = prepared.buffer.layout_runs().last() {
        // A cursor past the last line still belongs at the end of the text.
        geometry.x = run.line_w;
        geometry.top = run.line_top;
        geometry.height = run.line_height;
        geometry.baseline = run.line_y;
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

/// The box an icon is drawn in. Without a badge it is the square inset by [`ICON_INSET`], as the
/// whole set was drawn before the keys had a face. With one, the icon gives the bottom-left corner
/// to the badge: it is a little smaller and sits at the square's top right, clear of the glyph.
fn icon_box(square: Rect, badged: bool, scale: f32) -> Rect {
    if badged {
        let side = BADGE_ICON_SIDE * scale;
        Rect {
            x: square.x + square.w - ICON_INSET * scale - side,
            y: square.y + BADGE_ICON_TOP * scale,
            w: side,
            h: side,
        }
    } else {
        square.grown(-ICON_INSET * scale)
    }
}

/// Where a key badge sits: the bottom-left inside of its square. `width` is the shaped width of the
/// glyph, in logical pixels, so the box is the text's own size rather than a guess from its length.
fn badge_box(square: Rect, scale: f32, width: f32) -> Rect {
    let height = BADGE_TEXT_SIZE * LINE_HEIGHT_SCALE * scale;
    Rect {
        x: square.x + BADGE_INSET * scale,
        y: square.y + square.h - BADGE_INSET * scale - height,
        w: width * scale,
        h: height,
    }
}

/// The eraser's outline and band, in the box an icon is drawn in: a block lying at about 30° up
/// from the horizontal, with the band across the end its sleeve grips. The four corners and the
/// band's two ends are returned so the drawing and the test that checks it read the same figure.
fn eraser_icon(box_: Rect) -> ([[f32; 2]; 4], [[f32; 2]; 2]) {
    let (cx, cy) = (box_.x + box_.w / 2.0, box_.y + box_.h / 2.0);
    // Half the block's length and half its thickness. The angle is 30°: enough slant to read as
    // an object lying on the block rather than as another square tool.
    let (a, b) = (box_.w * 0.46, box_.h * 0.20);
    let (sin, cos) = (0.5, 0.866_025_4);
    let along = [cos * a, -sin * a];
    let across = [sin * b, cos * b];
    let corners = [
        [cx + along[0] + across[0], cy + along[1] + across[1]],
        [cx + along[0] - across[0], cy + along[1] - across[1]],
        [cx - along[0] + across[0], cy - along[1] + across[1]],
        [cx - along[0] - across[0], cy - along[1] - across[1]],
    ];
    // The band is a third of the way along the block from its upper end, where a sleeve would
    // meet the rubber.
    let (band_x, band_y) = (cx + along[0] * 0.35, cy + along[1] * 0.35);
    let band = [
        [band_x + across[0], band_y + across[1]],
        [band_x - across[0], band_y - across[1]],
    ];
    (corners, band)
}

/// An icon, drawn inside the square of its block: geometry, in the block's foreground colour, in the
/// interface's own stroke language. The shape tools' icons call the code their shapes are drawn by,
/// from the same two points, so an icon cannot drift from what its tool draws. A badged block draws
/// its icon in the top-right of the square and leaves the bottom-left to its key.
fn draw_icon(
    pixmap: &mut PixmapMut,
    icon: Icon,
    square: Rect,
    scale: f32,
    color: &str,
    badged: bool,
) {
    let c = parse_hex(color);
    let mut paint = Paint::default();
    paint.set_color_rgba8(c[0], c[1], c[2], c[3]);
    paint.anti_alias = true;
    let box_ = icon_box(square, badged, scale);
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
                rotate: 0.0,
                id: 0,
                attach: [None, None],
            },
            icon_screen,
            Ink::Precise,
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
    if icon == Icon::Cross {
        // A cross: two lines through the middle of the box.
        for (from, to) in [((0.25, 0.25), (0.75, 0.75)), ((0.75, 0.25), (0.25, 0.75))] {
            let mut line = PathBuilder::new();
            line.move_to(x + w * from.0, y + h * from.1);
            line.line_to(x + w * to.0, y + h * to.1);
            if let Some(path) = line.finish() {
                pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
            }
        }
        return;
    }
    if icon == Icon::Pencil {
        // A pencil: a body running up the box and a filled tip at its foot.
        let mut body = PathBuilder::new();
        body.move_to(x + w * 0.3, y + h * 0.8);
        body.line_to(x + w * 0.75, y + h * 0.2);
        if let Some(path) = body.finish() {
            pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
        }
        let mut tip = PathBuilder::new();
        tip.move_to(x + w * 0.3, y + h * 0.8);
        tip.line_to(x + w * 0.42, y + h * 0.78);
        tip.line_to(x + w * 0.32, y + h * 0.68);
        tip.close();
        if let Some(path) = tip.finish() {
            pixmap.fill_path(
                &path,
                &paint,
                tiny_skia::FillRule::Winding,
                Transform::identity(),
                None,
            );
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
                attach: [None, None],
            },
            icon_screen,
            Ink::Precise,
        );
        return;
    }
    let mut path = PathBuilder::new();
    let mut filled: Vec<tiny_skia::Path> = Vec::new();
    match icon {
        Icon::Eraser => {
            let (corners, band) = eraser_icon(box_);
            path.move_to(corners[0][0], corners[0][1]);
            for corner in &corners[1..] {
                path.line_to(corner[0], corner[1]);
            }
            path.close();
            path.move_to(band[0][0], band[0][1]);
            path.line_to(band[1][0], band[1][1]);
        }
        Icon::Undo | Icon::Redo => {
            let undo = icon == Icon::Undo;
            let cy = y + h * 0.4;
            let (tip, base, shaft, tail, far) = if undo {
                (
                    x + w * 0.1,
                    x + w * 0.3,
                    x + w * 0.62,
                    x + w * 0.55,
                    x + w * 0.96,
                )
            } else {
                (
                    x + w * 0.9,
                    x + w * 0.7,
                    x + w * 0.38,
                    x + w * 0.45,
                    x + w * 0.04,
                )
            };
            path.move_to(base, cy);
            path.line_to(shaft, cy);
            path.cubic_to(far, cy, far, y + h * 0.85, tail, y + h * 0.85);
            let mut head = PathBuilder::new();
            head.move_to(tip, cy);
            head.line_to(base, cy - h * 0.2);
            head.line_to(base, cy + h * 0.2);
            head.close();
            if let Some(head) = head.finish() {
                filled.push(head);
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
        // Both are drawn and returned before this match: see the blocks above.
        Icon::Cross | Icon::Pencil => {}
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
        Icon::Board => {
            // Two plates, one behind the other: a drawing that is not the screen it is shown on.
            for (dx, dy) in [(0.0, 0.3), (0.3, 0.0)] {
                if let Some(plate) = rounded_rect_path(
                    Rect {
                        x: x + w * dx * 0.6,
                        y: y + h * dy * 0.6,
                        w: w * 0.7,
                        h: h * 0.7,
                    },
                    2.0 * scale,
                ) {
                    path.push_path(&plate);
                }
            }
        }
        Icon::Clipboard => {
            // A board with a clip over its top edge.
            if let Some(body) = rounded_rect_path(
                Rect {
                    x: x + w * 0.2,
                    y: y + h * 0.2,
                    w: w * 0.6,
                    h: h * 0.7,
                },
                2.0 * scale,
            ) {
                path.push_path(&body);
            }
            if let Some(clip) = rounded_rect_path(
                Rect {
                    x: x + w * 0.38,
                    y: y + h * 0.1,
                    w: w * 0.24,
                    h: h * 0.2,
                },
                1.0 * scale,
            ) {
                path.push_path(&clip);
            }
        }
        Icon::Link => {
            // Two chain links, overlapping at the middle.
            for dx in [0.1, 0.4] {
                if let Some(link) = rounded_rect_path(
                    Rect {
                        x: x + w * dx,
                        y: y + h * 0.38,
                        w: w * 0.5,
                        h: h * 0.28,
                    },
                    h * 0.14,
                ) {
                    path.push_path(&link);
                }
            }
        }
        Icon::Clear => {
            if let Some(square) = rounded_rect_path(
                Rect {
                    x: x + w * 0.15,
                    y: y + h * 0.15,
                    w: w * 0.7,
                    h: h * 0.7,
                },
                2.0 * scale,
            ) {
                path.push_path(&square);
            }
        }
        Icon::Grid => {
            for at in [0.2, 0.5, 0.8] {
                path.move_to(x + w * at, y + h * 0.1);
                path.line_to(x + w * at, y + h * 0.9);
                path.move_to(x + w * 0.1, y + h * at);
                path.line_to(x + w * 0.9, y + h * at);
            }
        }
        Icon::Dots => {
            for dy in [0.25, 0.5, 0.75] {
                for dx in [0.25, 0.5, 0.75] {
                    let mut dot = PathBuilder::new();
                    dot.push_circle(x + w * dx, y + h * dy, ICON_STROKE * 0.7 * scale);
                    if let Some(dot) = dot.finish() {
                        filled.push(dot);
                    }
                }
            }
        }
        Icon::Plus => {
            path.move_to(x + w / 2.0, y + h * 0.2);
            path.line_to(x + w / 2.0, y + h * 0.8);
            path.move_to(x + w * 0.2, y + h / 2.0);
            path.line_to(x + w * 0.8, y + h / 2.0);
        }
        Icon::Shared => {
            // The same plate side by side with itself: one board on two screens.
            for dx in [0.0, 0.44] {
                if let Some(plate) = rounded_rect_path(
                    Rect {
                        x: x + w * dx,
                        y: y + h * 0.15,
                        w: w * 0.56,
                        h: h * 0.7,
                    },
                    2.0 * scale,
                ) {
                    path.push_path(&plate);
                }
            }
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
/// Whether a mark is drawn as the geometry stores it or as a hand would have traced it. `Precise` is
/// everything the interface draws: a rail icon is the same figure as the tool it names, and a control
/// is not a mark on the drawing.
#[derive(Clone, Copy)]
enum Ink {
    Precise,
    /// The seed the mark's wander and bow are derived from — the element's own, never a copy's.
    Sketch(u64),
}

/// Emit canvas-space sketch commands through the view. This is the module boundary: `sketch` knows the
/// canvas, `Screen` owns the mapping, and nothing else converts.
fn sketch_path(segs: &[Seg], screen: Screen) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    for seg in segs {
        match *seg {
            Seg::Move(p) => {
                let p = screen.point(p);
                pb.move_to(p[0], p[1]);
            }
            Seg::Line(p) => {
                let p = screen.point(p);
                pb.line_to(p[0], p[1]);
            }
            Seg::Quad(c, p) => {
                let (c, p) = (screen.point(c), screen.point(p));
                pb.quad_to(c[0], c[1], p[0], p[1]);
            }
            Seg::Cubic(a, b, p) => {
                let (a, b, p) = (screen.point(a), screen.point(b), screen.point(p));
                pb.cubic_to(a[0], a[1], b[0], b[1], p[0], p[1]);
            }
        }
    }
    pb.finish()
}

/// The shape's body as hand-drawn commands, in canvas coordinates. An arrow adds the outline of its
/// head, which is traced over the head's fill.
fn sketched_shape(s: &Shape, seed: u64) -> Vec<Seg> {
    let box_ = rect_of(s.from, s.to);
    let mut segs = match s.kind {
        ShapeKind::Circle => sketch::ellipse(
            [box_.x + box_.w / 2.0, box_.y + box_.h / 2.0],
            [box_.w / 2.0, box_.h / 2.0],
            s.width,
            seed,
        ),
        ShapeKind::Rect => sketch::closed(
            &[
                [box_.x, box_.y],
                [box_.x + box_.w, box_.y],
                [box_.x + box_.w, box_.y + box_.h],
                [box_.x, box_.y + box_.h],
            ],
            s.width,
            seed,
        ),
        ShapeKind::Diamond => {
            let (cx, cy) = (box_.x + box_.w / 2.0, box_.y + box_.h / 2.0);
            sketch::closed(
                &[
                    [cx, box_.y],
                    [box_.x + box_.w, cy],
                    [cx, box_.y + box_.h],
                    [box_.x, cy],
                ],
                s.width,
                seed,
            )
        }
        ShapeKind::Line => sketch::line(s.from, s.to, s.width, seed),
        ShapeKind::Arrow => {
            let mut segs = sketch::line(s.from, s.to, s.width, seed);
            if let Some(head) = arrow_head(s) {
                segs.extend(sketch::closed(&head, s.width, seed ^ HEAD_SALT));
            }
            segs
        }
    };
    if segs.is_empty() {
        segs.push(Seg::Move(s.from));
    }
    if s.kind.is_closed() && s.turn_angle() != 0.0 {
        for seg in &mut segs {
            *seg = match *seg {
                Seg::Move(p) => Seg::Move(s.to_world(p)),
                Seg::Line(p) => Seg::Line(s.to_world(p)),
                Seg::Quad(c, p) => Seg::Quad(s.to_world(c), s.to_world(p)),
                Seg::Cubic(a, b, p) => Seg::Cubic(s.to_world(a), s.to_world(b), s.to_world(p)),
            };
        }
    }
    segs
}

/// The three corners of an arrow's head in canvas coordinates: the tip, then the two barbs. `None`
/// when the arrow is too short to have a direction.
fn arrow_head(s: &Shape) -> Option<[[f32; 2]; 3]> {
    let (dx, dy) = (s.to[0] - s.from[0], s.to[1] - s.from[1]);
    let len = (dx * dx + dy * dy).sqrt();
    if len <= f32::EPSILON {
        return None;
    }
    let (ux, uy) = (dx / len, dy / len);
    let (head, half) = (s.head_len(), s.head_half());
    let base = [s.to[0] - ux * head, s.to[1] - uy * head];
    let (px, py) = (-uy * half, ux * half);
    Some([
        s.to,
        [base[0] + px, base[1] + py],
        [base[0] - px, base[1] - py],
    ])
}

const HEAD_SALT: u64 = 0x9ddf_ea08_eb38_2d69;

fn draw_shape(pixmap: &mut PixmapMut, s: &Shape, screen: Screen, ink: Ink) {
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
    let Ink::Sketch(seed) = ink else {
        draw_exact_shape(pixmap, s, screen, &paint, &stroke);
        return;
    };
    if s.kind == ShapeKind::Arrow {
        // The head is filled with the exact triangle and then outlined by hand: filling the two
        // traced passes instead would fill six slivers rather than one head.
        fill_arrow_head(pixmap, s, screen, &paint);
    }
    let Some(path) = sketch_path(&sketched_shape(s, seed), screen) else {
        return;
    };
    pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
}

/// The head of an arrow, filled with the shape's colour. Shared by both inks: a filled head is a
/// filled head, and only its outline is traced.
fn fill_arrow_head(pixmap: &mut PixmapMut, s: &Shape, screen: Screen, paint: &Paint) {
    let Some(corners) = arrow_head(s) else {
        return;
    };
    let mut pb = PathBuilder::new();
    let tip = screen.point(corners[0]);
    pb.move_to(tip[0], tip[1]);
    for corner in &corners[1..] {
        let p = screen.point(*corner);
        pb.line_to(p[0], p[1]);
    }
    pb.close();
    if let Some(path) = pb.finish() {
        pixmap.fill_path(
            &path,
            paint,
            tiny_skia::FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
}

/// The exact outline of a shape: today's geometry, unchanged, for the rail's icons and for anything
/// that has to be a figure rather than a mark.
fn draw_exact_shape(
    pixmap: &mut PixmapMut,
    s: &Shape,
    screen: Screen,
    paint: &Paint,
    stroke: &SkStroke,
) {
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
                let path = if s.turn_angle() != 0.0 {
                    let centre = screen.point(s.centre());
                    path.transform(Transform::from_rotate_at(
                        s.turn_angle().to_degrees(),
                        centre[0],
                        centre[1],
                    ))
                } else {
                    Some(path)
                };
                if let Some(path) = path {
                    pixmap.stroke_path(&path, paint, stroke, Transform::identity(), None);
                }
            }
        }
        ShapeKind::Line | ShapeKind::Arrow => {
            let mut pb = PathBuilder::new();
            pb.move_to(from[0], from[1]);
            pb.line_to(to[0], to[1]);
            if let Some(path) = pb.finish() {
                pixmap.stroke_path(&path, paint, stroke, Transform::identity(), None);
            }
            if s.kind == ShapeKind::Arrow {
                fill_arrow_head(pixmap, s, screen, paint);
            }
        }
    }
}

fn draw_stroke(pixmap: &mut PixmapMut, s: &Stroke, screen: Screen, ink: Ink) {
    let c = parse_hex(&s.color);
    let mut paint = Paint::default();
    paint.set_color_rgba8(c[0], c[1], c[2], c[3]);
    paint.anti_alias = true;
    let width = screen.len(s.width);
    let transform = Transform::identity();
    let stroke = SkStroke {
        width,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Default::default()
    };

    match s.points.as_slice() {
        [] => {}
        [only] => {
            // A dot has no direction to wander, so it stays a disc of the pen's own width.
            let mut pb = PathBuilder::new();
            let p = screen.point(*only);
            pb.push_circle(p[0], p[1], width / 2.0);
            if let Some(path) = pb.finish() {
                pixmap.fill_path(&path, &paint, tiny_skia::FillRule::Winding, transform, None);
            }
        }
        points => {
            let path = match ink {
                Ink::Precise => smoothed_path(points, screen),
                Ink::Sketch(seed) => sketch_path(&sketch::stroke(points, s.width, seed), screen),
            };
            let Some(path) = path else {
                return;
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
    use crate::canvas::{Note, View};

    const EDGES: [ToolbarPosition; 4] = [
        ToolbarPosition::Left,
        ToolbarPosition::Right,
        ToolbarPosition::Top,
        ToolbarPosition::Bottom,
    ];

    /// A block's painted rectangle on an output, from the layout the frame draws.
    fn block_rect(
        position: ToolbarPosition,
        surface: (f32, f32),
        pill: &ToolbarPill,
        control: &Control,
    ) -> Rect {
        position.place(
            position.origin(surface, pill.column),
            RailRect {
                along: control.along,
                across: control.across,
                along_extent: control.along_extent,
                across_extent: control.across_extent,
            },
        )
    }

    fn on_screen(rect: Rect, surface: (f32, f32), what: &str) {
        assert!(
            rect.x >= -TOOLBAR_HAIRLINE
                && rect.y >= -TOOLBAR_HAIRLINE
                && rect.x + rect.w <= surface.0 + TOOLBAR_HAIRLINE
                && rect.y + rect.h <= surface.1 + TOOLBAR_HAIRLINE,
            "{what} is off screen: {rect:?} on {surface:?}"
        );
    }

    /// Every edge lays the rail out as a real column or row: the blocks keep their upright squares,
    /// a stretched capsule grows toward the interior, the origin is inside the output, the first
    /// block sits on the edge it is docked to, and every block answers a press at its centre.
    #[test]
    fn the_rail_docks_to_every_edge() {
        let surface = (900.0, 1000.0);
        let rail = rail_for_legend();
        for position in EDGES {
            let mut renderer = Renderer::new();
            let pill = renderer.toolbar_pill(&rail, surface, position);
            let origin = position.origin(surface, pill.column);
            assert!(
                origin.0 >= 0.0
                    && origin.0 <= surface.0
                    && origin.1 >= 0.0
                    && origin.1 <= surface.1,
                "{position:?} put the rail's origin off screen: {origin:?}"
            );
            on_screen(
                toolbar_bounds_at(position, &rail, surface.0, surface.1),
                surface,
                "the rail's damage box",
            );
            let blocks: Vec<&Control> = pill
                .controls
                .iter()
                .filter(|control| control.block.column == 0)
                .collect();
            assert!(blocks.len() > 4, "the rail has no blocks on {position:?}");
            for control in &blocks {
                let rect = block_rect(position, surface, &pill, control);
                on_screen(rect, surface, "a rail block");
                let action = control.block.action.expect("a rail block is an action");
                assert_eq!(
                    renderer.toolbar_hit_at(
                        position,
                        &rail,
                        surface.0,
                        surface.1,
                        rect.x + rect.w / 2.0,
                        rect.y + rect.h / 2.0
                    ),
                    Some(action),
                    "{position:?}: no hit on {action:?} at its own centre"
                );
            }
            // The rail is a line of squares along its edge. A side rail advances down a column and
            // a top or bottom rail along a row; neither rotates a block.
            let (first, second) = (blocks[0], blocks[1]);
            let (a, b) = (
                block_rect(position, surface, &pill, first),
                block_rect(position, surface, &pill, second),
            );
            assert!(
                (a.w - TOOLBAR_BLOCK).abs() < f32::EPSILON
                    && (a.h - TOOLBAR_BLOCK).abs() < f32::EPSILON,
                "{position:?}: a block was rotated out of its square: {a:?}"
            );
            if position.horizontal() {
                assert!((a.y - b.y).abs() < f32::EPSILON, "a row stepped vertically");
                assert!(b.x > a.x, "a top or bottom rail does not advance sideways");
            } else {
                assert!(
                    (a.x - b.x).abs() < f32::EPSILON,
                    "a column stepped sideways"
                );
                assert!(b.y > a.y, "a side rail does not advance down");
            }
            // A stretched capsule grows toward the interior and keeps its text upright: across the
            // rail on a side, along it on a top or bottom edge. It is laid out from the same square,
            // so the block itself never rotates.
            let hovered = Toolbar {
                hover: Some(ToolbarAction::Tool(Tool::Pen)),
                grow: 1.0,
                ..rail_for_legend()
            };
            let hovered_pill = renderer.toolbar_pill(&hovered, surface, position);
            let pen = hovered_pill
                .controls
                .iter()
                .find(|control| control.block.action == Some(ToolbarAction::Tool(Tool::Pen)))
                .expect("the pen block");
            assert!(
                pen.stretched,
                "{position:?}: the hovered pen did not stretch"
            );
            if position.horizontal() {
                assert!(pen.along_extent > TOOLBAR_BLOCK);
                assert_eq!(pen.across_extent, TOOLBAR_BLOCK);
            } else {
                assert!(pen.across_extent > TOOLBAR_BLOCK);
                assert_eq!(pen.along_extent, TOOLBAR_BLOCK);
            }
            // The first block is on the edge the rail is docked to.
            let centre = (a.x + a.w / 2.0, a.y + a.h / 2.0);
            let near = |edge: f32| edge < 100.0;
            match position {
                ToolbarPosition::Left => assert!(centre.0 < 100.0, "left rail at {centre:?}"),
                ToolbarPosition::Right => {
                    assert!(centre.0 > surface.0 - 100.0, "right rail at {centre:?}")
                }
                ToolbarPosition::Top => assert!(near(centre.1), "top rail at {centre:?}"),
                ToolbarPosition::Bottom => {
                    assert!(centre.1 > surface.1 - 100.0, "bottom rail at {centre:?}")
                }
            }
        }
    }

    /// A panel opens toward the interior on every edge: its rows sit between the rail and the middle
    /// of the output, each row answers a press, and the plate and damage box stay on screen.
    #[test]
    fn every_edge_opens_its_panel_toward_the_interior() {
        let surface = (900.0, 1000.0);
        for position in EDGES {
            let rail = Toolbar {
                more: true,
                ..rail_for_legend()
            };
            let mut renderer = Renderer::new();
            let pill = renderer.toolbar_pill(&rail, surface, position);
            let origin = position.origin(surface, pill.column);
            let (corridor, owner) = pill.corridor.expect("an open drawer has a corridor");
            assert_eq!(owner, ToolbarAction::More);
            on_screen(
                position.place(origin, corridor),
                surface,
                "the drawer's corridor",
            );
            on_screen(
                toolbar_bounds_at(position, &rail, surface.0, surface.1),
                surface,
                "the open drawer's damage box",
            );
            let row = pill
                .controls
                .iter()
                .find(|control| control.block.action == Some(ToolbarAction::Scrim))
                .expect("the drawer's backdrop row");
            let row_rect = block_rect(position, surface, &pill, row);
            on_screen(row_rect, surface, "a drawer row");
            match position {
                ToolbarPosition::Left => {
                    assert!(
                        row_rect.x >= origin.0 + TOOLBAR_BLOCK,
                        "the drawer opened left"
                    )
                }
                ToolbarPosition::Right => assert!(
                    row_rect.x + row_rect.w <= origin.0 - TOOLBAR_BLOCK,
                    "the drawer opened right"
                ),
                ToolbarPosition::Top => assert!(
                    row_rect.y >= origin.1 + TOOLBAR_BLOCK,
                    "the drawer opened up"
                ),
                ToolbarPosition::Bottom => assert!(
                    row_rect.y + row_rect.h <= origin.1 - TOOLBAR_BLOCK,
                    "the drawer opened down"
                ),
            }
            assert_eq!(
                renderer.toolbar_hit_at(
                    position,
                    &rail,
                    surface.0,
                    surface.1,
                    row_rect.x + row_rect.w / 2.0,
                    row_rect.y + row_rect.h / 2.0
                ),
                Some(ToolbarAction::Scrim),
                "{position:?}: the drawer's row does not answer its own press"
            );
            // The whole drawer fits on a normal output whichever edge it opens from, and each of
            // its rows answers at its own centre.
            for action in DRAWER {
                let row = pill
                    .controls
                    .iter()
                    .find(|control| control.block.action == Some(action))
                    .unwrap_or_else(|| panic!("{position:?}: the drawer lost {action:?}"));
                let rect = block_rect(position, surface, &pill, row);
                on_screen(rect, surface, "a drawer row");
                assert_eq!(
                    renderer.toolbar_hit_at(
                        position,
                        &rail,
                        surface.0,
                        surface.1,
                        rect.x + rect.w / 2.0,
                        rect.y + rect.h / 2.0
                    ),
                    Some(action),
                    "{position:?}: {action:?} does not answer its own press"
                );
            }
            // An open menu must not swallow the rail's way out: `Esc` keeps its own press even when
            // the drawer hangs over the blocks beside it.
            let lock = pill
                .controls
                .iter()
                .find(|control| control.block.action == Some(ToolbarAction::Lock))
                .expect("the lock block");
            let rect = block_rect(position, surface, &pill, lock);
            assert_eq!(
                renderer.toolbar_hit_at(
                    position,
                    &rail,
                    surface.0,
                    surface.1,
                    rect.x + rect.w / 2.0,
                    rect.y + rect.h / 2.0
                ),
                Some(ToolbarAction::Lock),
                "{position:?}: the open drawer shadowed the lock block"
            );
            // The boards list and the clipboard list are panels too, and follow the same edge.
            let panels = [
                (
                    Toolbar {
                        boards_open: true,
                        boards: vec![crate::canvas::BoardRow {
                            name: "laptop".into(),
                            current: true,
                        }],
                        ..rail_for_legend()
                    },
                    ToolbarAction::Board(0),
                ),
                (
                    Toolbar {
                        clipboard_open: true,
                        clips: vec![crate::canvas::ClipRow {
                            items: 2,
                            preview: "note".into(),
                        }],
                        ..rail_for_legend()
                    },
                    ToolbarAction::Clip(0),
                ),
            ];
            for (panel_rail, action) in panels {
                let pill = renderer.toolbar_pill(&panel_rail, surface, position);
                let row = pill
                    .controls
                    .iter()
                    .find(|control| control.block.action == Some(action))
                    .unwrap_or_else(|| panic!("{position:?}: no row for {action:?}"));
                let rect = block_rect(position, surface, &pill, row);
                on_screen(rect, surface, "a panel row");
                assert_eq!(
                    renderer.toolbar_hit_at(
                        position,
                        &panel_rail,
                        surface.0,
                        surface.1,
                        rect.x + rect.w / 2.0,
                        rect.y + rect.h / 2.0
                    ),
                    Some(action),
                    "{position:?}: {action:?} does not answer its own press"
                );
            }
        }
    }

    /// Who is in the drawer is one list, and both of the rules that close it are built from that list.
    #[test]
    fn the_drawer_and_its_closing_rules_name_the_same_controls() {
        let rail = crate::render::tests::rail_for_legend();
        let blocks = drawer_blocks(&rail, &[]);
        assert_eq!(
            blocks.len(),
            DRAWER.len(),
            "a control in the drawer has no block, or the other way round"
        );
        for block in &blocks {
            let action = block.action.expect("a drawer control is an action");
            assert!(DRAWER.contains(&action), "{action:?} is not in DRAWER");
        }
        // Picking one of them closes the drawer; picking anything else leaves the rule out of it.
        for action in DRAWER {
            assert!(picks_from_panel(action));
        }
        assert!(!picks_from_panel(ToolbarAction::More));
        assert!(!picks_from_panel(ToolbarAction::Tool(Tool::Pen)));
        // And it stays open once opened, like the boards list: the pointer leaving is not a pick.
        assert!(panel_stays_open(
            Some(ToolbarAction::More),
            Some(ToolbarAction::More)
        ));
        assert!(panel_stays_open(Some(DRAWER[0]), Some(ToolbarAction::More)));
        assert!(panel_stays_open(None, Some(ToolbarAction::More)));
        assert!(panel_stays_open(
            Some(ToolbarAction::Tool(Tool::Pen)),
            Some(ToolbarAction::More)
        ));
        assert!(panel_stays_open(
            Some(ToolbarAction::Color(0)),
            Some(ToolbarAction::More)
        ));
    }

    /// Every list opened from the rail stays until it is dismissed; the pointer leaving it is not a
    /// dismissal. The drawer is the same kind as the boards list.
    #[test]
    fn the_boards_list_stays_open_when_the_pointer_leaves() {
        for hover in [
            None,
            Some(ToolbarAction::Tool(Tool::Hand)),
            Some(ToolbarAction::Lock),
            Some(ToolbarAction::Board(0)),
        ] {
            assert!(
                panel_stays_open(hover, Some(ToolbarAction::Boards)),
                "the boards list closed with {hover:?} under the pointer"
            );
            assert!(
                panel_stays_open(hover, Some(ToolbarAction::More)),
                "the drawer closed with {hover:?} under the pointer"
            );
        }
    }

    /// The opening moves the drawn rows only: the layout and the hits are the settled panel, at every
    /// frame of it, so a click during the 140 ms lands where the row will be.
    #[test]
    fn a_panel_opening_does_not_move_its_layout() {
        let rail = |opening: f32| Toolbar {
            boards_open: true,
            panel_grow: opening,
            boards: vec![crate::canvas::BoardRow {
                name: "office".into(),
                current: true,
            }],
            ..crate::render::tests::rail_for_legend()
        };
        // A fresh renderer per end of the transition: the layout cache is keyed by `same_layout`, which
        // deliberately ignores the opening, so one renderer would serve the first answer to both.
        let layout = |opening: f32| {
            let mut renderer = Renderer::new();
            let pill = renderer.toolbar_pill(
                &rail(opening),
                (900.0, 1000.0),
                crate::config::ToolbarPosition::Left,
            );
            pill.controls
                .iter()
                .map(|control| (control.block.action, control.along, control.across_extent))
                .collect::<Vec<_>>()
        };
        assert_eq!(layout(0.0), layout(1.0), "the opening moved the controls");
        // And the press lands on the same control at both ends of it.
        let mut renderer = Renderer::new();
        for (x, y) in [(20.0, 400.0), (50.0, 400.0), (50.0, 300.0)] {
            assert_eq!(
                renderer.toolbar_hit(&rail(0.0), 900.0, 1000.0, x, y),
                renderer.toolbar_hit(&rail(1.0), 900.0, 1000.0, x, y),
                "the hit at ({x},{y}) changed while the panel opened"
            );
        }
    }

    /// A hovered control with no label must not stretch: there would be nothing under the wider capsule,
    /// and the wider hit box would reach into the panel's column.
    #[test]
    fn a_hovered_readout_does_not_stretch() {
        let rail = Toolbar {
            hover: Some(ToolbarAction::Size),
            ..crate::render::tests::rail_for_legend()
        };
        let mut renderer = Renderer::new();
        let pill =
            renderer.toolbar_pill(&rail, (900.0, 1000.0), crate::config::ToolbarPosition::Left);
        let readout = pill
            .controls
            .iter()
            .find(|control| matches!(control.block.mark, Mark::Value(_)))
            .expect("the rail has no width readout");
        assert_eq!(
            readout.across_extent, TOOLBAR_BLOCK,
            "the readout stretched"
        );
        assert!(!readout.stretched);
    }

    /// A press outside an open panel dismisses it, and a press on the panel's block or one of its rows is
    /// the panel's own business: the board list is opened by a click, so it must close on the next one
    /// elsewhere rather than outlive it.
    #[test]
    fn a_press_outside_the_panel_dismisses_it() {
        let boards = Some(ToolbarAction::Boards);
        assert!(press_dismisses_panel(
            Some(ToolbarAction::Tool(Tool::Pen)),
            boards
        ));
        assert!(press_dismisses_panel(Some(ToolbarAction::Lock), boards));
        assert!(press_dismisses_panel(Some(ToolbarAction::Size), boards));
        assert!(
            press_dismisses_panel(Some(ToolbarAction::More), boards),
            "another list's block was not a press outside this one"
        );
        assert!(!press_dismisses_panel(Some(ToolbarAction::Boards), boards));
        assert!(!press_dismisses_panel(
            Some(ToolbarAction::Board(0)),
            boards
        ));
        assert!(!press_dismisses_panel(
            Some(ToolbarAction::NewBoard),
            boards
        ));
        assert!(
            !press_dismisses_panel(None, boards),
            "a press on the canvas"
        );
        assert!(!press_dismisses_panel(
            Some(ToolbarAction::Tool(Tool::Pen)),
            None
        ));
    }

    /// An open list's walkway stops at the rail's blocks: with the width list open over the boards block's
    /// row, the block answers for itself rather than for the readout that owns the list.
    #[test]
    fn an_open_list_does_not_shadow_the_rail_beside_it() {
        let rail = Toolbar {
            size_open: true,
            rename: None,
            delete_confirm: None,
            ..crate::render::tests::rail_for_legend()
        };
        let (w, h) = (900.0, 1000.0);
        let mut renderer = Renderer::new();
        let position = crate::config::ToolbarPosition::Left;
        let pill = renderer.toolbar_pill(&rail, (w, h), position);
        let boards = pill
            .controls
            .iter()
            .find(|control| control.block.action == Some(ToolbarAction::Boards))
            .expect("the boards block is not on the rail");
        let (ox, oy) = position.origin((w, h), pill.column);
        for x in [4.0, 12.0, 20.0, 28.0] {
            assert_eq!(
                renderer.toolbar_hit(&rail, w, h, ox + x, oy + boards.along + 5.0),
                Some(ToolbarAction::Boards),
                "the width list answered for the boards block at x={x}"
            );
        }
        // The walkway is the gap beside the rail's blocks, over the whole height the list spans: without it
        // the pointer would cross a dead zone on its way from the readout into a row.
        let row_y = |action: ToolbarAction| {
            pill.controls
                .iter()
                .find(|control| control.block.action == Some(action))
                .map(|control| control.along)
                .unwrap_or_else(|| panic!("{action:?} is not on the rail"))
        };
        for y in [row_y(ToolbarAction::Size), boards.along] {
            for x in [TOOLBAR_BLOCK + 1.0, TOOLBAR_DRAWER_X - 1.0] {
                assert_eq!(
                    renderer.toolbar_hit(&rail, w, h, ox + x, oy + y + 5.0),
                    Some(ToolbarAction::Size),
                    "the walkway at x={x}, y={y} is not the list's"
                );
            }
        }
        // And the panel's column is its rows.
        for (step, action) in [
            (ToolbarAction::SizeStep(-1.0), ToolbarAction::SizeStep(-1.0)),
            (ToolbarAction::SizeStep(1.0), ToolbarAction::SizeStep(1.0)),
        ] {
            assert_eq!(
                renderer.toolbar_hit(
                    &rail,
                    w,
                    h,
                    ox + TOOLBAR_DRAWER_X + 6.0,
                    oy + row_y(step) + 5.0
                ),
                Some(action),
                "the panel's column at {step:?} is not its row"
            );
        }
    }

    /// The width list is the second panel no hover state closes; the drawer is the only one left that
    /// leaves with the pointer.
    #[test]
    fn the_width_list_stays_open_when_the_pointer_leaves() {
        for hover in [
            None,
            Some(ToolbarAction::Tool(Tool::Pen)),
            Some(ToolbarAction::Boards),
        ] {
            assert!(
                panel_stays_open(hover, Some(ToolbarAction::Size)),
                "the width list closed with {hover:?} under the pointer"
            );
        }
        assert!(panel_stays_open(None, Some(ToolbarAction::More)));
    }

    /// A width step is a command you may want to repeat: it applies without putting its list away. Every
    /// other pick still closes the panel it came from.
    #[test]
    fn a_width_step_keeps_its_list_open() {
        assert!(!closes_panel_on_pick(ToolbarAction::SizeStep(-1.0)));
        assert!(!closes_panel_on_pick(ToolbarAction::SizeStep(1.0)));
        assert!(closes_panel_on_pick(ToolbarAction::Board(0)));
        assert!(closes_panel_on_pick(ToolbarAction::NewBoard));
        assert!(!closes_panel_on_pick(ToolbarAction::Scrim));
        assert!(!closes_panel_on_pick(ToolbarAction::Backdrop(
            Backdrop::Grid
        )));
        assert!(!closes_panel_on_pick(ToolbarAction::Tool(Tool::Pen)));
    }

    /// The backdrop modes are distinct paintings: nothing, a 24-point grid, a 24-point dot matrix,
    /// and the dim scrim.
    #[test]
    fn every_backdrop_mode_paints_what_it_says() {
        let paint = |mode, view| {
            let mut pixmap = Pixmap::new(120, 120).unwrap();
            paint_backdrop(&mut pixmap.as_mut(), mode, 1.0, view);
            pixmap
        };
        let alpha =
            |pm: &Pixmap, x: usize, y: usize| pm.data()[(y * pm.width() as usize + x) * 4 + 3];
        let origin = View::default();

        assert_eq!(alpha(&paint(Backdrop::Transparent, origin), 24, 24), 0);
        assert_eq!(alpha(&paint(Backdrop::Black, origin), 24, 24), 230);
        assert!(
            alpha(&paint(Backdrop::Grid, origin), 24, 50) > 0,
            "no grid line"
        );
        assert_eq!(
            alpha(&paint(Backdrop::Grid, origin), 40, 50),
            0,
            "grid line too wide"
        );
        assert!(
            alpha(&paint(Backdrop::Dots, origin), 12, 12) > 0,
            "no dot at the first cell"
        );
        assert_eq!(
            alpha(&paint(Backdrop::Dots, origin), 24, 24),
            0,
            "a dot painted on the cell corner"
        );

        // The pattern is canvas paper: a pan moves it and a zoom changes the cell.
        let panned = View {
            zoom: 1.0,
            offset: [10.0, 0.0],
        };
        assert!(
            alpha(&paint(Backdrop::Grid, panned), 10, 50) > 0,
            "the grid did not follow the pan"
        );
        assert!(
            alpha(&paint(Backdrop::Dots, panned), 22, 12) > 0,
            "the dots did not follow the pan"
        );
        let zoomed = View {
            zoom: 2.0,
            offset: [0.0, 0.0],
        };
        assert!(
            alpha(&paint(Backdrop::Grid, zoomed), 48, 50) > 0,
            "the grid did not follow the zoom"
        );
        assert_eq!(
            alpha(&paint(Backdrop::Grid, zoomed), 24, 50),
            0,
            "the grid cell did not double with the zoom"
        );

        assert_eq!(Backdrop::Black.next(), Backdrop::Transparent);
        assert_eq!(Backdrop::Transparent.next(), Backdrop::Grid);
        assert_eq!(Backdrop::Grid.next(), Backdrop::Dots);
        assert_eq!(Backdrop::Dots.next(), Backdrop::Black);
    }

    /// A snapped end reports its target for the accent outline and its world point for the solid
    /// anchor, and both are inside the box the frame has to damage.
    #[test]
    fn a_snap_marks_its_target_and_its_endpoint() {
        let target = Shape {
            kind: ShapeKind::Rect,
            color: "#ffffff".into(),
            width: 3.0,
            from: [50.0, 20.0],
            to: [150.0, 120.0],
            rotate: 0.0,
            id: 7,
            attach: [None, None],
        };
        let mut ann = Board::default();
        ann.add_shape(target.clone());
        let overlay = Overlay {
            shape: Some(Shape {
                kind: ShapeKind::Line,
                color: "#ffffff".into(),
                width: 2.0,
                from: [50.0, 70.0],
                to: [250.0, 70.0],
                rotate: 0.0,
                id: 0,
                attach: [
                    Some(crate::canvas::Attachment {
                        shape: 7,
                        at: [-1.0, 0.0],
                    }),
                    None,
                ],
            }),
            ..Default::default()
        };
        assert_eq!(snap_target_ids(&overlay), vec![7]);
        assert_eq!(snap_anchor_points(&overlay), vec![[50.0, 70.0]]);
        let bounds = snap_hint_bounds(&ann, &overlay).expect("no snap bounds");
        assert!(bounds.contains(50.0, 20.0) && bounds.contains(150.0, 120.0));
        assert!(bounds.contains(50.0, 70.0));

        // The target's outline is repainted in the accent, and the end carries a solid dot.
        let mut buf = vec![0u8; 400 * 200 * 4];
        Renderer::new().render(
            &mut buf,
            400,
            200,
            1.0,
            View::default(),
            &ann,
            &overlay,
            None,
        );
        let pixel = |x: u32, y: u32| {
            let i = ((y * 400 + x) * 4) as usize;
            [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
        };
        assert!(
            pixel(50, 20)[2] > pixel(50, 20)[0],
            "target is not accented"
        );
        assert!(pixel(50, 70)[3] > 0, "the anchor dot is not painted");
    }

    /// A live connector is overlay content: its box has to be part of the damage the frame reports,
    /// or the connector's old stroke stays behind while the shape is dragged.
    #[test]
    fn a_live_connector_joins_the_transient_damage() {
        let connector = Shape {
            kind: ShapeKind::Line,
            color: "#ffffff".into(),
            width: 3.0,
            from: [10.0, 20.0],
            to: [400.0, 200.0],
            rotate: 0.0,
            id: 0,
            attach: [
                Some(crate::canvas::Attachment {
                    shape: 1,
                    at: [1.0, 0.0],
                }),
                None,
            ],
        };
        let overlay = Overlay {
            connectors: vec![LiveConnector::Shape(ItemId::Shape(0), connector.clone())],
            ..Default::default()
        };
        let bounds = transient_bounds(&overlay).expect("a live connector has no box");
        assert!(bounds.contains(10.0, 20.0) && bounds.contains(400.0, 200.0));
        assert!(bounds.w >= connector.from[0].abs() && bounds.h > 0.0);
    }

    /// The four backdrop options are one horizontal row level with the `backdrop` row, and each is
    /// the action that sets its mode.
    #[test]
    fn the_backdrop_options_share_the_rows_height_to_its_right() {
        let mut renderer = Renderer::new();
        let rail = Toolbar {
            more: true,
            ..crate::render::tests::rail_for_legend()
        };
        let position = crate::config::ToolbarPosition::Left;
        let pill = renderer.toolbar_pill(&rail, (900.0, 1000.0), position);
        let row = pill
            .controls
            .iter()
            .find(|control| control.block.action == Some(ToolbarAction::Scrim))
            .expect("the backdrop row");
        let options: Vec<_> = pill
            .controls
            .iter()
            .filter(|control| matches!(control.block.action, Some(ToolbarAction::Backdrop(_))))
            .collect();
        assert_eq!(options.len(), Backdrop::ALL.len());
        for (index, control) in options.iter().enumerate() {
            assert_eq!(control.along, row.along, "an option left the backdrop row");
            assert_eq!(control.block.column, 2 + index as u8);
            assert_eq!(
                control.block.action,
                Some(ToolbarAction::Backdrop(Backdrop::ALL[index]))
            );
        }
        let (ox, oy) = position.origin((900.0, 1000.0), pill.column);
        for (index, mode) in Backdrop::ALL.iter().enumerate() {
            let x = ox + (2.0 + index as f32) * TOOLBAR_DRAWER_X + TOOLBAR_BLOCK / 2.0;
            let y = oy + row.along + TOOLBAR_BLOCK / 2.0;
            assert_eq!(
                renderer.toolbar_hit(&rail, 900.0, 1000.0, x, y),
                Some(ToolbarAction::Backdrop(*mode)),
                "the {mode:?} option has no hit target"
            );
        }
    }

    /// The panel hangs off the block that opens it and never shares that block's band, and its plate is the
    /// box the layout computed with its rows: what is painted and what is hit are one geometry.
    #[test]
    fn the_panel_clears_the_block_that_opens_it() {
        let mut renderer = Renderer::new();
        let rail = |more: bool| Toolbar {
            more,
            ..crate::render::tests::rail_for_legend()
        };
        let position = crate::config::ToolbarPosition::Left;
        let pill = |renderer: &mut Renderer, more: bool| {
            renderer.toolbar_pill(&rail(more), (900.0, 1000.0), position)
        };
        let closed = pill(&mut renderer, false);
        let open = pill(&mut renderer, true);
        assert!(closed.corridor.is_none(), "a shut drawer has no panel");
        let row_y = |pill: &ToolbarPill, action: Option<ToolbarAction>| {
            pill.controls
                .iter()
                .find(|c| c.block.action == action)
                .map(|c| c.along)
        };
        let more = row_y(&open, Some(ToolbarAction::More)).expect("the more block");
        let rows: Vec<f32> = open
            .controls
            .iter()
            .filter(|c| c.block.column == 1)
            .map(|c| c.along)
            .collect();
        assert_eq!(rows.len(), drawer_blocks(&rail(true), &[]).len());
        // One row is level with the block's own row, so the block and its list are one control and the
        // corridor the block's hit box builds leads into a row rather than into an empty strip.
        assert!(
            rows.iter().any(|y| *y <= more && more < *y + TOOLBAR_BLOCK),
            "no row shares the more block's band: {rows:?} against {more}"
        );
        // The plate holds its rows, and both are inside the column and the reserve the rail declares.
        let panel = open.corridor.expect("an open drawer has a panel").0;
        for y in &rows {
            assert!(*y >= panel.along && *y + TOOLBAR_BLOCK <= panel.along + panel.along_extent);
        }
        assert!(panel.across >= 0.0 && panel.across + panel.across_extent <= TOOLBAR_RESERVE);
        assert!(
            panel.along >= 0.0 && panel.along + panel.along_extent <= open.column,
            "the panel's plate left the rail's column"
        );
        // A row states its name and key without the pointer on it: an icon-only row would have to be
        // probed one at a time to be read. The backdrop row is the exception: its four mode buttons
        // beside it are the readable state, so it shrinks to its square.
        for control in open
            .controls
            .iter()
            .filter(|c| c.block.column == 1 && c.block.action != Some(ToolbarAction::Scrim))
        {
            assert!(
                control.across_extent > TOOLBAR_BLOCK,
                "{:?} is icon-only at rest",
                control.block.label
            );
        }
        // And the block that opened the panel takes no state mark for it: the accent means the item that is
        // on in the list it belongs to, and this block is not armed.
        let blocks = toolbar_blocks(&rail(true), 1000.0);
        let more_block = blocks
            .iter()
            .find(|b| b.action == Some(ToolbarAction::More))
            .expect("the more block");
        assert!(!more_block.active, "the more block is accented while open");
        // On the rail, the accent still means the armed tool and the active colour, and nothing else: this
        // block and the panel are not lists of things that can be "on".
        for block in blocks.iter().filter(|b| b.active && b.column == 0) {
            assert!(
                matches!(
                    block.action,
                    Some(ToolbarAction::Tool(_)) | Some(ToolbarAction::Color(_))
                ),
                "{:?} is accented on the rail",
                block.action
            );
        }
        // In the panel the accent marks the backdrop option that is on. The four options sit to the
        // right of the backdrop row, at that row's height.
        let option_active = |mode: Backdrop| {
            open.controls
                .iter()
                .find(|control| control.block.action == Some(ToolbarAction::Backdrop(mode)))
                .map(|control| control.block.active)
        };
        for mode in Backdrop::ALL {
            assert_eq!(
                option_active(mode),
                Some(mode == rail(true).backdrop),
                "the {mode:?} option does not show whether it is on"
            );
        }
        // And the rail's own blocks do not move when the panel opens.
        assert_eq!(
            row_y(&closed, Some(ToolbarAction::More)),
            row_y(&open, Some(ToolbarAction::More)),
            "the more block moved"
        );
        assert_eq!(
            row_y(&closed, Some(ToolbarAction::Lock)),
            row_y(&open, Some(ToolbarAction::Lock)),
            "the rail moved"
        );
    }

    /// The panel lists at most what the rail's box can hold, and always the row that creates a board:
    /// the damage box is fixed, so a document with a hundred boards cannot grow the panel past it.
    #[test]
    fn the_boards_panel_is_capped() {
        let toolbar = Toolbar {
            boards_open: true,
            boards: (0..MAX_BOARD_ROWS + 4)
                .map(|i| crate::canvas::BoardRow {
                    name: format!("board {i}"),
                    current: i == 0,
                })
                .collect(),
            ..crate::render::tests::rail_for_legend()
        };
        let rows = board_rows(&toolbar);
        assert_eq!(rows.len(), MAX_BOARD_ROWS + 1);
        assert_eq!(
            rows.last().and_then(|row| row.action),
            Some(ToolbarAction::NewBoard),
            "the row that creates a board is the last one, so it is the row level with the block"
        );
        assert_eq!(rows[0].action, Some(ToolbarAction::Board(0)));
        assert!(rows[0].active, "the board on screen is marked");
    }

    /// A board's row shows its two buttons only while the pointer is on the row or on one of them: the
    /// row makes room for them, and its own hit stops where they begin.
    #[test]
    fn every_board_row_reveals_its_buttons_under_the_pointer() {
        let boards = vec![
            crate::canvas::BoardRow {
                name: "laptop".into(),
                current: true,
            },
            crate::canvas::BoardRow {
                name: "monitor".into(),
                current: false,
            },
        ];
        let rail = |hover: Option<ToolbarAction>| Toolbar {
            boards_open: true,
            boards: boards.clone(),
            hover,
            ..crate::render::tests::rail_for_legend()
        };
        // The row's box and its screen origin, from the layout the frame draws.
        let row = |toolbar: &Toolbar| {
            let mut renderer = Renderer::new();
            let position = crate::config::ToolbarPosition::Left;
            let pill = renderer.toolbar_pill(toolbar, (900.0, 1000.0), position);
            let control = pill
                .controls
                .iter()
                .find(|control| control.block.action == Some(ToolbarAction::Board(0)))
                .expect("no row for the first board")
                .clone();
            let (ox, oy) = position.origin((900.0, 1000.0), pill.column);
            (
                ox + TOOLBAR_DRAWER_X,
                oy + control.along,
                control.across_extent,
            )
        };
        let hit = |toolbar: &Toolbar, x: f32, y: f32| {
            let mut renderer = Renderer::new();
            renderer.toolbar_hit(toolbar, 900.0, 1000.0, x, y)
        };
        // Away from the row: nothing to press beside the name.
        let away = rail(None);
        let (x, y, w) = row(&away);
        assert_eq!(hit(&away, x + 8.0, y + 8.0), Some(ToolbarAction::Board(0)));
        assert_eq!(hit(&away, x + w + 8.0, y + 8.0), None);
        // On the row: the two buttons at its right end, and the name's own box stops before them.
        let on = rail(Some(ToolbarAction::Board(0)));
        let (x, y, w) = row(&on);
        assert_eq!(
            hit(&on, x + w - 2.0 * TOOLBAR_BLOCK + 8.0, y + 8.0),
            Some(ToolbarAction::AskDeleteBoard(0))
        );
        assert_eq!(
            hit(&on, x + w - TOOLBAR_BLOCK + 8.0, y + 8.0),
            Some(ToolbarAction::RenameBoard(0))
        );
        assert_eq!(
            hit(&on, x + w - 2.5 * TOOLBAR_BLOCK, y + 8.0),
            Some(ToolbarAction::Board(0)),
            "the name's box did not stop before the buttons"
        );
        // On a button: both stay up, or the row would flicker as the pointer reached for them.
        let held = rail(Some(ToolbarAction::AskDeleteBoard(0)));
        let (x, y, w) = row(&held);
        assert_eq!(
            hit(&held, x + w - TOOLBAR_BLOCK + 8.0, y + 8.0),
            Some(ToolbarAction::RenameBoard(0)),
            "the buttons went away as the pointer reached one"
        );
        assert!(board_actions(&held, 0).is_some());
        assert!(board_actions(&rail(None), 0).is_none());

        // Once the X is pressed the row asks in place: its two slots become `no` and `yes`, and
        // they answer even with the pointer nowhere near the row.
        let asking = Toolbar {
            delete_confirm: Some("laptop".into()),
            ..rail(None)
        };
        let (x, y, w) = row(&asking);
        assert_eq!(
            hit(&asking, x + w - 2.0 * TOOLBAR_BLOCK + 8.0, y + 8.0),
            Some(ToolbarAction::CancelDeleteBoard)
        );
        assert_eq!(
            hit(&asking, x + w - TOOLBAR_BLOCK + 8.0, y + 8.0),
            Some(ToolbarAction::DeleteBoard(0))
        );
        assert!(board_actions(&asking, 0).is_some());
    }

    /// A document's last board has nothing to delete: its row shows the rename alone.
    #[test]
    fn the_delete_button_is_absent_on_the_only_board() {
        let toolbar = Toolbar {
            boards_open: true,
            boards: vec![crate::canvas::BoardRow {
                name: "laptop".into(),
                current: true,
            }],
            hover: Some(ToolbarAction::Board(0)),
            ..crate::render::tests::rail_for_legend()
        };
        assert_eq!(board_actions(&toolbar, 0), Some((false, true)));
    }

    /// The question replaces the name in the row itself, with the two button slots now meaning
    /// `no` and `yes`.
    #[test]
    fn a_delete_question_uses_the_board_row_itself() {
        let boards = vec![crate::canvas::BoardRow {
            name: "laptop".into(),
            current: true,
        }];
        let toolbar = Toolbar {
            boards_open: true,
            boards,
            delete_confirm: Some("laptop".into()),
            ..crate::render::tests::rail_for_legend()
        };
        let rows = board_rows(&toolbar);
        assert_eq!(rows[0].label, "delete laptop?");
        assert_eq!(rows[0].action, Some(ToolbarAction::Board(0)));
        assert_eq!(board_actions(&toolbar, 0), Some((true, true)));
        // The cached layout must see the question: the confirmation changes the row's label and
        // reserved button slots even when the pointer did not move.
        let plain = Toolbar {
            delete_confirm: None,
            ..toolbar.clone()
        };
        assert!(!same_layout(&toolbar, &plain));
    }

    /// Delete is a pick that closes the list; the rename is the one row press that leaves it open,
    /// because the row it edits is in the list.
    #[test]
    fn a_rename_press_keeps_its_list_and_a_delete_press_closes_it() {
        assert!(picks_from_panel(ToolbarAction::DeleteBoard(0)));
        assert!(picks_from_panel(ToolbarAction::RenameBoard(0)));
        assert!(picks_from_panel(ToolbarAction::AskDeleteBoard(0)));
        assert!(picks_from_panel(ToolbarAction::CancelDeleteBoard));
        assert!(closes_panel_on_pick(ToolbarAction::DeleteBoard(0)));
        assert!(!closes_panel_on_pick(ToolbarAction::RenameBoard(0)));
        assert!(
            !closes_panel_on_pick(ToolbarAction::AskDeleteBoard(0)),
            "asking must leave the row in place"
        );
        assert!(!closes_panel_on_pick(ToolbarAction::CancelDeleteBoard));
        assert!(panel_stays_open(
            Some(ToolbarAction::RenameBoard(0)),
            Some(ToolbarAction::Boards)
        ));
    }

    /// The row being edited draws the buffer, not the name it started from.
    #[test]
    fn the_edited_row_draws_the_buffer_instead_of_its_name() {
        let mut toolbar = Toolbar {
            boards_open: true,
            boards: vec![crate::canvas::BoardRow {
                name: "laptop".into(),
                current: true,
            }],
            ..crate::render::tests::rail_for_legend()
        };
        let row = board_rows(&toolbar)
            .into_iter()
            .find(|row| row.action == Some(ToolbarAction::Board(0)))
            .expect("no row for the first board");
        assert_eq!(row_label(&toolbar, &row), "laptop");
        toolbar.rename = Some(crate::canvas::BoardEdit {
            index: 0,
            text: "left".into(),
            cursor: 4,
            preedit: String::new(),
            caret: true,
        });
        assert_eq!(row_label(&toolbar, &row), "left");
    }

    /// The block whose list is open keeps its square: the list says what it is, so a caption growing
    /// from the block would push against the list's own rows.
    #[test]
    fn the_block_that_opened_a_panel_does_not_stretch() {
        let width = |boards_open: bool| {
            let toolbar = Toolbar {
                boards_open,
                boards: vec![crate::canvas::BoardRow {
                    name: "laptop".into(),
                    current: true,
                }],
                hover: Some(ToolbarAction::Boards),
                ..crate::render::tests::rail_for_legend()
            };
            let mut renderer = Renderer::new();
            let pill = renderer.toolbar_pill(
                &toolbar,
                (900.0, 1000.0),
                crate::config::ToolbarPosition::Left,
            );
            pill.controls
                .iter()
                .find(|control| control.block.action == Some(ToolbarAction::Boards))
                .expect("no block for the boards list")
                .across_extent
        };
        assert_eq!(width(true), TOOLBAR_BLOCK, "the open block grew a caption");
        assert!(
            width(false) > TOOLBAR_BLOCK,
            "the block under the pointer did not say what it does"
        );
    }

    /// The row an edit is in is measured from what is typed, and a document's only board makes room
    /// for its rename alone: the capsule and the name's clip follow the text, not the name it started
    /// from.
    #[test]
    fn a_row_being_edited_is_measured_from_its_buffer() {
        let row = |text: Option<&str>, boards: usize| {
            let mut toolbar = Toolbar {
                boards_open: true,
                boards: (0..boards)
                    .map(|i| crate::canvas::BoardRow {
                        name: format!("Board {i}"),
                        current: i == 0,
                    })
                    .collect(),
                hover: Some(ToolbarAction::Board(0)),
                ..crate::render::tests::rail_for_legend()
            };
            toolbar.rename = text.map(|text| crate::canvas::BoardEdit {
                index: 0,
                text: text.into(),
                cursor: text.chars().count(),
                preedit: String::new(),
                caret: true,
            });
            let mut renderer = Renderer::new();
            let pill = renderer.toolbar_pill(
                &toolbar,
                (900.0, 1000.0),
                crate::config::ToolbarPosition::Left,
            );
            pill.controls
                .iter()
                .find(|control| control.block.action == Some(ToolbarAction::Board(0)))
                .expect("no row for the first board")
                .across_extent
        };
        let short = row(Some("Board 1"), 2);
        let long = row(Some("a much longer name"), 2);
        assert!(
            long > short,
            "the row was measured from the name it started from"
        );
        // One board, one button: no empty slot where a delete would be.
        assert_eq!(
            row(None, 2) - row(None, 1),
            TOOLBAR_BLOCK,
            "the last board's row kept room for a button it does not show"
        );
    }

    /// A different list of boards is a different rail: reusing the previous layout would draw the panel
    /// with the rows of a board that is no longer there, and make those rows unclickable.
    #[test]
    fn a_changed_board_list_is_a_different_layout() {
        let rail = |name: &str, open: bool| Toolbar {
            boards_open: open,
            boards: vec![crate::canvas::BoardRow {
                name: name.into(),
                current: true,
            }],
            ..crate::render::tests::rail_for_legend()
        };
        assert!(same_layout(&rail("office", true), &rail("office", true)));
        assert!(
            !same_layout(&rail("office", true), &rail("desk", true)),
            "a renamed list reused the layout"
        );
        assert!(
            !same_layout(&rail("office", true), &rail("office", false)),
            "a shut panel reused the open one's layout"
        );
    }

    /// The rail is as tall as the controls that fit: the palette, the tools and the size group fill from the
    /// top while there is room, the foot always stays, and what does not fit is a row of the drawer.
    #[test]
    fn a_short_output_folds_its_tools_into_the_drawer() {
        let rail = crate::render::tests::rail_for_legend();
        let (tall, nothing_folded) = fitted_blocks(&rail, 1000.0);
        assert!(
            nothing_folded.is_empty(),
            "a tall output folded {} blocks",
            nothing_folded.len()
        );
        let (short, folded) = fitted_blocks(&rail, 500.0);
        assert!(!folded.is_empty(), "a 500 pt output folded nothing");
        assert!(
            column_of(&short) < column_of(&tall),
            "the rail did not get shorter"
        );
        // The foot is the way in and the way out, so it never folds.
        for action in [
            ToolbarAction::Boards,
            ToolbarAction::More,
            ToolbarAction::Lock,
        ] {
            assert!(
                short.iter().any(|block| block.action == Some(action)),
                "{action:?} left the rail"
            );
        }
        // A folded block is an ordinary block in the second column, so it is one of the drawer's rows.
        let drawer = drawer_blocks(
            &Toolbar {
                more: true,
                ..rail.clone()
            },
            &folded,
        );
        assert_eq!(drawer.len(), DRAWER.len() + folded.len());
        for block in &folded {
            assert!(
                drawer.iter().any(|row| row.action == block.action),
                "{:?} folded and is in no row",
                block.action
            );
        }
        assert!(
            drawer.iter().all(|row| row.column == 1),
            "a folded block was drawn on the rail"
        );
    }

    /// A row of names states the name and nothing else: a glyph repeated beside every board is an icon the
    /// reader has to identify before they can read the list, and it was covering the first glyphs of the
    /// name besides. The row that creates a board keeps its plus, which does say what it does.
    #[test]
    fn a_row_of_boards_states_its_name_and_no_icon() {
        let toolbar = Toolbar {
            boards_open: true,
            boards: vec![crate::canvas::BoardRow {
                name: "office".into(),
                current: true,
            }],
            ..crate::render::tests::rail_for_legend()
        };
        let rows = board_rows(&toolbar);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].mark, Mark::None, "a board row draws an icon");
        assert_eq!(
            rows[1].mark,
            Mark::Icon(Icon::Plus),
            "the row that creates a board lost its plus"
        );
    }

    /// A block's text starts after that block's own square — in the panel's column too, where measuring from
    /// the rail's own edge put every name inside its icon — and a row of names, which has no square, starts
    /// a pad inside its own capsule so the name is centred in it.
    #[test]
    fn a_labels_text_starts_after_its_own_block() {
        let left = crate::config::ToolbarPosition::Left;
        let capsule = |x| Rect {
            x,
            y: 0.0,
            w: TOOLBAR_BLOCK,
            h: TOOLBAR_BLOCK,
        };
        assert_eq!(
            label_origin(left, capsule(0.0), false, 20.0),
            TOOLBAR_BLOCK + TOOLBAR_LABEL_GAP
        );
        assert!(
            label_origin(left, capsule(TOOLBAR_DRAWER_X), false, 20.0)
                >= TOOLBAR_DRAWER_X + TOOLBAR_BLOCK,
            "a panel row's text starts inside its own square"
        );
        assert_eq!(
            label_origin(left, capsule(TOOLBAR_DRAWER_X), true, 20.0),
            TOOLBAR_DRAWER_X + TOOLBAR_LABEL_PAD
        );
        let right = crate::config::ToolbarPosition::Right;
        assert_eq!(
            label_origin(
                right,
                Rect {
                    x: 100.0,
                    y: 0.0,
                    w: 140.0,
                    h: TOOLBAR_BLOCK,
                },
                false,
                20.0
            ),
            100.0 + 140.0 - TOOLBAR_BLOCK - TOOLBAR_LABEL_GAP - 20.0,
            "a right rail's label ends before its square"
        );
    }

    /// The `more` block reports the drawer's own controls, and only while the drawer is shut: its one
    /// list is the drawer's, so the two cannot disagree. The legend uses the one-word captions because
    /// four full labels and keys do not fit the line the rail reserves for it.
    #[test]
    fn the_more_block_lists_what_is_in_the_drawer() {
        let rail = |more: bool| Toolbar {
            more,
            ..crate::render::tests::rail_for_legend()
        };
        let closed = drawer_legend(&rail(false), &[]).expect("a list while the drawer is shut");
        let residents: Vec<String> = drawer_blocks(&rail(false), &[])
            .iter()
            .filter_map(|block| block.action.and_then(drawer_caption))
            .map(str::to_string)
            .collect();
        assert!(!residents.is_empty(), "the drawer has no residents");
        for name in residents {
            assert!(closed.contains(&name), "{closed} does not name {name}");
        }
        assert_eq!(
            drawer_legend(&rail(true), &[]),
            None,
            "the list repeated the drawer"
        );
    }

    /// The rail with the pen armed, for the tests that only need a toolbar to look at.
    fn rail_for_legend() -> Toolbar {
        Toolbar {
            tool: Tool::Pen,
            color: crate::canvas::PALETTE[0].to_string(),
            size: 3.0,
            hover: None,
            grow: 0.0,
            panel_grow: 1.0,
            more: false,
            boards_open: false,
            size_open: false,
            clipboard_open: false,
            clips: Vec::new(),
            target: None,
            rename: None,
            delete_confirm: None,
            boards: Vec::new(),
            shared: false,
            backdrop: Backdrop::Black,
            zoom: 1.0,
            can_undo: false,
            can_redo: false,
        }
    }

    /// The identity view at a given output scale, for the tests that only care about the document., so a switch of the backdrop cannot reuse it.
    #[test]
    fn the_raster_is_rebuilt_when_the_backdrop_changes() {
        let key = |backdrop: Backdrop| BaseKey {
            width: 100,
            height: 100,
            scale: 1.0,
            view: View::default(),
            skip: vec![],
            backdrop,
        };
        // `BaseKey` is what the cached raster's identity is made of, so a different backing is a
        // different raster.
        assert!(!key(Backdrop::Black).eq(&key(Backdrop::Transparent)));
        assert!(key(Backdrop::Black).eq(&key(Backdrop::Black)));
    }

    /// The caret of a wrapped layout: the offset of each line's own text has to land on that line, or
    /// the caret sticks at the end of the last one.
    #[test]
    fn the_caret_follows_the_line_a_wrapped_offset_is_on() {
        let mut renderer = Renderer::new();
        let screen = Screen {
            scale: 1.0,
            view: View::default(),
        };
        let mut note = Note::new(0.0, 0.0, "#f6d32d", "2026/10/14".into());
        note.w = 200.0;
        let text = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda";
        let prepared =
            renderer.prepare_text(text, &note.text_item(), screen, Some(note.text_box().w));
        let lines: Vec<(usize, f32, f32)> = prepared
            .buffer
            .layout_runs()
            .map(|run| {
                (
                    run.glyphs.last().map_or(0, |glyph| glyph.end),
                    run.line_top,
                    run.line_w,
                )
            })
            .collect();
        assert!(lines.len() > 2, "the fixture did not wrap: {}", lines.len());
        let mut tops: Vec<f32> = lines.iter().map(|(_, top, _)| *top).collect();
        tops.dedup();
        assert_eq!(tops.len(), lines.len(), "the fixture's lines share a top");
        for (end, top, width) in lines {
            let caret = caret_geometry(&prepared, end, 0);
            assert_eq!(
                caret.top, top,
                "the caret at byte {end} was drawn on another line"
            );
            assert!(
                caret.x <= width + 0.5 && caret.x <= note.text_box().w + 0.5,
                "the caret at byte {end} went past its line: {} > {width}",
                caret.x
            );
        }
    }

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
            link: None,
        }
    }

    fn render_cached(
        renderer: &mut Renderer,
        cache: &mut FrameCache,
        annotations: &Board,
        buffer: &mut [u8],
    ) {
        renderer.render_with_cache(
            buffer,
            200,
            100,
            1.0,
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
    fn document() -> Board {
        Board {
            strokes: vec![Stroke {
                color: "#e01b24".into(),
                width: 4.0,
                points: vec![[10.0, 50.0], [190.0, 60.0]],
                attach: [None, None],
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
        let annotations = Board {
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

    /// The shapes the renderer draws a document into, which a test needs when it wants the raster
    /// path rather than the whole-surface one.
    fn render_damaged(
        renderer: &mut Renderer,
        cache: &mut FrameCache,
        annotations: &Board,
        buffer: &mut [u8],
    ) {
        renderer.render_with_cache(
            buffer,
            200,
            100,
            1.0,
            1.0,
            View::default(),
            annotations,
            &Overlay::default(),
            Some(crate::canvas::Rect {
                x: 0.0,
                y: 0.0,
                w: 200.0,
                h: 100.0,
            }),
            cache,
            &[],
        );
    }

    #[test]
    fn the_document_raster_is_reused_until_it_is_invalidated() {
        // What a frame costs stands or falls on this: a localized frame builds the raster and the
        // next one fills from it instead of rasterising the document again. A whole-surface frame
        // deliberately keeps no raster — it draws the document into the buffer instead — so the
        // reuse this test is about is the localized frame's.
        let mut renderer = Renderer::new();
        let mut cache = FrameCache::default();
        let annotations = document();
        let mut first = vec![0; 200 * 100 * 4];
        render_damaged(&mut renderer, &mut cache, &annotations, &mut first);
        assert!(cache.base.matches(&BaseKey {
            width: 200,
            height: 100,
            scale: 1.0,
            view: View::default(),
            skip: Vec::new(),
            backdrop: Backdrop::Transparent,
        }));

        let mut second = vec![0; 200 * 100 * 4];
        render_damaged(&mut renderer, &mut cache, &annotations, &mut second);
        assert_eq!(first, second, "a reused raster draws the same pixels");

        // Invalidating drops it, and the frame that follows is built from the document again.
        cache.invalidate();
        assert!(cache.base.key.is_none());
        let mut rebuilt = vec![0; 200 * 100 * 4];
        render_damaged(&mut renderer, &mut cache, &annotations, &mut rebuilt);
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
        let mut annotations = Board {
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
        let annotations = Board::default();
        let overlay = Overlay {
            toolbar: Some(Toolbar {
                tool: Tool::Pen,
                color: "#e01b24".to_string(),
                size: 3.0,
                hover: None,
                grow: 1.0,
                panel_grow: 1.0,
                more: false,
                boards_open: false,
                size_open: false,
                clipboard_open: false,
                clips: Vec::new(),
                target: None,
                rename: None,
                delete_confirm: None,
                boards: Vec::new(),
                shared: false,
                backdrop: Backdrop::Black,
                zoom: 1.0,
                can_undo: false,
                can_redo: false,
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
        let annotations = Board::default();
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
        let old_document = Board {
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
            &Board::default(),
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
            1.0,
            View::default(),
            &moved,
            &settled,
            None,
            &mut FrameCache::default(),
            &[],
        );
        // The two marks are the same pen over the same path, but not the same mark: the live one
        // wanders from where the element started and the committed one from where it landed, so they
        // differ by their wander and settle when the gesture is released. What must not differ is
        // where the ink is and how much of it there is — twice the ink would be a double draw.
        let ink = |buf: &[u8]| buf.as_chunks::<4>().0.iter().filter(|p| p[3] != 0).count();
        let ink_box = |buf: &[u8]| {
            let mut out: Option<(u32, u32, u32, u32)> = None;
            for y in 0..100u32 {
                for x in 0..200u32 {
                    if buf[((y * 200 + x) * 4 + 3) as usize] == 0 {
                        continue;
                    }
                    out = Some(match out {
                        None => (x, y, x, y),
                        Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                    });
                }
            }
            out.expect("something was painted")
        };
        assert_eq!(ink_box(&dragged), ink_box(&committed));
        let (live, settled) = (ink(&dragged), ink(&committed));
        let off = live.abs_diff(settled);
        assert!(
            off * 10 <= settled,
            "the live frame painted {live} to the committed {settled}"
        );
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
            rotate: 0.0,
            id: 0,
            attach: [None, None],
        };
        let mut renderer = Renderer::new();
        let stored = Board {
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
            color: crate::canvas::PALETTE[0].to_string(),
            size: 3.0,
            hover: None,
            grow: 1.0,
            panel_grow: 1.0,
            more: false,
            boards_open: false,
            size_open: false,
            clipboard_open: false,
            clips: Vec::new(),
            target: None,
            rename: None,
            delete_confirm: None,
            boards: Vec::new(),
            shared: false,
            backdrop: Backdrop::Black,
            zoom: 1.0,
            can_undo: false,
            can_redo: false,
        }
    }

    #[test]
    fn every_block_draws_an_icon_in_its_square() {
        // Every function's block shows a picture of what it does, at scale 1 and at scale 2: a
        // 30 pt block with a 1.5 pt stroke has to carry ink at both.
        let rail = rail_state();
        for scale in [1.0, 2.0] {
            let rail = rail.clone();
            // Logical size, and the device buffer the renderer is given for it.
            let (w, h) = (640.0, 1600.0);
            let (wd, hd) = ((w * scale) as u32, (h * scale) as u32);
            let mut renderer = Renderer::new();
            let position = crate::config::ToolbarPosition::Left;
            let pill = renderer
                .cached_toolbar_pill(&rail, (w, h), position)
                .unwrap();
            let mut buf = vec![0; (wd * hd * 4) as usize];
            renderer.render(
                &mut buf,
                wd,
                hd,
                scale,
                View::default(),
                &Board::default(),
                &Overlay {
                    toolbar: Some(rail),
                    ..Default::default()
                },
                None,
            );
            let (ox, oy) = position.origin((w, h), pill.column);
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
                    ((oy + control.along + 3.0) * scale) as u32,
                );
                let (x1, y1) = (
                    ((ox + TOOLBAR_BLOCK - 3.0) * scale) as u32,
                    ((oy + control.along + TOOLBAR_BLOCK - 3.0) * scale) as u32,
                );
                let ink = (y0..y1)
                    .flat_map(|y| (x0..x1).map(move |x| (x, y)))
                    .filter(|(x, y)| near_white(&buf, wd, *x, *y))
                    .count();
                assert!(ink > 0, "{icon:?} drew nothing at scale {scale}");
            }
            assert_eq!(
                icons, 14,
                "every function has an icon, and only functions do"
            );
        }
    }

    #[test]
    fn the_pen_icon_is_the_exact_figure_and_not_a_mark() {
        // The rail is interface, so its icon is the tool's own drawing in the exact ink. This compares
        // the icon against that exact drawing: routing the icon through `Ink::Sketch` anywhere would
        // make the two disagree.
        let (scale, square) = (
            1.0,
            Rect {
                x: 10.0,
                y: 10.0,
                w: TOOLBAR_BLOCK,
                h: TOOLBAR_BLOCK,
            },
        );
        let mut icon = Pixmap::new(60, 60).unwrap();
        draw_icon(
            &mut icon.as_mut(),
            Icon::Pen,
            square,
            scale,
            "#ffffffee",
            false,
        );
        let l = square.grown(-ICON_INSET * scale);
        let mut exact = Pixmap::new(60, 60).unwrap();
        draw_stroke(
            &mut exact.as_mut(),
            &Stroke {
                color: "#ffffffee".into(),
                width: ICON_STROKE,
                points: vec![
                    [l.x + l.w * 0.05, l.y + l.h * 0.8],
                    [l.x + l.w * 0.35, l.y + l.h * 0.25],
                    [l.x + l.w * 0.65, l.y + l.h * 0.75],
                    [l.x + l.w * 0.95, l.y + l.h * 0.2],
                ],
                attach: [None, None],
            },
            Screen {
                scale,
                view: View::default(),
            },
            Ink::Precise,
        );
        assert_eq!(icon.data(), exact.data());
    }

    /// The width list is the two steps, in reading order, and the readout itself is a control that says
    /// nothing: it has no key, so its label is empty and its mark is the value.
    #[test]
    fn the_width_list_is_the_two_steps() {
        let toolbar = Toolbar {
            size_open: true,
            rename: None,
            delete_confirm: None,
            ..crate::render::tests::rail_for_legend()
        };
        let rows = size_rows(&toolbar);
        let actions: Vec<Option<ToolbarAction>> = rows.iter().map(|row| row.action).collect();
        assert_eq!(
            actions,
            vec![
                Some(ToolbarAction::SizeStep(-1.0)),
                Some(ToolbarAction::SizeStep(1.0))
            ]
        );
        assert_eq!(rows[0].label, "smaller width ([)");
        assert!(rows.iter().all(|row| row.column == 1));
        assert!(picks_from_panel(ToolbarAction::SizeStep(1.0)));
        assert!(!picks_from_panel(ToolbarAction::Size));
    }

    /// The readout is a rail block with an action and no label: a click opens the list, so it must be
    /// hit and it must not stretch into a capsule with nothing in it.
    #[test]
    fn the_width_readout_is_a_control_with_no_label() {
        let blocks = toolbar_blocks(&crate::render::tests::rail_for_legend(), 1000.0);
        let readout = blocks
            .iter()
            .find(|block| matches!(block.mark, Mark::Value(_)))
            .expect("the rail has no width readout");
        assert_eq!(readout.action, Some(ToolbarAction::Size));
        assert!(readout.label.is_empty());
    }

    #[test]
    fn every_block_states_the_key_that_applies_it() {
        // A block's face carries its primary key, and its stretched text ends with the full key —
        // and that key has to be one this daemon actually handles.
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
            "V" => Some(Keysym::V),
            "O" => Some(Keysym::O),
            "R" => Some(Keysym::R),
            "D" => Some(Keysym::D),
            "L" => Some(Keysym::L),
            "A" => Some(Keysym::A),
            "H" => Some(Keysym::H),
            "N" => Some(Keysym::N),
            "M" => Some(Keysym::M),
            "C" => Some(Keysym::C),
            "F" => Some(Keysym::F),
            _ => None,
        };
        let blocks = toolbar_blocks(&rail_state(), 1000.0);
        assert_eq!(blocks.len(), 20);
        let mut stated = 0;
        for block in &blocks {
            let Some(action) = block.action else {
                // An actionless block can only be a readout: it shows a value and says nothing.
                assert!(block.label.is_empty());
                assert!(matches!(block.mark, Mark::Value(_)));
                continue;
            };
            let Some(key) = key_of(action) else {
                // A control with no key states no key: the width readout opens its list on hover.
                assert!(
                    block.label.is_empty(),
                    "'{}' states a key it has not",
                    block.label
                );
                assert!(matches!(block.mark, Mark::Value(_)));
                continue;
            };
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
                ToolbarAction::Boards => {
                    panel_action_for_keysym(keysym) == Some(ToolbarAction::Boards)
                }
                ToolbarAction::Shared => {
                    panel_action_for_keysym(keysym) == Some(ToolbarAction::Shared)
                }
                // A row of the boards panel states its name and no key: it is picked from the list the
                // `boards` block opened, and that block carries the key.
                ToolbarAction::Board(_)
                | ToolbarAction::NewBoard
                | ToolbarAction::DeleteBoard(_)
                | ToolbarAction::RenameBoard(_) => keysym == Keysym::C || keysym == Keysym::c,
                // These live in the drawer (closed here) or in the clipboard list, and name no
                // single key: their block labels are never walked by this test.
                ToolbarAction::Link
                | ToolbarAction::Clipboard
                | ToolbarAction::Clip(_)
                | ToolbarAction::AskDeleteBoard(_)
                | ToolbarAction::CancelDeleteBoard
                | ToolbarAction::Undo
                | ToolbarAction::Redo => false,
                ToolbarAction::Backdrop(_) => false,
                ToolbarAction::Size => unreachable!("no key, filtered above"),
            };
            assert!(applies, "'{}' names a key that does not do it", block.label);
        }
        // Nineteen blocks apply something: five colours, eleven tools, boards, the drawer's control and
        // lock. Both panel switches are in the drawer, which is closed in this rail, and the width readout
        // applies nothing of its own.
        assert_eq!(stated, 19);
    }

    /// A face mark is a lone key. The capsule names the full key, a chord and any alternate included.
    #[test]
    fn a_block_badge_names_its_primary_key_only() {
        assert_eq!(badge_of(ToolbarAction::Tool(Tool::Pen)), Some("P"));
        assert_eq!(badge_of(ToolbarAction::Tool(Tool::Selection)), Some("V"));
        assert_eq!(badge_of(ToolbarAction::Tool(Tool::Text)), Some("T"));
        assert_eq!(badge_of(ToolbarAction::Color(0)), Some("1"));
        assert_eq!(badge_of(ToolbarAction::SizeStep(-1.0)), Some("["));
        assert_eq!(badge_of(ToolbarAction::SizeStep(1.0)), Some("]"));
        assert_eq!(badge_of(ToolbarAction::Lock), Some("Esc"));
        assert_eq!(badge_of(ToolbarAction::More), Some("M"));
        assert_eq!(badge_of(ToolbarAction::Shared), Some("F"));
        // A chord has no letter of its own to wear, and a pick out of a panel names the key that
        // opened the list rather than one of its own.
        assert_eq!(badge_of(ToolbarAction::Clipboard), None);
        assert_eq!(badge_of(ToolbarAction::Link), None);
        assert_eq!(badge_of(ToolbarAction::Size), None);
        assert_eq!(badge_of(ToolbarAction::Board(0)), None);
        assert_eq!(badge_of(ToolbarAction::Backdrop(Backdrop::Grid)), None);
        // Whatever the face shows is exactly what the stretched text ends with.
        for action in [
            ToolbarAction::Tool(Tool::Selection),
            ToolbarAction::Color(2),
            ToolbarAction::Lock,
        ] {
            let key = badge_of(action).expect("a badged action");
            assert!(labelled(action, "does it").ends_with(&format!("({key})")));
        }
        // The capsule still teaches a key the square cannot abbreviate.
        assert_eq!(
            labelled(ToolbarAction::Clipboard, "clipboard history"),
            "clipboard history (Ctrl+Shift+V)"
        );
    }

    /// The badge and the icon are drawn in the same 30 pt square, so their boxes must stay apart.
    #[test]
    fn a_key_badge_does_not_reach_its_icon() {
        let mut renderer = Renderer::new();
        for key in ["P", "1", "Esc", "[", "]"] {
            for scale in [1.0, 2.0] {
                let square = Rect {
                    x: 10.0 * scale,
                    y: 10.0 * scale,
                    w: TOOLBAR_BLOCK * scale,
                    h: TOOLBAR_BLOCK * scale,
                };
                let width = renderer.text_width(key, BADGE_TEXT_SIZE);
                let badge = badge_box(square, scale, width);
                let icon = icon_box(square, true, scale);
                // The icon is stroked about its box; even that reach must stop short of the glyph.
                let stroke = icon.grown(ICON_STROKE * scale / 2.0);
                assert!(
                    !badge.intersects(stroke),
                    "'{key}' badge {badge:?} meets its icon {stroke:?} at scale {scale}"
                );
                assert!(square.contains(badge.x, badge.y));
                assert!(square.contains(badge.x + badge.w, badge.y + badge.h));
            }
        }
    }

    /// The eraser is a slanted block with a band, not the empty rounded square that read as a
    /// second rectangle tool.
    #[test]
    fn the_history_icons_paint_a_solid_arrow() {
        for icon in [Icon::Undo, Icon::Redo] {
            let mut pixmap = Pixmap::new(60, 60).unwrap();
            draw_icon(
                &mut pixmap.as_mut(),
                icon,
                Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 60.0,
                    h: 60.0,
                },
                1.0,
                "#ffffffff",
                false,
            );
            let painted = pixmap
                .data()
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|pixel| pixel[3] > 200)
                .count();
            assert!(painted > 40, "{icon:?} painted only {painted} pixels");
        }
    }

    #[test]
    fn the_eraser_icon_is_a_slanted_block_with_a_band() {
        let square = Rect {
            x: 10.0,
            y: 10.0,
            w: TOOLBAR_BLOCK,
            h: TOOLBAR_BLOCK,
        };
        let box_ = icon_box(square, false, 1.0);
        let (corners, band) = eraser_icon(box_);
        // The figure lies at an angle: its highest corner is on the right of the box and its
        // lowest on the left.
        let top = corners
            .iter()
            .min_by(|a, b| a[1].partial_cmp(&b[1]).unwrap())
            .unwrap();
        let bottom = corners
            .iter()
            .max_by(|a, b| a[1].partial_cmp(&b[1]).unwrap())
            .unwrap();
        let middle = box_.x + box_.w / 2.0;
        assert!(top[0] > middle, "the block does not slant up to the right");
        assert!(
            bottom[0] < middle,
            "the block does not slant up to the right"
        );
        for corner in corners {
            assert!(box_.contains(corner[0], corner[1]));
        }
        // The band is drawn across the block, so the block's middle carries ink the old hollow
        // square left blank.
        let mut icon = Pixmap::new(60, 60).unwrap();
        draw_icon(
            &mut icon.as_mut(),
            Icon::Eraser,
            square,
            1.0,
            "#ffffffff",
            false,
        );
        let mid = [
            (band[0][0] + band[1][0]) / 2.0,
            (band[0][1] + band[1][1]) / 2.0,
        ];
        assert!(
            near_white(icon.data(), 60, mid[0] as u32, mid[1] as u32),
            "the band is missing"
        );
    }

    #[test]
    fn cards_sit_in_each_configured_slot_and_stay_on_screen() {
        let surface = (900.0, 1000.0);
        for position in [
            CardPosition::TopLeft,
            CardPosition::TopRight,
            CardPosition::TopCenter,
            CardPosition::BottomLeft,
            CardPosition::BottomRight,
            CardPosition::BottomCenter,
        ] {
            let cards = cards_layout(surface, position);
            assert_eq!(cards.position, position);
            for rect in [cards.zoom, cards.history, cards.undo, cards.redo] {
                assert!(rect.x >= 0.0 && rect.y >= 0.0);
                assert!(rect.x + rect.w <= surface.0);
                assert!(rect.y + rect.h <= surface.1);
            }
            assert_eq!(cards.history.x - (cards.zoom.x + cards.zoom.w), TOOLBAR_GAP);
            if position.left() {
                assert_eq!(cards.zoom.x, TOOLBAR_MARGIN);
            }
            if position.top() {
                assert_eq!(cards.zoom.y, TOOLBAR_MARGIN);
            }
        }
    }

    #[test]
    fn the_history_cards_hit_only_when_their_action_is_available() {
        let mut renderer = Renderer::new();
        let mut toolbar = crate::render::tests::rail_for_legend();
        toolbar.can_undo = true;
        toolbar.can_redo = true;
        let surface = (900.0, 1000.0);
        let cards = cards_layout(surface, CardPosition::BottomRight);
        let centre = |r: Rect| (r.x + r.w / 2.0, r.y + r.h / 2.0);
        let (x, y) = centre(cards.undo);
        assert_eq!(
            renderer.toolbar_hit_at(ToolbarPosition::Left, &toolbar, surface.0, surface.1, x, y),
            Some(ToolbarAction::Undo)
        );
        let (x, y) = centre(cards.redo);
        assert_eq!(
            renderer.toolbar_hit_at(ToolbarPosition::Left, &toolbar, surface.0, surface.1, x, y),
            Some(ToolbarAction::Redo)
        );
        toolbar.can_undo = false;
        let (x, y) = centre(cards.undo);
        assert_eq!(
            renderer.toolbar_hit_at(ToolbarPosition::Left, &toolbar, surface.0, surface.1, x, y),
            None
        );
    }

    #[test]
    fn short_rules_mark_the_rail_families() {
        assert!(separates(
            Some(ToolbarAction::Color(0)),
            Some(ToolbarAction::Tool(Tool::Pen))
        ));
        assert!(!separates(
            Some(ToolbarAction::Color(0)),
            Some(ToolbarAction::Color(1))
        ));
        assert!(!separates(
            Some(ToolbarAction::Tool(Tool::Pen)),
            Some(ToolbarAction::Tool(Tool::Eraser))
        ));
        assert!(separates(
            Some(ToolbarAction::Boards),
            Some(ToolbarAction::More)
        ));
        assert!(!separates(
            Some(ToolbarAction::More),
            Some(ToolbarAction::Lock)
        ));
        assert!(!separates(
            Some(ToolbarAction::Tool(Tool::Pen)),
            Some(ToolbarAction::Size)
        ));
    }
}
