# Fill colour picker Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Put a fill colour in hand on the rail — a second swatch beside the stroke swatch, with a
strip of the four other palette colours and one "no fill" chip — so circles, rectangles and diamonds
are drawn and recoloured with a chosen inside colour, and the bucket paints with it.

**Architecture:** `Shape.fill: Option<String>` already exists and already paints, so the change is
rail state plus two writers. `Toolbar.colors_open: bool` becomes `Toolbar.open: Option<Palette>`
(`Stroke` | `Fill`) and gains `fill: Option<usize>`; `color_strip` becomes `palette_strip`, which
serves either swatch with the chips it is not showing. `App::set_fill` mirrors `App::set_color`
(in hand + `Annotation::recolour_fill` on the selection, one undo entry), and `begin_shape` and
`bucket_fill` read `fill_idx` instead of `color_idx`.

**Tech Stack:** Rust, `tiny-skia` via the existing renderer, `serde` for the unchanged schema, `jj`
for history. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-11-27-fill-colour-picker-design.md`

## Global Constraints

- No new dependencies.
- `VERSION = 6`, `READABLE_VERSION = 7` unchanged; `Shape.fill` keeps `#[serde(default, skip_serializing_if = "Option::is_none")]`, so a shape with no fill still writes no `fill` key.
- One undo step per user action: a fill pick with a selection is one entry; a bucket click that changes nothing is none.
- Keep raw input raw: nothing is persisted that is derived.
- Existing conventions: `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test`. The baseline failure `tests/render.rs::the_preedit_renders_at_the_cursor` is not ours.
- English in code, comments, docs and commit messages.
- Never commit `AGENTS.md`, `.agents/**` or `openspec/**`; every commit is path-scoped with `jj commit <paths> -m "..."`.

## Review Focus

- **Undo after a fill.** A rectangle drawn with a fill colour, then `Ctrl+Z`: the shape goes, and no fill is left behind on the previous frame or on a shape drawn earlier. Picking a fill colour with a selection is exactly one entry, and undoing it restores the old fill byte for byte (`None` included).
- **The bucket with no fill in hand.** `B` then a click on a filled shape clears it in one entry; a second click on the now-clear shape records nothing, and the early return must not damage the board.
- **A bottom rail on a short output.** The fill strip opens away from the rail, stays inside the output (`TOOLBAR_MARGIN`), and every chip it draws is hittable at the point it drew itself.
- **A configured palette.** `colors = [...]` with five arbitrary entries: the fill swatch and the fill strip follow the config (not `PALETTE`), and "no fill" stays distinguishable from a colour, including a white or a fully transparent one.
- **A mixed selection.** A selection holding a rectangle, a line, an arrow, a text box and a note: a fill pick changes only the rectangle; text and note keep their colour and their fill-free state.

---

### Task 1: Give a selection an inside colour

**Files:**
- Modify: `src/canvas.rs` (`Annotation::recolour_fill`, next to `recolour` at `:1339`)
- Test: `tests/canvas_hit.rs`

**Interfaces:**
- Consumes: `ItemId`, `Shape::kind`, `ShapeKind::is_closed`, `union_into`, `paint_bounds`.
- Produces: `Annotation::recolour_fill(&mut self, items: &[ItemId], fill: Option<&str>) -> Option<Rect>`.

- [ ] **Step 1: Write the failing test** in `tests/canvas_hit.rs`

```rust
#[test]
fn a_fill_recolours_only_the_closed_shapes_it_holds() {
    let mut board = Board {
        shapes: vec![
            shape(ShapeKind::Rect, 0.0, 0.0, 100.0, 80.0),
            shape(ShapeKind::Line, 0.0, 0.0, 100.0, 80.0),
        ],
        texts: vec![text(10.0, 10.0, "a label")],
        ..Board::default()
    };
    let held = [ItemId::Shape(0), ItemId::Shape(1), ItemId::Text(0)];
    let box_ = board.recolour_fill(&held, Some("#33d17a")).expect("a box");
    assert_eq!(board.shapes[0].fill.as_deref(), Some("#33d17a"));
    assert_eq!(board.shapes[1].fill, None, "a line has no inside");
    assert_eq!(board.texts[0].color, "#ffffff");
    assert_eq!(board.shapes[0].paint_bounds(), box_);
    let cleared = board.recolour_fill(&held, None).expect("a box");
    assert_eq!(board.shapes[0].fill, None);
    assert_eq!(cleared, box_);
}
```

Use the file's existing shape/text helpers; add a `shape(kind, x, y, w, h)` local if there is not one.

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test --test canvas_hit a_fill_recolours_only_the_closed_shapes_it_holds`
Expected: compile error, `no method named recolour_fill`.

- [ ] **Step 3: Implement it** in `src/canvas.rs`, beside `recolour`

A fill is the inside of a closed outline, so lines, arrows, text and notes are skipped rather than
given one; the box is the union of the closed shapes it did change.

- [ ] **Step 4: Run the test**

Run: `cargo test --test canvas_hit a_fill_recolours_only`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
jj commit src/canvas.rs tests/canvas_hit.rs docs/superpowers/specs/2026-11-27-fill-colour-picker-design.md -m "feat: recolour the closed shapes of a selection with a fill colour"
```

---

### Task 2: The rail carries a fill colour in hand

**Files:**
- Modify: `src/canvas.rs` (`ToolbarAction` `:168`, new `Palette`, `Toolbar` `:3927`)
- Modify: `src/render.rs` (`key_of` `:270`, `badge_of` `:316`, `same_layout` `:5151`)
- Test: `src/render.rs` (`mod tests`), `tests/render.rs`, `tests/config.rs`

**Interfaces:**
- Consumes: `Toolbar`, `ToolbarAction`, `config::current().colors`.
- Produces:
  - `pub enum Palette { Stroke, Fill }`
  - `ToolbarAction::FillColor(Option<usize>)`
  - `Toolbar { pub fill: Option<usize>, pub open: Option<Palette>, pub palette_grow: f32, .. }` replacing `colors_open`/`colors_grow`
  - `key_of(FillColor(_)) -> None`, `badge_of(FillColor(_)) -> None`

- [ ] **Step 1: Write the failing test** in `src/render.rs`'s `mod tests`

```rust
#[test]
fn the_rail_reopens_its_layout_for_a_fill_or_an_open_palette() {
    let rail = tests::rail_for_legend();
    let mut filled = rail.clone();
    filled.fill = Some(2);
    assert!(!same_layout(&rail, &filled), "a fill pick reuses the old layout");
    let mut opened = rail.clone();
    opened.open = Some(Palette::Fill);
    assert!(!same_layout(&rail, &opened), "an open strip reuses the old layout");
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test --lib the_rail_reopens_its_layout_for_a_fill_or_an_open_palette`
Expected: compile error — no field `fill`, no `Palette`.

- [ ] **Step 3: Rename and add the fields**

In `canvas.rs`: add `FillColor(Option<usize>)` to `ToolbarAction` with a doc comment saying `None`
is no fill and that picking it does what the swatch does; add `Palette`; replace `colors_open: bool`
with `open: Option<Palette>`, rename `colors_grow` to `palette_grow`, and add `fill: Option<usize>`
after `color`. Update `key_of` and `badge_of` to return `None` for `FillColor(_)`, and add both new
fields to `same_layout`.

- [ ] **Step 4: Make the tree compile again**

Every `Toolbar` literal gets `fill: None, open: None, palette_grow: 1.0`: `src/app.rs:966`
(`current_toolbar`, where `fill: self.fill_idx` and `open: self.open_palette` come later),
`src/render.rs:8387` (`rail_for_legend`), `tests/render.rs:381` and its two struct-update sites,
`tests/config.rs:45`. Run `cargo build --all-targets`.

- [ ] **Step 5: Run the tests**

Run: `cargo test --lib the_rail_reopens_its_layout_for_a_fill_or_an_open_palette && cargo test`
Expected: PASS, with the same baseline failure as before.

- [ ] **Step 6: Commit**

```bash
jj commit src/canvas.rs src/render.rs src/app.rs tests/render.rs tests/config.rs -m "feat: carry a fill colour in hand on the rail"
```

---

### Task 3: Two swatches on the rail

**Files:**
- Modify: `src/render.rs` (`Mark` `:142`, `rail_candidates` `:407`, the mark-drawing match `:2999` / `:3121`, `separates` `:354`)
- Test: `src/render.rs` `mod tests`, `tests/render.rs`

**Interfaces:**
- Consumes: `Toolbar.fill`, `ToolbarAction::FillColor`, `Mark`.
- Produces: `Mark::Fill(Option<usize>)`; `rail_candidates` yields `[Color(active), FillColor(fill), ..tools.., Size]`.

- [ ] **Step 1: Write the failing tests** in `src/render.rs`'s `mod tests`

```rust
#[test]
fn the_palette_group_holds_a_stroke_and_a_fill_swatch() {
    let mut rail = tests::rail_for_legend();
    let blocks = toolbar_blocks(&rail, 1000.0);
    assert_eq!(blocks.len(), 17, "a drawn block cannot be hit");
    assert_eq!(blocks[0].action, Some(ToolbarAction::Color(0)));
    assert_eq!(blocks[1].action, Some(ToolbarAction::FillColor(None)));
    assert_eq!(blocks[1].mark, Mark::Fill(None));
    assert_eq!(blocks[1].label, "no fill");
    assert!(!blocks[1].active, "an empty hand is not accented");
    rail.fill = Some(1);
    let blocks = toolbar_blocks(&rail, 1000.0);
    assert_eq!(blocks[1].mark, Mark::Fill(Some(1)));
    assert_eq!(blocks[1].label, "fill colour 2");
    assert!(blocks[1].active);
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test --lib the_palette_group_holds_a_stroke_and_a_fill_swatch`
Expected: FAIL — no `Mark::Fill`, 16 blocks.

- [ ] **Step 3: Add the swatch**

Add `Mark::Fill(Option<usize>)` with a doc comment; push the block in `rail_candidates` after the
stroke swatch, labelled `format!("fill colour {}", index + 1)` when it holds one and `no fill` when
it does not, with `active: toolbar.fill.is_some()`; move the stroke swatch's `TOOLBAR_GROUP_GAP` to
the fill block so the group still ends where it did. Extend `separates` so a rule still falls after
either swatch.

- [ ] **Step 4: Draw it**

Factor the square-and-edge painting out of the `Mark::Swatch(index)` arm into a helper that takes the
colour and the active/stretched/hovered flags, and call it from both arms: `Mark::Swatch(index)`
with `colors[*index]`, `Mark::Fill(Some(index))` the same without a badge, `Mark::Fill(None)` with
`TOOLBAR_BLOCK_FILL` as the square and a diagonal from the square's top-left to its bottom-right in
`TOOLBAR_LABEL_TEXT`. Add `Mark::Fill(_)` to the skip arm of the icon match.

- [ ] **Step 5: Fix the tests that counted blocks**

`every_block_states_the_key_that_applies_it`: `blocks.len()` becomes 17 and `stated` stays 15; the
`key_of(action) == None` branch must accept a labelled `FillColor(_)` swatch (only the width readout
has an empty label there). `the_palette_group_holds...` in `tests/render.rs` and the `16` in
`tests/render.rs:609` become 17. `short_rules_mark_the_rail_families` gains the case that no rule
falls between the two swatches.

- [ ] **Step 6: Run the tests**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
jj commit src/render.rs tests/render.rs -m "feat: show a fill swatch beside the stroke swatch"
```

---

### Task 4: One strip at a time, and the chips it shows

**Files:**
- Modify: `src/render.rs` (`ColorStrip` `:5262`, `color_strip_plate` `:5267`, `color_strip` `:5297`, `toolbar_hit_at` `:3736`, `toolbar_over_colors` `:3707`)
- Test: `src/render.rs` `mod tests`

**Interfaces:**
- Consumes: `Palette`, `Toolbar.open`, `Toolbar.fill`.
- Produces:
  - `struct PaletteStrip { palette: Palette, plate: Rect, chips: Vec<(Option<usize>, Rect)> }`
  - `fn palette_strip(pill, toolbar, origin, position) -> Option<PaletteStrip>`
  - `Renderer::toolbar_over_palette(&mut self, toolbar, width, height, x, y) -> Option<Palette>`

- [ ] **Step 1: Write the failing test** in `src/render.rs`'s `mod tests`

```rust
#[test]
fn the_fill_strip_offers_four_colours_and_no_fill() {
    let mut toolbar = tests::rail_for_legend();
    toolbar.fill = Some(0);
    toolbar.open = Some(Palette::Fill);
    let mut renderer = Renderer::new();
    let surface = (900.0, 1000.0);
    let pill = renderer.toolbar_pill(&toolbar, surface, ToolbarPosition::Left);
    let origin = ToolbarPosition::Left.origin(surface, pill.column);
    let strip = palette_strip(&pill, &toolbar, origin, ToolbarPosition::Left)
        .expect("the opened fill strip is missing");
    assert_eq!(strip.palette, Palette::Fill);
    assert_eq!(strip.chips.len(), 5);
    let picked: Vec<_> = strip.chips.iter().map(|(index, _)| *index).collect();
    assert_eq!(picked, vec![Some(1), Some(2), Some(3), Some(4), None]);
    for (index, rect) in &strip.chips {
        let hit = renderer.toolbar_hit_at(
            ToolbarPosition::Left,
            &toolbar,
            surface.0,
            surface.1,
            rect.x + rect.w / 2.0,
            rect.y + rect.h / 2.0,
        );
        assert_eq!(hit, Some(ToolbarAction::FillColor(*index)));
    }
    let gap = strip.chips[0].1.x + TOOLBAR_BLOCK + TOOLBAR_GAP / 2.0;
    assert_eq!(
        renderer.toolbar_over_palette(&toolbar, surface.0, surface.1, gap, strip.plate.y + 4.0),
        Some(Palette::Fill)
    );
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test --lib the_fill_strip_offers_four_colours_and_no_fill`
Expected: compile error — no `palette_strip`, no `toolbar_over_palette`.

- [ ] **Step 3: Generalise the strip**

`palette_strip` reads `toolbar.open`; the active chip is the index it hides (`toolbar.color`'s
position for `Stroke`, `toolbar.fill` for `Fill`); the plate is sized for `chips.len() + 1` so the
fill card is one chip wider; `None` is appended last for `Fill` when the active fill is a colour, and
always present otherwise. `toolbar_hit_at` maps a stroke chip to `Color(i)` and a fill chip to
`FillColor(i)`. `toolbar_over_palette` returns which palette's plate the pointer is inside.

- [ ] **Step 4: Run the test**

Run: `cargo test --lib the_fill_strip_offers_four_colours_and_no_fill && cargo test --lib the_colour_strip`
Expected: PASS; rename `the_colour_strip_shows_only_the_active_chip_until_opened` to
`the_stroke_strip_shows_only_the_active_chip_until_opened` and switch it to `open: Some(Palette::Stroke)`.

- [ ] **Step 5: Commit**

```bash
jj commit src/render.rs tests/render.rs -m "feat: serve either palette from one strip"
```

---

### Task 5: Draw the fill strip and its crossed chip

**Files:**
- Modify: `src/render.rs` (`draw_color_strip` `:3328`)
- Test: `tests/render.rs`

**Interfaces:**
- Consumes: `PaletteStrip`, `Mark`'s square helper from Task 3.
- Produces: nothing new; the strip draws 5 chips for `Fill` and 4 for `Stroke`.

- [ ] **Step 1: Write the failing test** in `tests/render.rs`

Render a rail with `fill: Some(0)`, `open: Some(Palette::Fill)` on a bottom-docked position
(`config` is global; use the existing pattern of the pixel tests in this file for `ToolbarPosition`)
and assert: the chip whose index is `None` is painted `TOOLBAR_BLOCK_FILL` (dark) at its centre and
has a lighter pixel on its diagonal, while the chip for `Some(1)` is painted the palette colour at
its centre.

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test --test render`
Expected: FAIL — the strip is not drawn for `Palette::Fill`.

- [ ] **Step 3: Draw it**

`draw_palette_strip` walks `PaletteStrip.chips`: each chip is the rounded square with its palette
colour, `TOOLBAR_HOVER_FILL` when hovered, `TOOLBAR_SWATCH_EDGE` edge; the `None` chip is drawn by
the same helper as `Mark::Fill(None)`; badges (`1`–`5`) are drawn only for `Palette::Stroke`. The
hover label under the strip names `colour {n}` for stroke, `fill colour {n}` for a fill chip and
`no fill` for the last one.

- [ ] **Step 4: Run the test**

Run: `cargo test --test render`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
jj commit src/render.rs tests/render.rs -m "feat: draw the fill strip with its no-fill chip"
```

---

### Task 6: Wire the pick up

**Files:**
- Modify: `src/app.rs` (`fill_idx`, `open_palette`, `palette_since`/`palette_until`, `reveal_colors` `:3830`, `hover_colors` `:3841`, `close_colors` `:3857`, `set_color` `:3866`, new `set_fill`, `recolour_fill_selection`, `current_toolbar` `:966`, `begin_shape` `:4631`, `bucket_fill` `:4070`, `animating` `:2644`, `tick` `:4992`)
- Modify: `src/input.rs` (`update_toolbar_hover` `:1423`, `apply_toolbar_action` `:1485`)
- Test: `scripts/smoke.sh`

**Interfaces:**
- Consumes: everything above.
- Produces: `App::set_fill(&mut self, fill: Option<usize>)`, `App::hover_palette(&mut self, over: Option<Palette>)`, `App::close_palette(&mut self)`, `App::recolour_fill_selection(&mut self, fill: Option<&str>)`.

- [ ] **Step 1: Add the state and the pick**

`fill_idx: Option<usize>` (default `None`); `open_palette: Option<Palette>` replacing
`colors_open`; `palette_since`/`palette_until` replacing `colors_since`/`colors_until`.
`set_fill` sets `fill_idx`, refreshes the rail and calls `recolour_fill_selection`, which mirrors
`recolour_selection` with `Annotation::recolour_fill`. `current_toolbar` passes `fill` and `open`.
`animating` and `tick` read the renamed fields.

- [ ] **Step 2: Dispatch the action and the hover**

`apply_toolbar_action`: `FillColor(index) => { self.set_fill(index); self.close_palette(); }`, and
`Color(index)` calls `close_palette`. `update_toolbar_hover` maps a chip hover to
`Some(Palette::Stroke)`/`Some(Palette::Fill)` and otherwise asks `toolbar_over_palette`; the pointer
counts as over the rail when either is `Some`.

- [ ] **Step 3: Read the fill in hand where shapes are born and painted**

`begin_shape`: a closed kind takes `fill_idx` mapped through `config::current().colors`, a line or an
arrow takes `None`. `bucket_fill`: the colour comes from `fill_idx`, compared as `Option<String>` so
an unchanged fill returns before the undo entry, exactly as today.

- [ ] **Step 4: Build and run the whole suite**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: no warnings, the same baseline failure as before, everything else green.

- [ ] **Step 5: Prove it on a real rail**

Add a stage to `scripts/smoke.sh` before the last one: `key R`, drag a rectangle, then `M` is not
needed — click the fill swatch, click the first fill chip, draw a second rectangle, sleep 1.2 s, and
assert the board JSON's second shape carries `fill` and the first does not. Then click the fill
swatch, click the last chip (`no fill`), bucket-click the filled rectangle (`shift`? no — `B`, then
the rectangle's centre), sleep, and assert its `fill` is gone. Run `scripts/smoke.sh` and report the
result.

- [ ] **Step 6: Commit**

```bash
jj commit src/app.rs src/input.rs scripts/smoke.sh -m "feat: paint shapes and bucket fills with the fill in hand"
```

---

### Task 7: Say it in the README

**Files:**
- Modify: `README.md`

**Interfaces:**
- Consumes: the behaviour built above.

- [ ] **Step 1: Update the two places that state the old rule**

The `B` row: the bucket paints the fill colour in hand, and clears the shape when the hand holds no
fill. The `O` `R` `D` row: a circle, rectangle or diamond is drawn filled with the fill in hand. Add
one bullet naming the second swatch, what a pick does (in hand + the selection's closed shapes), that
"no fill" is the default, and that fill colours have no shortcut while `1`–`5` stay stroke colours.

- [ ] **Step 2: Check the shortcut table is still true**

Run: `cargo test --lib every_block_states_the_key_that_applies_it && cargo test --test keys`
Expected: PASS — no binding was added.

- [ ] **Step 3: Commit**

```bash
jj commit README.md -m "docs: state the fill swatch and what the bucket now paints"
```
