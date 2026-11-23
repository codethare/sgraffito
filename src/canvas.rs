//! Annotation data model and hit testing. No Wayland dependency, so it unit tests easily.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::sketch::SKETCH_SLACK;

/// Eraser hit radius, in logical pixels.
pub const ERASER_THRESHOLD: f32 = 8.0;

/// Drawn diameter of a selection handle, in logical pixels.
pub const SELECT_HANDLE: f32 = 9.0;

/// Radius a selection handle is grabbed within. Larger than it is drawn, so a small dot stays an
/// easy target on a dense drawing.
pub const SELECT_HANDLE_HIT: f32 = 11.0;

/// How far above the frame's top edge the rotation handle sits, in logical pixels.
pub const SELECT_ROTATE_GAP: f32 = 18.0;

/// Drawn diameter of the small midpoint handle a linear element shows on every segment.
pub const LINEAR_MIDPOINT: f32 = 5.0;
/// Radius a midpoint handle is grabbed within, in logical pixels.
pub const LINEAR_MIDPOINT_HIT: f32 = 8.0;

/// Smallest factor a resize may apply. A frame cannot be collapsed into an extent no drag can
/// restore, and a negative factor would mirror the selection, which the text renderer cannot draw.
const MIN_SCALE: f32 = 0.05;

/// Text size bounds, in logical pixels. They bound a new box and every resize of an existing one.
pub const TEXT_SIZE_MIN: f32 = 8.0;
pub const TEXT_SIZE_MAX: f32 = 72.0;
/// The pen width `[`/`]` step and the bounds they step it within, in logical pixels.
pub const PEN_WIDTH_MIN: f32 = 1.0;
pub const PEN_WIDTH_MAX: f32 = 16.0;
pub const PEN_WIDTH_STEP: f32 = 1.0;
/// The text size step `[`/`]` apply, in logical pixels; the bounds are `TEXT_SIZE_MIN`/`TEXT_SIZE_MAX`.
pub const TEXT_SIZE_STEP: f32 = 2.0;

/// Step a size by `dir` (±1) within `[min, max]`: the one arithmetic the brush and a held selection share.
pub fn stepped_size(current: f32, dir: f32, step: f32, min: f32, max: f32) -> f32 {
    (current + dir * step).clamp(min, max)
}

/// The zoom range, and the factor one classic wheel notch applies. Multiplicative, so zooming in and
/// back out returns to where it started.
pub const ZOOM_MIN: f32 = 0.2;
pub const ZOOM_MAX: f32 = 8.0;
pub const ZOOM_STEP: f32 = 1.15;

/// What one arrow press moves a held element by, in logical pixels: one, and ten while `Shift` is held.
pub const NUDGE_STEP: f32 = 1.0;
pub const NUDGE_STEP_FAR: f32 = 10.0;

/// Arrowhead length as a multiple of the pen width, and the floor below which a hairline arrow
/// would stop reading as an arrow, in logical pixels.
const ARROW_HEAD_SCALE: f32 = 4.0;
const ARROW_HEAD_MIN: f32 = 9.0;
/// Half the arrowhead's width, as a fraction of its length.
const ARROW_HEAD_HALF: f32 = 0.45;
/// Segments an ellipse is flattened into where it is measured rather than drawn. At 96 the flatness
/// is under a pixel even for a shape the size of a 4K output, against a hit tolerance of eight.
const ELLIPSE_SEGMENTS: usize = 96;

/// Built-in palette (`#rrggbb`).
pub const PALETTE: [&str; 5] = ["#e01b24", "#f6d32d", "#33d17a", "#3584e4", "#ffffff"];

/// A `#rrggbb` or `#rrggbbaa` colour as its four channels. Anything else is white, which is what the
/// palette's last entry is: a colour that cannot be parsed is not worth a panic in a drawing program.
pub fn parse_hex(s: &str) -> [u8; 4] {
    let hex = s.trim_start_matches('#');
    let v = u32::from_str_radix(hex, 16).unwrap_or(0xff_ffff);
    match hex.len() {
        8 => [(v >> 24) as u8, (v >> 16) as u8, (v >> 8) as u8, v as u8],
        6 => [(v >> 16) as u8, (v >> 8) as u8, v as u8, 255],
        _ => [255, 255, 255, 255],
    }
}

/// How bright a colour is, on the sRGB scale the contrast ratio is defined on.
fn luminance(color: &str) -> f32 {
    let c = parse_hex(color);
    let channel = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(c[0]) + 0.7152 * channel(c[1]) + 0.0722 * channel(c[2])
}

/// The ink a note's text and date are drawn in: the dark ink on a light background, the light ink on a
/// dark one. No single colour can be read on all five palette entries — light ink on the yellow is
/// 1.4:1 and dark ink on the red is 3.5:1 — so the pair is chosen rather than fixed, and the threshold
/// sits between the red's brightness and the blue's. `tests/canvas_hit.rs` holds every pair above
/// 4.5:1.
const NOTE_INK_DARK: &str = "#1c1c1c";
const NOTE_INK_LIGHT: &str = "#ffffff";
const NOTE_INK_THRESHOLD: f32 = 0.2;

pub fn note_ink(background: &str) -> &'static str {
    if luminance(background) > NOTE_INK_THRESHOLD {
        NOTE_INK_DARK
    } else {
        NOTE_INK_LIGHT
    }
}

// --- the creation date ----------------------------------------------------------------

/// A moment as the civil date `yyyy/mm/dd`. **UTC**: this crate has no timezone database and the
/// project's rule is that a few lines beat a dependency, so a note made late in the evening east of
/// Greenwich carries tomorrow's date. That is in the README's limitations.
pub fn date_stamp(at: std::time::SystemTime) -> String {
    let days = at
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64 / 86_400)
        .unwrap_or(0);
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}/{month:02}/{day:02}")
}

/// Days since 1970-01-01 to a civil date, by Howard Hinnant's algorithm: shift the epoch to March so
/// the leap day is at the end of the year, then count eras of 400 years, years within the era, and days
/// within the year.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { y + 1 } else { y }, month, day)
}

/// The tool the next gesture uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tool {
    #[default]
    Pen,
    Eraser,
    /// Fills the inside of a closed shape with the colour in hand.
    Bucket,
    Text,
    Selection,
    Circle,
    Rect,
    Diamond,
    Line,
    Arrow,
    /// Pans the canvas and nothing else: it is the one tool whose gesture cannot touch the document.
    Hand,
    /// Creates a sticky note and puts the caret in it.
    Note,
}

/// What clicking one control of the edit-mode toolbar does. Each variant has exactly the
/// effect of the key the control carries.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ToolbarAction {
    Tool(Tool),
    Color(usize),
    /// Direction to step the active tool's size in, ±1.
    SizeStep(f32),
    /// Open or close the list of width steps. The readout is the only control that carries it: a click
    /// toggles the list, and the two steps live there rather than beside it on the rail.
    Size,
    /// The `Esc` control: finish the text box being edited first, lock on the next click.
    Lock,
    /// Step the document back one change, like `Ctrl+Z`. No block shows this one yet.
    Undo,
    /// Step the document forward again, like `Ctrl+Shift+Z`. No block shows this one yet.
    Redo,
    /// Open or close the drawer of controls that are not used often.
    More,
    /// Switch the edit-mode backdrop off and on.
    Scrim,
    /// Pick one backdrop mode from the row beside the backdrop button.
    Backdrop(Backdrop),
    /// Open or close the panel of boards.
    Boards,
    /// Pick the board at this index of the panel's list.
    Board(usize),
    /// Create a board and pick it.
    NewBoard,
    /// Put the board at this index of the panel's list into its delete confirmation: the row itself
    /// becomes the question instead of the name.
    AskDeleteBoard(usize),
    /// Delete the board at this index of the panel's list, from that board's own row.
    DeleteBoard(usize),
    /// Leave the board delete confirmation without deleting anything.
    CancelDeleteBoard,
    /// Rename the board at this index of the panel's list, from that board's own row. The one row
    /// press that leaves its list open: the row it edits is in the list.
    RenameBoard(usize),
    /// Switch between every screen sharing one board and each screen picking its own.
    Shared,
    /// Link the selected text box to a board: opens the board list as a target picker.
    Link,
    /// Open the clipboard history panel.
    Clipboard,
    /// Paste clipboard entry `index`, newest first.
    Clip(usize),
}

/// What edit mode paints behind the document. Locked mode always paints nothing, because that
/// surface sits below ordinary windows and a backdrop there would hide the wallpaper.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Backdrop {
    /// Nothing: the wallpaper shows through.
    Transparent,
    /// A 24-point square grid over the wallpaper.
    Grid,
    /// A 24-point dot matrix over the wallpaper.
    Dots,
    /// Black at 90% opacity, which dims whatever is behind it.
    #[default]
    Black,
}

impl Backdrop {
    /// The order the buttons list them in, and the order the `B` key cycles.
    pub const ALL: [Backdrop; 4] = [
        Backdrop::Transparent,
        Backdrop::Grid,
        Backdrop::Dots,
        Backdrop::Black,
    ];

    /// The next mode the shortcut steps to: transparent, grid, dots, black, transparent.
    pub fn next(self) -> Backdrop {
        match self {
            Backdrop::Transparent => Backdrop::Grid,
            Backdrop::Grid => Backdrop::Dots,
            Backdrop::Dots => Backdrop::Black,
            Backdrop::Black => Backdrop::Transparent,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Backdrop::Transparent => "transparent",
            Backdrop::Grid => "grid",
            Backdrop::Dots => "dots",
            Backdrop::Black => "dim",
        }
    }
}

/// What a press in the board list applies, when the list opened to pick a target rather than to
/// switch the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Link,
    Move,
}

impl Tool {
    /// Whether the tool strokes with the pen width. Every tool that draws does, except the text
    /// tool, which has a size of its own.
    pub fn uses_pen_width(self) -> bool {
        !matches!(self, Tool::Text | Tool::Bucket)
    }

    /// Whether a completed mark with this tool hands the pointer back to `select`. One-shot drawing
    /// tools do; the eraser, the hand and the note tool keep their tool, and `select` is already there.
    pub fn hands_to_select(self) -> bool {
        matches!(
            self,
            Tool::Pen
                | Tool::Text
                | Tool::Circle
                | Tool::Rect
                | Tool::Diamond
                | Tool::Line
                | Tool::Arrow
        )
    }

    /// The key that arms this tool, as the rail states it and as [`crate::input::tool_for_keysym`]
    /// maps it. One letter per tool, so a block's text cannot name a key that does nothing.
    pub fn key(self) -> &'static str {
        match self {
            Tool::Pen => "P",
            Tool::Eraser => "E",
            Tool::Bucket => "G",
            Tool::Text => "T",
            Tool::Selection => "V",
            Tool::Circle => "O",
            Tool::Rect => "R",
            Tool::Diamond => "D",
            Tool::Line => "L",
            Tool::Arrow => "A",
            Tool::Hand => "H",
            Tool::Note => "N",
        }
    }
}

/// A connector endpoint bound to a closed shape. `at` is a point on the shape in local coordinates,
/// where the shape's centre is `(0, 0)` and its half-extents are `1`: its direction from the centre
/// is what fixes the point, so the binding stays on the visible outline when the shape moves,
/// resizes or turns. `Shape::attachment_point` routes that direction onto the real curve or edge,
/// which keeps a circle's or diamond's binding on the outline rather than at a box corner.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Attachment {
    /// The target shape's stable id. Zero is never a target.
    pub shape: u64,
    pub at: [f32; 2],
}

/// A connector drawn from the overlay while its attached shapes are being dragged: the document
/// raster still holds the old geometry, so the frame paints this copy in its place.
#[derive(Debug, Clone, PartialEq)]
pub enum LiveConnector {
    Stroke(ItemId, Stroke),
    Shape(ItemId, Shape),
}

impl LiveConnector {
    pub fn id(&self) -> ItemId {
        match self {
            LiveConnector::Stroke(id, _) | LiveConnector::Shape(id, _) => *id,
        }
    }
}

/// A moved shape that has met a free connector end: the corrected translation, the end it met and
/// the attachment the release should write.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReverseSnap {
    pub transform: Transform,
    pub connector: ItemId,
    pub end: usize,
    pub attachment: Attachment,
}

/// How close a pointer must come to a closed shape, in logical pixels, before a connector end
/// sticks to it.
/// One snap decision for a connector or pen end: the point it should land on, and the shape
/// attachment when that point is on a closed shape. Strokes, open lines and the grid snap without
/// an attachment because they have no stable shape id to follow.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DrawSnap {
    pub point: [f32; 2],
    pub attachment: Option<Attachment>,
}

pub const SNAP_RADIUS: f32 = 12.0;
/// The repeating distance of the backdrop's grid and dot matrix, and therefore the grid snap.
pub const GRID_SPACING: f32 = 24.0;

/// A lattice for the fallback snap. The dots sit at the middle of each cell, so their lattice is
/// the grid shifted by half a cell; passing the phase here keeps the snap on the visible marks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridSnap {
    pub step: f32,
    pub offset: [f32; 2],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stroke {
    /// `#rrggbb`
    pub color: String,
    pub width: f32,
    pub points: Vec<[f32; 2]>,
    /// The closed shapes the first and last sample stick to, when a pen stroke was started or
    /// finished against one.
    #[serde(default)]
    pub attach: [Option<Attachment>; 2],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextItem {
    pub x: f32,
    pub y: f32,
    /// `#rrggbb`
    pub color: String,
    pub size: f32,
    pub text: String,
    /// The board this text links to, when it is a link: `Ctrl`+click follows it. Absent in a file
    /// written before links existed.
    #[serde(default)]
    pub link: Option<String>,
}

/// Padding between a note's text and its edges, its smallest side, the size a click creates, and the
/// corner radius a note is drawn with. A note's typography is its own: it is a fixed object rather than
/// a text box that happens to be square.
pub const NOTE_PADDING: f32 = 12.0;
pub const NOTE_MIN_SIZE: f32 = 96.0;
/// How far the pointer has to move before a press on empty space is a drag that sizes a new note rather
/// than a click that takes the default size.
pub const NOTE_DRAG_SLOP: f32 = 24.0;
pub const NOTE_SIZE: f32 = 180.0;
pub const NOTE_RADIUS: f32 = 8.0;
pub const NOTE_TEXT_SIZE: f32 = 18.0;
pub const NOTE_DATE_SIZE: f32 = 11.0;
/// The line the date sits on, which the text box leaves room for at the bottom.
pub const NOTE_DATE_LINE: f32 = NOTE_DATE_SIZE * 1.4;

/// A sticky note: a rounded square filled with a colour, the text written on it, and the day it was
/// made. One element rather than a shape plus a text box, because a shape has no fill, a text box has no
/// width to wrap at and no background to sit on, and because a note in three pieces would have to be
/// moved, erased and kept in step three times.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// The background, which is the palette colour that was in hand when it was made.
    pub color: String,
    pub text: String,
    /// `yyyy/mm/dd`, written once when the note was created. UTC; see [`date_stamp`].
    pub created: String,
}

impl Note {
    /// A new note at a point, with no text yet.
    pub fn new(x: f32, y: f32, color: &str, created: String) -> Self {
        Self {
            x,
            y,
            w: NOTE_SIZE,
            h: NOTE_MIN_SIZE,
            color: color.to_string(),
            text: String::new(),
            created,
        }
    }

    /// The square it is drawn as, which is what the eraser, the marquee and the frame use.
    pub fn bounds(&self) -> Rect {
        Rect {
            x: self.x,
            y: self.y,
            w: self.w,
            h: self.h,
        }
    }

    /// Where the text may be drawn: inside the padding, with the date's line left at the bottom.
    pub fn text_box(&self) -> Rect {
        Rect {
            x: self.x + NOTE_PADDING,
            y: self.y + NOTE_PADDING,
            w: (self.w - 2.0 * NOTE_PADDING).max(1.0),
            h: (self.h - 2.0 * NOTE_PADDING - NOTE_DATE_LINE).max(1.0),
        }
    }

    /// The line the date sits on, in the bottom-right corner.
    pub fn date_box(&self) -> Rect {
        Rect {
            x: self.x + NOTE_PADDING,
            y: self.y + self.h - NOTE_PADDING - NOTE_DATE_LINE,
            w: (self.w - 2.0 * NOTE_PADDING).max(1.0),
            h: NOTE_DATE_LINE,
        }
    }

    /// The note's text as the text path sees it: at its text box, its own size, in the ink that can be
    /// read on its background. The editing machinery takes one of these, which is why a note can be
    /// typed in without a second implementation.
    pub fn text_item(&self) -> TextItem {
        let box_ = self.text_box();
        TextItem {
            x: box_.x,
            y: box_.y,
            color: self.ink().to_string(),
            size: NOTE_TEXT_SIZE,
            text: self.text.clone(),
            link: None,
        }
    }

    /// The ink its text and date are drawn in.
    pub fn ink(&self) -> &'static str {
        note_ink(&self.color)
    }

    /// The height a note needs to hold `text_height` of text, never less than its minimum.
    pub fn height_for(&self, text_height: f32) -> f32 {
        (2.0 * NOTE_PADDING + text_height + NOTE_DATE_LINE).max(NOTE_MIN_SIZE)
    }

    /// The note grown to hold text that needs `text_height`, never shrunk: the height a note was
    /// dragged or resized to is what the user asked for, and the text that has been typed only ever
    /// wins when it needs more room than that. Assigning `height_for` directly instead snapped a tall
    /// note back to the height of its text on the first keystroke.
    pub fn grown_for(&self, text_height: f32) -> Self {
        let mut grown = self.clone();
        grown.h = grown.h.max(grown.height_for(text_height));
        grown
    }
}

/// The five kinds a shape tool draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShapeKind {
    Circle,
    Rect,
    Diamond,
    Line,
    Arrow,
}

impl ShapeKind {
    /// Whether a connector may attach to this shape. Lines and arrows are connectors themselves.
    pub fn is_closed(self) -> bool {
        matches!(
            self,
            ShapeKind::Circle | ShapeKind::Rect | ShapeKind::Diamond
        )
    }
}

/// Whether a serde field is the default zero, so a file written before it existed stays byte-for-byte
/// the same when it is written again.
fn is_zero(value: &f32) -> bool {
    *value == 0.0
}

/// A shape is two points, a kind and the angle a closed outline is turned by. Its outline is derived
/// from those when it is drawn and when it is hit, so it costs the same in the file whether it is ten
/// pixels or a whole output across, and a resize or a rotation is arithmetic on that small state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Shape {
    pub kind: ShapeKind,
    /// `#rrggbb`
    pub color: String,
    pub width: f32,
    pub from: [f32; 2],
    pub to: [f32; 2],
    /// The angle a closed outline is drawn at, in radians about the centre of `from`/`to`. Kept apart
    /// from the two points so rotating keeps the shape's size; lines and arrows turn their endpoints
    /// instead and ignore it. Files written before rotation existed load as zero.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub rotate: f32,
    /// A stable id inside its board, so a connector can keep pointing at this shape after other
    /// shapes are deleted or moved. Zero is assigned a real id when the shape enters a board.
    #[serde(default)]
    pub id: u64,
    /// Interior vertices of a line, arrow or pen-like linear element, between `from` and `to`.
    /// Empty for a straight two-point connector and for every closed shape. The first and last
    /// points stay in `from`/`to`, so an attachment continues to name an endpoint.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub points: Vec<[f32; 2]>,
    /// For a line or arrow, the closed shapes its two ends stick to.
    #[serde(default)]
    pub attach: [Option<Attachment>; 2],
    /// The colour inside a closed shape, set by the bucket. `None` leaves the inside clear.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<String>,
}

/// An item the eraser hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemId {
    Stroke(usize),
    Text(usize),
    Shape(usize),
    Note(usize),
}

/// Annotations belonging to a single board: what is drawn on it, however many screens show it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Board {
    #[serde(default)]
    pub strokes: Vec<Stroke>,
    #[serde(default)]
    pub texts: Vec<TextItem>,
    #[serde(default)]
    pub shapes: Vec<Shape>,
    #[serde(default)]
    pub notes: Vec<Note>,
}

impl Board {
    /// The next unused shape id. Ids are board-local and survive deletion, so a connector can name
    /// the shape it sticks to without depending on a vector index.
    pub fn next_shape_id(&self) -> u64 {
        self.shapes.iter().map(|shape| shape.id).max().unwrap_or(0) + 1
    }

    /// Push a new shape, giving it an id if it has none or one already taken.
    pub fn add_shape(&mut self, mut shape: Shape) {
        if shape.id == 0 || self.shapes.iter().any(|other| other.id == shape.id) {
            shape.id = self.next_shape_id();
        }
        self.shapes.push(shape);
    }

    /// Give every shape a unique non-zero id. Files written before ids existed load with zeroes;
    /// duplicate ids can only come from a hand-edited file or a copy that forgot to remap.
    pub fn ensure_shape_ids(&mut self) {
        let mut used = BTreeSet::new();
        let mut next = self.next_shape_id();
        for shape in &mut self.shapes {
            if shape.id == 0 || !used.insert(shape.id) {
                shape.id = next;
                used.insert(next);
                next += 1;
            }
        }
    }

    /// The closed shape a connector end at `p` should stick to, if one is within `tolerance` canvas
    /// pixels: its attachment and the snapped point.
    pub fn snap(&self, p: [f32; 2], tolerance: f32) -> Option<(Attachment, [f32; 2])> {
        let limit = tolerance * tolerance;
        let mut best: Option<(Attachment, [f32; 2], f32)> = None;
        for shape in &self.shapes {
            if shape.id == 0 || !shape.kind.is_closed() {
                continue;
            }
            let Some(point) = shape.nearest_boundary(p) else {
                continue;
            };
            let distance = dist_squared(point, p);
            if distance <= limit && best.is_none_or(|(_, _, nearest)| distance < nearest) {
                best = Some((shape.attachment_at(point), point, distance));
            }
        }
        best.map(|(attachment, point, _)| (attachment, point))
    }

    /// The magnetic target for a connector or pen end: closed shape outlines first, then the
    /// polylines of freehand strokes and open lines/arrows, then a grid intersection. Content beats
    /// the grid even when the grid point is a little closer, so an existing drawing remains the
    /// thing the next line attaches to.
    pub fn snap_drawing(
        &self,
        p: [f32; 2],
        tolerance: f32,
        grid: Option<GridSnap>,
    ) -> Option<DrawSnap> {
        let limit = tolerance * tolerance;
        let mut best: Option<(f32, DrawSnap)> = None;
        let mut consider = |point: [f32; 2], attachment: Option<Attachment>| {
            let distance = dist_squared(point, p);
            if distance <= limit && best.is_none_or(|(nearest, _)| distance < nearest) {
                best = Some((distance, DrawSnap { point, attachment }));
            }
        };
        for shape in &self.shapes {
            if !shape.paint_bounds().grown(tolerance).contains(p[0], p[1]) {
                continue;
            }
            if shape.kind.is_closed() {
                if shape.id == 0 {
                    continue;
                }
                if let Some(point) = shape.nearest_boundary(p) {
                    consider(point, Some(shape.attachment_at(point)));
                }
            } else {
                let outline = shape.outline();
                if let Some(point) = nearest_on_polyline(p, &outline) {
                    consider(point, None);
                }
            }
        }
        for stroke in &self.strokes {
            let Some(bounds) = stroke.paint_bounds() else {
                continue;
            };
            if !bounds.grown(tolerance).contains(p[0], p[1]) {
                continue;
            }
            if let Some(point) = nearest_on_stroke(p, &stroke.points) {
                consider(point, None);
            }
        }
        if best.is_none()
            && let Some(grid) = grid.filter(|grid| grid.step > f32::EPSILON)
        {
            let point = [
                grid.offset[0] + ((p[0] - grid.offset[0]) / grid.step).round() * grid.step,
                grid.offset[1] + ((p[1] - grid.offset[1]) / grid.step).round() * grid.step,
            ];
            if dist_squared(point, p) <= limit {
                best = Some((
                    dist_squared(point, p),
                    DrawSnap {
                        point,
                        attachment: None,
                    },
                ));
            }
        }
        best.map(|(_, snap)| snap)
    }

    /// Clear attachments whose target shape is no longer on this board.
    pub fn drop_dangling_attachments(&mut self) {
        let ids: BTreeSet<u64> = self
            .shapes
            .iter()
            .map(|shape| shape.id)
            .filter(|id| *id != 0)
            .collect();
        for stroke in &mut self.strokes {
            for attachment in &mut stroke.attach {
                if attachment.is_some_and(|attachment| !ids.contains(&attachment.shape)) {
                    *attachment = None;
                }
            }
        }
        for shape in &mut self.shapes {
            for attachment in &mut shape.attach {
                if attachment.is_some_and(|attachment| !ids.contains(&attachment.shape)) {
                    *attachment = None;
                }
            }
        }
    }

    /// Put every attached connector endpoint back on its shape. Returns the union of the boxes that
    /// moved, or `None` when no attachment changed anything.
    pub fn sync_connectors(&mut self) -> Option<Rect> {
        let shapes: BTreeMap<u64, Shape> = self
            .shapes
            .iter()
            .filter(|shape| shape.id != 0)
            .map(|shape| (shape.id, shape.clone()))
            .collect();
        let mut changed: Option<Rect> = None;
        for stroke in &mut self.strokes {
            if stroke.attach == [None, None] || stroke.points.is_empty() {
                continue;
            }
            let before = stroke.paint_bounds();
            if let Some(attachment) = stroke.attach[0]
                && let Some(shape) = shapes.get(&attachment.shape)
            {
                stroke.points[0] = shape.attachment_point(attachment);
            }
            if let Some(attachment) = stroke.attach[1]
                && let Some(shape) = shapes.get(&attachment.shape)
                && let Some(last) = stroke.points.last_mut()
            {
                *last = shape.attachment_point(attachment);
            }
            if let Some(after) = stroke.paint_bounds()
                && Some(after) != before
            {
                let box_ = before.map_or(after, |before| before.union(after));
                changed = Some(changed.map_or(box_, |box_| box_.union(box_)));
            }
        }
        for shape in &mut self.shapes {
            if shape.kind.is_closed() || shape.attach == [None, None] {
                continue;
            }
            let before = shape.paint_bounds();
            if let Some(attachment) = shape.attach[0]
                && let Some(target) = shapes.get(&attachment.shape)
            {
                shape.from = target.attachment_point(attachment);
            }
            if let Some(attachment) = shape.attach[1]
                && let Some(target) = shapes.get(&attachment.shape)
            {
                shape.to = target.attachment_point(attachment);
            }
            let after = shape.paint_bounds();
            if after != before {
                changed = Some(changed.map_or(before.union(after), |box_| box_.union(after)));
            }
        }
        changed
    }

    /// The live copies of every attached connector for a drag of `items` by `transform`. The
    /// raster keeps the committed connectors, so the overlay paints these in their place; the free
    /// endpoints still take the same transform as the selected elements.
    pub fn connector_preview(
        &self,
        items: &[ItemId],
        transform: Transform,
        pending: &[(ItemId, usize, Attachment)],
    ) -> Vec<LiveConnector> {
        let selected = |id: ItemId| items.contains(&id);
        let pending_at = |id: ItemId, end: usize| {
            pending
                .iter()
                .find(|(connector, index, _)| *connector == id && *index == end)
                .map(|(_, _, attachment)| *attachment)
        };
        let shapes: BTreeMap<u64, Shape> = self
            .shapes
            .iter()
            .enumerate()
            .filter(|(_, shape)| shape.id != 0)
            .map(|(index, shape)| {
                let shape = if selected(ItemId::Shape(index)) {
                    transform.shape(shape)
                } else {
                    shape.clone()
                };
                (shape.id, shape)
            })
            .collect();
        let mut live = Vec::new();
        for (index, stroke) in self.strokes.iter().enumerate() {
            let id = ItemId::Stroke(index);
            let pending = [pending_at(id, 0), pending_at(id, 1)];
            if stroke.attach == [None, None] && pending == [None, None] {
                continue;
            }
            let mut copy = if selected(id) {
                transform.stroke(stroke)
            } else {
                stroke.clone()
            };
            for (end, attachment) in pending.iter().enumerate() {
                if let Some(attachment) = attachment {
                    copy.attach[end] = Some(*attachment);
                }
            }
            if !copy.points.is_empty() {
                if let Some(attachment) = copy.attach[0]
                    && let Some(shape) = shapes.get(&attachment.shape)
                {
                    copy.points[0] = shape.attachment_point(attachment);
                }
                if let Some(attachment) = copy.attach[1]
                    && let Some(shape) = shapes.get(&attachment.shape)
                    && let Some(last) = copy.points.last_mut()
                {
                    *last = shape.attachment_point(attachment);
                }
            }
            live.push(LiveConnector::Stroke(id, copy));
        }
        for (index, shape) in self.shapes.iter().enumerate() {
            let id = ItemId::Shape(index);
            let pending = [pending_at(id, 0), pending_at(id, 1)];
            if shape.kind.is_closed() || (shape.attach == [None, None] && pending == [None, None]) {
                continue;
            }
            let mut copy = if selected(id) {
                transform.shape(shape)
            } else {
                shape.clone()
            };
            for (end, attachment) in pending.iter().enumerate() {
                if let Some(attachment) = attachment {
                    copy.attach[end] = Some(*attachment);
                }
            }
            if let Some(attachment) = copy.attach[0]
                && let Some(target) = shapes.get(&attachment.shape)
            {
                copy.from = target.attachment_point(attachment);
            }
            if let Some(attachment) = copy.attach[1]
                && let Some(target) = shapes.get(&attachment.shape)
            {
                copy.to = target.attachment_point(attachment);
            }
            live.push(LiveConnector::Shape(id, copy));
        }
        live
    }

    /// Let a moved closed shape meet the nearest free connector end. Returns the corrected
    /// translation and the link to write on release. `items` is the selection; connectors that are
    /// part of it are not candidates, because they move with the shape.
    pub fn reverse_snap(
        &self,
        items: &[ItemId],
        transform: Transform,
        tolerance: f32,
    ) -> Option<ReverseSnap> {
        let Transform::Translate { dx, dy } = transform else {
            return None;
        };
        let selected = |id: ItemId| items.contains(&id);
        let mut best: Option<(f32, ReverseSnap)> = None;
        let consider = |point: [f32; 2],
                        connector: ItemId,
                        end: usize,
                        preview: &Shape,
                        best: &mut Option<(f32, ReverseSnap)>| {
            let Some(snapped) = preview.nearest_boundary(point) else {
                return;
            };
            let distance = (point[0] - snapped[0]).hypot(point[1] - snapped[1]);
            if distance > tolerance || best.is_some_and(|(nearest, _)| distance >= nearest) {
                return;
            }
            best.replace((
                distance,
                ReverseSnap {
                    transform: Transform::Translate {
                        dx: dx + point[0] - snapped[0],
                        dy: dy + point[1] - snapped[1],
                    },
                    connector,
                    end,
                    attachment: preview.attachment_at(snapped),
                },
            ));
        };
        for (index, shape) in self.shapes.iter().enumerate() {
            if !selected(ItemId::Shape(index)) || !shape.kind.is_closed() || shape.id == 0 {
                continue;
            }
            let preview = transform.shape(shape);
            for (connector_index, connector) in self.shapes.iter().enumerate() {
                if connector.kind.is_closed() || selected(ItemId::Shape(connector_index)) {
                    continue;
                }
                for end in 0..2 {
                    if connector.attach[end].is_some() {
                        continue;
                    }
                    let point = if end == 0 {
                        connector.from
                    } else {
                        connector.to
                    };
                    consider(
                        point,
                        ItemId::Shape(connector_index),
                        end,
                        &preview,
                        &mut best,
                    );
                }
            }
            for (stroke_index, stroke) in self.strokes.iter().enumerate() {
                if selected(ItemId::Stroke(stroke_index)) || stroke.points.is_empty() {
                    continue;
                }
                for end in 0..2 {
                    if stroke.attach[end].is_some() {
                        continue;
                    }
                    let point = if end == 0 {
                        stroke.points[0]
                    } else {
                        *stroke.points.last().unwrap_or(&stroke.points[0])
                    };
                    consider(
                        point,
                        ItemId::Stroke(stroke_index),
                        end,
                        &preview,
                        &mut best,
                    );
                }
            }
        }
        best.map(|(_, snap)| snap)
    }

    /// The linked text under a point, measured with the renderer's boxes rather than the font-size
    /// estimate: a link is a thing you aim at.
    pub fn linked_at(
        &self,
        point: [f32; 2],
        mut bounds: impl FnMut(&TextItem) -> Rect,
    ) -> Option<&TextItem> {
        self.texts
            .iter()
            .find(|text| text.link.is_some() && bounds(text).contains(point[0], point[1]))
    }

    /// Eraser hit test: text boxes win over everything, then the nearest stroke or shape within the
    /// threshold. A shape is hit by its outline, never from inside it, so a rectangle's interior
    /// keeps belonging to whatever is drawn inside it.
    /// `tolerance` is a **canvas** distance: the caller converts the interface's own reach through
    /// the view, so a zoomed-out drawing is not a drawing whose lines are harder to hit.
    pub fn hit(&self, x: f32, y: f32, tolerance: f32) -> Option<ItemId> {
        if let Some(i) = self.texts.iter().position(|t| t.bounds().contains(x, y)) {
            return Some(ItemId::Text(i));
        }
        let limit = tolerance * tolerance;
        // Notes are drawn over everything else, so the note under the pointer is the element the pointer
        // is on: a hit test that preferred the ink would hand back an element that is not visible where
        // the click landed.
        if let Some(i) = self.notes.iter().position(|n| n.bounds().contains(x, y)) {
            return Some(ItemId::Note(i));
        }
        let mut best: Option<(ItemId, f32)> = None;
        let mut consider = |id: ItemId, d: f32| {
            if d <= limit && best.is_none_or(|(_, nearest)| d < nearest) {
                best = Some((id, d));
            }
        };
        for (i, s) in self.strokes.iter().enumerate() {
            consider(ItemId::Stroke(i), s.distance_squared(x, y));
        }
        for (i, s) in self.shapes.iter().enumerate() {
            consider(ItemId::Shape(i), s.distance_squared(x, y));
        }
        best.map(|(id, _)| id)
    }

    /// Remove the element under `(x, y)`, returning the box it occupied, so the frame that
    /// repaints it knows what to damage.
    pub fn erase(&mut self, x: f32, y: f32, tolerance: f32) -> Option<Rect> {
        match self.hit(x, y, tolerance) {
            Some(ItemId::Stroke(i)) => {
                let box_ = self.strokes[i].paint_bounds();
                self.strokes.remove(i);
                box_
            }
            Some(ItemId::Text(i)) => {
                let box_ = self.texts[i].paint_bounds();
                self.texts.remove(i);
                Some(box_)
            }
            Some(ItemId::Shape(i)) => {
                let box_ = self.shapes[i].paint_bounds();
                self.shapes.remove(i);
                self.drop_dangling_attachments();
                Some(box_)
            }
            Some(ItemId::Note(i)) => {
                let box_ = self.notes[i].bounds();
                self.notes.remove(i);
                Some(box_)
            }
            None => None,
        }
    }

    /// Remove the elements `items` names, returning the box they occupied so the frame that follows
    /// knows what to damage. Each kind's indices are removed from the highest down, because removing
    /// an earlier element shifts every later one: without that, a selection holding two strokes would
    /// remove the second twice and leave the first drawn. An index that is no longer there is skipped,
    /// which is what lets a `clear` arriving mid-gesture leave nothing to resurrect.
    /// A copy of every element `items` names, appended to its own list, one per kind, in the order it was
    /// given: the ids of the copies, and the box they occupy. Appending is what keeps the originals at the
    /// indices they had, so an element that is named elsewhere cannot be reached by a copy landing in the
    /// middle of the list.
    pub fn duplicate(&mut self, items: &[ItemId]) -> (Vec<ItemId>, Option<Rect>) {
        // Every copied shape gets a fresh id, and connectors copied with it are re-pointed to the
        // copies rather than to the originals they were attached to.
        let mut shape_ids = BTreeMap::new();
        let mut copy_ids = BTreeMap::new();
        let mut next = self.next_shape_id();
        for id in items {
            if let ItemId::Shape(index) = *id
                && let Some(shape) = self.shapes.get(index)
            {
                copy_ids.insert(index, next);
                if shape.id != 0 {
                    shape_ids.insert(shape.id, next);
                }
                next += 1;
            }
        }
        let mut copies = Vec::with_capacity(items.len());
        let mut box_: Option<Rect> = None;
        for id in items {
            match *id {
                ItemId::Stroke(i) => {
                    if let Some(mut s) = self.strokes.get(i).cloned() {
                        for attachment in &mut s.attach {
                            *attachment = attachment.and_then(|attachment| {
                                shape_ids.get(&attachment.shape).map(|id| Attachment {
                                    shape: *id,
                                    at: attachment.at,
                                })
                            });
                        }
                        copies.push(ItemId::Stroke(self.strokes.len()));
                        union_into(&mut box_, s.paint_bounds());
                        self.strokes.push(s);
                    }
                }
                ItemId::Text(i) => {
                    if let Some(t) = self.texts.get(i).cloned() {
                        copies.push(ItemId::Text(self.texts.len()));
                        union_into(&mut box_, Some(t.paint_bounds()));
                        self.texts.push(t);
                    }
                }
                ItemId::Shape(i) => {
                    if let Some(mut s) = self.shapes.get(i).cloned() {
                        s.id = copy_ids.get(&i).copied().unwrap_or(0);
                        for attachment in &mut s.attach {
                            *attachment = attachment.and_then(|attachment| {
                                shape_ids.get(&attachment.shape).map(|id| Attachment {
                                    shape: *id,
                                    at: attachment.at,
                                })
                            });
                        }
                        copies.push(ItemId::Shape(self.shapes.len()));
                        union_into(&mut box_, Some(s.paint_bounds()));
                        self.shapes.push(s);
                    }
                }
                ItemId::Note(i) => {
                    if let Some(n) = self.notes.get(i).cloned() {
                        copies.push(ItemId::Note(self.notes.len()));
                        union_into(&mut box_, Some(n.bounds()));
                        self.notes.push(n);
                    }
                }
            }
        }
        (copies, box_)
    }

    /// A new board holding copies of exactly the named elements, in the order the ids give them.
    /// The source is untouched; the copy is what a clipboard entry holds.
    pub fn copied(&self, items: &[ItemId]) -> Board {
        let mut out = Board::default();
        for id in items {
            match *id {
                ItemId::Stroke(i) => {
                    if let Some(s) = self.strokes.get(i) {
                        out.strokes.push(s.clone());
                    }
                }
                ItemId::Text(i) => {
                    if let Some(t) = self.texts.get(i) {
                        out.texts.push(t.clone());
                    }
                }
                ItemId::Shape(i) => {
                    if let Some(s) = self.shapes.get(i) {
                        out.shapes.push(s.clone());
                    }
                }
                ItemId::Note(i) => {
                    if let Some(n) = self.notes.get(i) {
                        out.notes.push(n.clone());
                    }
                }
            }
        }
        out
    }

    /// Append every element of `other`, returning their new ids and the box they occupy. Links on
    /// the copies are kept; a text linked to a board can move with its link.
    pub fn append(&mut self, other: &Board) -> (Vec<ItemId>, Option<Rect>) {
        let mut ids = Vec::new();
        let mut box_: Option<Rect> = None;
        // Ids are board-local: every appended shape gets a fresh one, and a connector copied with
        // it is re-pointed. A connector whose target stayed on the source board is cut loose rather
        // than left naming an unrelated shape here.
        let mut shape_ids = BTreeMap::new();
        let mut shapes = Vec::new();
        let first_id = self.next_shape_id();
        for (id, shape) in (first_id..).zip(&other.shapes) {
            let mut copy = shape.clone();
            if copy.id != 0 {
                shape_ids.insert(copy.id, id);
            }
            copy.id = id;
            shapes.push(copy);
        }
        for s in &other.strokes {
            let mut copy = s.clone();
            for attachment in &mut copy.attach {
                *attachment = attachment.and_then(|attachment| {
                    shape_ids.get(&attachment.shape).map(|id| Attachment {
                        shape: *id,
                        at: attachment.at,
                    })
                });
            }
            ids.push(ItemId::Stroke(self.strokes.len()));
            union_into(&mut box_, copy.paint_bounds());
            self.strokes.push(copy);
        }
        for t in &other.texts {
            ids.push(ItemId::Text(self.texts.len()));
            union_into(&mut box_, Some(t.paint_bounds()));
            self.texts.push(t.clone());
        }
        for mut shape in shapes {
            for attachment in &mut shape.attach {
                *attachment = attachment.and_then(|attachment| {
                    shape_ids.get(&attachment.shape).map(|id| Attachment {
                        shape: *id,
                        at: attachment.at,
                    })
                });
            }
            union_into(&mut box_, Some(shape.paint_bounds()));
            ids.push(ItemId::Shape(self.shapes.len()));
            self.shapes.push(shape);
        }
        for n in &other.notes {
            ids.push(ItemId::Note(self.notes.len()));
            union_into(&mut box_, Some(n.bounds()));
            self.notes.push(n.clone());
        }
        (ids, box_)
    }

    /// Recolour every element `items` names, returning the box they occupy, which is what the frame that
    /// repaints them damages. `None` means nothing was named. Unlike a removal this walks the ids as they
    /// come and leaves every index alone, so an element the selection does not hold cannot be reached.
    pub fn recolour(&mut self, items: &[ItemId], color: &str) -> Option<Rect> {
        let mut box_: Option<Rect> = None;
        for id in items {
            match *id {
                ItemId::Stroke(i) => {
                    if let Some(s) = self.strokes.get_mut(i) {
                        s.color = color.to_string();
                        union_into(&mut box_, s.paint_bounds());
                    }
                }
                ItemId::Text(i) => {
                    if let Some(t) = self.texts.get_mut(i) {
                        t.color = color.to_string();
                        union_into(&mut box_, Some(t.paint_bounds()));
                    }
                }
                ItemId::Shape(i) => {
                    if let Some(s) = self.shapes.get_mut(i) {
                        s.color = color.to_string();
                        union_into(&mut box_, Some(s.paint_bounds()));
                    }
                }
                ItemId::Note(i) => {
                    if let Some(n) = self.notes.get_mut(i) {
                        n.color = color.to_string();
                        union_into(&mut box_, Some(n.bounds()));
                    }
                }
            }
        }
        box_
    }

    /// Step the size of what is held, as one change: a stroke's or a shape's width by the pen step, a text
    /// box's size by the text step, each clamped to its own range. A note keeps its own type size. Returns
    /// the union of the ink before and after every item that changed, or `None` when nothing did — a press
    /// with every held item at its bound is not a change.
    pub fn step_size(&mut self, items: &[ItemId], dir: f32) -> Option<Rect> {
        let mut box_: Option<Rect> = None;
        for id in items {
            let (before, after) = match *id {
                ItemId::Stroke(i) => {
                    let Some(stroke) = self.strokes.get_mut(i) else {
                        continue;
                    };
                    let before = stroke.paint_bounds();
                    stroke.width = stepped_size(
                        stroke.width,
                        dir,
                        PEN_WIDTH_STEP,
                        PEN_WIDTH_MIN,
                        PEN_WIDTH_MAX,
                    );
                    (before, stroke.paint_bounds())
                }
                ItemId::Shape(i) => {
                    let Some(shape) = self.shapes.get_mut(i) else {
                        continue;
                    };
                    let before = Some(shape.paint_bounds());
                    shape.width = stepped_size(
                        shape.width,
                        dir,
                        PEN_WIDTH_STEP,
                        PEN_WIDTH_MIN,
                        PEN_WIDTH_MAX,
                    );
                    (before, Some(shape.paint_bounds()))
                }
                ItemId::Text(i) => {
                    let Some(text) = self.texts.get_mut(i) else {
                        continue;
                    };
                    let before = Some(text.paint_bounds());
                    text.size =
                        stepped_size(text.size, dir, TEXT_SIZE_STEP, TEXT_SIZE_MIN, TEXT_SIZE_MAX);
                    (before, Some(text.paint_bounds()))
                }
                ItemId::Note(_) => continue,
            };
            if before != after {
                union_into(&mut box_, before);
                union_into(&mut box_, after);
            }
        }
        box_
    }

    pub fn remove(&mut self, items: &[ItemId]) -> Option<Rect> {
        let (mut strokes, mut texts, mut shapes, mut notes) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for id in items {
            match *id {
                ItemId::Stroke(i) => strokes.push(i),
                ItemId::Text(i) => texts.push(i),
                ItemId::Shape(i) => shapes.push(i),
                ItemId::Note(i) => notes.push(i),
            }
        }
        let mut removed: Option<Rect> = None;
        for i in &strokes {
            union_into(&mut removed, self.strokes.get(*i).and_then(Stroke::bounds));
        }
        for i in &texts {
            union_into(&mut removed, self.texts.get(*i).map(TextItem::paint_bounds));
        }
        for i in &shapes {
            union_into(&mut removed, self.shapes.get(*i).map(Shape::bounds));
        }
        for i in &notes {
            union_into(&mut removed, self.notes.get(*i).map(Note::bounds));
        }
        remove_indices(&mut self.strokes, strokes);
        remove_indices(&mut self.texts, texts);
        remove_indices(&mut self.shapes, shapes);
        remove_indices(&mut self.notes, notes);
        if !items.is_empty() {
            self.drop_dangling_attachments();
        }
        removed
    }
}

/// Fold `box_` into an accumulator rectangle.
fn union_into(acc: &mut Option<Rect>, box_: Option<Rect>) {
    *acc = match (*acc, box_) {
        (Some(a), Some(b)) => Some(a.union(b)),
        (a, b) => a.or(b),
    };
}

/// Remove `indices` from `list`, highest first, skipping any that are past its end.
fn remove_indices<T>(list: &mut Vec<T>, mut indices: Vec<usize>) {
    indices.sort_unstable();
    indices.dedup();
    for index in indices.into_iter().rev() {
        if index < list.len() {
            list.remove(index);
        }
    }
}

impl Shape {
    /// The shape's centre and half-extents, in logical pixels.
    pub fn box_geometry(&self) -> ([f32; 2], [f32; 2]) {
        let box_ = rect_of(self.from, self.to);
        (
            [box_.x + box_.w / 2.0, box_.y + box_.h / 2.0],
            [box_.w / 2.0, box_.h / 2.0],
        )
    }

    /// The centre of the box `from` and `to` span.
    pub fn centre(&self) -> [f32; 2] {
        self.box_geometry().0
    }

    /// The angle a closed outline is drawn at. Lines and arrows turn their endpoints instead, so
    /// any angle stored on them is ignored.
    pub fn turn_angle(&self) -> f32 {
        if self.kind.is_closed() {
            self.rotate
        } else {
            0.0
        }
    }

    /// Whether a point lies inside a closed shape, in the shape's own turned frame. Lines and
    /// arrows have no inside; a zero-width box contains nothing.
    pub fn contains(&self, p: [f32; 2]) -> bool {
        if !self.kind.is_closed() {
            return false;
        }
        let box_ = rect_of(self.from, self.to);
        if box_.w <= f32::EPSILON || box_.h <= f32::EPSILON {
            return false;
        }
        let p = self.to_local(p);
        let (cx, cy) = (box_.x + box_.w / 2.0, box_.y + box_.h / 2.0);
        let (dx, dy) = ((p[0] - cx) / (box_.w / 2.0), (p[1] - cy) / (box_.h / 2.0));
        match self.kind {
            ShapeKind::Circle => dx * dx + dy * dy <= 1.0,
            ShapeKind::Rect => box_.contains(p[0], p[1]),
            ShapeKind::Diamond => dx.abs() + dy.abs() <= 1.0,
            ShapeKind::Line | ShapeKind::Arrow => false,
        }
    }

    /// A point of the unrotated outline in world coordinates.
    pub fn to_world(&self, p: [f32; 2]) -> [f32; 2] {
        rotate_about(p, self.centre(), self.turn_angle())
    }

    /// A world point in the shape's unrotated frame, which is where its box and attachments live.
    fn to_local(&self, p: [f32; 2]) -> [f32; 2] {
        rotate_about(p, self.centre(), -self.turn_angle())
    }

    /// The point on this shape's outline nearest to `p`, for a closed shape. Lines and arrows have
    /// no interior to attach to. The search runs in the outline's own unrotated frame and the result
    /// is turned back, so a rotated shape is attached to and hit on the outline the user sees.
    ///
    /// A box collapsed to a point or a segment has an outline too: its centre, or the point of the
    /// segment nearest `p`. Without that a zero-height rectangle would report a centre no edge
    /// passes through.
    pub fn nearest_boundary(&self, p: [f32; 2]) -> Option<[f32; 2]> {
        if !self.kind.is_closed() {
            return None;
        }
        let (centre, half) = self.box_geometry();
        if half[0] <= f32::EPSILON && half[1] <= f32::EPSILON {
            return Some(centre);
        }
        let p = self.to_local(p);
        if half[0] <= f32::EPSILON || half[1] <= f32::EPSILON {
            // One axis has no extent: the outline is the segment the box still spans.
            let on = if half[0] <= f32::EPSILON {
                [
                    centre[0],
                    p[1].clamp(centre[1] - half[1], centre[1] + half[1]),
                ]
            } else {
                [
                    p[0].clamp(centre[0] - half[0], centre[0] + half[0]),
                    centre[1],
                ]
            };
            return Some(self.to_world(on));
        }
        let box_ = rect_of(self.from, self.to);
        let hit = match self.kind {
            ShapeKind::Circle => {
                let u = (p[0] - centre[0]) / half[0];
                let v = (p[1] - centre[1]) / half[1];
                let len = (u * u + v * v).sqrt();
                let (u, v) = if len <= f32::EPSILON {
                    (1.0, 0.0)
                } else {
                    (u / len, v / len)
                };
                [centre[0] + u * half[0], centre[1] + v * half[1]]
            }
            ShapeKind::Rect => {
                let corners = [
                    [box_.x, box_.y],
                    [box_.x + box_.w, box_.y],
                    [box_.x + box_.w, box_.y + box_.h],
                    [box_.x, box_.y + box_.h],
                ];
                nearest_on_polygon(p, &corners)
            }
            ShapeKind::Diamond => {
                let corners = [
                    [centre[0], box_.y],
                    [box_.x + box_.w, centre[1]],
                    [centre[0], box_.y + box_.h],
                    [box_.x, centre[1]],
                ];
                nearest_on_polygon(p, &corners)
            }
            ShapeKind::Line | ShapeKind::Arrow => return None,
        };
        Some(self.to_world(hit))
    }

    /// The point where the ray from this shape's centre through `aim` meets the outline the user
    /// sees: a circle or ellipse's curve, a diamond's slanted edge, or a rectangle's side. `None`
    /// for lines and arrows, which have no outline to bind to.
    ///
    /// This is the route a connector end takes when it should leave a shape toward the other end of
    /// its line: the stored [`Attachment`] keeps only a local direction, so the visible boundary is
    /// found from it rather than assumed to be the corner of its box.
    pub fn boundary_toward(&self, aim: [f32; 2]) -> Option<[f32; 2]> {
        if !self.kind.is_closed() {
            return None;
        }
        let local = self.to_local(aim);
        Some(self.to_world(self.local_boundary_toward(local)))
    }

    /// The attachment naming the boundary point on the ray from this shape's centre through `aim`.
    /// Aim it at a connector's other end to bind an end so the line leaves the shape head-on.
    pub fn attachment_toward(&self, aim: [f32; 2]) -> Option<Attachment> {
        self.boundary_toward(aim)
            .map(|point| self.attachment_at(point))
    }

    /// The outline point in the unrotated local frame on the ray from the centre through `local`.
    /// A point at the centre has no direction, so the route falls back to the +x side, the same
    /// convention `nearest_boundary` uses.
    fn local_boundary_toward(&self, local: [f32; 2]) -> [f32; 2] {
        let (centre, half) = self.box_geometry();
        if half[0] <= f32::EPSILON && half[1] <= f32::EPSILON {
            return centre;
        }
        let d = [local[0] - centre[0], local[1] - centre[1]];
        if half[0] <= f32::EPSILON || half[1] <= f32::EPSILON {
            // Collapsed along one axis: the outline is a segment, so aim along its length.
            return if half[0] <= f32::EPSILON {
                [
                    centre[0],
                    (centre[1] + d[1]).clamp(centre[1] - half[1], centre[1] + half[1]),
                ]
            } else {
                [
                    (centre[0] + d[0]).clamp(centre[0] - half[0], centre[0] + half[0]),
                    centre[1],
                ]
            };
        }
        let (u, v) = (d[0] / half[0], d[1] / half[1]);
        let (u, v) = if u.abs() <= f32::EPSILON && v.abs() <= f32::EPSILON {
            (1.0, 0.0)
        } else {
            match self.kind {
                ShapeKind::Circle => {
                    let len = (u * u + v * v).sqrt();
                    (u / len, v / len)
                }
                // Scale the direction until its larger component reaches the box side: the point
                // where the ray leaves a rectangle is the first side it crosses.
                ShapeKind::Rect => {
                    let t = 1.0 / u.abs().max(v.abs());
                    (u * t, v * t)
                }
                // A diamond's sides are where `|u| + |v| == 1`.
                ShapeKind::Diamond => {
                    let t = 1.0 / (u.abs() + v.abs());
                    (u * t, v * t)
                }
                ShapeKind::Line | ShapeKind::Arrow => (1.0, 0.0),
            }
        };
        [centre[0] + u * half[0], centre[1] + v * half[1]]
    }

    /// The attachment that keeps `p`, a point on this shape's outline, on the same local point after
    /// the shape moves, is resized or is turned. A point on a rotated outline is mapped back through
    /// the shape's own angle first, and a box with no extent on an axis stores zero there, which is
    /// what keeps a collapse from producing an infinite local coordinate.
    pub fn attachment_at(&self, p: [f32; 2]) -> Attachment {
        let p = self.to_local(p);
        let (centre, half) = self.box_geometry();
        let at = [
            if half[0] > f32::EPSILON {
                (p[0] - centre[0]) / half[0]
            } else {
                0.0
            },
            if half[1] > f32::EPSILON {
                (p[1] - centre[1]) / half[1]
            } else {
                0.0
            },
        ];
        Attachment { shape: self.id, at }
    }

    /// The world point an attachment names: the same local point, on the rotated outline the user
    /// sees. The stored local point is a direction from the centre, so it is routed onto the real
    /// outline rather than through the corner of the box: an attachment at a circle's box corner
    /// lands on the circle, not outside it.
    pub fn attachment_point(&self, attachment: Attachment) -> [f32; 2] {
        let (centre, half) = self.box_geometry();
        let local = [
            centre[0] + attachment.at[0] * half[0],
            centre[1] + attachment.at[1] * half[1],
        ];
        if self.kind.is_closed() {
            self.to_world(self.local_boundary_toward(local))
        } else {
            self.to_world(local)
        }
    }

    /// Whether this element is a linear element: a polyline with editable vertices rather than a
    /// closed outline.
    pub fn is_linear(&self) -> bool {
        matches!(self.kind, ShapeKind::Line | ShapeKind::Arrow)
    }

    /// Every vertex of a linear element in draw order: `from`, the interior points, `to`. Empty for
    /// a closed shape. The endpoints stay in `from`/`to`, so connector attachments still name the
    /// two ends while the interior points make the line bend.
    pub fn linear_vertices(&self) -> Vec<[f32; 2]> {
        if !self.is_linear() {
            return Vec::new();
        }
        let mut points = Vec::with_capacity(self.points.len() + 2);
        points.push(self.from);
        points.extend_from_slice(&self.points);
        points.push(self.to);
        points
    }

    /// The number of vertices a linear element has.
    pub fn linear_vertex_count(&self) -> usize {
        if self.is_linear() {
            self.points.len() + 2
        } else {
            0
        }
    }

    /// One vertex of a linear element by its place in draw order.
    pub fn linear_vertex(&self, index: usize) -> Option<[f32; 2]> {
        if !self.is_linear() {
            return None;
        }
        match index {
            0 => Some(self.from),
            last if last + 1 == self.linear_vertex_count() => Some(self.to),
            _ => self.points.get(index - 1).copied(),
        }
    }

    /// The midpoint of every segment, which is where a press adds a vertex.
    pub fn linear_midpoints(&self) -> Vec<[f32; 2]> {
        let vertices = self.linear_vertices();
        vertices
            .windows(2)
            .map(|pair| {
                [
                    (pair[0][0] + pair[1][0]) / 2.0,
                    (pair[0][1] + pair[1][1]) / 2.0,
                ]
            })
            .collect()
    }

    /// Move one existing vertex of a linear element. The first and last are the endpoints in
    /// `from`/`to`, so a gesture that moves one keeps connectors bound to the same end.
    pub fn set_linear_vertex(&mut self, index: usize, point: [f32; 2]) -> bool {
        if !self.is_linear() {
            return false;
        }
        if index == 0 {
            self.from = point;
            return true;
        }
        if index + 1 == self.linear_vertex_count() {
            self.to = point;
            return true;
        }
        if let Some(slot) = self.points.get_mut(index - 1) {
            *slot = point;
            return true;
        }
        false
    }

    /// Insert a vertex on `segment` at its midpoint, returning the index the new vertex has in draw
    /// order. The segment is the one between vertex `segment` and `segment + 1`, so segment zero
    /// inserts before every existing interior point and the last segment appends.
    pub fn insert_linear_vertex(&mut self, segment: usize, point: [f32; 2]) -> Option<usize> {
        if !self.is_linear() || segment + 1 >= self.linear_vertex_count() {
            return None;
        }
        self.points.insert(segment, point);
        Some(segment + 1)
    }

    /// Length of the arrowhead, from the pen width with a floor so a hairline arrow still reads as
    /// an arrow.
    pub fn head_len(&self) -> f32 {
        (self.width * ARROW_HEAD_SCALE).max(ARROW_HEAD_MIN)
    }

    /// Half the arrowhead's width: how far it spreads off the line it ends.
    pub fn head_half(&self) -> f32 {
        self.head_len() * ARROW_HEAD_HALF
    }

    /// Whether the shape has nothing to draw: a line whose ends meet, or a box with no extent in one
    /// direction. Such a shape is never written, so a click with a shape tool armed cannot leave an
    /// element that is invisible but still erasable.
    pub fn is_empty(&self) -> bool {
        match self.kind {
            ShapeKind::Line | ShapeKind::Arrow => {
                let vertices = self.linear_vertices();
                vertices.windows(2).all(|pair| pair[0] == pair[1])
            }
            _ => self.from[0] == self.to[0] || self.from[1] == self.to[1],
        }
    }

    /// Axis-aligned box of everything the shape draws, in logical pixels, anti-aliasing and the
    /// arrowhead included. An unrotated rectangle and diamond are inside the box of their two points,
    /// a circle touches the middle of its edges, and the head of an arrow leaves that box. A rotated
    /// closed shape measures the box its turned outline sweeps.
    pub fn bounds(&self) -> Rect {
        let head = match self.kind {
            ShapeKind::Arrow => self.head_half(),
            _ => 0.0,
        };
        let box_ = if self.kind.is_closed() && self.turn_angle() != 0.0 {
            self.rotated_box()
        } else {
            let mut box_ = rect_of(self.from, self.to);
            for point in &self.points {
                box_ = box_.union(Rect {
                    x: point[0],
                    y: point[1],
                    w: 0.0,
                    h: 0.0,
                });
            }
            box_
        };
        box_.grown(self.width / 2.0 + 1.0 + head)
    }

    /// The axis-aligned box a closed outline sweeps once it is turned about the box centre. Each
    /// kind uses its own extreme points, so the frame hugs a turned diamond as tightly as a turned
    /// rectangle, and an ellipse uses the exact extent of the rotated ellipse rather than its box's.
    fn rotated_box(&self) -> Rect {
        let (centre, half) = self.box_geometry();
        let (sin, cos) = self.turn_angle().sin_cos();
        let (sx, cx) = (sin.abs(), cos.abs());
        let (hw, hh) = match self.kind {
            ShapeKind::Circle => (
                ((half[0] * cos).powi(2) + (half[1] * sin).powi(2)).sqrt(),
                ((half[0] * sin).powi(2) + (half[1] * cos).powi(2)).sqrt(),
            ),
            ShapeKind::Rect => (half[0] * cx + half[1] * sx, half[0] * sx + half[1] * cx),
            ShapeKind::Diamond => (
                (half[0] * cx).max(half[1] * sx),
                (half[0] * sx).max(half[1] * cx),
            ),
            ShapeKind::Line | ShapeKind::Arrow => (half[0], half[1]),
        };
        Rect {
            x: centre[0] - hw,
            y: centre[1] - hh,
            w: 2.0 * hw,
            h: 2.0 * hh,
        }
    }

    /// The box this shape paints: its bounds plus the reach of the hand-drawn style. Every shape kind is
    /// drawn by hand, so a damaged region cut from `bounds` would leave the outside of the wander
    /// behind when the shape moves. The frame and the marquee still measure `bounds`, because they are
    /// about the geometry the user drew, not about how it was drawn.
    pub fn paint_bounds(&self) -> Rect {
        self.bounds().grown(SKETCH_SLACK)
    }

    /// The seed the hand-drawn style is drawn from: its kind, colour and width, and the point it was
    /// started at. Not the whole geometry — `to` moves while a shape is being sized, and the mark has
    /// to sit still under the hand — and never the element's index, which a deletion would shift.
    pub fn seed(&self) -> u64 {
        let kind = match self.kind {
            ShapeKind::Circle => 1,
            ShapeKind::Rect => 2,
            ShapeKind::Diamond => 3,
            ShapeKind::Line => 4,
            ShapeKind::Arrow => 5,
        };
        crate::sketch::seed_for(kind, &self.color, self.width, self.from)
    }

    /// The shape as a polyline in logical pixels: what a hit test measures against. A closed shape
    /// repeats its first point, so the closing edge is a segment like any other, and an arrow adds
    /// the three edges of its head, which is enough to be hit anywhere inside it.
    ///
    /// ponytail: the outline is a flat approximation while the frame draws the real curve — a
    /// 96-gon, whose flatness is under a pixel even at 4K, against the eraser's tolerance of eight.
    /// Raise the segment count if a shape ever has to be hit more precisely than that.
    pub fn outline(&self) -> Vec<[f32; 2]> {
        let Rect { x, y, w, h } = rect_of(self.from, self.to);
        let (x1, y1) = (x + w, y + h);
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        let points: Vec<[f32; 2]> = match self.kind {
            ShapeKind::Circle => (0..=ELLIPSE_SEGMENTS)
                .map(|i| {
                    let t = i as f32 / ELLIPSE_SEGMENTS as f32 * std::f32::consts::TAU;
                    [cx + t.cos() * w / 2.0, cy + t.sin() * h / 2.0]
                })
                .collect(),
            ShapeKind::Rect => vec![[x, y], [x1, y], [x1, y1], [x, y1], [x, y]],
            ShapeKind::Diamond => vec![[cx, y], [x1, cy], [cx, y1], [x, cy], [cx, y]],
            ShapeKind::Line => self.linear_vertices(),
            ShapeKind::Arrow => {
                let mut outline = self.linear_vertices();
                if outline.len() < 2 {
                    return outline;
                }
                let (from, to) = (outline[outline.len() - 2], outline[outline.len() - 1]);
                let (dx, dy) = (to[0] - from[0], to[1] - from[1]);
                let len = (dx * dx + dy * dy).sqrt();
                if len <= f32::EPSILON {
                    return outline;
                }
                let (ux, uy) = (dx / len, dy / len);
                let (head, half) = (self.head_len(), self.head_half());
                let base = [to[0] - ux * head, to[1] - uy * head];
                let (px, py) = (-uy * half, ux * half);
                outline.extend([
                    [base[0] + px, base[1] + py],
                    [base[0] - px, base[1] - py],
                    to,
                ]);
                outline
            }
        };
        if self.kind.is_closed() && self.turn_angle() != 0.0 {
            let centre = [cx, cy];
            let angle = self.turn_angle();
            points
                .into_iter()
                .map(|p| rotate_about(p, centre, angle))
                .collect()
        } else {
            points
        }
    }

    /// Squared distance from a point to the shape's outline.
    pub fn distance_squared(&self, x: f32, y: f32) -> f32 {
        let p = [x, y];
        let outline = self.outline();
        match outline.as_slice() {
            [] => f32::INFINITY,
            [only] => dist_squared(p, *only),
            points => points
                .windows(2)
                .map(|w| segment_dist_squared(p, w[0], w[1]))
                .fold(f32::INFINITY, f32::min),
        }
    }

    /// Whether a world point lies inside this closed shape. The point is mapped back into the
    /// shape's unrotated frame first, so a turned rectangle or diamond contains what it looks like it
    /// contains.
    pub fn contains_point(&self, p: [f32; 2]) -> bool {
        if !self.kind.is_closed() {
            return false;
        }
        let (centre, half) = self.box_geometry();
        if half[0] <= f32::EPSILON || half[1] <= f32::EPSILON {
            return false;
        }
        let p = self.to_local(p);
        let box_ = rect_of(self.from, self.to);
        match self.kind {
            ShapeKind::Circle => {
                let u = (p[0] - centre[0]) / half[0];
                let v = (p[1] - centre[1]) / half[1];
                u * u + v * v <= 1.0 + 1e-4
            }
            ShapeKind::Diamond => {
                let u = ((p[0] - centre[0]) / half[0]).abs();
                let v = ((p[1] - centre[1]) / half[1]).abs();
                u + v <= 1.0 + 1e-4
            }
            ShapeKind::Rect => {
                p[0] >= box_.x - 0.01
                    && p[0] <= box_.x + box_.w + 0.01
                    && p[1] >= box_.y - 0.01
                    && p[1] <= box_.y + box_.h + 0.01
            }
            ShapeKind::Line | ShapeKind::Arrow => false,
        }
    }

    /// Whether every corner of `rect` is inside this closed shape.
    pub fn contains_rect(&self, rect: Rect) -> bool {
        [
            [rect.x, rect.y],
            [rect.x + rect.w, rect.y],
            [rect.x + rect.w, rect.y + rect.h],
            [rect.x, rect.y + rect.h],
        ]
        .into_iter()
        .all(|p| self.contains_point(p))
    }
}

impl Stroke {
    /// Axis-aligned box of the drawn geometry in logical pixels, stroke width and the
    /// anti-aliasing edge included. `None` when there is no sample to draw.
    pub fn bounds(&self) -> Option<Rect> {
        let first = self.points.first()?;
        let (mut x0, mut y0, mut x1, mut y1) = (first[0], first[1], first[0], first[1]);
        for p in &self.points[1..] {
            x0 = x0.min(p[0]);
            y0 = y0.min(p[1]);
            x1 = x1.max(p[0]);
            y1 = y1.max(p[1]);
        }
        Some(
            Rect {
                x: x0,
                y: y0,
                w: x1 - x0,
                h: y1 - y0,
            }
            .grown(self.width / 2.0 + 1.0),
        )
    }

    /// The box this stroke paints: its bounds plus the reach of the hand-drawn style, except for a
    /// single sample, which is drawn as an exact disc of the pen's width and does not wander.
    pub fn paint_bounds(&self) -> Option<Rect> {
        let bounds = self.bounds()?;
        Some(if self.points.len() < 2 {
            bounds
        } else {
            bounds.grown(SKETCH_SLACK)
        })
    }

    /// The seed the hand-drawn style is drawn from: its colour and width and the sample it started
    /// from. The anchor rather than the whole path, because a stroke gains a sample per motion event
    /// and hashing all of them would re-roll the mark under the hand.
    pub fn seed(&self) -> u64 {
        let anchor = self.points.first().copied().unwrap_or([0.0, 0.0]);
        crate::sketch::seed_for(0, &self.color, self.width, anchor)
    }

    /// Squared distance from a point to this polyline.
    fn distance_squared(&self, x: f32, y: f32) -> f32 {
        let p = [x, y];
        match self.points.as_slice() {
            [] => f32::INFINITY,
            [only] => dist_squared(p, *only),
            pts => pts
                .windows(2)
                .map(|w| segment_dist_squared(p, w[0], w[1]))
                .fold(f32::INFINITY, f32::min),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x <= self.x + self.w && y >= self.y && y <= self.y + self.h
    }

    /// Smallest rectangle covering both.
    pub fn union(self, other: Rect) -> Rect {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = (self.x + self.w).max(other.x + other.w);
        let bottom = (self.y + self.h).max(other.y + other.h);
        Rect {
            x,
            y,
            w: right - x,
            h: bottom - y,
        }
    }

    /// Whether the two rectangles share any area. Touching edges do not count.
    pub fn intersects(&self, other: Rect) -> bool {
        self.x < other.x + other.w
            && other.x < self.x + self.w
            && self.y < other.y + other.h
            && other.y < self.y + self.h
    }

    /// The same rectangle with every edge moved `by` outwards.
    pub fn grown(self, by: f32) -> Rect {
        Rect {
            x: self.x - by,
            y: self.y - by,
            w: self.w + 2.0 * by,
            h: self.h + 2.0 * by,
        }
    }

    /// The rectangle two opposite corners span. The corners may come in any order, and the
    /// result never has a negative extent, so a marquee dragged up and to the left is the same
    /// rectangle as the equivalent drag down and to the right.
    pub fn from_corners(a: [f32; 2], b: [f32; 2]) -> Rect {
        Rect {
            x: a[0].min(b[0]),
            y: a[1].min(b[1]),
            w: (b[0] - a[0]).abs(),
            h: (b[1] - a[1]).abs(),
        }
    }

    pub fn centre(self) -> [f32; 2] {
        [self.x + self.w / 2.0, self.y + self.h / 2.0]
    }
}

/// A resize handle of the selection frame: the four corners and the four edge midpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handle {
    Nw,
    N,
    Ne,
    E,
    Se,
    S,
    Sw,
    W,
}

impl Handle {
    /// Every handle, in the order the frame draws them.
    pub const ALL: [Handle; 8] = [
        Handle::Nw,
        Handle::N,
        Handle::Ne,
        Handle::E,
        Handle::Se,
        Handle::S,
        Handle::Sw,
        Handle::W,
    ];

    /// The point of `frame` this handle is centred on.
    pub fn point(self, frame: Rect) -> [f32; 2] {
        let (l, r, t, b) = (frame.x, frame.x + frame.w, frame.y, frame.y + frame.h);
        let (cx, cy) = (l + frame.w / 2.0, t + frame.h / 2.0);
        match self {
            Handle::Nw => [l, t],
            Handle::N => [cx, t],
            Handle::Ne => [r, t],
            Handle::E => [r, cy],
            Handle::Se => [r, b],
            Handle::S => [cx, b],
            Handle::Sw => [l, b],
            Handle::W => [l, cy],
        }
    }

    /// The handle across the frame from this one. Its point is the anchor a resize keeps still.
    fn opposite(self) -> Handle {
        match self {
            Handle::Nw => Handle::Se,
            Handle::N => Handle::S,
            Handle::Ne => Handle::Sw,
            Handle::E => Handle::W,
            Handle::Se => Handle::Nw,
            Handle::S => Handle::N,
            Handle::Sw => Handle::Ne,
            Handle::W => Handle::E,
        }
    }

    fn moves_x(self) -> bool {
        matches!(
            self,
            Handle::Nw | Handle::Ne | Handle::E | Handle::Se | Handle::Sw | Handle::W
        )
    }

    fn moves_y(self) -> bool {
        matches!(
            self,
            Handle::Nw | Handle::N | Handle::Ne | Handle::Se | Handle::S | Handle::Sw
        )
    }
}

/// What a press on the selection frame grabs, if it grabs anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grab {
    Handle(Handle),
    Rotate,
}

/// A point handle of a single selected linear element. A vertex moves an existing point; a midpoint
/// inserts a new point on that segment and then drags it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinearHandle {
    Vertex(usize),
    Midpoint(usize),
}

/// What a press with the selection tool resolves to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Press {
    /// A handle of the frame: a resize or a rotation.
    Handle(Grab),
    /// A point or segment midpoint of a single selected line or arrow.
    Linear(LinearHandle),
    /// Drag what is already held.
    Move,
    /// Take the element under the pointer, which replaces the selection.
    Element(ItemId),
    /// Sweep a marquee: the press is on empty space outside the frame.
    Marquee { canvas: [f32; 2] },
    /// Add the element under a `Shift` press to the selection, or remove it when it is held.
    Toggle(ItemId),
    /// A `Shift` press that grabbed nothing: it changes nothing and starts nothing.
    Inert,
}

/// The one linear element a selection holds, if it holds exactly one. The point handles belong to a
/// single line or arrow; a cluster keeps the ordinary frame so its bounds can still be resized.
pub fn selected_linear(ann: &Board, held: &[ItemId]) -> Option<usize> {
    let [ItemId::Shape(index)] = held else {
        return None;
    };
    ann.shapes
        .get(*index)
        .filter(|shape| shape.is_linear())
        .map(|_| *index)
}

/// The point handle at a screen position. A vertex wins over a midpoint, and in selection mode only
/// the two endpoints are grabbable; edit mode exposes every interior vertex as well.
pub fn linear_handle_at(
    shape: &Shape,
    view: View,
    screen: [f32; 2],
    edit: bool,
) -> Option<LinearHandle> {
    let limit = SELECT_HANDLE_HIT * SELECT_HANDLE_HIT;
    let count = shape.linear_vertex_count();
    if count == 0 {
        return None;
    }
    for index in 0..count {
        if !edit && index != 0 && index + 1 != count {
            continue;
        }
        let Some(point) = shape.linear_vertex(index) else {
            continue;
        };
        if dist_squared(screen, view.to_screen(point)) <= limit {
            return Some(LinearHandle::Vertex(index));
        }
    }
    let limit = LINEAR_MIDPOINT_HIT * LINEAR_MIDPOINT_HIT;
    shape
        .linear_midpoints()
        .into_iter()
        .enumerate()
        .find(|(_, point)| dist_squared(screen, view.to_screen(*point)) <= limit)
        .map(|(segment, _)| LinearHandle::Midpoint(segment))
}

/// Resolve a press with the selection tool, decided here so that a test can check it: `screen` is where
/// the pointer is, and the frame is measured in canvas coordinates and mapped to screen **only** to find
/// the handles, because that is where they are drawn. `shift` toggles the element under the pointer into
/// or out of the selection, after the handles have had their say and instead of any other press.
///
/// The two coordinate spaces are the whole of this function. A gesture and a marquee move the document,
/// so they are canvas; the handles are interface-sized, so they are screen. Confusing the two makes a
/// marquee drift away from the pointer the moment the view is panned or zoomed, which is what this
/// signature exists to prevent.
pub fn selection_press(
    ann: &Board,
    held: &[ItemId],
    view: View,
    screen: [f32; 2],
    tolerance: f32,
    shift: bool,
) -> Press {
    selection_press_with(ann, held, view, screen, tolerance, shift, &[])
}

/// The press, with the texts measured: the handles are drawn on the measured frame, so they have to be
/// grabbed on it too.
pub fn selection_press_with(
    ann: &Board,
    held: &[ItemId],
    view: View,
    screen: [f32; 2],
    tolerance: f32,
    shift: bool,
    texts: &[(usize, Rect)],
) -> Press {
    selection_press_full(ann, held, view, screen, tolerance, shift, texts, false)
}

/// The press for a single linear selection, with the edit-mode flag the caller keeps on the output.
/// In selection mode only the endpoints are vertex handles; edit mode exposes the interior points
/// too. Both modes show a small midpoint handle on every segment.
#[allow(clippy::too_many_arguments)]
pub fn selection_press_linear(
    ann: &Board,
    held: &[ItemId],
    view: View,
    screen: [f32; 2],
    tolerance: f32,
    shift: bool,
    texts: &[(usize, Rect)],
    linear_edit: bool,
) -> Press {
    selection_press_full(
        ann,
        held,
        view,
        screen,
        tolerance,
        shift,
        texts,
        linear_edit,
    )
}

#[allow(clippy::too_many_arguments)]
fn selection_press_full(
    ann: &Board,
    held: &[ItemId],
    view: View,
    screen: [f32; 2],
    tolerance: f32,
    shift: bool,
    texts: &[(usize, Rect)],
    linear_edit: bool,
) -> Press {
    let canvas = view.to_canvas(screen);
    let linear = selected_linear(ann, held);
    if let Some(index) = linear
        && let Some(handle) = linear_handle_at(&ann.shapes[index], view, screen, linear_edit)
    {
        return Press::Linear(handle);
    }
    let frame = selection_frame_with(ann, held, None, texts);
    // A linear selection draws point handles instead of the eight-handle frame, so only the points
    // may be grabbed. A cluster with a line in it still keeps the ordinary frame.
    if linear.is_none()
        && let Some(frame) = frame
        && let Some(grab) = grab_at(
            view.screen_rect(frame),
            screen[0],
            screen[1],
            SELECT_HANDLE_HIT,
        )
    {
        return Press::Handle(grab);
    }
    // A `Shift` press toggles the element under the pointer and starts nothing else; on empty space it
    // does nothing at all. The handle test above still comes first, so `Shift` + a handle resizes.
    if shift {
        return match ann.hit(canvas[0], canvas[1], tolerance) {
            Some(id) => Press::Toggle(id),
            None => Press::Inert,
        };
    }
    match ann.hit(canvas[0], canvas[1], tolerance) {
        // An element the selection does not hold replaces it, wherever the press landed.
        Some(id) if !held.contains(&id) => Press::Element(id),
        // An element it does hold keeps the whole selection, so a second press inside a cluster drags
        // the cluster rather than dropping to one element.
        Some(_) => Press::Move,
        // Nothing under the pointer: inside the frame that drags the whole selection, which is what
        // makes a cluster grabbable by its box rather than only by one of its elements. Outside it,
        // the marquee decides the selection on release.
        None => match frame.filter(|f| view.screen_rect(*f).contains(screen[0], screen[1])) {
            Some(_) => Press::Move,
            // A closed shape is an object, not a hole: a press inside it picks it and the group it
            // encloses. The topmost shape wins, so an inner shape remains reachable without having
            // to hit its outline exactly.
            None => match ann
                .shapes
                .iter()
                .rposition(|shape| shape.kind.is_closed() && shape.contains_point(canvas))
                .filter(|index| !held.contains(&ItemId::Shape(*index)))
            {
                Some(index) => Press::Element(ItemId::Shape(index)),
                None => Press::Marquee { canvas },
            },
        },
    }
}

/// The selection `items` implies once a closed shape takes what it encloses. A circle, rectangle or
/// diamond holds every stroke, text box, note and shape whose whole geometry lies inside it, and a
/// selected child shape carries its own contents in turn. An element picked directly stays on its
/// own; the container is what pulls a group together.
pub fn selection_with_contents(ann: &Board, items: &[ItemId]) -> Vec<ItemId> {
    let mut selected = items.to_vec();
    let mut containers: Vec<usize> = items
        .iter()
        .filter_map(|item| match *item {
            ItemId::Shape(index)
                if ann
                    .shapes
                    .get(index)
                    .is_some_and(|shape| shape.kind.is_closed()) =>
            {
                Some(index)
            }
            _ => None,
        })
        .collect();
    let mut scanned = vec![false; ann.shapes.len()];
    for &index in &containers {
        if let Some(slot) = scanned.get_mut(index) {
            *slot = true;
        }
    }
    let mut at = 0;
    while at < containers.len() {
        let container_index = containers[at];
        at += 1;
        let Some(container) = ann.shapes.get(container_index) else {
            continue;
        };
        let mut add = |id: ItemId| {
            if !selected.contains(&id) {
                selected.push(id);
                true
            } else {
                false
            }
        };
        for (index, stroke) in ann.strokes.iter().enumerate() {
            if !stroke.points.is_empty()
                && stroke.points.iter().all(|p| container.contains_point(*p))
            {
                add(ItemId::Stroke(index));
            }
        }
        for (index, text) in ann.texts.iter().enumerate() {
            if container.contains_rect(text.bounds()) {
                add(ItemId::Text(index));
            }
        }
        for (index, note) in ann.notes.iter().enumerate() {
            if container.contains_rect(note.bounds()) {
                add(ItemId::Note(index));
            }
        }
        for (index, shape) in ann.shapes.iter().enumerate() {
            if index == container_index {
                continue;
            }
            let outline = shape.outline();
            if !outline.is_empty()
                && outline.iter().all(|p| container.contains_point(*p))
                && add(ItemId::Shape(index))
                && shape.kind.is_closed()
                && !scanned[index]
            {
                scanned[index] = true;
                containers.push(index);
            }
        }
    }
    selected
}

/// The selection a `Shift` press on `id` produces: the element appended when it was not held, removed
/// when it was, and every other element left where it was in the order.
pub fn toggle_selection(held: &[ItemId], id: ItemId) -> Vec<ItemId> {
    let mut items = held.to_vec();
    match items.iter().position(|held| *held == id) {
        Some(index) => {
            items.remove(index);
        }
        None => items.push(id),
    }
    items
}

/// `Shift` toggling a closed shape toggles the contents it encloses with it: adding the shape adds
/// them, and removing it removes the group that came in with it.
pub fn toggle_selection_with_contents(ann: &Board, held: &[ItemId], id: ItemId) -> Vec<ItemId> {
    let target = selection_with_contents(ann, &[id]);
    let mut items = held.to_vec();
    if items.contains(&id) {
        items.retain(|item| !target.contains(item));
    } else {
        for item in target {
            if !items.contains(&item) {
                items.push(item);
            }
        }
    }
    items
}

/// A selection gesture in progress: what it grabbed and where the pointer went down. Held by the
/// daemon rather than the document, because nothing about a gesture exists until it is released.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Gesture {
    /// A marquee is being swept: it decides the selection when it is released.
    Marquee {
        origin: [f32; 2],
    },
    Move {
        origin: [f32; 2],
        /// Whether this drag copies what it holds, which is `Alt` read at the press: a copy that appeared
        /// because a key was pressed while dragging would be a copy nobody asked for.
        duplicate: bool,
    },
    Resize {
        handle: Handle,
        frame: Rect,
        origin: [f32; 2],
    },
    Rotate {
        pivot: [f32; 2],
        origin: [f32; 2],
    },
}

impl Gesture {
    /// The gesture a press at `(x, y)` starts: what it grabbed on the frame, or a move of the
    /// elements already held when it grabbed nothing. `alt` is read here and nowhere else, so whether a
    /// drag copies is decided by the press.
    pub fn start(grab: Option<Grab>, frame: Rect, x: f32, y: f32, alt: bool) -> Gesture {
        let origin = [x, y];
        match grab {
            Some(Grab::Handle(handle)) => Gesture::Resize {
                handle,
                frame,
                origin,
            },
            Some(Grab::Rotate) => Gesture::Rotate {
                pivot: frame.centre(),
                origin,
            },
            None => Gesture::Move {
                origin,
                duplicate: alt,
            },
        }
    }

    /// The transform this gesture implies for a pointer now at `(x, y)`. A resize scales by the
    /// pointer's own displacement from where it went down, so a press that landed beside a handle
    /// does not make the frame jump. `None` for a marquee, which transforms nothing.
    pub fn transform(self, at: [f32; 2], uniform: bool) -> Option<Transform> {
        Some(match self {
            Gesture::Marquee { .. } => return None,
            Gesture::Move { origin, .. } => Transform::Translate {
                dx: at[0] - origin[0],
                dy: at[1] - origin[1],
            },
            Gesture::Resize {
                handle,
                frame,
                origin,
            } => {
                let anchor = handle.opposite().point(frame);
                let moving = handle.point(frame);
                let factor = |m: f32, a: f32, o: f32, now: f32| {
                    let span = m - a;
                    if span.abs() < f32::EPSILON || (o - a).abs() < f32::EPSILON {
                        1.0
                    } else {
                        ((now - a) / (o - a)).max(MIN_SCALE)
                    }
                };
                let sx = if handle.moves_x() {
                    factor(moving[0], anchor[0], origin[0], at[0])
                } else {
                    1.0
                };
                let sy = if handle.moves_y() {
                    factor(moving[1], anchor[1], origin[1], at[1])
                } else {
                    1.0
                };
                // `Shift` keeps the shape of what is held: one factor for both axes. A corner handle has
                // two to choose from and takes the one the pointer moved further along — measured against
                // 1.0, so that a shrink is chosen over standing still — and an edge handle has only its
                // own axis, so the edge the pointer is on still follows the pointer.
                let (sx, sy) = if uniform {
                    let s = if handle.moves_x()
                        && handle.moves_y()
                        && (sy - 1.0).abs() > (sx - 1.0).abs()
                    {
                        sy
                    } else {
                        sx
                    };
                    (s, s)
                } else {
                    (sx, sy)
                };
                Transform::Scale { anchor, sx, sy }
            }
            Gesture::Rotate { pivot, origin } => Transform::Turn {
                pivot,
                radians: angle_of(at, pivot) - angle_of(origin, pivot),
            },
        })
    }
}

/// A two-dimensional affine map, stored as the six meaningful entries of a homogeneous 3×3 matrix:
///
/// ```text
/// | m00 m01 m02 |   | x |   | m00*x + m01*y + m02 |
/// | m10 m11 m12 | * | y | = | m10*x + m11*y + m12 |
/// |  0   0   1  |   | 1 |   |          1          |
/// ```
///
/// `Transform` is the small, tagged vocabulary a gesture speaks; this is the arithmetic every
/// mapping goes through, and the value a composed transform becomes. Composition order is
/// explicit: [`Affine::then`] applies `self` first and its argument second, while `a * b` is the
/// ordinary matrix product and therefore applies `b` first, then `a`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Affine {
    /// Row-major 2×3 entries. The hidden third row is always `[0, 0, 1]`.
    pub m: [[f32; 3]; 3],
}

impl Affine {
    /// The map that leaves every point where it is.
    pub const IDENTITY: Affine = Affine {
        m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
    };

    /// A pure translation by `(dx, dy)`.
    pub fn translation(dx: f32, dy: f32) -> Affine {
        Affine {
            m: [[1.0, 0.0, dx], [0.0, 1.0, dy], [0.0, 0.0, 1.0]],
        }
    }

    /// Scaling by `(sx, sy)` about `anchor`, which is the point that does not move.
    pub fn scaling(anchor: [f32; 2], sx: f32, sy: f32) -> Affine {
        Affine {
            m: [
                [sx, 0.0, anchor[0] - anchor[0] * sx],
                [0.0, sy, anchor[1] - anchor[1] * sy],
                [0.0, 0.0, 1.0],
            ],
        }
    }

    /// A rotation by `radians` about `pivot`, counter-clockwise in the canvas' y-down frame exactly
    /// as [`rotate_about`] and the shape outline have always done it.
    pub fn rotation(pivot: [f32; 2], radians: f32) -> Affine {
        let (sin, cos) = radians.sin_cos();
        Affine {
            m: [
                [cos, -sin, pivot[0] - pivot[0] * cos + pivot[1] * sin],
                [sin, cos, pivot[1] - pivot[0] * sin - pivot[1] * cos],
                [0.0, 0.0, 1.0],
            ],
        }
    }

    /// Where this map sends `p`.
    pub fn apply(self, p: [f32; 2]) -> [f32; 2] {
        let m = self.m;
        [
            m[0][0] * p[0] + m[0][1] * p[1] + m[0][2],
            m[1][0] * p[0] + m[1][1] * p[1] + m[1][2],
        ]
    }

    /// The map that applies `self` first and `next` second. Read left to right, unlike [`Mul`],
    /// which is the matrix product: `self.then(next) == next * self`.
    pub fn then(self, next: Affine) -> Affine {
        next * self
    }

    /// The determinant of the linear part: zero for a map that collapses the plane and cannot be
    /// undone.
    pub fn determinant(self) -> f32 {
        self.m[0][0] * self.m[1][1] - self.m[0][1] * self.m[1][0]
    }

    /// The inverse map, when `self` is invertible. A determinant at or below the float epsilon is
    /// treated as collapsed, so a near-zero scale does not produce infinities that poison geometry.
    pub fn inverse(self) -> Option<Affine> {
        let det = self.determinant();
        if !det.is_finite() || det.abs() <= f32::EPSILON {
            return None;
        }
        let inv = 1.0 / det;
        let m = self.m;
        let a = m[1][1] * inv;
        let b = -m[0][1] * inv;
        let c = -m[1][0] * inv;
        let d = m[0][0] * inv;
        Some(Affine {
            m: [
                [a, b, -(a * m[0][2] + b * m[1][2])],
                [c, d, -(c * m[0][2] + d * m[1][2])],
                [0.0, 0.0, 1.0],
            ],
        })
    }

    /// Whether this maps every point to itself, exactly. A rotation by a whole turn is not the
    /// identity here: its sine is not zero, and the tiny residual is a real (if invisible) move.
    pub fn is_identity(self) -> bool {
        let m = self.m;
        m[0][0] == 1.0
            && m[0][1] == 0.0
            && m[0][2] == 0.0
            && m[1][0] == 0.0
            && m[1][1] == 1.0
            && m[1][2] == 0.0
            && m[2] == [0.0, 0.0, 1.0]
    }
}

impl std::ops::Mul for Affine {
    type Output = Affine;

    /// The matrix product: the map that applies `rhs` first, then `self`. This is the convention
    /// [`Affine::then`] documents from the other side.
    fn mul(self, rhs: Affine) -> Affine {
        let (a, b) = (self.m, rhs.m);
        // Only the six meaningful entries are computed; the hidden row stays `[0, 0, 1]`, so a
        // composed map is always a well-formed affine one.
        Affine {
            m: [
                [
                    a[0][0] * b[0][0] + a[0][1] * b[1][0],
                    a[0][0] * b[0][1] + a[0][1] * b[1][1],
                    a[0][0] * b[0][2] + a[0][1] * b[1][2] + a[0][2],
                ],
                [
                    a[1][0] * b[0][0] + a[1][1] * b[1][0],
                    a[1][0] * b[0][1] + a[1][1] * b[1][1],
                    a[1][0] * b[0][2] + a[1][1] * b[1][2] + a[1][2],
                ],
                [0.0, 0.0, 1.0],
            ],
        }
    }
}

/// What a gesture does to the geometry it holds. Applied live while the gesture runs and once more
/// when it is written into the document, so the preview and the commit are the same arithmetic on
/// the same data and cannot disagree.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Transform {
    Translate { dx: f32, dy: f32 },
    Scale { anchor: [f32; 2], sx: f32, sy: f32 },
    Turn { pivot: [f32; 2], radians: f32 },
}

impl Transform {
    /// Whether this leaves every point where it is, which is what makes a press that did not move
    /// write nothing.
    pub fn is_identity(self) -> bool {
        match self {
            Transform::Translate { dx, dy } => dx == 0.0 && dy == 0.0,
            Transform::Scale { sx, sy, .. } => sx == 1.0 && sy == 1.0,
            Transform::Turn { radians, .. } => radians == 0.0,
        }
    }

    /// The affine map this transform applies. Every point mapping goes through it, so a composed
    /// transform and a single gesture agree by construction.
    pub fn matrix(self) -> Affine {
        match self {
            Transform::Translate { dx, dy } => Affine::translation(dx, dy),
            Transform::Scale { anchor, sx, sy } => Affine::scaling(anchor, sx, sy),
            Transform::Turn { pivot, radians } => Affine::rotation(pivot, radians),
        }
    }

    /// A composed map that applies this transform first and `next` second. The result is an
    /// [`Affine`] rather than a `Transform` because the tagged enum cannot hold a rotation and a
    /// scale together; the order is the same as `self.matrix().then(next.matrix())`.
    pub fn then(self, next: Transform) -> Affine {
        self.matrix().then(next.matrix())
    }

    pub fn point(self, p: [f32; 2]) -> [f32; 2] {
        self.matrix().apply(p)
    }

    /// The stroke this makes of one the document holds. The live preview draws this copy and the
    /// commit writes it, so a turned stroke is an ordinary stroke the moment it lands.
    pub fn stroke(self, s: &Stroke) -> Stroke {
        let matrix = self.matrix();
        Stroke {
            points: s.points.iter().map(|p| matrix.apply(*p)).collect(),
            ..s.clone()
        }
    }

    /// The text box this makes of one the document holds. Only the origin moves and only the size
    /// follows the vertical factor: a text box has no angle to turn, so a rotation carries its
    /// glyphs upright around the pivot.
    pub fn text(self, t: &TextItem) -> TextItem {
        let mut out = t.clone();
        let origin = self.point([t.x, t.y]);
        out.x = origin[0];
        out.y = origin[1];
        if let Transform::Scale { sy, .. } = self {
            out.size = (t.size * sy).clamp(TEXT_SIZE_MIN, TEXT_SIZE_MAX);
        }
        out
    }

    /// The note this makes of one the document holds: its origin moved, and its square scaled. Its text
    /// re-wraps at the new width, so a resize sets how narrow the note may be rather than how small its
    /// text is, and its glyphs stay upright as a text box's do.
    pub fn note(self, n: &Note) -> Note {
        let origin = self.point([n.x, n.y]);
        let mut out = n.clone();
        out.x = origin[0];
        out.y = origin[1];
        if let Transform::Scale { sx, sy, .. } = self {
            out.w = (out.w * sx).max(NOTE_MIN_SIZE);
            out.h = (out.h * sy).max(NOTE_MIN_SIZE);
        }
        out
    }

    /// The shape this makes of one the document holds: its two points moved, with its kind and its
    /// pen width untouched, so a scaled circle stays a circle and a turned arrow keeps its head.
    ///
    /// A closed shape also carries its own angle. A turn moves its centre about the pivot and adds
    /// the same angle to that state, so the shape keeps its size and turns about the pivot instead
    /// of being squashed into a new axis-aligned box. Lines and arrows have no such state: turning
    /// their two points is the whole of their turn.
    pub fn shape(self, s: &Shape) -> Shape {
        let matrix = self.matrix();
        let mut out = Shape {
            points: s.points.iter().map(|point| matrix.apply(*point)).collect(),
            from: matrix.apply(s.from),
            to: matrix.apply(s.to),
            ..s.clone()
        };
        if let Transform::Turn { radians, .. } = self
            && s.kind.is_closed()
        {
            let centre = s.centre();
            let turned = matrix.apply(centre);
            out.from = [
                s.from[0] + turned[0] - centre[0],
                s.from[1] + turned[1] - centre[1],
            ];
            out.to = [
                s.to[0] + turned[0] - centre[0],
                s.to[1] + turned[1] - centre[1],
            ];
            out.rotate = s.rotate + radians;
        }
        out
    }
}

/// What the selection tool holds, and the live gesture acting on it. Transient: never part of the
/// document, never persisted, and dropped when the tool changes or the surfaces are recreated.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Selection {
    pub items: Vec<ItemId>,
    /// The transform the pointer is dragging, if any. `Some` keeps the held elements out of the
    /// document cache for the duration, so the frame draws them from here instead.
    pub transform: Option<Transform>,
}

impl Selection {
    /// The elements a frame has to draw live: the held ones, while a gesture is dragging them.
    pub fn live(&self) -> &[ItemId] {
        match self.transform {
            Some(_) => &self.items,
            None => &[],
        }
    }
}

impl TextItem {
    /// Number of lines and the width in characters of the longest one.
    fn extent(&self) -> (f32, f32) {
        let lines = self.text.lines().count().max(1) as f32;
        let chars = self
            .text
            .lines()
            .map(|l| l.chars().count())
            .max()
            .unwrap_or(0)
            .max(1) as f32;
        (lines, chars)
    }

    /// ponytail: bounds estimated from the font size (0.6em per char wide, 1.2em per line high)
    /// instead of real glyph metrics; if erasing feels off, write back measured bounds while rendering.
    pub fn bounds(&self) -> Rect {
        let (lines, chars) = self.extent();
        Rect {
            x: self.x,
            y: self.y,
            w: 0.6 * self.size * chars,
            h: 1.2 * self.size * lines,
        }
    }

    /// Conservative box of everything the renderer can paint for this item. `bounds()` is
    /// tuned for the eraser and is too narrow for wide glyphs such as CJK, but a damaged
    /// frame may never paint outside the box it declared, so this one errs wide.
    /// ponytail: one em per character plus half an em of margin, not real glyph metrics.
    pub fn paint_bounds(&self) -> Rect {
        let (lines, chars) = self.extent();
        Rect {
            x: self.x,
            y: self.y,
            w: self.size * chars,
            h: 1.2 * self.size * lines,
        }
        .grown(self.size * 0.5)
    }
}

/// The rectangle two points span, in any order.
pub fn rect_of(a: [f32; 2], b: [f32; 2]) -> Rect {
    Rect::from_corners(a, b)
}

/// How one output is looking at the canvas: `screen = canvas * zoom + offset`, both in logical pixels.
/// Not part of the document and never persisted, because it is a way of looking at a drawing rather
/// than something the drawing is. The canvas itself has no extent: any coordinate is a real place, so
/// there is no clamping, no edge and no growth rule to keep in step with the content.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    pub zoom: f32,
    /// Where the canvas origin is on screen, in logical pixels.
    pub offset: [f32; 2],
}

impl Default for View {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            offset: [0.0, 0.0],
        }
    }
}

impl View {
    /// The same view moved so a canvas point sits in the middle of an output of this size. The zoom
    /// is untouched: a link says where to look, not how closely.
    pub fn centred_on(self, size: (f32, f32), canvas: [f32; 2]) -> View {
        View {
            zoom: self.zoom,
            offset: [
                size.0 / 2.0 - canvas[0] * self.zoom,
                size.1 / 2.0 - canvas[1] * self.zoom,
            ],
        }
    }

    /// Whether this view is the one a frame drawn before the view existed would have used.
    pub fn is_identity(self) -> bool {
        self.zoom == 1.0 && self.offset == [0.0, 0.0]
    }

    pub fn to_screen(self, p: [f32; 2]) -> [f32; 2] {
        [
            p[0] * self.zoom + self.offset[0],
            p[1] * self.zoom + self.offset[1],
        ]
    }

    pub fn to_canvas(self, p: [f32; 2]) -> [f32; 2] {
        [
            (p[0] - self.offset[0]) / self.zoom,
            (p[1] - self.offset[1]) / self.zoom,
        ]
    }

    /// Pan by a screen delta: the drawing moves with the pointer 1:1, which is what makes a grab feel
    /// like a grab.
    pub fn pan_by(&mut self, delta: [f32; 2]) {
        self.offset[0] += delta[0];
        self.offset[1] += delta[1];
    }

    /// Zoom by `factor` about a screen point, which stays where it is. The zoom clamps, and the anchor
    /// is honoured whatever the clamp did, so a burst of wheel events lands on the limit rather than
    /// overshooting it and needing a burst back.
    pub fn zoom_about(&mut self, anchor: [f32; 2], factor: f32) {
        let canvas = self.to_canvas(anchor);
        self.zoom_onto(factor, canvas, anchor);
    }

    /// Zoom by `factor` so that a given canvas point lands under `anchor` on screen. This is what a
    /// pinch needs and a wheel does not: the fingers both move and change their distance, so the
    /// canvas point they started on has to end up under where they are now.
    pub fn zoom_onto(&mut self, factor: f32, canvas: [f32; 2], anchor: [f32; 2]) {
        self.zoom = (self.zoom * factor).clamp(ZOOM_MIN, ZOOM_MAX);
        self.offset = [
            anchor[0] - canvas[0] * self.zoom,
            anchor[1] - canvas[1] * self.zoom,
        ];
    }

    /// A canvas rectangle as the screen rectangle it covers.
    pub fn screen_rect(self, r: Rect) -> Rect {
        let origin = self.to_screen([r.x, r.y]);
        Rect {
            x: origin[0],
            y: origin[1],
            w: r.w * self.zoom,
            h: r.h * self.zoom,
        }
    }

    /// A screen length as a canvas length: what the pointer aims with, such as the eraser's radius,
    /// expressed in the document's own units.
    pub fn canvas_len(self, screen: f32) -> f32 {
        screen / self.zoom
    }
}

/// A zoom in flight. A wheel notch, a pinch and a key ask for one of these instead of setting the
/// view's zoom, so a burst of events becomes one continuous transition that lands on the last target
/// rather than one jump per event — and so a transition is drawn a frame at a time instead of
/// arriving already finished.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Zoom {
    /// Where the view's zoom is heading, already clamped.
    pub target: f32,
    /// The zoom the transition started from, so a retarget mid-flight does not jump.
    from: f32,
    /// The canvas point the transition holds still.
    pub canvas: [f32; 2],
    /// Where on screen that point is held.
    pub anchor: [f32; 2],
    /// How long the transition has been running, in seconds.
    elapsed: f32,
}

impl Zoom {
    /// The transition a zoom request asks for. `base` is where the zoom is already heading, not where
    /// the view is now, so a burst of wheel notches compounds and the last one decides where the view
    /// lands.
    pub fn request(view: View, base: f32, canvas: [f32; 2], anchor: [f32; 2], factor: f32) -> Self {
        Self {
            target: (base * factor).clamp(ZOOM_MIN, ZOOM_MAX),
            from: view.zoom,
            canvas,
            anchor,
            elapsed: 0.0,
        }
    }

    /// Advance the view by `dt` seconds. Returns whether the transition is still running; the frame
    /// that lands sets the zoom to the target exactly, so nothing drifts and nothing is left to do.
    pub fn step(&mut self, view: &mut View, dt: f32) -> bool {
        self.elapsed += dt.max(0.0);
        let t = (self.elapsed / ZOOM_SECONDS).clamp(0.0, 1.0);
        // Ease-out, because the movement is a gesture settling rather than a thing being thrown.
        let eased = 1.0 - (1.0 - t).powi(3);
        let wanted = self.from + (self.target - self.from) * eased;
        // The anchor is honoured through the same path a one-shot zoom takes, so the point under the
        // pointer stays under it for the whole transition.
        let factor = if view.zoom > 0.0 {
            wanted / view.zoom
        } else {
            1.0
        };
        view.zoom_onto(factor, self.canvas, self.anchor);
        if t >= 1.0 {
            // Land on the target exactly. The ratio above rounds, and a zoom that stopped a hair
            // short would leave the last fraction of the transition on screen for good.
            view.zoom = self.target;
            view.offset = [
                self.anchor[0] - self.canvas[0] * view.zoom,
                self.anchor[1] - self.canvas[1] * view.zoom,
            ];
        }
        t < 1.0
    }
}

/// How long a zoom takes to land, in seconds: the same short-motion limit the rail's pop-out keeps
/// (`TOOLBAR_ANIM`), because a view change and an interface transition are the same kind of motion.
pub const ZOOM_SECONDS: f32 = 0.14;

/// The pointer speed above which a moving canvas is drawn at a coarser rung, in logical pixels a
/// second. Below it, the canvas is drawn at full resolution — so a slow adjustment, and every frame
/// after the pointer stops, is crisp.
pub const SETTLE_SPEED: f32 = 900.0;

/// The rung a fast gesture is drawn at: the document is rasterised at this fraction of the surface's
/// own pixels and blitted up. Half is a quarter of the work and is not readable as a loss while the
/// drawing is moving; the overlays stay at full resolution either way.
pub const MOTION_QUALITY: f32 = 0.5;

/// How quickly the measured speed follows the pointer, in seconds. It has to be long enough that one
/// slow event between fast ones does not flip the rung and short enough that the drawing is crisp
/// again a few frames after the pointer stops.
const MOTION_TAU: f32 = 0.05;

/// How fast the pointer is moving on one output, smoothed. It decides which rung the document is
/// drawn at: a gesture's speed, never the amount on screen, so the same gesture looks the same over a
/// crowded canvas and an empty one.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct Motion {
    speed: f32,
}

impl Motion {
    /// One pointer movement of `distance` logical pixels, over `dt` seconds.
    pub fn moved(&mut self, distance: f32, dt: f32) {
        let instant = if dt > 0.0 { distance / dt } else { 0.0 };
        let follow = (dt / MOTION_TAU).clamp(0.0, 1.0);
        self.speed += (instant - self.speed) * follow;
    }

    /// Time passing with the pointer where it is: the speed decays, so a canvas that has stopped is
    /// crisp again without waiting for another event.
    pub fn idle(&mut self, dt: f32) {
        self.moved(0.0, dt);
    }

    /// The speed this motion has been measured at, in logical pixels a second.
    pub fn speed(self) -> f32 {
        self.speed
    }

    /// The rung the document is drawn at: full resolution unless the pointer is moving fast.
    pub fn quality(self) -> f32 {
        if self.speed >= SETTLE_SPEED {
            MOTION_QUALITY
        } else {
            1.0
        }
    }
}

/// The size a note being dragged out gets: the rectangle the drag swept, in whichever direction it went,
/// with each side no smaller than a note may be. `None` while the drag is smaller than that in both
/// directions, which is a click — and a click keeps the default size rather than creating a sliver.
/// The direction a constrained line or arrow snaps to, in radians: 15°, so horizontal, vertical and the
/// diagonals are all reachable, which is the rule Excalidraw uses.
pub const SHAPE_ANGLE_STEP: f32 = std::f32::consts::PI / 12.0;

/// The second point of a shape's drag, constrained by `Shift`: a line or an arrow takes the nearest
/// `SHAPE_ANGLE_STEP` direction at the length the pointer asked for, and a closed figure is measured on the
/// longer side of its box so that both sides come out equal. The constraint is applied here, to the point
/// every other part of the drag reads, so the live shape, its box and the element that is written agree.
pub fn constrained_drag(
    kind: ShapeKind,
    origin: [f32; 2],
    at: [f32; 2],
    constrain: bool,
) -> [f32; 2] {
    if !constrain {
        return at;
    }
    match kind {
        ShapeKind::Line | ShapeKind::Arrow => {
            let (dx, dy) = (at[0] - origin[0], at[1] - origin[1]);
            let length = (dx * dx + dy * dy).sqrt();
            if length <= f32::EPSILON {
                return at;
            }
            let step = (dy.atan2(dx) / SHAPE_ANGLE_STEP).round() * SHAPE_ANGLE_STEP;
            [
                origin[0] + length * step.cos(),
                origin[1] + length * step.sin(),
            ]
        }
        ShapeKind::Circle | ShapeKind::Rect | ShapeKind::Diamond => snap_square(origin, at),
    }
}

/// `at` moved onto the square that reaches as far as it does: the longer of the two sides, in the
/// direction the pointer went. A rectangle's two points are its opposite corners, so this is what makes a
/// dragged figure square — and what keeps a note's press a corner of it.
pub fn snap_square(origin: [f32; 2], at: [f32; 2]) -> [f32; 2] {
    let side = (at[0] - origin[0]).abs().max((at[1] - origin[1]).abs());
    [
        origin[0] + if at[0] < origin[0] { -side } else { side },
        origin[1] + if at[1] < origin[1] { -side } else { side },
    ]
}

/// Where the second point of a note's drag is, with `Shift`'s square applied first.
pub fn note_drag(origin: [f32; 2], at: [f32; 2]) -> Option<Rect> {
    let swept = Rect::from_corners(origin, at);
    // What tells a click from a drag is the pointer having moved, not the note reaching its minimum:
    // measuring it by the minimum threw away every drag smaller than a note may be, so dragging a small
    // note gave the default size back and the tool looked as if it could not size anything.
    if swept.w < NOTE_DRAG_SLOP && swept.h < NOTE_DRAG_SLOP {
        return None;
    }
    let (w, h) = (swept.w.max(NOTE_MIN_SIZE), swept.h.max(NOTE_MIN_SIZE));
    // The press is the corner the note grows out of, and it stays put. Keeping the sweep's own corner
    // instead meant a side clamped up to the minimum reached back through the pointer: pulling a note
    // back in left its far edge jumping out and back, so both sides of the note flickered, and the note
    // grew away from the pointer rather than with it.
    Some(Rect {
        x: if at[0] < origin[0] {
            origin[0] - w
        } else {
            origin[0]
        },
        y: if at[1] < origin[1] {
            origin[1] - h
        } else {
            origin[1]
        },
        w,
        h,
    })
}

/// The box an element occupies, which is what the frame and the marquee measure. A text box
/// contributes its conservative estimate rather than its ink, so a frame errs outside the drawn
/// glyphs instead of cutting through them.
pub fn item_bounds(ann: &Board, id: ItemId) -> Option<Rect> {
    item_bounds_with(ann, id, &[])
}

/// Measured boxes of some of a board's texts, by index. A text's real width is a font question, so
/// only a caller with a renderer can say it; an index that is not here falls back to the damage
/// estimate, which is deliberately wider than the words.
pub fn item_bounds_with(ann: &Board, id: ItemId, texts: &[(usize, Rect)]) -> Option<Rect> {
    match id {
        ItemId::Stroke(i) => ann.strokes.get(i)?.bounds(),
        ItemId::Text(i) => ann.texts.get(i).map(|text| text_box(text, i, texts)),
        ItemId::Shape(i) => ann.shapes.get(i).map(Shape::bounds),
        ItemId::Note(i) => ann.notes.get(i).map(Note::bounds),
    }
}

/// A text's box: the measured one when the caller has it, else the damage estimate.
fn text_box(text: &TextItem, index: usize, texts: &[(usize, Rect)]) -> Rect {
    texts
        .iter()
        .find(|(i, _)| *i == index)
        .map_or_else(|| text.paint_bounds(), |(_, box_)| *box_)
}

/// The measured box a text box is framed by while a gesture is live. The cached box is the one at
/// the document origin, so it has to move with the text and follow a size step; treating it as fixed
/// was what left the frame behind while the glyphs moved.
fn transformed_text_box(
    text: &TextItem,
    index: usize,
    texts: &[(usize, Rect)],
    transform: Transform,
) -> Rect {
    let original = text_box(text, index, texts);
    let moved = transform.text(text);
    let scale = if text.size > f32::EPSILON {
        moved.size / text.size
    } else {
        1.0
    };
    Rect {
        x: moved.x + (original.x - text.x) * scale,
        y: moved.y + (original.y - text.y) * scale,
        w: original.w * scale,
        h: original.h * scale,
    }
}

/// Most elements one clipboard entry may hold, and most bytes of JSON it may be. A clip is memory,
/// not the file: these bound the one case where a copy is a whole busy board.
pub const MAX_CLIP_ITEMS: usize = 200;
pub const MAX_CLIP_BYTES: usize = 1_000_000;
/// Most entries the clipboard history keeps; an older entry is dropped to make room for a newer one.
pub const MAX_CLIP_ENTRIES: usize = 8;

/// The text a link action would link: the one text box held, and nothing else. `None` when nothing
/// is held, more than one thing is, or the held thing is not a text box.
pub fn link_source(ann: &Board, items: &[ItemId]) -> Option<usize> {
    match items {
        [ItemId::Text(index)] if *index < ann.texts.len() => Some(*index),
        _ => None,
    }
}

/// The element count and JSON byte count of a clipboard copy, or `None` when either cap would be
/// exceeded. Both caps matter: a few huge strokes and a thousand tiny ones are the same problem.
pub fn clip_size(board: &Board) -> Option<(usize, usize)> {
    let items = board.strokes.len() + board.texts.len() + board.shapes.len() + board.notes.len();
    if items > MAX_CLIP_ITEMS {
        return None;
    }
    let bytes = serde_json::to_vec(board).ok()?.len();
    (bytes <= MAX_CLIP_BYTES).then_some((items, bytes))
}

/// Drop the oldest clipboard entries until the history is within all three caps. The newest entry
/// is assumed to fit on its own: [`ClipEntry::new`] refused it otherwise, so the tail is always the
/// part that goes.
pub fn trim_clipboard(clips: &mut Vec<ClipEntry>) {
    while clips.len() > MAX_CLIP_ENTRIES {
        clips.pop();
    }
    let mut items: usize = clips.iter().map(|clip| clip.items).sum();
    let mut bytes: usize = clips.iter().map(|clip| clip.bytes).sum();
    while (items > MAX_CLIP_ITEMS || bytes > MAX_CLIP_BYTES) && clips.len() > 1 {
        if let Some(old) = clips.pop() {
            items -= old.items;
            bytes -= old.bytes;
        }
    }
}

/// Record a copy of `items` from `board` as the newest clipboard entry, and return how many
/// elements it holds. `None` when there is nothing to copy or the copy is too large for the
/// clipboard, in which case the history is left exactly as it was — the caller decides what that
/// means for the document. This is the one place a copy is encoded, so `Ctrl+C` and a cut cannot
/// disagree about what the clipboard holds.
pub fn record_clip(clips: &mut Vec<ClipEntry>, board: &Board, items: &[ItemId]) -> Option<usize> {
    if items.is_empty() {
        return None;
    }
    let entry = ClipEntry::new(board.copied(items))?;
    // A repeat copy of the same shape is not a second history entry: it moves to the front.
    if let Some(index) = clips.iter().position(|clip| clip.board == entry.board) {
        clips.remove(index);
    }
    let count = entry.items;
    clips.insert(0, entry);
    trim_clipboard(clips);
    Some(count)
}

/// One entry in the clipboard history, with the measurements its cap was decided from.
#[derive(Debug, Clone, PartialEq)]
pub struct ClipEntry {
    pub board: Board,
    pub items: usize,
    pub bytes: usize,
}

impl ClipEntry {
    /// A new entry when both caps allow it; `None` when the copy is too big to be stored at all.
    pub fn new(board: Board) -> Option<Self> {
        let (items, bytes) = clip_size(&board)?;
        Some(Self {
            board,
            items,
            bytes,
        })
    }

    pub fn preview(&self) -> String {
        self.board.clip_preview()
    }
}

/// One entry in the clipboard history, as the panel lists it: how many elements it holds and a short
/// preview so two similar copies can be told apart.
#[derive(Debug, Clone, PartialEq)]
pub struct ClipRow {
    pub items: usize,
    pub preview: String,
}

impl Board {
    /// A copy moved by `(dx, dy)`, so a paste is visible on top of what it copies.
    pub fn shifted(&self, dx: f32, dy: f32) -> Board {
        let mut out = self.clone();
        for stroke in &mut out.strokes {
            for point in &mut stroke.points {
                point[0] += dx;
                point[1] += dy;
            }
        }
        for text in &mut out.texts {
            text.x += dx;
            text.y += dy;
        }
        for shape in &mut out.shapes {
            shape.from[0] += dx;
            shape.from[1] += dy;
            shape.to[0] += dx;
            shape.to[1] += dy;
        }
        for note in &mut out.notes {
            note.x += dx;
            note.y += dy;
        }
        out
    }
}

impl Board {
    /// A one-line description of a clipboard entry, taken from its first non-empty text or note.
    pub fn clip_preview(&self) -> String {
        let text = self
            .texts
            .iter()
            .map(|t| t.text.as_str())
            .chain(self.notes.iter().map(|n| n.text.as_str()))
            .find(|text| !text.trim().is_empty());
        let mut preview = match text {
            Some(text) => text.lines().next().unwrap_or("").trim().to_string(),
            None => String::new(),
        };
        if preview.chars().count() > 24 {
            preview = preview.chars().take(24).collect::<String>() + "…";
        }
        preview
    }
}

/// The box the frame is drawn on: the union of the held elements' boxes as the live transform
/// would leave them, so the frame follows a drag. `None` when nothing held is still in the
/// document.
pub fn selection_frame(
    ann: &Board,
    items: &[ItemId],
    transform: Option<Transform>,
) -> Option<Rect> {
    selection_frame_with(ann, items, transform, &[])
}

/// The frame with the texts measured: the drawn box, the handles and the press that grabs them all use
/// these, so the frame sits on the words instead of on the damage estimate.
pub fn selection_frame_with(
    ann: &Board,
    items: &[ItemId],
    transform: Option<Transform>,
    texts: &[(usize, Rect)],
) -> Option<Rect> {
    items
        .iter()
        .filter_map(|id| match transform {
            Some(t) => match id {
                ItemId::Stroke(i) => ann
                    .strokes
                    .get(*i)
                    .map(|s| t.stroke(s).bounds())
                    .unwrap_or(None),
                ItemId::Text(i) => ann
                    .texts
                    .get(*i)
                    .map(|text| transformed_text_box(text, *i, texts, t)),
                ItemId::Shape(i) => ann.shapes.get(*i).map(|s| t.shape(s).bounds()),
                ItemId::Note(i) => ann.notes.get(*i).map(|n| t.note(n).bounds()),
            },
            None => item_bounds_with(ann, *id, texts),
        })
        .reduce(Rect::union)
}

/// The centre of the rotation handle: out of the middle of the frame's top edge, `gap` above it. The
/// gap is an interface length, so a caller holding a canvas frame passes `gap / zoom` and a caller
/// holding a screen frame passes it as it is.
pub fn rotate_handle(frame: Rect, gap: f32) -> [f32; 2] {
    [frame.x + frame.w / 2.0, frame.y - gap]
}

/// What a press at `(x, y)` grabs on the frame. A handle wins over the element below it, so a
/// gesture that starts on one never moves or re-selects anything.
pub fn grab_at(frame: Rect, x: f32, y: f32, hit: f32) -> Option<Grab> {
    let limit = hit * hit;
    let p = [x, y];
    if dist_squared(p, rotate_handle(frame, hit.max(SELECT_ROTATE_GAP))) <= limit {
        return Some(Grab::Rotate);
    }
    Handle::ALL
        .into_iter()
        .find(|h| dist_squared(p, h.point(frame)) <= limit)
        .map(Grab::Handle)
}

/// Everything a selection paints: the frame, its handles and the rotation handle, which is what
/// the frame that repaints it has to damage.
pub fn selection_bounds(frame: Rect, hit: f32) -> Rect {
    let handle = rotate_handle(frame, hit.max(SELECT_ROTATE_GAP));
    let dot = Rect {
        x: handle[0] - hit,
        y: handle[1] - hit,
        w: 2.0 * hit,
        h: 2.0 * hit,
    };
    frame.grown(hit).union(dot)
}

/// The elements a marquee catches: those whose whole box is inside it, so an element that only
/// crosses the marquee is left alone. Text boxes first, then strokes, each in document order.
pub fn marquee_select(ann: &Board, marquee: Rect) -> Vec<ItemId> {
    let inside = |b: Rect| {
        b.x >= marquee.x
            && b.y >= marquee.y
            && b.x + b.w <= marquee.x + marquee.w
            && b.y + b.h <= marquee.y + marquee.h
    };
    let mut ids: Vec<ItemId> = (0..ann.texts.len())
        .filter(|i| inside(ann.texts[*i].paint_bounds()))
        .map(ItemId::Text)
        .collect();
    ids.extend(
        (0..ann.strokes.len())
            .filter(|i| ann.strokes[*i].bounds().is_some_and(inside))
            .map(ItemId::Stroke),
    );
    ids.extend(
        (0..ann.shapes.len())
            .filter(|i| inside(ann.shapes[*i].bounds()))
            .map(ItemId::Shape),
    );
    ids.extend(
        (0..ann.notes.len())
            .filter(|i| inside(ann.notes[*i].bounds()))
            .map(ItemId::Note),
    );
    ids
}

/// Every element a bucket holds, in the order the keyboard walks them: one kind after another, each in
/// document order. The order a keyboard step and a marquee walk are the same one.
pub fn items_in_order(ann: &Board) -> Vec<ItemId> {
    let mut ids: Vec<ItemId> = (0..ann.strokes.len()).map(ItemId::Stroke).collect();
    ids.extend((0..ann.texts.len()).map(ItemId::Text));
    ids.extend((0..ann.shapes.len()).map(ItemId::Shape));
    ids.extend((0..ann.notes.len()).map(ItemId::Note));
    ids
}

/// The canvas delta one arrow press asks for. The step is a **screen** length — one logical pixel, ten
/// with `Shift` — so it is divided by the zoom: a nudge is the same size to look at however far out the
/// view is, which is the rule the eraser's radius already follows.
pub fn nudge_delta(dx: i32, dy: i32, far: bool, zoom: f32) -> [f32; 2] {
    let step = if far { NUDGE_STEP_FAR } else { NUDGE_STEP };
    let step = step / zoom;
    [dx as f32 * step, dy as f32 * step]
}

/// The one element `Tab` or `Shift+Tab` moves the selection to: the element after the first one held,
/// wrapping at both ends, so a walk always ends up holding exactly one thing. With nothing held `Tab`
/// takes the first element and `Shift+Tab` the last, and an empty bucket names nothing.
pub fn cycle_selection(ann: &Board, held: &[ItemId], forward: bool) -> Vec<ItemId> {
    let ids = items_in_order(ann);
    if ids.is_empty() {
        return Vec::new();
    }
    let at = held
        .first()
        .and_then(|first| ids.iter().position(|id| id == first));
    let next = match (at, forward) {
        (Some(i), true) => (i + 1) % ids.len(),
        (Some(0), false) => ids.len() - 1,
        (Some(i), false) => i - 1,
        (None, true) => 0,
        (None, false) => ids.len() - 1,
    };
    vec![ids[next]]
}

/// The direction from `pivot` to `p`, as an angle in radians.
fn angle_of(p: [f32; 2], pivot: [f32; 2]) -> f32 {
    (p[1] - pivot[1]).atan2(p[0] - pivot[0])
}

/// `p` turned `radians` about `centre`, the same arithmetic `Transform::Turn` applies.
fn rotate_about(p: [f32; 2], centre: [f32; 2], radians: f32) -> [f32; 2] {
    let (sin, cos) = radians.sin_cos();
    let (dx, dy) = (p[0] - centre[0], p[1] - centre[1]);
    [
        centre[0] + dx * cos - dy * sin,
        centre[1] + dx * sin + dy * cos,
    ]
}

/// The point on a closed polygon nearest to `p`.
/// The freehand path is the renderer's quad-through-midpoints curve, so snapping samples that
/// curve rather than the straight chords between raw samples. Eight steps per quad stay well
/// inside the 12-pixel magnet while the bounds check keeps most strokes out of the loop.
fn nearest_on_stroke(p: [f32; 2], points: &[[f32; 2]]) -> Option<[f32; 2]> {
    let first = *points.first()?;
    let last = *points.last()?;
    let mut best = first;
    let mut best_d = dist_squared(first, p);
    let mut consider = |a: [f32; 2], b: [f32; 2]| {
        let point = nearest_on_segment(p, a, b);
        let d = dist_squared(point, p);
        if d < best_d {
            best = point;
            best_d = d;
        }
    };
    if points.len() < 3 {
        consider(first, last);
        return Some(best);
    }
    const STEPS: usize = 8;
    let mut start = first;
    for index in 1..points.len() - 1 {
        let control = points[index];
        let end = [
            (points[index][0] + points[index + 1][0]) / 2.0,
            (points[index][1] + points[index + 1][1]) / 2.0,
        ];
        let mut previous = start;
        for step in 1..=STEPS {
            let t = step as f32 / STEPS as f32;
            let inv = 1.0 - t;
            let sample = [
                inv * inv * start[0] + 2.0 * inv * t * control[0] + t * t * end[0],
                inv * inv * start[1] + 2.0 * inv * t * control[1] + t * t * end[1],
            ];
            consider(previous, sample);
            previous = sample;
        }
        start = end;
    }
    consider(start, last);
    Some(best)
}

fn nearest_on_polyline(p: [f32; 2], points: &[[f32; 2]]) -> Option<[f32; 2]> {
    match points {
        [] => None,
        [only] => Some(*only),
        _ => {
            let mut best = points[0];
            let mut best_d = f32::INFINITY;
            for pair in points.windows(2) {
                let point = nearest_on_segment(p, pair[0], pair[1]);
                let d = dist_squared(point, p);
                if d < best_d {
                    best = point;
                    best_d = d;
                }
            }
            Some(best)
        }
    }
}

fn nearest_on_polygon(p: [f32; 2], corners: &[[f32; 2]]) -> [f32; 2] {
    let mut best = corners[0];
    let mut best_d = f32::INFINITY;
    for (index, &from) in corners.iter().enumerate() {
        let to = corners[(index + 1) % corners.len()];
        let point = nearest_on_segment(p, from, to);
        let d = dist_squared(point, p);
        if d < best_d {
            best = point;
            best_d = d;
        }
    }
    best
}

fn dist_squared(p: [f32; 2], q: [f32; 2]) -> f32 {
    let (dx, dy) = (p[0] - q[0], p[1] - q[1]);
    dx * dx + dy * dy
}

fn segment_dist_squared(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    dist_squared(p, nearest_on_segment(p, a, b))
}

fn nearest_on_segment(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    let (abx, aby) = (b[0] - a[0], b[1] - a[1]);
    let len2 = abx * abx + aby * aby;
    if len2 == 0.0 {
        return a;
    }
    let t = (((p[0] - a[0]) * abx + (p[1] - a[1]) * aby) / len2).clamp(0.0, 1.0);
    [a[0] + t * abx, a[1] + t * aby]
}

/// The document: every board by name, and the board the user picked last. A board belongs to no
/// screen — any number of screens may show one — which is what lets a drawing survive the monitor it
/// was made on being closed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Doc {
    #[serde(default)]
    pub boards: BTreeMap<String, Board>,
    /// The name of the board the user picked last, or empty when nothing has been picked since the file
    /// was written. A screen that appears afterwards lands on it, which is what makes "pick it on the
    /// laptop, read it on the monitor tomorrow" work.
    #[serde(default)]
    pub active: String,
}

/// What an unnamed board is called, before its number.
pub const BOARD_PREFIX: &str = "Board ";

impl Doc {
    pub fn board(&self, name: &str) -> Option<&Board> {
        self.boards.get(name)
    }

    pub fn board_mut(&mut self, name: &str) -> Option<&mut Board> {
        self.boards.get_mut(name)
    }

    /// Every board's name, in name order.
    pub fn names(&self) -> impl Iterator<Item = &String> {
        self.boards.keys()
    }

    /// Create an empty board named `Board N` for the smallest `N` no board holds, without selecting it.
    /// The smallest free number is what keeps creating from colliding with a board loaded from the file.
    pub fn new_board(&mut self) -> String {
        let mut n = 1;
        loop {
            let name = format!("{BOARD_PREFIX}{n}");
            if !self.boards.contains_key(&name) {
                self.boards.insert(name.clone(), Board::default());
                return name;
            }
            n += 1;
        }
    }

    /// Remove a board with everything drawn on it, and name the board that takes its place: the first one
    /// still in the document, which is the board [`Doc::adopt`] falls back to. `None` when the name is not
    /// a board's, or is the document's last one — a document with no boards is nowhere to draw. The board
    /// picked last is moved to the successor, so no later screen adopts a name that is gone.
    pub fn delete_board(&mut self, name: &str) -> Option<String> {
        if self.boards.len() <= 1 || !self.boards.contains_key(name) {
            return None;
        }
        self.boards.remove(name);
        // A link names a board, so a board that is gone must not leave texts pointing at nothing.
        for board in self.boards.values_mut() {
            for text in &mut board.texts {
                if text.link.as_deref() == Some(name) {
                    text.link = None;
                }
            }
        }
        let successor = self.boards.keys().next().cloned()?;
        if self.active == name {
            self.active = successor.clone();
        }
        Some(successor)
    }

    /// Move the named elements from one board to another, returning the box the source lost and the
    /// one the target gained. `None` when either board is missing, the boards are the same, or
    /// nothing was named. Links that name the source board keep naming it; a link is text content,
    /// not a place.
    pub fn move_items(&mut self, from: &str, to: &str, items: &[ItemId]) -> Option<(Rect, Rect)> {
        if from == to {
            return None;
        }
        // The target is checked before the source is touched: a move into a board that is gone must
        // not remove the elements and then fail to put them anywhere.
        self.boards.get(to)?;
        let copy = self.boards.get(from)?.copied(items);
        let lost = self.boards.get_mut(from)?.remove(items)?;
        let (_, gained) = self.boards.get_mut(to)?.append(&copy);
        Some((lost, gained.unwrap_or_default()))
    }

    /// Rename `old` to `new`, moving its drawing and the picked board with it. `false` when the new
    /// name is empty or another board holds it, or when there is no board called `old`: a refused
    /// rename changes nothing.
    pub fn rename_board(&mut self, old: &str, new: &str) -> bool {
        if old == new || new.is_empty() || self.boards.contains_key(new) {
            return false;
        }
        let Some(board) = self.boards.remove(old) else {
            return false;
        };
        self.boards.insert(new.to_string(), board);
        // A link names a board, so a rename has to move every link that named the old one.
        for board in self.boards.values_mut() {
            for text in &mut board.texts {
                if text.link.as_deref() == Some(old) {
                    text.link = Some(new.to_string());
                }
            }
        }
        if self.active == old {
            self.active = new.to_string();
        }
        true
    }

    /// Which board an output shows once its `wl_output` name arrives, when it was not known at the time the
    /// surface was made: the same rule as [`Doc::adopt`], run again, so a board picked last still wins over
    /// the board named after that output. `None` when nothing changes, or when the board was chosen by hand
    /// — a name that late must never move a board somebody picked.
    pub fn readopt(&mut self, output: &str, picked: bool, current: &str) -> Option<String> {
        if picked {
            return None;
        }
        let adopted = self.adopt(output);
        (adopted != current).then_some(adopted)
    }

    /// Which board "every screen shows one board" adopts when it is switched on: the focused output's, else
    /// the board picked last, else the only board there is. A key binding runs while locked, with no focused
    /// output, so the picked board has to come before an arbitrary one.
    pub fn shared_board(&self, focused: Option<&str>, first: Option<&str>) -> Option<String> {
        focused
            .map(str::to_string)
            .or_else(|| {
                self.boards
                    .contains_key(&self.active)
                    .then(|| self.active.clone())
            })
            .or_else(|| first.map(str::to_string))
    }

    /// Which board an output that has just appeared shows: the board picked last, else the board named
    /// after that output (which is the name a file written before boards existed stored its bucket
    /// under, so a two-screen setup comes back exactly as it was), else the first board, else a fresh one.
    /// It never writes [`Doc::active`]: a second screen must not inherit the first one's drawing.
    pub fn adopt(&mut self, output: &str) -> String {
        if self.boards.contains_key(&self.active) {
            return self.active.clone();
        }
        if self.boards.contains_key(output) {
            return output.to_string();
        }
        if let Some(first) = self.boards.keys().next() {
            return first.clone();
        }
        self.new_board()
    }
}

/// Transient edit-time state: rendered only in edit mode, never persisted.
#[derive(Debug, Clone, Default)]
pub struct Overlay {
    /// Stroke still being drawn, not yet committed.
    pub stroke: Option<Stroke>,
    /// Shape still being dragged out, not yet committed.
    pub shape: Option<Shape>,
    /// Note still being dragged out, not yet committed. A note is created by a press and its size is
    /// what the drag gave it, so it exists in the overlay until the release rather than in the document.
    pub note: Option<Note>,
    /// Eraser cursor position.
    pub eraser: Option<[f32; 2]>,
    /// Text box being edited, preedit included.
    pub text: Option<TextOverlay>,
    /// Edit-mode toolbar: the active tool, colour and size. Never part of the document, so it
    /// cannot be persisted, erased or exported.
    pub toolbar: Option<Toolbar>,
    /// What the selection tool holds, if anything.
    pub selection: Option<Selection>,
    /// The shape index whose linear points are in edit mode. Transient; the points themselves are
    /// document content and are written as they move.
    pub linear_edit: Option<usize>,
    /// Attached connectors redrawn while a selection gesture moves their shapes. The document
    /// raster still holds their old geometry, so the frame paints these and skips those ids.
    pub connectors: Vec<LiveConnector>,
    /// The rectangle a selection drag is sweeping, before it becomes a selection.
    pub marquee: Option<Rect>,
}

/// One board as the rail's panel states it: its name, and whether this output is showing it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BoardRow {
    pub name: String,
    pub current: bool,
}

/// A board's name being edited, as the frame draws it.
#[derive(Debug, Clone, PartialEq)]
pub struct BoardEdit {
    /// The row's place in [`Toolbar::boards`].
    pub index: usize,
    pub text: String,
    pub cursor: usize,
    pub preedit: String,
    /// Whether the caret is on this half of its blink.
    pub caret: bool,
}

/// What the edit-mode toolbar shows: the active tool, the active colour, the active size in
/// logical pixels, and the block the pointer is on, if any.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Toolbar {
    pub tool: Tool,
    /// The active colour, normalized to `#rrggbb` or `#rrggbbaa`; one of [`crate::config::current`]'s.
    pub color: String,
    /// Stroke width or text size, depending on the tool.
    pub size: f32,
    /// The block under the pointer: it stretches to say what it does.
    pub hover: Option<ToolbarAction>,
    /// How far that stretch has opened, 0 to 1. It moves only the capsule's trailing edge and
    /// the text the edge uncovers, so a frame of the transition is a localized repaint.
    pub grow: f32,
    /// How far the open panel has opened, 0 to 1: the rows' capsules grow from their squares the way a
    /// block's pop-out does, and the layout, the hits and the damage box are the settled panel throughout.
    pub panel_grow: f32,
    /// Whether the drawer of rarely used controls is open. It changes which controls exist, not the
    /// rail's own height, so it is rail state like the tool and the colour.
    pub more: bool,
    /// Whether the panel of boards is open. Only one panel is open at a time.
    pub boards_open: bool,
    /// Whether the width list is open. The readout toggles it, and it obeys the same one-panel rule as the
    /// other two.
    pub size_open: bool,
    /// Whether the clipboard history panel is open.
    pub clipboard_open: bool,
    /// The clipboard history, newest first, as the panel lists it.
    pub clips: Vec<ClipRow>,
    /// What a board pick applies while a target picker is open. `None` means a pick switches the screen.
    pub target: Option<Target>,
    /// The board's name being edited in the list, which its row draws instead of the name.
    pub rename: Option<BoardEdit>,
    /// The board whose row is asking whether to delete it. The row covers its name with the question
    /// and its two buttons with `no`/`yes`; the list stays open while it is set.
    pub delete_confirm: Option<String>,
    /// The boards the panel lists, in name order, with the one this output shows marked. Carried in the
    /// rail's own value because the panel's rows are document names: the layout, the hit test and the
    /// cache all read one value rather than three functions taking the list beside the rail.
    pub boards: Vec<BoardRow>,
    /// Whether every screen shows the board picked on any of them.
    pub shared: bool,
    /// What edit mode paints behind the document.
    pub backdrop: Backdrop,
    /// The zoom of the output this rail belongs to, as the cards show it.
    pub zoom: f32,
    /// Whether a document change can be undone and whether an undone one can be redone.
    pub can_undo: bool,
    pub can_redo: bool,
    /// Whether the collapsed colour swatch is showing the other four colours beside it.
    pub colors_open: bool,
    /// How far that flyout has unfurled, 0 to about 1.
    pub colors_grow: f32,
}

#[derive(Debug, Clone)]
pub struct TextOverlay {
    pub item: TextItem,
    pub buffer: TextBuffer,
    /// Index into the document when an existing box is edited; None for a new one.
    pub index: Option<usize>,
    /// Index into the notes when a note is edited, which is the one editing target that is not a text
    /// box: the caret, the preedit, the input method and paste are the same machinery either way, and
    /// only the commit needs to know which list the text belongs to.
    pub note: Option<usize>,
    /// Input method preedit string; never persisted.
    pub preedit: String,
}

/// Text box content and cursor. The cursor counts chars, not bytes, and stays at the end.
/// Where the caret moves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Caret {
    Left,
    Right,
    Up,
    Down,
}

/// Text box content and caret. The caret counts characters, not bytes, and starts at the end,
/// which is where re-entering a box puts it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TextBuffer {
    pub text: String,
    pub cursor: usize,
}

impl TextBuffer {
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let cursor = text.chars().count();
        Self { text, cursor }
    }

    /// Insert `s` at the caret, which ends up after it.
    pub fn insert(&mut self, s: &str) {
        let at = self.cursor_bytes();
        self.text.insert_str(at, s);
        self.cursor += s.chars().count();
    }

    pub fn backspace(&mut self) -> bool {
        self.delete_before(1)
    }

    /// Delete the `n` characters before the caret, keeping what follows it.
    pub fn delete_before(&mut self, n: usize) -> bool {
        let n = n.min(self.cursor);
        if n == 0 {
            return false;
        }
        let start = char_boundary(&self.text, self.cursor - n);
        let end = self.cursor_bytes();
        self.text.replace_range(start..end, "");
        self.cursor -= n;
        true
    }

    /// Move the caret: one character left or right, crossing a line end to the line before or
    /// after; or one line up or down keeping the column, clamped to the shorter line. Returns
    /// whether the caret moved, so the caller can skip a repaint at the ends of the text.
    pub fn move_cursor(&mut self, caret: Caret) -> bool {
        let (line, column) = self.line_column();
        let lines: Vec<&str> = self.text.split('\n').collect();
        let (line, column) = match caret {
            Caret::Left if column > 0 => (line, column - 1),
            Caret::Left if line > 0 => (line - 1, lines[line - 1].chars().count()),
            Caret::Right if column < lines[line].chars().count() => (line, column + 1),
            Caret::Right if line + 1 < lines.len() => (line + 1, 0),
            Caret::Up if line > 0 => (line - 1, column),
            Caret::Down if line + 1 < lines.len() => (line + 1, column),
            _ => return false,
        };
        let column = column.min(lines[line].chars().count());
        self.cursor = lines[..line]
            .iter()
            .map(|line| line.chars().count() + 1)
            .sum::<usize>()
            + column;
        true
    }

    /// The caret as a line index and a character column within that line.
    fn line_column(&self) -> (usize, usize) {
        let mut line = 0;
        let mut column = 0;
        for ch in self.text.chars().take(self.cursor) {
            if ch == '\n' {
                line += 1;
                column = 0;
            } else {
                column += 1;
            }
        }
        (line, column)
    }

    pub fn surrounding(&self) -> (&str, usize) {
        (&self.text, self.cursor)
    }

    /// Byte offset of the cursor, which is what `set_surrounding_text` takes.
    pub fn cursor_bytes(&self) -> usize {
        char_boundary(&self.text, self.cursor)
    }

    /// Delete `bytes` bytes before the cursor, landed on a character boundary. The input
    /// method's `delete_surrounding_text` counts bytes, unlike the local `backspace`.
    pub fn delete_before_bytes(&mut self, bytes: usize) -> bool {
        let end = self.cursor_bytes();
        if bytes == 0 || end == 0 {
            return false;
        }
        let mut start = end.saturating_sub(bytes);
        while start < end && !self.text.is_char_boundary(start) {
            start += 1;
        }
        self.text.replace_range(start..end, "");
        self.cursor = self.text[..start].chars().count();
        true
    }

    /// Character offset the preedit is inserted at.
    pub fn preedit_at(&self) -> usize {
        self.cursor
    }
}

/// Byte offset of the `chars`-th character.
fn char_boundary(text: &str, chars: usize) -> usize {
    text.char_indices()
        .nth(chars)
        .map(|(i, _)| i)
        .unwrap_or(text.len())
}
