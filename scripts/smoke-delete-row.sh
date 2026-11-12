#!/usr/bin/env bash
# End-to-end check of the boards panel's `delete <name>` row: a real pointer
# client warps onto the row and clicks it, and the board has to leave the
# document, the panel, and the annotation file.
#
# The pointer client is windhover (https://github.com/codethare/windhover), the
# keyboard-driven mouse: a session is driven through a virtual keyboard, it draws
# a fullscreen click-through overlay, and `hint` mode warps onto a labelled cell
# and clicks it in one batch. Nothing here clicks a hardcoded coordinate: the
# row's position is read from a screenshot, and the hint label for the cell over
# it is computed from windhover's own lattice and label assignment.
#
# Two facts about the pair make the shape of this script what it is:
#
# - The edit overlay holds the keyboard exclusively, and windhover's overlay is
#   click-through. On the first pointer motion sway re-arbitrates the two
#   exclusive layer surfaces and takes the keyboard back from windhover, which
#   ends its session — so walking the pointer to the row with h/j/k/l cannot
#   work, and `hint` mode's single warp-and-click is the way in.
# - A client that maps a layer surface over sgraffito makes the compositor
#   re-send the pointer position sgraffito last heard, and an open panel closes
#   when that position is off it. So the pointer is parked on the delete row
#   before windhover starts: the panel survives the map, and the warp that
#   follows lands on the row it already holds.
#
# Usage: scripts/smoke-delete-row.sh [path to a windhover checkout]
# WINDHOVER may also name it in the environment; the default is the sibling
# checkout `../windhover`. Needs sway, grim and python3, like `scripts/smoke.sh`.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
wh=${1:-${WINDHOVER:-$root/../windhover}}
[ -d "$wh" ] || { echo "FAIL: no windhover checkout at $wh (pass one, or set WINDHOVER)"; exit 1; }

echo "building"
cargo build --quiet --manifest-path "$root/Cargo.toml"
cargo build --quiet --manifest-path "$root/Cargo.toml" --example vinject
cargo build --quiet --manifest-path "$wh/Cargo.toml" --bin windhover --example virtual_keyboard

bin=$root/target/debug/sgraffito
vinject=$root/target/debug/examples/vinject
wh_bin=$wh/target/debug/windhover
kbd=$wh/target/debug/examples/virtual_keyboard

work=$(mktemp -d /tmp/sgraffito-delete-row.XXXXXX)
pids=()
keep=0
cleanup() {
    for p in "${pids[@]:-}"; do kill "$p" 2>/dev/null || true; done
    wait 2>/dev/null || true
    if [ "$keep" = 0 ]; then rm -rf "$work"; else echo "work kept: $work"; fi
}
trap cleanup EXIT

export XDG_RUNTIME_DIR="$work/rt"
export XDG_DATA_HOME="$work/data"
unset WAYLAND_DISPLAY
export WLR_BACKENDS=headless
export WLR_HEADLESS_OUTPUTS=1
export WLR_LIBINPUT_NO_DEVICES=1
export WLR_RENDERER=pixman
mkdir -p "$XDG_RUNTIME_DIR" "$XDG_DATA_HOME/sgraffito"
chmod 700 "$XDG_RUNTIME_DIR"

printf 'output HEADLESS-1 resolution 800x600\n' > "$work/sway.conf"
sway -c "$work/sway.conf" > "$work/sway.log" 2>&1 &
pids+=("$!")
sock=""
for _ in $(seq 50); do
    sock=$(find "$XDG_RUNTIME_DIR" -maxdepth 1 -name 'wayland-*' ! -name '*.lock' -printf '%f\n' 2>/dev/null | head -1)
    [ -n "$sock" ] && break
    sleep 0.1
done
[ -n "$sock" ] || { echo "FAIL: sway did not start"; cat "$work/sway.log"; exit 1; }
export WAYLAND_DISPLAY="$sock"

# Two empty boards, the second one picked and on screen: the panel's delete row names it.
cat > "$XDG_DATA_HOME/sgraffito/annotations.json" <<'JSON'
{"version":4,"boards":{"one":{"strokes":[],"texts":[],"shapes":[],"notes":[]},"two":{"strokes":[],"texts":[],"shapes":[],"notes":[]}},"active":"two"}
JSON

# A fixed hint lattice: 20 x 20 cells of 40 x 30 logical pixels, and a click after the warp.
printf 'hint_cell: 30\nclick_after_warp: yes\n' > "$work/wh.conf"

"$bin" daemon > "$work/daemon.log" 2>&1 &
pids+=("$!")
sleep 1
"$bin" edit > /dev/null
sleep 0.8

fail() {
    echo "FAIL: $1"
    echo "--- daemon"; tail -20 "$work/daemon.log"
    echo "--- windhover"; tail -20 "$work/wh.log" 2>/dev/null || true
    keep=1
    exit 1
}

# A press gives the edit overlay the keyboard; with no motion it leaves a dot, nothing else.
"$vinject" 800x600 seq > /dev/null <<'VINJECT'
click 700 500
sleep 300
VINJECT

# One long-lived virtual keyboard: a headless seat has none, and windhover refuses to start without.
mkfifo "$work/inject"
"$kbd" --stdin < "$work/inject" > "$work/kbd.log" 2>&1 &
pids+=("$!")
exec 3> "$work/inject"
sleep 0.5

tap() { printf '%s\n' "tap:$1" >&3; sleep "${2:-0.25}"; }

# `c` opens the boards panel; two boards means the delete row exists.
tap c 0.8
grim -t ppm -o HEADLESS-1 "$work/panel.ppm" 2>/dev/null || fail "grim failed"

read -r label x y < <(python3 - "$work/panel.ppm" <<'PY'
import sys

# Mirrors the lattice and `assign` in windhover's src/hint: `hint_cell: 30` over
# 800x600 gives 20x20 cells of 40x30, capped at the alphabet, and every target
# gets a two-character label because 20 x 20 names exactly 400 of them.
charset = "asdfjkl;ghqwertyuiop"
columns = rows = 20
cell_w, cell_h = 800 / columns, 600 / rows

d = open(sys.argv[1], 'rb').read()
body = d.index(b'255\n') + 4
head = d[:body].split()
w, h = int(head[1]), int(head[2])
px = d[body:]


def bright(x, y):
    i = (y * w + x) * 3
    return max(px[i], px[i + 1], px[i + 2]) > 120


counts = [sum(1 for x in range(44, 320) if bright(x, y)) for y in range(h)]
bands = []
start = None
for y, n in enumerate(counts):
    if n >= 4 and start is None:
        start = y
    elif n < 4 and start is not None:
        bands.append((start, y - 1))
        start = None
if start is not None:
    bands.append((start, h - 1))

if len(bands) != 4:
    sys.exit(f"expected four panel rows, found {bands}")

band = bands[-2]  # the row above `new board` is `delete <name>`
mid = (band[0] + band[1]) / 2
ink = [x for x in range(40, 320) for y in range(band[0], band[1] + 1) if bright(x, y)]
ink_mid = (min(ink) + max(ink)) / 2

column = min(range(columns), key=lambda c: abs((c + 0.5) * cell_w - ink_mid))
row = min(range(rows), key=lambda r: abs((r + 0.5) * cell_h - mid))
x = (column + 0.5) * cell_w
y = (row + 0.5) * cell_h
if abs(y - mid) > 10:
    sys.exit(f"no hint cell centre lands in the delete row ({band}, best y={y})")


def label(index):
    base = len(charset)
    count = columns * rows
    length = 1
    while base**length < count:
        length += 1
    if length == 1:
        return charset[index]
    room = base**length - count
    denial = base**(length - 1) - 1
    short = min(room // denial, base)
    tail = base**(length - 1)
    if index < short:
        return charset[index]
    i = index - short
    first, rest = short + i // tail, i % tail
    digits = [charset[0]] * (length - 1)
    for slot in range(length - 2, -1, -1):
        digits[slot] = charset[rest % base]
        rest //= base
    return charset[first] + "".join(digits)


print(label(row * columns + column), round(x), round(y))
PY
) || fail "could not find the panel's delete row"

echo "  delete row: label=$label at ($x,$y)"

# Park the pointer on the row: a bare warp from another client does not reach
# sgraffito, but a press does, and sgraffito re-hears the last press position when
# windhover's surface maps. Close the panel, press the row's point, open it again.
"$vinject" 800x600 seq > /dev/null <<VINJECT
key c
sleep 300
click $x $y
sleep 300
key c
sleep 500
VINJECT

"$wh_bin" --hint -c "$work/wh.conf" --trace > "$work/wh.log" 2>&1 &
wh_pid=$!
pids+=("$wh_pid")
sleep 1
kill -0 "$wh_pid" 2>/dev/null || fail "windhover exited early"

for ((i = 0; i < ${#label}; i++)); do tap "${label:i:1}" 0.4; done

# The click is the last thing a session does: wait for it, then for the debounced write.
for _ in $(seq 40); do kill -0 "$wh_pid" 2>/dev/null || break; sleep 0.1; done
kill -0 "$wh_pid" 2>/dev/null && fail "windhover did not exit on the click"
sleep 2

last=$(grep -o 'cursor=([0-9.,]*)' "$work/wh.log" | tail -1 | tr -cd '0-9,')
click=$(grep -o 'Click { button: [0-9]* }' "$work/wh.log" | tail -1)
python3 -c "assert abs(${last%,*} - $x) <= 2 and abs(${last#*,} - $y) <= 2, 'the click missed the row: ${last} vs $x,$y'" \
    || fail "the click did not land on the row"
[ -n "$click" ] || fail "windhover never clicked"

"$bin" boards > "$work/boards.txt" || fail "boards command failed"
grep -q 'two' "$work/boards.txt" && fail "the deleted board is still listed: $(cat "$work/boards.txt")"
grep -q 'one' "$work/boards.txt" || fail "the surviving board is gone: $(cat "$work/boards.txt")"
grep -q '"two"' "$XDG_DATA_HOME/sgraffito/annotations.json" && fail "the deleted board is still in the file"
grep -q 'board deleted: two' "$work/daemon.log" || fail "the daemon never logged the deletion"

python3 - "$XDG_DATA_HOME/sgraffito/annotations.json" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
assert list(d['boards']) == ['one'], d['boards']
assert d['active'] == 'one', d['active']
PY
echo "PASS: windhover warped onto the delete row, clicked it, and the board left the document and the file"
