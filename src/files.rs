//! Loading and saving files: encoding detection, writing through symlinks,
//! atomic replacement when that keeps the file's identity, and `pkexec` for
//! files the user can't write.

use crate::encoding::{self, Decoded};
use std::fs;
use std::io::{ErrorKind, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Files larger than this aren't opened.
pub const MAX_SIZE: u64 = 512 * 1024 * 1024;

/// What a file looked like when we last read or wrote it, to notice
/// changes made by other programs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    pub mtime: Option<SystemTime>,
    pub len: u64,
}

pub fn stamp(path: &Path) -> Option<Stamp> {
    let m = fs::metadata(path).ok()?;
    Some(Stamp { mtime: m.modified().ok(), len: m.len() })
}

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

/// Whether we can write the file (or create it, when it doesn't exist).
pub fn writable(path: &Path) -> bool {
    let target = resolve(path);
    match fs::metadata(&target) {
        Ok(_) => fs::OpenOptions::new().append(true).open(&target).is_ok(),
        Err(_) => target.parent().is_some_and(dir_writable),
    }
}

fn dir_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".m3-text-probe-{}", std::process::id()));
    match fs::File::create(&probe) {
        Ok(_) => {
            let _ = fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// The real file behind a path: symlinks are followed (even dangling
/// ones), so saving writes through the link and the link stays.
pub fn resolve(path: &Path) -> PathBuf {
    if let Ok(real) = fs::canonicalize(path) {
        return real;
    }
    let mut p = path.to_path_buf();
    for _ in 0..32 {
        match fs::read_link(&p) {
            Ok(target) => {
                p = if target.is_absolute() { target } else { p.parent().unwrap_or(Path::new("/")).join(target) };
            }
            Err(_) => break,
        }
    }
    p
}

#[derive(Debug)]
pub enum SaveError {
    /// Permission denied: offer to save as administrator.
    NeedsRoot,
    Io(String),
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaveError::NeedsRoot => write!(f, "permission denied"),
            SaveError::Io(e) => write!(f, "{e}"),
        }
    }
}

fn io_err(e: std::io::Error) -> SaveError {
    if e.kind() == ErrorKind::PermissionDenied {
        SaveError::NeedsRoot
    } else {
        SaveError::Io(e.to_string())
    }
}

/// Write `bytes` to `path`. An existing file is replaced atomically
/// (temp file + rename, permissions kept) when that doesn't change its
/// owner or break hard links; otherwise it's rewritten in place.
pub fn save(path: &Path, bytes: &[u8]) -> Result<Stamp, SaveError> {
    let target = resolve(path);
    let dir = target.parent().ok_or_else(|| SaveError::Io("no parent folder".into()))?;
    match fs::metadata(&target) {
        Ok(meta) => {
            let atomic = meta.nlink() == 1 && atomic_replace(&target, dir, &meta, bytes).is_ok();
            if !atomic {
                let mut f = fs::OpenOptions::new().write(true).truncate(true).open(&target).map_err(io_err)?;
                f.write_all(bytes).map_err(io_err)?;
                f.sync_all().map_err(io_err)?;
            }
        }
        Err(_) => {
            if !dir.exists() {
                fs::create_dir_all(dir).map_err(io_err)?;
            }
            let mut f = fs::File::create(&target).map_err(io_err)?;
            f.write_all(bytes).map_err(io_err)?;
            f.sync_all().map_err(io_err)?;
        }
    }
    stamp(&target).ok_or_else(|| SaveError::Io("the file vanished after saving".into()))
}

fn atomic_replace(target: &Path, dir: &Path, meta: &fs::Metadata, bytes: &[u8]) -> std::io::Result<()> {
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.m3-text-{}", std::process::id()));
    let result = (|| {
        let mut f = fs::File::create(&tmp)?;
        // A file owned by someone else would change hands: don't.
        if f.metadata()?.uid() != meta.uid() || f.metadata()?.gid() != meta.gid() {
            return Err(std::io::Error::other("owner differs"));
        }
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::set_permissions(&tmp, meta.permissions())?;
        fs::rename(&tmp, target)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Save through `pkexec` (one password prompt). The file is rewritten in
/// place, so its owner and mode stay.
pub fn save_as_root(path: &Path, bytes: &[u8]) -> Result<Stamp, SaveError> {
    let target = resolve(path);
    let script = r#"set -e; mkdir -p "$(dirname "$1")"; cat > "$1""#;
    material_3_ui::sys::cmd::run_with_stdin("pkexec", &["sh", "-c", script, "m3-text", &target.to_string_lossy()], bytes)
        .map_err(SaveError::Io)?;
    stamp(&target).ok_or_else(|| SaveError::Io("couldn't read the file back".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_through_symlinks_and_keeps_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.conf");
        fs::write(&real, "old").unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o640)).unwrap();
        let link = dir.path().join("link.conf");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        save(&link, b"new").unwrap();
        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "link kept");
        assert_eq!(fs::read_to_string(&real).unwrap(), "new");
        assert_eq!(fs::metadata(&real).unwrap().permissions().mode() & 0o777, 0o640);
        let leftovers = fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(leftovers, 2, "no temp files left");
    }

    #[test]
    fn hard_links_are_written_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        fs::write(&a, "1").unwrap();
        fs::hard_link(&a, &b).unwrap();
        save(&a, b"2").unwrap();
        assert_eq!(fs::read_to_string(&b).unwrap(), "2");
    }

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

    #[test]
    fn dangling_symlink_resolves_to_target() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("l");
        std::os::unix::fs::symlink("missing.txt", &link).unwrap();
        assert_eq!(resolve(&link), dir.path().join("missing.txt"));
        save(&link, b"x").unwrap();
        assert_eq!(fs::read_to_string(dir.path().join("missing.txt")).unwrap(), "x");
    }
}
