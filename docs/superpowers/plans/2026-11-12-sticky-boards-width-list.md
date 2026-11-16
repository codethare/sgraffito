# A sticky boards list, a width list on hover, and an opening transition — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The `boards` list stays open until a pick, a press elsewhere or `Esc`; the width readout becomes a control whose hover opens a list of `smaller width` / `larger width`; and either list eases open over 140 ms instead of appearing whole.

**Architecture:** The rail already owns one panel at a time through `open_panel`, `panel_owner` and the `set_*` pair; the width list is a third owner on that machinery, and the only new trigger is a hover. The boards list stops being dismissed by motion alone, which is one rule in `panel_stays_open`. The opening transition is the panel's own progress beside the capsule's, carried in the `Toolbar` value and read only where a row's width is drawn, so the settled layout, hits and damage box do not move.

**Tech Stack:** Rust 2024, no new dependencies.

**Spec:** `docs/superpowers/specs/2026-11-12-sticky-boards-width-list-design.md`

## Global Constraints

- Gate: `cargo fmt --check`, `cargo clippy --all-targets` (0 warnings), `cargo test`.
- One panel at a time; a hover never dismisses a list opened by a click or a key.
- Hit boxes, `toolbar_bounds` and the layout are the settled panel, at every frame of the opening.
- The panel rows never animate on hover; only opening does, and closing is immediate.
- `[` / `]` keep working, and pick from the width list.
- English in code comments, README, commit messages.
- Commit style: imperative sentence, no prefix.

## Review Focus

The five failure modes the spec implies that no step's test can exercise, most likely first.

1. **A hover-opened list that flickers.** `set_size(true)` must not clear `toolbar_hover`, unlike `set_boards`, or the frame that opened the panel forgets the block the pointer is on and the next motion re-opens it. The pure tests cannot see it; the reviewer reads `set_size` and `update_toolbar_hover` together.
2. **A hover stealing an open list.** Hovering the readout while `boards` or the drawer is open must not close it; the hover branch only opens when `panel_owner()` is `None`. Reviewer checks the guard.
3. **The opening's hit boxes.** A click during the 140 ms must land where the row will be, so `toolbar_blocks` and `toolbar_hit` must be identical at `panel_grow` 0 and 1. Pinned by a unit test and checked again by the reviewer in the width formula.
4. **The boards list staying open while a text box is edited or a drag starts.** Motion never closes it now, but a press elsewhere, a pick, `Esc` and a mode switch still must. Reviewer walks those paths in `input.rs`.
5. **The readout's empty label and the invariant test.** A control with no key states no key: `key_of(Size)` is `None`, `labelled` formats nothing for it, and the invariant test must not count it. Reviewer checks the test's new arm rather than only the passing count.

---

### Task 1: The boards list stays open

**Files:**
- Modify: `src/render.rs` (`panel_stays_open`)
- Test: `src/render.rs` (unit tests at the foot)
- Modify: `README.md` (the boards paragraph)

**Interfaces:**
- Consumes: nothing.
- Produces: no new names; `panel_stays_open(hover, owner)` keeps its signature and gains the one owner that is never dismissed by hover state.

- [ ] **Step 1: Write the failing test**

In `src/render.rs`, in `mod tests` beside `the_drawer_and_its_closing_rules_name_the_same_controls`:

```rust
    /// A menu opened by a click stays until it is dismissed; the drawer, which the pointer opens by
    /// walking into it, leaves with the pointer. The boards list is the first kind.
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
        }
        assert!(
            !panel_stays_open(None, Some(ToolbarAction::More)),
            "the drawer stopped leaving with the pointer"
        );
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib the_boards_list_stays_open_when_the_pointer_leaves`
Expected: FAIL at the `None` case — `the boards list closed with None under the pointer`.

- [ ] **Step 3: Make the boards owner unconditional**

In `src/render.rs`:

```rust
/// Whether the open panel stays open with the pointer on `hover`: on the block that opened it, or on one
/// of its own rows. The drawer is the one panel the pointer opens by walking into it, so it leaves with
/// the pointer; the boards list is opened by a click or a key and dismissed by a pick, a press elsewhere
/// or `Esc`, so no hover state closes it.
pub fn panel_stays_open(hover: Option<ToolbarAction>, owner: Option<ToolbarAction>) -> bool {
    matches!(owner, Some(ToolbarAction::Boards))
        || match owner {
            Some(owner) => hover == Some(owner) || hover.is_some_and(picks_from_panel),
            None => false,
        }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib`
Expected: PASS, including the drawer's own closing rules.

- [ ] **Step 5: Say it in the README**

In the boards paragraph, after "only one panel is open at a time", add:

> Unlike the drawer, it leaves only when a board is picked, when the pointer presses anywhere outside
> it, or on `Esc`: moving the pointer off the list does not close it.

- [ ] **Step 6: Commit**

```bash
git add src/render.rs README.md
git commit -m "Let the boards list stay open until it is dismissed"
```

---

### Task 2: The width readout opens a list

**Files:**
- Modify: `src/canvas.rs` (`ToolbarAction`, `key_of` is in `src/render.rs`; `Toolbar.size_open`)
- Modify: `src/render.rs` (`key_of`, `labelled`, `open_panel`, `picks_from_panel`, `same_layout`, `rail_candidates`, a new `size_rows`, the unit tests)
- Modify: `src/app.rs` (`size_open`, `set_size`, `panel_owner`, `update_toolbar_hover`, `current_toolbar`, `apply_toolbar_action`)
- Modify: `tests/render.rs` (its `toolbar()` helper and the panel tests)
- Modify: `README.md` (the rail paragraph, the `[`/`]` row)

**Interfaces:**
- Consumes: `panel_stays_open` (Task 1).
- Produces: `ToolbarAction::Size` (a rail control with no key, which opens the width list); `Toolbar.size_open: bool`; `render::size_rows(toolbar) -> Vec<Block>`; `App::set_size(&mut self, open: bool)`.

- [ ] **Step 1: Write the failing tests**

In `src/render.rs`, in `mod tests`:

```rust
    /// The width list is the two steps, in reading order, and the readout itself is a control that says
    /// nothing: it has no key, so its label is empty and its mark is the value.
    #[test]
    fn the_width_list_is_the_two_steps() {
        let toolbar = Toolbar {
            size_open: true,
            size: 3.0,
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

    /// The readout is a rail block with an action and no label: hovering it opens the list, so it must be
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
```

In the existing `every_block_states_the_key_that_applies_it`, the readout now carries an action: replace the
`let Some(action) = block.action else { … }` prologue with the two rules the rail now has —

```rust
        for block in &blocks {
            let Some(action) = block.action else {
                // The only actionless block left is the size readout, if a future one returns it.
                assert!(block.label.is_empty());
                assert!(matches!(block.mark, Mark::Value(_)));
                continue;
            };
            let Some(key) = key_of(action) else {
                // A control with no key states no key: the width readout opens its list on hover.
                assert!(block.label.is_empty(), "'{}' states a key it has not", block.label);
                assert!(matches!(block.mark, Mark::Value(_)));
                continue;
            };
            assert!(
                block.label.ends_with(&format!("({key})")),
                "'{}' does not end with its key ({key})",
                block.label
            );
            …
```

and drop `ToolbarAction::Size` from the `applies` match by adding `ToolbarAction::Size => continue,`… which the key
filter has already made unreachable; write it as `ToolbarAction::Size => unreachable!("no key, filtered above")`.
The counts change: the rail loses the two `SizeStep` blocks, so `blocks.len()` becomes 20 and `stated` 19.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib`
Expected: FAIL to compile — `no variant named 'Size'`, `struct Toolbar has no field named 'size_open'`,
`cannot find function 'size_rows'`.

- [ ] **Step 3: Implement the readout and the list**

`src/canvas.rs`, in `enum ToolbarAction` after `SizeStep`:

```rust
    /// Open the list of width steps. The readout is the only control that carries it: hovering it opens
    /// the list, and the two steps live there rather than beside it on the rail.
    Size,
```

and in `struct Toolbar`, beside `boards_open`:

```rust
    /// Whether the width list is open. It is the one panel a hover opens, and it is the same one-panel
    /// rule as the other two.
    pub size_open: bool,
```

`src/render.rs`: `key_of` becomes `fn key_of(action: ToolbarAction) -> Option<&'static str>` with
`ToolbarAction::Size => None` and `Some(…)` around every existing arm; `labelled` formats the key only when
there is one:

```rust
fn labelled(action: ToolbarAction, what: &str) -> String {
    match key_of(action) {
        Some(key) => format!("{what} ({key})"),
        None => what.to_string(),
    }
}
```

`rail_candidates` drops its two `SizeStep` blocks and makes the readout:

```rust
    blocks.push(Block {
        mark: Mark::Value(format!("{:.0}", toolbar.size)),
        column: 0,
        // A control with no label: its value is its face, and hovering it opens the steps' list.
        label: String::new(),
        action: Some(ToolbarAction::Size),
        active: false,
        gap: TOOLBAR_GROUP_GAP,
    });
```

beside `board_rows`:

```rust
/// The width list's rows: the two steps, smaller first, so the panel reads in the order the rail did.
fn size_rows(toolbar: &Toolbar) -> Vec<Block> {
    let axis = if toolbar.tool == Tool::Text { "size" } else { "width" };
    [(-1.0, "smaller"), (1.0, "larger")]
        .into_iter()
        .map(|(dir, word)| Block {
            mark: Mark::Icon(if dir < 0.0 { Icon::Smaller } else { Icon::Larger }),
            column: 1,
            label: labelled(ToolbarAction::SizeStep(dir), &format!("{word} {axis}")),
            action: Some(ToolbarAction::SizeStep(dir)),
            active: false,
            gap: TOOLBAR_GAP,
        })
        .collect()
}
```

`open_panel` gains the third case (`else if toolbar.size_open { Some((Size, size_rows(toolbar))) }`),
`picks_from_panel` adds `ToolbarAction::SizeStep(_)`, and `same_layout` adds `a.size_open == b.size_open`.
Every full `Toolbar` literal gains the field: the two helpers `rail_for_legend` and `rail_state` in
`src/render.rs` and `toolbar()` in `tests/render.rs` (`size_open: false`); the literals that update one of
them inherit it.

`src/app.rs`: `App` gains `size_open: bool` (beside `boards_open`) and `panel_owner` its case;
`set_size` is `set_boards` with the hover left alone:

```rust
    /// Open or close the width list, the one panel a hover opens. Unlike `set_boards` it keeps the hover:
    /// clearing it would make the frame that opened the panel forget the block the pointer is on.
    pub(crate) fn set_size(&mut self, open: bool) {
        if self.size_open == open {
            return;
        }
        self.boards_open = false;
        self.more = false;
        self.size_open = open;
        self.refresh_toolbar();
    }
```

`current_toolbar` carries `size_open: self.size_open`; `apply_toolbar_action` gains `ToolbarAction::Size => {}`
(a click on the readout neither steps nor closes anything); and `update_toolbar_hover`, after computing
`hover`, opens the list on a hover that finds nothing else open:

```rust
        // Hovering the readout opens the width list, but a hover never dismisses a list that a click or a
        // key opened: those are dismissed by their own rules.
        if hover == Some(ToolbarAction::Size) && self.panel_owner().is_none() {
            self.set_size(true);
        }
```

and the closing branch calls `self.set_size(false)` beside the other two.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib` and `cargo test --test render`
Expected: PASS — the two new unit tests, the invariant test with its new counts, and the existing panel
geometry tests. The `tests/render.rs` panel tests build their toolbar through `toolbar()`, which now carries
`size_open: false`.

- [ ] **Step 5: Say it in the README**

In the rail paragraph, replace the sentence about the size keys with one that says the readout opens its
list and the two steps live there; in the `[` / `]` key-table row, add "(the run's steps are in the width
list, opened by hovering the number)".

- [ ] **Step 6: Commit**

```bash
git add src/canvas.rs src/render.rs src/app.rs tests/render.rs README.md
git commit -m "Let the width readout open its own list of steps"
```

---

### Task 3: The lists ease open

**Files:**
- Modify: `src/canvas.rs` (`Toolbar.panel_grow`)
- Modify: `src/app.rs` (`panel_open_since`, `panel_grow`, `animating`, `current_toolbar`, `set_size`, `set_boards`)
- Modify: `src/render.rs` (the row width formula; a unit test and a render test)
- Modify: `tests/render.rs` (the `toolbar()` helper's new field)
- Modify: `README.md` (one sentence on the lists easing open)

**Interfaces:**
- Consumes: `Toolbar.size_open`, `App::set_size` (Task 2).
- Produces: `Toolbar.panel_grow: f32`; `App::panel_grow(&self, now: Instant) -> f32`.

- [ ] **Step 1: Write the failing tests**

In `src/render.rs`, in `mod tests`:

```rust
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
        let mut renderer = Renderer::new();
        let layout = |renderer: &mut Renderer, opening: f32| {
            let pill = renderer
                .cached_toolbar_pill(&rail(opening), (900.0, 1000.0))
                .expect("the rail has a layout");
            pill.controls
                .iter()
                .map(|control| (control.block.action, control.y, control.width))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            layout(&mut renderer, 0.0),
            layout(&mut renderer, 1.0),
            "the opening moved the controls"
        );
    }
```

In `tests/render.rs`, beside the panel painting test:

```rust
/// A panel opening uncovers its rows: at the start of the transition the panel's column carries only the
/// row squares, and at the end their labels.
#[test]
fn a_panel_eases_open_from_squares_to_rows() {
    let (w, h) = (900u32, 1000u32);
    let draw = |opening: f32| {
        let mut buf = buffer(w, h);
        Renderer::new().render(
            &mut buf,
            w,
            h,
            1.0,
            View::default(),
            &Board::default(),
            &Overlay {
                toolbar: Some(Toolbar {
                    panel_grow: opening,
                    ..boards_rail(&[("Board 1", true), ("office", false)])
                }),
                ..Default::default()
            },
            None,
        );
        buf
    };
    let ink = |buf: &[u8]| {
        (40..260)
            .flat_map(|x| (0..h).map(move |y| (x, y)))
            .filter(|(x, y)| alpha(buf, w, *x, *y) > 0)
            .count()
    };
    let (start, end) = (ink(&draw(0.0)), ink(&draw(1.0)));
    assert!(start > 0, "the row squares are not drawn at the start");
    assert!(start < end, "the opening uncovered nothing: {start} against {end}");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib a_panel_opening_does_not_move_its_layout` and `cargo test --test render a_panel_eases_open_from_squares_to_rows`
Expected: FAIL to compile — `struct Toolbar has no field named 'panel_grow'`.

- [ ] **Step 3: Carry the opening and draw by it**

`src/canvas.rs`, in `struct Toolbar`:

```rust
    /// How far the open panel has opened, 0 to 1: the rows' capsules grow from their squares the way a
    /// block's pop-out does, and the layout, the hits and the damage box are the settled panel throughout.
    pub panel_grow: f32,
```

`src/app.rs`: `App` gains `panel_open_since: Option<Instant>`, set to `Some(Instant::now())` when `set_size`
or `set_boards` opens a panel and to `None` when either closes one. Add beside `toolbar_grow`:

```rust
    /// How far the open panel has opened, 0 to 1.
    fn panel_grow(&self, now: Instant) -> f32 {
        hover_grow(self.panel_open_since, now)
    }
```

`current_toolbar` carries `panel_grow: self.panel_grow(Instant::now())`, and `animating` adds
`|| self.panel_grow(Instant::now()) < 1.0` beside the pop-out's test.

`src/render.rs`, in `draw_toolbar`, replace the width line:

```rust
            // A rail block's capsule opens under the pointer; a panel's rows open once, when the panel
            // does. Both grow from the block square, and neither moves while it opens.
            let opening = if control.block.column == 0 {
                grow
            } else {
                toolbar.panel_grow.clamp(0.0, 1.0)
            };
            let width = if control.stretched {
                TOOLBAR_BLOCK + (control.width - TOOLBAR_BLOCK) * opening
            } else {
                control.width
            };
```

Every full `Toolbar` literal gains `panel_grow: 1.0` where it wants a settled rail: the helpers
`rail_for_legend`, `rail_state` and `toolbar()`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib` and `cargo test --test render`
Expected: PASS — the layout is identical at both ends of the opening, the render test sees more ink at the
end, and every existing panel and rail test is unchanged.

- [ ] **Step 5: Say it in the README**

In the rail paragraph, after the sentence on the width list, add: "A list eases open the way a block's
pop-out does, over the same 140 ms; it closes at once."

- [ ] **Step 6: Run the whole suite**

Run: `cargo fmt --check && cargo clippy --all-targets && cargo test --no-fail-fast`
Expected: `fmt` clean, 0 warnings, every test passes except `the_preedit_renders_at_the_cursor`, which fails
identically before this plan.

- [ ] **Step 7: Commit**

```bash
git add src/canvas.rs src/app.rs src/render.rs tests/render.rs README.md
git commit -m "Ease a list open instead of snapping it into place"
```
