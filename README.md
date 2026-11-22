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

The release build is optimised for drawing speed rather than for size (`opt-level = 3`, with LTO and
one codegen unit), which is what keeps a pan or a zoom of a dense drawing interactive. The stripped
binary is about 3.8 MB; the size-optimised profile that preceded it produced 3.0 MB and cost 2.8x on
every whole-surface frame.

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

**Boards** are what a drawing belongs to, not screens. A board is a named document — its strokes, text boxes, shapes and notes — and every output shows one, which is what keeps work drawn on the laptop's own panel readable from the external monitor: shut the laptop, connect the monitor, and the monitor shows the board that was picked last, because the drawing belongs to the board rather than to the panel it was made on. The rail's **`boards`** block (`C`) opens a menu of the document's boards — the board this output shows is accent-filled, `new board` creates `Board 1`, `Board 2`, …, and a row shows two buttons at its right end while the pointer is on it: `✕` turns that row into `delete <name>?` with `no` and `yes` where the buttons were, and only `yes` removes that board with everything drawn on it, moving every screen showing it to the first board left; the `✕` is absent when one board is all the document has, because a document with no boards is nowhere to draw; `✎` edits the name in the row — `Enter` takes it, `Esc` leaves it, and a press anywhere else takes it too — and the drawer's `boards: every screen` row (`F`) decides whether a pick moves one screen or all of them: off, each screen picks its own board, so two screens can show the same drawing or two different ones; on, every screen shows the board picked on any of them. A file written before boards existed loads as one board per output, named after that output, so an upgrade leaves every screen showing what it was showing. The mode is a decision about this sitting and is not saved; the board picked last is, because that is what a screen connected tomorrow lands on. A renamed board keeps everything drawn on it, and every screen showing it stays on it.

Every mark on the canvas is drawn as a **hand-drawn** one: a stroke is a pen that went over the same line twice, a shape's edges bow, an oval is lopsided and an arrowhead is filled and then outlined by hand. The wander is derived from the element's own geometry — from the point it was started at, its colour and its width — so the same drawing looks the same on every frame, at every zoom and after a restart, and nothing about it is stored. The interface is not sketched: the rail and its icons, the selection frame, the marquee and the eraser cursor stay exact figures. A dragged element keeps its mark while it moves and settles into its new one when the gesture is released; a moved or nudged element is drawn from where it now is, because the point it was started at is part of what the mark is derived from.

| Key | Action |
|---|---|
| drag with the left button held | freehand drawing (motion with no button held draws nothing) |
| `P` | pen |
| `E` | eraser (deletes whole strokes, shapes or text boxes; hit radius 8 logical pixels) |
| `G` | bucket fill: click inside a circle, rectangle or diamond to fill its background with the current colour; clicking a line or arrow does nothing |
| `T` | text tool: click to place a text box with that point as its top-left corner |
| `V` | selection tool: click an element, or drag a marquee on empty space, then drag to move, grab a handle to resize, grab the handle above the frame to rotate — circles, rectangles and diamonds turn as a whole, while lines and arrows turn their endpoints. Picking a circle, rectangle or diamond takes every stroke, text, note and shape wholly inside it, so dragging the container drags the group; picking an element inside it directly selects that element alone; a single click on text selects and can drag it, and a quick double-click opens that box for editing |
| `O` `R` `D` `L` `A` | circle, rectangle, diamond, line, arrow: drag to draw the shape between the press point and the pointer |
| `H` | hand: drag to pan the canvas |
| `N` | sticky note: drag out the square to create one and start typing; click an existing one to write in it |
| `M` | open the drawer of rarely used tools beside the rail, and close it again (it also closes when something in it is picked, the pointer leaves it, or `Esc` is pressed) |
| `B` | switch the edit-mode backdrop off and on |
| `C` | open the boards panel beside the rail: pick a board, create one, or delete the board this screen shows (it closes on a pick, on a press elsewhere, or on `Esc`) |
| `d` / `r` / `n` while the boards panel is open | delete / rename the board this screen shows / create `Board N`; these are the list's own keys, not the diamond, rectangle or note tools |
| `F` | switch between one board on every screen and one board per screen — the drawer's own row |
| `1`–`5` | pick a colour |
| `[` / `]` | shrink / grow the active tool's size — pen width, or text size while the text tool is armed — and, with a selection held, the width of its strokes and shapes and the size of its text boxes |
| `Esc` | close the open drawer, else cancel the drag in progress, else finish the text edit, else clear the selection, else return to the locked mode |
| `Delete` | remove everything the selection holds, as one change |
| `Ctrl`+`Z` | undo the last document change; outside a text box, because the editor owns the keys while it is focused |
| `Ctrl`+`Shift`+`Z` | redo the change last undone |
| `Ctrl`+`X` | cut the selection: exactly what `Ctrl`+`C` copies enters the clipboard history, then the originals are removed as one change |
| `Tab` / `Shift+Tab` | walk the selection to the next / previous element on this output, in document order and wrapping at both ends |
| `←` `→` `↑` `↓` | with something selected, move it by 1 logical pixel, or 10 while `Shift` is held; with nothing selected, pan the view by 40 pixels, or 400 with `Shift` |
| `+` / `=` / `-` | zoom the view in / out by one step, about the centre of the output |
| `0` | put the view back at 1:1 from the canvas origin |
| `Enter` | open the selected text box or note for writing, with the caret at its end |
| double-click a text box with `V` armed | open it for writing; a single click selects and can drag it |
| hold `Shift` while dragging | constrain the drag: a line or an arrow snaps to the nearest 15°, a circle, rectangle, diamond or note comes out with equal sides, and a resize of a selection keeps its proportions |
| hold `Shift` and click an element | add it to the selection, or take it out when it is already held |
| hold `Alt` when a selection drag starts | copy what the selection holds and drag the copy, leaving the original where it was |
| while a text box is focused: `←` `→` `↑` `↓` | move the caret by one character or one line |
| while a text box is focused: `Ctrl`+`V` | paste the clipboard at the caret |

The rail floats clear of its screen edge as one bordered card with an inset shadow, and hovering a button paints a visible wash behind it; the buttons share that card rather than drawing a border around each square; a three-quarter-block rule separates the colour chips from the tools, and another precedes `more tools`; `more tools` and the lock control share a group. The rail shows only the colour in hand. Hovering that swatch opens the other four in one horizontal row beside it, and a colour key opens the same strip for a 1.5-second dwell so the new choice is visible; hovering a chip names it after the row, and the whole open card holds it so the pointer can sweep from one colour to the next. Every pop-out opens with a lightly damped spring. The size, boards, clipboard and `more tools` lists each open in one card fitted to their rows and set clear of the rail by the same gap the colour card uses; the rows share that card instead of drawing a capsule each. Every rail button with a one-key shortcut shows that letter in the bottom-left of its square; full chords and alternate bindings stay in the hover expansion. The zoom percentage card and one undo/redo card share the same dark fill and border, without an inset shadow, and sit side by side in a configurable corner or centre slot, bottom right by default; the two history halves share the card background and are dimmed when their action has nothing to do.

Pen, circle, rectangle, diamond, line and arrow are one-shot drawing tools: once a stroke or shape is committed, the pointer returns to `select`, so the next gesture picks or moves instead of drawing again. The text tool does the same when a non-empty text box is committed. A click that draws nothing, the eraser and the note tool keep their tool.

While a text box is focused, printable characters plus `Backspace` and `Enter` (newline) go into that box at the caret, the arrow keys move the caret, `Ctrl`+`V` pastes the clipboard there, and `P`/`E`/`T`/digits are content instead of shortcuts. Clicking inside a committed box re-enters it with the caret at the end, whatever the script, because the click is tested against the region the renderer measured; clicking anywhere else finishes the edit in progress and keeps what was typed, and starting a new box beside an old one stays possible. `Esc` finishes the text edit without leaving edit mode, so a tool or size change is one keypress away; it clears a selection next, and only then locks. Text boxes grow only when `Enter` inserts a newline or a pasted selection has one; a long logical line is clipped at the output edge instead of being wrapped automatically. Ordinary committed text edits reuse the measured text region when possible; preedit and lifecycle transitions deliberately use a full redraw.

`Ctrl`+`Z` steps back through document changes made in this sitting, and `Ctrl`+`Shift`+`Z` moves forward again: one drawing gesture, one drag or nudge, one text-edit session or one clipboard paste is one step. Any new change clears the redo path. The history and redo path are in memory, capped at 64 steps each; switching boards, panning, zooming, tools and panels are not document changes, so they are not undone.

Pen width and text size are brush settings, like the colour: they apply to content created afterwards, and with a selection held they also step what it holds — each selected stroke's and shape's width by the pen step inside 1–16, each selected text box's size by the text step inside 8–72, and a note never, because its type size is its own — as one change per press. `[` and `]` adjust the active tool's size, bounded to 1–16 logical pixels for the pen and 8–72 for text. The selection tool creates nothing, so with it armed the readout and both keys stay on the pen width, which is the size the next stroke will take; a selected text box grows or shrinks without the readout moving, because the readout is the brush's.

The selection tool (`V`) is the only one that creates nothing. A press on an element takes it; with `Shift` held it toggles instead — the element joins the selection when it was not held and leaves it when it was, the press starts no drag, and `Shift` on empty space or inside the frame but off an element changes nothing; a press on empty space **outside** the frame sweeps a marquee, and the elements it **wholly** contains become the selection on release, so a stroke that only crosses the box is left alone and a long connector is not dragged along by accident. Once something is selected, the frame is the target: a press anywhere inside it drags the whole set — the empty gaps between its strokes included, so a cluster is grabbable by its box and not only by one of its elements — the eight handles on the frame resize it about the opposite handle's corner or edge, and the handle above the middle of the top edge rotates it about the frame's centre. A handle wins over whatever is under it, so a resize that starts on a corner never moves the selection instead, a press on an element outside the selection takes that element instead, and at most one selection exists at a time. Holding `Alt` when that press starts copies what the selection holds and drags the copy, leaving the original exactly where it was; the copy is what stays selected, so the next gesture acts on it, and `Esc` before the release takes the copy back and puts the selection on the originals again, because a gesture never writes to the document until it is released. Holding `Shift` through a resize keeps the selection's proportions — one factor for both axes, the one the pointer moved further along — so a circle cannot be stretched into an ellipse; letting `Shift` go mid-drag returns the resize to one factor per axis.

A **colour chip** does two things at once: it arms that colour for the next stroke, shape or note, and — while a selection is held — it recolours everything the selection holds, strokes, shapes, text boxes and notes alike, as one change. The selection stays held, so a second chip recolours the same elements, and a note's ink follows its new background because it is derived from it. With nothing selected a chip only arms the colour.

`Delete` removes everything the selection holds — strokes, text boxes and shapes alike — as one change, and then clears the selection. It is the only key that removes content: `Backspace` deletes a character inside a text box and never a cluster outside one, and `Delete` itself is inert while a text box is being edited. There is no undo behind it, so a `Delete` on a large selection is the one gesture here that cannot be walked back.

**The keyboard alone can do all of it**, without a pointer anywhere: `Tab` walks the selection through everything the output holds — strokes, then text boxes, then shapes, then notes, each in document order — one element per press and wrapping at both ends, and the frame, the colour keys and `Delete` then act on what it put there. The arrows are the tool the selection makes them, which is the split `tmmpr` uses: with something held they move it, one logical pixel a press and ten with `Shift`, as one change per press, and the element stays held so the next press carries on from where the last one ended; with nothing held they pan the view, in the direction the viewport moves, so `→` looks further right. A held element's step is a screen distance, so it is the same length to look at at every zoom — a zoomed-out drawing is not a drawing that is harder to nudge. `+`/`=`/`-` zoom about the centre of the output and `0` puts the view back at 1:1, which is the only way back from a pan that went too far. `Enter` opens the element the selection holds for writing — a single text box or a single note — so committed text is reachable without clicking it; it never creates anything, so a text box where there was none still needs the text tool and a point. Only the arrows repeat when held: a held `Esc`, `Tab`, `0` or `Enter` acts once, because a key held a fraction too long must not walk the cancel stages, the selection or the view. The keys act on the output the keyboard is focused on, the same one the rail's keys act on, and a text box that is being edited keeps every key it owns — the arrows are the caret's there, not the selection's.

What a gesture changes is the geometry itself: stroke points are offset, scaled or turned, and a text box's origin moves while its size follows the vertical factor of a resize. The pen width never changes, a frame cannot be collapsed to nothing, and a factor that would mirror the selection is clamped, because a mirrored text box cannot be drawn. A press that does not move writes nothing at all. Rotating turns strokes truly — a turned stroke is an ordinary stroke afterwards, erased by the points it now has — but a text box has no angle, so it orbits the frame's centre with its glyphs upright.

Until the pointer is released a gesture is only drawn: the document keeps its stored geometry, the elements it holds are left out of the document cache and drawn live over it, so nothing half-applied can reach the file even if the debounce fires mid-drag. `Esc` abandons a drag and keeps the selection; arming another tool, locking and `clear` drop it, and it is never saved, so a restart starts with nothing selected.

While editing, every output shows a vertical rail against its left edge, its vertical centre at the optical centre of the output, above the geometric one. Reading downwards it holds one block per palette colour (the active one ringed), a block per tool showing an **icon of what that tool does**, the active size as a bare number — which opens the two size steps when it is clicked — and `Esc`. A list eases open the way a block's pop-out does, over the same 140 ms; it closes at once. The width list stays once it is open: a step does not put it away, and only `Esc`, a press elsewhere or a mode switch do. The drawer is the one list that leaves with the pointer. While the width list is open the rail's own blocks keep their own presses — the walkway into it is the gap beside them — where the drawer and the boards list own the strip beside the rail for the height of their rows. Each block that has a shortcut carries its primary key as a small badge in the bottom-left corner of its square, clear of the icon; the text the block says when the pointer is on it states the full key, which is where an alternate or a chord is named, and a control that lives in the drawer is listed under the `more` block's own text while the pointer is on that block, so what the drawer holds can be read without opening it. The drawer opens as a **menu panel**: a plate of its own, in the rail plate's fill, behind one row per control, each row stating its icon, its name and the key that does the same thing without needing the pointer on it, and the row of a control that is on is filled with the accent — so whether the backdrop is on can be read off the panel rather than by trying it. That is the one place the "a control shows one thing at rest" rule is not followed, and deliberately: a rail block is aimed at, while a menu row is read and chosen between. A panel has no plate of its own: its rows are the control and the wallpaper shows between them, where the rail does keep its plate. The panel is anchored to the block that opened it: exactly one of its rows is level with that block — the row the pointer reaches by moving sideways out of it — and the rest grow down while they fit the rail's height and up when they do not, so the block and its list read as one control. While it is open the block owns its row as far as the panel's edge, and the four pixels between two rows belong to the row above them, so the pointer can walk from the block into the list and between its rows without ever crossing a point that would close it — the strip of the rail beside the panel belongs to the block that opened it, from the panel's first row to its last, so leaving the block at any height is still on the way in. The block itself takes no accent for having opened a panel: the accent means the item that is on in the list it belongs to, and a block that opened a menu is not armed. Its rows are its own two preferences — the backdrop and `boards: every screen` — and then whatever the rail had no room for, in the rail's own order, so a short output's drawer can be a long one. It closes like a menu: picking a control out of it both does the thing and puts the list away, and so does moving the pointer off it or pressing anywhere outside it — a click outside a menu dismisses it, and that is also what stops a menu from being open over a text box you have just started typing in. `M` still holds it open, and `Esc` closes it before anything else it does. The `boards` block opens a second panel of exactly that shape, in the same column and anchored to its own block; only one panel is open at a time, because the rail declares one box for the panel it repaints. Unlike the drawer, it leaves only when a board is picked, when the pointer presses anywhere outside it, or on `Esc`: moving the pointer off the list does not close it.

Icons are drawn as vector geometry by the same code that draws the shapes, so a circle's block is a circle and the five shape tools' blocks are the shapes themselves, not a picture of one. A symbol font would be a coverage gamble that ends in a tofu box on a machine whose fonts differ from this one, so there is no icon font and no image asset: the pen's icon is a stroke, the eraser's a slanted block with a band, the text tool's a `T`, the selection tool's a marquee with its corner grips, the size keys a `−` and a `+`, and `Esc` a padlock. The key badge is drawn in the block's own ink, in the corner the icon leaves free: a colour chip, whose face is the colour rather than a picture, takes the ink a note on that colour would use.

The five shape tools draw an **exact** shape from the drag rather than a smoothed freehand path, and each is stored as **two points and a kind**: a circle is the ellipse inscribed in the box the two points span, a rectangle is that box, a diamond is the rhombus whose corners are its edge midpoints, a line is the segment between the points, and an arrow is that segment with a filled head at the point the pointer ended on. A shape therefore costs two points in the file however large it is, it is stroked with the active palette colour and the pen width, and it is never filled — the arrowhead is the one filled part. A drag with no extent, or one with none across its box, draws nothing. While `Shift` is held the drag is constrained with the figure in mind: a line and an arrow take the nearest 15°, so horizontal, vertical and the diagonals are all reachable, and a circle, a rectangle and a diamond are measured on the longer side of the box the drag swept, so both sides come out equal. Letting `Shift` go mid-drag returns the second point to the pointer's own. A shape is an ordinary element afterwards: the eraser deletes it whole, a click or a marquee selects it, the selection moves, resizes and rotates it, and it cannot be edited corner by corner once it has landed.

A shape is hit by its **outline**, within the same 8 logical pixels a stroke is, and never from inside it: a stroke drawn inside a rectangle keeps its own clicks, and the eraser removes whichever of the two is nearer. A click so deep inside a circle that nothing is within 8 pixels of the rim removes nothing.

Until the pointer is released a shape is only drawn: the document still holds what it held, each frame damages only the shape's box, and `Esc`, a mode switch or a tool change abandons it with nothing written.

A **sticky note** (`N`) is a rounded square filled with the active palette colour: it is **drawn the way a shape is**, a drag sweeping out the rectangle that becomes the note, the press staying a corner of it and each side never smaller than the minimum, and a click with no drag leaves nothing behind — the caret goes in the new note, and `Shift` squares the drag the way it squares a rectangle, and the text wraps inside a 12 pt padding so it never touches the edge. The note **grows downwards** to hold what is written — it is at least 96 pt on a side, and taller once the wrapped text needs it — and the day it was made is drawn in the bottom-right corner as `yyyy/mm/dd`. Its text is drawn in an ink chosen from the background's brightness, because the palette reaches from a yellow that no light text can sit on to a red that no dark text can: every palette colour keeps at least 4.5:1 for the ink it is paired with. Clicking an existing note with the note or the text tool writes in it rather than stacking another one on top, and a note is an element like any other for the eraser, the selection, a marquee, a move and a resize — its text re-wraps at a new width rather than shrinking, and a resize's height is a minimum that the text overrides if it needs more room. Notes are painted **over** the strokes and shapes, so a drawing can never bury one, and the square grows with the text as it is typed rather than when the edit ends — never shrinking, so the height a note was dragged to is kept however little is written in it. An empty note stays until it is deleted, nothing is placed until the drag has moved, and its date keeps to the bottom-right corner at the note's own scale. Its date is **UTC**: the daemon has no timezone database, so late in the evening east of Greenwich a note carries tomorrow's date.

The **canvas is unbounded and per output**: annotations live at whatever coordinates they were drawn at, nothing is clamped to the output's edge, and there is no visible boundary. A **view** — a zoom and a pan — moves what you see over it, so a drawing bigger than the screen is reachable and so is one that runs off the edge:

| Gesture | Effect |
|---|---|
| touchpad two-finger swipe | pan, following the fingers |
| touchpad two-finger pinch | zoom about the pinch's focal point, and pan with it |
| mouse wheel | zoom about the pointer |
| middle button drag | pan, anchored where the press started |
| `H` and drag the hand tool | pan, anchored where the press started |

The zoom is multiplicative and clamped to 0.2–8×, so zooming in and back out returns you to where you started, and it **lands rather than jumping**: a wheel notch, a pinch and a `+`/`-` key ask for a new zoom factor, and the view eases to it over 140 ms with an ease-out, holding the point the gesture is anchored on under the pointer for the whole transition. A burst of events is therefore one continuous zoom that lands on the last target, and turning the wheel twenty times does not draw twenty jumps. A pan is never eased: it follows the pointer 1:1, which is what makes a grab feel like a grab. Everything the canvas holds scales with a zoom — strokes, their widths, text and text sizes, shapes — while the rail, its plate, the scrim, the selection's handles and the marquee keep their size and their place, because they are controls rather than content. The eraser's 8-pixel reach is what the pointer aims with, so it is a screen distance: zoomed out, it reaches further into the drawing's own units.

The view is **not saved**: reopening always starts at 1:1 from the origin, and it survives a mode switch, so locking and returning to edit mode leaves you where you were looking.

A canvas that is **moving fast** is drawn at half resolution, and a canvas that is still is drawn sharp: above roughly 900 logical pixels a second the drawing is rasterised at half size and each pixel is repeated twice, so a fast pan or zoom is coarse rather than blurry, and it returns to full resolution on its own a few frames after the pointer stops — you never have to move it again to get the sharp image back. The rail, the caret, the eraser marker and the selection's frame are always full resolution, and nothing you can point at moves: a click during a fast pan lands on the same canvas point it would have landed on while standing still.

At the foot of the rail is a **more** block (`M`): it opens a **drawer** — a second column beside the rail, holding the controls that are not used often. The backdrop switch is its first resident. A control in the drawer behaves exactly like a rail block: it has an icon, it says what it does and its key when the pointer is on it, it is hit the same way, and its key reaches it. The drawer does not make the rail taller, which is the point of it.

Each block is clickable, and a click does what the block's key does where it has one: a colour block picks that colour, `P`/`E`/`T`/`V`/`O`/`R`/`D`/`L`/`A` arm that tool, `[` and `]` — the two rows of the width list — step the active size, and `Esc` cancels what is in progress and locks on the next click.

While the pointer is on a block, that block opens away from the edge into a capsule that states what it does and ends with the key that does the same thing: `pen (P)`, `text: click to place (T)`, `select: drag to move (V)`, `smaller width ([)`, `cancel, then lock (Esc)`, `colour 3 (3)`. That is where the full shortcuts are taught — an alternate binding and a chord such as `Ctrl+Shift+V` are named here rather than squeezed into the square. 140 ms, ease-out, brief and in one direction. The label is uncovered by the edge as it moves, so the text is never drawn outside the shape that carries it; the other blocks stay squares and nothing moves under the pointer, which keeps the block it is on for as long as it stays on the capsule. Leaving a block closes it at once instead of animating back. Hovering is the only thing that repaints: it damages the rail's own box, not the whole output, at frame cadence.

Controls are laid out at the macOS metrics recorded in `CLAUDE.md`: 13 pt control text for both a glyph and its label, a 30 pt square block, 6 pt between a block and its label, and 12 pt of padding after a label, so text never touches the capsule's edge.

A press anywhere on the rail, or on the capsule a block has stretched into, belongs to it: it never starts a stroke, never erases and never places a text box, and the eraser marker is not drawn over it. The rail is transient: it is not part of the annotations, it cannot be erased, it does not widen the region a drag damages unless the drag reaches it, and it is not rendered while locked.

The pointer follows the armed tool through `wp_cursor_shape_v1`: a crosshair for the pen, a cell for the eraser, an I-beam for the text tool and the compositor's default arrow for the selection tool, which creates nothing of its own. With a selection held, the arrow becomes the resize shape of the handle under it, a grab shape over the rotation handle, and the protocol's held-gesture shape while a drag is in flight. The compositor's own cursor is used while the pointer is on the rail and once the daemon is locked again. A compositor that does not advertise the protocol keeps its own cursor and the daemon says so in its log.

## Links, moving and the clipboard

- A text box can link to a board. Select exactly one text box, open the drawer (`M`) and pick `link text to board`, then a board from the list. The text is underlined in the accent colour; `Ctrl`+click switches to that board and centres the text's canvas position. A plain click still selects or edits it. Deleting the target board clears the links that named it, and renaming the board moves them.
- With a selection held, `move to board` appears at the bottom centre of the canvas. It opens the same board list and moves every held element to the picked board, then lets go of the selection.
- `Ctrl+C` copies the held elements into sgraffito's own clipboard history; `Ctrl+X` cuts them — the same copy is recorded first, and only then are the originals removed as one change, so a clipboard that refuses an oversized copy removes nothing; `Ctrl+V` pastes the newest copy onto the board the screen shows, offset slightly and selected; `Ctrl+Shift+V` opens the history panel, where a row pastes that copy. Inside a text box, `Ctrl+C`/`Ctrl+V` keep the compositor's text semantics, and `Ctrl+X` does not cut: the buffer has a caret but no selected range. A board name takes its own keys too, so `Ctrl+X` cannot cut the canvas while a name is being edited.
- The clipboard is session state, never written to the file. It keeps at most 8 entries, 200 elements and 1 MB of JSON in total; adding a copy drops the oldest entries until all three hold. A copy that alone exceeds the element or byte cap is refused whole.

## Drawer and backdrop

- `more tools` (`M`) opens on a click and then stays open like the boards list: `Esc`, a press outside it, or a pick dismisses it. The pointer leaving it no longer closes it.
- `backdrop` (`B`) cycles transparent → grid → dots → dim → transparent. Its four mode buttons sit in one horizontal row to the right of the row, joined by a hairline at their middle; each button picks that mode and leaves the drawer open.
- The grid uses a 24-point cell with a one-point line; the dot matrix uses the same 24-point spacing with two-point dots. Both pan and zoom with the drawing and are painted mid-grey at partial opacity, so they read over light and dark wallpapers without dimming it. `dim` is the previous black 90% scrim.

## Logs

Errors and state changes go to stderr and to `$XDG_STATE_HOME/sgraffito/sgraffito.log` (default `~/.local/state/sgraffito/sgraffito.log`), the XDG location for state that survives a run.

- One file holds at most 1 MiB and three older files are kept, so the log never exceeds 4 MiB. 1 MiB × 3 rotations is the usual small-daemon bound; the size is checked as each line is written, and a file left over the cap is rotated when the daemon starts.
- Each event is one line: an ISO date and UTC time, `[sgraffito]`, then the message. Newlines in a message are escaped and a single message is capped at 8 KiB, so one large error cannot fill a generation.
- Panics are recorded with their location before the default panic output. If the file cannot be opened or written, the daemon keeps running and keeps logging to stderr.

## Connectors

- While a line, arrow or pen is snapped, every target shape's outline is repainted in the accent colour and each attached end gets a solid accent anchor dot. The feedback follows the connector while it is drawn and while an attached shape is dragged.
- A `line`, `arrow` or `pen` end that comes within 12 logical pixels of a `circle`, `rectangle` or `diamond` snaps to the nearest point on that shape's outline while it is drawn.
- The magnet also works the other way: dragging a circle, rectangle or diamond with `select` near a free `line`, `arrow` or `pen` end pulls the shape onto that end and leaves the connector attached when the drag is released.
- With `select`, dragging a connected shape stretches every attached connector live, even when the connector is not selected. Both ends stay on their shapes, so a line between two shapes follows whichever end moved.
- Attachments are saved with stable shape ids; deleting a shape clears the ends that pointed at it, and copying or moving shapes remaps their connectors to the copies.

## Data

`$XDG_DATA_HOME/sgraffito/annotations.json` (default `~/.local/share/sgraffito/`):

```json
{"version":6,
 "boards":{"office":{"strokes":[{"color":"#e01b24","width":3,"points":[[12,40],[14,44]],"attach":[null,null]}],
  "texts":[{"x":100,"y":200,"color":"#ffffff","size":18,"text":"buy milk","link":"desk"}],
  "shapes":[{"kind":"rect","color":"#3584e4","width":3,"from":[30,400],"to":[180,260],"rotate":0.4,"id":2,"attach":[null,null]},
            {"kind":"arrow","color":"#3584e4","width":3,"from":[180,430],"to":[400,430],"id":3,"attach":[{"shape":2,"at":[1.0,0.0]},null]}]}},
 "active":"office"}
```

A shape is a `kind` (`circle`, `rect`, `diamond`, `line` or `arrow`), its colour, its width and its two points — never its outline, and never a box. A closed shape may also carry `rotate`, in radians about its centre; upright shapes omit it. Closed shapes carry a stable board-local `id`; a line, arrow or pen's `attach` names the shapes its two ends stick to, with `at` the point on that shape (the centre is `(0, 0)` and its half-extents are `1`). A text's optional `link` is the board it jumps to; it is absent from files written before version 5. Versions 1 to 5 load as well: old shapes get fresh ids and no attachments, and a version 1–3 `outputs` map, keyed by output name, becomes boards of the same name. `active` is the board picked last — what a screen connected after a restart shows.

Version 7 was the text-attachment build. This build reads it too: the extra stroke `id` and text `attach` fields are ignored and dropped on the next save, which writes version 6. A file newer than 7, older than 1, or unreadable is backed up rather than guessed at. If the main file is missing and a readable `.bak` remains, the backup is copied back into place on startup, so a rollback does not leave the boards in the backup while the program starts empty. If a main file already exists and the boards are still missing, stop the daemon and copy `annotations.json.bak` over `annotations.json`; version 7 now loads directly, without editing its `version`.

- Coordinates are **canvas coordinates** — the canvas is unbounded and a board is one canvas — and annotations are bucketed by **board name**, not by output.
- Changes land on disk within 1 second at most, written through a temp file in the same directory plus `rename`; `clear` and shutdown flush immediately. If a write fails, the change stays pending and the daemon retries without dropping the in-memory annotations.
- A corrupt file or an unsupported version is renamed to `annotations.json.bak` and the daemon starts with no annotations.
- A board outlives the output that showed it: closing an output changes nothing, whatever was drawn on the board is still there, and the next output to appear adopts the board picked last.

## Configuration

Optional: `$XDG_CONFIG_HOME/sgraffito/config.toml` (default `~/.config/sgraffito/config.toml`). A missing file or an invalid value keeps the built-in default; the daemon says what it ignored.

```toml
# Sticky-note border thickness, in logical pixels. 0 draws no border.
border = 1.0

# The screen edge the edit-mode toolbar is docked to: "left" (the default), "right",
# "top" or "bottom". The value is matched case-insensitively; an invalid value keeps
# the built-in left edge and says so in the log. A left or right rail is a column, a
# top or bottom rail a row, and its panels always open toward the middle of the screen.
toolbar = "left"

# The zoom percentage and shared undo/redo card. One of "top left", "top right",
# "top center", "bottom left", "bottom right" (the default) or "bottom center".
cards = "bottom right"

# Exactly five palette colours, used by the rail's 1-5 chips and by new strokes, shapes,
# notes and text boxes. Hex (#rgb, #rgba, #rrggbb, #rrggbbaa), rgb()/rgba() and
# hsl()/hsla() are accepted and normalized to #rrggbb or #rrggbbaa.
colors = [
  "#e01b24",
  "rgb(246, 211, 45)",
  "hsl(130, 60%, 51%)",
  "rgba(53, 132, 228, 0.8)",
  "#ffffff",
]
```

The file is read once when the daemon starts.

## Known limitations

- Undo and redo are in memory only, so both are empty after a restart; there is no persisted history. There is no z-order, no layers, no PNG export and no pressure or touch support. A link points at a board and the linked text's position, not at a particular element inside that board; the clipboard history is session state, so it is lost when the daemon restarts. A cut is an ordinary undoable document change, but the clipboard side of it is not restored by undo.
- The toolbar is part of the full-screen edit overlay, not a surface of its own, so it cannot respect another layer surface's exclusive zone: a compositor bar or dock that occupies the edge the rail is docked to, or the optical-centre band, is covered by it while editing. Its edge comes from `toolbar` in the configuration file; it cannot be dragged.
- A block shows an icon of what it does and says what it does only while the pointer is on it, so a first-time user learns the rail by hovering, or from this README's key table. Each block's text ends with its key, and that is the only place a key appears: when a block's text does not fit the room reserved for the rail, the block stays a square showing its icon and no text, and its key is then only in this README. The longest text is `text: click to place (T)`, about 177 logical pixels of capsule, which fits the 320-pixel reserve on any output at least 336 pixels wide.
- A drawer shows the rows that fit the output. A very short screen folds almost the whole rail into it, and a panel taller than the screen would be rows nobody could click or read, so the rows nearest the `more` block are kept and the rest stay on their keys — a folded control never loses its key.
- A board can be created, picked, renamed and deleted. A delete is final — there is no undo behind it, and the button sits on the row of the board it removes, so a slip removes that board with everything drawn on it. A name is refused when it is empty or another board already has it: the board keeps the name it had. The last board cannot be deleted, because a document with no boards is nowhere to draw; the panel lists eight boards and `sgraffito boards` lists them all, because the panel is drawn inside a fixed damage box.
- Which board a screen shows is not saved, only which board was picked last: after a restart every screen starts on that board, so a per-screen setup has to be picked again. With the screens sharing one board there is nothing to pick again.
- The rail is only as tall as the output it is drawn on: the palette, the tools and the size keys fill from the top, `boards`, `more` and `Esc` always stay at its foot, and every control that does not fit is a row of the drawer instead — so a short output loses no control, at the price of the tools it cannot show being one click (or one key) away. There is no scrolling rail. The rail sits on a black plate at 90% opacity, so it reads as one control over any wallpaper: the plate is the block column plus 6 pt on every side, and a block stretched into a capsule is wider than the plate, so a capsule reaches out over the wallpaper by design.
- The hand-drawn style is fixed: there is no roughness setting, no way to switch it off, and no clean-line mode. It is not a configuration option either, so a drawing made before the style existed opens in it.
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
- Paste is plain text only, the best of `text/plain;charset=utf-8`, `UTF8_STRING` and `text/plain`, capped at 256 KiB, and only into a text box that is being edited: nothing is written to the compositor's clipboard (the app's `Ctrl`+`C`/`Ctrl`+`X` use an in-app history instead), there is no image paste, and `Ctrl`+`V` reaches the input method first while a preedit is active, like every other local key. Pasting waits for the selection's owner to write, which happens off the event loop, so a slow clipboard delays only the paste.
- Arrows are one segment with one head, drawn at the end the pointer stopped on; a double-headed arrow, a head at the start, a bend and a snapped connector to the element it points at are not implemented. A shape is stroked and never filled, and there is no fill colour.
- A shape's hit test measures a flattened outline (a 96-gon for a circle) while the frame draws the real curve, so the two differ by well under a pixel against an 8-pixel tolerance.
- A text box's size is fixed when it is created; there is no per-box resize, so changing the size means creating a new box, and the only way to change it afterwards is to resize or step a selection that holds it. Clicking a box puts the caret at its end; moving the caret inside with the mouse, or by word, by `Home`/`End` or by `PageUp`/`PageDown`, is not implemented.
- A text box has no angle of its own, so a rotation moves it around the frame's centre with its glyphs upright instead of turning it. Turning the glyphs would need an angle per text box in the file, and so a version 3 annotation file.
- The selection is a tool's state, not something the document remembers: there is no group, so what a gesture holds is recomputed from the geometry every frame, and a shift-click adds or removes one element, so a multi-selection is built either by a marquee in one sweep or one element at a time. Elements cannot be resized on their own either: a resize of a selection scales its geometry rather than thickening its pen, so a stroke's or shape's width and a text box's size change through `[` and `]` on the selection and not through a handle, and nothing can be resized from a corner of its own.
- The keyboard selects, moves, opens, recolours and deletes what is drawn, but it cannot **create** anything: a stroke, a shape, a note or a text box where there was none still needs a pointer, so a keyboard-only session can edit a drawing and not start one. It cannot resize or rotate a selection either — the arrows translate only — and `Tab` inside a text box is content, so `Esc` is the way out of one. The canvas keys act on the output the compositor gave the keyboard to, which with more than one output in edit mode is the compositor's choice rather than the user's.
- The pop-out is the only animation, it lasts 140 ms, and it runs in one direction only: leaving a block closes its capsule in the same frame instead of animating back. Nothing consumes Reduce Motion — a Wayland client has no equivalent of the macOS setting — so the animation cannot be turned off from the compositor's accessibility settings.
- The pointer shapes are the named ones of `wp_cursor_shape_v1`, so the eraser gets the closest named shape (a cell) rather than an eraser image; there are no custom cursor images. A compositor without the protocol keeps its own cursor.
- The pointer **and the keyboard** are exercised for real: `scripts/smoke.sh` builds `examples/vinject.rs`,
which injects motion, presses and drags through `zwlr_virtual_pointer_manager_v1` and keys through
`zwp_virtual_keyboard_manager_v1` (sway advertises both), so a click on a rail block, a drag that draws a
stroke, the eraser's hit test, a note created by its own drag, a selection drag, and a `Tab` → arrow →
`Enter` → `Delete` round trip on the keyboard are checked against the file *and* the surface, not only
against a unit test. What that still cannot reach is input from a real device or an input method; the
wheel and the pinch, which need axis and gesture injection; the rail's drawer, which is off the bottom of
the smoke test's 600-pixel outputs; and the resize and rotation handles, which stay unit only.

**Unverified**: `exclusive` keyboard and `text-input-v3` on niri / hyprland / river, the fcitx5 preedit/commit flow, real-compositor output-name/closed-surface lifecycle events, which output the compositor gives the keyboard when several are in edit mode (the keyboard's canvas keys follow it, and the smoke test clicks the output it then drives), and input from a real device: `scripts/smoke.sh` runs sway with `WLR_LIBINPUT_NO_DEVICES=1`, so both devices there are the ones this repository injects. The rail's shape and a committed shape are checked by screenshot, but the pointer shape, the hover, the caret's movement to a place other than the end, clipboard paste, most of the selection tool (the smoke test injects a real pointer and checks a press that picks an element and a drag that moves it end to end, while the marquee, the resize handles and the rotation handle remain unit and geometry only), the whole of the five shape tools, whose shapes are only ever drawn by a drag, and every canvas gesture — the two-finger swipe, the wheel, the middle button, the pinch and the hand tool — are covered by unit tests and geometry checks only: their wiring to the compositor is what a real session has to confirm. The preedit is now cleared per input-method batch and `surrounding text` uses UTF-8 byte offsets, but real compositor and input-method sessions are what confirm those paths. The boards are covered from both ends — the migration, `clear`, the four `sgraffito` commands and the picked board surviving a switch are exercised on two headless outputs by `scripts/smoke.sh`, and the panel's rows, box and hit test by unit and geometry tests — but **clicking a board in the panel, or the row that deletes one, and watching another output follow** is what a real session with two monitors has to confirm (the delete row's own click is covered by `scripts/smoke-delete-row.sh`, on one headless output), as is a per-screen setup being picked again after a restart.

## Development

```sh
cargo test                # pure logic unit tests: data model, hit testing, JSON, rendering, key mapping, command parsing
cargo bench --bench render # release render measurements: sparse/dense drag, rail, hovered rail, text, scale, pan/zoom at 2K and 4K
scripts/smoke.sh          # headless sway end to end: rendering, mode switching, the rail, IPC, clear, single instance, graceful exit
scripts/smoke-delete-row.sh # headless sway + windhover: warp onto the boards panel's delete row, click it, and check the board leaves the document and the file
```

`scripts/smoke-delete-row.sh` needs a windhover checkout — the sibling `../windhover`, or `WINDHOVER=/path` — and sway, grim and python3 like the smoke test. It reads the delete row's position from a screenshot, computes the hint label for a cell over it, and has windhover's hint mode warp there and click: the pointer path is a separate client's real warp and button, not the test injector's hardcoded coordinate.

The render benchmark uses the standard library only and reports median/p95 frame times, the view the frame was drawn at, the rung it was drawn at, whether the document cache was warm, and the optimisation level the build used. It covers sparse and dense pointer drags, whole-surface frames, the rail, hover, text editing, and a `view-change` case — the frame a pan or a zoom produces — at both 2560x1440 and 3840x2160, at full resolution and at the motion rung. The baseline below was collected on Arch Linux, Intel Xeon E3-1220L v2, Rust 1.98.1, 2026-11-09, with the release profile: 4K sparse drag 90/127 us, 4K drag over 200 overlapping strokes 91/134 us, 4K whole-surface frame over a dense document 8.8/9.4 ms, 4K whole-surface rail frame 10.3/11.6 ms, 4K rail frame that rebuilds the cache 10.4/13.0 ms, 4K frame of the pop-out with its box damaged 2.4/2.6 ms, 1080p text edit drawn whole 2.2/2.5 ms, 1080p keystroke with its box damaged 95/132 us, 4K frame after a commit over a dense document 16.4/17.8 ms, a scale-2 whole-surface frame 8.0/8.9 ms, a 4K pan or zoom frame 16.7/19.6 ms at full resolution and 12.5/13.7 ms at the motion rung, and the same frame at 2560x1440 9.9/11.0 ms at full resolution and 7.2/8.4 ms at the rung (median/p95). Repeat runs on this machine move by up to 20%, so these are comparison numbers and not hardware-independent thresholds. A whole-surface frame is the expensive case: it fills the whole output and draws the document, and only a view change, a mode switch, a resize or scale change, `clear` or a fresh surface has to pay it. The frames that follow the pointer and the keyboard damage a box and cost between 90 us and 2.5 ms.
