# Two actions on a board's row: delete it, rename it

The boards list shows one row per board. Acting on a board means acting on the one this screen
happens to show: the single `delete <name>` row at the foot of the list names it. This change gives
every row its own two actions — delete and rename — revealed while the pointer is on that row, and
adds the rename the document never had.

## What exists today

- The list's rows are `board_rows`: one row per board (its name, the current one accent-filled, no
  key — the `boards` block's `C` is the key), then `delete <current board>` when the document has
  more than one board, then `new board`.
- `Doc.boards` is a map keyed by the board's name, `Doc.active` is a name, and every
  output holds the name of the board it shows (`Output.board: String`). The annotation file keys a
  board's drawing by that name too, so a rename moves the map key, `active`, and every output's name.
- **There is no rename in the document, the commands or the panel.** The README says so twice.
- A panel row is a `Block` with `column: 1`, one `Control` for drawing and one hit in
  `ToolbarPill.hits`. Its box is a constant width — `TOOLBAR_RESERVE` less the rail — not measured
  from its label.
- The pointer's position on the rail is `Toolbar.hover`; the rows already stretch into a capsule
  under it, and the panel's corridor carries the pointer from the rail into the rows.
- Text boxes and notes are edited by an editor that owns a `TextBuffer` per output, with IME
  (preedit, commit), `Enter`, `Esc`, a caret and a commit on a press elsewhere.

## Goals

1. **Every board row owns delete and rename.** With the pointer on a board's row, the row's right
   end shows two buttons: `✕` deletes that board, `✎` renames it. With the pointer elsewhere the
   row shows its name only, clipped to the room left.
2. **Delete acts on the row's board.** The single `delete <current board>` row goes. A press on `✕`
   removes the board the row shows, with everything drawn on it, and moves every screen showing it to
   the first board left — what the deleted row did, for whichever board the row names. The button is
   absent on a document's last board, which cannot be deleted.
3. **Rename edits the row in place.** A press on `✎` turns the row's name into an editor: a buffer
   with a caret, typed into with the keyboard and the input method, `Enter` commits, `Esc` cancels,
   and a press anywhere else commits. The list stays open throughout, because the row being edited
   is in it.
4. **The rename reaches everything that names the board.** Committing moves the board's entry in
   the document, the picked board when it is the one renamed, and every output showing it.

## Anti-goals

- A confirmation before a delete.
- An undo.
- A key on either button: the boards list's key is `C`, and a row's key would repeat the block's.
- A command-line rename, a rename dialog or a temporary text box used as a name field.
- A wider panel: the two buttons fit inside the row's own box.

## Constraints

- The annotation file's version stays 4: a rename moves a key, it does not change the format.
- One panel at a time, and the panel's box, the corridor and the entry rules stay as they are.
- The list keeps its `MAX_BOARD_ROWS` cap, buttons included.
- The boards list's rows keep their order and their `Board(index)` action: an index is the row's
  identity at the moment it is pressed, as it already is for a pick.

## How it works

- **The reveal** is `hover` on the row or on one of its two buttons. The buttons are hits of their
  own (`DeleteBoard(index)`, `RenameBoard(index)`) inside the row's box, and they answer before the
  row's own hit, so a press on one is the button and not the row. Keeping them visible while the
  pointer is on them is what stops the row from flickering as the pointer moves from the name onto a
  button; the row's capsule uses the same predicate.
- **Delete** applies `Doc::delete_board` to the row's board, then closes the list — a pick, as
  the old row was. The last board has no `✕`.
- **Rename** is the one press that does not close its panel: the row becomes the editor. `Esc`
  cancels the name and leaves the list open; a press outside the row commits and then applies as it
  would have. A committed name is trimmed; an empty name or one another board holds leaves the board
  as it was (edited text is dropped, no error is shown — the panel has nowhere to show one).
- **`Doc::rename_board(old, new) -> bool`** moves `boards[old]` to `new` and rewrites `active`
  when it matches, returning whether anything changed; the app rewrites every output whose board it
  was, saves, and refreshes the rail's list.
- **The editor** is app-level state next to the toolbar's (`rename: Option<Rename>`, holding the old
  name and a `TextBuffer`), because the toolbar is not per output. Its keys are taken before the
  canvas's; its IME branch sits beside the text box's, in the same handlers. The row draws the
  buffer and the caret where its name was, and the preedit after the caret.
- **Drawing** — the row keeps one `Control`, its label swapped for the buffer while it is edited;
  the two buttons are painted over the row's right end, in the row's colour, and the name is drawn
  clipped to `width - 2 * BUTTON` while they are up.

## Alternatives

- **A — widen the panel by two button columns (rejected).** Every row pays the width for two
  buttons only one row can show, and the panel's box is a constant the hits, the corridor and the
  damage are placed from.
- **B — reuse the text box editor by placing a text box on the row (rejected).** The editor commits
  into a board's text list, so the name would become a drawing.
- **C — the buttons live in the drawer (rejected).** They act on a board the drawer does not list,
  and the drawer is one press further away.
- **D — rename only from the command line (rejected).** The list is where a board's name is read,
  and the user asked for the button there.

## Tests first

- `every_board_row_reveals_its_two_buttons_only_under_the_pointer` — no button hit without hover;
  two with `Board(0)`; one (rename) with one board.
- `a_board_rows_buttons_stay_up_while_the_pointer_is_on_them` — hovering `DeleteBoard(0)` or
  `RenameBoard(0)` keeps both buttons up.
- `the_buttons_answer_before_the_row` — a press on either rect is its action, not the row's.
- `a_delete_press_removes_the_board_the_row_shows` — the row's board, not the current one.
- `a_rename_press_keeps_the_list_open_and_edits_that_row`.
- `renaming_a_board_moves_its_drawing_and_every_screen` — the map entry, `active`, the outputs.
- `an_empty_or_taken_name_leaves_the_board_as_it_was`.
- `enter_commits_and_escape_cancels_a_rename`.
- `a_press_elsewhere_commits_the_rename_before_it_applies`.
- `the_edited_row_draws_the_buffer_instead_of_its_name`.

## Review focus

- The reveal predicate and the hit order around a row's right end: no dead strip, no flicker, and a
  press on the name still picks the board.
- The rename's reach: every place that holds a board name (map key, `active`, each output, the rail's
  board list, the delete row the list no longer has).
- The editor's interaction with IME and with the panel's dismissal rules (`Esc`, press outside, mode
  switch) and with the text box editor that shares the keys.
- The last board's `✕` and the `MAX_BOARD_ROWS` cap with two buttons per row.
