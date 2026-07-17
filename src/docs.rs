//! The note store — yedit's single source of truth for open notes.
//!
//! Host-resident, like every libyggterm app's state: the buffers live in this
//! process, the session (which tabs were open) persists under
//! `~/.yggterm/yedit/`, and yggterm renders but never stores any of it.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// A stable id for a path: FNV-1a over the canonical path, hex. Row ids and
/// API ids are this, never an index — indices shift when a tab closes.
pub fn note_id(path: &Path) -> String {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut hash = FNV_OFFSET;
    for byte in path.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{hash:016x}")
}

/// `mtime_ms:len` of the file on disk — the revision the save guard compares.
/// A missing file is revision "new" (a note created in yedit, not yet saved).
pub fn disk_revision(path: &Path) -> String {
    let Ok(metadata) = std::fs::metadata(path) else {
        return "new".to_string();
    };
    let mtime_ms = metadata
        .modified()
        .ok()
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default();
    format!("{mtime_ms}:{}", metadata.len())
}

pub struct Note {
    pub id: String,
    pub path: PathBuf,
    /// The editor buffer. May be ahead of disk (dirty).
    pub content: String,
    /// Disk revision at load or last save — the save guard's baseline.
    pub loaded_revision: String,
    pub dirty: bool,
}

impl Note {
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.to_string_lossy().into_owned())
    }
}

pub struct Store {
    pub notes: Vec<Note>,
    pub active_id: Option<String>,
    pub markdown_mode: bool,
    /// Bumped on every mutation; the page polls it to know when to refetch.
    pub epoch: u64,
    pub recent: Vec<PathBuf>,
    home: PathBuf,
}

/// What a save attempt came to.
pub enum SaveOutcome {
    Saved { revision: String },
    /// The file changed on disk since this note loaded it. NEVER silently
    /// clobber (the vault edit path's revision-guard rule): the caller shows
    /// the conflict and only a `force` save overwrites.
    Conflict { disk: String, loaded: String },
}

impl Store {
    pub fn state_dir(home: &Path) -> PathBuf {
        home.join(".yggterm").join("yedit")
    }

    pub fn new(home: PathBuf) -> Self {
        let mut store = Self {
            notes: Vec::new(),
            active_id: None,
            markdown_mode: true,
            epoch: 1,
            recent: Vec::new(),
            home,
        };
        store.load_session();
        store
    }

    fn session_path(&self) -> PathBuf {
        Self::state_dir(&self.home).join("session.json")
    }

    /// Restore markdown-mode, recents, and the previously open tabs — the
    /// "yedit reopens its tabs" half of the acceptance test. Files that no
    /// longer exist are dropped silently.
    fn load_session(&mut self) {
        let Ok(bytes) = std::fs::read(self.session_path()) else {
            return;
        };
        let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
            return;
        };
        self.markdown_mode = value["markdown_mode"].as_bool().unwrap_or(true);
        if let Some(recent) = value["recent"].as_array() {
            self.recent = recent
                .iter()
                .filter_map(|p| p.as_str())
                .map(PathBuf::from)
                .collect();
        }
        let open: Vec<PathBuf> = value["open"]
            .as_array()
            .map(|paths| {
                paths
                    .iter()
                    .filter_map(|p| p.as_str())
                    .map(PathBuf::from)
                    .collect()
            })
            .unwrap_or_default();
        for path in open {
            let _ = self.open(&path);
        }
        if let Some(active) = value["active"].as_str() {
            let id = note_id(Path::new(active));
            if self.notes.iter().any(|n| n.id == id) {
                self.active_id = Some(id);
            }
        }
    }

    pub fn persist_session(&self) {
        let dir = Self::state_dir(&self.home);
        let _ = std::fs::create_dir_all(&dir);
        let active_path = self
            .active()
            .map(|n| n.path.to_string_lossy().into_owned());
        let value = json!({
            "markdown_mode": self.markdown_mode,
            "open": self.notes.iter().map(|n| n.path.to_string_lossy()).collect::<Vec<_>>(),
            "active": active_path,
            "recent": self.recent.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>(),
        });
        let _ = std::fs::write(
            self.session_path(),
            serde_json::to_string_pretty(&value).unwrap_or_default(),
        );
    }

    pub fn touch(&mut self) {
        self.epoch += 1;
        self.persist_session();
    }

    pub fn active(&self) -> Option<&Note> {
        let id = self.active_id.as_deref()?;
        self.notes.iter().find(|n| n.id == id)
    }

    pub fn get(&self, id: &str) -> Option<&Note> {
        self.notes.iter().find(|n| n.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Note> {
        self.notes.iter_mut().find(|n| n.id == id)
    }

    /// Open (or focus) a note. A path that does not exist on disk opens as an
    /// empty NEW note — created on first save. Relative paths and `~` resolve
    /// against the invoking host's home, like any CLI.
    pub fn open(&mut self, raw: &Path) -> Result<String> {
        let path = self.resolve(raw);
        let id = note_id(&path);
        if self.notes.iter().any(|n| n.id == id) {
            self.active_id = Some(id.clone());
            self.touch();
            return Ok(id);
        }
        let (content, loaded_revision) = if path.exists() {
            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            (content, disk_revision(&path))
        } else {
            (String::new(), "new".to_string())
        };
        self.notes.push(Note {
            id: id.clone(),
            path: path.clone(),
            content,
            loaded_revision,
            dirty: false,
        });
        self.active_id = Some(id.clone());
        self.remember_recent(&path);
        self.touch();
        Ok(id)
    }

    fn resolve(&self, raw: &Path) -> PathBuf {
        let text = raw.to_string_lossy();
        let expanded = if let Some(rest) = text.strip_prefix("~/") {
            self.home.join(rest)
        } else if text == "~" {
            self.home.clone()
        } else {
            raw.to_path_buf()
        };
        if expanded.is_absolute() {
            expanded
                .canonicalize()
                .unwrap_or(expanded)
        } else {
            let joined = std::env::current_dir()
                .map(|cwd| cwd.join(&expanded))
                .unwrap_or(expanded);
            joined.canonicalize().unwrap_or(joined)
        }
    }

    fn remember_recent(&mut self, path: &Path) {
        self.recent.retain(|p| p != path);
        self.recent.insert(0, path.to_path_buf());
        self.recent.truncate(12);
    }

    pub fn close(&mut self, id: &str) {
        self.notes.retain(|n| n.id != id);
        if self.active_id.as_deref() == Some(id) {
            self.active_id = self.notes.first().map(|n| n.id.clone());
        }
        self.touch();
    }

    /// Update the editor buffer (the page mirrors edits up on a debounce so
    /// the dirty-dot and session persistence always see the latest draft).
    pub fn edit(&mut self, id: &str, content: String) {
        if let Some(note) = self.get_mut(id) {
            if note.content != content {
                note.content = content;
                note.dirty = true;
            }
        }
        self.epoch += 1;
    }

    pub fn save(&mut self, id: &str, content: String, force: bool) -> Result<SaveOutcome> {
        let Some(note) = self.get_mut(id) else {
            anyhow::bail!("unknown note {id}");
        };
        note.content = content;
        let disk = disk_revision(&note.path);
        if !force && disk != note.loaded_revision {
            return Ok(SaveOutcome::Conflict {
                disk,
                loaded: note.loaded_revision.clone(),
            });
        }
        if let Some(parent) = note.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(&note.path, note.content.as_bytes())
            .with_context(|| format!("writing {}", note.path.display()))?;
        note.loaded_revision = disk_revision(&note.path);
        note.dirty = false;
        let revision = note.loaded_revision.clone();
        self.touch();
        Ok(SaveOutcome::Saved { revision })
    }

    /// Drop the buffer and re-read the file — the "Reload from disk" arm of a
    /// save conflict.
    pub fn reload(&mut self, id: &str) -> Result<()> {
        let Some(note) = self.get_mut(id) else {
            anyhow::bail!("unknown note {id}");
        };
        note.content = std::fs::read_to_string(&note.path)
            .with_context(|| format!("reading {}", note.path.display()))?;
        note.loaded_revision = disk_revision(&note.path);
        note.dirty = false;
        self.touch();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_home(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "yedit-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn save_guard_conflicts_when_disk_changed_and_force_overwrites() {
        let home = temp_home("guard");
        let file = home.join("note.md");
        std::fs::write(&file, "original").unwrap();
        let mut store = Store::new(home.clone());
        let id = store.open(&file).unwrap();

        // Simulate an outside edit: content AND length change so the
        // mtime_ms:len revision moves even on coarse filesystem clocks.
        std::fs::write(&file, "outside edit, longer content").unwrap();

        match store.save(&id, "my edit".into(), false).unwrap() {
            SaveOutcome::Conflict { .. } => {}
            SaveOutcome::Saved { .. } => panic!("save must conflict, not clobber"),
        }
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "outside edit, longer content",
            "a conflicted save must not touch the file"
        );

        match store.save(&id, "my edit".into(), true).unwrap() {
            SaveOutcome::Saved { .. } => {}
            SaveOutcome::Conflict { .. } => panic!("force save must overwrite"),
        }
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "my edit");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn session_restores_tabs_active_note_and_markdown_mode() {
        let home = temp_home("session");
        let a = home.join("a.md");
        let b = home.join("b.md");
        std::fs::write(&a, "# a").unwrap();
        std::fs::write(&b, "# b").unwrap();
        {
            let mut store = Store::new(home.clone());
            store.open(&a).unwrap();
            let id_b = store.open(&b).unwrap();
            store.active_id = Some(id_b);
            store.markdown_mode = false;
            store.touch();
        }
        let restored = Store::new(home.clone());
        assert_eq!(restored.notes.len(), 2, "both tabs restore");
        assert_eq!(
            restored.active().map(|n| n.name()),
            Some("b.md".to_string()),
            "the active tab restores"
        );
        assert!(!restored.markdown_mode, "markdown mode restores");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn note_ids_are_stable_and_open_is_idempotent() {
        let home = temp_home("ids");
        let file = home.join("x.md");
        std::fs::write(&file, "x").unwrap();
        let mut store = Store::new(home.clone());
        let first = store.open(&file).unwrap();
        let second = store.open(&file).unwrap();
        assert_eq!(first, second);
        assert_eq!(store.notes.len(), 1, "re-opening focuses, never duplicates");
        let _ = std::fs::remove_dir_all(&home);
    }
}
