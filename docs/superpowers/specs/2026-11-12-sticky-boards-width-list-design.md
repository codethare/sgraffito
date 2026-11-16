# A sticky boards list, a width list on hover, and an opening transition — design

## Intent

Three complaints about the rail's menus. The `boards` list vanishes the moment the pointer leaves
it, although the README already describes it as closing only on a pick, a press elsewhere or `Esc`.
The width readout is a dead number: it cannot be hovered, says nothing, and the two keys that change
it sit beside it on the rail. And a list appears with no transition where every other control eases
into its open state.

## Goals

1. **`boards` stops closing on leave.** It opens by click or `C`, and closes on a pick, on `Esc`, or
   on a press anywhere outside it — never because the pointer simply moved off it. The drawer keeps
   its leave-close: it is a transient menu, and the user asked for `boards` only.
2. **The width readout becomes a control with its own list.** Hovering it opens a panel of two rows,
   `smaller width ([)` and `larger width (])`, anchored to the readout; the list closes when the
   pointer leaves the readout and its rows; picking a row applies that step and closes the list.
   `[` and `]` still work, and the two blocks leave the rail.
3. **Both lists ease open.** The `boards` panel and the width panel grow into place over the rail's
   existing 140 ms ease-out when they open, and close at once, exactly like a block's pop-out. The
   panel rows themselves never animate on hover: they state their names at rest, and animating them
   was rejected because the pointer moving down a list made the whole list twitch.

## Non-goals

- A hover-triggered `boards` list, or a sticky width list. The two lists have the dismissal their
  trigger deserves: a click-open menu stays, a hover-open popover goes.
- Panel rows that stretch on hover, or a hover highlight on them.
- A key that opens the width list: hovering is the way in, and `[`/`]` step without it.

## Anti-goals

- A hover-opened list that flickers: opening it must not be undone by the motion that opened it, so
  the readout counts as "on the panel" for the stay-open rule while the panel is open.
- A list that can be clicked while invisible: hit boxes stay the settled layout, as they do for the
  rail's pop-out, so a click during the 140 ms lands where the row will be and not on nothing.
- Two lists open at once: the one-panel-at-a-time rule carries over unchanged.

## Constraints

- `open_panel`, `panel_owner` and the `set_*` pair know two panels today; a third one means a third
  case in each, not a second mechanism.
- The panel is anchored to its owner and grows up or down about it; the readout is mid-rail, so its
  panel hangs off the middle rather than the foot.
- The rail's damage box already covers the panel (`toolbar_bounds` takes the larger extent), so an
  animated panel stays inside the box every frame repaints.
- A panel's rows keep their full hit geometry while the panel animates.
- The rail's invariant test says every block's label ends with its key. The readout has no key, so it
  keeps an empty label and the test gains one documented exception: a control with no key states no
  key.

## Approaches considered

- **A — a third panel on the existing machinery (chosen).** The width list is a panel like the
  drawer: an owner block, rows in the panel column, the corridor from the owner into it, and the
  one-panel rule. The hover trigger is the only new thing.
- **B — a bespoke popover for the width steps.** Two rows floating under the readout with no plate
  and no exclusivity: less code, but a second kind of list that can be open beside the drawer, and
  two sets of rules for what closes when.
- **C — keep the two size blocks on the rail and only add a hover list to the readout.** The list
  would duplicate the blocks that are beside it.

## How it works

- `ToolbarAction::Size` is the readout's own action. The readout keeps `Mark::Value` and an empty
  label; `rail_candidates` drops the two `SizeStep` blocks and gives the readout `action:
  Some(ToolbarAction::Size)`.
- The width panel's rows are the two `SizeStep` actions with the labels the rail gave them, so the
  panel machinery, the pick-and-close rule and the key they state are the existing ones.
- `update_toolbar_hover` opens the width panel when the pointer lands on the readout, and the stay-
  open rule for that owner is "on the readout or on one of its rows"; `Boards` is the one owner whose
  rule has no "off it" case.
- `Toolbar` gains the panel's own progress beside the pop-out's, so a list can ease open while a
  capsule is still growing; the layout is settled and only the drawn height is clipped.
- The README's boards paragraph and key table say the list stays; the rail paragraph and the `[`/`]`
  row say the keys live in the readout's list.

## What is left to the plan

- Task boundaries: the boards rule, the width panel, and the opening transition can land separately.
- The exact clip and progress arithmetic for the panel's opening, and whether the panel's *close*
  needs any frame at all (it does not: the rail closes at once and so does this).
