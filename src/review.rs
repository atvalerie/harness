//! Optimistic checkpoints: rollback refuses to overwrite subsequent edits.
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
#[derive(Clone, Default)]
pub struct ReviewStore {
    entries: Arc<Mutex<Vec<Checkpoint>>>,
    journal: Arc<Mutex<Option<PathBuf>>>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub id: usize,
    pub path: PathBuf,
    before: Option<Vec<u8>>,
    after: Option<Vec<u8>>,
    pub turn: u64,
    restored: bool,
}
pub struct Pending {
    path: PathBuf,
    before: Option<Vec<u8>>,
    turn: u64,
}
fn read(path: &Path) -> Result<Option<Vec<u8>>, String> {
    match std::fs::read(path) {
        Ok(bytes) if bytes.len() <= 2_000_000 => Ok(Some(bytes)),
        Ok(_) => Err("Checkpoint limit: file exceeds 2 MB".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}
impl ReviewStore {
    pub fn bind(&self, session: &Path, load: bool) -> Result<(), String> {
        let path = session.with_extension("changes.jsonl");
        *self
            .journal
            .lock()
            .map_err(|_| "Checkpoint journal unavailable")? = Some(path.clone());
        if load {
            if let Ok(file) = std::fs::File::open(path) {
                use std::io::BufRead;
                let mut records = std::collections::BTreeMap::new();
                for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
                    if let Ok(checkpoint) = serde_json::from_str::<Checkpoint>(&line) {
                        records.insert(checkpoint.id, checkpoint);
                    }
                }
                if !records.is_empty() {
                    *self
                        .entries
                        .lock()
                        .map_err(|_| "Checkpoint store unavailable")? =
                        records.into_values().collect();
                }
            }
        }
        Ok(())
    }
    fn persist(&self, entry: &Checkpoint) -> Result<(), String> {
        let path = self
            .journal
            .lock()
            .map_err(|_| "Checkpoint journal unavailable")?
            .clone();
        if let Some(path) = path {
            use std::io::Write;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let mut options = std::fs::OpenOptions::new();
            options.append(true).create(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(path).map_err(|e| e.to_string())?;
            let mut bytes = serde_json::to_vec(entry).map_err(|e| e.to_string())?;
            bytes.insert(0, b'\n');
            bytes.push(b'\n');
            file.write_all(&bytes)
                .and_then(|_| file.sync_data())
                .map_err(|e| format!("Checkpoint journal failed: {e}"))?;
        }
        Ok(())
    }
    pub fn before(&self, root: &Path, path: &str, turn: u64) -> Result<Pending, String> {
        let root = root.canonicalize().map_err(|e| e.to_string())?;
        let path = crate::tools::resolve_path(path, &root);
        let resolved = if path.exists() {
            path.canonicalize()
        } else {
            path.parent()
                .unwrap_or(&root)
                .canonicalize()
                .map(|parent| parent.join(path.file_name().unwrap_or_default()))
        }
        .map_err(|e| e.to_string())?;
        if !resolved.starts_with(&root) {
            return Err("Checkpoint refuses a path outside the workspace".into());
        }
        Ok(Pending {
            before: read(&resolved)?,
            path: resolved,
            turn,
        })
    }
    pub fn after(&self, pending: Pending) -> Result<(), String> {
        let after = read(&pending.path)?;
        if pending.before == after {
            return Ok(());
        }
        let mut all = self
            .entries
            .lock()
            .map_err(|_| "Checkpoint store unavailable")?;
        let id = all
            .iter()
            .map(|c| c.id)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        all.push(Checkpoint {
            id,
            path: pending.path,
            before: pending.before,
            after,
            turn: pending.turn,
            restored: false,
        });
        self.persist(all.last().unwrap())?;
        Ok(())
    }
    pub fn snapshot(&self) -> Vec<Checkpoint> {
        self.entries.lock().map(|v| v.clone()).unwrap_or_default()
    }
    pub fn restore(&self, entries: Vec<Checkpoint>) {
        if let Ok(mut all) = self.entries.lock() {
            *all = entries;
        }
    }
    pub fn summary(&self) -> String {
        self.entries
            .lock()
            .map(|all| {
                all.iter()
                    .map(|c| {
                        format!(
                            "#{} turn {} {}{}",
                            c.id,
                            c.turn,
                            c.path.display(),
                            if c.restored { " [restored]" } else { "" }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    }
    pub fn diff(&self, id: usize) -> String {
        self.entries
            .lock()
            .ok()
            .and_then(|all| {
                all.iter().find(|c| c.id == id).map(|c| {
                    let before = String::from_utf8_lossy(c.before.as_deref().unwrap_or_default());
                    let after = String::from_utf8_lossy(c.after.as_deref().unwrap_or_default());
                    similar::TextDiff::from_lines(&before, &after)
                        .unified_diff()
                        .header("before", "after")
                        .to_string()
                })
            })
            .unwrap_or_else(|| "Unknown checkpoint".into())
    }
    pub fn rollback(&self, root: &Path, id: usize) -> Result<String, String> {
        let mut all = self
            .entries
            .lock()
            .map_err(|_| "Checkpoint store unavailable")?;
        let c = all
            .iter_mut()
            .find(|c| c.id == id)
            .ok_or("Unknown checkpoint")?;
        let root = root.canonicalize().map_err(|e| e.to_string())?;
        if !c.path.starts_with(&root) {
            return Err("Checkpoint is outside the current workspace".into());
        }
        if c.restored {
            return Err("Checkpoint already restored".into());
        }
        // Canonical identity check also rejects a parent replaced by a symlink.
        let current = if c.path.exists() {
            c.path.canonicalize()
        } else {
            c.path
                .parent()
                .unwrap()
                .canonicalize()
                .map(|p| p.join(c.path.file_name().unwrap()))
        }
        .map_err(|e| e.to_string())?;
        if current != c.path || read(&c.path)? != c.after {
            return Err(
                "Rollback conflict: file changed since this checkpoint; nothing overwritten".into(),
            );
        }
        match &c.before {
            Some(bytes) => std::fs::write(&c.path, bytes).map_err(|e| e.to_string())?,
            None => {
                if c.path.exists() {
                    std::fs::remove_file(&c.path).map_err(|e| e.to_string())?;
                }
            }
        }
        c.restored = true;
        self.persist(c)?;
        Ok(format!(
            "Restored checkpoint #{id}: {} (conversation unchanged)",
            c.path.display()
        ))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rollback_refuses_later_edits() {
        let root = std::env::temp_dir().join(format!("holiday-review-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("test.txt");
        std::fs::write(&path, "before").unwrap();
        let store = ReviewStore::default();
        let pending = store.before(&root, "test.txt", 1).unwrap();
        std::fs::write(&path, "after").unwrap();
        store.after(pending).unwrap();
        std::fs::write(&path, "user edit").unwrap();
        assert!(store.rollback(&root, 1).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "user edit");
        std::fs::write(&path, "after").unwrap();
        store.rollback(&root, 1).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "before");
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
}
