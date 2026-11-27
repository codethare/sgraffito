# Fill colour picker on the rail — design

## Intent

Today the inside of a circle, rectangle or diamond can only be coloured one shape at a time, by
picking the bucket out of the drawer and clicking the shape, and the colour it uses is the *stroke*
colour in hand. There is no way to say "the next shape I draw is filled", and no way to take a fill
back off.

The rail gets a second swatch beside the stroke swatch: a fill colour in hand. Picking a colour
there does two things, exactly as picking a stroke colour does — it arms the next shape and it
recolours the closed shapes in the current selection — and it can also pick **no fill**.

## Goals

- A fill colour in hand: new circles, rectangles and diamonds carry it; lines and arrows never do.
- Picking a fill colour with a selection held recolours the closed shapes the selection holds, as
  one undo step.
- "No fill" is a choice on the same strip, and it is the default.
- The bucket paints with the fill colour in hand, and clears the shape when the hand holds no fill.
- Works on all four rail edges, including the bottom one.

## Non-goals

- Fill styles other than solid (hachure, cross-hatch, zigzag). sgraffito draws one style.
- A per-output fill colour, or persisting it across restarts: the stroke colour is not persisted
  either, and the rail is one state per run.
- Filling open paths whose endpoints meet, as Excalidraw does for a looped line. A line is still a
  line.
- Any keyboard shortcut for fill colours.

## Anti-goals

- A second notion of "fill colour" living in the bucket tool while the rail shows a different one.
- Filling a line or arrow because it was in the selection.
- A strip that opens but cannot be reached, or one that stays open while the pointer is elsewhere.

## Excalidraw deconstruction

Reference: `/tmp/excalidraw-src` (sparse checkout; `packages/excalidraw/actions/` and
`packages/common/` are not present, so the picker's own action file could not be read — the
behaviour below comes from the sites that are there).

- `packages/element/src/shape.ts:236-258` — `backgroundColor` is stored per element; when it is
  `"transparent"` the rough-options object gets *no* `fill` at all, so "no fill" is absence, not a
  colour with zero alpha.
- `packages/excalidraw/components/Actions.tsx:645-655` — background is its own picker beside the
  stroke picker. With the bucket armed it renders `changeBucketFillBackgroundColor` instead of
  `changeBackgroundColor`; the bucket variant's palette excludes `transparent`.
- `packages/excalidraw/components/App.bucketFill.ts:129-160` — a fill paints with
  `currentItemBackgroundColor`; clicking an already-filled region restyles it rather than stacking
  a second element, and a no-op restyle returns early.

Observed behaviour: stroke colour and background colour are two independent states, each "in hand
plus applied to the selection". Background includes a transparent entry. Number keys bind stroke
colours only; background has no shortcut. Fills are stored per element, so undo, duplicate and
persistence come for free.

Data model: `backgroundColor` is one string on the element (`"transparent"` is the no-fill value);
the in-hand colour is `appState.currentItemBackgroundColor`.

Rendering: the fill is painted on the exact centreline path *before* the hand-drawn outline, so a
wobbling border cannot spill outside the shape.

Undo/selection/persistence: picking a background colour with a selection is one history entry and
restyles every eligible element in it; the in-hand colour is not part of the document.

sgraffito mapping: `Shape.fill: Option<String>` already holds exactly this, `None` being the
transparent case, and `render.rs` already fills the exact interior before sketching the outline
(`closed_shape_path`, ~`:6670`). No schema change, no version bump. What is new is rail state
(`fill_idx`, a second swatch, a second strip), one toolbar action, and the two places that write
`fill` (shape creation, and the bucket, which today reads the stroke colour).

Deliberate deviations:

- The bucket with **no fill** in hand clears the shape. Excalidraw's bucket palette hides
  transparent, so its bucket cannot erase. sgraffito has no other way to take a fill off, and one
  rule ("the bucket paints the fill in hand, and the fill in hand can be nothing") beats a hidden
  exception. Documented in the README, because it changes what `B` does by default.
- No fill-style picker (one style only), and no fill for looped lines or arrows.

## Approaches considered

- **A. Two swatches, one strip each** (chosen). The fill swatch sits after the stroke swatch in the
  palette group; each opens its own strip of the choices it is not currently showing; only one
  strip is open at a time.
- **B. One swatch plus a stroke/fill target toggle.** Cheaper on rail room, but the user has to
  remember which target the chips and the number keys act on, and the two colours can never be seen
  together.
- **C. One strip with two rows**, stroke above fill. Everything visible at once, but the strip's
  geometry is one row and one card today; a second row changes the plate maths on all four edges
  for no gain over A.

A costs one block on the rail. On a 600-pixel-tall output the rail already folds note, hand and
arrow into the drawer; the palette group is first, so the extra block costs one more tail tool, not
the palette.

## The design

### The two swatches

`rail_candidates` puts the stroke swatch first and the fill swatch second, both in column 0:

| Block | Mark | Label | Action |
| --- | --- | --- | --- |
| stroke | `Mark::Swatch(active)` | `current colour` | `ToolbarAction::Color(active)` |
| fill | `Mark::Fill(fill)` | `fill colour` / `no fill` | `ToolbarAction::FillColor(fill)` |

The stroke swatch keeps its `TOOLBAR_GROUP_GAP`; both are separated from the tools by one group gap.

`Mark::Fill(Option<usize>)` draws the palette colour inside the same rounded square the stroke
swatch uses (same edge, same accent edge when active). `None` draws the square in
`TOOLBAR_BLOCK_FILL` with the hairline `TOOLBAR_SWATCH_EDGE` edge and a diagonal from its top-left
corner to its bottom-right in `TOOLBAR_LABEL_TEXT` — a crossed-out square, the way a colourless
choice reads without a checkerboard.

Neither swatch carries a key badge: fill has no shortcut, and `badge_of` returns `None` for
`FillColor`.

### One strip at a time

`Toolbar.colors_open: bool` becomes `Toolbar.open: Option<Palette>`, with
`enum Palette { Stroke, Fill }`. The app's `colors_open`/`colors_since`/`colors_until` become
`open_palette`/`palette_since`/`palette_until`; `colors_until` is still only set by
`reveal_colors`, which is still only the stroke shortcut's dwell.

`palette_strip(pill, toolbar, origin, position) -> Option<PaletteStrip>` replaces `color_strip`. It
returns which palette is open, the plate, and the chips as `(Option<usize>, Rect)`:

- `Palette::Stroke`: 4 chips, the palette indices other than the active one.
- `Palette::Fill`: 5 chips — the palette indices other than the active fill, plus `None` when the
  active fill is a colour. `None` is always the last chip, drawn as the crossed-out square described
  above.

The plate is anchored to the swatch of the palette that is open and is `PLATE(count)` wide for that
count, so the fill strip is one chip wider than the stroke strip.

Hit test: a chip under the pointer is `Color(i)` or `FillColor(i)`; `FillColor(None)` for the last
fill chip. `toolbar_over_palette` replaces `toolbar_over_colors` and reports the plate of whichever
strip is open, so sweeping between chips keeps it open exactly as today.

### What a pick does

`App::set_fill(fill: Option<usize>)`:

1. sets `fill_idx`, refreshes the rail;
2. collects each output's selection the way `set_color` does, and calls
   `Annotation::recolour_fill(&items, fill)` on the matching board;
3. records one undo entry if anything changed and damages the union box.

`Annotation::recolour_fill(&mut self, items: &[ItemId], fill: Option<&str>) -> Option<Rect>` walks
`ItemId::Shape(i)` only, skips a shape whose `kind.is_closed()` is false, sets `fill`, and unions
the paint bounds. Strokes, text and notes are untouched.

### Where the fill in hand is used

- `begin_shape`: `fill` is `self.fill_idx` mapped through the palette for a closed kind, and `None`
  for a line or an arrow.
- `bucket_fill`: the colour comes from `fill_idx`, not from `color_idx`; an unchanged fill returns
  early exactly as today, so a repeated click is not a second undo step.
- Nothing else: the AI pipeline, paste and duplicate keep writing what the document says.

### Persistence and history

`Shape.fill` is already serialized per shape at `VERSION = 6` with `skip_serializing_if`, so a
board written by this build is still readable by a build without the picker. Nothing is added to
the config file; the fill in hand is rail state like the tool.

## Decisions left to the builder

- The exact diagonal of the "no fill" glyph and its inset.
- Whether the fill strip's last chip gets a hairline separating it from the colours (cosmetic; no
  behaviour depends on it).

## Verification

- `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.
- `tests/render.rs`: the rail carries seventeen blocks with two swatches in the palette group; a
  fill pick opens the fill strip with five chips; opening the fill strip closes the stroke strip;
  the bottom rail's fill strip sits above the rail and inside the output.
- `tests/canvas_hit.rs`: a rectangle drawn with a fill colour is painted at its centre and the
  stroke's colour is still the stroke's; picking a fill colour with a rectangle and a line selected
  fills only the rectangle; the bucket with no fill in hand clears a filled shape.
- `tests/undo.rs`: picking a fill colour with a selection is one undo step, and undoing it restores
  the previous fill exactly.
- `tests/store.rs`: a board carrying `fill` round-trips unchanged; a shape with no fill writes no
  `fill` key.
- `scripts/smoke.sh` (headless sway): pick the fill swatch, pick a colour, draw a rectangle, read
  the board JSON for `fill`, then pick `no fill` and bucket-click it away.
