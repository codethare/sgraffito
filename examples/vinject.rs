//! Pointer input for the smoke test.
//!
//! `scripts/smoke.sh` runs headless sway with `WLR_LIBINPUT_NO_DEVICES=1`, so the seat has no capabilities
//! and nothing can reach the daemon's pointer paths: the hover, the rail's blocks, a stroke, a note's drag,
//! a marquee, a handle. This client injects the pointer as a device the compositor believes in, through
//! `zwlr_virtual_pointer_manager_v1`, which sway advertises.
//!
//! **One invocation is one gesture, and a sequence of gestures is one `seq`.** A virtual pointer is a device:
//! creating one makes the seat gain the pointer capability and destroying it takes the capability away, and a
//! client that connected, clicked and exited was measured dropping the events of the invocation after it. A
//! `seq` keeps one device for the whole sequence, which is what a test wants anyway — press, move, release
//! and the waits between them are one run of the program.
//!
//! ```text
//! vinject <W>x<H> move X Y
//! vinject <W>x<H> click X Y
//! vinject <W>x<H> drag X0 Y0 X1 Y1 [steps]     - left button
//! vinject <W>x<H> mdrag X0 Y0 X1 Y1 [steps]    - middle button, which pans
//! vinject <W>x<H> seq <<'EOF'                  - commands on stdin, one per line:
//!   move 100 100
//!   click 15 205
//!   drag 300 300 600 420 8
//!   sleep 400
//!   mpress 400 300
//!   move 500 300
//!   mrelease 500 300
//! EOF
//! ```
//!
//! `W`x`H` is the layout the coordinates are in, in the compositor's own pixels: `motion_absolute` maps its
//! position onto the layout's bounding box, so the smoke test's two outputs — 800x600 at scale 1 beside
//! 800x600 at scale 2 — are a 1200x600 layout.

use std::io::BufRead as _;
use std::process::ExitCode;
use std::time::Duration;

use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_pointer::ButtonState;
use wayland_client::protocol::wl_registry;
use wayland_client::protocol::wl_seat;
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
    zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
};

/// `BTN_LEFT` and `BTN_MIDDLE` of the Linux input numbering, which `wl_pointer` buttons use.
const BTN_LEFT: u32 = 0x110;
const BTN_MIDDLE: u32 = 0x112;

/// How many moves a `drag` interpolates when it is not told: enough that the daemon sees a drag rather than
/// a jump, which is what a note's size, a marquee and a handle all depend on.
const DRAG_STEPS: u32 = 8;

const USAGE: &str = "\
vinject — inject pointer input for the sgraffito smoke test

Usage:
  vinject <W>x<H> move X Y
  vinject <W>x<H> click X Y
  vinject <W>x<H> drag X0 Y0 X1 Y1 [steps]
  vinject <W>x<H> mdrag X0 Y0 X1 Y1 [steps]
  vinject <W>x<H> seq          (commands on stdin: move, click, press, release, mpress, mrelease,
                                drag, mdrag, sleep MS, one per line)
";

/// One thing the pointer does, in the order the compositor will deliver it.
enum Step {
    Move {
        x: u32,
        y: u32,
    },
    Button {
        x: u32,
        y: u32,
        button: u32,
        state: ButtonState,
    },
    /// A wait, so the daemon has time to act on what it was just sent before the next thing arrives.
    Sleep(Duration),
}

#[derive(Default)]
struct State {
    /// Whether the seat has said it has a pointer: the device this client creates is only usable once the
    /// compositor has added it to the seat and broadcast that.
    has_pointer: bool,
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities { capabilities } = event
            && let WEnum::Value(capabilities) = capabilities
        {
            state.has_pointer |= capabilities.contains(wl_seat::Capability::Pointer);
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrVirtualPointerManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwlrVirtualPointerManagerV1,
        _: <ZwlrVirtualPointerManagerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrVirtualPointerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwlrVirtualPointerV1,
        _: <ZwlrVirtualPointerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((layout, rest)) = args.split_first() else {
        eprint!("{USAGE}");
        return ExitCode::FAILURE;
    };
    let Some((extent_w, extent_h)) = layout.split_once('x') else {
        eprintln!("vinject: the first argument is the layout, as WxH");
        return ExitCode::FAILURE;
    };
    let (Ok(extent_w), Ok(extent_h)) = (extent_w.parse::<u32>(), extent_h.parse::<u32>()) else {
        eprintln!("vinject: {layout} is not WxH");
        return ExitCode::FAILURE;
    };
    let Some((command, rest)) = rest.split_first() else {
        eprint!("{USAGE}");
        return ExitCode::FAILURE;
    };
    let plan = match command.as_str() {
        "seq" => match sequence() {
            Ok(plan) => plan,
            Err(error) => {
                eprintln!("vinject: {error}");
                return ExitCode::FAILURE;
            }
        },
        _ => match single(command, rest) {
            Ok(plan) => plan,
            Err(error) => {
                eprintln!("vinject: {error}");
                return ExitCode::FAILURE;
            }
        },
    };
    if plan.is_empty() {
        eprint!("{USAGE}");
        return ExitCode::FAILURE;
    }
    if let Err(error) = inject(&plan, extent_w, extent_h) {
        eprintln!("vinject: {error:#}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn numbers(rest: &[String], count: usize) -> Option<Vec<f32>> {
    let values: Vec<f32> = rest
        .iter()
        .take(count)
        .filter_map(|value| value.parse().ok())
        .collect();
    (values.len() == count).then_some(values)
}

fn optional_steps(rest: &[String], index: usize) -> u32 {
    rest.get(index)
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(DRAG_STEPS)
        .max(1)
}

/// The one-gesture forms, which are what a single assertion in a shell script wants.
fn single(command: &str, rest: &[String]) -> Result<Vec<Step>, String> {
    let two = numbers(rest, 2);
    let four = numbers(rest, 4);
    let plan = match (command, two, four) {
        ("move", Some(p), _) => vec![step_move(p[0], p[1])],
        ("click", Some(p), _) => {
            let mut plan = vec![step_move(p[0], p[1])];
            plan.extend(click(p[0], p[1], BTN_LEFT));
            plan
        }
        ("drag", _, Some(p)) => drag(p[0], p[1], p[2], p[3], optional_steps(rest, 4), BTN_LEFT),
        ("mdrag", _, Some(p)) => drag(p[0], p[1], p[2], p[3], optional_steps(rest, 4), BTN_MIDDLE),
        _ => return Err(format!("{command} is not a command")),
    };
    Ok(plan)
}

/// A whole sequence in one connection, which is what keeps the virtual pointer — and so the seat's pointer
/// capability — alive from the first gesture to the last.
fn sequence() -> Result<Vec<Step>, String> {
    let mut plan = Vec::new();
    for (number, line) in std::io::stdin().lock().lines().enumerate() {
        let line = line.map_err(|error| format!("line {}: {error}", number + 1))?;
        let words: Vec<&str> = line.split_whitespace().collect();
        let Some((command, rest)) = words.split_first() else {
            continue;
        };
        let rest: Vec<String> = rest.iter().map(|word| word.to_string()).collect();
        let args = numbers(&rest, 2);
        let left = numbers(&rest, 4);
        let bad = || format!("line {}: {line}", number + 1);
        match (*command, args, left) {
            ("move", Some(p), _) => plan.push(step_move(p[0], p[1])),
            ("click", Some(p), _) => {
                plan.push(step_move(p[0], p[1]));
                plan.extend(click(p[0], p[1], BTN_LEFT));
            }
            ("mclick", Some(p), _) => {
                plan.push(step_move(p[0], p[1]));
                plan.extend(click(p[0], p[1], BTN_MIDDLE));
            }
            ("press", Some(p), _) => {
                plan.push(step_button(p[0], p[1], BTN_LEFT, ButtonState::Pressed))
            }
            ("mpress", Some(p), _) => {
                plan.push(step_button(p[0], p[1], BTN_MIDDLE, ButtonState::Pressed))
            }
            ("release", Some(p), _) => {
                plan.push(step_button(p[0], p[1], BTN_LEFT, ButtonState::Released))
            }
            ("mrelease", Some(p), _) => {
                plan.push(step_button(p[0], p[1], BTN_MIDDLE, ButtonState::Released))
            }
            ("drag", _, Some(p)) => plan.extend(drag(
                p[0],
                p[1],
                p[2],
                p[3],
                optional_steps(&rest, 4),
                BTN_LEFT,
            )),
            ("mdrag", _, Some(p)) => plan.extend(drag(
                p[0],
                p[1],
                p[2],
                p[3],
                optional_steps(&rest, 4),
                BTN_MIDDLE,
            )),
            ("sleep", _, _) => {
                let ms = rest
                    .first()
                    .and_then(|value| value.parse::<f64>().ok())
                    .ok_or_else(bad)?;
                plan.push(Step::Sleep(Duration::from_millis(ms.max(0.0) as u64)));
            }
            _ => return Err(bad()),
        }
    }
    Ok(plan)
}

fn step_move(x: f32, y: f32) -> Step {
    Step::Move {
        x: x as u32,
        y: y as u32,
    }
}

fn step_button(x: f32, y: f32, button: u32, state: ButtonState) -> Step {
    Step::Button {
        x: x as u32,
        y: y as u32,
        button,
        state,
    }
}

fn click(x: f32, y: f32, button: u32) -> Vec<Step> {
    vec![
        step_button(x, y, button, ButtonState::Pressed),
        step_button(x, y, button, ButtonState::Released),
    ]
}

/// A press, `steps` moves, and the release: the compositor and the daemon see every one of them, so a drag
/// that only makes sense as an end state — a note that is sized by its drag, a marquee, a handle — is
/// exercised the way a hand would.
fn drag(x0: f32, y0: f32, x1: f32, y1: f32, steps: u32, button: u32) -> Vec<Step> {
    let mut plan = vec![step_button(x0, y0, button, ButtonState::Pressed)];
    for step in 1..=steps {
        let t = step as f32 / steps as f32;
        plan.push(step_move(x0 + (x1 - x0) * t, y0 + (y1 - y0) * t));
    }
    plan.push(step_button(x1, y1, button, ButtonState::Released));
    plan
}

fn inject(plan: &[Step], extent_w: u32, extent_h: u32) -> anyhow::Result<()> {
    let conn = Connection::connect_to_env()?;
    let (globals, mut queue) = registry_queue_init::<State>(&conn)?;
    let qh = queue.handle();
    let manager: ZwlrVirtualPointerManagerV1 = globals.bind(&qh, 1..=2, ())?;
    let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=9, ())?;
    let pointer = manager.create_virtual_pointer(Some(&seat), &qh, ());
    let mut state = State::default();
    // Wait for the seat to say it has a pointer. The device is only usable once the compositor has added it,
    // and an event sent before that is dropped — which is what made the first click of one run land and the
    // same click of the next one vanish.
    for _ in 0..100 {
        queue.roundtrip(&mut state)?;
        if state.has_pointer {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // And let the other clients of this session — the daemon is one — bind their own pointer to the seat it
    // has just gained, so the first event of the sequence is delivered rather than dropped on the way in.
    std::thread::sleep(Duration::from_millis(150));
    // A timestamp orders and coalesces events, and the compositor compares it with its own clock: one that
    // restarted at 1 in every invocation would be a timestamp going backwards from the last. Wall-clock
    // milliseconds only ever rise.
    let mut time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u32)
        .unwrap_or(1);
    for step in plan {
        match step {
            Step::Sleep(duration) => std::thread::sleep(*duration),
            Step::Move { x, y } => {
                pointer.motion_absolute(
                    time,
                    (*x).min(extent_w - 1),
                    (*y).min(extent_h - 1),
                    extent_w,
                    extent_h,
                );
                pointer.frame();
                queue.roundtrip(&mut state)?;
                time = time.wrapping_add(1);
            }
            Step::Button {
                x,
                y,
                button,
                state: press,
            } => {
                pointer.motion_absolute(
                    time,
                    (*x).min(extent_w - 1),
                    (*y).min(extent_h - 1),
                    extent_w,
                    extent_h,
                );
                pointer.button(time, *button, *press);
                pointer.frame();
                queue.roundtrip(&mut state)?;
                time = time.wrapping_add(1);
            }
        }
    }
    pointer.destroy();
    queue.roundtrip(&mut state)?;
    Ok(())
}
