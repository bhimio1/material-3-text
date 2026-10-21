//! An open document: its text, file details, highlighting and view.

use crate::buffer::{Buffer, Indent};
use crate::config::EditorSettings;
use crate::editor::ViewState;
use crate::encoding::{self, LineEnding, TextEncoding};
use crate::files::{self, Loaded, Stamp};
use crate::search::{self, Query};
use crate::swap::{self, SwapData};
use crate::syntax::{self, Highlight};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// A change made to the file by another program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum External {
    Changed,
    Deleted,
}

/// Find matches cached for one version of the text.
pub struct FindCache {
    key: (u64, Query),
    pub matches: Vec<(usize, usize)>,
    pub error: Option<String>,
    pub truncated: bool,
}

pub struct Doc {
    pub id: u64,
    pub buffer: Buffer,
    pub path: Option<PathBuf>,
    /// "Untitled N".
    pub untitled: usize,
    pub encoding: TextEncoding,
    pub line_ending: LineEnding,
    pub stamp: Option<Stamp>,
    /// Saving needs administrator rights.
    pub readonly: bool,
    pub highlight: Highlight,
    /// Picked by hand in the status bar (not re-detected on rename).
    pub language_pinned: bool,
    pub view: ViewState,
    pub external: Option<External>,
    pub last_edit: Option<Instant>,
    /// The buffer version last written to the swap file.
    pub swapped_version: u64,
    pub swap_file: Option<PathBuf>,
    pub preview: bool,
    pub find: Option<FindCache>,
    /// Bytes that didn't decode cleanly / line endings were mixed.
    pub lossy: bool,
    pub mixed_endings: bool,
    /// The bytes as last read, for "Reopen with encoding".
    pub raw: Option<Vec<u8>>,
}

impl Doc {
    pub fn untitled(id: u64, n: usize, settings: &EditorSettings) -> Self {
        let mut buffer = Buffer::new("");
        buffer.indent = Indent { tabs: !settings.insert_spaces, width: settings.tab_size };
        Self {
            id,
            buffer,
            path: None,
            untitled: n,
            encoding: TextEncoding::default(),
            line_ending: LineEnding::Lf,
            stamp: None,
            readonly: false,
            highlight: Highlight::new(syntax::by_name("Plain Text")),
            language_pinned: false,
            view: ViewState::default(),
            external: None,
            last_edit: None,
            swapped_version: 0,
            swap_file: None,
            preview: false,
            find: None,
            lossy: false,
            mixed_endings: false,
            raw: None,
        }
    }

    pub fn from_file(id: u64, path: &Path, loaded: Loaded, settings: &EditorSettings) -> Self {
        let mut doc = Self::untitled(id, 0, settings);
        doc.path = Some(path.to_path_buf());
        doc.apply_loaded(loaded, settings);
        doc.detect_language();
        doc.preview = false;
        doc
    }

    fn apply_loaded(&mut self, loaded: Loaded, settings: &EditorSettings) {
        let d = loaded.decoded;
        self.buffer = Buffer::new(&d.text);
        self.buffer.indent = match (settings.detect_indent, Indent::detect(self.buffer.rope())) {
            (true, Some(i)) => Indent { width: if i.tabs { settings.tab_size } else { i.width }, ..i },
            _ => Indent { tabs: !settings.insert_spaces, width: settings.tab_size },
        };
        self.encoding = d.encoding;
        self.line_ending = d.line_ending;
        self.lossy = d.lossy;
        self.mixed_endings = d.mixed_endings;
        self.stamp = loaded.stamp;
        self.readonly = loaded.readonly;
        self.raw = Some(loaded.bytes);
        self.external = None;
        self.highlight = Highlight::new(self.highlight.syntax());
        self.view.metrics = Default::default();
        self.find = None;
    }

    pub fn detect_language(&mut self) {
        let first = self.buffer.line_text(0);
        let s = syntax::detect(self.path.as_deref(), &first);
        if s.name != self.highlight.name() {
            self.highlight = Highlight::new(s);
        }
        self.preview = false;
    }

    pub fn set_language(&mut self, name: &str) {
        self.highlight = Highlight::new(syntax::by_name(name));
        self.language_pinned = true;
    }

    pub fn is_markdown(&self) -> bool {
        matches!(self.highlight.name(), "Markdown" | "MultiMarkdown")
    }

    pub fn title(&self) -> String {
        match &self.path {
            Some(p) => p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string()),
            None => format!("Untitled {}", self.untitled),
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.buffer.is_dirty() || (self.path.is_none() && self.buffer.len_chars() > 0)
    }

    /// Bring highlighting and layout up to date after edits made outside
    /// the widget (find/replace, reload).
    pub fn sync(&mut self) {
        if let Some(c) = self.buffer.take_changed() {
            self.highlight.invalidate(c, self.buffer.len_lines());
            self.view.metrics.apply(c, &self.buffer);
            self.last_edit = Some(Instant::now());
        }
    }

    /// The file's bytes as they'd be saved.
    pub fn encode(&mut self, settings: &EditorSettings) -> Result<Vec<u8>, String> {
        if settings.trim_trailing_whitespace {
            self.buffer.trim_trailing_whitespace();
        }
        if settings.final_newline && self.buffer.len_chars() > 0 && !self.buffer.text().ends_with('\n') {
            let n = self.buffer.len_chars();
            self.buffer.transact(vec![(n, n, "\n".into())], crate::buffer::After::Map, crate::buffer::EditKind::Other);
        }
        self.sync();
        encoding::encode(&self.buffer.text(), self.encoding, self.line_ending)
    }

    /// After a successful save.
    pub fn saved(&mut self, path: &Path, stamp: Stamp) {
        let renamed = self.path.as_deref() != Some(path);
        self.path = Some(path.to_path_buf());
        self.stamp = Some(stamp);
        self.external = None;
        self.buffer.mark_saved();
        self.readonly = !files::writable(path);
        if renamed && !self.language_pinned {
            self.detect_language();
        }
        self.drop_swap();
    }

    /// Reload from disk, as an undoable change.
    pub fn reload(&mut self, loaded: Loaded) {
        let d = &loaded.decoded;
        self.buffer.replace_all_text(&d.text);
        self.buffer.mark_saved();
        self.encoding = d.encoding;
        self.line_ending = d.line_ending;
        self.lossy = d.lossy;
        self.mixed_endings = d.mixed_endings;
        self.stamp = loaded.stamp;
        self.readonly = loaded.readonly;
        self.raw = Some(loaded.bytes);
        self.external = None;
        self.sync();
        self.drop_swap();
    }

    /// Decode the last read bytes in another encoding.
    pub fn reinterpret(&mut self, enc: &'static encoding_rs::Encoding) -> bool {
        let Some(raw) = &self.raw else { return false };
        let d = encoding::decode_as(raw, enc);
        self.buffer.replace_all_text(&d.text);
        self.buffer.mark_saved();
        self.encoding = d.encoding;
        self.line_ending = d.line_ending;
        self.lossy = d.lossy;
        self.sync();
        true
    }

    /// Check the file on disk. Returns true when something changed.
    pub fn check_disk(&mut self) -> bool {
        let Some(path) = &self.path else { return false };
        let now = files::stamp(path);
        let ext = match (self.stamp, now) {
            (Some(_), None) if !path.exists() => Some(External::Deleted),
            (Some(old), Some(new)) if old != new => Some(External::Changed),
            (None, Some(_)) if self.external == Some(External::Deleted) => Some(External::Changed),
            _ => None,
        };
        if ext.is_some() && ext != self.external {
            self.external = ext;
            if ext == Some(External::Changed) {
                self.stamp = now;
            }
            return true;
        }
        false
    }

    pub fn swap_data(&self) -> SwapData {
        SwapData {
            path: self.path.clone(),
            title: self.title(),
            pid: std::process::id(),
            saved: swap::now_secs(),
            text: self.buffer.text(),
        }
    }

    pub fn drop_swap(&mut self) {
        if let Some(f) = self.swap_file.take() {
            swap::remove(&f);
        }
        self.swapped_version = self.buffer.version();
    }

    /// Find matches for `query`, cached per text version.
    pub fn find_matches(&mut self, query: &Query) -> &FindCache {
        let key = (self.buffer.version(), query.clone());
        if self.find.as_ref().map(|f| &f.key) != Some(&key) {
            let mut cache = FindCache { key, matches: Vec::new(), error: None, truncated: false };
            match query.compile() {
                Ok(Some(re)) => {
                    let text = self.buffer.text();
                    let rope = self.buffer.rope();
                    let found = search::find_all(&re, &text, search::MAX_MATCHES);
                    cache.truncated = found.len() >= search::MAX_MATCHES;
                    cache.matches = found.into_iter().map(|r| (rope.byte_to_char(r.start), rope.byte_to_char(r.end))).collect();
                }
                Ok(None) => {}
                Err(e) => cache.error = Some(e),
            }
            self.find = Some(cache);
        }
        self.find.as_ref().expect("just set")
    }

    /// Replace the given match with `with` (regex groups expanded).
    pub fn replace_match(&mut self, query: &Query, index: usize, with: &str) -> bool {
        let Ok(Some(re)) = query.compile() else { return false };
        let Some(&(a, b)) = self.find_matches(query).matches.get(index) else { return false };
        let text = self.buffer.text();
        let rope = self.buffer.rope();
        let range = rope.char_to_byte(a)..rope.char_to_byte(b);
        let new = search::replacement_for(&re, &text, range, with, query.regex);
        let len = new.chars().count();
        self.buffer.transact(
            vec![(a, b, new)],
            crate::buffer::After::Exactly(vec![crate::buffer::Sel::caret(a + len)]),
            crate::buffer::EditKind::Other,
        );
        self.sync();
        true
    }

    /// Replace every match; returns how many.
    pub fn replace_all(&mut self, query: &Query, with: &str) -> usize {
        let Ok(Some(re)) = query.compile() else { return 0 };
        let matches = self.find_matches(query).matches.clone();
        if matches.is_empty() {
            return 0;
        }
        let text = self.buffer.text();
        let rope = self.buffer.rope().clone();
        let n = self.buffer.replace_ranges(&matches, |i| {
            let (a, b) = matches[i];
            search::replacement_for(&re, &text, rope.char_to_byte(a)..rope.char_to_byte(b), with, query.regex)
        });
        self.sync();
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(text: &str) -> Doc {
        let mut d = Doc::untitled(1, 1, &EditorSettings::default());
        d.buffer = Buffer::new(text);
        d
    }

    #[test]
    fn find_and_replace() {
        let mut d = doc("cat hat cat");
        let q = Query { text: "cat".into(), ..Default::default() };
        assert_eq!(d.find_matches(&q).matches, vec![(0, 3), (8, 11)]);
        assert!(d.replace_match(&q, 1, "dog"));
        assert_eq!(d.buffer.text(), "cat hat dog");
        assert_eq!(d.find_matches(&q).matches.len(), 1, "cache follows the text");
        let re = Query { text: r"(\w)at".into(), regex: true, ..Default::default() };
        assert_eq!(d.replace_all(&re, "${1}ot"), 2);
        assert_eq!(d.buffer.text(), "cot hot dog");
        d.buffer.undo();
        assert_eq!(d.buffer.text(), "cat hat dog", "replace all is one undo step");
        let bad = Query { text: "(".into(), regex: true, ..Default::default() };
        assert!(d.find_matches(&bad).error.is_some());
    }

    #[test]
    fn matches_are_char_positions() {
        let mut d = doc("héllo wörld wörld");
        let q = Query { text: "wörld".into(), ..Default::default() };
        assert_eq!(d.find_matches(&q).matches, vec![(6, 11), (12, 17)]);
    }

    #[test]
    fn open_save_reload_cycle() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("notes.md");
        std::fs::write(&p, "a\r\nb\r\n").unwrap();
        let settings = EditorSettings::default();
        let mut d = Doc::from_file(1, &p, files::load(&p).unwrap(), &settings);
        assert_eq!(d.highlight.name(), "Markdown");
        assert_eq!(d.line_ending, LineEnding::CrLf);
        d.buffer.set_caret(0);
        d.buffer.insert("x");
        let bytes = d.encode(&settings).unwrap();
        assert_eq!(bytes, b"xa\r\nb\r\n");
        let st = files::save(&p, &bytes).unwrap();
        d.saved(&p, st);
        assert!(!d.is_dirty());
        assert!(!d.check_disk());

        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&p, "changed elsewhere\n").unwrap();
        assert!(d.check_disk());
        assert_eq!(d.external, Some(External::Changed));
        d.reload(files::load(&p).unwrap());
        assert_eq!(d.buffer.text(), "changed elsewhere\n");
        assert_eq!(d.line_ending, LineEnding::Lf);
        assert!(!d.is_dirty());
        d.buffer.undo();
        assert_eq!(d.buffer.text(), "xa\nb\n", "reload can be undone");

        std::fs::remove_file(&p).unwrap();
        assert!(d.check_disk());
        assert_eq!(d.external, Some(External::Deleted));
    }

    #[test]
    fn untitled_with_text_is_dirty() {
        let mut d = Doc::untitled(1, 3, &EditorSettings::default());
        assert!(!d.is_dirty());
        assert_eq!(d.title(), "Untitled 3");
        d.buffer.insert("x");
        assert!(d.is_dirty());
    }
}
