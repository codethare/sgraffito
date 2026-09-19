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
}

impl TextItem {
    /// ponytail: bounds estimated from the font size (0.6em per char wide, 1.2em per line high)
    /// instead of real glyph metrics; if erasing feels off, write back measured bounds while rendering.
    pub fn bounds(&self) -> Rect {
        let lines = self.text.lines().count().max(1) as f32;
        let chars = self
            .text
            .lines()
            .map(|l| l.chars().count())
            .max()
            .unwrap_or(0)
            .max(1) as f32;
        Rect {
            x: self.x,
            y: self.y,
            w: 0.6 * self.size * chars,
            h: 1.2 * self.size * lines,
        }
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
