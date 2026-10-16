# sgraffito

Doodle and sticky notes on the Wayland wallpaper layer: annotations are drawn above the wallpaper and below ordinary windows, and while locked the mouse and keyboard pass straight through.

## Requirements

A compositor implementing `wlr-layer-shell`:

- Supported: sway, niri, hyprland, river (wlroots and similar implementations)
- **Not supported: GNOME (Mutter), KDE (KWin)** — they do not implement the protocol
- Optional: fcitx5 or another input method for IME text (through `zwp_text_input_v3`)

## Build and run

```sh
cargo build --release
./target/release/sgraffito daemon     # long-running process, started by your compositor
```

Control commands (they talk to `$XDG_RUNTIME_DIR/sgraffito.sock` and fail if the daemon is not running). The daemon reads control clients outside the Wayland event loop, so an incomplete client cannot delay drawing or keyboard input:

```sh
sgraffito toggle   # switch between locked and edit mode
sgraffito edit     # enter edit mode
sgraffito lock     # go back to the locked mode
sgraffito clear    # erase every annotation
```

Compositor key binding examples:

```
# sway
exec sgraffito daemon
bindsym $mod+d exec sgraffito toggle
bindsym $mod+Shift+d exec sgraffito clear

# niri (~/.config/niri/config.kdl)
spawn-at-startup "sgraffito" "daemon"
binds { Mod+D { spawn "sgraffito" "toggle"; } }

# hyprland
exec-once = sgraffito daemon
bind = SUPER, D, exec, sgraffito toggle

# river (from your init script)
sgraffito daemon &
riverctl map normal Super D spawn 'sgraffito toggle'
```

## Usage

**Locked mode** (the default): annotations render on the wallpaper layer, the input region is empty, and mouse and keyboard never reach the surface.

**Edit mode**: a fullscreen overlay grabs the pointer and the keyboard, and the surface is painted on a **backdrop** — black at 90% opacity over the whole output, under the annotations and under the rail — so whatever is behind the surface is dimmed and the content on it is what you read. The alpha is one constant (`SCRIM_FILL`); it is edit mode only, because a locked surface sits *below* ordinary windows and would dim only the wallpaper the annotations are meant to be read against. It is on by default and the **`B`** switch in the drawer turns it off, for when what is behind the surface is worth seeing; the switch is a preference, so it is not saved and a new session starts with the backdrop on.

| Key | Action |
|---|---|
| drag with the left button held | freehand drawing (motion with no button held draws nothing) |
| `P` | pen |
| `E` | eraser (deletes whole strokes, shapes or text boxes; hit radius 8 logical pixels) |
| `T` | text tool: click to place a text box with that point as its top-left corner |
| `S` | selection tool: click an element, or drag a marquee on empty space, then drag to move, grab a handle to resize, grab the handle above the frame to rotate |
| `O` `R` `D` `L` `A` | circle, rectangle, diamond, line, arrow: drag to draw the shape between the press point and the pointer |
| `H` | hand: drag to pan the canvas |
| `N` | sticky note: drag out the square to create one and start typing; click an existing one to write in it |
| `M` | open and close the drawer of rarely used tools, beside the rail |
| `B` | switch the edit-mode backdrop off and on |
| `1`–`5` | pick a colour |
| `[` / `]` | shrink / grow the active tool's size: pen width, or text size while the text tool is armed |
| `Esc` | cancel the drag in progress, else finish the text edit, else clear the selection, else return to the locked mode |
| `Delete` | remove everything the selection holds, as one change |
| while a text box is focused: `←` `→` `↑` `↓` | move the caret by one character or one line |
| while a text box is focused: `Ctrl`+`V` | paste the clipboard at the caret |

While a text box is focused, printable characters plus `Backspace` and `Enter` (newline) go into that box at the caret, the arrow keys move the caret, `Ctrl`+`V` pastes the clipboard there, and `P`/`E`/`T`/digits are content instead of shortcuts. Clicking inside a committed box re-enters it with the caret at the end, whatever the script, because the click is tested against the region the renderer measured; clicking anywhere else finishes the edit in progress and keeps what was typed, and starting a new box beside an old one stays possible. `Esc` finishes the text edit without leaving edit mode, so a tool or size change is one keypress away; it clears a selection next, and only then locks. Text boxes grow only when `Enter` inserts a newline or a pasted selection has one; a long logical line is clipped at the output edge instead of being wrapped automatically. Ordinary committed text edits reuse the measured text region when possible; preedit and lifecycle transitions deliberately use a full redraw.

Pen width and text size are brush settings, like the colour: they apply to content created afterwards and never change what is already drawn or what the file holds. `[` and `]` adjust the active tool's size, bounded to 1–16 logical pixels for the pen and 8–72 for text. The selection tool creates nothing, so with it armed the readout and both keys stay on the pen width, which is the size the next stroke will take.

The selection tool (`S`) is the only one that creates nothing. A press on an element takes it; a press on empty space **outside** the frame sweeps a marquee, and the elements it **wholly** contains become the selection on release, so a stroke that only crosses the box is left alone and a long connector is not dragged along by accident. Once something is selected, the frame is the target: a press anywhere inside it drags the whole set — the empty gaps between its strokes included, so a cluster is grabbable by its box and not only by one of its elements — the eight handles on the frame resize it about the opposite handle's corner or edge, and the handle above the middle of the top edge rotates it about the frame's centre. A handle wins over whatever is under it, so a resize that starts on a corner never moves the selection instead, a press on an element outside the selection takes that element instead, and at most one selection exists at a time.

`Delete` removes everything the selection holds — strokes, text boxes and shapes alike — as one change, and then clears the selection. It is the only key that removes content: `Backspace` deletes a character inside a text box and never a cluster outside one, and `Delete` itself is inert while a text box is being edited. There is no undo behind it, so a `Delete` on a large selection is the one gesture here that cannot be walked back.

What a gesture changes is the geometry itself: stroke points are offset, scaled or turned, and a text box's origin moves while its size follows the vertical factor of a resize. The pen width never changes, a frame cannot be collapsed to nothing, and a factor that would mirror the selection is clamped, because a mirrored text box cannot be drawn. A press that does not move writes nothing at all. Rotating turns strokes truly — a turned stroke is an ordinary stroke afterwards, erased by the points it now has — but a text box has no angle, so it orbits the frame's centre with its glyphs upright.

Until the pointer is released a gesture is only drawn: the document keeps its stored geometry, the elements it holds are left out of the document cache and drawn live over it, so nothing half-applied can reach the file even if the debounce fires mid-drag. `Esc` abandons a drag and keeps the selection; arming another tool, locking and `clear` drop it, and it is never saved, so a restart starts with nothing selected.

While editing, every output shows a vertical rail against its left edge, its vertical centre at the optical centre of the output, above the geometric one. Reading downwards it holds one block per palette colour (the active one ringed), a block per tool showing an **icon of what that tool does**, `[` and `]` beside the active size as a bare number, and `Esc`. No block draws a key letter: the key is stated by the text the block says when the pointer is on it.

Icons are drawn as vector geometry by the same code that draws the shapes, so a circle's block is a circle and the five shape tools' blocks are the shapes themselves, not a picture of one. A symbol font would be a coverage gamble that ends in a tofu box on a machine whose fonts differ from this one, so there is no icon font and no image asset: the pen's icon is a stroke, the eraser's a rounded square outline, the text tool's a `T`, the selection tool's a marquee with its corner grips, the size keys a `−` and a `+`, and `Esc` a padlock.

The five shape tools draw an **exact** shape from the drag rather than a smoothed freehand path, and each is stored as **two points and a kind**: a circle is the ellipse inscribed in the box the two points span, a rectangle is that box, a diamond is the rhombus whose corners are its edge midpoints, a line is the segment between the points, and an arrow is that segment with a filled head at the point the pointer ended on. A shape therefore costs two points in the file however large it is, it is stroked with the active palette colour and the pen width, and it is never filled — the arrowhead is the one filled part. A drag with no extent, or one with none across its box, draws nothing. A shape is an ordinary element afterwards: the eraser deletes it whole, a click or a marquee selects it, the selection moves, resizes and rotates it, and it cannot be edited corner by corner once it has landed.

A shape is hit by its **outline**, within the same 8 logical pixels a stroke is, and never from inside it: a stroke drawn inside a rectangle keeps its own clicks, and the eraser removes whichever of the two is nearer. A click so deep inside a circle that nothing is within 8 pixels of the rim removes nothing.

Until the pointer is released a shape is only drawn: the document still holds what it held, each frame damages only the shape's box, and `Esc`, a mode switch or a tool change abandons it with nothing written.

A **sticky note** (`N`) is a rounded square filled with the active palette colour: it is **drawn the way a shape is**, a drag sweeping out the rectangle that becomes the note, the press staying a corner of it and each side never smaller than the minimum, and a click with no drag leaves nothing behind — the caret goes in the new note, and the text wraps inside a 12 pt padding so it never touches the edge. The note **grows downwards** to hold what is written — it is at least 96 pt on a side, and taller once the wrapped text needs it — and the day it was made is drawn in the bottom-right corner as `yyyy/mm/dd`. Its text is drawn in an ink chosen from the background's brightness, because the palette reaches from a yellow that no light text can sit on to a red that no dark text can: every palette colour keeps at least 4.5:1 for the ink it is paired with. Clicking an existing note with the note or the text tool writes in it rather than stacking another one on top, and a note is an element like any other for the eraser, the selection, a marquee, a move and a resize — its text re-wraps at a new width rather than shrinking, and a resize's height is a minimum that the text overrides if it needs more room. Notes are painted **over** the strokes and shapes, so a drawing can never bury one, and the square grows with the text as it is typed rather than when the edit ends — never shrinking, so the height a note was dragged to is kept however little is written in it. An empty note stays until it is deleted, nothing is placed until the drag has moved, and its date keeps to the bottom-right corner at the note's own scale. Its date is **UTC**: the daemon has no timezone database, so late in the evening east of Greenwich a note carries tomorrow's date.

The **canvas is unbounded and per output**: annotations live at whatever coordinates they were drawn at, nothing is clamped to the output's edge, and there is no visible boundary. A **view** — a zoom and a pan — moves what you see over it, so a drawing bigger than the screen is reachable and so is one that runs off the edge:

| Gesture | Effect |
|---|---|
| touchpad two-finger swipe | pan, following the fingers |
| touchpad two-finger pinch | zoom about the pinch's focal point, and pan with it |
| mouse wheel | zoom about the pointer |
| middle button drag | pan, anchored where the press started |
| `H` and drag the hand tool | pan, anchored where the press started |

The zoom is multiplicative and clamped to 0.2–8×, so zooming in and back out returns you to where you started, and everything the canvas holds scales with it — strokes, their widths, text and text sizes, shapes — while the rail, its plate, the scrim, the selection's handles and the marquee keep their size and their place, because they are controls rather than content. The eraser's 8-pixel reach is what the pointer aims with, so it is a screen distance: zoomed out, it reaches further into the drawing's own units.

The view is **not saved**: reopening always starts at 1:1 from the origin, and it survives a mode switch, so locking and returning to edit mode leaves you where you were looking.

At the foot of the rail is a **more** block (`M`): it opens a **drawer** — a second column beside the rail, holding the controls that are not used often. The backdrop switch is its first resident. A control in the drawer behaves exactly like a rail block: it has an icon, it says what it does and its key when the pointer is on it, it is hit the same way, and its key reaches it. The drawer does not make the rail taller, which is the point of it.

Each block is clickable, and each click does exactly what the key it carries does: a colour block picks that colour, `P`/`E`/`T`/`S`/`O`/`R`/`D`/`L`/`A` arm that tool, `[` and `]` step the active size, and `Esc` cancels what is in progress and locks on the next click.

While the pointer is on a block, that block opens away from the edge into a capsule that states what it does and ends with the key that does the same thing: `pen (P)`, `text: click to place (T)`, `select: drag to move (S)`, `smaller width ([)`, `cancel, then lock (Esc)`, `colour 3 (3)`. That is where the shortcuts are taught, since no block carries its key on its face. 140 ms, ease-out, brief and in one direction. The label is uncovered by the edge as it moves, so the text is never drawn outside the shape that carries it; the other blocks stay squares and nothing moves under the pointer, which keeps the block it is on for as long as it stays on the capsule. Leaving a block closes it at once instead of animating back. Hovering is the only thing that repaints: it damages the rail's own box, not the whole output, at frame cadence.

Controls are laid out at the macOS metrics recorded in `CLAUDE.md`: 13 pt control text for both a glyph and its label, a 30 pt square block, 6 pt between a block and its label, and 12 pt of padding after a label, so text never touches the capsule's edge.

A press anywhere on the rail, or on the capsule a block has stretched into, belongs to it: it never starts a stroke, never erases and never places a text box, and the eraser marker is not drawn over it. The rail is transient: it is not part of the annotations, it cannot be erased, it does not widen the region a drag damages unless the drag reaches it, and it is not rendered while locked.

The pointer follows the armed tool through `wp_cursor_shape_v1`: a crosshair for the pen, a cell for the eraser, an I-beam for the text tool and the compositor's default arrow for the selection tool, which creates nothing of its own. With a selection held, the arrow becomes the resize shape of the handle under it, a grab shape over the rotation handle, and the protocol's held-gesture shape while a drag is in flight. The compositor's own cursor is used while the pointer is on the rail and once the daemon is locked again. A compositor that does not advertise the protocol keeps its own cursor and the daemon says so in its log.

## Data

`$XDG_DATA_HOME/sgraffito/annotations.json` (default `~/.local/share/sgraffito/`):

```json
{"version":2,"outputs":{"eDP-1":{"strokes":[{"color":"#e01b24","width":3,"points":[[12,40],[14,44]]}],
 "texts":[{"x":100,"y":200,"color":"#ffffff","size":18,"text":"buy milk"}],
 "shapes":[{"kind":"arrow","color":"#3584e4","width":3,"from":[30,400],"to":[180,260]}]}}}
```

A shape is a `kind` (`circle`, `rect`, `diamond`, `line` or `arrow`), its colour, its width and its two points — never its outline, and never a box. Version 1 files load unchanged with no shapes in them; an unknown version is still backed up rather than guessed at.

- Coordinates are **output-local logical pixels**, bucketed by the `wl_output` name.
- Changes land on disk within 1 second at most, written through a temp file in the same directory plus `rename`; `clear` and shutdown flush immediately. If a write fails, the change stays pending and the daemon retries without dropping the in-memory annotations.
- A corrupt file or an unsupported version is renamed to `annotations.json.bak` and the daemon starts with no annotations.
- After an output is renamed (or replugged), annotations in the old bucket stop being rendered but are never deleted.

## Known limitations

- v1 has no undo/redo — a gesture cannot be undone once the pointer is released, and the eraser is the only way back — no copy, cut or duplicate, no z-order, no layers, no PNG export, no configuration file and no pressure or touch support.
- The toolbar is part of the full-screen edit overlay, not a surface of its own, so it cannot respect another layer surface's exclusive zone: a compositor bar or dock that occupies the left edge, or the optical-centre band, is covered by it while editing. Its position is fixed; it cannot be dragged.
- A block shows an icon of what it does and says what it does only while the pointer is on it, so a first-time user learns the rail by hovering, or from this README's key table. Each block's text ends with its key, and that is the only place a key appears: when a block's text does not fit the room reserved for the rail, the block stays a square showing its icon and no text, and its key is then only in this README. The longest text is `text: click to place (T)`, about 177 logical pixels of capsule, which fits the 224-pixel reserve on any output at least 240 pixels wide.
- One set of annotations per output; after an output is renamed or replugged the old annotations are not rendered (the data stays in the file).
- The rail is twenty-one blocks tall — about 740 logical pixels — so on an output shorter than roughly 765 pixels its top is pinned to the margin and the bottom of it, the `Esc` block, is off the output. `Esc` on the keyboard still locks, and there is no scrolling rail or submenu. The rail sits on a black plate at 90% opacity, so it reads as one control over any wallpaper: the plate is the block column plus 6 pt on every side, and a block stretched into a capsule is wider than the plate, so a capsule reaches out over the wallpaper by design.
- **Panning or zooming a dense drawing is the most expensive thing this program does.** The committed document is rasterised into a screen-space cache, so a view change moves every pixel of it and rebuilds that cache: measured at 3840x2160 over 200 strokes, a pan frame costs 32.5 ms median (38.5 ms p95) against 19.7 ms for the commit that rebuilds the same cache, because a pan also copies the whole surface back out. On a sparse drawing or a smaller output it is milliseconds. A partial rebuild — redrawing only the band a pan exposes — is the fix, and it is the same work as the partial cache rebuild already listed as the next step.
- The rail's drawer holds only the controls that are not used often, and it is closed every time the daemon starts: there is no way to make it stay open, and a control in it is only reachable by key while it is open.
- A note's date is UTC, and its typography is its own (18 pt text, an 11 pt date): the size keys do not change a note, and there is no way to set a different size on one note. A note cannot be resized by dragging its corner while it is being typed in, and its text does not rotate with the selection that holds it.
- The view is not persisted, so reopening a drawing starts at 1:1; a pinch depends on `zwp_pointer_gestures_v1`, which sctk does not wrap and some compositors do not advertise, in which case the wheel, the two-finger swipe, the middle button and the hand tool still pan and zoom.
- `Delete` has no undo behind it: removing a selection is final, and the only way back is to draw the elements again. `Delete` is also inert while a text box is focused, so there is no forward-delete inside a box — moving the caret with the arrows and pressing `Backspace` is the way.
- A shape cannot be edited once it has landed: there is no way to change its kind, drag one of its corners or fill it. Changing one means erasing it and drawing it again. A shape's interior is never a hit, so a fill would have nothing to be clicked either.
- Every frame is painted on the edit-mode scrim: black at 90% opacity over the whole output, under the annotations and the rail, so the windows behind the surface are dimmed and the content on the surface is what reads. Locked mode paints none of it, because that surface sits below ordinary windows. The scrim is the committed-document raster's **background** rather than a per-frame paint: a frame filling its damaged region from the raster carries it, a whole-surface frame stays one row-wise copy, and a bounding-box frame stays pixel-identical to a whole-surface one. The raster's key holds whether the scrim is on, so it can never be stale. The smoke test cannot see the scrim: it runs headless with no wallpaper client, so its backdrop is already black.
- Committed annotations are rasterised once per change into a per-output document cache, and every frame fills the region it repaints from that cache before drawing the overlays on top. A frame therefore paints only what moved: a drag, the eraser marker and a keystroke damage the box of the element that changed, a click on the rail damages the rail's strip, and a document change damages the box the change occupied. Only a mode switch, a resize or scale change, the creation of a surface and `clear` damage the whole surface.
- Measured on this machine on 2026-10-08 (release, pixman software rendering, median of 30): at 3840x2160 a drag costs 78 us against 209 us before the document cache, a drag over 200 mutually overlapping strokes 77 us against 167 ms, a frame of the rail's pop-out 1.3 ms against 8.1 ms, and a keystroke in a 1080p text box 109 us against 3.1 ms for the same frame drawn whole. What got more expensive is the surface-bound frame: filling the whole output from the cache costs a full-surface copy, so a 4K whole-surface frame is now about 13 ms rather than 8.4 ms (2 ms rather than 3 ms at 1080p), and the frame that reports a document change rebuilds the cache by redrawing the document, so a commit over a dense 4K drawing costs 20 ms. The cache is one pixmap per output in ordinary memory — 33 MB at 3840x2160, 8 MB at 1920x1080 — which is what pays for the rest.
- Strokes are rendered as a quadratic curve through the midpoints of the pointer samples; the stored sample points are never smoothed, so the geometry in the file stays the raw input.
- Text editing relies on the input method's `commit_string`; without `text-input-v3` on the compositor it falls back to local keys, which can only produce characters the keyboard layout yields directly (no candidate list).
- Paste is plain text only, the best of `text/plain;charset=utf-8`, `UTF8_STRING` and `text/plain`, capped at 256 KiB, and only into a text box that is being edited: there is no copy or cut, no image paste, and `Ctrl`+`V` reaches the input method first while a preedit is active, like every other local key. Pasting waits for the selection's owner to write, which happens off the event loop, so a slow clipboard delays only the paste.
- Arrows are one segment with one head, drawn at the end the pointer stopped on; a double-headed arrow, a head at the start, a bend and a snapped connector to the element it points at are not implemented. A shape is stroked and never filled, and there is no fill colour.
- A shape's hit test measures a flattened outline (a 96-gon for a circle) while the frame draws the real curve, so the two differ by well under a pixel against an 8-pixel tolerance.
- A text box's size is fixed when it is created; there is no per-box resize, so changing the size means creating a new box, and the only way to change it afterwards is to resize a selection that holds it. Clicking a box puts the caret at its end; moving the caret inside with the mouse, or by word, by `Home`/`End` or by `PageUp`/`PageDown`, is not implemented.
- A text box has no angle of its own, so a rotation moves it around the frame's centre with its glyphs upright instead of turning it. Turning the glyphs would need an angle per text box in the file, and so a version 3 annotation file.
- The selection is a tool's state, not something the document remembers: there is no group, so what a gesture holds is recomputed from the geometry every frame, and there is no shift-click to add one element to a selection — a marquee is how several are taken at once. Elements cannot be resized on their own either: a text box's size and a stroke's width belong to the element, and a resize of a selection scales its geometry rather than thickening its pen.
- The pop-out is the only animation, it lasts 140 ms, and it runs in one direction only: leaving a block closes its capsule in the same frame instead of animating back. Nothing consumes Reduce Motion — a Wayland client has no equivalent of the macOS setting — so the animation cannot be turned off from the compositor's accessibility settings.
- The pointer shapes are the named ones of `wp_cursor_shape_v1`, so the eraser gets the closest named shape (a cell) rather than an eraser image; there are no custom cursor images. A compositor without the protocol keeps its own cursor.
- **Unverified**: `exclusive` keyboard and `text-input-v3` on niri / hyprland / river, the fcitx5 preedit/commit flow, real-compositor output-name/closed-surface lifecycle events, and everything that needs input devices: `scripts/smoke.sh` runs sway with `WLR_LIBINPUT_NO_DEVICES=1`, which leaves the seat without capabilities, so no pointer or key can be injected there. The rail's shape and a committed shape are checked by screenshot, but the pointer shape, the hover, the caret's movement, re-entering a box by clicking it, clipboard paste, the whole of the selection tool — the press that picks an element, the marquee, dragging, the handles and the rotation handle — the whole of the five shape tools, whose shapes are only ever drawn by a drag, and every canvas gesture — the two-finger swipe, the wheel, the middle button, the pinch and the hand tool — are covered by unit tests and geometry checks only: their wiring to the compositor is what a real session has to confirm. The preedit is now cleared per input-method batch and `surrounding text` uses UTF-8 byte offsets, but real compositor and input-method sessions are what confirm those paths.

## Development

```sh
cargo test                # pure logic unit tests: data model, hit testing, JSON, rendering, key mapping, command parsing
cargo bench --bench render # release render measurements: sparse/dense drag, rail, hovered rail, text, scale
scripts/smoke.sh          # headless sway end to end: rendering, mode switching, the rail, IPC, clear, single instance, graceful exit
```

The render benchmark uses the standard library only and reports median/p95 frame times and whether the document cache was warm. The baseline below was collected on Arch Linux, Intel Xeon E3-1220L v2, Rust 1.98.1, 2026-10-08: 4K sparse drag 78/125 us, 4K drag over 200 overlapping strokes 77/137 us, 4K whole-surface frame over a dense document 12.6/16.1 ms, 4K whole-surface rail frame 13.4/14.6 ms, 4K rail frame that rebuilds the cache 25.0/28.1 ms, 4K frame of the pop-out with its box damaged 1.3/1.4 ms, 1080p text edit drawn whole 3.1/3.9 ms, 1080p keystroke with its box damaged 109/167 us, 4K frame after a commit over a dense document 20.2/23.0 ms, and a scale-2 whole-surface frame 12.2/13.2 ms (median/p95). The baseline before the document cache, for comparison, was collected on 2026-09-28 and is recorded in the commit that introduced it. These are comparison numbers, not hardware-independent thresholds, and repeat runs on this machine move by more than 10%. A whole-surface frame is the worst case: it fills the whole output from the cache instead of drawing it, and only a mode switch, a resize or scale change, `clear` or a fresh surface has to pay that; the frames that follow the pointer and the keyboard damage a box and cost between 0.1 and 2 ms.
