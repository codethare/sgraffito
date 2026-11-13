# Stepping the width of a selected drawing — design

## Intent

The size keys only ever arm the brush. A colour chip already recolours what the selection holds,
but the same drawing cannot be made heavier or lighter without erasing and redrawing it. Extend
the colour chip's idea to size: the keys that step the width should step the selected ink.

## Goals

- With a selection held, `[` / `]` — and the rail's `−` / `+` size blocks, which go through the
  same entry point — step every selected stroke's and shape's `width` by the pen step (1) inside
  1–16, and every selected text box's `size` by the text step (2) inside 8–72, as **one document
  change per press**.
- The armed brush is stepped as it is today, so the next stroke is drawn at the new width.
- The selection stays held, the frame follows the new ink, and the change is saved like any other.
- A note keeps its own typography: the size keys do not change a note, which is the rule the
  README already states.

## Non-goals

- Scaling the geometry. That is what the selection's resize handles do, and a scale leaves the pen
  width alone.
- A note's type size, or a readout of what a selected item's own width is. The rail shows the armed
  brush and nothing else.
- Undo.

## Anti-goals

- A press with nothing to step — every held item already at its bound — writes nothing, saves
  nothing and damages nothing.
- Nothing moves. Only widths and text sizes change.
- The annotation file format is unchanged.

## Constraints

- A selection can mix kinds, so each kind steps by its own step and its own clamp.
- A selection belongs to one board per output and several outputs may each hold one; the colour
  chip already walks them all, and this walks the same list.
- The damage box is the union of the ink before and after, because a thicker line reaches further
  and a bigger text box is taller as well as wider.
- The readout on the rail is the armed brush, which with the selection tool armed is the pen
  width; a text box's size is not on the rail, so a press that grows a selected text box shows its
  own effect on the canvas rather than in the readout.

## Approaches considered

- **A — change the widths and text sizes in place (chosen).** The request names the width control,
  the colour chip is the exact precedent (a rail control that also changes what is held), and the
  change is small: one document method and one call from `nudge_size`.
- **B — scale the geometry about the selection frame's centre.** The drawing gets bigger, but the
  pen width never changes, so its lines get relatively thinner; the resize handles already do it.
- **C — both, width by default and scale with a modifier.** Two meanings on one pair of keys, and
  a second gesture the handles already provide.

## How it works

- `Board::step_size(&mut self, items: &[ItemId], dw: f32, dtext: f32) -> Option<Rect>` walks the
  held items: a stroke's or shape's `width` becomes `stepped_size(width, dw.signum(), PEN_WIDTH_STEP,
  PEN_WIDTH_MIN, PEN_WIDTH_MAX)`, a text box's `size` the same with the text constants, and a note
  is left alone. It returns the union of `paint_bounds()` before and after for every item that
  changed, or `None` when none did.
- `App::nudge_size(dir)` keeps stepping the armed brush, then walks every output that holds a
  selection the way `recolour_selection` does, calls the board's `step_size` with the pen step and
  the text step, and reports the box through `doc_changed_at`.
- The `[`/`]` keys, the rail's size blocks and a held `]` all reach `nudge_size`, so they inherit it.

## What is left to the plan

- Test names and placement for the board rule, and whether the app glue gets a seam (it needs a
  compositor, like the deletion's).
- The README wording: the `[`/`]` key-table row, and the paragraph that says the brush "never
  changes what is already drawn", which becomes false the way the colour chip already made it false.
