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
| while a text box is focused: `←` `→` `↑` `↓` | move the caret by one character or one line |
| while a text box is focused: `Ctrl`+`V` | paste the clipboard at the caret |

While a text box is focused, printable characters plus `Backspace` and `Enter` (newline) go into that box at the caret, the arrow keys move the caret, `Ctrl`+`V` pastes the clipboard there, and `P`/`E`/`T`/digits are content instead of shortcuts. Clicking inside a committed box re-enters it with the caret at the end, whatever the script, because the click is tested against the region the renderer measured; clicking anywhere else finishes the edit in progress and keeps what was typed, and starting a new box beside an old one stays possible. `Esc` finishes the text edit without leaving edit mode, so a tool or size change is one keypress away; a second `Esc`, with no text box focused, locks. Text boxes grow only when `Enter` inserts a newline or a pasted selection has one; a long logical line is clipped at the output edge instead of being wrapped automatically. Ordinary committed text edits reuse the measured text region when possible; preedit and lifecycle transitions deliberately use a full redraw.

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
- Committed annotations are rasterised once per change into a per-output document cache, and every frame fills the region it repaints from that cache before drawing the overlays on top. A frame therefore paints only what moved: a drag, the eraser marker and a keystroke damage the box of the element that changed, a click on the rail damages the rail's strip, and a document change damages the box the change occupied. Only a mode switch, a resize or scale change, the creation of a surface and `clear` damage the whole surface.
- Measured on this machine on 2026-10-08 (release, pixman software rendering, median of 30): at 3840x2160 a drag costs 78 us against 209 us before the document cache, a drag over 200 mutually overlapping strokes 77 us against 167 ms, a frame of the rail's pop-out 1.3 ms against 8.1 ms, and a keystroke in a 1080p text box 109 us against 3.1 ms for the same frame drawn whole. What got more expensive is the surface-bound frame: filling the whole output from the cache costs a full-surface copy, so a 4K whole-surface frame is now about 13 ms rather than 8.4 ms (2 ms rather than 3 ms at 1080p), and the frame that reports a document change rebuilds the cache by redrawing the document, so a commit over a dense 4K drawing costs 20 ms. The cache is one pixmap per output in ordinary memory — 33 MB at 3840x2160, 8 MB at 1920x1080 — which is what pays for the rest.
- Strokes are rendered as a quadratic curve through the midpoints of the pointer samples; the stored sample points are never smoothed, so the geometry in the file stays the raw input.
- Text editing relies on the input method's `commit_string`; without `text-input-v3` on the compositor it falls back to local keys, which can only produce characters the keyboard layout yields directly (no candidate list).
- Paste is plain text only, the best of `text/plain;charset=utf-8`, `UTF8_STRING` and `text/plain`, capped at 256 KiB, and only into a text box that is being edited: there is no copy or cut, no image paste, and `Ctrl`+`V` reaches the input method first while a preedit is active, like every other local key. Pasting waits for the selection's owner to write, which happens off the event loop, so a slow clipboard delays only the paste.
- A text box's size is fixed when it is created; there is no per-box resize, so changing the size means creating a new box. Clicking a box puts the caret at its end; moving the caret inside with the mouse, or by word, by `Home`/`End` or by `PageUp`/`PageDown`, is not implemented.
- The pop-out is the only animation, it lasts 140 ms, and it runs in one direction only: leaving a block closes its capsule in the same frame instead of animating back. Nothing consumes Reduce Motion — a Wayland client has no equivalent of the macOS setting — so the animation cannot be turned off from the compositor's accessibility settings.
- The pointer shapes are the named ones of `wp_cursor_shape_v1`, so the eraser gets the closest named shape (a cell) rather than an eraser image; there are no custom cursor images. A compositor without the protocol keeps its own cursor.
- **Unverified**: `exclusive` keyboard and `text-input-v3` on niri / hyprland / river, the fcitx5 preedit/commit flow, real-compositor output-name/closed-surface lifecycle events, and everything that needs input devices: `scripts/smoke.sh` runs sway with `WLR_LIBINPUT_NO_DEVICES=1`, which leaves the seat without capabilities, so no pointer or key can be injected there. The rail's shape is checked by screenshot, but the pointer shape, the hover, the caret's movement, re-entering a box by clicking it and clipboard paste are covered by unit tests and geometry checks only: their wiring to the compositor is what a real session has to confirm. The preedit is now cleared per input-method batch and `surrounding text` uses UTF-8 byte offsets, but real compositor and input-method sessions are what confirm those paths.

## Development

```sh
cargo test                # pure logic unit tests: data model, hit testing, JSON, rendering, key mapping, command parsing
cargo bench --bench render # release render measurements: sparse/dense drag, rail, hovered rail, text, scale
scripts/smoke.sh          # headless sway end to end: rendering, mode switching, the rail, IPC, clear, single instance, graceful exit
```

The render benchmark uses the standard library only and reports median/p95 frame times and whether the document cache was warm. The baseline below was collected on Arch Linux, Intel Xeon E3-1220L v2, Rust 1.98.1, 2026-10-08: 4K sparse drag 78/125 us, 4K drag over 200 overlapping strokes 77/137 us, 4K whole-surface frame over a dense document 12.6/16.1 ms, 4K whole-surface rail frame 13.4/14.6 ms, 4K rail frame that rebuilds the cache 25.0/28.1 ms, 4K frame of the pop-out with its box damaged 1.3/1.4 ms, 1080p text edit drawn whole 3.1/3.9 ms, 1080p keystroke with its box damaged 109/167 us, 4K frame after a commit over a dense document 20.2/23.0 ms, and a scale-2 whole-surface frame 12.2/13.2 ms (median/p95). The baseline before the document cache, for comparison, was collected on 2026-09-28 and is recorded in the commit that introduced it. These are comparison numbers, not hardware-independent thresholds, and repeat runs on this machine move by more than 10%. A whole-surface frame is the worst case: it fills the whole output from the cache instead of drawing it, and only a mode switch, a resize or scale change, `clear` or a fresh surface has to pay that; the frames that follow the pointer and the keyboard damage a box and cost between 0.1 and 2 ms.
