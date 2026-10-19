# sgraffito — session handoff

Working notes from the sessions that built the selection tool, the shape tools and the rail's icons
and plate. `CLAUDE.md` is the behaviour contract and the source of truth; this file is the state left
behind, so a new session can start without re-deriving it. English, like the rest of the repository;
the user writes in Chinese and expects Chinese in chat, English in the artifacts.

## State right now (2026-10-14)

- The tree is clean and nothing is in flight: `@` is an empty working-copy commit on top of the
  delete-and-plate work. `jj log -r 'main@origin..main'` is what says whether anything is left to
  push; the human pushes, this file does not track that. The human also committed their own work
  mid-session (`853567b4 Add crates.io package metadata`), so `main@origin` may be ahead of what the
  session notes describe — read the log, not this file, for that.
- `de5533af` carries the selection work plus two edits that were already in flight when that session
  started and belong to the one before it: `HANDOFF.md` (these notes) and a `.gitignore` tidy that
  drops `CLAUDE.md` and `openspec/` from the repository's list because `.git/info/exclude` already
  ignores both locally.
- What landed in this session, on top of the previous one:
  - the selection tool: a press picks an element or sweeps a marquee, dragging moves the set, the
    eight frame handles resize it and the handle above the top edge rotates it. A gesture is written
    into the geometry on release; the document is untouched while it is live.
  - five shape tools: circle, rectangle, diamond, line and arrow, dragged out and stored as two
    points and a kind. The annotation file is version 2 for them, and version 1 files still load.
  - every rail block shows an icon of what it does, drawn as geometry by the same code the shapes are
    drawn by, and the key of a block is stated at the end of the text it says when the pointer is on
    it (`circle (O)`, `colour 3 (3)`, `cancel, then lock (Esc)`).
  - `Delete` removes everything the selection holds as one change, and the rail sits on a black
    plate at 90% opacity.
  - edit mode is painted on a **scrim**: black at 90% opacity over the whole output, under the
    annotations and the rail, so the windows behind the surface are dimmed. It is the requirement
    the plate's earlier change had read more narrowly.
  - the canvas pans and zooms: an unbounded plane, a per-output `View`, two-finger swipe to pan,
    pinch or wheel to zoom, the middle button and a new hand tool (`H`) to pan, and `Delete` already
    removing a selection.
  - the plans and the decisions are in `openspec/changes/selection-and-transform/`,
    `openspec/changes/shape-tools/`, `openspec/changes/toolbar-icons/`,
    `openspec/changes/delete-key-and-rail-plate/`, `openspec/changes/edit-mode-scrim/` and
    `openspec/changes/canvas-pan-and-zoom/` (all four artifacts complete in each, 30/30, 28/28,
    14/14, 12/12, 11/11 and 27/27 tasks ticked, all six `openspec validate <change> --strict`
    valid).
- `openspec/specs/` holds only `.gitkeep`, so every change dir under `openspec/changes/` stays
  unarchived and `openspec validate <change> --strict` always prints
  `INFO ... only ADDED requirements are allowed for new specs`. That is expected here, not a
  failure; only the `Change '<name>' is valid` line matters.
- `openspec/` and `CLAUDE.md` are gitignored on purpose; the specs and the contract live
  outside the repository.
- `cargo test` is 144 tests across the lib plus six files in `tests/`.

## What the last sessions changed and why, oldest first

1. **The rail** (`render.rs`). Edit mode shows a vertical rail of 12 blocks against the left
   edge, its centre at 0.45 of the output height, which is the *optical* centre (roughly 5 %
   above the geometric one). Top to bottom: five palette swatches, `P`, `E`, `T`, `[`, the size
   readout, `]`, `Esc`. Placement evidence is in
   `openspec/changes/interactive-edit-toolbar/design.md` (D1): *Click This!* (HFES 2010) measured
   fastest responses with a toolbar on the sides, slowest at the bottom, and worse when the
   position moved; *UEyes* (CHI 2023) finds UI fixations concentrated in the upper-left; Fitts's
   law makes every block an unbounded target against the screen edge. Apple's "the toolbar
   resides at the top of the window" describes a window with a title bar and a menu bar, which
   this full-screen overlay has not got, so only the HIG's metrics were taken from it.
2. **Hover** — the block under the pointer stretches away from the edge into a capsule whose
   secondary text says what it does. Only the pointer's block moves; the layout cache keys on
   everything *except* the transition progress (`same_layout`), so a frame of the pop-out is a
   localized repaint of the rail's box. The block's hit rectangle is its fully opened width from
   the first frame, so the pointer cannot lose the block it is on.
3. **Motion** (`app.rs`). 140 ms, ease-out, opening only: leaving a block collapses it in the
   same frame. `App::tick` returns the wait the loop should use (16 ms while opening, 200 ms
   idle), which is how the animation gets frame cadence without a timer source. `hover_grow` and
   `ease_out` are pure and tested.
4. **Metrics and contrast** (`CLAUDE.md` §UI aesthetics, `render.rs`). 13 pt control text for a
   glyph and its label, a 30 pt block, `TOOLBAR_LABEL_GAP` 6 between a block and its label,
   `TOOLBAR_LABEL_PAD` 12 after it, 8 pt radius, a 1 pt hairline, 4 pt inside a group and 10 pt
   between groups, accent `#0a84ff`, fill `#1c1c1ce6`. The fill's 90 % opacity is not taste: at
   the previous 78 % a label measured 3.8:1 over a white wallpaper, below AA, so
   `a_label_keeps_aa_contrast_over_any_wallpaper` in `tests/render.rs` now holds the floor and
   the rule is "darken the fill, never lighten it".
5. **Text tool, three bugs the user reported**:
   - The caret was a count that only ever grew: `insert` appended, the caret was drawn at the
     last line's `line_w`, and `delete_before` truncated the text, which is only right when the
     caret is at the end. Now the buffer inserts at the caret, `move_cursor` handles
     left/right/up/down (crossing line ends, clamping the column), `delete_before` removes the
     range before the caret, and `caret_geometry` in `render.rs` places the caret and the preedit
     from the shaped line the caret is on. The preedit is spliced at the caret's byte offset with
     the caret *after* it, which is where an IME grows its composition.
   - Clicking a committed box mostly failed, because the click was tested against
     `TextItem::bounds()` — the eraser's estimate of 0.6 em per character, and CJK is one em, so
     a click on the right part of a glyph fell outside and started a new empty box. The click now
     uses `Renderer::text_edit_bounds`, the measured region the frame paints.
   - There was no clipboard path: no `wl_data_device`, no modifier state. Now
     `wl_data_device_manager` is bound, the seat's `DataDevice` and the current `SelectionOffer`
     are kept, `Ctrl` is tracked, and `Ctrl`+`V` reads the best plain-text mime on a worker
     thread (256 KiB cap, CR dropped) which pings the loop; `tick` inserts it through the same
     path a local key takes.

6. **Frame cost** (`rkqxvxp`, `openspec/changes/idle-and-frame-cost/`). Before this the daemon
   spent 0.85% of a core doing nothing and every non-drag frame rasterised the whole document.
   - The control thread polled `accept()` every 10 ms: 100 wakeups a second, 170 ms of processor
     time per 20 s whether or not an output, a document or a frame existed, and unmoved by the
     loop's own timer (verified by measuring at 200 ms and 2000 ms). It now blocks in `accept()`,
     and shutdown connects to its own socket to wake it. Idle cost: **10 ms per 20 s**.
   - `FrameCache` (per output) holds the shaped lines and `BaseLayer`: the committed document
     rasterised once into a pixmap. A frame fills the region it repaints from that raster and
     then draws the overlays, so it paints no document element at all.
   - The growth loop in the old `damage_box` is gone. It existed only because a bounding-box
     frame rasterised the elements intersecting the box, so the box had to cover them; a frame
     that paints none of them does not. The damage is replaced by `transient_damage(last, now,
     rail)`: the element that changed, never the document. That is what removed the cliff the
     README used to record (200 overlapping strokes, box 2005x1805: 167 ms against 163 ms).
   - The package's own damage is now inline in `App::draw_output` (transient, local text, or a
     known document-change box in `Output::pending`), and the two O(n) rescans per frame became
     one `memcpy` per row.
   - Measured at 4K: a drag 209 -> 78 us, a drag over 200 overlapping strokes 167 ms -> 77 us, a
     pop-out frame 8.1 -> 1.3 ms, a 1080p keystroke 3.1 ms -> 109 us. Recorded in the README.

7. **The selection tool** (`openspec/changes/selection-and-transform/`). The program could add and
   remove elements but not move them, so a mind map whose nodes shift had to be erased and redrawn
   one `E` press at a time. The agreed scope, from the user's own description of that pain: pick one
   node or sweep several, then move, resize or rotate them. Explicitly out of scope, because it is
   the expensive half: turning a **text box's** glyphs, which would need an angle per text item and
   therefore a version 2 file, plus a second rendering path.
   - The gesture is **arithmetic on the geometry that is already stored**, so the file schema, the
     file version and `src/store.rs` are untouched: a move offsets points and a text origin, a
     resize scales points about the opposite handle's anchor and rewrites a text box's `size`, and a
     rotation turns a stroke's points and moves a text box's origin. `Transform::stroke` and
     `Transform::text` are the only two functions, and the live preview and the release both call
     them, so what is drawn during a drag and what is written afterwards cannot disagree.
   - **A gesture never reaches the document.** The elements it holds are left out of the document
     raster — `BaseKey::skip_text` became `BaseKey::skip: Vec<ItemId>`, the same mechanism
     generalised — and the overlay draws them with the live transform, exactly as it already drew
     the text box under edit. That is what makes it safe that the debounced writer can fire
     mid-drag: the document still holds the stored geometry. The transform is written once, on
     release, through `doc_changed_at` with the union of the frame before and after.
   - `Esc` **cancels** a drag rather than committing it (a keystroke is a request, an interruption
     is not), a `clear` from the socket mid-drag drops the gesture instead of resurrecting what it
     removed, and a mode switch or tool change commits it. `Esc`'s stages are now cancel, finish the
     text edit, clear the selection, lock.
   - The frame is the axis-aligned box of what is held, recomputed every frame, with eight handles a
     capsule wide and a rotation handle on a stem above the top edge; a text box contributes its
     conservative box (not the eraser's narrow estimate, not the renderer's measurement, which lives
     behind `FontSystem`) so the choice stays in the pure layer.
   - The marquee takes only what it **wholly** contains, so a connector that merely clips the box is
     left alone, and it does not drop what is already held until it is released.
   - A press anywhere inside the frame drags the whole selection, the gaps between its strokes
     included, so a cluster is grabbable by its box and not only by one of its elements; only a press
     on empty space outside the frame starts a marquee. The user asked for this after using the first
     version, where only the elements themselves were grabbable.
   - The rail gained the `S` block (`TOOLBAR_COLUMN` is a hand-written sum with a debug assertion:
     thirteen blocks, nine gaps, three group gaps) and its `Esc` label became `cancel, then lock`.

8. **The five shape tools** (`openspec/changes/shape-tools/`). The program could draw freehand and
   place text but no shape, so a mind map's boxes and connectors had to be drawn by hand — and the
   pen's smoothing rounds a hand-drawn right angle on purpose, which is what made a rectangle look
   wrong. Circle, rectangle, diamond, line and arrow, keyed `O`, `R`, `D`, `L`, `A`.
   - **A shape is two points and a kind** (`Shape { kind, color, width, from, to }`). One gesture
     covers all five tools and no direction is lost, a shape costs two points however large it is
     drawn, and a resize or a rotation of one is the arithmetic the selection work already applies to
     a pair of points (`Transform::shape`). Nothing derived is stored: no samples, no box.
   - The gesture is the pen's, and the live shape lives in `Overlay::shape` beside the in-progress
     stroke, so it inherits the transient rule whole: the frames it is dragged through damage only
     its box, the release is one document change, and `Esc`, a mode switch, a tool change or a
     `clear` abandons it with nothing written. `commit_transient_strokes` became
     `commit_transient_drawing` and commits both kinds.
   - **Hit by the outline, never by the interior**, so a stroke drawn inside a rectangle keeps its
     own clicks. The hit test flattens the shape (`Shape::outline`, a 96-gon for an ellipse) because
     tiny-skia has no distance query, while the frame draws the real curve (`push_oval`,
     `push_rect`); the two differ by under a pixel against the 8-pixel tolerance, and the marker on
     `outline` says so.
   - A drag with no extent — a line whose ends meet, or a box flat in one direction — is dropped
     rather than written, so a click with a shape tool armed cannot leave an element that is
     invisible but still erasable.
   - `Shape::bounds` allows for the arrowhead's spread as well as the pen, because a head that pokes
     out of its own box would be clipped by the damage region and missed by the eraser.
   - The file went to **version 2** for the `shapes` list, with `load()` accepting 1 as well; an
     older build has no `shapes` field and would silently drop every shape on its next save, which is
     exactly what the version is for.
   - The rail is eighteen blocks now (about 638 logical pixels). `toolbar_origin` already pinned the
     top to the margin, so a short output loses the bottom of the rail; `scripts/smoke.sh` runs on an
     800x600 output, which is now shorter than the rail, and its top-strip check was updated to say
     that the rail reaches the margin and that nothing to the right of it does.

9. **Icons on the rail** (`openspec/changes/toolbar-icons/`). Eighteen blocks of key letters had become
   an arbitrary letter per shape: `O`, `D`, `L`, `A` say nothing about what they draw, so a new user
   had to learn five letters and a returning one had to hover to remember which was the diamond.
   - Every block that is a function now shows an **icon**, drawn as geometry by tiny-skia and never as
     a glyph: a symbol font is a coverage gamble that ends in a tofu box on a machine whose fonts
     differ from this one, and this program already draws its own geometry. The pen's icon is a
     stroke (drawn by `draw_stroke`), the eraser's a rounded square outline, the text tool's a `T`,
     the selection tool's a marquee with its corner grips, the size keys a `−` and a `+`, `Esc` a
     padlock.
   - The five shape tools' icons **are the shapes they draw**: the same two points, the same
     `draw_shape`, so an icon cannot drift from its tool.
   - `Block::glyph` + `Block::swatch` became one `Mark` (an icon, a colour, or a value): a face is
     exactly one of three things, and "what if both are set?" is no longer a question a caller has to
     answer. `TOOLBAR_KEY_TEXT` became `TOOLBAR_FOREGROUND`, which is what it now holds.
   - **The key moved into the stretched text.** `labelled(action, what)` builds every label, so a block
     cannot acquire an action whose text does not name its key, and `Tool::key()` is the one place a
     tool's letter lives. A test walks the rail asserting each label ends with its key *and* that the
     key really applies that action, through `tool_for_keysym` / `color_for_keysym` /
     `size_step_for_keysym` — the label cannot name a key the daemon does not handle.
   - The trade, agreed before it was built: with no key letter on the block face, a block whose text
     does not fit the reserve shows an icon and no key at all. Less bad than before, because the icon
     *is* the meaning; it is in the README's known limitations.
   - `ICON_STROKE` (1.5 pt, the preedit underline's weight) and `ICON_INSET` (7 pt) are named
     constants, because a hairline icon does not read as a picture at scale 1. A test renders the
     whole rail at scale 1 and at scale 2 and asserts every icon's square carries foreground ink.
   - The icons were also checked by eye, not only by that ink test: a scratch render of the rail at
     scale 2 — at rest, and with a tool, a colour and the lock block stretched in turn — confirmed the
     shapes read as what they are, that a colour chip keeps its ring inside the capsule, and that the
     longest label (`text: click to place (T)`, about 177 pt) fits the 224 pt reserve. The scratch
     test was deleted; re-deriving it means building an `Overlay { toolbar: Some(Toolbar { .. }) }` and
     writing a PPM, as the preview section below describes.

10. **Delete, and the rail's plate** (`openspec/changes/delete-key-and-rail-plate/`). Two small gaps:
    the selection could be moved but not thrown away, and the rail was eighteen translucent chips
    with the wallpaper showing through the gaps between them.
    - `Delete` removes every element the selection holds — strokes, text boxes and shapes alike — as
      **one** document change whose damage box is the union of what went, then clears the selection.
      The removal is `OutputAnnotations::remove`, which groups the ids by kind and removes from the
      **highest index down**: removing an earlier element shifts every later one, so ascending order
      would remove the wrong pair from a selection of two adjacent strokes. An index that is no
      longer there is skipped, which is what stops a `clear` arriving mid-gesture from resurrecting
      anything.
    - `Delete` and the keypad's `KP_Delete` only, via a pure `deletes_selection(keysym)` that
      `tests/keys.rs` pins. `Backspace` is deliberately not a second way in: inside a text box it
      deletes a character, and one key should not mean both. `Delete` is inert while a text box is
      focused (there is no forward-delete there, and that is recorded, not implemented).
    - A gesture still in flight is abandoned rather than written before its elements go: writing it
      would put a transform into the file that no frame ever showed, for elements that are leaving.
      `drop_selection` already clears the gesture, so this falls out of the order of the calls.
    - The plate is `TOOLBAR_PLATE_FILL` (`#000000e6`, the block fill's own 90%) painted as one rounded
      rectangle behind every block and before them. It is **the column's own extent**, which is
      already inset `TOOLBAR_PAD` from the blocks at both ends, plus `TOOLBAR_PAD` either side — so the
      plate is inset a pad from the blocks on all four sides, and it *is* the rectangle the hit test
      covers. Its width is the block column, never the reserve, so a capsule still reaches out over
      the wallpaper as its own block.
    - `toolbar_bounds` was the trap: the plate paints outside the old box, so the box had to grow or
      the plate's margin would be outside every rail-only frame's damage region and keep stale
      pixels. It is now the plate plus one hairline, and its height is `TOOLBAR_COLUMN + 2 *
      TOOLBAR_HAIRLINE` — **not** plus the pads, which the column already carries. Getting that wrong
      made the box 13 pt taller than anything painted, and four existing tests caught it.
    - Checked by eye as well as by test: the rail composited over a bright checkered wallpaper (a
      layer surface is transparent where nothing is drawn, so the preview blends the frame over the
      wallpaper by hand — a raw buffer holds the alpha and would show black otherwise), at rest, with
      a capsule open, and on a 400 px-tall output where the rail is pinned and clipped.

11. **The edit-mode scrim** (`openspec/changes/edit-mode-scrim/`). The user's original wording —
    "a black 90% transparent 底板" — had been read as a plate behind the rail; they clarified that what
    they wanted was a whole-output black sheet *under* the content, so that with other windows open the
    annotations are the thing to read. The plate stays (it still separates the rail from bright content
    drawn under it), and this change adds the sheet.
    - `SCRIM_FILL` (`#000000e6`) is painted as the **document raster's background**: `draw_document`
      fills the whole pixmap before drawing the document into it, and `BaseLayer::rasterise` no longer
      fills its own pixmap. One fill either way, from the one place that paints a background, and the
      same pixels whether the frame came from the raster or drew the document straight into the buffer.
    - That placement is what keeps the frame model intact: a damaged region filled from the raster
      carries the scrim, a whole-surface frame stays one row-wise copy, and a bounding-box frame stays
      pixel-identical to a whole-surface one. Painting the scrim per frame instead would have forced
      every frame to damage the whole surface.
    - `BaseKey` gained `scrim`, so a mode switch could not leave a stale raster even if surfaces were
      not already recreated on every mode switch. `Overlay::toolbar` being `Some` exactly in edit mode
      is the signal, written down where it is read.
    - Edit mode only, deliberately: a locked surface is a `background` layer *below* ordinary windows,
      so a scrim there would dim only the wallpaper the annotations are read against.
    - **The test ripple was the work.** An edit-mode frame now paints every pixel, so five rail tests
      that swept for "painted" or "empty" pixels broke. They were rewritten around a `content()` helper
      (alpha greater than the scrim's) rather than loosened: "is anything painted here" would now be
      satisfied by the scrim everywhere and would test nothing. One of them — the label contrast check —
      had been sampling the frame *beyond* the capsule, which is the scrim now; it samples the capsule's
      own fill again.
    - Checked by eye: the frame composited over a bright checkered wallpaper, in edit mode (the
      wallpaper is dimmed to near-black, the annotations and the rail read clearly) and in locked mode
      (no scrim, the wallpaper at full strength).

12. **The canvas pans and zooms** (`openspec/changes/canvas-pan-and-zoom/`). Annotations were pinned to
    output-local coordinates at 1:1, so a note written past the edge of the screen was unreachable and
    there was no way to see a whole drawing. The user asked for a plane that grows with the content,
    and chose an **unbounded plane with no visible edge** over a board rectangle with one.
    - `View { zoom, offset }` with `screen = canvas * zoom + offset` (per output, in `Output`, **not**
      persisted, and carried across the surface recreation a mode switch does — `set_mode` already
      carries `name` that way, and losing the view on every `Esc` would be felt at once).
    - **One transform at the input boundary, one per draw site.** Everything the document is asked is
      canvas coordinates; the interface — the rail, the plate, the scrim, the selection's handles, the
      marquee's hairline — is screen space and ignores the zoom. `Screen { scale, view }` carries both
      to the draw sites, with `point`/`len` for canvas and `ui`/`ui_rect` for interface lengths. The
      eraser's radius is divided by the zoom, because it is what the pointer aims with.
    - The selection frame is mapped to **screen** before the handles are placed or grabbed, so
      `grab_at`, `selection_bounds` and `draw_selection` all work in the same units the handles are
      drawn in; `rotate_handle` and `grab_at` gained the gap/radius they use as a parameter.
    - A view change is a whole-surface frame: `BaseKey` gained the view and the raster is screen space,
      so a pan or a zoom rebuilds it. **Measured: 32.5 ms median at 4K over 200 strokes** (bench case
      `pan-cold-dense`), against 19.7 ms for the commit that rebuilds the same raster, because a pan
      also copies the whole surface back out. That is the most expensive thing the program does, and
      the partial rebuild bounded by the exposed band is the fix — it is the same work as the
      existing next step 1.
    - The gestures: `axis_intent(source)` maps `Finger` to pan and `Wheel` to zoom and **ignores
      anything else** rather than guessing (`Discrete` is not an axis *source*, as the first draft of
      the plan assumed — the compiler said so). `zoom_factor` makes one notch exactly `ZOOM_STEP`.
      The pinch is bound by hand from `zwp_pointer_gestures_v1` (sctk 0.21 wraps no part of it) with a
      `Dispatch2` impl next to the text-input one, and the protocol reports the scale as **cumulative
      since `begin`**, so consecutive updates take a ratio. The middle button and the hand tool share
      `begin_pan`/`drag_pan`/`end_pan`.
    - A pan is not a document change: it never touches `doc_changed_at`, never reaches the file, and
      `Esc` has nothing to commit or abandon. It takes the pointer for its duration, and it ends on
      the button's release or on the pointer leaving the surface — the left-release case was a real
      bug caught while finishing (the hand tool would have gone on panning with no button down), which
      is the kind of thing the missing input test would have caught.
    - **The first thing the user found after this landed was a coordinate-space bug**, and it is worth
      reading as a warning about the missing input test: `select_press` passed the **screen** press point
      and the **screen** frame into `Gesture::start`, and `drag_selection` built the marquee and the
      transform from screen coordinates, while everything downstream — `marquee_select`, `Transform`
      applied to element geometry, the damage boxes — reads canvas coordinates. The symptom was exactly
      what a mixed space produces: after a pan the selection tool's marquee drifted away from the cursor
      and only worked in the region where screen and canvas happen to coincide.
      The fix is structural rather than local: `canvas::selection_press` is now a pure function holding
      both spaces — the handle hit test in screen (that is where the handles are drawn), the element hit
      test and every gesture and marquee point in canvas — and `App::select_press` only carries its
      answer out. `tests/canvas_hit.rs` pins it: a handle is grabbed at its drawn position and *not* at
      its canvas position, an element is found at its screen position, and a marquee is stored as a
      canvas point. `drag_selection` converts the pointer once, for the same reason.
    - What is **not** verified: no touchpad or wheel exists in this environment, so none of these
      gestures has been used for real, and the wiring above the pure functions — including the
      coordinate conversions this bug lived in — is exactly what a session with a pointer has to
      confirm. The pure parts are tested (the view's maths, the axis mapping,
      the notch arithmetic, a panned and zoomed frame landing where the transform says, the rail not
      moving with the view), and the wiring to the compositor is not.

## Where things live

- `src/render.rs` — rail constants at the top (including `ICON_STROKE` and `ICON_INSET`), the scrim
  (`SCRIM_FILL`, and the raster's background),
  `Icon`/`Mark`/`Block`/`key_of`/`labelled`, `toolbar_blocks`, `toolbar_pill`, `draw_toolbar`,
  `draw_icon`, `toolbar_hit`, `toolbar_bounds`, `caret_geometry`, `draw_text`, `text_edit_bounds`,
  `draw_selection`, `draw_marquee`, `draw_stroke`, `draw_shape`, `sk_rect`, `DamageRegion`. The
  frame path: `render_with_cache(buf, …, damage, &mut FrameCache, skip: &[ItemId])` fills the damaged
  region from `FrameCache::base` (a `Pixmap` holding the committed document), draws the elements
  named in `skip` with the live gesture over it, then draws the overlays; `draw_document` is both the
  cache build and the fallback when the pixmap cannot be allocated; `transient_damage(last, now,
  rail)` is the damage rule. `BaseKey::skip` is the set the raster leaves out: the box under edit, and
  the elements a live selection gesture holds.
- `src/app.rs` — layer surfaces and mode switching, rail state (`current_toolbar`,
  `refresh_toolbar`, `refresh_toolbar_hover`), the animation (`hover_grow`, `tick`, `next_wait`),
  the document cache's invalidation (`doc_changed`, `doc_changed_at`, `Output::pending`,
  `Output::cache`), the clipboard wayland side (`DataDeviceHandler`, `selection`), cursor shapes
  (`tool_cursor`, `handle_cursor`, `desired_cursor`, `sync_cursor`), shapes (`begin_shape`,
  `commit_transient_drawing`), and the selection's pointer state machine (`select_press`,
  `drag_selection`, `release_selection`, `write_gesture`, `commit_any_gesture`, `drop_selection`,
  `finish_marquee`).
- `src/input.rs` — pointer (press/motion/release, hover, cursor, `pointer_release`), keyboard
  shortcuts and the local text-key path (`edit_action`), `Esc`'s stages (`escape`), the handle
  under the pointer (`update_selection_grab`), `zwp_text_input_v3` handling, `paste`/`take_pasted`.
- `src/canvas.rs` — the document model (strokes, text boxes, shapes), `TextBuffer` and `Caret`,
  `Toolbar`/`ToolbarAction`, `PALETTE`, `Tool`, `erase` and `remove` (both return the box they freed),
  `Rect`. The
  shape model and its geometry, pure and tested: `Shape`/`ShapeKind`, `head_len`/`head_half`,
  `is_empty`, `bounds`, `outline` (the flattened polyline a hit test measures), `distance_squared`,
  `rect_of`, `item_bounds`. And all of the selection geometry: `Handle`, `Grab`, `Gesture`,
  `Transform` (with `stroke`, `text` and `shape`), `Selection`, `grab_at`, `selection_frame`,
  `selection_bounds`, `marquee_select`, `rotate_handle`. Pure: no Wayland.
- `tests/` — `canvas_hit`, `ipc`, `keys`, `render`, `store`, `text_buffer`.
- `scripts/smoke.sh` — headless sway, drives the CLI, greps the daemon log, checks screenshots,
  and measures the daemon's processor time over 5 idle seconds (fails above 30 ms).

## How to work in this repository

- Behaviour changes start in OpenSpec: `openspec/changes/<name>/` with `proposal.md`,
  `design.md`, `specs/<capability>/spec.md`, `tasks.md`; run
  `openspec instructions <artifact> --change <id>` and `openspec validate <id> --strict` before
  and after editing, and tick `- [x] N.M` as work finishes.
- Commit only after the contract passes: `cargo fmt --check`, `cargo clippy --all-targets`
  (must stay warning-free), `cargo test`, `scripts/smoke.sh`. `cargo bench --bench render` is a
  measurement tool, not a gate; the README records its baseline and on this machine repeat runs
  move by more than 10 %.
- The human pushes. Finish with `jj describe -m "..."`, `jj bookmark set main -r @`, `jj new`,
  then hand over `jj git push --bookmark main`.
- Scope discipline: implement what was asked, name optional extras separately.
- Log lines the smoke greps: `layer surface created`, `mode switched to Edit`,
  `wp_cursor_shape_v1 available`, `tool switched to Text`. Renaming one means updating
  `scripts/smoke.sh` in the same commit. A newer one, `text edit on an existing box` / `a new
  box`, is emitted but not grepped yet: it was written for the pointer-driven check that the
  seat cannot deliver.

## Keep the tools awake

- `scripts/smoke.sh` runs sway with `WLR_LIBINPUT_NO_DEVICES=1`, so the seat has
  `capabilities: 0` and **no** injected input reaches any surface: `swaymsg seat <s> cursor
  set/press/release` succeeds but nothing happens (verified). The pointer shape, the hover, the
  caret, click-to-re-edit, paste and every part of the selection tool — the press that picks an
  element, the marquee, dragging, the handles, the rotation handle — are therefore covered by unit
  and geometry tests only, and the README's **Unverified** bullet says exactly that. The rail's
  *shape* is covered by the smoke screenshot and its *icons* by an ink test at two scales plus a
  by-eye render at scale 2, but which icon a block shows after a *click* on it is not covered.
- Closing that gap is feasible: this sway advertises `zwlr_virtual_pointer_manager_v1` **and**
  `zwp_virtual_keyboard_manager_v1` (checked with
  `WAYLAND_DEBUG=1 target/debug/sgraffito daemon 2>&1 | grep global` against headless sway), so a
  test client can inject both clicks and keys — which would also cover the hover, the caret keys
  and `Ctrl`+`V`. Nothing is written yet; it is the single highest-value next step.
- Still unverified for the same reason: niri/hyprland/river, a real fcitx5 session, and
  real-compositor output lifecycle events.
- Measuring idle cost without a profiler: read fields 14 and 15 of `/proc/<pid>/stat` (user and
  system clock ticks) around a quiet window. `getconf CLK_TCK` is 100 here, so one tick is 10 ms
  and the measurement is coarse — a difference of 1 is 10 ms. `scripts/smoke.sh` does this over a
  five-second window and fails above 30 ms. To attribute a cost, slow one suspect down (change a
  timer constant, rebuild) and see whether the number moves: that is how the 10 ms `accept()`
  poll was identified, and a per-tick cost that does not move with the timer is not the loop.

## Rendering a preview (there is no ImageMagick or PIL here)

`grim -o <output> -t ppm` against a headless sway, then PPM → PNG with a ~20-line
python/`zlib` writer (IHDR/IDAT/IEND, no filtering), then open the PNG with the image tool.
`swaymsg` needs `SWAYSOCK` exported (`find "$XDG_RUNTIME_DIR" -name 'sway-ipc.*.sock'`): its own
fallback lookup fails in this environment. Hover states can be previewed by rendering
`Overlay { toolbar: Some(Toolbar { hover, grow, ..rail }) }` from a scratch test.

What the test compositor offers, worth knowing before planning a test (same probe):
`wl_data_device_manager` v3, `zwp_primary_selection_device_manager_v1`,
`wp_cursor_shape_manager_v1` v2, `zwp_text_input_manager_v3`, `zwp_input_method_manager_v2`,
`zwlr_screencopy_manager_v1` (what `grim` uses), both virtual input managers, and `wl_seat` with
no capabilities until a device exists.

## Deliberate ceilings

- A text box has no angle, so a rotation moves it around the frame's centre with its glyphs upright.
  Turning the glyphs is additive if wanted: one field, one rendering path (rasterise the shaped box,
  fill the turned rectangle with it as a `Pattern` shader), one inverse in front of the measured-region
  hit test, and a version 2 file.
- The selection is a tool's state, not the document's: there is no group entity, so the frame is
  recomputed from the geometry every frame, a restart starts with nothing selected, and there is no
  shift-click to add one element to a selection — a marquee is how several are taken at once. There
  is no delete key for a selection either; the eraser still removes one element per click.
- A resize scales the geometry, not the pen: `Stroke.width` never changes, and a text box's `size`
  follows the vertical factor of the resize, clamped to the existing text size bounds. A factor that
  would mirror the selection is clamped instead, because a mirrored text box cannot be drawn.
- The frame and the marquee are drawn in `TOOLBAR_ACCENT` as hairlines and capsules, with no fill of
  their own, so the interface gained no third shape or colour.
- While editing, the desktop is nearly hidden: the scrim is 90% opaque black, which is the point
  (the annotations are what to read) and is one constant if it ever reads as too much. It is also why
  the smoke test cannot see it — that run has no wallpaper client, so its backdrop is already black.
- `Delete` removes a whole selection with no undo behind it, and the rail is essentially opaque now
  that it sits on a 90% black plate: the wallpaper no longer reads through the gaps. Both were asked
  for; reverting the plate is one constant and one call in `draw_toolbar`, plus `toolbar_bounds` back
  to the hairline-only box.
- A shape is stroked and never filled, and it is hit by its outline and never by its interior. There
  is no way to change a shape's kind, drag one of its corners or fill it once it has landed: erasing
  it and drawing it again is the way. An arrow is one segment with one head, drawn at the end the
  pointer stopped on; no double head, no bend, no snapping to the element it points at.
- A rail icon is 16 pt of drawing in a 30 pt block at `ICON_STROKE`. The eraser's rounded square and
  the rectangle tool's icon are the closest pair in the set: the rectangle's corners are sharp and
  the eraser's are round, and the hover text is what settles it. No block carries its key on its
  face, so a block whose text does not fit the reserve teaches no key at all — the icon is the
  meaning, and the README's key table is the fallback.
- A shape's hit test measures a 96-gon for an ellipse while the frame draws the real curve (`push_oval`),
  because tiny-skia has no distance query. The flatness is under a pixel at 4K against the 8-pixel
  eraser tolerance; the `ponytail:` marker on `Shape::outline` is where that ceiling is written down.
- The rail is eighteen blocks, about 638 logical pixels tall, so an output shorter than roughly 660
  loses the bottom of it. `toolbar_origin` pins the top to the margin in that case, which is why the
  smoke test's 800x600 output sees the rail reach its top rows.
- A selection press starts with a whole-surface frame (`out.dirty`, as every other tool's press
  does); the frames that follow a gesture damage the union of the frame's previous and current box.
  Only a gesture's first movement rebuilds the raster, because that is the frame the held elements
  leave it on.
- A whole-surface frame now costs a full-surface copy from the document cache: ~13 ms at 4K where
  it used to be 8.4 ms, ~2 ms at 1080p. Only a mode switch, a resize or scale change, `clear`,
  scale-factor change or a fresh surface pays it.
- The frame that follows a document change rebuilds the cache by clearing and redrawing the whole
  document: 20 ms over a dense 4K drawing, against 15 ms for the whole-surface frame it replaced.
  A partial rebuild limited to the change's box is the next step if that shows up in use.
- One pixmap per output in ordinary memory: 33 MB at 3840x2160, 8 MB at 1920x1080, on top of two
  shm buffers.
- The pop-out is the only animation; the collapse is instant, and nothing consumes Reduce Motion
  (a Wayland client has no equivalent of the macOS setting without a portal dependency).
- Paste is plain text only (`text/plain;charset=utf-8`, then `UTF8_STRING`, then `text/plain`),
  capped at 256 KiB, into the box being edited; there is no copy or cut, and with an IME active
  the key goes to the IME first.
- `TextItem::bounds()` (0.6 em per character) is still the *eraser's* hit area; only the
  click-to-edit path moved to measured bounds.
- The caret moves by arrows only: no mouse placement inside a box, no `Home`/`End`, no
  word-wise movement, no desired-column memory for `Up`/`Down`.
- The cursor shapes are the named ones of `wp_cursor_shape_v1` (the eraser gets `cell`), so there
  are no custom cursor images.
- The rail's damage reserve is a 224 px left strip that is 434 px tall; it only joins a damage
  box that reaches it, because reserving it unconditionally would swallow a drawing box far from
  the rail.
- `TOOLBAR_COLUMN` is a hand-written sum of the block heights and gaps, guarded by a
  `debug_assert_eq!` in `toolbar_pill`; adding a block means updating both.

## Next steps nobody has started

1. A partial document-cache rebuild bounded by the change's box, so the frame after a commit stops
   redrawing the whole document (see *Deliberate ceilings*).
2. A virtual-pointer and virtual-keyboard test client, to make the pointer, keyboard, caret and
   paste paths end-to-end (see *Keep the tools awake*: the globals exist here).
3. Copy/cut, and `wl_data_device` primary-selection paste (the compositor advertises
   `zwp_primary_selection_device_manager_v1`), if wanted.
4. Moving the control listener into the calloop loop, which would delete the control thread, its
   request channel and the ping source — deliberately not done, because a client that connects
   and never sends would then sit on the event loop (see `design.md` D1).
5. Eraser hit testing from measured text bounds (the measurement now exists); long text wrapping;
   mouse caret placement; undo/redo. Undo/redo is what a mis-drag wants most: a gesture is written
   into the geometry, so an undo stack would have to record the before-image of the elements it
   touched, and `Esc` only helps while the pointer is still down.
6. Turning a text box's glyphs, which the selection work deliberately left out: an `angle_deg` field
   on `TextItem`, the shaped box rasterised and used as a `Pattern` shader for a turned rectangle,
   an inverse transform in front of the measured-region hit test, and a version 2 file. Everything
   else about the rotation is already in place.
7. Shift-click to add one element to a selection. It is an addition to the press rules in
   `select_press`, not to the gesture arithmetic, and `Delete` already removes a whole selection.
8. Filling a shape, and a second arrowhead style: a fill would need a fill colour per shape in the
   file and a decision about what a click inside a filled shape does, since the interior is
   deliberately not a hit today. Editing a shape after it lands (its kind, one corner) is the other
   half of that, and a `kind` field already in the file makes it additive.
9. Forward-delete inside a text box: `Delete` is inert there today, which is recorded rather than
   implemented. It would be a `Caret::Right`-shaped deletion in `TextBuffer`, and it is a text
   feature, not a selection one.
10. Colour the icons? They are drawn in `TOOLBAR_FOREGROUND` and take the accent block's fill behind
   them, which is what a macOS toolbar does; a per-tool tint would be a second meaning for colour in
   an interface whose colour already means the active pen.
11. Archiving the delivered OpenSpec changes, which stays impossible until the capability specs
    exist under `openspec/specs/`.

## Sticky-note interaction fixes (this session)

Three reports from real use, all in the note tool.

1. **English did not wrap while typing and the note grew only after `Esc`; Chinese looked fine.** The
   layout was never wrong — `Renderer::wrapped_text_height` wraps English into four lines at a 176 pt
   inner width, and always did. The bug was the **damage box**: `render_output` classified a note's
   keystroke as `local_text`, so the frame copied only the *text box*. A note is not a text box — its
   square is document geometry that grows with the text, and it is left out of the raster — so the grown
   part of the square, and every wrapped line that fell below the old text box, were drawn into the
   canvas and never copied out. A script that arrives through an IME has a preedit, which sets
   `out.dirty` and forces a whole-surface frame, which is why only English (no preedit) showed the bug.
   Fix: a note's edit is never a local text damage. It repaints whole, which is the cost a preedit
   keystroke already paid.
2. **A fast click left an unwanted default-sized note.** The note tool now behaves like a shape tool: a
   drag sweeps out the square, and a press that never moved creates nothing at all (`note_sized` in
   `App`, `canvas::note_drag` for the rule, `NOTE_DRAG_SLOP` 6 pt to tell a click from a drag — measuring
   that by `NOTE_MIN_SIZE` threw away every drag smaller than a note may be).
3. **A note was buried by the drawing.** Notes are now painted **last**, over strokes, shapes and text,
   and the hit test asks for the note first. This reverses the earlier choice (ink on top of a note, so a
   line stuck to a note could be erased on its own) — the element on top is the element the pointer sees,
   so the hit test follows the z-order. `a_note_is_hit_by_its_body_and_the_note_is_what_is_on_top` and
   `a_note_is_drawn_over_the_ink_that_crosses_it` hold both halves.

Still unverified: real input devices. Nothing below the pointer translation layer is covered by a test,
which is where the last three user reports all landed. (1) and (3) are covered by pure tests now; the
feel of the drag is not.

### Second round of note reports (same session)

- **Caret stuck at the right edge of a wrapped note.** `caret_geometry` located the caret's line from
  `run.text.len()`, but in this cosmic-text version `LayoutRun::text` is the *whole* buffer text and
  `line_i` is not the line index — the glyph offsets are what carry the position. The accumulated line
  starts were therefore wrong past the first line, no line matched, and the caret kept its fallback
  `x = prepared.line_w`. The line is now the last run whose first glyph starts at or before the caret.
  `the_caret_follows_the_line_a_wrapped_offset_is_on` (inline in `render.rs`, the function is private).
- **The date left the note's corner when zoomed.** `draw_note` measured the date in screen units and
  subtracted it from a canvas-space box, so it drifted by `(zoom - 1) * width`. The measured width is now
  converted back into canvas units. `the_date_stays_inside_the_note_at_every_zoom` diffs a frame with the
  date against one with `created` emptied, at four zooms.
- **A click still made a note.** `NOTE_DRAG_SLOP` was 6 pt, which a *click* that drifts a few pixels
  exceeds; the sweep was then clamped up to `NOTE_MIN_SIZE` and committed. 24 pt now, well above hand
  jitter. What the user described as "creation fails, leaving only border fragments" is the note created
  by that jittered click and then removed by the next one, leaving the frame to be repainted.
- **A note placed before another vanished.** `end_text_edit` deleted a note whose text was empty. An
  empty sticky is what the user dragged out, and dropping it made the note look as if it had failed to be
  created, so empty notes are kept now.
- **A note changed shape as soon as text was typed.** `grow_note` and `end_text_edit` assigned
  `note.h = height_for(text)`, which shrank a note dragged out tall back to the height of its text.
  `Note::grown_for` takes the maximum instead: the dragged or resized height is the floor and typing only
  adds to it. `a_note_is_never_shrunk_by_the_text_that_is_typed_into_it`.

Still not reproducible here: the drag's "feel". Nothing in the pointer path can be exercised without a
real device or the virtual-pointer test client listed under next steps, so a drag that is *janky* rather
than *wrong* is a report I can only act on with the detail of when it stalls.

### Third round of note reports

- **The date did not scale with the zoom.** It was deliberately drawn at a fixed interface size (and
  placed with `View::default()`), which the user rejected: the date is note content. It is now measured
  and placed in canvas units and drawn through the view, so it is the size of the square at every zoom.
  `the_date_stays_inside_the_note_at_every_zoom` also asserts the glyph band scales (2.5x the band of
  1.0x, and narrower below 1.0x). The earlier drift fix (`/ screen.len(1.0)`) is gone with it: nothing is
  converted any more, because both the box and the measurement are canvas.
- **A click still showed a border-only "transparent window".** `begin_note` created the note on the
  *press* — a default 180x96 square — which the frame between press and release drew, and `commit_note`
  dropped. So the note flashed on every click. Now nothing is placed on the press: `begin_note` records
  the origin, and `drag_note` creates the note with the first size the drag produces. The slop (24 pt)
  is what separates the two. `a_note_being_dragged_out_is_filled_and_not_only_outlined` covers the live
  note's fill (it was suspected of drawing an outline only; it is filled, and the suspicion was a byte
  order mistake in the test's own comparison).

Remaining unknown: the note tool's feel on real hardware, which no test here can exercise.

### The note's trail (fourth round)

Dragging a note leftwards left a faint outline behind. Reproduced in a test that runs two frames into one
buffer — the far note, then the near note with the damage box the app would use — and compares them with a
full repaint: 426 pixels differed, all of them on the old note's rounded outline, at the edge of the box.
An element reports the box it occupies and its hairline is centred on that edge, so half the stroke lands
outside the box and is never repainted. `DamageRegion::from_logical` now grows the reported box by
`DAMAGE_SLACK` (1 logical pixel), which fixes it for every element and every damage path at once.
`a_note_dragged_left_leaves_no_trail_in_its_damage_box` (zoom 1 and 2).

Correction to an earlier note here: `Stroke::bounds` does cover the stroke's width and its anti-aliasing
edge (see `stroke_bounds_cover_the_width_and_handle_the_empty_and_single_case`), so the pen has no overhang
of its own; only the hairline drawn on an element's edge needed the slack.

### The flicker when a note is pulled back in

The damage machinery was not at fault: a two-frame test and a whole-gesture test (nine positions into one
buffer, each frame damaging `transient_damage(last, now)`) both compared clean against a full repaint.
The fault was `canvas::note_drag`: it clamped `w`/`h` up to `NOTE_MIN_SIZE` but kept the *sweep's* corner,
so a clamped side reached back through the pointer. Pulling a note back in therefore made its far edge jump
out past the pointer and back on consecutive frames — a flicker on both sides, while every frame's damage
box was correct. The press is now a corner of the note at every moment and each side is the distance the
pointer moved from it. `a_note_keeps_the_press_point_as_a_corner_however_the_drag_turns`.

### A colour chip recolours the selection (feature)

`openspec/changes/selection-recolour/`. `OutputAnnotations::recolour` is the pure rule — per-kind, in
place, indices untouched, returning the union of the boxes it changed — and `App::set_color` is the one
entry point for a chip (the keys and a click both go through `apply_toolbar_action`), so the click arms the
colour *and* applies it to whatever the selection holds, as one document change per bucket. All four kinds
take the colour, notes included: a note's ink is derived from its background's brightness, so it follows on
its own. The selection stays held.

### Shift and Alt (feature)

`openspec/changes/modifier-drags/`. Modifiers reach a drag from the **keyboard**, not the pointer:
`update_modifiers` already kept `ctrl` for paste, and now keeps `shift` and `alt` beside it.

- **`Alt` + a selection drag copies it.** The decision is read **at the press** and carried in the gesture
  (`Gesture::Move { duplicate }`), because Excalidraw's own issue tracker treats reading it later as a bug
  (issue #9750) — a copy appearing under an element that was only being moved is a copy nobody asked for.
  The copy is made at the first movement (`OutputAnnotations::duplicate` **appends** per kind, so the
  originals keep their indices), the selection is handed to the copies, and the drag's transform lands on
  them, leaving the original in place (Excalidraw's PR #9403 fixed the opposite). `pending_duplicate`
  records the copies and the originals so `Esc` can put both the document and the selection back — a
  gesture still writes nothing until it is released. The record is cleared when a selection is dropped, so
  a stale list of indices can never be applied to a document that moved on underneath it.
- **`Shift` constrains**, read live so it can be added or dropped mid-drag: 15° for a line and an arrow
  (`SHAPE_ANGLE_STEP`, the Excalidraw rule; the user chose it over axes only), equal sides for a closed
  shape (`constrained_drag`) and for a note (`snap_square`), and one factor for both axes in
  `Gesture::transform` — the factor of the axis the pointer moved further along, measured against 1.0 so a
  shrink is not mistaken for standing still. An edge handle has one axis, so the edge the pointer is on
  still follows the pointer exactly.

Ceiling worth knowing: the copy is a document change made *during* a gesture, which is the one place the
"gestures never touch the document" rule is relaxed — `Esc` is what makes it safe, and any new path that
ends a gesture without going through `write_gesture` or `escape` would have to clear `pending_duplicate`
too (`drop_selection` and `cancel_transient` do).

### The backdrop switch, and the `more` block's list (this round)

- **`toggle_scrim` never told the outputs.** The frame reads the scrim from the toolbar each *output* holds
  (`overlay.toolbar.scrim`), and `toggle_scrim` flipped only `App::scrim` — no `refresh_toolbar`, unlike
  `toggle_more`, which does refresh. So the frame went on painting the backing it had, and the switch only
  showed after a mode switch rebuilt the surfaces. One line fixes it, and `mark_all_dirty` (which was
  already there) is what makes the frame whole-surface. Tests:
  `switching_the_backdrop_repaints_the_whole_surface` (pixels well away from the rail lose the scrim) and
  an inline `the_raster_is_rebuilt_when_the_backdrop_changes` (`BaseKey` carries the backing).
- **The `more` block lists the drawer.** `drawer_blocks` is now the one place the drawer's residents are
  described; `drawer_legend` is the same list as a string, drawn under the `more` capsule in the drawer's
  own column while that block is hovered and only while the drawer is shut (an open drawer names its own
  controls, and its column is then occupied). `openspec/changes/drawer-hover-list/`.

### The drawer is anchored to `more` (feature)

`openspec/changes/drawer-anchored-to-more/`. The drawer's column started at the top of the rail, so opening
it (the `more` block is second-to-last) made a control appear at the top right, far from the block that owns
it. `toolbar_pill` now anchors the drawer's rows to the `more` block's own row and pushes the list up when
the rows below would not hold it, the way a menu at the bottom of a window opens upwards. The rail's rows
and height are untouched (`TOOLBAR_COLUMN`'s debug assertion still holds), and the list stays inside
`toolbar_bounds`. The hover legend from `drawer-hover-list` still sits where the list's first item will be,
so the closed and open states read as the same thing. `the_drawer_hangs_off_the_more_block` checks the row,
the column's bounds and that the rail does not move.

### The drawer is a menu now (feature)

`openspec/changes/drawer-closes-itself/`. The user asked for "选完即关、光标移开即关". `DRAWER` is the one list of
what the drawer holds; `picks_from_drawer` and `drawer_stays_open` are the two rules, both pure and both
built from that list, so the layout, the hover legend and the closing rules cannot drift apart.
`App::set_more(open)` is the single place the drawer is opened or closed (it clears the hover too, because a
block that no longer exists cannot own the pointer); `toggle_more` is a call to it, the pick rule is applied
where a toolbar action is applied, and the leave rule where the pointer's hover is updated — the first motion
onto anything other than `more` or a drawer control closes it, with no timer and no polling.

The click on a rail block needs no rule of its own: the pointer had to reach that block, so the drawer was
already closed on the way (`update_toolbar_hover`). That is also why the spec's scenario for it says "already
closed" rather than adding a second mechanism.

### The drawer is a menu panel (feature)

`openspec/changes/drawer-menu-panel/`, the design being the user's approved proposal with one correction they
raised themselves: **the `more` block takes no state mark while its panel is open.** The accent on the rail
means "the item that is on in the list it belongs to" (the armed tool, the active colour), so accenting a block
that merely opened a menu would put two accented blocks on the screen at once, one of them not a tool. The panel
hanging off it is the state.

What the panel is: a plate (rail's fill, radius and inset), one row per control stating its icon, its name and
its key **at rest** — the one deliberate departure from "a control shows one thing at rest", because a menu row
is read and compared rather than aimed at — and the row of a control that is on takes the accent. Its rows start
`TOOLBAR_GAP` above the `more` block's own row and grow upwards, so the block's band holds only the block and its
hover capsule, which is kept.

Measured before the change (`more: true, hover: Some(More)`, hit test along the panel's row): every x from 40 to
120 returned `More`, so the panel's control was unclickable — the capsule is up to 200pt wide and the rail's hit
boxes are tested first. Fixed by the corridor (the `more` row's box reaches exactly to the panel's left edge,
`TOOLBAR_DRAWER_X`) and by moving the rows clear of that row; `a_row_of_the_panel_can_be_clicked_with_the_pointer_on_the_block_that_opens_it`
is the regression test, and it asserts the case that used to be impossible.

Geometry is computed once in `toolbar_pill` (`panel_top`, and the plate box on `ToolbarPill::panel`), and both the
painting and the hit boxes read it — the user asked for exactly that, having seen the anchor and the corridor
drift apart once already. Drawer rows also get their own width ceiling (`ceiling - TOOLBAR_DRAWER_X`), because a
row that starts a column to the right would otherwise reach past the reserve and out of `toolbar_bounds`.

Note for that regression test: the corridor's width is `TOOLBAR_DRAWER_X` (34 pt), *not* the 54 pt I had
assumed when first writing it — the test now finds the panel's left edge by probing for the row's action and
asserts that one step left of it is still the block, so the corridor and the row are checked against each
other rather than against a number I typed.

Not testable here: `Esc` closing the panel first (App-level, needs a session).

### The hole the user's Esc question found

Asked to check what `Esc` does with the panel open and the pointer elsewhere. `escape()` itself is right: the
drawer stage is first and returns, so nothing else in the chain runs (no gesture abandoned, no edit ended, no
selection dropped, no lock). But the state the question describes **was reachable**: `pointer_press` does not
refresh the hover (only `pointer_motion` does), and it never touched `self.more`, so `M` with the pointer over
the canvas, then `T`, then a click on the canvas entered a text box with the panel still open. In that state the
focused-box branch of the key handler claims `Esc` (`EditAction::EndText`) before the rail sees it, so the first
`Esc` ended the edit and left the menu up — the opposite order to the one specified.

Fixed where the state is created rather than where it is noticed: a press that is not on the panel closes it
(`pointer_press`, one line), which is the "clicking outside a menu dismisses it" idiom and makes the two states
mutually exclusive. Amended `openspec/changes/drawer-closes-itself/` (it owns the closing rules) with the rule
and two scenarios.

Still untestable here: `pointer_press` and `escape()` are App-level, so the press rule has no unit test; it is
verified by reading the branches above, not by a test that runs. Worth knowing when reading claims like this one.

## The pointer injector, and the first two bugs it caught

`examples/vinject.rs` + a new stage 5 in `scripts/smoke.sh`. `zwlr_virtual_pointer_manager_v1` is already in
`wayland-protocols-wlr`, which is a direct dependency, so the injector adds **no dependency** to the crate.

What it took to make it reliable, all measured rather than guessed:

- **One invocation is one connection.** A virtual pointer is a device: creating one makes the seat gain the
  pointer capability, destroying it takes the capability away. With one invocation per command, the second
  command's events were dropped. `seq` reads a list of gestures from stdin and keeps one device for the whole
  list; each smoke stage is one `seq`.
- **Wait for the seat's capability.** The injector binds `wl_seat`, creates the pointer, and round-trips until
  `capabilities` contains `Pointer`, plus 150 ms for the other clients of the session (the daemon is one) to
  bind their own `wl_pointer`. Without it the *first* click of an invocation was dropped at random.
- **Wall-clock timestamps.** A counter restarted at 1 per invocation is a timestamp going backwards; the
  injector now starts from `SystemTime` milliseconds.
- **Layout coordinates.** `motion_absolute` maps its position onto the layout's bounding box, so the smoke
  session's two outputs (800x600 at scale 1 beside 800x600 at scale 2) are a **1200x600 layout** and a point on
  HEADLESS-1 is its own x plus 400.
- **Rail rows.** A block's centre is its row's y from `toolbar_pill` plus the rail's top margin (8) plus half a
  block (15): `Color(3)` 131, `Pen` 205, `Eraser` 239, `Selection` 307, `Note` 545. The rail is 740 tall, so on
  the smoke test's 600-pixel outputs everything from `SizeStep(-1)` down — including the drawer's `more` block —
  is **off the surface**, which is why the drawer's own click path is still unit-tested only.

What stage 5 asserts: a colour chip arms the colour (`#3584e4` in the stroke's own record), a drag draws a
stroke that reaches both the file and the surface (852 blue-dominant pixels), the eraser's hit test removes it,
a note is created **at the size its drag gave it**, and a selection drag moves it by exactly the drag's delta.

### Two bugs it caught

1. **A note created by a drag was never marked for the store.** `commit_note` pushed the note into the document
   and entered its edit, but it never called `doc_changed_at` — every other commit path does — so the
   debounced save was never armed by the creation. The note was on the screen and in memory and only reached
   the file when the edit ended: a crash or a `SIGKILL` in between lost a note that had just been dragged out.
   One line fixes it; the fix is what makes the note assertion above pass.
2. **The smoke test used a stale binary once.** `cargo clippy --all-targets` did not refresh
   `target/debug/sgraffito`, so a run of the whole script exercised the previous build and reported failures
   from code that was already fixed. The verification order in `CLAUDE.md` (fmt, clippy, test, smoke) does
   build the binary — `cargo test` builds all targets — so this only bites a hand-run; `cargo build` first.

### What is still not covered, and why

The **keyboard** needs `zwp_virtual_keyboard_manager_v1`, which lives in `wayland-protocols-misc` (a
dev-dependency would be enough for an example) *and* a hand-written xkb keymap sent over an fd. That is the
next step for this harness and it is what would cover `Esc`, `Delete`, `B`/`M`, the tool keys, typing, the
caret and paste. The **wheel and the pinch** need `axis` and gesture injection (the protocol has `axis`; no
gesture support). The **drawer** needs an output taller than the rail (768+) or the keyboard's `M`.
The **handles** (resize, rotate) could be covered by the pointer alone and are the cheapest next assertion.
One flake seen once in an unrelated stage: `2c`'s rail screenshot asserted no ink after 0.5 s — a grim race,
not a renderer change; if it recurs it wants a retry around the screenshot rather than a longer sleep.
