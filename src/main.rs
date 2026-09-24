//! `sgraffito` command line and daemon entry point.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use calloop::EventLoop;
use calloop::ping;
use calloop::signals::{Signal, Signals};
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

const MAX_COMMAND_BYTES: usize = 4096;
const CLIENT_TIMEOUT: Duration = Duration::from_secs(2);
const REPLY_TIMEOUT: Duration = Duration::from_secs(2);

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

struct ControlRequest {
    command: String,
    reply: mpsc::Sender<String>,
}

fn read_command(stream: &mut UnixStream) -> io::Result<String> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let count = reader.read(&mut chunk)?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "command ended before newline",
            ));
        }
        if let Some(newline) = chunk[..count].iter().position(|byte| *byte == b'\n') {
            let end = newline + 1;
            if line.len() + end > MAX_COMMAND_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "command line is too long",
                ));
            }
            line.extend_from_slice(&chunk[..end]);
            break;
        }
        if line.len() + count > MAX_COMMAND_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "command line is too long",
            ));
        }
        line.extend_from_slice(&chunk[..count]);
    }
    let line = String::from_utf8(line)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok(line.trim_end().to_string())
}

fn serve_client(mut stream: UnixStream, requests: mpsc::Sender<ControlRequest>, ping: ping::Ping) {
    let _ = stream.set_read_timeout(Some(CLIENT_TIMEOUT));
    let response = match read_command(&mut stream) {
        Ok(command) => {
            let (reply, response) = mpsc::channel();
            match requests.send(ControlRequest { command, reply }) {
                Ok(()) => {
                    ping.ping();
                    response
                        .recv_timeout(REPLY_TIMEOUT)
                        .unwrap_or_else(|_| "error: daemon did not respond".into())
                }
                Err(_) => "error: daemon is shutting down".into(),
            }
        }
        Err(error) => format!("error: failed to read the command: {error}"),
    };
    let _ = writeln!(stream, "{response}");
    let _ = stream.flush();
}

fn spawn_control_server(
    listener: UnixListener,
    requests: mpsc::Sender<ControlRequest>,
    ping: ping::Ping,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        if let Err(error) = listener.set_nonblocking(true) {
            log(&format!(
                "failed to make control socket non-blocking: {error}"
            ));
            return;
        }
        while !stop.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let requests = requests.clone();
                    let ping = ping.clone();
                    thread::spawn(move || serve_client(stream, requests, ping));
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => {
                    log(&format!("accept failed: {error}"));
                    thread::sleep(Duration::from_millis(10));
                }
            }
        }
    })
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

    let (requests, request_rx) = mpsc::channel::<ControlRequest>();
    let (ping, ping_source) = ping::make_ping()?;
    handle.insert_source(ping_source, move |_, _, app: &mut App| {
        while let Ok(request) = request_rx.try_recv() {
            let response = app.command(&request.command);
            let _ = request.reply.send(response);
        }
    })?;

    handle.insert_source(
        Signals::new(&[Signal::SIGINT, Signal::SIGTERM])?,
        |_, signal, app: &mut App| {
            log(&format!("got {signal:?}, shutting down"));
            app.request_exit();
        },
    )?;

    let stop = Arc::new(AtomicBool::new(false));
    let control_thread = spawn_control_server(listener, requests, ping, stop.clone());

    while !app.exiting() {
        if let Err(e) = event_loop.dispatch(Some(Duration::from_millis(200)), &mut app) {
            log(&format!("event loop error: {e}"));
            break;
        }
        app.tick();
    }

    stop.store(true, Ordering::Relaxed);
    let _ = control_thread.join();
    app.shutdown();
    drop(app);
    let _ = std::fs::remove_file(&path);
    log("daemon exited");
    Ok(())
}
