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

**Edit mode**: a fullscreen overlay grabs the pointer and the keyboard.

| Key | Action |
|---|---|
| drag with the left button held | freehand drawing (motion with no button held draws nothing) |
| `P` | pen |
| `E` | eraser (deletes whole strokes or text boxes; hit radius 8 logical pixels) |
| `T` | text tool: click to place a text box with that point as its top-left corner |
| `1`–`5` | pick a colour |
| `[` / `]` | shrink / grow the active tool's size: pen width, or text size while the text tool is armed |
| `Esc` | finish the text edit (if any) and stay in edit mode; press again to return to the locked mode |

While a text box is focused, printable characters plus `Backspace` and `Enter` (newline) go into that box, and `P`/`E`/`T`/digits are content instead of shortcuts. Clicking anywhere finishes the edit in progress and keeps what was typed. `Esc` finishes the text edit without leaving edit mode, so a tool or size change is one keypress away; a second `Esc`, with no text box focused, locks. Text boxes grow only when `Enter` inserts a newline; a long logical line is clipped at the output edge instead of being wrapped automatically. Ordinary committed text edits reuse the measured text region when possible; preedit and lifecycle transitions deliberately use a full redraw.

Pen width and text size are brush settings, like the colour: they apply to content created afterwards and never change what is already drawn or what the file holds. `[` and `]` adjust the active tool's size, bounded to 1–16 logical pixels for the pen and 8–72 for text.

While editing, every output shows a vertical rail against its left edge, its vertical centre at the optical centre of the output, above the geometric one. Reading downwards it holds one block per palette colour (the active one ringed), a block per tool with the armed tool's block in the accent, `[` and `]` around the active size as a number, and `Esc`.

Each block is clickable, and each click does exactly what the key it carries does: a colour block picks that colour, `P`/`E`/`T` arm that tool, `[` and `]` step the active size, and `Esc` finishes the text box being edited and locks on the next click. The glyphs keep the keyboard in view, so the rail teaches the keys rather than replacing them.

While the pointer is on a block, that block opens away from the edge into a capsule that says what it does (`colour 3`, `pen`, `text: click to place`, `smaller width`, `end text, then lock`): 140 ms, ease-out, brief and in one direction. The label is uncovered by the edge as it moves, so the text is never drawn outside the shape that carries it; the other blocks stay squares and nothing moves under the pointer, which keeps the block it is on for as long as it stays on the capsule. Leaving a block closes it at once instead of animating back. Hovering is the only thing that repaints: it damages the rail's own box, not the whole output, at frame cadence.

Controls are laid out at the macOS metrics recorded in `CLAUDE.md`: 13 pt control text for both a glyph and its label, a 30 pt square block, 6 pt between a block and its label, and 12 pt of padding after a label, so text never touches the capsule's edge.

A press anywhere on the rail, or on the capsule a block has stretched into, belongs to it: it never starts a stroke, never erases and never places a text box, and the eraser marker is not drawn over it. The rail is transient: it is not part of the annotations, it cannot be erased, it does not widen the region a drag damages unless the drag reaches it, and it is not rendered while locked.

The pointer follows the armed tool through `wp_cursor_shape_v1`: a crosshair for the pen, a cell for the eraser and an I-beam for the text tool, with the compositor's own cursor while the pointer is on the rail and once the daemon is locked again. A compositor that does not advertise the protocol keeps its own cursor and the daemon says so in its log.

## Data

`$XDG_DATA_HOME/sgraffito/annotations.json` (default `~/.local/share/sgraffito/`):

```json
{"version":1,"outputs":{"eDP-1":{"strokes":[{"color":"#e01b24","width":3,"points":[[12,40],[14,44]]}],
 "texts":[{"x":100,"y":200,"color":"#ffffff","size":18,"text":"buy milk"}]}}}
```

- Coordinates are **output-local logical pixels**, bucketed by the `wl_output` name.
- Changes land on disk within 1 second at most, written through a temp file in the same directory plus `rename`; `clear` and shutdown flush immediately. If a write fails, the change stays pending and the daemon retries without dropping the in-memory annotations.
- A corrupt file or an unsupported version is renamed to `annotations.json.bak` and the daemon starts with no annotations.
- After an output is renamed (or replugged), annotations in the old bucket stop being rendered but are never deleted.

## Known limitations

- v1 has no undo/redo, no select/move/resize, no layers, no PNG export, no configuration file and no pressure or touch support.
- The toolbar is part of the full-screen edit overlay, not a surface of its own, so it cannot respect another layer surface's exclusive zone: a compositor bar or dock that occupies the left edge, or the optical-centre band, is covered by it while editing. Its position is fixed; it cannot be dragged.
- A block's meaning appears only while the pointer is on it, so a first-time user learns the rail from the key glyphs, by hovering, or from this README. A block whose meaning does not fit the room reserved for the rail stays a square and shows no meaning.
- One set of annotations per output; after an output is renamed or replugged the old annotations are not rendered (the data stays in the file).
- While a drag is in progress only the bounding box of the transient overlay (the in-progress stroke and the eraser marker) is cleared and damaged, and elements outside that box are not rasterised — including the edit-mode rail, which is only repainted when the box reaches it or when the pointer moves on it. Anything else — committing a stroke, erasing, a text edit, a click on the rail, a mode switch, a resize or a scale change, `clear` — redraws and damages the whole surface.
- Measured on this machine on 2026-09-24 (release, pixman software rendering, 3840x2160, median of 15): a whole-surface frame over 30 strokes costs 4.0 ms, the same frame with a bounding-box damage 0.06 ms. The box is grown until it contains every element it overlaps, so a drag on top of a dense drawing can grow it back to nearly the whole surface, where the fast path disappears (200 mutually overlapping strokes, box 2005x1805: 167 ms against 163 ms whole-surface). The whole-surface frame is three times cheaper than it was while every frame was byte-swapped: the buffer is allocated in `wl_shm` `Abgr8888`, whose byte order is already tiny-skia's, and only the ARGB8888 fallback pays the swap.
- Strokes are rendered as a quadratic curve through the midpoints of the pointer samples; the stored sample points are never smoothed, so the geometry in the file stays the raw input.
- Text editing relies on the input method's `commit_string`; without `text-input-v3` on the compositor it falls back to local keys, which can only produce characters the keyboard layout yields directly (no candidate list).
- A text box's size is fixed when it is created; there is no per-box resize, so changing the size means creating a new box.
- The pop-out is the only animation, it lasts 140 ms, and it runs in one direction only: leaving a block closes its capsule in the same frame instead of animating back. Nothing consumes Reduce Motion — a Wayland client has no equivalent of the macOS setting — so the animation cannot be turned off from the compositor's accessibility settings.
- The pointer shapes are the named ones of `wp_cursor_shape_v1`, so the eraser gets the closest named shape (a cell) rather than an eraser image; there are no custom cursor images. A compositor without the protocol keeps its own cursor.
- **Unverified**: `exclusive` keyboard and `text-input-v3` on niri / hyprland / river, the fcitx5 preedit/commit flow, real-compositor output-name/closed-surface lifecycle events, and pointer input with real devices: `scripts/smoke.sh` runs sway with `WLR_LIBINPUT_NO_DEVICES=1`, so no pointer ever enters the surface there and the pointer shape is only checked as far as binding the protocol and logging it. The preedit is now cleared per input-method batch and `surrounding text` uses UTF-8 byte offsets, but real compositor and input-method sessions are what confirm those paths.

## Development

```sh
cargo test                # pure logic unit tests: data model, hit testing, JSON, rendering, key mapping, command parsing
cargo bench --bench render # release render measurements: sparse/dense drag, rail, hovered rail, text, scale
scripts/smoke.sh          # headless sway end to end: rendering, mode switching, the rail, IPC, clear, single instance, graceful exit
```

The render benchmark uses the standard library only and reports median/p95 frame times. The baseline below was collected on Arch Linux, Intel Xeon E3-1220L v2, Rust 1.98.1, 2026-09-28: sparse drag 209/351 us, dense overlap 15.0/19.8 ms, 4K rail frame 8.4/10.1 ms, 4K frame with the widest capsule open 8.1/9.4 ms, 1080p committed text edit 2.12/3.19 ms, and scale-2 full frame 7.17/9.04 ms (median/p95). These are comparison numbers, not hardware-independent thresholds, and repeat runs on this machine move by more than 10%: the 4K rail frame has ranged from 7.1 to 8.4 ms over a day. Every number above except the sparse drag is a whole-surface frame, which is the worst case for a frame that has to use the other buffer; a frame of the pop-out damages the rail's box alone, like the sparse drag, so it costs an order of magnitude less. The rail cache stores layout only; a rendered rail pixmap and a spatial index remain deferred until a workload shows they are worthwhile.
