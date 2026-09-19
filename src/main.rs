//! `sgraffito` command line and daemon entry point.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use calloop::generic::Generic;
use calloop::signals::{Signal, Signals};
use calloop::{EventLoop, Interest, Mode, PostAction};
use calloop_wayland_source::WaylandSource;
use sgraffito::app::{App, log};
use sgraffito::store;
use smithay_client_toolkit::compositor::CompositorState;
use smithay_client_toolkit::output::OutputState;
use smithay_client_toolkit::registry::RegistryState;
use smithay_client_toolkit::seat::SeatState;
use smithay_client_toolkit::shell::wlr_layer::LayerShell;
use smithay_client_toolkit::shm::Shm;
use wayland_client::Connection;
use wayland_client::globals::registry_queue_init;

const USAGE: &str = "\
sgraffito — doodle and sticky notes on the Wayland wallpaper layer

Usage:
  sgraffito daemon    run the daemon in the foreground (start it from your compositor)
  sgraffito toggle    switch between locked and edit mode
  sgraffito edit      enter edit mode
  sgraffito lock      go back to the locked mode
  sgraffito clear     erase every annotation

Compositor key binding example (sway):
  bindsym $mod+d exec sgraffito toggle
  bindsym $mod+Shift+d exec sgraffito clear
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("daemon") => run_daemon(),
        Some("toggle") => send_command("toggle"),
        Some("edit") => send_command("edit"),
        Some("lock") => send_command("lock"),
        Some("clear") => send_command("clear"),
        Some(other) => {
            eprintln!("unknown subcommand '{other}'");
            print!("{USAGE}");
            return ExitCode::from(2);
        }
        None => {
            print!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("sgraffito: {e}");
            ExitCode::FAILURE
        }
    }
}

fn socket_path() -> anyhow::Result<PathBuf> {
    let dir = std::env::var("XDG_RUNTIME_DIR").map_err(|_| {
        anyhow::anyhow!("missing $XDG_RUNTIME_DIR, cannot locate the control socket")
    })?;
    Ok(PathBuf::from(dir).join("sgraffito.sock"))
}

fn send_command(cmd: &str) -> anyhow::Result<()> {
    let path = socket_path()?;
    let mut stream = UnixStream::connect(&path).map_err(|e| {
        anyhow::anyhow!(
            "cannot connect to {} (is the daemon running?): {e}",
            path.display()
        )
    })?;
    writeln!(stream, "{cmd}")?;
    let mut response = String::new();
    BufReader::new(stream).read_line(&mut response)?;
    let response = response.trim_end();
    if let Some(err) = response.strip_prefix("error:") {
        anyhow::bail!("{cmd} failed:{err}");
    }
    if !response.starts_with("ok") {
        anyhow::bail!("{cmd} got an unexpected response '{response}'");
    }
    println!("{response}");
    Ok(())
}

fn run_daemon() -> anyhow::Result<()> {
    let path = socket_path()?;
    // Exit if a daemon is already running; otherwise reuse the path after removing a stale socket.
    if UnixStream::connect(&path).is_ok() {
        anyhow::bail!("a daemon is already running ({})", path.display());
    }
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;

    let conn = Connection::connect_to_env()?;
    let (globals, event_queue) = registry_queue_init(&conn)?;
    let qh = event_queue.handle();

    let registry_state = RegistryState::new(&globals);
    let compositor_state = CompositorState::bind(&globals, &qh)?;
    let layer_shell = LayerShell::bind(&globals, &qh)?;
    let shm = Shm::bind(&globals, &qh)?;
    let output_state = OutputState::new(&globals, &qh);
    let seat_state = SeatState::new(&globals, &qh);

    let mut app = App::new(
        conn.clone(),
        qh,
        registry_state,
        compositor_state,
        layer_shell,
        output_state,
        seat_state,
        shm,
    )?;
    log(&format!(
        "daemon started, mode {:?}, annotations {}",
        app.mode(),
        store::path()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    ));

    let mut event_loop: EventLoop<App> = EventLoop::try_new()?;
    let handle = event_loop.handle();
    WaylandSource::new(conn.clone(), event_queue).insert(handle.clone())?;

    handle.insert_source(
        Generic::new(listener, Interest::READ, Mode::Level),
        |_, listener, app: &mut App| {
            match listener.accept() {
                Ok((stream, _)) => {
                    // Our CLI writes the whole line right after connecting; the timeout keeps a stuck client from stalling the loop.
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                    let mut line = String::new();
                    let mut reader = BufReader::new(&stream);
                    let cmd = match reader.read_line(&mut line) {
                        Ok(_) => line.trim_end().to_string(),
                        Err(e) => format!("error: failed to read the command: {e}"),
                    };
                    let response = if cmd.starts_with("error:") {
                        cmd
                    } else {
                        app.command(&cmd)
                    };
                    let mut stream = stream;
                    let _ = writeln!(stream, "{response}");
                    let _ = stream.flush();
                }
                Err(e) => log(&format!("accept failed: {e}")),
            }
            Ok(PostAction::Continue)
        },
    )?;

    handle.insert_source(
        Signals::new(&[Signal::SIGINT, Signal::SIGTERM])?,
        |_, signal, app: &mut App| {
            log(&format!("got {signal:?}, shutting down"));
            app.request_exit();
        },
    )?;

    while !app.exiting() {
        if let Err(e) = event_loop.dispatch(Some(Duration::from_millis(200)), &mut app) {
            log(&format!("event loop error: {e}"));
            break;
        }
        app.tick();
    }

    app.shutdown();
    drop(app);
    let _ = std::fs::remove_file(&path);
    log("daemon exited");
    Ok(())
}
