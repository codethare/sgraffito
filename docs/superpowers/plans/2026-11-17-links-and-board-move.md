# Plan: board links, the picker, a clipboard, and moving a selection

Spec: `docs/superpowers/specs/2026-11-17-links-and-board-move-design.md`

## Global constraints

- The annotation file becomes version 5. No new dependency. `cargo fmt` clean, `cargo clippy
  --all-targets` 0 warnings, every test green but the order-dependent `the_preedit_renders_at_the_cursor`.
- One panel at a time. The picker is the boards panel plus a pending target (`App.target`).
- The clipboard is session state: never written to the file; one slot; capped at 200 elements and
  1 MB of JSON.
- Interface that lives on the canvas (the move button) is screen-space and fixed size, like the rail.

## Interfaces

- `TextItem.link: Option<String>` — task 1, consumed by tasks 2 and 3.
- `Board::copied(&self, items) -> Board` and `Board::append(&mut self, other: &Board) -> (Vec<ItemId>, Option<Rect>)`
  — task 1, consumed by tasks 4 and 5 (`Board::duplicate` is rewritten on top of them).
- `Doc::move_items(&mut self, from, to, items) -> Option<(Rect, Rect)>` — task 1, consumed by task 5.
- `canvas::centred(view, size, canvas) -> View` and `canvas::linked_at(ann, point, bounds) -> Option<&TextItem>`
  — task 2, consumed by task 2 only.
- `ToolbarAction::Link` and `App.target: Option<Target>` — task 3, consumed by tasks 3 and 5.
- `App.clip: Option<Board>` and `clipboard_fits(&Board) -> bool` — task 4, consumed by task 4 only.
- `canvas::move_button_bounds(size) -> Rect` — task 5, consumed by task 5 only.

---

## Task 1 — the model: a link on a text, and one board's elements joining another

**Files:** `src/canvas.rs`, `src/store.rs`, `src/app.rs`, every file with a `TextItem {` literal,
`tests/store.rs`.

**Step 1.1 — the field.** `TextItem` gains, after `text`:

```rust
    /// The board this text links to, when it is a link: `Ctrl`+click follows it. Absent in a file
    /// written before links existed.
    #[serde(default)]
    pub link: Option<String>,
```

Then add `link: None,` to every `TextItem {` literal: `grep -rln 'TextItem {' src tests benches`
(30 in this tree). Compile: `cargo build`.
**Expected:** errors only about the missing field, each fixed by that one line.

**Step 1.2 — write the failing tests.** In `tests/store.rs`, the `TextItem` import is already there;
add:

```rust
/// A link is the text's, and it is written: a round trip keeps it, and a version 4 file — which has
/// no links — loads unchanged.
#[test]
fn a_version_4_file_loads_and_a_link_survives_the_round_trip() {
    let dir = tmpdir("links");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("annotations.json");
    let mut doc = Doc::default();
    doc.boards.insert("a".into(), Board::default());
    let mut text = TextItem {
        x: 8.0,
        y: 8.0,
        color: "#ffffff".into(),
        size: 18.0,
        text: "next".into(),
        link: Some("b".into()),
    };
    // A version 4 file holds the same text without the field.
    std::fs::write(
        &path,
        r#"{"version":4,"boards":{"a":{"texts":[{"x":8,"y":8,"color":"#ffffff","size":18,"text":"next"}]}},"active":"a"}"#,
    )
    .unwrap();
    let loaded = store::load(&path);
    assert_eq!(loaded.boards["a"].texts[0].link, None);
    // Written back at this version, the link survives.
    doc.boards.get_mut("a").unwrap().texts.push(text.clone());
    text.link = Some("b".into());
    store::save(&path, &doc).unwrap();
    let again = store::load(&path);
    assert_eq!(again.boards["a"].texts[0].link, Some("b".into()));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A link names a board by name, so a deleted board must not leave texts pointing at nothing.
#[test]
fn a_link_to_a_deleted_board_is_dropped_with_the_board() {
    let mut doc = Doc::default();
    doc.boards.insert("a".into(), Board::default());
    doc.boards.insert("b".into(), Board::default());
    doc.boards.get_mut("a").unwrap().texts.push(TextItem {
        x: 0.0,
        y: 0.0,
        color: "#ffffff".into(),
        size: 18.0,
        text: "go".into(),
        link: Some("c".into()),
    });
    doc.delete_board("c");
    assert!(!doc.boards["a"].texts[0].link.is_some(), "a link to a gone board survived");
}

/// Elements moved to another board leave the first one: the move is a copy into the target and a
/// removal from the source, reported as the box each board gained and lost.
#[test]
fn moving_elements_leaves_the_source_board_without_them() {
    let mut doc = Doc::default();
    doc.boards.insert("a".into(), Board::default());
    doc.boards.insert("b".into(), Board::default());
    doc.boards
        .get_mut("a")
        .unwrap()
        .strokes
        .push(Stroke {
            color: "#ffffff".into(),
            width: 3.0,
            points: vec![[0.0, 0.0], [10.0, 10.0]],
        });
    let moved = doc.move_items("a", "b", &[ItemId::Stroke(0)]);
    assert!(moved.is_some());
    assert!(doc.boards["a"].strokes.is_empty(), "the source kept the element");
    assert_eq!(doc.boards["b"].strokes.len(), 1);
    // A move onto the board it is already on, or a name that is not a board, does nothing.
    assert!(doc.move_items("a", "a", &[]).is_none());
    assert!(doc.move_items("a", "gone", &[]).is_none());
}
```

Run: `cargo test --test store link moving_elements`
**Expected:** compile error — `link` does not exist / `move_items` does not exist, and `store::save`
is only reachable as `store::save`.

Meanwhile, `VERSION` becomes 5 in `src/store.rs`, with its doc comment gaining one sentence: links are
version 5. Nothing else in the store changes: `MIN_VERSION..=VERSION` already accepts older files, and
a newer file is backed up.

**Step 1.3 — copies and appends.** In `impl Board`, above `duplicate`:

```rust
    /// The named elements as their own board: a copy of each, in the order given, with the ids that
    /// copy and append use.
    pub fn copied(&self, items: &[ItemId]) -> Board {
        let mut copy = Board::default();
        for id in items {
            match *id {
                ItemId::Stroke(i) => copy.strokes.extend(self.strokes.get(i).cloned()),
                ItemId::Text(i) => copy.texts.extend(self.texts.get(i).cloned()),
                ItemId::Shape(i) => copy.shapes.extend(self.shapes.get(i).cloned()),
                ItemId::Note(i) => copy.notes.extend(self.notes.get(i).cloned()),
            }
        }
        copy
    }

    /// Append every element of `other` to this board, naming the ids they were given and the box they
    /// occupy here.
    pub fn append(&mut self, other: &Board) -> (Vec<ItemId>, Option<Rect>) {
        let mut ids = Vec::new();
        let mut box_: Option<Rect> = None;
        for stroke in &other.strokes {
            ids.push(ItemId::Stroke(self.strokes.len()));
            union_into(&mut box_, stroke.paint_bounds());
            self.strokes.push(stroke.clone());
        }
        for text in &other.texts {
            ids.push(ItemId::Text(self.texts.len()));
            union_into(&mut box_, text.paint_bounds());
            self.texts.push(text.clone());
        }
        for shape in &other.shapes {
            ids.push(ItemId::Shape(self.shapes.len()));
            union_into(&mut box_, shape.paint_bounds());
            self.shapes.push(shape.clone());
        }
        for note in &other.notes {
            ids.push(ItemId::Note(self.notes.len()));
            union_into(&mut box_, note.bounds());
            self.notes.push(note.clone());
        }
        (ids, box_)
    }
```

Then `duplicate` becomes those two calls with its return kept:

```rust
    pub fn duplicate(&mut self, items: &[ItemId]) -> (Vec<ItemId>, Option<Rect>) {
        let copy = self.copied(items);
        self.append(&copy)
    }
```

Its existing tests must pass unchanged.

**Step 1.4 — moving between boards.** In `impl Doc`:

```rust
    /// Move the named elements of `from` into `to`, reporting the box the source lost and the target
    /// gained. `None` when either board is missing or nothing was there to move.
    pub fn move_items(
        &mut self,
        from: &str,
        to: &str,
        items: &[ItemId],
    ) -> Option<(Rect, Rect)> {
        if from == to || items.is_empty() {
            return None;
        }
        let copy = self.boards.get(from)?.copied(items);
        let gone = self.boards.get_mut(from)?.remove(items)?;
        let (_, gained) = self.boards.get_mut(to)?.append(&copy);
        Some((gone, gained?))
    }
```

**Step 1.5 — a deleted board takes its links with it.** In `Doc::delete_board`, before returning the
successor:

```rust
        // A link names a board, so the boards that survive drop the ones that point at this name.
        for board in self.boards.values_mut() {
            for text in &mut board.texts {
                if text.link.as_deref() == Some(name) {
                    text.link = None;
                }
            }
        }
```

Run: `cargo test --test store`. **Expected:** all pass, the three new ones included.

**Step 1.6 — commit.** `git add -A src tests benches && git commit -m "Give a text a board link, and let a board take another's elements"`.

---

## Task 2 — following a link, and seeing one

**Files:** `src/canvas.rs`, `src/input.rs`, `src/render.rs`, `tests/render.rs`.

**Step 2.1 — the failing tests.** In `src/canvas.rs`'s own `#[cfg(test)] mod tests` if it has one;
this crate keeps document tests in `tests/store.rs` and renderer ones in `tests/render.rs`, so put
these two in `tests/render.rs` (it already builds a `Renderer`) and the view arithmetic in
`tests/store.rs`:

```rust
// tests/store.rs
/// A link puts its text in the middle of the output: the view moves so the text's centre is the
/// output's centre, at whatever zoom is in hand.
#[test]
fn going_to_a_link_centres_the_text() {
    let view = View { zoom: 2.0, offset: [30.0, -10.0] };
    let centred = view.centred_on((800.0, 600.0), [100.0, 50.0]);
    assert_eq!(centred.zoom, 2.0, "a link changed the zoom");
    assert_eq!(centred.offset, [800.0 / 2.0 - 100.0 * 2.0, 600.0 / 2.0 - 50.0 * 2.0]);
}
```

```rust
// tests/render.rs
/// Only a linked text follows a click, and only the one under the pointer.
#[test]
fn a_click_follows_only_the_linked_text_under_it() {
    let ann = Board {
        texts: vec![
            TextItem { x: 0.0, y: 0.0, color: "#fff".into(), size: 18.0, text: "plain".into(), link: None },
            TextItem { x: 0.0, y: 40.0, color: "#fff".into(), size: 18.0, text: "go".into(), link: Some("b".into()) },
        ],
        ..Board::default()
    };
    let bounds = |t: &TextItem| t.paint_bounds();
    assert_eq!(ann.linked_at([4.0, 4.0], bounds).and_then(|t| t.link.clone()), None);
    assert_eq!(
        ann.linked_at([4.0, 44.0], bounds).and_then(|t| t.link.clone()),
        Some("b".into())
    );
    assert!(ann.linked_at([400.0, 400.0], bounds).is_none());
}
```

Run: `cargo test centred a_click_follows`
**Expected:** compile error — `centred`, `linked_at` missing.

**Step 2.2 — the view arithmetic.** In `impl View`:

```rust
    /// The same view moved so a canvas point sits in the middle of an output of this size.
    pub fn centred_on(self, size: (f32, f32), canvas: [f32; 2]) -> View {
        View {
            zoom: self.zoom,
            offset: [
                size.0 / 2.0 - canvas[0] * self.zoom,
                size.1 / 2.0 - canvas[1] * self.zoom,
            ],
        }
    }
```

**Step 2.3 — the hit.** In `src/canvas.rs`, beside `Board::hit`:

```rust
    /// The linked text under a point, tested with the boxes the click test measures rather than the
    /// font-size estimate: a link is a thing you aim at.
    pub fn linked_at<'a>(
        &'a self,
        point: [f32; 2],
        bounds: impl Fn(&TextItem) -> Rect,
    ) -> Option<&'a TextItem> {
        self.texts
            .iter()
            .find(|text| text.link.is_some() && bounds(text).contains(point[0], point[1]))
    }
```

**Step 2.4 — the press.** In `input.rs`'s `pointer_press`, before `toolbar_hit`:

```rust
        // `Ctrl`+click on a linked text goes to its board; a plain click keeps belonging to the tool.
        if self.ctrl
            && let Some((index, board)) = self.linked_text_at(key, x, y)
        {
            self.follow_link(key, index, &board);
            return;
        }
```

with, in `impl App` (app.rs):

```rust
    /// The linked text under a screen point on this output, with its board.
    pub(crate) fn linked_text_at(&mut self, key: &u32, x: f32, y: f32) -> Option<(usize, String)> {
        let out = self.outputs.get(key)?;
        let board_name = out.board.clone();
        let (scale, view) = (out.scale, out.view);
        let ann = self.doc.boards.get(&board_name)?;
        let point = view.to_canvas([x, y]);
        let renderer = &self.renderer;
        let index = ann
            .texts
            .iter()
            .position(|text| {
                text.link.is_some()
                    && renderer
                        .text_edit_bounds(&text.text, text, Screen { scale, view })
                        .contains(point[0], point[1])
            })?;
        let board = ann.texts[index].link.clone()?;
        Some((index, board))
    }

    /// Go to a linked text: switch this output to its board and put the text in the middle.
    pub(crate) fn follow_link(&mut self, key: &u32, index: usize, board: &str) {
        if !self.switch_board(Some(*key), board) {
            return;
        }
        let Some(out) = self.outputs.get(key) else {
            return;
        };
        let Some(text) = self
            .doc
            .boards
            .get(board)
            .and_then(|ann| ann.texts.get(index))
        else {
            return;
        };
        let bounds = text.paint_bounds();
        let centre = [bounds.x + bounds.w / 2.0, bounds.y + bounds.h / 2.0];
        let size = (out.width as f32, out.height as f32);
        self.view_changed(*key, |view| *view = view.centred_on(size, centre));
    }
```

**Step 2.5 — the underline.** In `render::draw_text`, where the glyphs are laid out, when
`item.link.is_some()` draw a hairline under the measured line in `TOOLBAR_ACCENT` — one
`PathBuilder` line from the run's x to its end at `y + run height`, stroked at `screen.ui(TOOLBAR_HAIRLINE)`.

**Step 2.6 — gate, commit.** `cargo fmt --check && cargo clippy --all-targets && cargo test`.
**Expected:** green but the one order-dependent test. Commit:
`git commit -m "Follow a link with ctrl and a click, and underline it"`.

---

## Task 3 — the picker, and the row that sets a link

**Files:** `src/canvas.rs`, `src/render.rs`, `src/input.rs`, `src/app.rs`.

**Step 3.1 — the failing tests.** In `src/render.rs`'s test module:

```rust
    /// The `link` row needs one text box held: nothing, or a stroke, links nothing.
    #[test]
    fn the_link_row_needs_exactly_one_held_text_box() {
        let ann = Board {
            texts: vec![TextItem { x: 0.0, y: 0.0, color: "#fff".into(), size: 18.0, text: "go".into(), link: None }],
            strokes: vec![Stroke { color: "#fff".into(), width: 3.0, points: vec![[0.0, 0.0]] }],
            ..Board::default()
        };
        assert_eq!(link_source(&ann, &[ItemId::Text(0)]), Some(0));
        assert_eq!(link_source(&ann, &[ItemId::Text(0), ItemId::Stroke(0)]), None);
        assert_eq!(link_source(&ann, &[ItemId::Stroke(0)]), None);
        assert_eq!(link_source(&ann, &[]), None);
    }

    /// A pick out of the board list is a target when one is pending, and a switch otherwise; and the
    /// `link` row is in the drawer, so the drawer's own list states it.
    #[test]
    fn a_pending_target_changes_what_a_board_row_press_does() {
        assert!(applies_target(Some(Target::Link)));
        assert!(applies_target(Some(Target::Move)));
        assert!(!applies_target(None));
        assert!(DRAWER.contains(&ToolbarAction::Link));
    }
```

Run: `cargo test the_link_row a_pending_target`
**Expected:** compile error — `link_source`, `Target`, `applies_target`, `ToolbarAction::Link`.

**Step 3.2 — the action and its row.** `ToolbarAction` gains:

```rust
    /// Link the selected text to a board. The row lives in the drawer; a press needs one held text.
    Link,
```

`DRAWER` becomes `[Scrim, Shared, Link]`, `picks_from_panel` gains `Link`, `key_of` maps it to `"M"`
(the drawer's block) like `Scrim`/`Shared`, and `drawer_blocks` gains its arm:

```rust
            ToolbarAction::Link => Some(Block {
                mark: Mark::Icon(Icon::Link),
                column: 1,
                label: labelled(*action, "link board"),
                action: Some(*action),
                active: false,
                gap: TOOLBAR_GAP,
            }),
```

`Icon` gains `Link`: two chain links, drawn as two stroked rounded rects overlapping — the same
stroke language as `Icon::Note`.

**Step 3.3 — the predicate.** In `src/canvas.rs` (free function, so the renderer test can reach it):

```rust
/// The text a link press would link: the one text box held, and nothing else. `None` when nothing is
/// held, more than one thing is, or the held thing is not a text box.
pub fn link_source(ann: &Board, items: &[ItemId]) -> Option<usize> {
    match items {
        [ItemId::Text(index)] if *index < ann.texts.len() => Some(*index),
        _ => None,
    }
}
```

**Step 3.4 — the pending target.** In `src/app.rs`:

```rust
/// What a press in the board list applies, when a list opened to pick a target rather than to switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Target {
    Link,
    Move,
}

/// Whether a press in the board list applies a target instead of switching the screen.
pub fn applies_target(target: Option<Target>) -> bool {
    target.is_some()
}
```

`App` gains `pub(crate) target: Option<Target>` (init `None`; cleared in `close_panels` and
`set_mode`; kept when the panel is only re-laid out). `panel_owner` is unchanged: the picker *is* the
boards panel. `apply_toolbar_action_on`'s `Board(index)` arm becomes:

```rust
            ToolbarAction::Board(index) => {
                let picked = board.or_else(|| self.doc.names().nth(index).cloned());
                if let Some(name) = picked {
                    if let Some(target) = self.target.take() {
                        self.apply_target(target, &name);
                    } else {
                        self.switch_board(acting, &name);
                    }
                }
            }
```

and `ToolbarAction::Link => self.open_link_target()`:

```rust
    /// The `link` row: hold one text box, then pick the board it should link to.
    pub(crate) fn open_link_target(&mut self) {
        let held = self
            .keyboard_focus
            .and_then(|key| self.outputs.get(&key))
            .and_then(|out| out.selection_for_link());
        match held {
            Some(_) => {
                self.target = Some(Target::Link);
                self.set_boards(true);
            }
            None => log("link needs one text box selected"),
        }
    }

    /// Apply a pending target to the board a row picked.
    fn apply_target(&mut self, target: Target, board: &str) {
        match target {
            Target::Link => self.link_selected(board),
            Target::Move => self.move_selected(board),
        }
    }
```

The app reads the held elements once, through one helper both the link and the move use:

```rust
    /// The board an output shows and the elements it holds, if it holds any.
    fn held(&self, key: u32) -> Option<(String, Vec<ItemId>)> {
        let out = self.outputs.get(&key)?;
        let items = out.overlay.selection.as_ref()?.items.clone();
        Some((out.board.clone(), items))
    }
```

and `open_link_target` becomes:

```rust
    pub(crate) fn open_link_target(&mut self) {
        let Some(key) = self.keyboard_focus.filter(|k| self.outputs.contains_key(k)) else {
            log("link needs a focused screen");
            return;
        };
        let Some((board_name, items)) = self.held(key) else {
            log("link needs one text box selected");
            return;
        };
        let Some(ann) = self.doc.boards.get(&board_name) else {
            return;
        };
        if link_source(ann, &items).is_none() {
            log("link needs one text box selected");
            return;
        }
        self.target = Some(Target::Link);
        self.set_boards(true);
    }

    /// Set the link of the held text box, which is a document change like any other.
    fn link_selected(&mut self, board: &str) {
        let Some(key) = self.keyboard_focus else { return };
        let Some((board_name, items)) = self.held(key) else { return };
        let Some(ann) = self.doc.boards.get_mut(&board_name) else { return };
        let Some(index) = link_source(ann, &items) else { return };
        ann.texts[index].link = Some(board.to_string());
        self.doc_changed();
        log(&format!("text linked to {board}"));
    }
```

`Target::Move`'s `move_selected` arrives in task 5; stub it with `{}` and a comment for now.

**Step 3.5 — cancel with the panel.** `close_panels` sets `self.target = None`; `Esc` goes through
`close_panels` already; `set_mode` calls it.

**Step 3.6 — gate, commit.** Green but the one order-dependent test; commit:
`git commit -m "Pick a link target from the board list"`.

---

## Task 4 — one clipboard

**Files:** `src/app.rs`, `src/input.rs`, `src/canvas.rs`.

**Step 4.1 — the failing test.** In `src/canvas.rs`'s tests are none; put the cap in `tests/store.rs`
with a helper of its own:

```rust
/// A copy that would be too big is refused whole rather than trimmed: half a drawing pasted is worse
/// than nothing.
#[test]
fn a_copy_over_the_cap_is_refused_whole() {
    let mut board = Board::default();
    assert!(clipboard_fits(&board));
    board.strokes = (0..MAX_CLIP_ITEMS + 1)
        .map(|i| Stroke { color: "#fff".into(), width: 3.0, points: vec![[i as f32, 0.0]] })
        .collect();
    assert!(!clipboard_fits(&board));
    // A small copy of a very wordy text is refused by bytes, not by count.
    let mut wordy = Board::default();
    wordy.texts.push(TextItem {
        x: 0.0,
        y: 0.0,
        color: "#fff".into(),
        size: 18.0,
        text: "x".repeat(MAX_CLIP_BYTES),
        link: None,
    });
    assert!(!clipboard_fits(&wordy));
}
```

Run: `cargo test a_copy_over_the_cap` **Expected:** compile error — `clipboard_fits`, the two caps.

**Step 4.2 — the cap.** In `src/canvas.rs`:

```rust
/// Most elements one copy may hold, and most bytes of JSON it may be. A clip is memory, not the file:
/// these bound the one case where a copy is a whole busy board.
pub const MAX_CLIP_ITEMS: usize = 200;
pub const MAX_CLIP_BYTES: usize = 1_000_000;

/// Whether a copy may be taken. Both limits, because a few huge strokes and a thousand tiny ones are
/// the same problem seen two ways.
pub fn clipboard_fits(board: &Board) -> bool {
    let items = board.strokes.len() + board.texts.len() + board.shapes.len() + board.notes.len();
    items <= MAX_CLIP_ITEMS
        && serde_json::to_vec(board).is_ok_and(|json| json.len() <= MAX_CLIP_BYTES)
}
```

**Step 4.3 — the clipboard.** `App` gains `pub(crate) clip: Option<Board>`, and:

```rust
    /// Copy the held elements into the app's own clipboard. Over the cap, nothing is copied.
    pub(crate) fn copy_selection(&mut self) {
        let Some(key) = self.keyboard_focus else { return };
        let Some((board_name, items)) = self.held(key) else { return };
        let Some(ann) = self.doc.boards.get(&board_name) else { return };
        let copy = ann.copied(&items);
        if !clipboard_fits(&copy) {
            log("copy refused: too large for the clipboard");
            return;
        }
        self.clip = Some(copy);
        log(&format!("copied {} elements", items.len()));
    }

    /// Paste the clipboard onto the board this screen shows, a little offset, and hand the selection
    /// to the copies so a paste can be moved at once.
    pub(crate) fn paste_selection(&mut self) {
        let Some(clip) = self.clip.clone() else { return };
        let Some(key) = self.keyboard_focus else { return };
        let Some(out) = self.outputs.get(&key) else { return };
        let board_name = out.board.clone();
        // A paste lands a little away from where it was copied from, so it is visible on top of the
        // thing it copies rather than exactly under it.
        let mut shifted = clip;
        for stroke in &mut shifted.strokes {
            for point in &mut stroke.points {
                point[0] += COPY_OFFSET;
                point[1] += COPY_OFFSET;
            }
        }
        for text in &mut shifted.texts {
            text.x += COPY_OFFSET;
            text.y += COPY_OFFSET;
        }
        for shape in &mut shifted.shapes {
            shape.from[0] += COPY_OFFSET;
            shape.to[0] += COPY_OFFSET;
            shape.from[1] += COPY_OFFSET;
            shape.to[1] += COPY_OFFSET;
        }
        for note in &mut shifted.notes {
            note.x += COPY_OFFSET;
            note.y += COPY_OFFSET;
        }
        let Some(ann) = self.doc.boards.get_mut(&board_name) else { return };
        let (ids, box_) = ann.append(&shifted);
        if ids.is_empty() {
            return;
        }
        self.set_selection(key, ids, None);
        if let Some(box_) = box_ {
            self.doc_changed_at(key, box_);
        }
    }
```

`pub const COPY_OFFSET: f32 = 12.0;` beside the cap constants.

**Step 4.4 — the keys.** In `press_key`, after the editor branches and before the `Escape` branch:

```rust
        // With something held and nothing being typed into, the clipboard is the app's own: a paste
        // of a drawing is not a paste of text.
        if self.ctrl && self.edit_target().is_none() {
            match event.keysym {
                Keysym::c | Keysym::C if self.selection_held() => {
                    self.copy_selection();
                    return;
                }
                Keysym::v | Keysym::V if self.clip.is_some() => {
                    self.paste_selection();
                    return;
                }
                _ => {}
            }
        }
```

with `fn selection_held(&self) -> bool` = any output holds a selection.

**Step 4.5 — gate, commit.** Commit: `git commit -m "Copy a selection into the app's own clipboard and paste it"`.

---

## Task 5 — the button that moves a selection to another board

**Files:** `src/canvas.rs`, `src/render.rs`, `src/input.rs`, `src/app.rs`, `README.md`.

**Step 5.1 — the failing test.** In `src/render.rs`'s test module:

```rust
    /// The move button sits in the middle of the bottom of the canvas, inside every output's width,
    /// and it is only there while a selection is held.
    #[test]
    fn the_move_button_is_in_the_bottom_middle() {
        for size in [(800.0, 600.0), (1920.0, 1080.0)] {
            let box_ = move_button_bounds(size);
            assert!(box_.x > 0.0 && box_.x + box_.w < size.0, "the button left the width");
            assert!(box_.y + box_.h < size.1 && box_.y > size.1 / 2.0, "the button left the bottom");
            let centre = box_.x + box_.w / 2.0;
            assert!((centre - size.0 / 2.0).abs() < 1.0, "the button is not centred");
        }
    }
```

Run: `cargo test the_move_button_is_in_the_bottom_middle` **Expected:** compile error — `move_button_bounds`.

**Step 5.2 — the box.** In `src/canvas.rs`:

```rust
/// The move button: a capsule at the bottom centre of the canvas, wide enough for `move to board`.
pub fn move_button_bounds(size: (f32, f32)) -> Rect {
    Rect {
        x: size.0 / 2.0 - MOVE_BUTTON_W / 2.0,
        y: size.1 - TOOLBAR_BLOCK - 2.0 * TOOLBAR_PAD,
        w: MOVE_BUTTON_W,
        h: TOOLBAR_BLOCK,
    }
}
```

with `pub const MOVE_BUTTON_W: f32 = 128.0;`. This needs `TOOLBAR_BLOCK`/`TOOLBAR_PAD` where they are
now private in `render.rs`: the simplest is to build the box in `render.rs` instead, beside the
toolbar's own geometry, and export it. Put `move_button_bounds` and its constants in `src/render.rs`,
where those numbers already live, and have the test sit there too.

**Step 5.3 — hit before the tool.** In `input.rs`'s `pointer_press`, after the toolbar test and before
the canvas:

```rust
        // The move button is interface: a press on it must not also start a stroke.
        if self.selection_held()
            && let Some(out) = self.outputs.get(key)
            && move_button_bounds((out.width as f32, out.height as f32)).contains(x, y)
        {
            self.target = Some(Target::Move);
            self.set_boards(true);
            return;
        }
```

**Step 5.4 — move the selection.** `move_selected`:

```rust
    /// Move the held elements onto the board a target press picked.
    fn move_selected(&mut self, board: &str) {
        let Some(key) = self.keyboard_focus else { return };
        let Some((board_name, items)) = self.held(key) else { return };
        if board_name == board {
            return;
        }
        if self.doc.move_items(&board_name, board, &items).is_none() {
            return;
        }
        // Two boards changed: the selection named the source's indices, so it goes.
        self.drop_selection();
        self.doc_changed();
        log(&format!("moved {} elements to {board}", items.len()));
    }
```

`doc_changed` already drops every raster, which covers a screen showing the target board.

**Step 5.5 — draw it.** In `render_with_cache`, after the toolbar is drawn, when the overlay holds a
selection and the toolbar is present:

```rust
        if let Some(toolbar) = &overlay.toolbar
            && overlay.selection.is_some()
        {
            let box_ = move_button_bounds((w, h));
            // The same capsule a rail row is: the interface gains no shape it did not have.
            round_rect(
                &mut pixmap,
                Rect { x: box_.x * scale, y: box_.y * scale, w: box_.w * scale, h: box_.h * scale },
                (TOOLBAR_BLOCK / 2.0) * scale,
                parse_hex(TOOLBAR_BLOCK_FILL),
                Some((parse_hex(TOOLBAR_BLOCK_EDGE), TOOLBAR_HAIRLINE * scale)),
            );
            let item = toolbar_text(
                box_.x + TOOLBAR_LABEL_PAD,
                box_.y,
                TOOLBAR_BLOCK,
                TOOLBAR_TEXT_SIZE,
                TOOLBAR_LABEL_TEXT,
                "move to board",
            );
            self.draw_text(&mut pixmap, &item, Screen { scale, view: View::default() }, None, None, None);
        }
```

**Step 5.6 — its damage.** In `app.rs`'s `draw_output`, union the button's box into `transient` when a
selection is held (the same place `selection_bounds` is unioned), so the frame that removes it
repaints where it was:

```rust
        let transient = if out.overlay.selection.is_some() {
            let button = crate::render::move_button_bounds((w as f32, h as f32));
            Some(transient.map_or(button, |box_| box_.union(button)))
        } else {
            transient
        };
```

**Step 5.7 — README.** The `select` paragraph gains: `Ctrl+C`/`Ctrl+V` copy a selection into the app's
own clipboard (capped, session-only, crossing boards), and a `move to board` button appears at the
bottom of the screen while something is held. A new sentence in the boards paragraph: a text can be
linked to a board from the drawer's `link board` row, and `Ctrl`+click follows it. The limitations list
gains the version-5 sentence: a file written by this build is not read by an older one, which backs it
up and starts empty.

**Step 5.8 — gate, commit.** `cargo fmt --check && cargo clippy --all-targets && cargo test && scripts/smoke.sh`.
Commit: `git commit -m "Move a selection to another board from a button under the canvas"`.

---

## Review focus

- The version bump, and what an older build does with a version 5 file.
- The move button's box against the canvas press and against damage: planted and removed cleanly.
- `Ctrl+C`/`Ctrl+V` inside a text box, inside a name, and with nothing held.
- A link whose board is gone; shared mode versus per-screen when a link or a move lands.
- The picker's dismissal: `Esc`, a press outside, another panel, a mode switch; the pending target
  surviving a re-layout.
- The cap's arithmetic, and pasting the same clip twice.
