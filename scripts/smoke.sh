#!/usr/bin/env bash
# End-to-end smoke test for sgraffito on headless sway.
# Usage: scripts/smoke.sh [path to the binary]
# The seeded document uses CJK text on purpose, to check multi-byte rendering.
set -euo pipefail

bin=${1:-target/debug/sgraffito}
bin=$(realpath "$bin")
work=$(mktemp -d /tmp/sgraffito-smoke.XXXXXX)
sway_pid=""
daemon_pid=""
slow_pid=""

cleanup() {
    [ -n "$slow_pid" ] && kill "$slow_pid" 2>/dev/null || true
    [ -n "$daemon_pid" ] && kill "$daemon_pid" 2>/dev/null || true
    [ -n "$sway_pid" ] && kill "$sway_pid" 2>/dev/null || true
    wait 2>/dev/null || true
    rm -rf "$work"
}
trap cleanup EXIT

export XDG_RUNTIME_DIR="$work/rt"
export XDG_DATA_HOME="$work/data"
unset WAYLAND_DISPLAY
export WLR_BACKENDS=headless
# The second output carries scale 2, so the scale path is covered too.
export WLR_HEADLESS_OUTPUTS=2
export WLR_LIBINPUT_NO_DEVICES=1
mkdir -p "$XDG_RUNTIME_DIR" "$XDG_DATA_HOME/sgraffito"
chmod 700 "$XDG_RUNTIME_DIR"

printf 'output HEADLESS-1 resolution 800x600\noutput HEADLESS-2 resolution 800x600 scale 2\n' > "$work/sway.conf"
sway -c "$work/sway.conf" > "$work/sway.log" 2>&1 &
sway_pid=$!

# sway picks its own socket name; wait for it, then pass it on to the clients
sock=""
for _ in $(seq 50); do
    sock=$(find "$XDG_RUNTIME_DIR" -maxdepth 1 -name 'wayland-*' ! -name '*.lock' -printf '%f\n' 2>/dev/null | head -1)
    [ -n "$sock" ] && break
    sleep 0.1
done
[ -n "$sock" ] || { echo "FAIL: sway did not start"; cat "$work/sway.log"; exit 1; }
export WAYLAND_DISPLAY="$sock"

cat > "$XDG_DATA_HOME/sgraffito/annotations.json" <<'JSON'
{"version":3,"outputs":{"HEADLESS-1":{"strokes":[{"color":"#e01b24","width":8,"points":[[100,300],[700,300]]}],"texts":[{"x":100,"y":100,"color":"#ffffff","size":40,"text":"hi 你好"}],"shapes":[{"kind":"circle","color":"#33d17a","width":6,"from":[500,430],"to":[700,560]}],"notes":[{"x":300,"y":180,"w":220,"h":120,"color":"#f6d32d","text":"a sticky note that wraps its text inside the square","created":"2026/10/14"}]},"HEADLESS-2":{"strokes":[{"color":"#e01b24","width":8,"points":[[50,75],[150,75]]}],"texts":[]}}}
JSON

"$bin" daemon > "$work/daemon.log" 2>&1 &
daemon_pid=$!

fail() { echo "FAIL: $1"; echo "--- daemon.log"; cat "$work/daemon.log"; exit 1; }

for _ in $(seq 50); do
    grep -q "mode switched\|layer surface created" "$work/daemon.log" 2>/dev/null && break
    sleep 0.1
done
sleep 1
kill -0 "$daemon_pid" 2>/dev/null || fail "daemon exited early"

# 1. annotations on disk are rendered while locked
"$bin" lock > /dev/null
sleep 0.5
grim -t ppm "$work/locked.ppm" 2>/dev/null || fail "grim screenshot failed"
python3 - "$work/locked.ppm" <<'PY' || exit 1
import sys
d = open(sys.argv[1], 'rb').read()
px = d[d.index(b'255\n') + 4:]
red = sum(1 for j in range(0, len(px), 3) if px[j] > 120 and px[j+1] < 80 and px[j+2] < 80)
ink = sum(1 for j in range(0, len(px), 3) if max(px[j:j+3]) > 20)
# A circle is stroked, never filled: its rim is ink and its middle is not.
green = sum(1 for j in range(0, len(px), 3) if px[j+1] > 120 and px[j] < 100 and px[j+2] < 120)
assert red > 1000, f"no red line drawn while locked (red={red})"
assert ink > red, f"text was not rendered (ink={ink}, red={red})"
assert green > 200, f"the shape was not rendered (green={green})"
# The note's own colour, so a note that did not render cannot hide behind the wallpaper.
note = sum(1 for j in range(0, len(px), 3)
           if px[j] > 200 and px[j + 1] > 170 and px[j + 2] < 120)
assert note > 2000, f"the sticky note was not rendered (note={note})"
print(f"  rendering ok: red={red} green={green} note={note} ink={ink}")
PY

# 1b. the output scale reaches the compositor: on the scale-2 output a stroke between logical
# (50,75) and (150,75) has to land at physical x 100..300 on row 150, not twice as far out.
grim -o HEADLESS-2 -t ppm "$work/scale2.ppm" 2>/dev/null || fail "grim on the scale-2 output failed"
python3 - "$work/scale2.ppm" <<'PY' || exit 1
import sys
d = open(sys.argv[1], 'rb').read()
hdr = d[:d.index(b'255\n') + 4].split()
w, h = int(hdr[1]), int(hdr[2])
px = d[d.index(b'255\n') + 4:]
red = [(x, y) for y in range(h) for x in range(w)
       if px[(y * w + x) * 3] > 120 and px[(y * w + x) * 3 + 1] < 80 and px[(y * w + x) * 3 + 2] < 80]
assert red, "no red line on the scale-2 output"
xs = [p[0] for p in red]
ys = [p[1] for p in red]
assert 90 <= min(xs) and max(xs) <= 310, f"line x {min(xs)}..{max(xs)} is not 100..300: the scale is not applied"
assert 140 <= min(ys) and max(ys) <= 160, f"line y {min(ys)}..{max(ys)} is not at 150: the scale is not applied"
print(f"  scale 2 ok: red x {min(xs)}..{max(xs)} y {min(ys)}..{max(ys)}")
PY

# 1c. an idle daemon does not poll: the control thread waits in `accept()`, so the only wakeup
# left is the loop's own 200 ms timer. Measured over a quiet window, the processor time it
# accumulates is a fraction of a millisecond per second; the poll this replaced cost 8 ms/s.
cpu_before=$(awk '{print $14 + $15}' "/proc/$daemon_pid/stat")
sleep 5
cpu_after=$(awk '{print $14 + $15}' "/proc/$daemon_pid/stat")
idle_ms=$(( (cpu_after - cpu_before) * 1000 / $(getconf CLK_TCK) ))
[ "$idle_ms" -le 30 ] || fail "the daemon used ${idle_ms}ms of processor time in 5 idle seconds (polling?)"
echo "  idle cost ok: ${idle_ms}ms over 5s"

# 2. mode switching is idempotent
for cmd in toggle toggle edit lock lock; do
    out=$("$bin" "$cmd") || fail "$cmd exited non-zero"
    [ "$out" = "ok" ] || fail "$cmd returned '$out'"
done
grep -q "mode switched to Edit" "$work/daemon.log" || fail "edit mode was never entered"
kill -0 "$daemon_pid" 2>/dev/null || fail "daemon died after switching modes"
echo "  mode switching ok"

# 2b. the pointer follows the tool through wp_cursor_shape_v1; sway advertises it, so a
# compositor without the global is the only case that logs the fallback instead.
grep -q "wp_cursor_shape_v1 available" "$work/daemon.log" \
    || fail "the pointer shape protocol was not bound"
echo "  pointer shape protocol ok"

# 2c. while editing, the toolbar is a vertical rail on the left edge: it paints two separated
# bands of the left strip, and nothing to the right of that strip in the top rows (where a
# horizontal bar used to sit). The seeded annotations start at x=100, so the strip x<60 can only
# hold the rail. This output is shorter than the rail, so the rail's top is the top margin and its
# bottom is what falls off the output.
"$bin" edit > /dev/null || fail "edit failed"
sleep 0.5
grim -o HEADLESS-1 -t ppm "$work/toolbar.ppm" 2>/dev/null || fail "grim on the toolbar failed"
python3 - "$work/toolbar.ppm" <<'PY' || exit 1
import sys
d = open(sys.argv[1], 'rb').read()
hdr = d[:d.index(b'255\n') + 4].split()
w, h = int(hdr[1]), int(hdr[2])
px = d[d.index(b'255\n') + 4:]
def ink(x0, y0, x1, y1):
    return sum(1 for y in range(y0, y1) for x in range(x0, x1)
               if max(px[(y * w + x) * 3:(y * w + x) * 3 + 3]) > 20)
high = ink(0, int(h * 0.14), 60, int(h * 0.20))
low = ink(0, int(h * 0.62), 60, int(h * 0.68))
rail_top = ink(0, 0, 60, 20)
right = ink(60, 0, w, 20)
assert high > 100 and low > 100, f"no vertical rail on the left edge (high={high} low={low})"
assert rail_top > 0, "the rail is pinned to the margin, so it reaches the top rows"
assert right == 0, f"the top strip is not clear to the right of the rail (ink={right})"
print(f"  rail ok: high={high} low={low} top={rail_top}")
PY
"$bin" lock > /dev/null || fail "lock failed"

# 3. an unknown command changes nothing and reports an error
if "$bin" 2>/dev/null; then fail "no arguments should exit non-zero"; fi
if printf 'frobnicate\n' | timeout 3 python3 -c "
import socket,os,sys
s=socket.socket(socket.AF_UNIX); s.connect(os.environ['XDG_RUNTIME_DIR']+'/sgraffito.sock')
s.sendall(b'frobnicate\n'); print(s.recv(200).decode().strip())
" | grep -q '^ok'; then fail "an unknown command returned ok"; fi
echo "  unknown command ok"

# 3b. an incomplete client does not block another command
python3 - "$XDG_RUNTIME_DIR/sgraffito.sock" <<'PY' &
import socket
import sys
import time
s = socket.socket(socket.AF_UNIX)
s.connect(sys.argv[1])
time.sleep(3)
PY
slow_pid=$!
sleep 0.1
timeout 1 "$bin" lock >/dev/null || fail "an incomplete client blocked the control socket"
kill "$slow_pid" 2>/dev/null || true
wait "$slow_pid" 2>/dev/null || true
slow_pid=""
echo "  incomplete client is non-blocking"

# 3c. oversized commands are rejected
response=$(python3 - "$XDG_RUNTIME_DIR/sgraffito.sock" <<'PY'
import socket
import sys
s = socket.socket(socket.AF_UNIX)
s.connect(sys.argv[1])
s.sendall(b"x" * 5000)
print(s.recv(200).decode().strip())
PY
)
case "$response" in
    error:*) ;;
    *) fail "oversized command returned '$response'" ;;
esac
echo "  oversized command rejected"

# 4. clear empties memory and file
"$bin" clear > /dev/null || fail "clear failed"
sleep 0.3
python3 -c "
import json,os
p=os.environ['XDG_DATA_HOME']+'/sgraffito/annotations.json'
d=json.load(open(p))
assert d['version']==3, d['version']
assert d['outputs']=={}, d
print('  file empty after clear ok')
"
grim -t ppm "$work/clear.ppm" 2>/dev/null
python3 - "$work/clear.ppm" <<'PY' || exit 1
import sys
d = open(sys.argv[1], 'rb').read()
px = d[d.index(b'255\n') + 4:]
ink = sum(1 for j in range(0, len(px), 3) if max(px[j:j+3]) > 20)
assert ink == 0, f"the screen still has content after clear (ink={ink})"
print("  screen empty after clear ok")
PY

# 5. injected pointer input. A headless seat has no pointer at all, so the paths only a pointer reaches —
# the hover, a click on a block, a drag that draws, the eraser's hit test, a note's own drag, a selection
# drag — are given one through `zwlr_virtual_pointer_manager_v1`, which sway advertises. Coordinates are the
# layout's: a point on HEADLESS-1 is its own x plus the 400 the other output takes, and a rail block is at
# the y the rail lays it out at, its centre 15 below that row's top (the rows `toolbar_pill` computes, which
# `the_panel_clears_the_block_that_opens_it` and the debug assertion on `TOOLBAR_COLUMN` keep honest).
# The locked surface is click-through — the input region is empty — so the pointer has to have a mode to
# act in: enter edit mode and let the surfaces that replace it be mapped.
"$bin" edit > /dev/null || fail "edit before the pointer stage failed"
sleep 0.8
cargo build --example vinject >/dev/null 2>&1 || fail "the pointer injector did not build"
vinject=target/debug/examples/vinject
layout=1200x600
h1=400
rail=$((h1 + 23))
annotations="$XDG_DATA_HOME/sgraffito/annotations.json"
kinds() {
    python3 -c "
import json
ann = json.load(open('$annotations'))['outputs'].get('HEADLESS-1', {})
print(' '.join(f'{kind}={len(ann.get(kind, []))}' for kind in ('strokes', 'texts', 'shapes', 'notes')))
"
}

# 5a. the blue chip arms the colour, the pen's block arms the pen, and the drag draws a stroke that reaches
# both the file and the surface.
"$vinject" "$layout" seq <<VINJECT
click $rail 131
click $rail 205
drag $((h1 + 50)) 300 $((h1 + 250)) 420 8
sleep 1500
VINJECT
[ "$(kinds)" = "strokes=1 texts=0 shapes=0 notes=0" ] || fail "the injected drag left $(kinds)"
python3 - "$annotations" <<PY || exit 1
import json, sys
stroke = json.load(open(sys.argv[1]))["outputs"]["HEADLESS-1"]["strokes"][0]
assert stroke["color"] == "#3584e4", f"the colour chip did not arm: {stroke['color']}"
first, last = stroke["points"][0], stroke["points"][-1]
assert abs(first[0] - 50) < 8 and abs(last[1] - 420) < 8, stroke["points"]
print(f"  injected stroke ok: {stroke['color']} {first} -> {last}")
PY
grim -o HEADLESS-1 -t ppm "$work/pointer.ppm" 2>/dev/null || fail "grim after the injected drag failed"
python3 - "$work/pointer.ppm" <<PY || exit 1
import sys
d = open(sys.argv[1], 'rb').read()
hdr = d[:d.index(b'255\n') + 4].split()
w, h = int(hdr[1]), int(hdr[2])
px = d[d.index(b'255\n') + 4:]
# The blue chip's stroke over the 90% scrim: its blue dominates its red, which the scrim's grey does not.
ink = sum(1 for y in range(290, 440) for x in range(40, 270)
          if px[(y * w + x) * 3 + 2] > px[(y * w + x) * 3] + 40)
assert ink > 50, f"the stroke was not painted on the surface (ink={ink})"
print(f"  injected stroke painted: ink={ink}")
PY

# 5b. the eraser's hit test, through a pointer: arm it by its block and click the stroke away.
"$vinject" "$layout" seq <<VINJECT
click $rail 239
click $((h1 + 150)) 360
sleep 1500
VINJECT
grep -q "tool switched to Eraser" "$work/daemon.log" || fail "the eraser's block was not clicked"
[ "$(kinds)" = "strokes=0 texts=0 shapes=0 notes=0" ] || fail "the eraser left $(kinds)"

# 5c. a note is created by the drag that sizes it. This is the path that silently wrote nothing until the
# gesture was wired to the document, and the drag's own size — not the note's default — is what proves it.
"$vinject" "$layout" seq <<VINJECT
click $rail 545
drag $((h1 + 120)) 120 $((h1 + 300)) 260 8
sleep 1500
VINJECT
grep -q "tool switched to Note" "$work/daemon.log" || fail "the note's block was not clicked"
python3 - "$annotations" <<PY || exit 1
import json, sys
notes = json.load(open(sys.argv[1]))["outputs"]["HEADLESS-1"]["notes"]
assert len(notes) == 1, f"{len(notes)} notes"
note = notes[0]
for field, want in (("x", 120), ("y", 120), ("w", 180), ("h", 140)):
    assert abs(note[field] - want) < 10, f"the drag did not size the note: {note}"
print(f"  injected note ok: {note['x']},{note['y']} {note['w']}x{note['h']}")
PY

# 5d. a selection drag moves what the frame holds. The press is resolved in screen space and the gesture
# works in canvas ones, so a mix-up between the two is exactly what a drag that drifts looks like.
"$vinject" "$layout" seq <<VINJECT
click $rail 307
drag $((h1 + 200)) 190 $((h1 + 260)) 250 8
sleep 1500
VINJECT
grep -q "tool switched to Selection" "$work/daemon.log" || fail "the selection's block was not clicked"
python3 - "$annotations" <<PY || exit 1
import json, sys
note = json.load(open(sys.argv[1]))["outputs"]["HEADLESS-1"]["notes"][0]
assert abs(note["x"] - 180) < 14 and abs(note["y"] - 180) < 14, f"the drag moved it to {note['x']},{note['y']}"
assert abs(note["w"] - 180) < 14 and abs(note["h"] - 140) < 14, "a move resized the note"
print(f"  injected selection drag ok: {note['x']},{note['y']}")
PY

# 5e. the keyboard alone: `Tab` selects, the arrows move what is held, `Delete` removes it. The click on
# empty space is also what gives that output's surface the keyboard focus — every edit-mode surface is
# `exclusive`, so exactly one output has it — and it clears the selection the drag above left behind.
# One press moves a held element by 1 logical pixel and ten while `Shift` is held, so the note the drag
# put at 180 moves to 180 + 1 + 1 + 10.
"$vinject" "$layout" seq <<VINJECT
click $((h1 + 700)) 500
sleep 200
key Tab
key Right
key Right
key shift+Right
sleep 1500
VINJECT
python3 - "$annotations" <<PY || exit 1
import json, sys
notes = json.load(open(sys.argv[1]))["outputs"]["HEADLESS-1"]["notes"]
assert len(notes) == 1, f"{len(notes)} notes"
note = notes[0]
assert abs(note["x"] - 192) < 2, f"the arrows did not move it by their steps: {note['x']}"
assert abs(note["y"] - 180) < 2, f"an arrow moved it on the other axis: {note['y']}"
print(f"  keyboard nudge ok: {note['x']},{note['y']}")
PY

# and `Enter` opens the held note for writing, which is the only way into a committed box without a
# click: the caret lands at the end, what is typed lands in the note when the edit ends, and `Esc` ends
# the edit while the selection stays held.
"$vinject" "$layout" seq <<VINJECT
key Return
sleep 300
key a
key b
sleep 200
key Escape
sleep 1500
VINJECT
python3 - "$annotations" <<PY || exit 1
import json, sys
note = json.load(open(sys.argv[1]))["outputs"]["HEADLESS-1"]["notes"][0]
assert note["text"] == "ab", f"Enter did not open the note for writing: {note['text']!r}"
print(f"  keyboard note edit ok: {note['text']!r}")
PY

# and the same key with nothing held removes nothing: a second `Esc` drops the selection, and `Delete` on
# an empty selection is inert. A press on empty space does not clear a selection — a marquee only decides
# one when it is released after moving — so `Esc` is what the keyboard has for this.
"$vinject" "$layout" seq <<VINJECT
key Escape
sleep 300
key Delete
sleep 1200
VINJECT
[ "$(kinds)" = "strokes=0 texts=0 shapes=0 notes=1" ] || fail "Delete with nothing selected removed $(kinds)"
"$vinject" "$layout" seq <<VINJECT
key Tab
key Delete
sleep 1500
VINJECT
[ "$(kinds)" = "strokes=0 texts=0 shapes=0 notes=0" ] || fail "the keyboard left $(kinds)"
"$bin" lock > /dev/null || fail "lock after the pointer stage failed"

# 6. a second daemon is rejected
if "$bin" daemon > /dev/null 2>&1; then fail "a second daemon started"; fi
echo "  single instance ok"

# 7. graceful exit on SIGTERM
kill -TERM "$daemon_pid"
for _ in $(seq 50); do kill -0 "$daemon_pid" 2>/dev/null || break; sleep 0.1; done
wait "$daemon_pid" || fail "daemon exit code was non-zero"
daemon_pid=""
[ -S "$XDG_RUNTIME_DIR/sgraffito.sock" ] && fail "socket file left behind"
echo "  graceful exit ok"

echo "PASS: sgraffito smoke test passed"
