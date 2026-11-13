# Adding to a selection with Shift-click — design

## Intent

A selection is made in one gesture and then replaced by the next: there is no way to put one more
element into it, and the README says so. A press with `Shift` held should add the element under the
pointer to what is held, or take it out when it is already there.

## Goals

- `Shift` + a press on an element toggles it: added when the selection does not hold it, removed
  when it does. The selection is set on the press, and no drag starts from it.
- Removing the last element leaves no selection at all — the frame goes with it.
- `Shift` + a press anywhere else — empty canvas, or inside the frame but not on an element —
  changes nothing and starts nothing.
- A handle keeps its precedence: a press on a handle with `Shift` held still begins a resize, and
  `Shift` read during the drag still keeps its proportions. That is why the press path tests the
  handles before it looks at `Shift`.

## Non-goals

- `Shift` + a marquee adding to the selection instead of replacing it.
- Add without remove (an unselected element joins and a selected one ignores the press).
- `Esc`: it already clears the selection, as the fifth of its stages. Nothing changes there.

## Anti-goals

- A `Shift` press writes no document change by itself: a selection is transient, and only a drag
  that moves things reaches the document.
- A `Shift` press starts no gesture, so a `Shift` drag over an element cannot move it.
- No empty selection object is left behind when the last element is toggled out.

## Constraints

- `canvas::selection_press` is the one place the press is decided, in screen and canvas coordinates
  together, and it is pure so a test can reach it. `Shift` joins its inputs.
- The toggle itself is a set operation on `Vec<ItemId>` and belongs beside it, also pure.
- `App::set_selection(key, items, None)` already drops the selection when `items` is empty.
- The document is untouched: nothing about the file changes.

## Approaches considered

- **A — toggle one element on a `Shift` press (chosen).** The standard editor gesture, and the only
  way to drop one element from a multi-selection short of `Esc`, which clears all of them.
- **B — add only.** Simpler, but a multi-selection can then never lose one element without losing
  every one of them.
- **C — A plus a `Shift` marquee that adds.** More reaching, but it needs a second meaning on the
  marquee's release and doubles the state the release path carries.

## How it works

- `canvas::selection_press(ann, held, view, screen, tolerance, shift)` gains `shift`. With `Shift`
  held: a handle still answers first; an element under the pointer becomes `Press::Toggle(id)`;
  nothing under the pointer becomes `Press::Inert`.
- `canvas::toggle_selection(held: &[ItemId], id: ItemId) -> Vec<ItemId>` returns the held list with
  `id` appended when absent and removed when present, keeping the order the others were in.
- `App::select_press` carries them out: `Toggle` sets the selection to the toggled list with no
  gesture; `Inert` does nothing at all.
- The README's selection paragraph, its key table and the "no shift-click" limitation say what the
  gesture does.

## What is left to the plan

- Test names and placement for `selection_press` and `toggle_selection` (beside the existing
  selection-press tests), and the README wording.
