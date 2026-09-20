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
| drag the mouse | freehand drawing |
| `P` | pen |
| `E` | eraser (deletes whole strokes or text boxes; hit radius 8 logical pixels) |
| `T` | text tool: click to place a text box |
| `1`–`5` | pick a colour |
| `Esc` | finish text editing (if any) and return to the locked mode |

While a text box is focused, printable characters plus `Backspace` and `Enter` (newline) go into that box, and `P`/`E`/`T`/digits are content instead of shortcuts. To switch tools, press `Esc` to leave edit mode, run `sgraffito edit` again, then press the tool key.

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
- Every frame redraws the whole surface and damages all of it, so CPU cost on 4K or high refresh rate displays is unmeasured. If it hurts, switch to damage by bounding box.
- Text editing relies on the input method's `commit_string`; without `text-input-v3` on the compositor it falls back to local keys, which can only produce characters the keyboard layout yields directly (no candidate list).
- **Unverified**: `exclusive` keyboard and `text-input-v3` on niri / hyprland / river, and fcitx5 IME candidate selection. Confirm those two in a real session first.

## Development

```sh
cargo test                # pure logic unit tests: data model, hit testing, JSON, rendering, key mapping, command parsing
scripts/smoke.sh          # headless sway end to end: rendering, mode switching, IPC, clear, single instance, graceful exit
```

The spec and design live in `openspec/changes/sgraffito-v1/`.
