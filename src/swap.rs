//! Crash recovery: unsaved changes are copied to swap files in
//! `~/.local/state/m3-text/swap` a moment after typing stops, and removed
//! when the document is saved or closed. Swaps left behind by a process
//! that's gone are offered for recovery at startup.

use serde::{Deserialize, Serialize};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SwapData {
    /// The file the changes belong to (`None` for an untitled document).
    pub path: Option<PathBuf>,
    pub title: String,
    pub pid: u32,
    /// Seconds since the epoch.
    pub saved: u64,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct SwapEntry {
    pub file: PathBuf,
    pub data: SwapData,
}

pub fn dir() -> PathBuf {
    material_3_ui::sys::paths::state_dir("m3-text").join("swap")
}

/// The swap file for a document: by path, or by id for untitled ones.
pub fn file_for(dir: &Path, path: Option<&Path>, doc_id: u64) -> PathBuf {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let name = match path {
        Some(p) => {
            p.hash(&mut h);
            let stem = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            format!("{stem}-{:016x}.swp", h.finish())
        }
        None => format!("untitled-{}-{doc_id}.swp", std::process::id()),
    };
    dir.join(name)
}

pub fn write(file: &Path, data: &SwapData) -> std::io::Result<()> {
    if let Some(d) = file.parent() {
        fs::create_dir_all(d)?;
    }
    let tmp = file.with_extension("swp.tmp");
    fs::write(&tmp, serde_json::to_vec(data).map_err(std::io::Error::other)?)?;
    fs::rename(&tmp, file)
}

pub fn remove(file: &Path) {
    let _ = fs::remove_file(file);
}

fn alive(pid: u32) -> bool {
    pid == std::process::id()
        || fs::read_to_string(format!("/proc/{pid}/comm")).is_ok_and(|c| c.trim() == "m3-text")
}

/// Swaps whose editor isn't running any more.
pub fn orphans(dir: &Path) -> Vec<SwapEntry> {
    let Ok(rd) = fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<SwapEntry> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "swp"))
        .filter_map(|file| {
            let data: SwapData = serde_json::from_slice(&fs::read(&file).ok()?).ok()?;
            (!alive(data.pid)).then_some(SwapEntry { file, data })
        })
        .collect();
    out.sort_by_key(|e| std::cmp::Reverse(e.data.saved));
    out
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orphaned_swaps_are_found() {
        let dir = tempfile::tempdir().unwrap();
        let mine = file_for(dir.path(), Some(Path::new("/tmp/a.txt")), 1);
        let gone = file_for(dir.path(), Some(Path::new("/tmp/b.txt")), 2);
        assert_ne!(mine, gone);
        let data = |pid, name: &str| SwapData {
            path: Some(PathBuf::from(name)),
            title: name.into(),
            pid,
            saved: now_secs(),
            text: "unsaved".into(),
        };
        write(&mine, &data(std::process::id(), "/tmp/a.txt")).unwrap();
        // A pid that can't be a running m3-text.
        write(&gone, &data(u32::MAX - 1, "/tmp/b.txt")).unwrap();
        let found = orphans(dir.path());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].data.path.as_deref(), Some(Path::new("/tmp/b.txt")));
        assert_eq!(found[0].data.text, "unsaved");
        remove(&found[0].file);
        assert!(orphans(dir.path()).is_empty());
    }
}
