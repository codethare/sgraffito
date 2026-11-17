# Plan: a board's row deletes and renames it

Spec: `docs/superpowers/specs/2026-11-17-board-row-actions-design.md`

## Global constraints

- Annotation file version stays 4. No new dependency. `cargo fmt` clean, `cargo clippy
  --all-targets` 0 warnings, every test green except the pre-existing
  `the_preedit_renders_at_the_cursor`.
- One panel at a time; the panel's box, the corridor and the entry rules stay as they are.
- The list keeps `MAX_BOARD_ROWS`, buttons included. `new board` stays the last row.
- Board rows stay bare (`Mark::None`) and keep their `Board(index)` action.

## Interfaces

- `Doc::rename_board(&mut self, old: &str, new: &str) -> bool` — task 1, consumed by task 3.
- `App::rename_board(&mut self, old: &str, new: &str) -> bool` — task 1, consumed by task 3.
- `ToolbarAction::DeleteBoard(usize)`, `ToolbarAction::RenameBoard(usize)` — task 2, consumed by
  tasks 2, 3, 4.
- `Toolbar.rename: Option<BoardEdit>` (`BoardEdit { index, text, cursor, preedit }`) — task 3,
  consumed by tasks 3 and 4.
- `board_actions(toolbar, index) -> Option<(bool, bool)>` — task 2, consumed by tasks 2 and 4.

---

## Task 1 — a board can be renamed, everywhere it is named

**Files:** `src/canvas.rs`, `src/app.rs`, `README.md` (not yet).

**Step 1.1 — write the failing tests.** In `src/canvas.rs`'s test module:

```rust
    /// A rename moves the drawing with the name: the annotations and the picked board follow it, and
    /// nothing is left under the old key.
    #[test]
    fn renaming_a_board_moves_its_drawing() {
        let mut doc = Doc::default();
        doc.boards.insert("laptop".into(), Board::default());
        doc.boards.insert("monitor".into(), Board::default());
        doc.boards
            .get_mut("laptop")
            .unwrap()
            .notes
            .push(Note::new(0.0, 0.0, "#ffffff", "2026/11/17".into()));
        doc.active = "laptop".into();
        assert!(doc.rename_board("laptop", "left"));
        assert_eq!(doc.active, "left");
        assert_eq!(doc.boards["left"].notes.len(), 1);
        assert!(!doc.boards.contains_key("laptop"));
    }

    /// A name the rail cannot state is refused rather than half-applied: empty, taken, or not a board.
    #[test]
    fn a_rename_refuses_a_name_it_cannot_take() {
        let mut doc = Doc::default();
        doc.boards.insert("laptop".into(), Board::default());
        doc.boards.insert("monitor".into(), Board::default());
        for bad in ["", "monitor", "desk"] {
            assert!(!doc.rename_board("laptop", bad), "{bad:?} was taken");
            assert!(doc.boards.contains_key("laptop"));
        }
        assert_eq!(doc.boards.len(), 2);
    }
```

Run: `cargo test --lib renaming_a_board_moves_its_drawing a_rename_refuse`
**Expected:** compile error — `rename_board` does not exist.

**Step 1.2 — the document half.** In `impl Doc`, beside `delete_board`:

```rust
    /// Rename `old` to `new`, moving its drawing and the picked board with it. `false` when the new
    /// name is empty or another board holds it, or when there is no board called `old`: a refused
    /// rename changes nothing.
    pub fn rename_board(&mut self, old: &str, new: &str) -> bool {
        if old == new || new.is_empty() || self.boards.contains_key(new) {
            return false;
        }
        let Some(board) = self.boards.remove(old) else {
            return false;
        };
        self.boards.insert(new.to_string(), board);
        if self.active == old {
            self.active = new.to_string();
        }
        true
    }
```

Run the two tests. **Expected:** both pass.

**Step 1.3 — the app half.** In `src/app.rs`, beside `delete_board`:

```rust
    /// Rename a board and keep every screen that shows it on it. Unlike a pick, this is document
    /// content, so it goes to the file.
    pub(crate) fn rename_board(&mut self, old: &str, new: &str) -> bool {
        if !self.doc.rename_board(old, new) {
            return false;
        }
        for out in self.outputs.values_mut() {
            if out.board == old {
                out.board = new.to_string();
                out.dirty = true;
            }
        }
        self.dirty = true;
        self.store.mark_dirty(Instant::now());
        self.refresh_toolbar();
        true
    }
```

Run `cargo test --lib rename`. **Expected:** the two tests pass; nothing else breaks.

**Step 1.4 — commit.** `git add -A src && git commit -m "Let a board be renamed, with its drawing and every screen"`.

---

## Task 2 — every board's row owns delete and rename

**Files:** `src/canvas.rs`, `src/render.rs`, `src/input.rs`, `tests/render.rs`.

**Step 2.1 — write the failing tests.** In `src/render.rs`'s test module:

```rust
    /// A board's row shows its two buttons only while the pointer is on the row or on one of them,
    /// and a document's only board has nothing to delete.
    #[test]
    fn every_board_row_reveals_its_buttons_under_the_pointer() {
        let mut toolbar = Toolbar {
            boards_open: true,
            boards: vec![
                BoardRow { name: "laptop".into(), current: true },
                BoardRow { name: "monitor".into(), current: false },
            ],
            ..crate::render::tests::rail_for_legend()
        };
        let hit = |toolbar: &mut Toolbar, x: f32, y: f32| {
            let mut renderer = Renderer::new();
            renderer.toolbar_hit(toolbar, 900.0, 1000.0, x, y)
        };
        let pill = {
            let mut renderer = Renderer::new();
            renderer.toolbar_pill(&toolbar, (900.0, 1000.0))
        };
        let row = pill
            .controls
            .iter()
            .find(|c| c.block.action == Some(ToolbarAction::Board(0)))
            .expect("no row for the first board");
        let (ox, oy) = toolbar_origin(1000.0, pill.column);
        // Away from every row: nothing but the names.
        let away = hit(&mut toolbar, ox + 40.0, oy + row.y + 8.0);
        assert_eq!(away, Some(ToolbarAction::Board(0)));
        toolbar.hover = Some(ToolbarAction::Board(0));
        let pill = {
            let mut renderer = Renderer::new();
            renderer.toolbar_pill(&toolbar, (900.0, 1000.0))
        };
        let row = pill
            .controls
            .iter()
            .find(|c| c.block.action == Some(ToolbarAction::Board(0)))
            .expect("no row for the first board");
        let (ox, oy) = toolbar_origin(1000.0, pill.column);
        // The two buttons sit at the row's right end, past the name.
        assert_eq!(
            hit(&mut toolbar, ox + TOOLBAR_DRAWER_X + row.width - TOOLBAR_BLOCK / 2.0, oy + row.y + 8.0),
            Some(ToolbarAction::DeleteBoard(0))
        );
        assert_eq!(
            hit(&mut toolbar, ox + TOOLBAR_DRAWER_X + row.width - TOOLBAR_BLOCK / 2.0 + TOOLBAR_BLOCK, oy + row.y + 8.0),
            Some(ToolbarAction::RenameBoard(0))
        );
        // The pointer on a button keeps both up: the reveal is the row's, not the name's.
        toolbar.hover = Some(ToolbarAction::DeleteBoard(0));
        assert!(board_actions(&toolbar, 0).is_some());
        toolbar.hover = Some(ToolbarAction::RenameBoard(0));
        assert!(board_actions(&toolbar, 0).is_some());
        toolbar.hover = None;
        assert!(board_actions(&toolbar, 0).is_none());
    }

    /// With one board there is nothing to delete, and the rename is still there.
    #[test]
    fn the_delete_button_is_absent_on_the_only_board() {
        let toolbar = Toolbar {
            boards_open: true,
            boards: vec![BoardRow { name: "laptop".into(), current: true }],
            hover: Some(ToolbarAction::Board(0)),
            ..crate::render::tests::rail_for_legend()
        };
        assert_eq!(board_actions(&toolbar, 0), Some((false, true)));
    }

    /// A pick that closes its panel, and the one press that does not.
    #[test]
    fn a_rename_press_keeps_its_list_and_a_delete_press_closes_it() {
        assert!(closes_panel_on_pick(ToolbarAction::DeleteBoard(0)));
        assert!(!closes_panel_on_pick(ToolbarAction::RenameBoard(0)));
        assert!(picks_from_panel(ToolbarAction::DeleteBoard(0)));
        assert!(picks_from_panel(ToolbarAction::RenameBoard(0)));
    }
```

The press order — the buttons answer before the row they sit on — is what the two points at the
row's right end assert: each lands on a button rect inside the row's own box.

Run: `cargo test --lib board_row`
**Expected:** compile error — `board_actions`, `DeleteBoard(usize)`, `RenameBoard` do not exist.

**Step 2.2 — the actions.** `src/canvas.rs`:

```rust
    /// Delete the board at this index of the panel's list, from that board's own row.
    DeleteBoard(usize),
    /// Rename the board at this index of the panel's list, from that board's own row. The one row
    /// press that leaves its list open: the row it edits is in the list.
    RenameBoard(usize),
```

**Step 2.3 — `src/render.rs`: `key_of`, `picks_from_panel`, `closes_panel_on_pick`, `board_rows`.**

```rust
        ToolbarAction::Boards
        | ToolbarAction::Board(_)
        | ToolbarAction::NewBoard
        | ToolbarAction::DeleteBoard(_)
        | ToolbarAction::RenameBoard(_) => "C",
```

```rust
            ToolbarAction::Board(_)
                | ToolbarAction::NewBoard
                | ToolbarAction::DeleteBoard(_)
                | ToolbarAction::RenameBoard(_)
                | ToolbarAction::SizeStep(_)
```

```rust
/// Whether applying `action` puts its panel away. A width step and a rename are the two presses that
/// work on what the panel holds rather than choosing from it, so they leave it open.
pub fn closes_panel_on_pick(action: ToolbarAction) -> bool {
    picks_from_panel(action)
        && !matches!(
            action,
            ToolbarAction::SizeStep(_) | ToolbarAction::RenameBoard(_)
        )
}
```

`board_rows`: delete the block that pushes `delete {name}`; the doc comment loses the paragraph
about it. The function becomes the board rows then `new board`.

**Step 2.4 — the reveal and the hits.** In `src/render.rs`, above `board_rows`:

```rust
/// Whether a board's row shows its buttons, as `(delete, rename)`: the pointer is on the row or on
/// one of them, the document has a board to delete, and the row is not the one being renamed.
fn board_actions(toolbar: &Toolbar, index: usize) -> Option<(bool, bool)> {
    if toolbar
        .rename
        .as_ref()
        .is_some_and(|edit| edit.index == index)
    {
        return None;
    }
    let on_it = matches!(
        toolbar.hover,
        Some(
            ToolbarAction::Board(i)
                | ToolbarAction::DeleteBoard(i)
                | ToolbarAction::RenameBoard(i)
        ) if i == index
    );
    on_it.then(|| (toolbar.boards.len() > 1, true))
}
```

In `toolbar_pill`'s block loop, after `let width = match label { ... };`, before the hit push:

```rust
            // A board's row makes room for its two buttons while they are up; the name is drawn to
            // the left of them, and the row's own hit stops where they start.
            let buttons = match block.action {
                Some(ToolbarAction::Board(index)) => board_actions(toolbar, index),
                _ => None,
            };
            let button_w = 2.0 * TOOLBAR_BLOCK;
            let width = if buttons.is_some() {
                (width + button_w).min(ceiling)
            } else {
                width
            };
```

The `hits.push` for a row becomes:

```rust
            if let Some(action) = block.action {
                let w = if owner == Some(action) {
                    TOOLBAR_DRAWER_X
                } else {
                    width
                };
                hits.push((
                    Rect {
                        x,
                        y: y[column],
                        w: if buttons.is_some() { w - button_w } else { w },
                        h: TOOLBAR_BLOCK,
                    },
                    action,
                ));
            }
```

and the buttons' hits are pushed immediately after the row's, into a separate list appended once
the row loop is done (so the panel's gap-filling loop only ever sees the rows):

```rust
        // The row buttons answer before the row they sit on: both their rects are inside the row's
        // own box, and a hit is the first rect that contains the point.
        let mut board_hits = Vec::new();
        for control in &controls {
            let Some(ToolbarAction::Board(index)) = control.block.action else {
                continue;
            };
            let Some((delete, rename)) = board_actions(toolbar, index) else {
                continue;
            };
            let mut actions = Vec::new();
            if delete {
                actions.push(ToolbarAction::DeleteBoard(index));
            }
            actions.push(ToolbarAction::RenameBoard(index));
            let right = control.width;
            for (number, action) in actions.iter().enumerate() {
                let slot = actions.len() - number;
                board_hits.push((
                    Rect {
                        x: TOOLBAR_DRAWER_X + right - slot as f32 * TOOLBAR_BLOCK,
                        y: control.y,
                        w: TOOLBAR_BLOCK,
                        h: TOOLBAR_BLOCK,
                    },
                    *action,
                ));
            }
        }
        hits.extend(board_hits);
```

`hits.extend` goes after the loop that fills the gaps between rows and before the `ToolbarPill` is
built: the gap-filling walks the row hits in order, and a button hit in between would shorten its
neighbour to nothing.

(`controls` is complete by then: the loop above pushes to it.)

**Step 2.5 — the press.** `src/input.rs`, `apply_toolbar_action`:

```rust
            ToolbarAction::DeleteBoard(index) => {
                let name = self.doc.names().nth(index).cloned();
                if let Some(name) = name {
                    self.delete_board_named(&name);
                }
            }
            ToolbarAction::RenameBoard(index) => {
                let name = self.doc.names().nth(index).cloned();
                if let Some(name) = name {
                    self.begin_rename(index, &name);
                }
            }
```

`App::delete_board(key)` reads the board off the output; the row's delete must not. Split it:

```rust
    /// Delete the board at this key's output — the command's way, where the board is the one on
    /// screen.
    pub(crate) fn delete_board(&mut self, key: u32) {
        let Some(name) = self.outputs.get(&key).map(|out| out.board().to_string()) else {
            return;
        };
        self.delete_board_named(&name);
    }

    /// Delete a board by name, moving every screen that shows it to the first board left.
    pub(crate) fn delete_board_named(&mut self, name: &str) {
        self.end_text_edit();
        self.commit_transient_drawing();
        let Some(successor) = self.doc.delete_board(name) else {
            return;
        };
        let keys: Vec<u32> = self
            .outputs
            .iter()
            .filter(|(_, out)| out.board == name)
            .map(|(key, _)| *key)
            .collect();
        for key in keys {
            self.switch_board(Some(key), &successor);
        }
        self.store.mark_dirty(Instant::now());
        log(&format!("board deleted: {name}"));
    }
```

`begin_rename` arrives in task 3; for this task, stub it with `todo!()`? No: give it a body that does
nothing but is legal, so the tree compiles and the tests of this task pass:

```rust
    /// Start editing the name of the board at `index` of the rail's list.
    pub(crate) fn begin_rename(&mut self, _index: usize, _name: &str) {}
```

Task 3 replaces it.

**Step 2.6 — update the tests that pinned the delete row.** In `src/render.rs`:
- `the_panel_lists_every_board_then_the_row_that_creates_one` (the `MAX_BOARD_ROWS + 2` test):
  `rows.len()` becomes `MAX_BOARD_ROWS + 1`; drop the `DeleteBoard` assert; keep `NewBoard` last.
- Delete `the_delete_row_names_the_board_it_would_delete` (the row is gone; its replacement is
  `board_actions`).
- `the_delete_row_is_a_pick_of_its_panel` becomes `a_rename_press_keeps_its_list_and_a_delete_press_closes_it`
  (step 2.1).

In `tests/render.rs`:
- `the_boards_panel_lists_the_boards_new_board_and_the_delete_row`'s expected vector becomes
  `[Board(0), Board(1), NewBoard]`, and the `panel.contains(&Some(DeleteBoard))` assert goes; the
  `block_y` / `NewBoard` assert stays.
- Any other `DeleteBoard` reference in that file (grep `DeleteBoard`) is updated to the new shape
  or removed.

Run: `cargo test board` **Expected:** all pass. Then `cargo test` **Expected:** everything green but
the one pre-existing failure.

**Step 2.7 — commit.** `git add -A src tests && git commit -m "Give every board's row its own delete and rename buttons"`.

---

## Task 3 — a row's name is edited in place, with the input method

**Files:** `src/canvas.rs`, `src/app.rs`, `src/input.rs`.

**Step 3.1 — write the failing tests.** In `src/input.rs`'s test module, beside the `edit_action`
tests:

```rust
    /// A name being edited takes the keys a name needs: `Enter` commits, `Esc` cancels, and the
    /// rest is the buffer's.
    #[test]
    fn a_rename_commits_on_enter_and_cancels_on_escape() {
        assert_eq!(rename_key(Keysym::Return, true), RenameKey::Commit);
        assert_eq!(rename_key(Keysym::Escape, true), RenameKey::Cancel);
        assert_eq!(rename_key(Keysym::BackSpace, true), RenameKey::Buffer);
        assert_eq!(rename_key(Keysym::Left, true), RenameKey::Ignore);
        assert_eq!(rename_key(Keysym::p, true), RenameKey::Buffer);
        // A key with a modifier is not text: it neither types nor commits.
        assert_eq!(rename_key(Keysym::p, false), RenameKey::Ignore);
    }
```

`rename_key` takes `(keysym, ok: bool)` where `ok` says no modifier is held.

Run: `cargo test --lib a_rename_commits_on_enter`
**Expected:** compile error — `rename_key`/`RenameKey` do not exist.

**Step 3.2 — the pure key decision.** In `src/input.rs`, beside `edit_action`:

```rust
/// What a key does while a board's name is being edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenameKey {
    /// `Enter`: the name is taken.
    Commit,
    /// `Esc`: the name is left as it was.
    Cancel,
    /// A character, a deletion or a paste: the buffer's business.
    Buffer,
    /// Everything else: a name is one line with no caret moves.
    Ignore,
}

/// `ok` is false when a modifier is held, and a chord is not text.
pub fn rename_key(keysym: Keysym, ok: bool) -> RenameKey {
    match keysym {
        Keysym::Return | Keysym::KP_Enter => RenameKey::Commit,
        Keysym::Escape => RenameKey::Cancel,
        Keysym::BackSpace | Keysym::Delete | Keysym::V if ok => RenameKey::Buffer,
        Keysym::Left | Keysym::Right | Keysym::Up | Keysym::Down => RenameKey::Ignore,
        _ if ok && !is_modifier(keysym) => RenameKey::Buffer,
        _ => RenameKey::Ignore,
    }
}
```

Check `Keysym` variant names in this file (`Keysym::BackSpace`, `Keysym::Return`, `Keysym::KP_Enter`,
`is_modifier`) and use the ones the file already spells.

Run the test. **Expected:** pass.

**Step 3.3 — the state.** `src/app.rs`:

```rust
/// A board's name being edited in the rail's list.
pub struct Rename {
    /// The row's place in the rail's list, which is stable while the keyboard is on it.
    pub(crate) index: usize,
    /// The name the row had: the key the rename moves.
    pub(crate) board: String,
    pub(crate) text: TextBuffer,
    /// Input method preedit string; never persisted.
    pub(crate) preedit: String,
}
```

`App` gains `pub(crate) rename: Option<Rename>,` (initialised `None` beside `size_open`), and:

```rust
    /// The buffer and preedit being typed into: a text box, a note, or a board's name. One of them at
    /// most — starting a rename ends a text edit, and a press ends a rename.
    fn edit_target_mut(&mut self) -> Option<(&mut TextBuffer, &mut String)> {
        if let Some(rename) = self.rename.as_mut() {
            return Some((&mut rename.text, &mut rename.preedit));
        }
        let out = self
            .outputs
            .values_mut()
            .find_map(|out| out.overlay.text.as_mut())?;
        Some((&mut out.buffer, &mut out.preedit))
    }

    fn edit_target(&self) -> Option<(&TextBuffer, &str)> {
        if let Some(rename) = self.rename.as_ref() {
            return Some((&rename.text, &rename.preedit));
        }
        let out = self
            .outputs
            .values()
            .find_map(|out| out.overlay.text.as_ref())?;
        Some((&out.buffer, &out.preedit))
    }

    /// Begin editing the name of the board at `index`, whose name is `name`.
    pub(crate) fn begin_rename(&mut self, index: usize, name: &str) {
        self.end_text_edit();
        self.commit_transient_drawing();
        self.rename = Some(Rename {
            index,
            board: name.to_string(),
            text: TextBuffer::new(name),
            preedit: String::new(),
        });
        self.dirty = true;
        self.sync_text_input();
        self.refresh_toolbar();
    }

    /// Take the edited name if it is one the document can hold; anything else leaves the board as it
    /// was. A name is dropped rather than reported: the rail has nowhere to say why.
    pub(crate) fn commit_rename(&mut self) {
        let Some(rename) = self.rename.take() else {
            return;
        };
        let name = rename.text.text.trim().to_string();
        if name != rename.board {
            self.rename_board(&rename.board, &name);
        }
        self.dirty = true;
        self.sync_text_input();
        self.refresh_toolbar();
    }

    /// Leave the name as it was.
    pub(crate) fn cancel_rename(&mut self) {
        if self.rename.take().is_none() {
            return;
        }
        self.dirty = true;
        self.sync_text_input();
        self.refresh_toolbar();
    }
```

`sync_text_input` becomes:

```rust
        let state = self
            .edit_target()
            .map(|(buffer, _)| (buffer.text.clone(), buffer.cursor_bytes()));
```

`apply_text_input_batch` becomes:

```rust
        let Some((buffer, preedit_slot)) = self.edit_target_mut() else {
            im_trace(&format!(
                "batch dropped, nothing is being typed into: preedit {preedit:?} commit {commit:?}"
            ));
            return;
        };
        *preedit_slot = preedit.unwrap_or_default();
        if let Some((before, _after)) = delete {
            let bytes = bytes_to_delete(before, preedit_slot.len());
            buffer.delete_before_bytes(bytes);
        }
        if !commit.is_empty() {
            buffer.insert(&commit);
        }
        im_trace(&format!(
            "applied: preedit {:?}, commit {commit:?}, buffer {:?}",
            preedit_slot, buffer.text
        ));
        self.mark_edit_dirty();
        self.sync_text_input();
```

`edit_buffer`:

```rust
    pub(crate) fn edit_buffer(&mut self, f: impl FnOnce(&mut TextBuffer)) {
        let Some((buffer, _)) = self.edit_target_mut() else {
            return;
        };
        f(buffer);
        self.mark_edit_dirty();
        self.sync_text_input();
    }
```

`preedit_is_empty`: `self.edit_target().is_none_or(|(_, p)| p.is_empty())`.
`paste`: gate on `self.edit_target().is_some()`.
`focused_edit`/`focused_edit_mut` are deleted; the remaining callers (`press_key`, `repeat_key`)
switch to `self.edit_target_mut().is_some()` — `press_key` first gains the rename branch below.

**Step 3.4 — the keys.** `press_key`, before the text-box branch:

```rust
        if self.rename.is_some() {
            match rename_key(event.keysym, !self.ctrl) {
                RenameKey::Commit => self.commit_rename(),
                RenameKey::Cancel => self.cancel_rename(),
                RenameKey::Buffer => match edit_action(event.keysym, self.ctrl, !self.preedit_is_empty()) {
                    EditAction::Backspace => self.edit_buffer(|b| {
                        b.backspace();
                    }),
                    EditAction::Paste => self.paste(),
                    EditAction::Local => {
                        if let Some(text) = event.utf8 {
                            self.edit_buffer(|b| b.insert(&text));
                        }
                    }
                    EditAction::EndText | EditAction::Newline | EditAction::Move(_) | EditAction::InputMethod => {}
                },
                RenameKey::Ignore => {}
            }
            return;
        }
```

`repeat_key`: `if self.mode != Mode::Edit || self.edit_target_mut().is_some() { return; }`.

**Step 3.5 — a press elsewhere commits.** `pointer_press`, first statement:

```rust
        // A name being edited is committed by leaving it, the way a text box is: the press then goes
        // on to do what it would have done.
        self.commit_rename();
```

**Step 3.6 — a mode switch commits.** `set_mode`: add `self.commit_rename();` beside the existing
`self.end_text_edit();`.

**Step 3.7 — the snapshot.** `src/canvas.rs`, beside `BoardRow`:

```rust
/// A board's name being edited, as the frame draws it.
#[derive(Debug, Clone, PartialEq)]
pub struct BoardEdit {
    /// The row's place in `Toolbar::boards`.
    pub index: usize,
    pub text: String,
    pub cursor: usize,
    pub preedit: String,
}
```

`Toolbar` gains `pub rename: Option<BoardEdit>,` (with a doc line: the row draws this instead of its
name). `current_toolbar` fills it:

```rust
                    rename: self.rename.as_ref().map(|rename| BoardEdit {
                        index: rename.index,
                        text: rename.text.text.clone(),
                        cursor: rename.text.cursor,
                        preedit: rename.preedit.clone(),
                    }),
```

Add `rename` to `Toolbar`'s `PartialEq` compare in `render.rs` (the `a.size_open == b.size_open`
chain) and to the six struct literals (`src/render.rs` ×5, `tests/render.rs` ×1, `benches/render.rs`
if it has one): `rename: None,`.

Run `cargo test` **Expected:** green but the one pre-existing failure. Then
`cargo test --lib a_rename` **Expected:** pass.

**Step 3.8 — commit.** `git add -A src && git commit -m "Edit a board's name in its row, with the input method"`.

---

## Task 4 — the buttons and the editor are drawn

**Files:** `src/render.rs`, `README.md`.

**Step 4.1 — write the failing test.** In `src/render.rs`'s test module:

```rust
    /// The row being edited draws the buffer, not the name it started from.
    #[test]
    fn the_edited_row_draws_the_buffer_instead_of_its_name() {
        let mut toolbar = Toolbar {
            boards_open: true,
            boards: vec![BoardRow { name: "laptop".into(), current: true }],
            ..crate::render::tests::rail_for_legend()
        };
        let row = board_rows(&toolbar)
            .into_iter()
            .find(|row| row.action == Some(ToolbarAction::Board(0)))
            .expect("no row for the first board");
        assert_eq!(row_label(&toolbar, &row), "laptop");
        toolbar.rename = Some(crate::canvas::BoardEdit {
            index: 0,
            text: "left".into(),
            cursor: 4,
            preedit: String::new(),
        });
        assert_eq!(row_label(&toolbar, &row), "left");
    }
```

Run: `cargo test --lib the_edited_row_draws_the_buffer`
**Expected:** compile error — `row_label` does not exist.

**Step 4.2 — the label the row draws.**

```rust
/// What a row draws where its name goes: the buffer while it is being edited.
fn row_label<'a>(toolbar: &'a Toolbar, row: &'a Block) -> &'a str {
    if let (Some(edit), Some(ToolbarAction::Board(index))) = (&toolbar.rename, row.action)
        && edit.index == index
    {
        return &edit.text;
    }
    &row.label
}
```

In `draw_toolbar`'s label block, replace `&control.block.label` with `row_label(toolbar, &control.block)`
and compute its width with `self.text_width` for the clip.

Run the test. **Expected:** pass.

**Step 4.3 — the two icons.** `src/render.rs`, `enum Icon` gains `Cross, Pencil,`. In `draw_icon`,
before the final `match`, two blocks in the file's own stroke language:

```rust
    if icon == Icon::Cross {
        // A cross: two lines through the middle of the box.
        for (from, to) in [
            ((0.25, 0.25), (0.75, 0.75)),
            ((0.75, 0.25), (0.25, 0.75)),
        ] {
            let mut line = PathBuilder::new();
            line.move_to(x + w * from.0, y + h * from.1);
            line.line_to(x + w * to.0, y + h * to.1);
            if let Some(path) = line.finish() {
                pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
            }
        }
        return;
    }
    if icon == Icon::Pencil {
        // A pencil: a body running up the box and the tip at its foot.
        let mut body = PathBuilder::new();
        body.move_to(x + w * 0.3, y + h * 0.8);
        body.line_to(x + w * 0.75, y + h * 0.2);
        if let Some(path) = body.finish() {
            pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
        }
        let mut tip = PathBuilder::new();
        tip.move_to(x + w * 0.3, y + h * 0.8);
        tip.line_to(x + w * 0.42, y + h * 0.78);
        tip.line_to(x + w * 0.32, y + h * 0.68);
        tip.close();
        if let Some(path) = tip.finish() {
            pixmap.fill_path(&path, &paint, tiny_skia::FillRule::Winding, Transform::identity(), None);
        }
        return;
    }
```

**Step 4.4 — draw the buttons.** In `draw_toolbar`'s loop over `pill.controls`, after the label
block, for a board row:

```rust
            // A board's row shows its two buttons at its right end while the pointer is on it: the
            // name is clipped where they begin, and each button is a block-sized square of its own.
            if let Some(ToolbarAction::Board(index)) = control.block.action
                && let Some((delete, _)) = board_actions(toolbar, index)
            {
                let mut actions = Vec::new();
                if delete {
                    actions.push(Icon::Cross);
                }
                actions.push(Icon::Pencil);
                for (number, icon) in actions.iter().enumerate() {
                    let slot = actions.len() - number;
                    let square = Rect {
                        x: (ox + column_x + width - slot as f32 * TOOLBAR_BLOCK) * scale,
                        y: (oy + control.y) * scale,
                        w: TOOLBAR_BLOCK * scale,
                        h: TOOLBAR_BLOCK * scale,
                    };
                    draw_icon(pixmap, *icon, square, scale, TOOLBAR_LABEL_TEXT);
                }
            }
```

and the name's clip becomes `Some(ox + column_x + width - buttons_width)` where
`buttons_width = if board_actions(toolbar, index).is_some() { 2.0 * TOOLBAR_BLOCK } else { 0.0 }`.

**Step 4.5 — the caret and the preedit.** In the same block, when the row is the one being edited:

```rust
                // The caret sits after the buffer's text, and the preedit after the caret: a name is
                // typed in the row the way a text box is typed in the canvas.
                if let (Some(edit), Some(ToolbarAction::Board(index))) =
                    (&toolbar.rename, control.block.action)
                    && edit.index == index
                {
                    let typed: String = edit.text.chars().take(edit.cursor).collect();
                    let x = ox + label_x(column_x, true) + self.text_width(&typed, TOOLBAR_TEXT_SIZE);
                    let caret = Rect {
                        x: x * scale,
                        y: (oy + control.y + TOOLBAR_BLOCK * 0.2) * scale,
                        w: TOOLBAR_HAIRLINE.max(1.0) * scale,
                        h: TOOLBAR_BLOCK * 0.6 * scale,
                    };
                    round_rect(pixmap, caret, 0.0, parse_hex(TOOLBAR_ACCENT), None);
                    if !edit.preedit.is_empty() {
                        let item = toolbar_text(
                            x + TOOLBAR_HAIRLINE,
                            oy + control.y,
                            TOOLBAR_BLOCK,
                            TOOLBAR_TEXT_SIZE,
                            TOOLBAR_FOREGROUND,
                            &edit.preedit,
                        );
                        self.draw_text(
                            pixmap,
                            &item,
                            Screen { scale, view: View::default() },
                            None,
                            Some(ox + column_x + width),
                            None,
                        );
                    }
                }
```

**Step 4.6 — README.** The boards paragraph (line ~61) loses "and `delete <name>` removes the board
this screen shows with everything drawn on it, moving every screen showing it to the first board
left; the row is absent when one board is all the document has, because a document with no boards is
nowhere to draw" and gains: each row shows `✕` and `✎` while the pointer is on it; `✕` removes that
board with everything drawn on it, moving every screen showing it to the first board left, and is
absent on the last board; `✎` edits the name in the row, `Enter` takes it, `Esc` leaves it, and a
press elsewhere takes it too. Delete "Boards cannot be renamed." from that paragraph and "A board
can be created, picked and deleted but not renamed." from the limitations list (that row keeps the
rest of its sentences).

**Step 4.7 — gate.** `cargo fmt --check && cargo clippy --all-targets && cargo test --no-fail-fast`
**Expected:** fmt clean, no warnings, everything green but `the_preedit_renders_at_the_cursor`.

**Step 4.8 — commit.** `git add -A src tests README.md && git commit -m "Draw a board row's buttons and its name editor"`.

---

## Review focus

- The reveal predicate and the hit order at a row's right end: no dead strip between the name and
  the buttons, no flicker when the pointer moves onto a button, and a press on the name still picks
  the board.
- The rename's reach: the map key, `active`, every output, the rail's list, and the name the row
  started from.
- The editor against the input method and the dismissal rules: `Esc`, press outside, mode switch,
  and the text box editor that shares `edit_target`.
- The last board's `✕`, and `MAX_BOARD_ROWS` with two buttons a row.
