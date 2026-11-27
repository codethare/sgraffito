# Clear-on-empty-press and a keyword search — design

## Intent

Two changes to how a drawing is picked and found.

1. With the selection tool armed, a press on empty canvas clears what is selected. Today the
   selection survives that press: only `Esc` lets go of it, and a marquee only decides a selection
   when it is released *after moving*. So the one thing a user tries first — click the wallpaper —
   does nothing.
2. A keyword search over the board's words, opened by `/`, living in the `more tools` drawer beside
   the other rarely-used controls. Typing filters, the matched characters are highlighted, and
   picking a result puts you on the element.

## Goals

- A press on empty canvas with the selection tool armed drops the selection at once, before any
  marquee.
- `/` opens a search card in the drawer's column; typing narrows the list; matched characters are
  drawn in the accent colour; `↑`/`↓` move the mark, `Enter` or a click picks, `Esc` closes.
- Picking a result arms the selection tool, selects that element and centres the view on it without
  changing the zoom.

## Non-goals

- Searching other boards. The panel searches the board the output shows, the way `C` lists the
  document's boards but a pick shows one.
- Fuzzy matching (subsequence, typo tolerance). The match is a case-insensitive substring, and the
  highlight covers one contiguous run, which is what makes "the matched characters" a thing you can
  point at.
- Highlighting the match on the canvas. The canvas shows the element selected, as any pick does;
  painting the query inside a text box would mean splitting the drawn text into runs for a state
  that lasts until the next click.
- Searching shapes: a shape carries no text of its own. Its label is a text element and is searched
  like any other.
- Renaming, replacing or jumping between matches inside a text box.

## Excalidraw deconstruction

Reference: `/tmp/excalidraw-src` (sparse checkout).

**Deselecting on empty canvas.** `packages/excalidraw/components/App.selectionTool.ts` — the
selection tool's `onPointerDown` resets `selectedElementIds` before it decides between a box
selection, a handle drag or nothing; the marquee that follows only ever *adds*. So the empty press
is what clears, and the drag is what re-selects.

**Search.** `packages/excalidraw/components/SearchMenu.tsx`:

- It searches **text elements** and frame names: `elements.filter(isTextElement)` and
  `elements.filter(isFrameLikeElement)` (`handleSearch`, :803-813), sorted by `y` so the list reads
  top to bottom.
- Matching is a **case-insensitive regular expression** built from the escaped query
  (`new RegExp(escapeSpecialCharacters(searchQuery), "gi")`, :817), and a text with three hits
  produces three results, each with its own `index` (:819-842).
- Each row shows a **preview window** around the match (whole words before and after,
  `getMatchPreview`, :560-600) and draws the matched run in a different colour (`matchedText`,
  :749-767).
- `↑`/`↓` move a focus index; a click or `Enter` picks it (`onItemClick`, :508); the panel is a
  debounced (350 ms) search of the live scene.
- Picking scrolls the element into view and selects it (the scroll is the app's
  `scrollToContent`, outside this component).

**Mapping to sgraffito.** The words on a board are `TextItem`s (a shape's label is a text whose
`container` names the shape) and `Note`s; together they are what a search can read. So:
`Board::search(query)` in `src/canvas.rs` returns hits in canvas order, `Toolbar` carries the search
state, the panel is one more branch of `open_panel`, and the jump reuses `View::centred_on`, which
already exists for following a link.

**Deliberate deviations.** One row per element with its first match, not one per occurrence: the
panel is a fixed-height window beside the rail, and a note with the same word three times would
spend three of its six rows on one note. The view is centred with the zoom untouched, which is what
a link does here; a jump that also zoomed would be a second, unrelated bet.

## Approaches considered

- **A. Search in the drawer (chosen).** A `search` row in `more tools`, `/` to open, its card in the
  drawer's column above/beside the rail — the same machinery as the boards list and the width list.
  One new panel kind, no new surface.
- **B. A floating search box over the canvas** (Excalidraw's own shape, a bar under the top-left
  corner). More visible, but it is a second floating surface with its own position, damage box,
  IME focus rule and set of edge cases, for a control that is used rarely.
- **C. Search folded into the boards panel**, as a filter row over the board names. Cheapest, but it
  answers a different question (which board) than the one asked (which words).

## The design

### 1. Cleared by an empty press

In `App::select_press`, the `Press::Marquee { canvas }` arm drops the selection before it starts the
gesture: `self.set_selection(key, Vec::new(), None)`. The pure decision
(`canvas::selection_press_linear`) is untouched, so the press still resolves to `Marquee`, and a
drag after the press selects what it sweeps. `Shift` on empty space stays `Press::Inert` — a
modified press never changes the selection in Excalidraw either, and a `Shift` marquee was never a
thing here.

Consequence to state in the README: `Esc` is no longer the only way out of a selection.

### 2. The search panel

**State** (`src/canvas.rs`, on `Toolbar`, so the layout, the hit test and the drawing read one
value):

```rust
pub struct Search {
    pub query: TextBuffer,      // the field's text and caret, the same buffer a text box uses
    pub preedit: String,        // input-method preedit, never persisted
    pub caret: bool,            // the blink phase, as the rename row has
    pub hits: Vec<SearchHit>,   // one per element, in canvas order
    pub focus: usize,           // the row `Enter` would pick
    pub scroll: usize,          // the window's first row, like the boards list
}

pub struct SearchHit {
    pub item: ItemId,
    /// The line shown: `PREVIEW_BEFORE` words of lead-in, the match, `PREVIEW_AFTER` of follow-on.
    pub preview: String,
    /// The matched run, in characters of `preview`.
    pub at: usize,
    pub len: usize,
}
```

`Board::search(&self, query: &str) -> Vec<SearchHit>` walks `texts` then `notes` in canvas order,
finds the first case-insensitive occurrence in each, and builds the preview. Pure, so it is tested
without a document.

**Rows** (`src/render.rs`): `open_panel` gains a fourth branch, `toolbar.search_open()` →
`search_rows(toolbar)`:

- row 0 is the field: label = the query, with the caret drawn where the rename row draws its own;
- rows 1..n are the hits: a new `Mark::Hit(Vec<(String, bool)>)` — runs of text, `true` for the
  matched characters — drawn in the row's own text colour and the accent, clipped to the row;
- a hit row is accented while it is the focused one, the way a board row is when it is the one on
  screen.

**Actions**: `ToolbarAction::Search` (the drawer row and `/`: open, close, or re-focus) and
`ToolbarAction::SearchPick(usize)` (a hit row, by index).

**Keys**, in `press_key`, before the tool keys and only while the search panel is open:

| Key | Does |
| --- | --- |
| printable / paste | into the query, which re-runs the search |
| `Backspace` | one character back |
| `↑` / `↓` | move the focus, scrolling the window |
| `Enter` | pick the focused row |
| `Esc` | close the panel |
| `/` | close the panel (it opened it) |

While the panel is open the field is the text sink, so the input method, the preedit and `Ctrl+V`
work through the same `edit_target`/`edit_target_mut` path the boards rename row uses. A text box
being edited keeps its keys: the panel is only open when no text box is.

**Picking**: `App::search_pick(index)` arms the selection tool, sets the selection to the hit's
`ItemId`, moves the view with `View::centred_on(size, element.centre())`, closes the panel and keeps
the query, so the same search can be walked again. `Ctrl+Z` is unaffected: nothing in the document
changes.

**Where it lives**: the drawer row (`DRAWER`) beside `link text to board`, labelled `find text (/)`.
`/` arms it exactly as the row does, which is the rule every drawer control already follows.

## Decisions left to the builder

- The preview's word budget (start: two words before, two after, ellipsis when clipped).
- Whether a hit row shows the element's kind at its right end (text or note). Start without: the
  preview's own words are enough to tell them apart.
- Whether `Enter` on an empty query does nothing (it does) or closes the panel.

## Verification

- `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.
- `tests/canvas_hit.rs`: `Board::search` — case-insensitive, no match, a match at the very start and
  the very end, a note and a text box in the same result list in canvas order, the preview's
  `at`/`len` naming the matched characters, an empty query returning nothing.
- `src/render.rs` tests: the drawer holds a `search` row; `/`-opened rows are a field then one row
  per hit; the focused row is accented; the `Mark::Hit` runs paint the matched characters in the
  accent (pixel sample).
- `tests/render.rs` / `tests/keys.rs`: `/` opens the panel and is otherwise unbound; `Esc` closes it;
  `↑`/`↓` move the focus and wrap; `Enter` on an empty query does nothing.
- `scripts/smoke.sh`: with the selection tool, click a note, click empty canvas, assert the element
  is no longer selected (the selection card is gone); then `/`, type a word from the seeded note,
  `Enter`, and assert the view centred on the note (the note's canvas point lands at the output's
  centre) and the selection card returns.
- `README.md`: the shortcut table gains `/` and the search row; the sentence that says `Esc` is the
  only way out of a selection is corrected.
