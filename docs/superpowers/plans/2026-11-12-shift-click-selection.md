# Shift-click adds to a selection — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A press with `Shift` held toggles the element under the pointer in the selection — added when absent, removed when held — without starting a drag.

**Architecture:** `canvas::selection_press` decides every press with the selection tool, in one pure function a test can reach; `Shift` joins its inputs and the decision becomes `Press::Toggle(id)` or, on nothing, `Press::Inert`. The toggle itself is a pure set operation beside it. `App::select_press` carries the two new decisions out and changes no gesture.

**Tech Stack:** Rust 2024, no new dependencies.

**Spec:** `docs/superpowers/specs/2026-11-12-shift-click-selection-design.md`

## Global Constraints

- Gate: `cargo fmt --check`, `cargo clippy --all-targets` (0 warnings), `cargo test`.
- A `Shift` press writes no document change and starts no gesture.
- A handle answers before `Shift` does, so `Shift` + a handle is still a proportional resize.
- `Esc` is unchanged: it already clears the selection.
- English in code comments, README, commit messages.
- Commit style: imperative sentence, no prefix.

## Review Focus

The five failure modes the spec implies that no step's test can exercise, most likely first.

1. **A `Shift` press that begins a drag.** `Toggle` and `Inert` must leave `self.gesture` untouched (not merely `None`), or a `Shift` drag would move the element it just toggled. Task 1's `select_press` arms are read by the reviewer; the pure tests cannot see the app's gesture.
2. **A `Shift` press on a handle must still resize.** `selection_press` tests the handles before it looks at `Shift`; the plan's test pins the handle case, and the reviewer checks the order in the function rather than only the result.
3. **Toggling out the last element must leave no empty selection object.** `set_selection` drops a selection with no items; the toggle test covers the list, and the reviewer checks the call passes `None` for the transform.
4. **The toggle keeps the other elements' order and does not duplicate.** Repeatedly toggling one element in and out must return the list it started from. Pinned by `toggling_an_element_in_and_out_returns_the_list`.
5. **`Shift` on empty canvas starts no marquee.** A marquee on a `Shift` press would replace the selection on release, against the spec's non-goal; the plan's test pins `Inert` for empty space.

---

### Task 1: The decision and the toggle

**Files:**
- Modify: `src/canvas.rs` (`Press`, `selection_press`, a new `toggle_selection`)
- Modify: `src/app.rs` (`select_press` call and match)
- Test: `tests/canvas_hit.rs`

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `canvas::selection_press(ann: &Board, held: &[ItemId], view: View, screen: [f32; 2], tolerance: f32, shift: bool) -> Press`; `Press::Toggle(ItemId)` and `Press::Inert`; `canvas::toggle_selection(held: &[ItemId], id: ItemId) -> Vec<ItemId>`.

- [ ] **Step 1: Write the failing tests**

In `tests/canvas_hit.rs`, add `toggle_selection` to the `sgraffito::canvas` import and append:

```rust
/// A shift press toggles membership and never replaces the selection, and it keeps the order the
/// other elements were in.
#[test]
fn toggling_an_element_in_and_out_returns_the_list() {
    let held = vec![ItemId::Stroke(0), ItemId::Text(1)];
    let added = toggle_selection(&held, ItemId::Stroke(2));
    assert_eq!(
        added,
        vec![ItemId::Stroke(0), ItemId::Text(1), ItemId::Stroke(2)]
    );
    assert_eq!(toggle_selection(&added, ItemId::Stroke(2)), held);
    assert_eq!(toggle_selection(&held, ItemId::Stroke(0)), vec![ItemId::Text(1)]);
    assert_eq!(toggle_selection(&[ItemId::Text(1)], ItemId::Text(1)), vec![]);
}

/// With `Shift` held, an element under the pointer is a toggle, empty space is nothing, and a handle is
/// still a handle — the order the press is decided in is what keeps a proportional resize reachable.
#[test]
fn a_shift_press_toggles_and_a_handle_still_wins() {
    let a = ann(vec![stroke_at(0.0, 0.0), stroke_at(400.0, 300.0)], vec![]);
    let held = vec![ItemId::Stroke(0)];
    let view = View::default();
    let on_held = view.to_screen([0.0, 0.0]);
    let on_other = view.to_screen([400.0, 300.0]);
    assert_eq!(
        selection_press(&a, &held, view, on_other, ERASER_THRESHOLD, true),
        Press::Toggle(ItemId::Stroke(1))
    );
    assert_eq!(
        selection_press(&a, &held, view, on_held, ERASER_THRESHOLD, true),
        Press::Toggle(ItemId::Stroke(0)),
        "a held element toggles out instead of dragging the selection"
    );
    assert_eq!(
        selection_press(&a, &held, view, [1200.0, 900.0], ERASER_THRESHOLD, true),
        Press::Inert,
        "a shift press on empty space started something"
    );
    let frame = selection_frame(&a, &held, None).unwrap();
    assert_eq!(
        selection_press(
            &a,
            &held,
            view,
            view.to_screen(Handle::Nw.point(frame)),
            ERASER_THRESHOLD,
            true
        ),
        Press::Handle(Grab::Handle(Handle::Nw)),
        "shift took the handle's press"
    );
}
```

Every existing `selection_press(…)` call in that file gains a final `false`. The helpers `ann` and
`stroke_at` are the file's own; a canvas point becomes a screen point through `view.to_screen(...)`, as
the existing press test already does.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test canvas_hit`
Expected: FAIL to compile — `no function or associated item named 'toggle_selection'`, `no variant named
'Toggle'`, and `this function takes 5 arguments but 6 were supplied`.

- [ ] **Step 3: Implement the decision and the toggle**

In `src/canvas.rs`, in `enum Press`:

```rust
    /// Add the element under a `Shift` press to the selection, or remove it when it is held.
    Toggle(ItemId),
    /// A `Shift` press that grabbed nothing: it changes nothing and starts nothing.
    Inert,
```

Change `selection_press` to take `shift: bool` as its last parameter, and after the handle test:

```rust
    // A `Shift` press toggles the element under the pointer and starts nothing else; on empty space it
    // does nothing at all. The handle test above still comes first, so `Shift` + a handle resizes.
    if shift {
        return match ann.hit(canvas[0], canvas[1], tolerance) {
            Some(id) => Press::Toggle(id),
            None => Press::Inert,
        };
    }
```

In `src/canvas.rs`, beside `selection_press`:

```rust
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
```

In `src/app.rs`, in `select_press`, pass `self.shift` as the last argument to `selection_press`, and add
the two arms to the `match press`:

```rust
            // A `Shift` press is a click, not a drag: the selection changes on the press and no gesture
            // is started, so a `Shift` drag cannot move what it just toggled.
            Press::Toggle(id) => {
                let held = self
                    .outputs
                    .get(&key)
                    .and_then(|out| out.overlay.selection.as_ref())
                    .map(|s| s.items.clone())
                    .unwrap_or_default();
                self.set_selection(key, toggle_selection(&held, id), None);
            }
            Press::Inert => {}
```

Add `toggle_selection` to the `use crate::canvas::{…}` import in `src/app.rs`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --test canvas_hit`
Expected: PASS — the two new tests and the existing selection-press tests, with `false` added to
their calls.

- [ ] **Step 5: Update the README**

In the selection paragraph, after "A press on an element takes it;", add:

> with `Shift` held it toggles instead — the element joins the selection when it was not held and
> leaves it when it was, and the press starts no drag;

Replace the key-table row

> | hold `Shift` while dragging | constrain the drag: … |

with that row followed by a new one:

> | hold `Shift` and click an element | add it to the selection, or take it out when it is already held |

In Known limitations, replace "there is no shift-click to add one element to a selection — a marquee is
how several are taken at once" with:

> a shift-click adds or removes one element, so a multi-selection is built either by a marquee in one
> sweep or one element at a time.

- [ ] **Step 6: Run the whole suite and commit**

Run: `cargo fmt --check && cargo clippy --all-targets && cargo test --no-fail-fast`
Expected: `fmt` clean, 0 warnings, every test passes except `the_preedit_renders_at_the_cursor`, which
fails identically before this change.

```bash
git add src/canvas.rs src/app.rs tests/canvas_hit.rs README.md
git commit -m "Let a shift press toggle an element in the selection"
```
