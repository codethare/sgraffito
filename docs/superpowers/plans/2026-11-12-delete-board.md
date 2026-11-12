# Delete a board from the boards panel — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The open `boards` panel gets one more row, `delete <name>`, that removes the board this output is showing with everything drawn on it.

**Architecture:** The document rule (which board may go, and which board takes its place) lives in `Doc` so it is testable without a compositor. The row is one `ToolbarAction` rendered by the existing panel machinery, so layout, hit testing, label drawing and the panel's closing rules need no new code. The click is glue in `App`: end the edit, drop the gesture, delete the board, then move every output that was showing it with the existing `switch_board`.

**Tech Stack:** Rust 2024, `tiny-skia` (vector drawing), `BTreeMap` document, no new dependencies.

**Spec:** `docs/superpowers/specs/2026-11-12-delete-board-design.md`

## Global Constraints

- Gate: `cargo fmt --check`, `cargo clippy --all-targets` (0 warnings), `cargo test`.
- No new dependencies and no change to the annotation file format (`store::VERSION` stays 4).
- The document never has zero boards.
- Deletion is final: there is no undo, and nothing offers a confirmation.
- English in code comments, README, commit messages.
- A panel row carries one action and one hit box, and its label is what it says at rest.
- The panel lists at most `MAX_BOARD_ROWS` boards and is drawn inside the rail's fixed damage box.
- Commit style: imperative sentence, no prefix (`Let a board be deleted, first of what is left taking its place`).

## Review Focus

The five failure modes the spec implies that no step's test can exercise, most likely first.
Each names the task that owns the code and what a reviewer must check there.

1. **A stroke, shape or text edit in flight when the delete row is clicked.** Every commit path looks its board up with `entry(...).or_default()`, so a commit after the removal would write the deleted board back as a fresh empty `Board N`. There is no unit seam (the app needs a Wayland connection); Task 3 runs `end_text_edit` and `commit_transient_drawing` *before* the removal and the reviewer checks that order.
2. **Two outputs in per-screen mode showing the deleted board.** Both must land on the successor, and an output on another board must not move. No unit seam; Task 3 snapshots the affected keys before switching and the reviewer checks that this is a snapshot, not a live loop over outputs it is mutating.
3. **The last board.** The row must be absent, and a `Doc::delete_board` call must refuse rather than leave an empty map. Pinned by `the_only_board_cannot_be_deleted` and `a_document_with_one_board_has_no_delete_row`.
4. **The panel's row cap and the output's height.** The new row is the first thing truncation drops on a short output, and it must not push the panel outside the box the rail declares. Pinned by `the_boards_panel_is_capped` and the existing `the_boards_panel_paints_inside_the_rails_box`.
5. **A click that lands one frame after the panel was laid out.** The row carries no board name and no index, so it cannot go stale: Task 3 resolves the board from the output at click time. The reviewer checks that nothing remembers the label's name.

---

### Task 1: The document rule

**Files:**
- Modify: `src/canvas.rs` (`impl Doc`, after `new_board`)
- Test: `tests/store.rs` (after `a_new_board_takes_the_first_free_number`)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `impl Doc { pub fn delete_board(&mut self, name: &str) -> Option<String> }` — removes the board, returns the name of the first remaining board, and returns `None` (changing nothing) when `name` is not a board's or is the document's only board. It also moves `Doc::active` to that first remaining name when `active` named the deleted board.

- [ ] **Step 1: Write the failing tests**

Append to `tests/store.rs`:

```rust
/// A board can be removed, and the board that takes its place is the first one the document still has,
/// so which board a screen lands on after a deletion is the document's decision, not the pointer's.
#[test]
fn deleting_a_board_names_the_first_remaining_board() {
    let mut doc = Doc::default();
    doc.boards.insert("Board 1".into(), Board::default());
    doc.boards.insert("Board 3".into(), Board::default());
    doc.boards.insert("office".into(), Board::default());
    assert_eq!(doc.delete_board("Board 3"), Some("Board 1".to_string()));
    assert!(!doc.boards.contains_key("Board 3"));
    assert!(doc.boards.contains_key("Board 1"));
    assert!(doc.boards.contains_key("office"));
}

/// A document with no boards is nowhere to draw, so the last one is refused rather than removed.
#[test]
fn the_only_board_cannot_be_deleted() {
    let mut doc = Doc::default();
    doc.boards.insert("office".into(), Board::default());
    assert_eq!(doc.delete_board("office"), None);
    assert!(doc.boards.contains_key("office"));
}

/// A name the document does not hold changes nothing: the row a click lands on and the document can
/// disagree by a frame, and that must not empty a board or the map.
#[test]
fn deleting_a_board_that_is_not_there_changes_nothing() {
    let mut doc = Doc::default();
    doc.boards.insert("Board 1".into(), Board::default());
    doc.boards.insert("office".into(), Board::default());
    let before = doc.clone();
    assert_eq!(doc.delete_board("gone"), None);
    assert_eq!(doc, before);
}

/// The board picked last must not be the one just deleted: a screen that appears afterwards lands on
/// the board an output adopted, and that name has to exist.
#[test]
fn deleting_the_picked_board_moves_the_pick_to_the_one_that_takes_its_place() {
    let mut doc = Doc::default();
    doc.boards.insert("Board 1".into(), Board::default());
    doc.boards.insert("office".into(), Board::default());
    doc.active = "office".into();
    assert_eq!(doc.delete_board("office"), Some("Board 1".to_string()));
    assert_eq!(doc.active, "Board 1");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test store`
Expected: FAIL to compile — `no method named 'delete_board' found for struct 'Doc'`.

- [ ] **Step 3: Implement `Doc::delete_board`**

In `src/canvas.rs`, inside `impl Doc` right after `new_board`:

```rust
    /// Remove a board with everything drawn on it, and name the board that takes its place: the first one
    /// still in the document, which is the board [`Doc::adopt`] falls back to. `None` when the name is not
    /// a board's, or is the document's last one — a document with no boards is nowhere to draw. The board
    /// picked last is moved to the successor, so no later screen adopts a name that is gone.
    pub fn delete_board(&mut self, name: &str) -> Option<String> {
        if self.boards.len() <= 1 || !self.boards.contains_key(name) {
            return None;
        }
        self.boards.remove(name);
        let successor = self.boards.keys().next().cloned()?;
        if self.active == name {
            self.active = successor.clone();
        }
        Some(successor)
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --test store`
Expected: PASS — the four new tests plus the existing store tests.

- [ ] **Step 5: Commit**

```bash
git add src/canvas.rs tests/store.rs
git commit -m "Let a board be deleted, first of what is left taking its place"
```

---

### Task 2: The panel's row

**Files:**
- Modify: `src/canvas.rs` (`enum ToolbarAction`, after `NewBoard`)
- Modify: `src/render.rs` (`key_of`, `picks_from_panel`, `board_rows`; the unit tests at the foot)
- Test: `tests/render.rs` (`boards_rail` neighbours)

**Interfaces:**
- Consumes: `Toolbar.boards: Vec<BoardRow>` where `BoardRow { name: String, current: bool }`.
- Produces: `ToolbarAction::DeleteBoard` (a unit variant, no index and no name); `board_rows(toolbar) -> Vec<Block>` appends it after `new board` only when `toolbar.boards.len() > 1` and one row is `current`; `picks_from_panel(ToolbarAction::DeleteBoard)` is `true`, so the click closes the panel like any other pick.

- [ ] **Step 1: Write the failing tests**

In `tests/render.rs`, update the doc comment above `the_boards_panel_states_the_documents_boards` to
"one row per board, the one this output shows marked, then the row that creates one and the row that
deletes one", and replace the expected vector in that test:

```rust
    assert_eq!(
        rows,
        vec![
            ToolbarAction::Board(0),
            ToolbarAction::Board(1),
            ToolbarAction::NewBoard,
            ToolbarAction::DeleteBoard
        ],
        "the panel's rows are not its boards, the row that creates one and the row that deletes one"
    );
```

and add, after that test:

```rust
/// A document with one board has nothing to delete, so the row is absent rather than inert: a control
/// that does nothing is a control nobody can tell from one that does.
#[test]
fn a_panel_with_one_board_has_no_delete_row() {
    let (w, h) = (900.0f32, 1000.0f32);
    let mut renderer = Renderer::new();
    let rail = boards_rail(&[("Board 1", true)]);
    let sweep = sweep_rail(&mut renderer, &rail, w, h, 40.0);
    let mut rows: Vec<ToolbarAction> = Vec::new();
    for action in sweep.into_iter().flatten() {
        if !rows.contains(&action) {
            rows.push(action);
        }
    }
    assert_eq!(rows, vec![ToolbarAction::Board(0), ToolbarAction::NewBoard]);
}
```

In `src/render.rs`, in the `mod tests` at the foot, after `the_boards_panel_is_capped`:

```rust
    /// The row says which board it removes, because the panel is a list and the footer is where the
    /// list's own actions live: `delete office` cannot be mistaken for a board called that.
    #[test]
    fn the_delete_row_names_the_board_it_removes() {
        let toolbar = Toolbar {
            boards_open: true,
            boards: vec![
                crate::canvas::BoardRow {
                    name: "Board 1".into(),
                    current: false,
                },
                crate::canvas::BoardRow {
                    name: "office".into(),
                    current: true,
                },
            ],
            ..crate::render::tests::rail_for_legend()
        };
        let rows = board_rows(&toolbar);
        let delete = rows
            .iter()
            .find(|row| row.action == Some(ToolbarAction::DeleteBoard))
            .expect("the panel has no row that deletes a board");
        assert_eq!(delete.label, "delete office");
        assert!(!delete.active, "the row that deletes is not a list item that is on");
    }

    /// The delete row is a pick out of the panel it lives in: it closes the panel like every other row,
    /// and the panel stays open while the pointer is on it.
    #[test]
    fn the_delete_row_is_a_pick_of_its_panel() {
        assert!(picks_from_panel(ToolbarAction::DeleteBoard));
        assert!(panel_stays_open(
            Some(ToolbarAction::DeleteBoard),
            Some(ToolbarAction::Boards)
        ));
        assert!(!panel_stays_open(
            Some(ToolbarAction::DeleteBoard),
            Some(ToolbarAction::More)
        ));
    }
```

and update `the_boards_panel_is_capped` to count the row:

```rust
        let rows = board_rows(&toolbar);
        assert_eq!(rows.len(), MAX_BOARD_ROWS + 2);
        assert_eq!(
            rows.get(rows.len() - 2).and_then(|row| row.action),
            Some(ToolbarAction::NewBoard),
            "the row that creates a board is the one that must survive the cap"
        );
        assert_eq!(
            rows.last().and_then(|row| row.action),
            Some(ToolbarAction::DeleteBoard),
            "the row that deletes comes after the row that creates"
        );
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib` and `cargo test --test render`
Expected: FAIL to compile — `no variant or associated item named 'DeleteBoard' found`.

- [ ] **Step 3: Add the action and the row**

`src/canvas.rs`, in `enum ToolbarAction` after `NewBoard`:

```rust
    /// Delete the board this output shows, from the panel's own row. It carries no name or index, so a
    /// click reads which board it means from the output rather than from the row it landed on.
    DeleteBoard,
```

`src/render.rs`, in `key_of`, add the variant to the panel-row arm:

```rust
        // The panel's own rows are picks out of the list the block opens, so they name that block's key.
        ToolbarAction::Boards
        | ToolbarAction::Board(_)
        | ToolbarAction::NewBoard
        | ToolbarAction::DeleteBoard => "C",
```

`src/render.rs`, in `picks_from_panel`:

```rust
    DRAWER.contains(&action)
        || matches!(
            action,
            ToolbarAction::Board(_) | ToolbarAction::NewBoard | ToolbarAction::DeleteBoard
        )
```

`src/render.rs`, at the end of `board_rows`, after the `new board` block:

```rust
    // The rows nearest the block are the ones a short output keeps, so the two rows that act on the list
    // rather than being an item of it come after every board, and the destructive one comes last.
    if toolbar.boards.len() > 1
        && let Some(current) = toolbar.boards.iter().find(|board| board.current)
    {
        rows.push(Block {
            mark: Mark::Icon(Icon::Smaller),
            column: 1,
            label: format!("delete {}", current.name),
            action: Some(ToolbarAction::DeleteBoard),
            active: false,
            gap: TOOLBAR_GAP,
        });
    }
    rows
```

Also add `ToolbarAction::DeleteBoard` to the `applies` match in the unit test
`every_block_states_the_key_that_applies_it` (it must compile; the rail it tests has the panel
closed, so its block counts do not change):

```rust
                ToolbarAction::Board(_) | ToolbarAction::NewBoard | ToolbarAction::DeleteBoard => {
                    keysym == Keysym::C || keysym == Keysym::c
                }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib` and `cargo test --test render`
Expected: PASS, including the existing panel tests (`the_boards_panel_paints_inside_the_rails_box`,
`the_boards_block_owns_its_row_as_far_as_the_panel_and_a_row_owns_its_column`).

- [ ] **Step 5: Commit**

```bash
git add src/canvas.rs src/render.rs tests/render.rs
git commit -m "Give the boards panel a row that says which board it deletes"
```

---

### Task 3: The click, and the README

**Files:**
- Modify: `src/app.rs` (new method after `new_board`)
- Modify: `src/input.rs` (`apply_toolbar_action`, beside `ToolbarAction::NewBoard`)
- Modify: `README.md` (the boards paragraph, the `C` key-table row, the board bullet under Known limitations, the Unverified paragraph)

**Interfaces:**
- Consumes: `Doc::delete_board` (Task 1), `ToolbarAction::DeleteBoard` and `picks_from_panel` (Task 2), `App::switch_board(acting: Option<u32>, name: &str) -> bool`, `App::cancel_transients`, `App::end_text_edit`, `App::commit_transient_drawing`, `Store::mark_dirty(Instant)`.
- Produces: `App::delete_board(&mut self, key: u32)` — no return value, nothing consumes it.

- [ ] **Step 1: Implement `App::delete_board`**

In `src/app.rs`, after `new_board`:

```rust
    /// Delete the board an output is showing, from the panel's row that names it. Everything the board
    /// holds goes with it, so the deletion is a document change and schedules the write.
    ///
    /// The edit and the drawing in hand belong to the board being left, and both commit through
    /// `entry(...).or_default()`: run them before the removal, or the board comes back as a fresh empty
    /// one. Every screen that was showing it then moves to the board that takes its place, through
    /// `switch_board`, which is what drops each moved output's selection and raster.
    pub(crate) fn delete_board(&mut self, key: u32) {
        let Some(name) = self.outputs.get(&key).map(|out| out.board().to_string()) else {
            return;
        };
        self.end_text_edit();
        self.commit_transient_drawing();
        let Some(successor) = self.doc.delete_board(&name) else {
            return;
        };
        // The keys are taken before anything moves: in shared mode the first switch moves every output,
        // and a loop that read `self.outputs` while switching would see boards change under it.
        let keys: Vec<u32> = self
            .outputs
            .iter()
            .filter(|(_, out)| out.board == name)
            .map(|(key, _)| *key)
            .collect();
        for key in keys {
            self.switch_board(Some(key), &successor);
        }
        // `switch_board` never schedules a write — a switch is a decision about this sitting — but a
        // deleted board has to leave the file or it is back the next time the daemon starts.
        self.store.mark_dirty(Instant::now());
        log(&format!("board deleted: {name}"));
    }
```

- [ ] **Step 2: Wire the click**

In `src/input.rs`, in `apply_toolbar_action`, beside the `NewBoard` arm:

```rust
            ToolbarAction::DeleteBoard => {
                // The row names the board through the output, so a click with no output has nothing to
                // delete. The panel still closes below, as it does after any pick.
                if let Some(key) = acting {
                    self.delete_board(key);
                }
            }
```

- [ ] **Step 3: Run the whole gate**

Run: `cargo fmt --check && cargo clippy --all-targets && cargo test`
Expected: clean formatting, 0 warnings, every test passes (the new `Doc`, panel and label tests included).
No test targets `App::delete_board` itself: `src/app.rs`'s tests only reach pure helpers and the app
needs a Wayland connection, so its two ordering rules are Review Focus items 1, 2 and 5.

- [ ] **Step 4: Update the README**

In the boards paragraph, replace the sentence that starts with "The rail's **`boards`** block" and the
closing "Boards cannot be renamed or deleted." with:

> The rail's **`boards`** block (`C`) opens a menu of the document's boards — the board this output
> shows is accent-filled, `new board` creates `Board 1`, `Board 2`, …, and `delete <name>` removes the
> board this screen shows with everything drawn on it, moving every screen showing it to the first
> board left; the row is absent when one board is all the document has, because a document with no
> boards is nowhere to draw. Boards cannot be renamed.

In the key table, replace the `C` row with:

> | `C` | open the boards panel beside the rail: pick a board, create one, or delete the board this screen shows (it closes on a pick, on a press elsewhere, or on `Esc`) |

In Known limitations, replace the board bullet with:

> - A board can be created, picked and deleted but not renamed. Deleting is final — there is no undo
>   behind it, and the row names the board it removes, so the board to delete has to be the one the
>   screen is showing; the last board cannot be deleted, and the panel lists eight boards because it
>   is drawn inside a fixed damage box, while `sgraffito boards` lists them all.

In the **Unverified** paragraph, replace "**clicking a board in the panel and watching another output
follow**" with "**clicking a board in the panel, or the row that deletes one, and watching another
output follow**".

- [ ] **Step 5: Commit**

```bash
git add src/app.rs src/input.rs README.md
git commit -m "Delete the board the panel's row names, and say so in the README"
```
