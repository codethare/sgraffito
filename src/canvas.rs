//! Annotation data model and hit testing. No Wayland dependency, so it unit tests easily.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Eraser hit radius, in logical pixels.
pub const ERASER_THRESHOLD: f32 = 8.0;

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

/// An item the eraser hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemId {
    Stroke(usize),
    Text(usize),
}

/// Annotations belonging to a single output.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OutputAnnotations {
    #[serde(default)]
    pub strokes: Vec<Stroke>,
    #[serde(default)]
    pub texts: Vec<TextItem>,
}

impl OutputAnnotations {
    /// Eraser hit test: text boxes win over strokes, nearest one of each kind.
    pub fn hit(&self, x: f32, y: f32) -> Option<ItemId> {
        if let Some(i) = self.texts.iter().position(|t| t.bounds().contains(x, y)) {
            return Some(ItemId::Text(i));
        }
        let limit = ERASER_THRESHOLD * ERASER_THRESHOLD;
        let mut best: Option<(usize, f32)> = None;
        for (i, s) in self.strokes.iter().enumerate() {
            let d = s.distance_squared(x, y);
            if d <= limit && best.is_none_or(|(_, bd)| d < bd) {
                best = Some((i, d));
            }
        }
        best.map(|(i, _)| ItemId::Stroke(i))
    }

    /// Delete the hit item; returns whether anything was removed.
    pub fn erase(&mut self, x: f32, y: f32) -> bool {
        match self.hit(x, y) {
            Some(ItemId::Stroke(i)) => {
                self.strokes.remove(i);
                true
            }
            Some(ItemId::Text(i)) => {
                self.texts.remove(i);
                true
            }
            None => false,
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

#[derive(Debug, Clone, Copy, PartialEq)]
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
    /// Eraser cursor position.
    pub eraser: Option<[f32; 2]>,
    /// Text box being edited, preedit included.
    pub text: Option<TextOverlay>,
    /// Edit-mode affordance: the active tool and colour. Never part of the document, so it
    /// cannot be persisted, erased or exported.
    pub hint: Option<Hint>,
}

/// What the edit-mode hint shows: the active tool's label and the active colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Hint {
    pub tool: &'static str,
    /// `#rrggbb`
    pub color: &'static str,
}

#[derive(Debug, Clone)]
pub struct TextOverlay {
    pub item: TextItem,
    pub buffer: TextBuffer,
    /// Index into the document when an existing box is edited; None for a new one.
    pub index: Option<usize>,
    /// Input method preedit string; never persisted.
    pub preedit: String,
}

/// Text box content and cursor. The cursor counts chars, not bytes, and stays at the end.
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

    pub fn insert(&mut self, s: &str) {
        self.text.push_str(s);
        self.cursor += s.chars().count();
    }

    pub fn backspace(&mut self) -> bool {
        self.delete_before(1)
    }

    pub fn delete_before(&mut self, n: usize) -> bool {
        let n = n.min(self.cursor);
        if n == 0 {
            return false;
        }
        let cut = char_boundary(&self.text, self.cursor - n);
        self.text.truncate(cut);
        self.cursor -= n;
        true
    }
    pub fn surrounding(&self) -> (&str, usize) {
        (&self.text, self.cursor)
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
