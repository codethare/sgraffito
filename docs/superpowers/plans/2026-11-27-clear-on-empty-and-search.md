# Clear-on-empty-press and keyword search Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** With the selection tool armed, a press on empty canvas drops the selection at once; and `/`
opens a keyword search over the board's words in the `more tools` drawer, with the matched characters
highlighted and a pick that selects and centres the element.

**Architecture:** `Board::search` (pure, `canvas.rs`) turns a query into `SearchHit`s in canvas order.
The rail carries a `Search` snapshot (`Toolbar.search`), which `open_panel` turns into a field row plus
one row per hit, drawn with a new `Mark::Hit` of text runs. The app owns the live state, routes the
keys and paste through the existing `edit_target` text-sink path, and picks with `View::centred_on`.

**Tech Stack:** Rust, `tiny-skia` through the existing renderer, `serde` (unchanged schema), `jj`.

**Spec:** `docs/superpowers/specs/2026-11-27-clear-on-empty-and-search-design.md`

## Global Constraints

- No new dependencies.
- `VERSION = 6`, `READABLE_VERSION = 7` unchanged. The search is rail state: nothing about it is
  written to a board, and nothing on disk changes.
- One undo step per user action: a search pick changes no document state, so it records nothing.
- English in code, comments, docs and commits.
- The known baseline failure `tests/render.rs::the_preedit_renders_at_the_cursor` is not ours.
- Never commit `AGENTS.md`, `.agents/**` or `openspec/**`; every commit is path-scoped with
  `jj commit <paths> -m "..."`.
- The `/` binding must update `README.md`, the drawer row's key badge, and
  `src/render.rs`'s `every_block_states_the_key_that_applies_it` / `tests/keys.rs`.

## Review Focus

- **Typing `/` in a text box.** With a text box or a note being edited, `/` must reach the box, not
  open the search: a URL or a path typed on a note is the common case.
- **A stale hit after a document change.** Delete the matched text while the panel is open, then
  press `Enter`: the pick must do nothing, not select a different element that now sits at the old
  index.
- **A query with no hits.** The panel keeps its field row and no result rows; `Enter` does nothing;
  `↑`/`↓` do not panic on an empty list.
- **A very long match.** A note with a 200-character line: the row must clip at the capsule edge, not
  run across the output or under the rail.
- **The empty press with something held.** A press on empty canvas while a selection is held clears
  it, and the marquee that may follow still selects what it sweeps; `Shift`+press on empty still
  changes nothing.

---

### Task 1: `Board::search`

**Files:**
- Modify: `src/canvas.rs` (beside `Annotation::recolour_fill`)
- Test: `tests/canvas_hit.rs`

**Interfaces:**
- Consumes: `TextItem::text`, `Note::text`, `ItemId`, `Rect`/`centre`.
- Produces:
  - `pub struct SearchHit { pub item: ItemId, pub preview: String, pub at: usize, pub len: usize }`
  - `impl Board { pub fn search(&self, query: &str) -> Vec<SearchHit> }`
  - `pub const PREVIEW_WORDS: usize = 2;`

- [ ] **Step 1: Write the failing test** in `tests/canvas_hit.rs`

```rust
#[test]
fn a_search_finds_every_word_on_the_board_and_names_the_match() {
    let mut a = Board {
        texts: vec![text(20.0, 80.0, "buy Milk and eggs"), text(20.0, 200.0, "nothing here")],
        notes: vec![Note::new(100.0, 20.0, PALETTE[1], "milk first".into())],
        ..Board::default()
    };
    let hits = a.search("MILK");
    // Texts come before notes, and only the elements that hold the word are listed.
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].item, ItemId::Text(0));
    assert_eq!(hits[1].item, ItemId::Note(0));
    // `at` and `len` name the matched characters of the preview, case-insensitively.
    let preview = &hits[0].preview;
    let at = hits[0].at;
    let len = hits[0].len;
    assert_eq!(len, 4);
    assert_eq!(&preview[char_bytes(preview, at)..char_bytes(preview, at + len)], "Milk");
    // A query that matches nothing, and the empty query, list nothing.
    assert!(a.search("yogurt").is_empty());
    assert!(a.search("").is_empty());
    // The preview stays short: a long line is clipped around the match with an ellipsis.
    a.texts[0].text = format!("{}milk{}", "word ".repeat(40), " tail".repeat(40));
    let hits = a.search("milk");
    assert!(hits[0].preview.chars().count() < 80, "{}", hits[0].preview);
    assert!(hits[0].preview.contains('…'));
}
```

Add a `char_bytes(s: &str, chars: usize) -> usize` helper beside the test if the file has none.

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test --test canvas_hit a_search_finds_every_word`
Expected: compile error, `no method named search`.

- [ ] **Step 3: Implement it** in `src/canvas.rs`

`search` walks `texts` then `notes`, keeps the first case-insensitive occurrence per element, and
builds the preview with a helper that keeps `PREVIEW_WORDS` words before and after, adding `…` where
it clipped. `at`/`len` are **characters**, not bytes, because the renderer draws by glyph and the
board holds CJK.

- [ ] **Step 4: Run the test**

Run: `cargo test --test canvas_hit a_search_finds_every_word`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
jj commit src/canvas.rs tests/canvas_hit.rs -m "feat: search a board's words and name the match"
```

---

### Task 2: The rail carries a search

**Files:**
- Modify: `src/canvas.rs` (`ToolbarAction`, `Toolbar`, new `Search`/`SearchHit` re-export)
- Modify: `src/render.rs` (`key_of` `:277`, `badge_of`, `same_layout` `:5173`, `DRAWER` `:860`, `drawer_blocks` `:940`)
- Modify: `src/app.rs` (`current_toolbar` `:966`)
- Test: `src/render.rs` `mod tests`, `tests/render.rs`, `tests/config.rs`, `benches/render.rs`

**Interfaces:**
- Consumes: `SearchHit` from Task 1, `TextBuffer`.
- Produces:
  - `pub struct Search { pub query: TextBuffer, pub preedit: String, pub caret: bool, pub hits: Vec<SearchHit>, pub focus: usize, pub scroll: usize }`
  - `ToolbarAction::Search`, `ToolbarAction::SearchPick(usize)`
  - `Toolbar.search: Option<Search>`
  - `key_of(Search) == Some("/")`, `key_of(SearchPick(_)) == None`

- [ ] **Step 1: Write the failing test** in `src/render.rs`'s `mod tests`

```rust
#[test]
fn the_drawer_holds_the_search_and_states_its_key() {
    let mut rail = rail_for_legend();
    rail.more = true;
    let rows = toolbar_blocks(&rail, 1000.0);
    let row = rows
        .iter()
        .find(|block| block.action == Some(ToolbarAction::Search))
        .expect("the drawer has no search row");
    assert!(row.label.ends_with("(/)"), "{}", row.label);
    assert_eq!(badge_of(ToolbarAction::Search), Some("/"));
    // The panel is the rail's rows and one search card, so a query change must miss the layout cache.
    let mut typed = rail.clone();
    typed.search = Some(Search {
        query: TextBuffer::new("milk"),
        ..Default::default()
    });
    assert!(!same_layout(&rail, &typed), "a typed query reuses the old layout");
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test --lib the_drawer_holds_the_search`
Expected: compile error — no `Search`, no `ToolbarAction::Search`.

- [ ] **Step 3: Add the state and the action**

`Search` derives `Debug, Clone, Default, PartialEq` (`TextBuffer` and `SearchHit` already do). Add
`Search` to the end of `DRAWER` and an arm to `drawer_blocks` (mark `Icon::Find`, label
`find text`, gap `TOOLBAR_GAP`), the `key_of`/`badge_of` arms, `search` in `same_layout`, and
`search: self.search` in `current_toolbar`. Add `Icon::Find` and its geometry next to the other rail
icons (a magnifier: a circle and a short handle).

- [ ] **Step 4: Fix every `Toolbar` literal**

`tests/render.rs` (three sites), `tests/config.rs`, `benches/render.rs`, `src/render.rs`'s
`rail_for_legend` all gain `search: None`. Run `cargo build --all-targets`.

- [ ] **Step 5: Run the tests**

Run: `cargo test --lib the_drawer_holds_the_search && cargo test`
Expected: PASS, with the known preedit baseline failure as the only failure.

- [ ] **Step 6: Commit**

```bash
jj commit src/canvas.rs src/render.rs src/app.rs tests/render.rs tests/config.rs benches/render.rs -m "feat: hold the search on the rail"
```

---

### Task 3: The card — a field and one row per hit

**Files:**
- Modify: `src/render.rs` (`open_panel` `:880`, new `search_rows`, `Mark` `:142`, the row drawing `:3140-3280`, `row_label` `:1053`)
- Test: `src/render.rs` `mod tests`

**Interfaces:**
- Consumes: `Toolbar.search`, `SearchHit`.
- Produces: `Mark::Hit(Vec<(String, bool)>)` where `true` marks the matched run;
  `fn search_rows(toolbar: &Toolbar) -> Vec<Block>`.

- [ ] **Step 1: Write the failing tests** in `src/render.rs`'s `mod tests`

```rust
#[test]
fn the_search_card_is_a_field_and_one_row_per_hit() {
    let mut rail = rail_for_legend();
    rail.search = Some(Search {
        query: TextBuffer::new("milk"),
        hits: vec![hit(ItemId::Text(0), "buy Milk and eggs", 4, 4)],
        ..Default::default()
    });
    let rows = search_rows(&rail);
    assert_eq!(rows.len(), 2, "a field and one hit");
    assert_eq!(rows[0].action, Some(ToolbarAction::Search), "the field is the block");
    assert_eq!(rows[0].label, "milk");
    assert_eq!(rows[1].action, Some(ToolbarAction::SearchPick(0)));
    assert_eq!(
        rows[1].mark,
        Mark::Hit(vec![
            ("buy ".into(), false),
            ("Milk".into(), true),
            (" and eggs".into(), false),
        ])
    );
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test --lib the_search_card_is_a_field`
Expected: compile error — no `search_rows`, no `Mark::Hit`.

- [ ] **Step 3: Build the rows**

`open_panel` gains `else if toolbar.search.is_some() { Some((ToolbarAction::Search, search_rows(toolbar))) }`
before the `more` branch — the search lives inside the drawer, so `more` may be open behind it; the
search wins, and closing it returns to the drawer. Row 0 is the field (`Mark::None`, label = query,
action `Search`); rows 1..n are hits with `Mark::Hit` and action `SearchPick(i)`, the focused one
`active: true`. `row_label` returns the query for the field row so its caret is measured on the typed
text.

- [ ] **Step 4: Draw them**

The generic label drawing skips a `Mark::Hit` row; that row draws its runs left to right with the
capsule's own `Reveal`, `TOOLBAR_LABEL_TEXT` for the plain runs and `TOOLBAR_ACCENT` for the matched
one. The field row gets the caret and the preedit exactly as the rename row does, keyed on
`control.block.action == Some(ToolbarAction::Search)`.

- [ ] **Step 5: Run the tests**

Run: `cargo test --lib the_search_card && cargo test`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
jj commit src/render.rs -m "feat: draw the search card's field and its highlighted hits"
```

---

### Task 4: `/`, typing, and the pick

**Files:**
- Modify: `src/input.rs` (new `search_key` beside `panel_action_for_keysym` `:263`, `panel_action_for_keysym`, `press_key` `:424`, `edit_target`/`edit_target_mut` `:889`)
- Modify: `src/app.rs` (`search: Option<Search>`, `open_search`, `close_search`, `refresh_search`, `search_pick`, `edit_buffer`'s other ends, `current_toolbar`, `switch_board`, `doc_changed`/`doc_changed_at`)
- Test: `tests/keys.rs`

**Interfaces:**
- Consumes: `Board::search`, `Toolbar.search`, `SearchHit`, `View::centred_on`.
- Produces:
  - `pub enum SearchKey { Up, Down, Pick, Close }`, `pub fn search_key(keysym: Keysym) -> Option<SearchKey>`
  - `App::{open_search, close_search, refresh_search, search_pick}`

- [ ] **Step 1: Write the failing test** in `tests/keys.rs`

```rust
#[test]
fn the_search_takes_its_own_keys_and_slash_opens_it() {
    assert_eq!(panel_action_for_keysym(Keysym::slash, false), Some(ToolbarAction::Search));
    assert_eq!(search_key(Keysym::Up), Some(SearchKey::Up));
    assert_eq!(search_key(Keysym::Down), Some(SearchKey::Down));
    assert_eq!(search_key(Keysym::Return), Some(SearchKey::Pick));
    assert_eq!(search_key(Keysym::Escape), Some(SearchKey::Close));
    // Everything else is the field's text, not a command.
    assert_eq!(search_key(Keysym::a), None);
    assert_eq!(search_key(Keysym::slash), None);
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test --test keys the_search_takes_its_own_keys`
Expected: compile error — no `SearchKey`.

- [ ] **Step 3: Route the keys**

`panel_action_for_keysym` gains `Keysym::slash => Some(ToolbarAction::Search)`. In `press_key`, after
the AI block and **before** the text-edit block (the field is an edit target, so it would otherwise
swallow these): if a search is open, `search_key` decides `↑`/`↓`/`Enter`/`Esc`, a live preedit owns
everything else, and the rest goes through `edit_action`/`edit_buffer` into the query. `/` inside the
open panel types a slash into the query, because the field is a text field.

- [ ] **Step 4: Wire the app**

`open_search` (also the `Search` action): builds a fresh `Search`, focuses it, computes hits for the
current board and syncs the text input. `refresh_search` re-runs `Board::search` and clamps `focus`.
`edit_buffer` already writes through `edit_target_mut`, so the search buffer is appended there (first
in the chain, before rename). Every document change (`doc_changed`, `doc_changed_at`) recomputes the
hits while the panel is open, because `SearchHit.item` is an index and a deletion shifts it.
`switch_board` closes the panel. `search_pick(i)` re-runs the search, takes the fresh hit at `i`,
does nothing when it is gone, and otherwise arms the selection tool, sets the selection, centres the
view with `View::centred_on(output size, element centre)` and closes the panel but keeps the query.
The caret blinks: add the search's `caret` to the same blink `tick` the rename row uses.

- [ ] **Step 5: Run the tests**

Run: `cargo test --test keys && cargo test`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
jj commit src/input.rs src/app.rs tests/keys.rs -m "feat: open the search on slash and pick a hit"
```

---

### Task 5: An empty press clears the selection

**Files:**
- Modify: `src/app.rs` (`select_press`, `Press::Marquee` arm `:3113`)
- Test: `scripts/smoke.sh`

**Interfaces:**
- Consumes: `App::set_selection`.
- Produces: no new API; the `Press::Marquee` arm clears first.

- [ ] **Step 1: Make the change**

In the `Press::Marquee { canvas }` arm, call `self.set_selection(key, Vec::new(), None)` before
storing the gesture. `Press::Inert` (a `Shift` press on empty space) keeps doing nothing.

- [ ] **Step 2: Prove it on a real pointer**

Add a stage to `scripts/smoke.sh` after the note stage: arm the selection tool, click the note, then
click empty canvas, and assert the note is no longer selected — the selection action card leaves
exactly one frame of evidence (`edit` is live only for one line, so use the card's bounds and a
screenshot, or the `log` line the selection writes; use whichever the earlier stages already read).

- [ ] **Step 3: Run the gate**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: no warnings, the known preedit baseline failure, everything else green.

- [ ] **Step 4: Run the smoke**

Run: `scripts/smoke.sh`
Expected: `PASS`, with the new stage's line in the output.

- [ ] **Step 5: Commit**

```bash
jj commit src/app.rs scripts/smoke.sh -m "feat: an empty press drops the selection"
```

---

### Task 6: Say it in the README

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Update the sentences that are now wrong**

The shortcut table gains `/` (find text, in the `more tools` drawer: type to filter, `↑`/`↓` to move,
`Enter` to jump to the element with the selection tool armed and the view centred, `Esc` to close).
The selection paragraph gains the empty press as the ordinary way out of a selection, leaving `Esc` as
the keyboard's. The rail's paragraph names the new drawer row.

- [ ] **Step 2: Check the table still matches the code**

Run: `cargo test --lib every_block_states_the_key_that_applies_it && cargo test --test keys`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
jj commit README.md -m "docs: state the search and the empty press"
```
