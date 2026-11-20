//! The daemon's log: errors and state changes on stderr and in a size-capped file.
//!
//! The file lives at `$XDG_STATE_HOME/sgraffito/sgraffito.log` (default
//! `~/.local/state/sgraffito/`), which is the XDG base directory for state that persists between
//! runs. It rotates at 1 MiB and keeps three older files, the usual small-daemon bound: at most
//! four files and 4 MiB in total. A single message is capped at 8 KiB so a huge error string cannot
//! occupy a whole generation, and newlines are escaped so one event stays one line.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, Once, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Bytes one log file may grow to before it rotates. 1 MiB is the common `logrotate`/daemon
/// default for a low-volume service.
pub const MAX_FILE_BYTES: u64 = 1 << 20;
/// Older files kept beside the current one. Four files total is at most 4 MiB.
pub const KEEP_FILES: usize = 3;
/// Longest message content written for one event, in bytes.
pub const MAX_MESSAGE_BYTES: usize = 8 << 10;

struct LogFile {
    path: PathBuf,
    file: File,
    size: u64,
}

static LOGGER: OnceLock<Mutex<Option<LogFile>>> = OnceLock::new();
static PANIC_HOOK: Once = Once::new();

/// `$XDG_STATE_HOME/sgraffito/sgraffito.log`, falling back to `$HOME/.local/state`.
pub fn path_in(xdg_state_home: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    let base = match xdg_state_home {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(home?).join(".local/state"),
    };
    Some(base.join("sgraffito/sgraffito.log"))
}

pub fn path() -> Option<PathBuf> {
    path_in(
        std::env::var("XDG_STATE_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
}

/// Open the log file once, rotating it first if an earlier run left it over the cap. A failure is
/// not fatal: the daemon still runs and still writes to stderr.
pub fn init() {
    let logger = LOGGER.get_or_init(|| Mutex::new(None));
    let mut guard = match logger.lock() {
        Ok(guard) => guard,
        Err(_) => return,
    };
    if guard.is_some() {
        return;
    }
    let Some(path) = path() else {
        eprintln!(
            "[sgraffito] log: neither $XDG_STATE_HOME nor $HOME is set; logging to stderr only"
        );
        return;
    };
    match open(&path) {
        Ok(log) => *guard = Some(log),
        Err(e) => eprintln!(
            "[sgraffito] log: cannot open {}: {e}; logging to stderr only",
            path.display()
        ),
    }
    drop(guard);
    PANIC_HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // The default hook still prints the panic to stderr; the file gets the same event with
            // its location, and no duplicate stderr line from this path. `try_lock` keeps a panic
            // that happened while another thread held the log from deadlocking the hook.
            record_with(&format!("panic: {info}"), false, true);
            previous(info);
        }));
    });
}

/// Write one event to stderr and, once [`init`] has run, to the log file. Safe to call before
/// `init`: the message is still printed, just not stored.
pub fn write(msg: &str) {
    record(msg, true);
}

fn record(msg: &str, stderr: bool) {
    record_with(msg, stderr, false);
}

fn record_with(msg: &str, stderr: bool, nonblocking: bool) {
    let line = format!(
        "{} [sgraffito] {}\n",
        timestamp(SystemTime::now()),
        clean(msg)
    );
    if stderr {
        eprint!("{line}");
    }
    let Some(logger) = LOGGER.get() else {
        return;
    };
    let guard = if nonblocking {
        logger.try_lock().ok()
    } else {
        logger.lock().ok()
    };
    let Some(mut guard) = guard else {
        return;
    };
    let Some(log) = guard.as_mut() else {
        return;
    };
    log.append(line.as_bytes());
}

impl LogFile {
    fn append(&mut self, line: &[u8]) {
        let len = line.len() as u64;
        if self.size > 0 && self.size.saturating_add(len) > MAX_FILE_BYTES {
            match self.rotate() {
                Ok(()) => {}
                Err(e) => {
                    // Keep writing rather than lose the event: a failed rotation may leave the file
                    // over its cap, but the next write tries again.
                    eprintln!(
                        "[sgraffito] log: cannot rotate {}: {e}",
                        self.path.display()
                    );
                }
            }
        }
        if let Err(e) = self.file.write_all(line) {
            eprintln!("[sgraffito] log: cannot write {}: {e}", self.path.display());
            return;
        }
        self.size = self.size.saturating_add(len);
    }

    fn rotate(&mut self) -> io::Result<()> {
        let path = self.path.clone();
        rotate(&path, KEEP_FILES)?;
        let next = open(&path)?;
        self.file = next.file;
        self.size = next.size;
        Ok(())
    }
}

/// Move `path` to `path.1`, shifting the older generations, and drop the one past `keep`.
pub fn rotate(path: &Path, keep: usize) -> io::Result<()> {
    if keep == 0 {
        let _ = fs::remove_file(path);
        return Ok(());
    }
    let _ = fs::remove_file(archive(path, keep));
    for number in (1..keep).rev() {
        let from = archive(path, number);
        let to = archive(path, number + 1);
        match fs::rename(&from, &to) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    match fs::rename(path, archive(path, 1)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

fn archive(path: &Path, number: usize) -> PathBuf {
    path.with_extension(format!("log.{number}"))
}

fn open(path: &Path) -> io::Result<LogFile> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if size >= MAX_FILE_BYTES {
        rotate(path, KEEP_FILES)?;
        size = 0;
    }
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    Ok(LogFile {
        path: path.to_path_buf(),
        file,
        size,
    })
}

/// One line per event, with the message capped and its newlines escaped.
fn clean(msg: &str) -> String {
    let mut out = String::with_capacity(msg.len().min(MAX_MESSAGE_BYTES));
    for ch in msg.chars() {
        if out.len() + ch.len_utf8() > MAX_MESSAGE_BYTES {
            out.push_str("…[truncated]");
            break;
        }
        match ch {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(ch),
        }
    }
    out
}

fn timestamp(at: SystemTime) -> String {
    let secs = at
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let rest = secs.rem_euclid(86_400);
    let (hour, minute, second) = (rest / 3600, (rest / 60) % 60, rest % 60);
    format!(
        "{} {hour:02}:{minute:02}:{second:02}",
        crate::canvas::date_stamp(at)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sgraffito-log-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn path_is_under_the_xdg_state_home() {
        assert_eq!(
            path_in(Some("/state"), Some("/home/u")).as_deref(),
            Some(Path::new("/state/sgraffito/sgraffito.log"))
        );
        assert_eq!(
            path_in(None, Some("/home/u")).as_deref(),
            Some(Path::new("/home/u/.local/state/sgraffito/sgraffito.log"))
        );
        assert_eq!(path_in(None, None), None);
    }

    #[test]
    fn rotation_keeps_the_requested_number_of_archives() {
        let dir = dir("rotate");
        let path = dir.join("sgraffito.log");
        for number in 1..=3 {
            fs::write(archive(&path, number), format!("generation {number}")).unwrap();
        }
        fs::write(&path, "current").unwrap();
        rotate(&path, 3).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(fs::read_to_string(archive(&path, 1)).unwrap(), "current");
        assert_eq!(
            fs::read_to_string(archive(&path, 2)).unwrap(),
            "generation 1"
        );
        assert_eq!(
            fs::read_to_string(archive(&path, 3)).unwrap(),
            "generation 2"
        );
        assert!(
            !archive(&path, 4).exists(),
            "an archive past the keep count survived"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_message_is_one_capped_line() {
        let escaped = clean("one\ntwo\rthree");
        assert_eq!(escaped, "one\\ntwo\\rthree");
        let long = "x".repeat(MAX_MESSAGE_BYTES + 1);
        let capped = clean(&long);
        assert!(capped.len() < MAX_MESSAGE_BYTES + 32);
        assert!(capped.ends_with("…[truncated]"));
    }

    #[test]
    fn a_write_that_reaches_the_cap_rotates_first() {
        let dir = dir("append");
        let path = dir.join("sgraffito.log");
        fs::write(&path, vec![b'x'; MAX_FILE_BYTES as usize]).unwrap();
        let mut log = open(&path).unwrap();
        log.append(b"next\n");
        assert!(archive(&path, 1).exists());
        assert_eq!(fs::read(&path).unwrap(), b"next\n");
        let _ = fs::remove_dir_all(&dir);
    }
}
