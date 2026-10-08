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
{"version":1,"outputs":{"HEADLESS-1":{"strokes":[{"color":"#e01b24","width":8,"points":[[100,300],[700,300]]}],"texts":[{"x":100,"y":100,"color":"#ffffff","size":40,"text":"hi 你好"}]},"HEADLESS-2":{"strokes":[{"color":"#e01b24","width":8,"points":[[50,75],[150,75]]}],"texts":[]}}}
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
assert red > 1000, f"no red line drawn while locked (red={red})"
assert ink > red, f"text was not rendered (ink={ink}, red={red})"
print(f"  rendering ok: red={red} ink={ink}")
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
# bands of the left strip, and the top strip (where a horizontal bar used to sit) stays clear.
# The seeded annotations start at x=100, so the strip x<60 can only hold the rail.
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
top = ink(0, 0, w, 20)
assert high > 100 and low > 100, f"no vertical rail on the left edge (high={high} low={low})"
assert top == 0, f"the top strip is not clear (ink={top})"
print(f"  rail ok: high={high} low={low} top={top}")
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

# 5. a second daemon is rejected
if "$bin" daemon > /dev/null 2>&1; then fail "a second daemon started"; fi
echo "  single instance ok"

# 6. graceful exit on SIGTERM
kill -TERM "$daemon_pid"
for _ in $(seq 50); do kill -0 "$daemon_pid" 2>/dev/null || break; sleep 0.1; done
wait "$daemon_pid" || fail "daemon exit code was non-zero"
daemon_pid=""
[ -S "$XDG_RUNTIME_DIR/sgraffito.sock" ] && fail "socket file left behind"
echo "  graceful exit ok"

echo "PASS: sgraffito smoke test passed"
