//! Command line: `m3-text [FILE|DIR]… [+LINE]`.
//!
//! `+LINE` applies to the file after it, or to the one before it when it
//! comes last (`m3-text notes.md +40`). `FILE:LINE[:COL]` works too when
//! no file has that literal name. A folder opens in the sidebar; `-` reads
//! standard input into a new document.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenRequest {
    pub path: PathBuf,
    pub line: Option<usize>,
    pub col: Option<usize>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Args {
    pub files: Vec<OpenRequest>,
    pub folder: Option<PathBuf>,
    pub stdin: bool,
    pub help: bool,
    pub version: bool,
}

pub const USAGE: &str = "Usage: m3-text [FILE|DIR]... [+LINE]

Opens files in tabs and a folder in the sidebar.

  +LINE            go to LINE in the next file (or the previous one, at the end)
  FILE:LINE[:COL]  open FILE at LINE and column
  -                read standard input into a new document
  -h, --help       show this help
  -V, --version    show the version
";

fn parse_num(s: &str) -> Option<usize> {
    s.parse::<usize>().ok().filter(|n| *n > 0)
}

/// `path:12` or `path:12:4`, when `path` exists (or the full text doesn't).
fn split_position(arg: &str, exists: &dyn Fn(&Path) -> bool) -> (PathBuf, Option<usize>, Option<usize>) {
    if exists(Path::new(arg)) {
        return (PathBuf::from(arg), None, None);
    }
    let parts: Vec<&str> = arg.rsplitn(3, ':').collect();
    match parts.as_slice() {
        [c, l, p] if parse_num(l).is_some() && parse_num(c).is_some() && !p.is_empty() => {
            (PathBuf::from(p), parse_num(l), parse_num(c))
        }
        [l, p, ..] if parse_num(l).is_some() && !p.is_empty() => {
            let p = arg[..arg.len() - l.len() - 1].to_string();
            (PathBuf::from(p), parse_num(l), None)
        }
        _ => (PathBuf::from(arg), None, None),
    }
}

pub fn parse(args: impl IntoIterator<Item = String>, cwd: &Path, exists: &dyn Fn(&Path) -> bool, is_dir: &dyn Fn(&Path) -> bool) -> Args {
    let mut out = Args::default();
    let mut pending_line: Option<usize> = None;
    let mut only_paths = false;
    for arg in args {
        if !only_paths {
            match arg.as_str() {
                "-h" | "--help" => {
                    out.help = true;
                    continue;
                }
                "-V" | "--version" => {
                    out.version = true;
                    continue;
                }
                "--" => {
                    only_paths = true;
                    continue;
                }
                "-" => {
                    out.stdin = true;
                    continue;
                }
                _ => {}
            }
            if let Some(n) = arg.strip_prefix('+').and_then(parse_num) {
                pending_line = Some(n);
                continue;
            }
        }
        let (path, line, col) = split_position(&arg, exists);
        let path = if path.is_absolute() { path } else { cwd.join(path) };
        if is_dir(&path) {
            out.folder = Some(path);
            continue;
        }
        out.files.push(OpenRequest { path, line: line.or(pending_line.take()), col });
    }
    if let (Some(n), Some(last)) = (pending_line, out.files.last_mut()) {
        last.line.get_or_insert(n);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> Args {
        let exists = |p: &Path| p.ends_with("weird:1") || p == Path::new("weird:1");
        let is_dir = |p: &Path| p.ends_with("proj");
        parse(args.iter().map(|s| s.to_string()), Path::new("/home/u"), &exists, &is_dir)
    }

    #[test]
    fn files_folders_and_lines() {
        let a = run(&["a.rs", "+12", "b.rs", "proj"]);
        assert_eq!(a.folder, Some(PathBuf::from("/home/u/proj")));
        assert_eq!(a.files.len(), 2);
        assert_eq!(a.files[0], OpenRequest { path: "/home/u/a.rs".into(), line: None, col: None });
        assert_eq!(a.files[1].line, Some(12));
    }

    #[test]
    fn trailing_line_applies_to_previous_file() {
        let a = run(&["/etc/hosts", "+3"]);
        assert_eq!(a.files[0].path, PathBuf::from("/etc/hosts"));
        assert_eq!(a.files[0].line, Some(3));
    }

    #[test]
    fn colon_positions() {
        let a = run(&["src/main.rs:40:7", "x.txt:9", "weird:1"]);
        assert_eq!(a.files[0], OpenRequest { path: "/home/u/src/main.rs".into(), line: Some(40), col: Some(7) });
        assert_eq!(a.files[1].line, Some(9));
        assert_eq!(a.files[2].path, PathBuf::from("/home/u/weird:1"), "a real file with a colon wins");
        assert_eq!(a.files[2].line, None);
    }

    #[test]
    fn flags() {
        let a = run(&["--help", "-", "--", "-V"]);
        assert!(a.help && a.stdin && !a.version);
        assert_eq!(a.files[0].path, PathBuf::from("/home/u/-V"));
    }
}
