# Brush size on a selection — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** With a selection held, `[` / `]` step the width of the selected strokes and shapes and the size of the selected text boxes, as one document change per press, and leave a note's typography alone.

**Architecture:** The rule lives on `Board`, next to `recolour` and `remove`, and returns the union of the ink before and after so the frame can damage exactly what changed. `App::nudge_size` steps the brush as it does today and then walks every output that holds a selection, the way `recolour_selection` already walks them.

**Tech Stack:** Rust 2024, no new dependencies.

**Spec:** `docs/superpowers/specs/2026-11-12-selection-width-design.md`

## Global Constraints

- Gate: `cargo fmt --check`, `cargo clippy --all-targets` (0 warnings), `cargo test`.
- One document change per press: one damage box, one save, no item moves.
- Pen widths stay inside 1–16, text sizes inside 8–72; a note keeps its own type size.
- Nothing is added to the annotation file format (`store::VERSION` stays 4).
- English in code comments, README, commit messages.
- Commit style: imperative sentence, no prefix.

## Review Focus

The five failure modes the spec implies that no step's test can exercise, most likely first.

1. **A mixed selection where only some items can step.** A press must change the ones that can and report a box for those alone; a press with nothing to step must return `None` and write nothing. Pinned by `a_size_step_at_the_bound_writes_nothing` and the mixed-item test in Task 1.
2. **The damage box of a thicker stroke and a bigger text box.** A thicker line reaches further and a text box grows on both axes, so the box must be the union of the ink before *and* after; the tests assert the after ink, and the reviewer checks the before half is unioned too (Task 1's `step_size` body).
3. **Two outputs holding selections on different boards.** Both must step, and an output with no selection must not touch the document. There is no unit seam (the app needs a compositor); Task 2 walks the same list `recolour_selection` walks, and the reviewer checks that it is a snapshot taken before the boards are borrowed.
4. **`doc_changed_at` is the only thing that marks the save and drops the rasters.** A step that returns `None` must not call it, or a press at a bound would dirty the file and repaint for nothing. Reviewer checks the `if let Some(box_)` in Task 2.
5. **A text box steps by the text step, not the pen's**, even though the readout shows the pen width with the selection tool armed. A box at 18 must become 20, and one at 72 must stay 72 — not step by 1 or clamp at 16. Pinned by Task 1's mixed test and the bound test.

---

### Task 1: The document rule

**Files:**
- Modify: `src/canvas.rs` (constants near `TEXT_SIZE_MIN`, `impl Board` after `recolour`)
- Modify: `src/input.rs` (move `stepped_size` out)
- Modify: `src/app.rs` (imports only)
- Test: `tests/store.rs`; move one test out of `tests/keys.rs`

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `canvas::stepped_size(current: f32, dir: f32, step: f32, min: f32, max: f32) -> f32` and the brush bounds `canvas::PEN_WIDTH_MIN`, `PEN_WIDTH_MAX`, `PEN_WIDTH_STEP`, `canvas::TEXT_SIZE_STEP` (moved here beside `TEXT_SIZE_MIN`/`TEXT_SIZE_MAX`); `Board::step_size(&mut self, items: &[ItemId], dir: f32) -> Option<Rect>`.

- [ ] **Step 1: Write the failing tests**

In `tests/store.rs`, add `ItemId` and `Note` to the `sgraffito::canvas` import and append:

```rust
/// A size step walks what the selection holds: a stroke's and a shape's width by the pen step, a text
/// box's size by the text step, and a note not at all — its type size is its own.
#[test]
fn stepping_a_selection_steps_its_widths_and_text_sizes() {
    let mut board = Board {
        strokes: vec![Stroke {
            color: "#e01b24".into(),
            width: 4.0,
            points: vec![[10.0, 10.0], [110.0, 10.0]],
        }],
        texts: vec![TextItem {
            x: 10.0,
            y: 40.0,
            color: "#ffffff".into(),
            size: 18.0,
            text: "hi".into(),
        }],
        shapes: vec![Shape {
            kind: ShapeKind::Rect,
            color: "#33d17a".into(),
            width: 6.0,
            from: [10.0, 80.0],
            to: [110.0, 140.0],
        }],
        notes: vec![Note {
            x: 10.0,
            y: 160.0,
            w: 100.0,
            h: 100.0,
            color: "#f6d32d".into(),
            text: "hold".into(),
            created: "2026/11/12".into(),
        }],
    };
    let note = board.notes[0].clone();
    let items = [
        ItemId::Stroke(0),
        ItemId::Text(0),
        ItemId::Shape(0),
        ItemId::Note(0),
    ];
    let box_ = board
        .step_size(&items, 1.0)
        .expect("a step that changes something reports its box");
    assert_eq!(board.strokes[0].width, 5.0);
    assert_eq!(board.shapes[0].width, 7.0);
    assert_eq!(board.texts[0].size, 20.0);
    assert_eq!(board.notes[0], note, "a note changed its type size");
    // The box covers the ink the step produced, or the frame leaves the thicker edge behind.
    let after = board.texts[0].paint_bounds();
    assert!(box_.contains(after.x, after.y));
    assert!(box_.contains(after.x + after.w, after.y + after.h));
}

/// Nothing to step is not a change: every held item already at its bound leaves the board equal and
/// reports no box, so a press there cannot dirty the file or repaint anything.
#[test]
fn a_size_step_at_the_bound_writes_nothing() {
    let mut board = Board {
        strokes: vec![Stroke {
            color: "#e01b24".into(),
            width: PEN_WIDTH_MAX,
            points: vec![[10.0, 10.0], [110.0, 10.0]],
        }],
        texts: vec![TextItem {
            x: 10.0,
            y: 40.0,
            color: "#ffffff".into(),
            size: TEXT_SIZE_MAX,
            text: "hi".into(),
        }],
        ..Default::default()
    };
    let before = board.clone();
    assert_eq!(
        board.step_size(&[ItemId::Stroke(0), ItemId::Text(0)], 1.0),
        None
    );
    assert_eq!(board, before);
}

/// The step is a fixed amount on either side and stops at the range's ends.
#[test]
fn the_size_step_clamps_to_its_range() {
    assert_eq!(stepped_size(3.0, 1.0, 1.0, 1.0, 16.0), 4.0);
    assert_eq!(stepped_size(3.0, -1.0, 1.0, 1.0, 16.0), 2.0);
    assert_eq!(stepped_size(1.0, -1.0, 1.0, 1.0, 16.0), 1.0);
    assert_eq!(stepped_size(16.0, 1.0, 1.0, 1.0, 16.0), 16.0);
    assert_eq!(stepped_size(72.0, 1.0, 2.0, 8.0, 72.0), 72.0);
}
```

Add `PEN_WIDTH_MAX`, `TEXT_SIZE_MAX` and `stepped_size` to that same import, and in `tests/keys.rs`
delete the whole `stepping_a_size_clamps_to_its_bounds` test and `stepped_size` from the
`sgraffito::input` import.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test store --test keys`
Expected: FAIL to compile — `no function or associated item named 'step_size'` and
`no 'stepped_size' in the root`/`unresolved import` for `sgraffito::canvas`.

- [ ] **Step 3: Move the arithmetic and constants, and implement `Board::step_size`**

In `src/canvas.rs`, beside `TEXT_SIZE_MIN`/`TEXT_SIZE_MAX`:

```rust
/// The pen width `[`/`]` step and the bounds they step it within, in logical pixels.
pub const PEN_WIDTH_MIN: f32 = 1.0;
pub const PEN_WIDTH_MAX: f32 = 16.0;
pub const PEN_WIDTH_STEP: f32 = 1.0;
/// The text size step `[`/`]` apply, in logical pixels; the bounds are `TEXT_SIZE_MIN`/`TEXT_SIZE_MAX`.
pub const TEXT_SIZE_STEP: f32 = 2.0;

/// One size step, clamped to its own range: the one arithmetic the brush and a held selection share.
pub fn stepped_size(current: f32, dir: f32, step: f32, min: f32, max: f32) -> f32 {
    (current + dir * step).clamp(min, max)
}
```

Delete `PEN_WIDTH_MIN`, `PEN_WIDTH_MAX`, `PEN_WIDTH_STEP` and `TEXT_SIZE_STEP` from `src/app.rs`, and
`stepped_size` from `src/input.rs`. In `src/app.rs`, add them to the `use crate::canvas::{…}` import and
drop `stepped_size` from the `use crate::input::{Nav, PinchData, stepped_size};` import. The comments on
the two constants that stay, `PEN_WIDTH` and `TEXT_SIZE`, no longer mention a range — the range moved
with the step.

In `src/canvas.rs`, in `impl Board` after `recolour`:

```rust
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
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --test store --test keys`
Expected: PASS — the three new store tests and the unchanged key tests.

- [ ] **Step 5: Commit**

```bash
git add src/canvas.rs src/input.rs src/app.rs tests/store.rs tests/keys.rs
git commit -m "Step the size of what a selection holds"
```

---

### Task 2: The keys reach a selection, and the README says so

**Files:**

- Modify: `src/app.rs` (`nudge_size`, a new `step_selection_size` beside `recolour_selection`)
- Modify: `README.md` (the `[` / `]` key-table row, the brush paragraph, the selection limitation)

**Interfaces:**

- Consumes: `Board::step_size`, `canvas::stepped_size` and the brush constants (Task 1); `App::doc_changed_at(key, box_)`, the `outputs` map, `out.board()`.
- Produces: `App::step_selection_size(&mut self, dir: f32)` — no return value, nothing consumes it.

- [ ] **Step 1: Step the brush, then what the selection holds**

In `src/app.rs`, after the brush has been stepped at the end of `nudge_size`, before
`self.refresh_toolbar()`:

```rust
        self.step_selection_size(dir);
```

and add, beside `recolour_selection`:

```rust
    /// The other half of a size step: what the selection holds takes the step too, as one change, the way
    /// `recolour_selection` applies a colour chip to it. The list is taken first, because the boards are
    /// borrowed mutably in the loop — and two outputs may each hold a selection, on different boards.
    fn step_selection_size(&mut self, dir: f32) {
        let held: Vec<(u32, String, Vec<ItemId>)> = self
            .outputs
            .iter()
            .filter_map(|(key, out)| {
                out.overlay
                    .selection
                    .as_ref()
                    .map(|s| (*key, out.board().to_string(), s.items.clone()))
            })
            .collect();
        let mut changed = Vec::new();
        for (key, board_name, items) in held {
            if let Some(box_) = self
                .doc
                .boards
                .get_mut(&board_name)
                .and_then(|ann| ann.step_size(&items, dir))
            {
                changed.push((key, box_));
            }
        }
        for (key, box_) in changed {
            self.doc_changed_at(key, box_);
        }
    }
```

- [ ] **Step 2: Run the gate**

Run: `cargo fmt --check && cargo clippy --all-targets && cargo test --lib`
Expected: clean formatting, 0 warnings, every library test passes. No test targets the app glue itself:
`src/app.rs`'s tests only reach pure helpers and the app needs a Wayland connection, so Review Focus
items 3 and 4 are the reviewer's.

- [ ] **Step 3: Update the README**

Replace the `[` / `]` key-table row with:

> | `[` / `]` | shrink / grow the active tool's size — pen width, or text size while the text tool is armed — and, with a selection held, the width of its strokes and shapes and the size of its text boxes |

Replace the sentence that begins "Pen width and text size are brush settings" with:

> Pen width and text size are brush settings, like the colour: they apply to content created
> afterwards, and with a selection held they also step what it holds — each selected stroke's and
> shape's width by the pen step inside 1–16, each selected text box's size by the text step inside
> 8–72, and a note never, because its type size is its own — as one change per press. `[` and `]`
> adjust the active tool's size, bounded to 1–16 logical pixels for the pen and 8–72 for text. The
> selection tool creates nothing, so with it armed the readout and both keys stay on the pen width,
> which is the size the next stroke will take; a selected text box grows or shrinks without the
> readout moving, because the readout is the brush's.

In the selection limitation under Known limitations, replace "a text box's size and a stroke's width
belong to the element, and a resize of a selection scales its geometry rather than thickening its
pen." with:

> a resize of a selection scales its geometry rather than thickening its pen, so a stroke's or
> shape's width and a text box's size change through `[` and `]` on the selection and not through a
> handle, and nothing can be resized from a corner of its own.

- [ ] **Step 4: Run the whole suite**

Run: `cargo test --no-fail-fast`
Expected: every test passes except `the_preedit_renders_at_the_cursor`, which fails identically
before this change (it compares ink positions under this machine's fonts) and is not this change's.

- [ ] **Step 5: Commit**

```bash
git add src/app.rs README.md
git commit -m "Let the size keys step a held selection, and say so in the README"
```
