//! Find and replace: in a document, and across a folder in the background.

use regex::{Regex, RegexBuilder};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Stop collecting after this many matches.
pub const MAX_MATCHES: usize = 100_000;
const FOLDER_MAX_HITS: usize = 10_000;
const FOLDER_MAX_FILE: u64 = 8 * 1024 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Query {
    pub text: String,
    pub regex: bool,
    pub case: bool,
    pub whole_word: bool,
}

impl Query {
    /// The query as a regex; `Ok(None)` when it's empty.
    pub fn compile(&self) -> Result<Option<Regex>, String> {
        if self.text.is_empty() {
            return Ok(None);
        }
        let mut pat = if self.regex { self.text.clone() } else { regex::escape(&self.text) };
        if self.whole_word {
            pat = format!(r"\b(?:{pat})\b");
        }
        RegexBuilder::new(&pat)
            .case_insensitive(!self.case)
            .multi_line(true)
            .size_limit(50 << 20)
            .build()
            .map(Some)
            .map_err(|e| match e {
                regex::Error::Syntax(s) => s.lines().last().unwrap_or("invalid pattern").trim().to_string(),
                other => other.to_string(),
            })
    }
}

/// Byte ranges of the non-empty matches.
pub fn find_all(re: &Regex, text: &str, limit: usize) -> Vec<Range<usize>> {
    re.find_iter(text).filter(|m| !m.is_empty()).take(limit).map(|m| m.range()).collect()
}

/// The text replacing one match: `$1`-style groups expand in regex mode.
pub fn replacement_for(re: &Regex, text: &str, range: Range<usize>, with: &str, regex_mode: bool) -> String {
    if !regex_mode {
        return with.to_string();
    }
    // Re-run at the match so look-arounds and anchors see the context.
    match re.captures_at(text, range.start) {
        Some(caps) if caps.get(0).map(|m| m.range()) == Some(range.clone()) => {
            let mut out = String::new();
            caps.expand(with, &mut out);
            out
        }
        _ => with.to_string(),
    }
}

/// One match in a folder search.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    /// 0-based line.
    pub line: usize,
    /// Char column of the match start in the line.
    pub col: usize,
    /// The line, trimmed for display.
    pub preview: String,
    /// The match within `preview` (bytes).
    pub range: Range<usize>,
}

#[derive(Clone, Debug)]
pub struct FileHits {
    pub path: PathBuf,
    pub hits: Vec<Hit>,
}

/// Matches in one file's text, with line numbers and previews.
pub fn hits_in(re: &Regex, text: &str, limit: usize) -> Vec<Hit> {
    let mut out = Vec::new();
    let mut line = 0usize;
    let mut scanned = 0usize;
    let mut line_start = 0usize;
    for m in re.find_iter(text).filter(|m| !m.is_empty()).take(limit) {
        let nl = text[scanned..m.start()].bytes().filter(|b| *b == b'\n').count();
        if nl > 0 {
            line += nl;
            line_start = text[..m.start()].rfind('\n').map_or(0, |i| i + 1);
        }
        scanned = m.start();
        let line_end = text[m.start()..].find('\n').map_or(text.len(), |i| m.start() + i);
        let (preview, range) = preview(&text[line_start..line_end], m.start() - line_start, m.end().min(line_end) - line_start);
        out.push(Hit { line, col: text[line_start..m.start()].chars().count(), preview, range });
    }
    out
}

/// Trim a long line around the match for the results list.
fn preview(line: &str, start: usize, end: usize) -> (String, Range<usize>) {
    let lead = line.len() - line.trim_start().len();
    let mut from = lead.min(start);
    // Keep a little context before the match; the sidebar is narrow.
    if start - from > 24 {
        from = start - 16;
        while !line.is_char_boundary(from) {
            from += 1;
        }
    }
    let mut to = line.len().min(end.max(start) + 120);
    while !line.is_char_boundary(to) {
        to -= 1;
    }
    let to = to.max(end);
    let prefix = if from > lead { "…" } else { "" };
    let text = format!("{prefix}{}", &line[from..to]);
    let shift = prefix.len();
    (text, start - from + shift..end - from + shift)
}

/// Whether bytes look like text worth searching.
fn is_text(bytes: &[u8]) -> bool {
    !bytes[..bytes.len().min(8192)].contains(&0)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct WalkOptions {
    pub hidden: bool,
    pub ignored: bool,
}

pub fn walker(root: &Path, opts: WalkOptions) -> ignore::Walk {
    ignore::WalkBuilder::new(root)
        .hidden(!opts.hidden)
        .git_ignore(!opts.ignored)
        .git_global(!opts.ignored)
        .git_exclude(!opts.ignored)
        .ignore(!opts.ignored)
        .require_git(false)
        .filter_entry(|e| e.file_name() != ".git")
        .build()
}

enum Msg {
    File(FileHits),
    Done { searched: usize, truncated: bool },
}

/// A search across a folder, running on a background thread.
pub struct FolderSearch {
    rx: flume::Receiver<Msg>,
    cancel: Arc<AtomicBool>,
    pub query: Query,
    pub results: Vec<FileHits>,
    pub total: usize,
    pub searched: usize,
    pub done: bool,
    pub truncated: bool,
}

impl FolderSearch {
    pub fn start(root: PathBuf, query: Query, re: Regex, opts: WalkOptions, repaint: impl Fn() + Send + 'static) -> Self {
        let (tx, rx) = flume::unbounded();
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        std::thread::spawn(move || {
            let mut total = 0usize;
            let mut searched = 0usize;
            let mut truncated = false;
            for entry in walker(&root, opts).flatten() {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                if !entry.file_type().is_some_and(|t| t.is_file()) {
                    continue;
                }
                if entry.metadata().map_or(true, |m| m.len() > FOLDER_MAX_FILE) {
                    continue;
                }
                let Ok(bytes) = std::fs::read(entry.path()) else { continue };
                if !is_text(&bytes) {
                    continue;
                }
                searched += 1;
                let text = String::from_utf8_lossy(&bytes);
                let hits = hits_in(&re, &text, FOLDER_MAX_HITS - total);
                if !hits.is_empty() {
                    total += hits.len();
                    let _ = tx.send(Msg::File(FileHits { path: entry.path().to_path_buf(), hits }));
                    repaint();
                }
                if total >= FOLDER_MAX_HITS {
                    truncated = true;
                    break;
                }
            }
            let _ = tx.send(Msg::Done { searched, truncated });
            repaint();
        });
        Self { rx, cancel, query, results: Vec::new(), total: 0, searched: 0, done: false, truncated: false }
    }

    pub fn poll(&mut self) {
        for msg in self.rx.try_iter() {
            match msg {
                Msg::File(f) => {
                    self.total += f.hits.len();
                    self.results.push(f);
                }
                Msg::Done { searched, truncated } => {
                    self.searched = searched;
                    self.truncated = truncated;
                    self.done = true;
                }
            }
        }
    }
}

impl Drop for FolderSearch {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Replace every match in a file on disk. Only UTF-8 files are changed;
/// returns the number of replacements.
pub fn replace_in_file(path: &Path, re: &Regex, with: &str, regex_mode: bool) -> Result<usize, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let text = String::from_utf8(bytes).map_err(|_| "not UTF-8, skipped".to_string())?;
    let (new, n) = replace_text(re, &text, with, regex_mode);
    if n > 0 {
        crate::files::save(path, new.as_bytes()).map_err(|e| e.to_string())?;
    }
    Ok(n)
}

pub fn replace_text(re: &Regex, text: &str, with: &str, regex_mode: bool) -> (String, usize) {
    let ranges = find_all(re, text, usize::MAX);
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for r in &ranges {
        out.push_str(&text[last..r.start]);
        out.push_str(&replacement_for(re, text, r.clone(), with, regex_mode));
        last = r.end;
    }
    out.push_str(&text[last..]);
    (out, ranges.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(text: &str, regex: bool, case: bool, whole_word: bool) -> Regex {
        Query { text: text.into(), regex, case, whole_word }.compile().unwrap().unwrap()
    }

    #[test]
    fn query_options() {
        let t = "Foo foo food f.o";
        assert_eq!(find_all(&q("foo", false, false, false), t, 99).len(), 3);
        assert_eq!(find_all(&q("foo", false, true, false), t, 99).len(), 2);
        assert_eq!(find_all(&q("foo", false, false, true), t, 99).len(), 2);
        assert_eq!(find_all(&q("f.o", false, false, false), t, 99), vec![13..16]);
        assert_eq!(find_all(&q("f.o", true, false, false), t, 99).len(), 4);
        assert!(Query { text: "(".into(), regex: true, ..Default::default() }.compile().is_err());
        assert!(Query::default().compile().unwrap().is_none());
    }

    #[test]
    fn regex_replacement_expands_groups() {
        let re = q(r"(\w+)=(\d+)", true, true, false);
        let (out, n) = replace_text(&re, "a=1 b=2", "$2:$1", true);
        assert_eq!((out.as_str(), n), ("1:a 2:b", 2));
        let (out, _) = replace_text(&re, "a=1", "$2", false);
        assert_eq!(out, "$2", "plain mode is literal");
    }

    #[test]
    fn hits_have_lines_and_previews() {
        let re = q("needle", false, true, false);
        let text = "one\n    two needle\nthree\nneedle needle";
        let hits = hits_in(&re, text, 99);
        assert_eq!(hits.len(), 3);
        assert_eq!((hits[0].line, hits[0].col), (1, 8));
        assert_eq!(hits[0].preview, "two needle");
        assert_eq!(&hits[0].preview[hits[0].range.clone()], "needle");
        assert_eq!((hits[2].line, hits[2].col), (3, 7));
        let long = format!("{}needle{}", "x".repeat(200), "y".repeat(300));
        let h = &hits_in(&re, &long, 9)[0];
        assert!(h.preview.starts_with('…'));
        assert_eq!(&h.preview[h.range.clone()], "needle");
    }

    #[test]
    fn folder_search_respects_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".gitignore"), "target/\n").unwrap();
        std::fs::create_dir(dir.path().join("target")).unwrap();
        std::fs::write(dir.path().join("target/out.txt"), "needle").unwrap();
        std::fs::write(dir.path().join("a.txt"), "x\nneedle\n").unwrap();
        std::fs::write(dir.path().join("bin"), b"needle\0\0").unwrap();
        let re = q("needle", false, true, false);
        let mut s = FolderSearch::start(dir.path().into(), Query::default(), re.clone(), WalkOptions::default(), || {});
        while !s.done {
            std::thread::sleep(std::time::Duration::from_millis(5));
            s.poll();
        }
        assert_eq!(s.results.len(), 1);
        assert!(s.results[0].path.ends_with("a.txt"));
        assert_eq!(s.results[0].hits[0].line, 1);

        let n = replace_in_file(&dir.path().join("a.txt"), &re, "pin", false).unwrap();
        assert_eq!(n, 1);
        assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "x\npin\n");
    }
}
