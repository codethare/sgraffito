//! Annotation persistence: `$XDG_DATA_HOME/sgraffito/annotations.json`.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::canvas::{Doc, OutputAnnotations};

/// The version this daemon writes. Version 1 had no `shapes` list, version 2 adds that, and version 3
/// adds the `notes` list. All of them are
/// read, because a version 1 file is a version 2 file with no shapes in it, and an older build
/// reading a version 2 file would silently drop every shape on its next save — which is what the
/// number is for.
pub const VERSION: u32 = 3;
/// The oldest version whose files still load.
pub const MIN_VERSION: u32 = 1;
pub const DEBOUNCE: Duration = Duration::from_secs(1);
pub const RETRY_DELAY: Duration = Duration::from_secs(1);

#[derive(Serialize, Deserialize)]
struct File {
    version: u32,
    outputs: BTreeMap<String, OutputAnnotations>,
}

#[derive(Serialize)]
struct FileRef<'a> {
    version: u32,
    outputs: &'a BTreeMap<String, OutputAnnotations>,
}

/// Resolve the annotation path: `$XDG_DATA_HOME/sgraffito/annotations.json`, falling back to `$HOME/.local/share`.
pub fn path_in(xdg_data_home: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    let base = match xdg_data_home {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => PathBuf::from(home?).join(".local/share"),
    };
    Some(base.join("sgraffito/annotations.json"))
}

pub fn path() -> Option<PathBuf> {
    path_in(
        std::env::var("XDG_DATA_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
}

/// Load annotations. Missing means empty; corrupt or unsupported versions are backed up as `.bak` and treated as empty.
/// A version older than [`MIN_VERSION`] or newer than [`VERSION`] is unsupported and is backed up.
pub fn load(path: &Path) -> Doc {
    let raw = match fs::read(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Doc::default(),
        Err(e) => {
            eprintln!("[sgraffito] failed to read {}: {e}", path.display());
            return Doc::default();
        }
    };
    let parsed = serde_json::from_slice::<File>(&raw).map_err(|e| e.to_string());
    match parsed {
        // A supported older version loads; its missing fields already default. It is written back
        // in the current version the next time the daemon saves.
        Ok(file) if (MIN_VERSION..=VERSION).contains(&file.version) => Doc {
            outputs: file.outputs,
        },
        Ok(file) => {
            backup(path, &format!("unsupported file version {}", file.version));
            Doc::default()
        }
        Err(e) => {
            backup(path, &e);
            Doc::default()
        }
    }
}

fn backup(path: &Path, reason: &str) {
    let bak = path.with_extension("json.bak");
    match fs::rename(path, &bak) {
        Ok(()) => eprintln!(
            "[sgraffito] {} is unusable ({reason}), backed up to {}",
            path.display(),
            bak.display()
        ),
        Err(e) => eprintln!("[sgraffito] failed to back up {}: {e}", path.display()),
    }
}

/// Atomic write: temp file in the same directory, then rename.
pub fn save(path: &Path, doc: &Doc) -> io::Result<()> {
    let file = FileRef {
        version: VERSION,
        outputs: &doc.outputs,
    };
    let json = serde_json::to_vec_pretty(&file)?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json)?;
    fs::rename(&tmp, path)
}

/// Debounced writer.
pub struct Store {
    path: PathBuf,
    deadline: Option<Instant>,
}

impl Store {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            deadline: None,
        }
    }

    pub fn mark_dirty(&mut self, now: Instant) {
        self.deadline = Some(now + DEBOUNCE);
    }

    pub fn due(&self, now: Instant) -> bool {
        self.deadline.is_some_and(|d| now >= d)
    }

    pub fn flush(&mut self, doc: &Doc) -> io::Result<()> {
        let result = save(&self.path, doc);
        self.deadline = if result.is_ok() {
            None
        } else {
            Some(Instant::now() + RETRY_DELAY)
        };
        result
    }
}
