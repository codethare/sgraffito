# Delete a board from the boards panel — design

## Intent

Boards accumulate and today a board can be created and picked but not removed; the README says so
under Known limitations. A user tidying up needs to throw away a board and everything drawn on it,
from the place that lists them.

## Goals

- The open `boards` panel offers one more row, at its foot: `delete <name>`, where `<name>` is the
  board this output is showing.
- One click deletes that board with its strokes, text boxes, shapes and notes, and the panel closes
  the way it does after any pick.
- Every screen that was showing the deleted board moves to a board that still exists; screens on
  other boards do not move.
- The deletion is saved, like creating a board.
- The README and its Known limitations say what the row does.

## Non-goals

- Renaming a board.
- Deleting a board that is not the one this screen shows (pick it first instead).
- Per-row delete buttons (`×` on each board row).
- Undo, a confirmation dialog, or a control-socket command.
- Raising the panel's eight-row cap; a board past it is still reached with `sgraffito board <name>`
  and then deleted from the panel.

## Anti-goals

- A document with zero boards.
- A row that deletes a board its label does not name.
- An output or `active` left naming a board that no longer exists.

## Constraints

- A panel row carries one action and one hit box (`Block`), and its label is what it says at rest.
- `Doc.boards` is a `BTreeMap`, so "the first remaining board" is deterministic.
- The app has no undo; `Delete` on a selection is equally final.
- A board being edited or drawn on owns transient state (text edit, stroke, shape, note, selection)
  that must not survive the board.

## Approaches considered

- **A — a `delete <name>` row at the foot of the panel (chosen).** Smallest change: one action, one
  row, one document method. Final like `Delete`, mitigated by the row naming its board and by the
  row not existing when only one board is left.
- **B — the same row, clicked twice (`delete Board 3?` first).** Guards the one hazard that matters
  (no undo) but adds a pending-confirmation state to the toolbar, its cache key, and a panel row
  that must not close the panel — a new interaction pattern in this app.
- **C — a `×` on every row.** Deletes any listed board, but gives each row a second hit target and
  its own layout and hover rules, and the eight-row cap still hides the rest.

## How it works

- `canvas::ToolbarAction::DeleteBoard` joins the actions. Nothing else in the toolbar changes:
  `board_rows` appends, after `new board`, one row with the `Icon::Smaller` mark and the label
  `delete <name>` for the current row of `toolbar.boards`, when there is more than one board. With a
  single board the row is absent, not inert.
- `picks_from_panel` includes it, so the click closes the panel and `panel_stays_open` accepts the
  row as part of it.
- `Doc::delete_board(&mut self, name) -> Option<String>` holds the rule: `None` when `name` is not
  the document's, or is its only board; otherwise it removes the board, moves [`Doc::active`] to the
  first remaining name when it named the deleted board, and returns that name. No side effects on
  outputs, so it is testable beside the other `Doc` tests.
- `App::delete_board(key)` resolves the board from the acting output, calls `Doc::delete_board`, then
  for every output whose board was removed calls the existing `switch_board(Some(key), fallback)`.
  That is what ends a text edit, commits or abandons a gesture, clears the selection and drops the
  raster; in shared mode the first call moves every output and the rest are no-ops. A stroke or shape
  still in flight is dropped before the removal, because committing it looks its board up with
  `entry(...).or_default()` and would create the deleted board again. The store is marked dirty, the
  event is logged, and the toolbar is refreshed.
- `Icon::Smaller` (the existing `−`) stands for "remove"; a new bin icon is not worth the drawing
  code. The row's label is visible at rest in a panel, so it names the board without a hover.

## What is left to the plan

- Exact test names and placement: `Doc::delete_board` beside the other board tests in
  `tests/store.rs`, the row's presence, order, label and hit in `tests/render.rs`, and whether
  `scripts/smoke.sh` should click the row and check the file.
- Whether any existing test that asserts the panel's rows (`the_boards_panel_states_the_documents_boards`
  and neighbours) needs updating beyond the new row.
- The README wording for the boards paragraph, the `C` key-table row, and the Known limitations line.
