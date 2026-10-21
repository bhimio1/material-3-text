//! The folder sidebar: a lazily loaded file tree that honours .gitignore,
//! the file index for quick open, and file operations.

use crate::search::{walker, WalkOptions};
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Node {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub expanded: bool,
    /// Loaded when first expanded.
    pub children: Option<Vec<Node>>,
}

impl Node {
    fn new(path: PathBuf, is_dir: bool) -> Self {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
        Self { path, name, is_dir, expanded: false, children: None }
    }
}

/// Natural-ish ordering: folders first, then case-insensitive names.
fn sort(nodes: &mut [Node]) {
    nodes.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
}

pub fn read_dir(dir: &Path, opts: WalkOptions) -> Vec<Node> {
    let mut b = ignore::WalkBuilder::new(dir);
    b.max_depth(Some(1))
        .hidden(!opts.hidden)
        .git_ignore(!opts.ignored)
        .git_global(!opts.ignored)
        .git_exclude(!opts.ignored)
        .ignore(!opts.ignored)
        .require_git(false)
        .filter_entry(|e| e.file_name() != ".git");
    let mut out: Vec<Node> = b
        .build()
        .flatten()
        .filter(|e| e.depth() == 1)
        .map(|e| {
            let is_dir = e.file_type().is_some_and(|t| t.is_dir())
                || (e.path_is_symlink() && e.path().is_dir());
            Node::new(e.into_path(), is_dir)
        })
        .collect();
    sort(&mut out);
    out
}

pub struct Tree {
    pub root: Node,
    pub opts: WalkOptions,
}

impl Tree {
    pub fn open(root: &Path, opts: WalkOptions) -> Self {
        let mut node = Node::new(root.to_path_buf(), true);
        node.expanded = true;
        node.children = Some(read_dir(root, opts));
        Self { root: node, opts }
    }

    fn find_mut<'a>(node: &'a mut Node, path: &Path) -> Option<&'a mut Node> {
        if node.path == path {
            return Some(node);
        }
        if !path.starts_with(&node.path) {
            return None;
        }
        node.children.as_mut()?.iter_mut().find_map(|c| Self::find_mut(c, path))
    }

    pub fn toggle(&mut self, path: &Path) {
        let opts = self.opts;
        if let Some(n) = Self::find_mut(&mut self.root, path) {
            n.expanded = !n.expanded;
            if n.expanded && n.children.is_none() {
                n.children = Some(read_dir(&n.path, opts));
            }
        }
    }

    /// Expand the folders down to `path`.
    pub fn reveal(&mut self, path: &Path) {
        let opts = self.opts;
        let Ok(rel) = path.strip_prefix(&self.root.path) else { return };
        let mut cur = self.root.path.clone();
        let comps: Vec<_> = rel.components().collect();
        for c in comps.iter().take(comps.len().saturating_sub(1)) {
            cur.push(c);
            if let Some(n) = Self::find_mut(&mut self.root, &cur) {
                n.expanded = true;
                if n.children.is_none() {
                    n.children = Some(read_dir(&n.path, opts));
                }
            }
        }
    }

    /// Re-read every loaded folder, keeping what's expanded.
    pub fn refresh(&mut self) {
        fn walk(n: &mut Node, opts: WalkOptions) {
            if let Some(old) = n.children.take() {
                let mut fresh = read_dir(&n.path, opts);
                for f in &mut fresh {
                    if let Some(o) = old.iter().find(|o| o.path == f.path && o.is_dir == f.is_dir) {
                        f.expanded = o.expanded;
                        f.children = o.children.clone();
                        if f.children.is_some() {
                            walk(f, opts);
                        }
                    }
                }
                n.children = Some(fresh);
            }
        }
        walk(&mut self.root, self.opts);
    }

    /// The rows to draw: (depth, node), depth-first, inside expanded folders.
    pub fn rows(&self) -> Vec<(usize, &Node)> {
        fn walk<'a>(n: &'a Node, depth: usize, out: &mut Vec<(usize, &'a Node)>) {
            for c in n.children.iter().flatten() {
                out.push((depth, c));
                if c.is_dir && c.expanded {
                    walk(c, depth + 1, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.root, 0, &mut out);
        out
    }
}

/// Every file under `root` (relative paths), for quick open.
pub fn index(root: &Path, opts: WalkOptions, limit: usize) -> Vec<PathBuf> {
    walker(root, opts)
        .flatten()
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .filter_map(|e| e.path().strip_prefix(root).ok().map(Path::to_path_buf))
        .take(limit)
        .collect()
}

/// Fuzzy-match paths. The file name counts more than the folders.
/// Returns (index into `paths`, matched char positions), best first.
pub fn fuzzy(paths: &[PathBuf], query: &str, limit: usize) -> Vec<(usize, Vec<usize>)> {
    let matcher = SkimMatcherV2::default().smart_case();
    let mut scored: Vec<(i64, usize, Vec<usize>)> = paths
        .iter()
        .enumerate()
        .filter_map(|(i, p)| {
            let s = p.to_string_lossy();
            let (mut score, idx) = matcher.fuzzy_indices(&s, query)?;
            let name_start = s.rfind('/').map_or(0, |i| s[..=i].chars().count());
            if let Some((name_score, _)) = matcher.fuzzy_indices(&s[s.rfind('/').map_or(0, |i| i + 1)..], query) {
                score += name_score;
            }
            let in_name = idx.iter().filter(|&&c| c >= name_start).count() as i64;
            score += in_name * 2 - (s.len() as i64) / 8;
            Some((score, i, idx))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().take(limit).map(|(_, i, idx)| (i, idx)).collect()
}

/// Move to the trash with `gio trash`, else delete.
pub fn delete(path: &Path) -> Result<(), String> {
    if material_3_ui::sys::cmd::exists("gio")
        && material_3_ui::sys::cmd::run("gio", &["trash", &path.to_string_lossy()]).is_ok()
    {
        return Ok(());
    }
    let r = if path.is_dir() { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) };
    r.map_err(|e| e.to_string())
}

/// A file or folder name the user typed: no separators, not `.`/`..`.
pub fn valid_name(name: &str) -> Result<&str, &'static str> {
    let n = name.trim();
    if n.is_empty() {
        Err("Enter a name")
    } else if n.contains('/') || n.contains('\0') {
        Err("Names can't contain /")
    } else if n == "." || n == ".." {
        Err("That name is reserved")
    } else {
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn project() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join(".gitignore"), "target/\n*.log\n").unwrap();
        fs::create_dir_all(d.path().join("src/ui")).unwrap();
        fs::create_dir_all(d.path().join("target")).unwrap();
        fs::write(d.path().join("src/main.rs"), "").unwrap();
        fs::write(d.path().join("src/ui/editor.rs"), "").unwrap();
        fs::write(d.path().join("Cargo.toml"), "").unwrap();
        fs::write(d.path().join("debug.log"), "").unwrap();
        fs::write(d.path().join("README.md"), "").unwrap();
        d
    }

    #[test]
    fn tree_hides_ignored_and_sorts_folders_first() {
        let d = project();
        let mut t = Tree::open(d.path(), WalkOptions::default());
        let names: Vec<_> = t.rows().iter().map(|(_, n)| n.name.clone()).collect();
        assert_eq!(names, vec!["src", "Cargo.toml", "README.md"]);
        t.toggle(&d.path().join("src"));
        let names: Vec<_> = t.rows().iter().map(|(depth, n)| format!("{depth}{}", n.name)).collect();
        assert_eq!(names, vec!["0src", "1ui", "1main.rs", "0Cargo.toml", "0README.md"]);

        let all = Tree::open(d.path(), WalkOptions { hidden: true, ignored: true });
        let names: Vec<_> = all.rows().iter().map(|(_, n)| n.name.clone()).collect();
        assert!(names.contains(&"target".to_string()) && names.contains(&".gitignore".to_string()));
    }

    #[test]
    fn reveal_and_refresh_keep_expansion() {
        let d = project();
        let mut t = Tree::open(d.path(), WalkOptions::default());
        t.reveal(&d.path().join("src/ui/editor.rs"));
        assert_eq!(t.rows().len(), 6);
        fs::write(d.path().join("src/ui/new.rs"), "").unwrap();
        t.refresh();
        assert!(t.rows().iter().any(|(_, n)| n.name == "new.rs"));
    }

    #[test]
    fn index_and_fuzzy() {
        let d = project();
        let files = index(d.path(), WalkOptions::default(), 1000);
        assert_eq!(files.len(), 4, "{files:?}");
        let hits = fuzzy(&files, "edit", 10);
        assert_eq!(files[hits[0].0], PathBuf::from("src/ui/editor.rs"));
        let hits = fuzzy(&files, "main", 10);
        assert_eq!(files[hits[0].0], PathBuf::from("src/main.rs"));
        assert!(fuzzy(&files, "zzz", 10).is_empty());
    }

    #[test]
    fn names_are_checked() {
        assert!(valid_name("a/b").is_err());
        assert!(valid_name("  ").is_err());
        assert_eq!(valid_name(" x.rs "), Ok("x.rs"));
    }
}
