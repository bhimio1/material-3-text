//! Loading files with encoding detection. Saving (through symlinks,
//! atomically when that keeps the file's identity, `pkexec` for files the
//! user can't write) is material-3-ui's `fs`.

use crate::encoding::{self, Decoded};
use std::fs;
use std::path::Path;

pub use material_3_ui::fs::{resolve, save, stamp, writable, SaveError, Stamp};

/// Files larger than this aren't opened.
pub const MAX_SIZE: u64 = 512 * 1024 * 1024;

pub struct Loaded {
    pub decoded: Decoded,
    pub bytes: Vec<u8>,
    pub stamp: Option<Stamp>,
    /// We can't write it ourselves (saving goes through pkexec).
    pub readonly: bool,
}

#[derive(Debug)]
pub enum LoadError {
    Binary,
    TooLarge(u64),
    Io(String),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Binary => write!(f, "it looks like a binary file"),
            LoadError::TooLarge(n) => write!(f, "it's too large ({} MB)", n / 1024 / 1024),
            LoadError::Io(e) => write!(f, "{e}"),
        }
    }
}

pub fn load(path: &Path) -> Result<Loaded, LoadError> {
    let meta = fs::metadata(path).map_err(|e| LoadError::Io(e.to_string()))?;
    if meta.is_dir() {
        return Err(LoadError::Io("it's a folder".into()));
    }
    if meta.len() > MAX_SIZE {
        return Err(LoadError::TooLarge(meta.len()));
    }
    let bytes = fs::read(path).map_err(|e| LoadError::Io(e.to_string()))?;
    let decoded = encoding::decode(&bytes).map_err(|_| LoadError::Binary)?;
    Ok(Loaded {
        decoded,
        bytes,
        stamp: stamp(path),
        readonly: !writable(path),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_new_files_and_folders() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub/new.txt");
        let st = save(&p, b"hello").unwrap();
        assert_eq!(st.len, 5);
        let loaded = load(&p).unwrap();
        assert_eq!(loaded.decoded.text, "hello");
        assert!(!loaded.readonly);
    }

    #[test]
    fn load_refuses_binary() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bin");
        fs::write(&p, b"\x00\x01\x02\x03binary").unwrap();
        assert!(matches!(load(&p), Err(LoadError::Binary)));
    }
}
