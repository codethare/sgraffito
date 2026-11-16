# A width list that stays, and a rail that keeps its own blocks — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The width list stops closing on a pointer leave and on its own steps, and a list's walkway stops answering for the rail's blocks.

**Architecture:** Three one-place changes on the rail's existing rules: the width list's owner joins the boards list as one no hover state closes; what closes on a pick becomes its own predicate, with a width step as the exception; and `toolbar_hit` asks the rail's own blocks before the corridor that leads into a panel.

**Tech Stack:** Rust 2024, no new dependencies.

**Spec:** `docs/superpowers/specs/2026-11-12-sticky-width-list-design.md`

## Global Constraints

- Gate: `cargo fmt --check`, `cargo clippy --all-targets` (0 warnings), `cargo test`.
- One panel at a time; only the drawer closes on a pointer leave.
- A width step applies and leaves the list open; `[` and `]` keep working.
- The rail's own blocks answer before any panel's walkway, on every panel.
- The list opens only on entering the readout (the existing entry rule is unchanged).
- English in code comments, README, commit messages; commit style is an imperative sentence with no prefix.

## Review Focus

The five failure modes the spec implies that no step's test can exercise, most likely first.

1. **The closing tail's swap from `picks_from_panel` to `closes_panel_on_pick`.** A drawer pick and a boards pick must still close their panel; only a width step must not. The pure predicate is tested, but the app's tail is glue — the reviewer checks that the tail calls the new predicate and that nothing else used `picks_from_panel` for closing.
2. **The walkway still works.** Rail-first must not cut the strip between the rail's blocks and the panel's column: a point in that strip at any height must still answer with the owner, or the pointer cannot cross from a block into a list without a dead zone.
3. **A hovered block's capsule overlapping the panel's column.** Rail-first puts a wide capsule ahead of the rows at the same height. That order already held (rail hits were first in the vector), so it is unchanged — the reviewer confirms the vector order is untouched.
4. **The sticky width list and the entry rule.** With no leave-close, `opens_size_list`'s entry guard is what stops an `Esc` being undone by jitter; it must stay exactly as it is.
5. **The drawer's changed strip.** A rail block level with a drawer row now keeps its own press; the README's drawer paragraph must say so, and `the_boards_block_owns_its_row_as_far_as_the_panel_and_a_row_owns_its_column` must still pass (its assertions are at the panel's column edge and the block's own row).

---

### Task 1: The width list stays, a step keeps it, and the rail keeps its blocks

**Files:**
- Modify: `src/render.rs` (`panel_stays_open`, a new `closes_panel_on_pick`, `toolbar_hit`)
- Modify: `src/input.rs` (the closing tail's call)
- Modify: `README.md` (the lists paragraph, the drawer paragraph)
- Test: `src/render.rs` (unit tests), `tests/render.rs` (the hit order)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `render::closes_panel_on_pick(action: ToolbarAction) -> bool`; `panel_stays_open` keeps its signature and now stays for the `Size` owner.

- [ ] **Step 1: Write the failing tests**

In `src/render.rs`, in `mod tests`, extend `the_boards_list_stays_open_when_the_pointer_leaves`… no: add two tests beside it:

```rust
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
        assert!(!panel_stays_open(None, Some(ToolbarAction::More)));
    }

    /// A width step is a command you may want to repeat: it applies without putting its list away. Every
    /// other pick still closes the panel it came from.
    #[test]
    fn a_width_step_keeps_its_list_open() {
        assert!(!closes_panel_on_pick(ToolbarAction::SizeStep(-1.0)));
        assert!(!closes_panel_on_pick(ToolbarAction::SizeStep(1.0)));
        assert!(closes_panel_on_pick(ToolbarAction::Board(0)));
        assert!(closes_panel_on_pick(ToolbarAction::NewBoard));
        assert!(closes_panel_on_pick(ToolbarAction::Scrim));
        assert!(!closes_panel_on_pick(ToolbarAction::Tool(Tool::Pen)));
    }
```

In `tests/render.rs`:

```rust
/// An open list's walkway stops at the rail's blocks: with the width list open over the boards block's
/// row, hovering that block answers for itself rather than for the readout that owns the list.
#[test]
fn an_open_list_does_not_shadow_the_rail_beside_it() {
    let (w, h) = (900.0f32, 1000.0f32);
    let mut renderer = Renderer::new();
    let rail = Toolbar {
        size_open: true,
        ..toolbar(Tool::Pen, PALETTE[0], 3.0, None)
    };
    let band = toolbar_bounds(&rail, w, h);
    let base = band.x + 1.0 + 6.0;
    let sweep = sweep_rail(&mut renderer, &rail, w, h, 10.0);
    let boards_y = sweep
        .iter()
        .position(|hit| *hit == Some(ToolbarAction::Boards))
        .expect("the boards block is not on the rail");
    for x in [4, 12, 20, 28] {
        assert_eq!(
            renderer.toolbar_hit(&rail, w, h, base + x as f32, band.y + boards_y as f32 + 5.0),
            Some(ToolbarAction::Boards),
            "the width list answered for the boards block at x={x}"
        );
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib the_width_list_stays_open_when_the_pointer_leaves` and
`cargo test --lib a_width_step_keeps_its_list_open` and
`cargo test --test render an_open_list_does_not_shadow_the_rail_beside_it`
Expected: the two lib tests FAIL to compile (`cannot find function 'closes_panel_on_pick'`) except the
sticky one, which fails on its assertion (`the width list closed with None under the pointer`); the render
test fails with `the width list answered for the boards block at x=4`.

- [ ] **Step 3: Make the three changes**

`src/render.rs`, `panel_stays_open`:

```rust
pub fn panel_stays_open(hover: Option<ToolbarAction>, owner: Option<ToolbarAction>) -> bool {
    matches!(
        owner,
        Some(ToolbarAction::Boards | ToolbarAction::Size)
    ) || match owner {
        Some(owner) => hover == Some(owner) || hover.is_some_and(picks_from_panel),
        None => false,
    }
}
```

with the doc comment's last two sentences rewritten to say that the boards and width lists are opened by a
click or a hover and dismissed by their own rules, and the drawer is the one that leaves with the pointer.

Beside `panel_stays_open`:

```rust
/// Whether a pick out of a panel closes it: every pick does, except a width step, which is a command you
/// may want to repeat. `picks_from_panel` still says what belongs to a panel, for the walkway and for a
/// press on a row that must not be read as a press outside it.
pub fn closes_panel_on_pick(action: ToolbarAction) -> bool {
    picks_from_panel(action) && !matches!(action, ToolbarAction::SizeStep(_))
}
```

`src/render.rs`, `toolbar_hit`: ask the rail first.

```rust
        let inside = |hit: &&(Rect, ToolbarAction)| {
            column_x >= hit.0.x
                && column_x < hit.0.x + hit.0.w
                && column_y >= hit.0.y
                && column_y < hit.0.y + hit.0.h
        };
        // The rail's own blocks answer first: a list's walkway is the way into the list, not a claim on
        // the rail beside it, so a block level with a list keeps its own press.
        if let Some((_, action)) = pill.hits.iter().filter(|(hit, _)| hit.x == 0.0).find(inside) {
            return Some(*action);
        }
        // The corridor answers before the panel's own rows: it is the way from the block into the list.
        if let Some((corridor, owner)) = pill.corridor
            && column_x >= corridor.x
            && column_x < corridor.x + corridor.w
            && column_y >= corridor.y
            && column_y < corridor.y + corridor.h
        {
            return Some(owner);
        }
        pill.hits
            .iter()
            .filter(|(hit, _)| hit.x != 0.0)
            .find(inside)
            .map(|(_, action)| *action)
```

`src/input.rs`, in `apply_toolbar_action`'s tail:

```rust
        if closes_panel_on_pick(action) {
            self.close_panels();
        }
```

and in the `use crate::render::{…}` import add `closes_panel_on_pick`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib` and `cargo test --test render`
Expected: PASS — the two new unit tests, the hit-order test, and every existing panel, drawer and rail test.

- [ ] **Step 5: Say it in the README**

In the lists paragraph (the sentence that ends "it closes at once"), add:

> The width list stays once it is open: a step does not put it away, and only `Esc`, a press elsewhere or
> a mode switch do. The drawer is the one list that leaves with the pointer.

In the drawer paragraph, replace the clause that says the strip of the rail beside the panel belongs to the
block that opened it "from the panel's first row to its last" with a clause that says the strip **between
the rail's blocks and the panel** belongs to it, so leaving the block rightward is still the way in, and a
rail block level with a row keeps its own press.

- [ ] **Step 6: Run the whole suite and commit**

Run: `cargo fmt --check && cargo clippy --all-targets && cargo test --no-fail-fast`
Expected: `fmt` clean, 0 warnings, every test passes except `the_preedit_renders_at_the_cursor`, which fails
identically before this plan.

```bash
git add src/render.rs src/input.rs README.md tests/render.rs
git commit -m "Keep the width list open, and let the rail keep its own blocks"
```
