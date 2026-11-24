//! Annotation persistence: one board record per file plus a small manifest.
//!
//! `$XDG_DATA_HOME/sgraffito/annotations.json` names the boards and the board picked last;
//! `$XDG_DATA_HOME/sgraffito/boards/<board>.json` holds each board's drawing. A board can therefore
//! be saved, renamed or deleted without rewriting every other board, and the number of boards is not
//! bounded by one document's save.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::canvas::{Board, Doc};

/// The version this daemon writes. Version 1 had no `shapes` list, version 2 adds that, version 3
/// adds the `notes` list, version 4 keys the annotations by board name instead of output name and
/// records the board picked last, version 5 adds a text's board link, version 6 gives shapes
/// stable ids and connector attachments, version 7 was the text-attachment build, and version 8
/// moves each board's record into its own file behind a small manifest. All of them are read,
/// because an older file's missing fields default, and an older build reading a newer file would
/// silently drop what it does not know on its next save — which is what the number is for.
pub const VERSION: u32 = 8;
/// The oldest version whose files still load.
pub const MIN_VERSION: u32 = 1;
/// The newest schema this build can still read. Version 7 was the text-attachment build: its extra
/// fields are ignored here, so a rollback must not turn a board file into a `.bak` and an empty
/// document. Writes stay at [`VERSION`].
pub const READABLE_VERSION: u32 = 8;
pub const DEBOUNCE: Duration = Duration::from_secs(1);
pub const RETRY_DELAY: Duration = Duration::from_secs(1);

const BOARD_DIR: &str = "boards";

/// What a legacy single-file document is read as. Versions 1 to 3 hold the annotations under
/// `outputs`, keyed by output name, and versions 4 to 7 hold them under `boards`, keyed by board
/// name — one key renamed, so the only change in the format is what its keys mean.
#[derive(Deserialize)]
struct LegacyFile {
    version: u32,
    #[serde(default)]
    boards: BTreeMap<String, Board>,
    #[serde(default)]
    outputs: BTreeMap<String, Board>,
    #[serde(default)]
    active: String,
}

/// The small index written at `annotations.json` in version 8.
#[derive(Deserialize)]
struct Manifest {
    #[serde(default)]
    boards: Vec<String>,
    #[serde(default)]
    active: String,
}

#[derive(Serialize)]
struct ManifestRef<'a> {
    version: u32,
    boards: Vec<&'a str>,
    active: &'a str,
}

/// One board's own record. The name is stored beside the board so a missing manifest can be rebuilt
/// by scanning `boards/`; the manifest name remains authoritative when a hand-edited file disagrees.
#[derive(Deserialize)]
struct BoardFile {
    version: u32,
    #[serde(default)]
    name: String,
    board: Board,
}

#[derive(Serialize)]
struct BoardFileRef<'a> {
    version: u32,
    name: &'a str,
    board: &'a Board,
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

fn boards_dir(path: &Path) -> PathBuf {
    path.parent()
        .unwrap_or_else(|| Path::new("."))
        .join(BOARD_DIR)
}

/// The file name for one board. Names are encoded so that `/`, `%`, newlines and CJK board names are
/// safe file names; percent is itself encoded, so the mapping is reversible and collision-free.
fn board_file_name(name: &str) -> String {
    let mut file = String::with_capacity(name.len() + 5);
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b' ' | b'-' | b'_') {
            file.push(byte as char);
        } else {
            file.push_str(&format!("%{byte:02X}"));
        }
    }
    file.push_str(".json");
    file
}

/// Load annotations. Missing means empty; corrupt or unsupported versions are backed up as `.bak`
/// and treated as empty. A version older than [`MIN_VERSION`] or newer than [`READABLE_VERSION`] is
/// unsupported and is backed up. A legacy single-file document is split into one board record per
/// file on load, so the board count stops being bounded by one JSON document.
pub fn load(path: &Path) -> Doc {
    let raw = match fs::read(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return recover_backup(path),
        Err(e) => {
            crate::log::write(&format!("failed to read {}: {e}", path.display()));
            return Doc::default();
        }
    };
    match decode_any(path, &raw) {
        Ok((doc, legacy)) => {
            if legacy {
                migrate(path, &doc);
            }
            doc
        }
        Err(reason) => {
            backup(path, &reason);
            // The board records are still the drawings even when the small index is gone. Recover
            // them by scanning their directory; the next save writes a fresh manifest.
            load_board_dir(&boards_dir(path))
        }
    }
}

/// Keep an old single-file document but leave the new per-board layout behind. A failed migration
/// must not make the loaded document disappear from this run; it is retried on the next save.
fn migrate(path: &Path, doc: &Doc) {
    match save_changed(path, doc, &BTreeMap::new()) {
        Ok(()) => crate::log::write(&format!(
            "migrated {} to one board file per board",
            path.display()
        )),
        Err(e) => crate::log::write(&format!(
            "failed to migrate {} to per-board files: {e}",
            path.display()
        )),
    }
}

/// Decode either a legacy single-file document or a version-8 manifest. The filesystem is needed for
/// the manifest because each board lives in its own file.
fn decode_any(path: &Path, raw: &[u8]) -> Result<(Doc, bool), String> {
    let value: serde_json::Value =
        serde_json::from_slice(raw).map_err(|e| format!("not json: {e}"))?;
    let version = value
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| "missing version".to_string())? as u32;
    if !(MIN_VERSION..=READABLE_VERSION).contains(&version) {
        return Err(format!("unsupported file version {version}"));
    }
    if version <= 7 {
        let file: LegacyFile =
            serde_json::from_value(value).map_err(|e| format!("legacy schema: {e}"))?;
        if file.version != version {
            return Err("version changed while reading".to_string());
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
        normalize(&mut doc);
        return Ok((doc, true));
    }
    let manifest: Manifest =
        serde_json::from_value(value).map_err(|e| format!("manifest schema: {e}"))?;
    let dir = boards_dir(path);
    let mut boards = BTreeMap::new();
    for name in manifest.boards {
        if boards.contains_key(&name) {
            continue;
        }
        let board = load_board(&dir, &name);
        boards.insert(name, board);
    }
    let mut doc = Doc {
        boards,
        active: manifest.active,
    };
    normalize(&mut doc);
    Ok((doc, false))
}

fn normalize(doc: &mut Doc) {
    for board in doc.boards.values_mut() {
        board.ensure_shape_ids();
        board.drop_dangling_attachments();
    }
}

/// Read one board record. A missing or damaged record becomes an empty board rather than removing
/// the board's name: the board still exists and the user can draw on it again. The damaged file is
/// left in a `.bak` beside it.
fn load_board(dir: &Path, name: &str) -> Board {
    let path = dir.join(board_file_name(name));
    let raw = match fs::read(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return recover_board_backup(&path).unwrap_or_default();
        }
        Err(e) => {
            crate::log::write(&format!("failed to read board {}: {e}", path.display()));
            return Board::default();
        }
    };
    match decode_board(&raw) {
        Ok(file) => file.board,
        Err(reason) => {
            backup(&path, &reason);
            Board::default()
        }
    }
}

fn decode_board(raw: &[u8]) -> Result<BoardFile, String> {
    let file: BoardFile = serde_json::from_slice(raw).map_err(|e| e.to_string())?;
    if !(MIN_VERSION..=READABLE_VERSION).contains(&file.version) {
        return Err(format!("unsupported board file version {}", file.version));
    }
    Ok(file)
}

/// Rebuild a document from board records alone. Used when the manifest is missing or damaged: each
/// record carries its board name, so no drawing depends on the index surviving.
fn load_board_dir(dir: &Path) -> Doc {
    let mut doc = Doc::default();
    let Ok(entries) = fs::read_dir(dir) else {
        return doc;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(raw) = fs::read(&path) else {
            continue;
        };
        match decode_board(&raw) {
            Ok(file) if !file.name.is_empty() => {
                doc.boards.insert(file.name, file.board);
            }
            Ok(_) => {}
            Err(reason) => crate::log::write(&format!(
                "ignoring board record {} during recovery: {reason}",
                path.display()
            )),
        }
    }
    normalize(&mut doc);
    doc
}

fn recover_board_backup(path: &Path) -> Option<Board> {
    let bak = path.with_extension("json.bak");
    let raw = fs::read(&bak).ok()?;
    let board = decode_board(&raw).ok()?.board;
    match fs::copy(&bak, path) {
        Ok(_) => crate::log::write(&format!(
            "recovered board {} from {}",
            path.display(),
            bak.display()
        )),
        Err(e) => crate::log::write(&format!(
            "read {} but failed to restore it to {}: {e}",
            bak.display(),
            path.display()
        )),
    }
    Some(board)
}

/// The previous failed load of the manifest, if any. A newer-but-readable version is the common case:
/// the old build renamed the good file to `.bak` and started empty, so a missing main file with a
/// readable backup is the same document, not a fresh start. Recovery copies the backup back into
/// place instead of renaming it, so the evidence survives a second failure.
fn recover_backup(path: &Path) -> Doc {
    let bak = path.with_extension("json.bak");
    let Ok(raw) = fs::read(&bak) else {
        // No index and no backup: the board records may still be there.
        return load_board_dir(&boards_dir(path));
    };
    let Ok((doc, legacy)) = decode_any(path, &raw) else {
        return load_board_dir(&boards_dir(path));
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
    if legacy {
        migrate(path, &doc);
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
fn write_atomic<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let json = serde_json::to_vec_pretty(value)?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json)?;
    fs::rename(&tmp, path)
}

/// Save every board record and the manifest. Public callers (tests and `clear`) get the full write;
/// [`Store`] uses the previous board map to write only records whose content changed.
pub fn save(path: &Path, doc: &Doc) -> io::Result<()> {
    save_changed(path, doc, &BTreeMap::new())
}

fn save_changed(path: &Path, doc: &Doc, previous: &BTreeMap<String, Board>) -> io::Result<()> {
    let dir = boards_dir(path);
    fs::create_dir_all(&dir)?;
    let mut current = BTreeSet::new();
    for (name, board) in &doc.boards {
        let file = board_file_name(name);
        current.insert(file.clone());
        let board_path = dir.join(file);
        if previous.get(name) != Some(board) || !board_path.exists() {
            let file = BoardFileRef {
                version: VERSION,
                name,
                board,
            };
            write_atomic(&board_path, &file)?;
        }
    }
    let manifest = ManifestRef {
        version: VERSION,
        boards: doc.boards.keys().map(String::as_str).collect(),
        active: &doc.active,
    };
    write_atomic(path, &manifest)?;
    // A board removed or renamed no longer has a record. Only our own `.json` files are touched;
    // backups and anything else in the directory are left alone.
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.ends_with(".json") && !current.contains(name) {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

/// Debounced writer.
pub struct Store {
    path: PathBuf,
    deadline: Option<Instant>,
    /// What was last written, so a document with many boards rewrites only the board that changed.
    /// `None` means nothing has been written by this process yet: the next flush writes them all.
    last: Option<BTreeMap<String, Board>>,
}

impl Store {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            deadline: None,
            last: None,
        }
    }

    pub fn mark_dirty(&mut self, now: Instant) {
        self.deadline = Some(now + DEBOUNCE);
    }

    pub fn due(&self, now: Instant) -> bool {
        self.deadline.is_some_and(|d| now >= d)
    }

    pub fn flush(&mut self, doc: &Doc) -> io::Result<()> {
        let previous = self.last.clone().unwrap_or_default();
        let result = save_changed(&self.path, doc, &previous);
        if result.is_ok() {
            self.last = Some(doc.boards.clone());
        }
        self.deadline = if result.is_ok() {
            None
        } else {
            Some(Instant::now() + RETRY_DELAY)
        };
        result
    }
}
