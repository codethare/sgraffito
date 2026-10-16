//! Annotation data model and hit testing. No Wayland dependency, so it unit tests easily.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Eraser hit radius, in logical pixels.
pub const ERASER_THRESHOLD: f32 = 8.0;

/// Drawn diameter of a selection handle, in logical pixels.
pub const SELECT_HANDLE: f32 = 9.0;

/// Radius a selection handle is grabbed within. Larger than it is drawn, so a small dot stays an
/// easy target on a dense drawing.
pub const SELECT_HANDLE_HIT: f32 = 11.0;

/// How far above the frame's top edge the rotation handle sits, in logical pixels.
pub const SELECT_ROTATE_GAP: f32 = 18.0;

/// Smallest factor a resize may apply. A frame cannot be collapsed into an extent no drag can
/// restore, and a negative factor would mirror the selection, which the text renderer cannot draw.
const MIN_SCALE: f32 = 0.05;

/// Text size bounds, in logical pixels. They bound a new box and every resize of an existing one.
pub const TEXT_SIZE_MIN: f32 = 8.0;
pub const TEXT_SIZE_MAX: f32 = 72.0;

/// The zoom range, and the factor one classic wheel notch applies. Multiplicative, so zooming in and
/// back out returns to where it started.
pub const ZOOM_MIN: f32 = 0.2;
pub const ZOOM_MAX: f32 = 8.0;
pub const ZOOM_STEP: f32 = 1.15;

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Pen,
    Eraser,
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
    /// The `Esc` control: finish the text box being edited first, lock on the next click.
    Lock,
    /// Open or close the drawer of controls that are not used often.
    More,
    /// Switch the edit-mode backdrop off and on.
    Scrim,
}

impl Tool {
    /// Whether the tool strokes with the pen width. Every tool that draws does, except the text
    /// tool, which has a size of its own.
    pub fn uses_pen_width(self) -> bool {
        !matches!(self, Tool::Text)
    }

    /// The key that arms this tool, as the rail states it and as [`crate::input::tool_for_keysym`]
    /// maps it. One letter per tool, so a block's text cannot name a key that does nothing.
    pub fn key(self) -> &'static str {
        match self {
            Tool::Pen => "P",
            Tool::Eraser => "E",
            Tool::Text => "T",
            Tool::Selection => "S",
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stroke {
    /// `#rrggbb`
    pub color: String,
    pub width: f32,
    pub points: Vec<[f32; 2]>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextItem {
    pub x: f32,
    pub y: f32,
    /// `#rrggbb`
    pub color: String,
    pub size: f32,
    pub text: String,
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

/// A shape is two points and a kind, and nothing else: its outline is derived from the pair when it
/// is drawn and when it is hit, so it costs the same in the file whether it is ten pixels or a whole
/// output across, and a resize or a rotation is the arithmetic a pair of points already takes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Shape {
    pub kind: ShapeKind,
    /// `#rrggbb`
    pub color: String,
    pub width: f32,
    pub from: [f32; 2],
    pub to: [f32; 2],
}

/// An item the eraser hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemId {
    Stroke(usize),
    Text(usize),
    Shape(usize),
    Note(usize),
}

/// Annotations belonging to a single output.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OutputAnnotations {
    #[serde(default)]
    pub strokes: Vec<Stroke>,
    #[serde(default)]
    pub texts: Vec<TextItem>,
    #[serde(default)]
    pub shapes: Vec<Shape>,
    #[serde(default)]
    pub notes: Vec<Note>,
}

impl OutputAnnotations {
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
                let box_ = self.strokes[i].bounds();
                self.strokes.remove(i);
                box_
            }
            Some(ItemId::Text(i)) => {
                let box_ = self.texts[i].paint_bounds();
                self.texts.remove(i);
                Some(box_)
            }
            Some(ItemId::Shape(i)) => {
                let box_ = self.shapes[i].bounds();
                self.shapes.remove(i);
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
            ShapeKind::Line | ShapeKind::Arrow => self.from == self.to,
            _ => self.from[0] == self.to[0] || self.from[1] == self.to[1],
        }
    }

    /// Axis-aligned box of everything the shape draws, in logical pixels, anti-aliasing and the
    /// arrowhead included. A rectangle and a diamond are inside the box of their two points, a
    /// circle touches the middle of its edges, and the head of an arrow leaves that box.
    pub fn bounds(&self) -> Rect {
        let head = match self.kind {
            ShapeKind::Arrow => self.head_half(),
            _ => 0.0,
        };
        rect_of(self.from, self.to).grown(self.width / 2.0 + 1.0 + head)
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
        match self.kind {
            ShapeKind::Circle => (0..=ELLIPSE_SEGMENTS)
                .map(|i| {
                    let t = i as f32 / ELLIPSE_SEGMENTS as f32 * std::f32::consts::TAU;
                    [cx + t.cos() * w / 2.0, cy + t.sin() * h / 2.0]
                })
                .collect(),
            ShapeKind::Rect => vec![[x, y], [x1, y], [x1, y1], [x, y1], [x, y]],
            ShapeKind::Diamond => vec![[cx, y], [x1, cy], [cx, y1], [x, cy], [cx, y]],
            ShapeKind::Line => vec![self.from, self.to],
            ShapeKind::Arrow => {
                let (dx, dy) = (self.to[0] - self.from[0], self.to[1] - self.from[1]);
                let len = (dx * dx + dy * dy).sqrt();
                if len <= f32::EPSILON {
                    return vec![self.from, self.to];
                }
                let (ux, uy) = (dx / len, dy / len);
                let (head, half) = (self.head_len(), self.head_half());
                let base = [self.to[0] - ux * head, self.to[1] - uy * head];
                let (px, py) = (-uy * half, ux * half);
                vec![
                    self.from,
                    self.to,
                    [base[0] + px, base[1] + py],
                    [base[0] - px, base[1] - py],
                    self.to,
                ]
            }
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

/// What a press with the selection tool resolves to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Press {
    /// A handle of the frame: a resize or a rotation.
    Handle(Grab),
    /// Drag what is already held.
    Move,
    /// Take the element under the pointer, which replaces the selection.
    Element(ItemId),
    /// Sweep a marquee: the press is on empty space outside the frame.
    Marquee { canvas: [f32; 2] },
}

/// Resolve a press with the selection tool, decided here so that a test can check it: `screen` is where
/// the pointer is, and the frame is measured in canvas coordinates and mapped to screen **only** to find
/// the handles, because that is where they are drawn.
///
/// The two coordinate spaces are the whole of this function. A gesture and a marquee move the document,
/// so they are canvas; the handles are interface-sized, so they are screen. Confusing the two makes a
/// marquee drift away from the pointer the moment the view is panned or zoomed, which is what this
/// signature exists to prevent.
pub fn selection_press(
    ann: &OutputAnnotations,
    held: &[ItemId],
    view: View,
    screen: [f32; 2],
    tolerance: f32,
) -> Press {
    let canvas = view.to_canvas(screen);
    let frame = selection_frame(ann, held, None);
    if let Some(frame) = frame
        && let Some(grab) = grab_at(
            view.screen_rect(frame),
            screen[0],
            screen[1],
            SELECT_HANDLE_HIT,
        )
    {
        return Press::Handle(grab);
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
            None => Press::Marquee { canvas },
        },
    }
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
    /// elements already held when it grabbed nothing.
    pub fn start(grab: Option<Grab>, frame: Rect, x: f32, y: f32) -> Gesture {
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
            None => Gesture::Move { origin },
        }
    }

    /// The transform this gesture implies for a pointer now at `(x, y)`. A resize scales by the
    /// pointer's own displacement from where it went down, so a press that landed beside a handle
    /// does not make the frame jump. `None` for a marquee, which transforms nothing.
    pub fn transform(self, at: [f32; 2]) -> Option<Transform> {
        Some(match self {
            Gesture::Marquee { .. } => return None,
            Gesture::Move { origin } => Transform::Translate {
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
                Transform::Scale {
                    anchor,
                    sx: if handle.moves_x() {
                        factor(moving[0], anchor[0], origin[0], at[0])
                    } else {
                        1.0
                    },
                    sy: if handle.moves_y() {
                        factor(moving[1], anchor[1], origin[1], at[1])
                    } else {
                        1.0
                    },
                }
            }
            Gesture::Rotate { pivot, origin } => Transform::Turn {
                pivot,
                radians: angle_of(at, pivot) - angle_of(origin, pivot),
            },
        })
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

    pub fn point(self, p: [f32; 2]) -> [f32; 2] {
        match self {
            Transform::Translate { dx, dy } => [p[0] + dx, p[1] + dy],
            Transform::Scale { anchor, sx, sy } => [
                anchor[0] + (p[0] - anchor[0]) * sx,
                anchor[1] + (p[1] - anchor[1]) * sy,
            ],
            Transform::Turn { pivot, radians } => {
                let (sin, cos) = radians.sin_cos();
                let (dx, dy) = (p[0] - pivot[0], p[1] - pivot[1]);
                [
                    pivot[0] + dx * cos - dy * sin,
                    pivot[1] + dx * sin + dy * cos,
                ]
            }
        }
    }

    /// The stroke this makes of one the document holds. The live preview draws this copy and the
    /// commit writes it, so a turned stroke is an ordinary stroke the moment it lands.
    pub fn stroke(self, s: &Stroke) -> Stroke {
        Stroke {
            points: s.points.iter().map(|p| self.point(*p)).collect(),
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
    pub fn shape(self, s: &Shape) -> Shape {
        Shape {
            from: self.point(s.from),
            to: self.point(s.to),
            ..s.clone()
        }
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

/// The size a note being dragged out gets: the rectangle the drag swept, in whichever direction it went,
/// with each side no smaller than a note may be. `None` while the drag is smaller than that in both
/// directions, which is a click — and a click keeps the default size rather than creating a sliver.
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
pub fn item_bounds(ann: &OutputAnnotations, id: ItemId) -> Option<Rect> {
    match id {
        ItemId::Stroke(i) => ann.strokes.get(i)?.bounds(),
        ItemId::Text(i) => ann.texts.get(i).map(TextItem::paint_bounds),
        ItemId::Shape(i) => ann.shapes.get(i).map(Shape::bounds),
        ItemId::Note(i) => ann.notes.get(i).map(Note::bounds),
    }
}

/// The box the frame is drawn on: the union of the held elements' boxes as the live transform
/// would leave them, so the frame follows a drag. `None` when nothing held is still in the
/// document.
pub fn selection_frame(
    ann: &OutputAnnotations,
    items: &[ItemId],
    transform: Option<Transform>,
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
                ItemId::Text(i) => ann.texts.get(*i).map(|x| t.text(x).paint_bounds()),
                ItemId::Shape(i) => ann.shapes.get(*i).map(|s| t.shape(s).bounds()),
                ItemId::Note(i) => ann.notes.get(*i).map(|n| t.note(n).bounds()),
            },
            None => item_bounds(ann, *id),
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
pub fn marquee_select(ann: &OutputAnnotations, marquee: Rect) -> Vec<ItemId> {
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

/// The direction from `pivot` to `p`, as an angle in radians.
fn angle_of(p: [f32; 2], pivot: [f32; 2]) -> f32 {
    (p[1] - pivot[1]).atan2(p[0] - pivot[0])
}

fn dist_squared(p: [f32; 2], q: [f32; 2]) -> f32 {
    let (dx, dy) = (p[0] - q[0], p[1] - q[1]);
    dx * dx + dy * dy
}

fn segment_dist_squared(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let (abx, aby) = (b[0] - a[0], b[1] - a[1]);
    let len2 = abx * abx + aby * aby;
    if len2 == 0.0 {
        return dist_squared(p, a);
    }
    let t = (((p[0] - a[0]) * abx + (p[1] - a[1]) * aby) / len2).clamp(0.0, 1.0);
    dist_squared(p, [a[0] + t * abx, a[1] + t * aby])
}

/// Annotations of all outputs, grouped by output name.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Doc {
    #[serde(default)]
    pub outputs: BTreeMap<String, OutputAnnotations>,
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
    /// The rectangle a selection drag is sweeping, before it becomes a selection.
    pub marquee: Option<Rect>,
}

/// What the edit-mode toolbar shows: the active tool, the active colour, the active size in
/// logical pixels, and the block the pointer is on, if any.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Toolbar {
    pub tool: Tool,
    /// `#rrggbb`, one of `PALETTE`.
    pub color: &'static str,
    /// Stroke width or text size, depending on the tool.
    pub size: f32,
    /// The block under the pointer: it stretches to say what it does.
    pub hover: Option<ToolbarAction>,
    /// How far that stretch has opened, 0 to 1. It moves only the capsule's trailing edge and
    /// the text the edge uncovers, so a frame of the transition is a localized repaint.
    pub grow: f32,
    /// Whether the drawer of rarely used controls is open. It changes which controls exist, not the
    /// rail's own height, so it is rail state like the tool and the colour.
    pub more: bool,
    /// Whether the edit-mode backdrop is painted. A preference rather than content: it decides how the
    /// surface reads, and the raster is keyed by it so a frame cannot hold the other state.
    pub scrim: bool,
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
