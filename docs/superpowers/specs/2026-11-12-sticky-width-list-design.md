# A width list that stays, and a rail that keeps its own blocks — design

## Intent

Two things about the width list are wrong in use. It leaves the moment the pointer moves off it, and
stepping the width closes it, so changing a size twice means opening it twice. And the list's walkway
claims the rail: while it is open, the two rows' band reaches from the rail's left edge across the
boards block, so hovering or pressing Boards answers for the width readout instead.

## Goals

1. **The width list is a button, and it stays once open.** A click on the readout toggles it. Like
   the boards list, no hover state closes it. It is dismissed
   by `Esc`, by a press anywhere outside it (the canvas, another rail block, another list), and by a
   mode switch. Only the drawer keeps its leave-close: it is the panel the pointer opens by walking
   into it.
2. **A step is a command, not a pick.** Pressing `smaller width` or `larger width` applies its step and
   leaves the list open, so a run of three steps is three presses. `[` and `]` keep working and never
   closed it.
3. **The width list's walkway does not cover the rail's blocks.** Its walkway starts where the rail's
   blocks end, so the boards block just below the readout stays itself while the list is open. The
   walkway is still what lies between the rail's blocks and the panel's column, so the pointer can
   still travel from a block into a row. The drawer and the boards list keep their strip: a block
   level with one of their rows answers for the list, which is how the pointer walks into a tall list
   from any height, and two tests pin that.

## Non-goals

- Making the drawer sticky: it stays the one menu that leaves with the pointer.
- A click outside the rail that only closes the list: a press on the canvas is a press outside, and it
  both closes the list and does what a press there does.

## Anti-goals

- A walkway that shadows a rail block for the width list.
- A step press that closes the list it came from.

## Constraints

- `panel_stays_open` is the one place a panel's dismissal is decided; the width list's owner becomes
  the second unconditional one.
- `picks_from_panel` still says what belongs to a panel (used by the walkway and by the press-outside
  rule); what *closes* on a pick is a separate decision, and a width step is the exception.
- `toolbar_hit`'s order is untouched: the fix is the width list's corridor starting at
  `TOOLBAR_BLOCK` instead of the rail's edge, so the drawer's and the boards list's strips are the
  same rect they were.

## Approaches considered

- **A — a narrower walkway for the width list (chosen).** One width in the corridor's construction;
  the walk into a row still works, and the two lists whose strip is documented and tested keep it.
- **B — the rail's hits answering before every walkway.** It breaks the drawer's and boards list's
  pinned strip: a block level with a row would stop answering for the list.
- **C — shrink the width list to one row.** It would fit between the readout and Boards, but two
  steps need two rows.

## How it works

- `panel_stays_open` returns true for the `Size` owner as well as `Boards`; the drawer's owner is the
  only one left with the hover rule.
- `ToolbarAction::SizeStep` keeps belonging to its panel (`picks_from_panel`) but stops closing it:
  the app's closing tail asks a new `closes_panel_on_pick(action)` — true for a pick that is not a
  width step.
- `toolbar_hit` looks for a rail block's own hit (`x == 0`) before the corridor, then the corridor,
  then the panel's rows.
- The README's width-list and drawer paragraphs say which list leaves with the pointer, that a step
  keeps the list, and that a rail block level with the width list keeps its own press. A hovered
  boards capsule can reach past the gap and be drawn under the width list's second row: accepted, a
  cosmetic sliver.

## What is left to the plan

- Test names and placement: the sticky owner, the closing predicate, and the hit order (a width list
  open over the boards block).
- Whether the drawer's changed strip behaviour needs a line of its own in the README.
