# Board links, a clipboard, and moving a selection to another board

Three things at once, because they share one new piece: a way to pick a board from the rail — a
target for a link, a destination for a move, and the board a paste lands on is already the one the
screen shows.

## What exists today

- A text box is `TextItem { x, y, color, size, text }`: no link. A click with the text tool edits it,
  a click with the selection tool takes it.
- The store writes version 4 (boards keyed by name). `load` backs up and starts empty on a version
  newer than the build knows, so a field added without a version bump would be silently dropped by an
  older build's next save.
- `Ctrl+V` pastes the compositor's text into a text box or a board's name, through `edit_target`.
- Nothing is drawn on the canvas but the document and its overlay: the rail is the only interface.
- The selection tool holds `Selection { items, transform }`; nothing can copy it or move it between
  boards.
- The rail's boards panel lists the boards and switches the screen on a press.

## Goals

1. **A text box can link to a board.** The link is part of the text (`TextItem.link`, the target
   board's name), drawn as a hairline underline in the accent colour so a link is visible.
2. **`Ctrl`+click follows a link.** On a linked text the output switches to that board and the view
   centres on the text. A plain click keeps doing what the tool does — selecting or editing it.
3. **The drawer's `link` row links the selected text.** With exactly one text box held, a press opens
   the board list as a *target picker*; picking a board sets that text's link. With nothing held, more
   than one thing held, or a held thing that is not a text box, the row does nothing.
4. **A selection can be copied and pasted.** `Ctrl+C` copies the held elements into one in-app
   clipboard; `Ctrl+V` pastes a copy onto the board the screen shows, a little offset, and hands the
   selection to the copy. The clipboard crosses boards and outlives the selection, is capped, and is
   session state only. Inside a text box or a name, both keys keep their present meaning.
5. **A selection can be moved to another board.** While something is held, a button sits at the
   bottom centre of the canvas; a press opens the same picker; picking a board moves the held
   elements there and lets go of them.
6. **One picker, one list.** The picker is the rail's board list with a pending target: the same rows,
   the same corridor, the same keys — a row press applies the target instead of switching the screen.

## Anti-goals

- A second floating list widget at the button, or a floating board menu.
- A multi-slot clipboard, a clipboard history, or a clipboard that outlives the daemon.
- Links to URLs, files or anything but a board.
- Underline, colour or thickness options for a link.
- Moving or copying between two daemons.

## Constraints

- **The file version becomes 5.** A link is document data, and the store's own rule is that a field an
  older build does not know is a version bump: otherwise that build drops every link on its next
  save. An older build reading version 5 backs the file up and starts empty — the store's existing
  policy, and the cost of the feature.
- `link` is optional and absent by default, so a version 4 file loads unchanged and gains the field
  when it is written back.
- The bottom-centre button is the second piece of canvas interface: it is drawn from the overlay,
  damages only its own box, exists only while a selection is held, and must not take a press meant
  for the drawing.
- One panel at a time: the picker is a panel, so opening it closes the drawer and the width list, and
  `Esc` or a press outside closes it.
- No new dependency; the cap is measured with `serde_json`, which is already here.

## How it works

- **The link** is `TextItem.link: Option<String>` with `#[serde(default)]`. A linked text draws a
  hairline underline beneath its measured line, in the accent colour.
- **Following** is decided before the tool sees the press: `Ctrl`+press on a linked text switches that
  output's board through `switch_board`, then centres the view on the text's centre —
  `view.offset = centre_of_the_output - text_centre * zoom`, a whole-surface frame because the view
  moved. A link whose board is gone does nothing, and the press falls through to the tool.
- **The picker** is `App.target: Option<Target>` (`Link` or `Move`) plus the boards panel being open.
  With no target the rows switch the screen, as now; with one they apply it and close. `Esc`, a press
  outside, a mode switch and opening another panel all cancel the pending target with the panel.
- **The `link` row** lives in the drawer, closed by default like every rarely used control. Its press
  is refused unless the selection holds exactly one text box; the refusal is a log line, because the
  rail has nowhere to say why.
- **Moving** is `Doc::move_items(from, to, items) -> Option<(Rect, Rect)>`: take the elements out of
  the source board, append them to the target, and report the box each board lost and gained. Two
  boards changed, so the frame that follows is a whole-surface one (`doc_changed`), and the selection
  is dropped — its indices name the source board.
- **The clipboard** is one slot holding *copies*, not indices: `Clip { items: Vec<ClipItem> }` where
  `ClipItem` is a stroke, a text, a shape or a note. Paste appends the copies to the board the screen
  shows, offset by a small step so a paste is visible on top of what it copied, and hands the new
  indices to the selection.
- **The cap** is 200 elements and 1 MB of `serde_json` size, whichever comes first; a copy over it is
  refused whole and logged. One clip of a whole board is the case this bounds — the memory, not the
  file.
- **The keys** route on `edit_target().is_none() && selection.is_some()`: then `Ctrl+C`/`Ctrl+V` are
  the in-app clipboard; otherwise they are the compositor's, as today.
- **The button** is drawn whenever a selection is held, at the bottom centre: a capsule with the text
  `move to board`. Its hit box is its own rect, tested before the tool sees the press, and the frame
  damages the union of its old and new box.
- **Deleting a board** drops every link that names it, in the same pass that removes the board.

## Alternatives

- **A — a floating list anchored at the button (rejected).** A second widget family with its own
  damage, hits and dismissal rules, for a list the rail already draws.
- **B — move to the "next" board (rejected).** Predictable only by accident.
- **C — the clipboard as ids into the source board (rejected).** The source can be deleted, or the
  paste can land on a board whose indices mean something else.
- **D — links as a separate list beside the text (rejected).** A link belongs to the text it is on.

## Tests first

- `a_version_4_file_loads_and_a_link_survives_the_round_trip`.
- `going_to_a_link_switches_the_board_and_centres_the_text` — the view arithmetic, pure.
- `ctrl_click_follows_a_link_and_a_plain_click_does_not` — the hit that decides it.
- `a_link_to_a_deleted_board_is_dropped_with_the_board`.
- `the_link_row_needs_exactly_one_selected_text_box`.
- `a_press_in_the_target_list_does_not_switch_the_screen` — the picker's mode.
- `moving_elements_leaves_the_source_board_without_them`.
- `a_copy_over_the_cap_is_refused_whole`.
- `pasting_appends_a_copy_and_hands_it_to_the_selection`.
- `the_move_button_is_hit_before_the_canvas` — its box against the tool's press.

## Review focus

- The version bump and what an older build does with the file — the one irreversible edge here.
- The button's box against the canvas: a press on it must not also start a stroke, and its damage
  must cover both where it was and where it is.
- `Ctrl+C`/`Ctrl+V` inside a text box or a name, and while no selection is held.
- A link whose board is gone; a link followed on one screen in shared mode versus per-screen mode.
- The picker's dismissal: `Esc`, press outside, another panel, a mode switch.
- The clipboard's cap arithmetic, and pasting the same clip twice.
