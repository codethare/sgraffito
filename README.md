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

Control commands (they talk to `$XDG_RUNTIME_DIR/sgraffito.sock` and fail if the daemon is not running):

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

While a text box is focused, printable characters plus `Backspace` and `Enter` (newline) go into that box, and `P`/`E`/`T`/digits are content instead of shortcuts. Clicking anywhere finishes the edit in progress and keeps what was typed. `Esc` finishes the text edit without leaving edit mode, so a tool or size change is one keypress away; a second `Esc`, with no text box focused, locks.

Pen width and text size are brush settings, like the colour: they apply to content created afterwards and never change what is already drawn or what the file holds. `[` and `]` adjust the active tool's size, bounded to 1–16 logical pixels for the pen and 8–72 for text.

While editing, every output shows a capsule centred near its top edge: a well with the active colour, the active size as a number, and a keycap per key with its meaning beside it, the armed tool's keycap highlighted. The hint is transient: it is not part of the annotations, it cannot be erased, it does not widen the region a drag damages, and it is not rendered while locked.

## Data

`$XDG_DATA_HOME/sgraffito/annotations.json` (default `~/.local/share/sgraffito/`):

```json
{"version":1,"outputs":{"eDP-1":{"strokes":[{"color":"#e01b24","width":3,"points":[[12,40],[14,44]]}],
 "texts":[{"x":100,"y":200,"color":"#ffffff","size":18,"text":"buy milk"}]}}}
```

- Coordinates are **output-local logical pixels**, bucketed by the `wl_output` name.
- Changes land on disk within 1 second at most, written through a temp file in the same directory plus `rename`; `clear` and shutdown flush immediately.
- A corrupt file or an unsupported version is renamed to `annotations.json.bak` and the daemon starts with no annotations.
- After an output is renamed (or replugged), annotations in the old bucket stop being rendered but are never deleted.

## Known limitations

- v1 has no undo/redo, no select/move/resize, no layers, no PNG export, no toolbar UI, no configuration file and no pressure or touch support.
- One set of annotations per output; after an output is renamed or replugged the old annotations are not rendered (the data stays in the file).
- While a drag is in progress only the bounding box of the transient overlay (the in-progress stroke and the eraser marker) is cleared and damaged, and elements outside that box are not rasterised — including the edit-mode hint, which is only repainted when the box reaches it. Anything else — committing a stroke, erasing, a text edit, a mode switch, a resize or a scale change, `clear` — redraws and damages the whole surface.
- Measured on this machine (release, pixman software rendering, 3840x2160, median of 15): a whole-surface frame over 30 strokes costs 4.0 ms, the same frame with a bounding-box damage 0.06 ms. The box is grown until it contains every element it overlaps, so a drag on top of a dense drawing can grow it back to nearly the whole surface, where the fast path disappears (200 mutually overlapping strokes, box 2005x1805: 167 ms against 163 ms whole-surface). The whole-surface frame is three times cheaper than it was while every frame was byte-swapped: the buffer is allocated in `wl_shm` `Abgr8888`, whose byte order is already tiny-skia's, and only the ARGB8888 fallback pays the swap.
- Strokes are rendered as a quadratic curve through the midpoints of the pointer samples; the stored sample points are never smoothed, so the geometry in the file stays the raw input.
- Text editing relies on the input method's `commit_string`; without `text-input-v3` on the compositor it falls back to local keys, which can only produce characters the keyboard layout yields directly (no candidate list).
- A text box's size is fixed when it is created; there is no per-box resize, so changing the size means creating a new box.
- **Unverified**: `exclusive` keyboard and `text-input-v3` on niri / hyprland / river, and the fcitx5 preedit/commit flow. The preedit is now cleared per input-method batch and `surrounding text` uses UTF-8 byte offsets, but a real fcitx5 session is what confirms the pair.

## Development

```sh
cargo test                # pure logic unit tests: data model, hit testing, JSON, rendering, key mapping, command parsing
scripts/smoke.sh          # headless sway end to end: rendering, mode switching, IPC, clear, single instance, graceful exit
```
