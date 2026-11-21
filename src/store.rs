//! Annotation persistence: `$XDG_DATA_HOME/sgraffito/annotations.json`.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::canvas::{Board, Doc};

/// The version this daemon writes. Version 1 had no `shapes` list, version 2 adds that, version 3
/// adds the `notes` list, version 4 keys the annotations by board name instead of output name and
/// records the board picked last, version 5 adds a text's board link, and version 6 gives shapes
/// stable ids and connector attachments. All of them are read, because an older file's missing
/// fields default, and an older build reading a newer file would silently drop what it does not
/// know on its next save — which is what the number is for.
pub const VERSION: u32 = 6;
/// The oldest version whose files still load.
pub const MIN_VERSION: u32 = 1;
/// The newest schema this build can still read. Version 7 was the text-attachment build: its extra
/// fields are ignored here, so a rollback must not turn a board file into a `.bak` and an empty
/// document. Writes stay at [`VERSION`]; the unknown fields are dropped on the next save.
pub const READABLE_VERSION: u32 = 7;
pub const DEBOUNCE: Duration = Duration::from_secs(1);
pub const RETRY_DELAY: Duration = Duration::from_secs(1);

/// What is read. Both keyings are here: versions 1 to 3 hold the annotations under `outputs`, keyed by
/// output name, and version 4 holds them under `boards`, keyed by board name — one key renamed, so the
/// only change in the format is what its keys mean.
#[derive(Deserialize)]
struct File {
    version: u32,
    #[serde(default)]
    boards: BTreeMap<String, Board>,
    #[serde(default)]
    outputs: BTreeMap<String, Board>,
    #[serde(default)]
    active: String,
}

#[derive(Serialize)]
struct FileRef<'a> {
    version: u32,
    boards: &'a BTreeMap<String, Board>,
    active: &'a str,
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
/// A version older than [`MIN_VERSION`] or newer than [`READABLE_VERSION`] is unsupported and is backed up.
/// A newer-but-readable file is loaded and written back at [`VERSION`] on the next save.
pub fn load(path: &Path) -> Doc {
    let raw = match fs::read(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return recover_backup(path),
        Err(e) => {
            crate::log::write(&format!("failed to read {}: {e}", path.display()));
            return Doc::default();
        }
    };
    match decode(&raw) {
        // A supported older or newer-readable version loads; missing fields default and unknown
        // fields, such as a version 7 text attachment, are ignored. It is written back in the
        // current version the next time the daemon saves.
        Ok(doc) => doc,
        Err(reason) => {
            backup(path, &reason);
            Doc::default()
        }
    }
}

/// Decode and normalise one annotation file, without touching the filesystem.
fn decode(raw: &[u8]) -> Result<Doc, String> {
    let file = serde_json::from_slice::<File>(raw).map_err(|e| e.to_string())?;
    if !(MIN_VERSION..=READABLE_VERSION).contains(&file.version) {
        return Err(format!("unsupported file version {}", file.version));
    }
    if file.version > VERSION {
        crate::log::write(&format!(
            "annotation file version {} is newer than this build's {}; loading it and saving as {}",
            file.version, VERSION, VERSION
        ));
    }
    let mut doc = Doc {
        // A file from before boards existed keys its annotations by output name; those keys become
        // board names of their own, which is what lets an output that appears afterwards find the
        // board named after it and show what it used to show.
        boards: if file.boards.is_empty() {
            file.outputs
        } else {
            file.boards
        },
        active: file.active,
    };
    for board in doc.boards.values_mut() {
        board.ensure_shape_ids();
        board.drop_dangling_attachments();
    }
    Ok(doc)
}

/// The previous failed load of this file, if any. A newer-but-readable version is the common case:
/// the old build renamed the good file to `.bak` and started empty, so a missing main file with a
/// readable backup is the same document, not a fresh start. Recovery copies the backup back into
/// place instead of renaming it, so the evidence survives a second failure.
fn recover_backup(path: &Path) -> Doc {
    let bak = path.with_extension("json.bak");
    let Ok(raw) = fs::read(&bak) else {
        return Doc::default();
    };
    let Ok(doc) = decode(&raw) else {
        return Doc::default();
    };
    match fs::copy(&bak, path) {
        Ok(_) => crate::log::write(&format!(
            "recovered {} from {}",
            path.display(),
            bak.display()
        )),
        Err(e) => crate::log::write(&format!(
            "read {} but failed to restore it to {}: {e}",
            bak.display(),
            path.display()
        )),
    }
    doc
}

fn backup(path: &Path, reason: &str) {
    let bak = path.with_extension("json.bak");
    match fs::rename(path, &bak) {
        Ok(()) => crate::log::write(&format!(
            "{} is unusable ({reason}), backed up to {}",
            path.display(),
            bak.display()
        )),
        Err(e) => crate::log::write(&format!("failed to back up {}: {e}", path.display())),
    }
}

/// Atomic write: temp file in the same directory, then rename.
pub fn save(path: &Path, doc: &Doc) -> io::Result<()> {
    let file = FileRef {
        version: VERSION,
        boards: &doc.boards,
        active: &doc.active,
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
