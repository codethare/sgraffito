# sgraffito — session handoff

Working notes from the session that built the selection tool. `CLAUDE.md` is the behaviour contract
and the source of truth; this file is the state left behind, so a new session can start without
re-deriving it. English, like the rest of the repository; the user writes in Chinese and expects
Chinese in chat, English in the artifacts.

## State right now (2026-10-14)

- `main` = `@` — **unpushed**, two revisions ahead of `main@origin`. The lower one is the previous
  session's `rkqxvxp`; the upper one is this session's selection work. Hand the user
  `jj git push --bookmark main`.
- The working copy also carries two edits that were in flight when this session started and belong
  to the previous one: `HANDOFF.md` (these notes) and a `.gitignore` tidy that drops `CLAUDE.md` and
  `openspec/` from the repository's list because `.git/info/exclude` already ignores both locally.
  They are in the same revision as the selection work; `jj split .gitignore` if that matters before
  the push.
- What landed in this session, on top of the previous one:
  - the selection tool: a press picks an element or sweeps a marquee, dragging moves the set, the
    eight frame handles resize it and the handle above the top edge rotates it. A gesture is written
    into the geometry on release; the document is untouched while it is live.
  - the plan and the decisions are in `openspec/changes/selection-and-transform/` (all four artifacts
    complete, 30/30 tasks ticked, `openspec validate selection-and-transform --strict` valid).
- `openspec/specs/` holds only `.gitkeep`, so every change dir under `openspec/changes/` stays
  unarchived and `openspec validate <change> --strict` always prints
  `INFO ... only ADDED requirements are allowed for new specs`. That is expected here, not a
  failure; only the `Change '<name>' is valid` line matters.
- `openspec/` and `CLAUDE.md` are gitignored on purpose; the specs and the contract live
  outside the repository.
- `cargo test` is 96 tests across the lib plus six files in `tests/`.

## What this session changed and why

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

## What this session changed and why

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
   - The rail gained the `S` block (`TOOLBAR_COLUMN` is a hand-written sum with a debug assertion:
     thirteen blocks, nine gaps, three group gaps) and its `Esc` label became `cancel, then lock`.

## Where things live

- `src/render.rs` — rail constants at the top, `toolbar_blocks`, `toolbar_pill`, `draw_toolbar`,
  `toolbar_hit`, `toolbar_bounds`, `caret_geometry`, `draw_text`, `text_edit_bounds`,
  `DamageRegion`. The frame path: `render_with_cache(buf, …, damage, &mut FrameCache, skip_text)`
  fills the damaged region from `FrameCache::base` (a `Pixmap` holding the committed document),
  then draws the overlays; `draw_document` is both the cache build and the fallback when the
  pixmap cannot be allocated; `transient_damage(last, now, rail)` is the damage rule.
- `src/app.rs` — layer surfaces and mode switching, rail state (`current_toolbar`,
  `refresh_toolbar`, `refresh_toolbar_hover`), the animation (`hover_grow`, `tick`, `next_wait`),
  the document cache's invalidation (`doc_changed`, `doc_changed_at`, `Output::pending`,
  `Output::cache`), the clipboard wayland side (`DataDeviceHandler`, `selection`), cursor shapes
  (`tool_cursor`, `desired_cursor`, `sync_cursor`).
- `src/input.rs` — pointer (press/motion/release, hover, cursor, `pointer_release`), keyboard
  shortcuts and the local text-key path (`edit_action`), `zwp_text_input_v3` handling,
  `paste`/`take_pasted`.
- `src/canvas.rs` — the document model, `TextBuffer` and `Caret`, `Toolbar`/`ToolbarAction`,
  `PALETTE`, `Tool`, `erase` (returns the box it removed), `Rect`. Pure: no Wayland.
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
  and geometry tests only, and the README's **Unverified** bullet says exactly that.
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
7. Shift-click to add one element to a selection, and a delete key for a selection (today the eraser
   removes one element per click). Both are additions to the press rules in `select_press`, not to
   the gesture arithmetic.
8. Archiving the delivered OpenSpec changes, which stays impossible until the capability specs
   exist under `openspec/specs/`.
