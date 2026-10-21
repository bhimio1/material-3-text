//! The text model: a rope, one or more selections, and undo history.
//!
//! Positions are char indices into the rope. Line breaks are always `\n`
//! here; the file's own line ending is restored when saving. Every change
//! goes through [`Buffer::transact`], which records it for undo and notes
//! the first changed line so the highlighter and layout can catch up.

use crate::layout::{self, Wrap};
use ropey::Rope;
use std::time::{Duration, Instant};

/// A selection. `anchor == head` is a plain caret.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sel {
    pub anchor: usize,
    pub head: usize,
    /// The display column up/down movement tries to keep.
    pub goal: Option<usize>,
}

impl Sel {
    pub fn caret(pos: usize) -> Self {
        Self { anchor: pos, head: pos, goal: None }
    }

    pub fn new(anchor: usize, head: usize) -> Self {
        Self { anchor, head, goal: None }
    }

    pub fn start(&self) -> usize {
        self.anchor.min(self.head)
    }

    pub fn end(&self) -> usize {
        self.anchor.max(self.head)
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
}

/// The lines an edit touched: `first..=last` in the current text, which
/// had `old_lines` lines before.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Changed {
    pub first: usize,
    pub last: usize,
    pub old_lines: usize,
}

/// How undo groups a change with the one before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditKind {
    Typing,
    Delete,
    Other,
}

#[derive(Clone, Debug)]
struct Edit {
    at: usize,
    removed: String,
    inserted: String,
}

#[derive(Clone, Debug)]
struct Transaction {
    id: u64,
    /// Applied in this order; undone in reverse.
    edits: Vec<Edit>,
    before: Vec<Sel>,
    after: Vec<Sel>,
    kind: EditKind,
    time: Instant,
}

/// What the selections become after a change.
pub enum After {
    /// A caret at the end of each change's inserted text.
    EndOfInsert,
    /// The current selections, shifted through the change.
    Map,
    /// These selections (in post-change positions).
    Exactly(Vec<Sel>),
}

/// Indentation of a document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Indent {
    pub tabs: bool,
    pub width: usize,
}

impl Default for Indent {
    fn default() -> Self {
        Self { tabs: false, width: 4 }
    }
}

impl Indent {
    pub fn unit(&self) -> String {
        if self.tabs {
            "\t".into()
        } else {
            " ".repeat(self.width.max(1))
        }
    }

    /// Guess from the content: the most common indentation step among the
    /// first few thousand lines. `None` when nothing is indented.
    pub fn detect(rope: &Rope) -> Option<Indent> {
        let (mut tabs, mut spaces) = (0usize, 0usize);
        let mut steps = [0usize; 9];
        let mut prev = 0usize;
        for line in rope.lines().take(5000) {
            let n = match line.chars().next() {
                Some('\t') => {
                    tabs += 1;
                    continue;
                }
                Some(' ') => line.chars().take_while(|c| *c == ' ').count(),
                Some('\n') | None => continue,
                Some(_) => {
                    prev = 0;
                    continue;
                }
            };
            // Whitespace-only lines say nothing.
            if line.chars().nth(n).is_none_or(|c| c == '\n') {
                continue;
            }
            spaces += 1;
            let step = n.abs_diff(prev);
            if (2..=8).contains(&step) {
                steps[step] += 1;
            }
            prev = n;
        }
        if tabs == 0 && spaces == 0 {
            return None;
        }
        if tabs > spaces {
            return Some(Indent { tabs: true, width: 4 });
        }
        let width = (2..=8).max_by_key(|&w| (steps[w], usize::from(w == 4))).unwrap_or(4);
        Some(Indent { tabs: false, width: if steps[width] == 0 { 4 } else { width } })
    }
}

/// Line comment and block comment markers of a language.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CommentStyle {
    pub line: Option<&'static str>,
    pub block: Option<(&'static str, &'static str)>,
}

/// Cursor movements.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    WordLeft,
    WordRight,
    Up,
    Down,
    /// Rows to move.
    PageUp(usize),
    PageDown(usize),
    Home,
    End,
    DocStart,
    DocEnd,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Space,
    Word,
    Punct,
    Newline,
}

fn class(c: char) -> CharClass {
    if c == '\n' {
        CharClass::Newline
    } else if c.is_whitespace() {
        CharClass::Space
    } else if c.is_alphanumeric() || c == '_' {
        CharClass::Word
    } else {
        CharClass::Punct
    }
}

const GROUP_GAP: Duration = Duration::from_millis(1200);
const BRACKETS: [(char, char); 3] = [('(', ')'), ('[', ']'), ('{', '}')];
/// How far bracket matching looks, in chars.
const BRACKET_SCAN: usize = 200_000;

pub struct Buffer {
    rope: Rope,
    pub sels: Vec<Sel>,
    /// Index into `sels` of the selection the view follows.
    pub primary: usize,
    pub indent: Indent,
    undo: Vec<Transaction>,
    redo: Vec<Transaction>,
    next_id: u64,
    saved_id: u64,
    /// Don't merge the next change into the last transaction.
    sealed: bool,
    version: u64,
    /// Lines changed since the last [`Buffer::take_changed`].
    changed: Option<Changed>,
    /// Text last copied as whole lines (copy with nothing selected), so
    /// pasting it inserts above the current line.
    line_clip: Option<String>,
}

impl Buffer {
    pub fn new(text: &str) -> Self {
        let rope = Rope::from_str(text);
        let indent = Indent::detect(&rope).unwrap_or_default();
        Self {
            rope,
            sels: vec![Sel::caret(0)],
            primary: 0,
            indent,
            undo: Vec::new(),
            redo: Vec::new(),
            next_id: 1,
            saved_id: 0,
            sealed: true,
            version: 0,
            changed: None,
            line_clip: None,
        }
    }

    pub fn rope(&self) -> &Rope {
        &self.rope
    }

    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    pub fn len_lines(&self) -> usize {
        self.rope.len_lines()
    }

    /// Bumped on every change.
    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn line_start(&self, line: usize) -> usize {
        self.rope.line_to_char(line.min(self.rope.len_lines()))
    }

    /// Chars in `line`, without its line break.
    pub fn line_len(&self, line: usize) -> usize {
        let l = self.rope.line(line);
        let n = l.len_chars();
        if n > 0 && l.char(n - 1) == '\n' {
            n - 1
        } else {
            n
        }
    }

    pub fn line_end(&self, line: usize) -> usize {
        self.line_start(line) + self.line_len(line)
    }

    /// The line's text without its line break.
    pub fn line_text(&self, line: usize) -> String {
        let l = self.rope.line(line);
        l.slice(..self.line_len(line)).to_string()
    }

    pub fn char_to_line(&self, pos: usize) -> usize {
        self.rope.char_to_line(pos.min(self.rope.len_chars()))
    }

    /// (line, char column) of a position.
    pub fn line_col(&self, pos: usize) -> (usize, usize) {
        let line = self.char_to_line(pos);
        (line, pos.min(self.len_chars()) - self.line_start(line))
    }

    pub fn char_at(&self, pos: usize) -> Option<char> {
        (pos < self.rope.len_chars()).then(|| self.rope.char(pos))
    }

    pub fn slice(&self, start: usize, end: usize) -> String {
        let n = self.len_chars();
        self.rope.slice(start.min(n)..end.min(n)).to_string()
    }

    pub fn primary(&self) -> Sel {
        self.sels[self.primary.min(self.sels.len() - 1)]
    }

    /// Takes the lines changed since the last call.
    pub fn take_changed(&mut self) -> Option<Changed> {
        self.changed.take()
    }

    pub fn is_dirty(&self) -> bool {
        self.undo.last().map_or(0, |t| t.id) != self.saved_id
    }

    pub fn mark_saved(&mut self) {
        self.saved_id = self.undo.last().map_or(0, |t| t.id);
        self.sealed = true;
    }

    /// Mark modified without an edit (e.g. a different encoding to save in).
    pub fn mark_modified(&mut self) {
        self.saved_id = u64::MAX;
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// Start a new undo group with the next change.
    pub fn seal(&mut self) {
        self.sealed = true;
    }

    // ------------------------------------------------------------ changes

    /// Record an edit about to replace `removed` at `at` with `inserted`.
    fn note_changed(&mut self, at: usize, removed: &str, inserted: &str) {
        let line = self.char_to_line(at);
        let gone = removed.matches('\n').count() as isize;
        let added = inserted.matches('\n').count();
        let c = self.changed.get_or_insert(Changed { first: line, last: line, old_lines: self.rope.len_lines() });
        // Lines after the edit move by the difference.
        if c.last > line {
            c.last = (c.last as isize + added as isize - gone).max(line as isize) as usize;
        }
        c.first = c.first.min(line);
        c.last = c.last.max(line + added);
        self.version += 1;
    }

    fn apply_edit(&mut self, at: usize, remove: usize, insert: &str) -> Edit {
        let removed = self.rope.slice(at..at + remove).to_string();
        self.note_changed(at, &removed, insert);
        self.rope.remove(at..at + remove);
        self.rope.insert(at, insert);
        Edit { at, removed, inserted: insert.to_string() }
    }

    /// Replace char ranges with text. `changes` are sorted by start and
    /// don't overlap. Returns false when there was nothing to change.
    pub fn transact(&mut self, mut changes: Vec<(usize, usize, String)>, after: After, kind: EditKind) -> bool {
        let n = self.len_chars();
        for c in &mut changes {
            c.0 = c.0.min(n);
            c.1 = c.1.clamp(c.0, n);
        }
        changes.retain(|(s, e, t)| s != e || !t.is_empty());
        if changes.is_empty() {
            return false;
        }
        changes.sort_by_key(|c| c.0);
        // Merge overlaps from multiple cursors on the same spot.
        let mut merged: Vec<(usize, usize, String)> = Vec::with_capacity(changes.len());
        for c in changes {
            match merged.last_mut() {
                Some(last) if c.0 < last.1 => {
                    last.1 = last.1.max(c.1);
                }
                _ => merged.push(c),
            }
        }
        let changes = merged;

        // New positions of each change, for selection mapping.
        let mut spans = Vec::with_capacity(changes.len());
        let mut shift: isize = 0;
        for (s, e, t) in &changes {
            let ns = (*s as isize + shift) as usize;
            let ins = t.chars().count();
            spans.push((*s, *e, ns, ins));
            shift += ins as isize - (*e - *s) as isize;
        }

        let before = self.sels.clone();
        let mut edits = Vec::with_capacity(changes.len());
        for (s, e, t) in changes.iter().rev() {
            edits.push(self.apply_edit(*s, e - s, t));
        }

        let new_sels = match after {
            After::EndOfInsert => spans.iter().map(|&(_, _, ns, ins)| Sel::caret(ns + ins)).collect(),
            After::Map => before
                .iter()
                .map(|s| Sel::new(map_pos(s.anchor, &spans), map_pos(s.head, &spans)))
                .collect(),
            After::Exactly(v) => v,
        };
        self.set_sels(new_sels);
        self.redo.clear();

        let now = Instant::now();
        let mergeable = !self.sealed
            && kind != EditKind::Other
            && self.undo.last().is_some_and(|t| {
                t.kind == kind && t.id != self.saved_id && now.duration_since(t.time) < GROUP_GAP && t.after == before
            });
        if mergeable {
            let t = self.undo.last_mut().expect("checked");
            t.edits.extend(edits);
            t.after = self.sels.clone();
            t.time = now;
        } else {
            let id = self.next_id;
            self.next_id += 1;
            self.undo.push(Transaction { id, edits, before, after: self.sels.clone(), kind, time: now });
        }
        self.sealed = kind == EditKind::Other;
        true
    }

    /// Replace the whole text, as one undoable change.
    pub fn replace_all_text(&mut self, text: &str) {
        let caret = self.primary().head;
        let n = self.len_chars();
        self.transact(vec![(0, n, text.to_string())], After::Exactly(vec![Sel::caret(0)]), EditKind::Other);
        let caret = caret.min(self.len_chars());
        self.set_sels(vec![Sel::caret(caret)]);
    }

    pub fn undo(&mut self) -> bool {
        let Some(t) = self.undo.pop() else { return false };
        for e in t.edits.iter().rev() {
            self.note_changed(e.at, &e.inserted, &e.removed);
            self.rope.remove(e.at..e.at + e.inserted.chars().count());
            self.rope.insert(e.at, &e.removed);
        }
        self.set_sels(t.before.clone());
        self.redo.push(t);
        self.sealed = true;
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(t) = self.redo.pop() else { return false };
        for e in &t.edits {
            self.note_changed(e.at, &e.removed, &e.inserted);
            self.rope.remove(e.at..e.at + e.removed.chars().count());
            self.rope.insert(e.at, &e.inserted);
        }
        self.set_sels(t.after.clone());
        self.undo.push(t);
        self.sealed = true;
        true
    }

    // --------------------------------------------------------- selections

    /// Replace the selections, sorting and merging overlaps. The last one
    /// given becomes primary.
    pub fn set_sels(&mut self, sels: Vec<Sel>) {
        let n = self.len_chars();
        let primary_head = sels.last().map(|s| s.head.min(n));
        let mut sels: Vec<Sel> = sels
            .into_iter()
            .map(|s| Sel { anchor: s.anchor.min(n), head: s.head.min(n), goal: s.goal })
            .collect();
        if sels.is_empty() {
            sels.push(Sel::caret(0));
        }
        sels.sort_by_key(|s| s.start());
        let mut out: Vec<Sel> = Vec::with_capacity(sels.len());
        for s in sels {
            match out.last_mut() {
                Some(last) if s.start() < last.end() || (s.is_empty() && last.is_empty() && s.head == last.head) => {
                    let (start, end) = (last.start(), last.end().max(s.end()));
                    let forward = last.head >= last.anchor;
                    *last = if forward { Sel::new(start, end) } else { Sel::new(end, start) };
                }
                _ => out.push(s),
            }
        }
        self.primary = primary_head
            .and_then(|h| out.iter().position(|s| s.start() <= h && h <= s.end()))
            .unwrap_or(out.len() - 1);
        self.sels = out;
    }

    pub fn set_caret(&mut self, pos: usize) {
        self.sealed = true;
        self.set_sels(vec![Sel::caret(pos)]);
    }

    /// Keep only the primary selection, collapsed if `collapse`.
    pub fn single(&mut self, collapse: bool) {
        let mut p = self.primary();
        if collapse {
            p = Sel::caret(p.head);
        }
        self.set_sels(vec![p]);
    }

    pub fn select_all(&mut self) {
        self.set_sels(vec![Sel::new(0, self.len_chars())]);
    }

    pub fn has_selection(&self) -> bool {
        self.sels.iter().any(|s| !s.is_empty())
    }

    pub fn selected_text(&self) -> String {
        self.sels
            .iter()
            .filter(|s| !s.is_empty())
            .map(|s| self.slice(s.start(), s.end()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Lines covered by a selection. A selection ending at the very start
    /// of a line doesn't include that line.
    fn sel_lines(&self, s: &Sel) -> (usize, usize) {
        let first = self.char_to_line(s.start());
        let mut last = self.char_to_line(s.end());
        if last > first && s.end() == self.line_start(last) {
            last -= 1;
        }
        (first, last)
    }

    /// Distinct line blocks covered by the selections, merged.
    fn line_blocks(&self) -> Vec<(usize, usize)> {
        let mut blocks: Vec<(usize, usize)> = Vec::new();
        for s in &self.sels {
            let (a, b) = self.sel_lines(s);
            match blocks.last_mut() {
                Some(last) if a <= last.1 + 1 => last.1 = last.1.max(b),
                _ => blocks.push((a, b)),
            }
        }
        blocks
    }

    /// Select whole lines; repeated, extends by one line.
    pub fn select_line(&mut self) {
        let sels = self
            .sels
            .iter()
            .map(|s| {
                let (a, b) = self.sel_lines(s);
                let whole = s.start() == self.line_start(a) && s.end() == self.line_start(b + 1) && !s.is_empty();
                let b = if whole { b + 1 } else { b };
                Sel::new(self.line_start(a), self.line_start(b + 1))
            })
            .collect();
        self.set_sels(sels);
    }

    /// Move every caret; `extend` keeps the anchors (Shift).
    pub fn move_cursors(&mut self, m: Motion, extend: bool, wrap: Wrap) {
        self.sealed = true;
        let sels = self
            .sels
            .iter()
            .map(|s| {
                // Collapsing a selection with Left/Right goes to its edge.
                if !extend && !s.is_empty() && matches!(m, Motion::Left | Motion::Right) {
                    let pos = if m == Motion::Left { s.start() } else { s.end() };
                    return Sel::caret(pos);
                }
                let (head, goal) = self.motion(s.head, s.goal, m, wrap);
                Sel { anchor: if extend { s.anchor } else { head }, head, goal }
            })
            .collect();
        self.set_sels(sels);
    }

    fn motion(&self, pos: usize, goal: Option<usize>, m: Motion, wrap: Wrap) -> (usize, Option<usize>) {
        let n = self.len_chars();
        match m {
            Motion::Left => (pos.saturating_sub(1), None),
            Motion::Right => ((pos + 1).min(n), None),
            Motion::WordLeft => (self.word_left(pos), None),
            Motion::WordRight => (self.word_right(pos), None),
            Motion::Up => self.vertical(pos, goal, -1, wrap),
            Motion::Down => self.vertical(pos, goal, 1, wrap),
            Motion::PageUp(rows) => self.vertical(pos, goal, -(rows.max(1) as isize), wrap),
            Motion::PageDown(rows) => self.vertical(pos, goal, rows.max(1) as isize, wrap),
            Motion::Home => {
                let (line, col) = self.line_col(pos);
                let text = self.line_text(line);
                let first = text.chars().take_while(|c| c.is_whitespace()).count();
                let start = self.line_start(line);
                (if col == first { start } else { start + first }, None)
            }
            Motion::End => (self.line_end(self.char_to_line(pos)), None),
            Motion::DocStart => (0, None),
            Motion::DocEnd => (n, None),
        }
    }

    fn word_left(&self, pos: usize) -> usize {
        let mut p = pos;
        while p > 0 && class(self.rope.char(p - 1)) == CharClass::Space {
            p -= 1;
        }
        if p == 0 {
            return 0;
        }
        let c = class(self.rope.char(p - 1));
        if c == CharClass::Newline {
            return p - 1;
        }
        while p > 0 && class(self.rope.char(p - 1)) == c {
            p -= 1;
        }
        p
    }

    fn word_right(&self, pos: usize) -> usize {
        let n = self.len_chars();
        let mut p = pos;
        while p < n && class(self.rope.char(p)) == CharClass::Space {
            p += 1;
        }
        if p >= n {
            return n;
        }
        let c = class(self.rope.char(p));
        if c == CharClass::Newline {
            return p + 1;
        }
        while p < n && class(self.rope.char(p)) == c {
            p += 1;
        }
        p
    }

    /// Up/down by `delta` visual rows, keeping the goal column.
    fn vertical(&self, pos: usize, goal: Option<usize>, delta: isize, wrap: Wrap) -> (usize, Option<usize>) {
        let (line, col) = self.line_col(pos);
        let text = self.line_text(line);
        let dcol = layout::display_col(&text, col, wrap.tab);
        let (mut row, in_row) = wrap.row_of(&text, dcol);
        let goal = goal.unwrap_or(in_row);
        let mut line = line as isize;
        let mut rows_here = wrap.rows(&text) as isize;
        let mut steps = delta;
        let mut row_i = row as isize;
        while steps != 0 {
            if steps < 0 {
                if row_i > 0 {
                    row_i -= 1;
                } else if line > 0 {
                    line -= 1;
                    rows_here = wrap.rows(&self.line_text(line as usize)) as isize;
                    row_i = rows_here - 1;
                } else {
                    return (0, Some(goal));
                }
                steps += 1;
            } else {
                if row_i + 1 < rows_here {
                    row_i += 1;
                } else if (line as usize) + 1 < self.len_lines() {
                    line += 1;
                    rows_here = wrap.rows(&self.line_text(line as usize)) as isize;
                    row_i = 0;
                } else {
                    return (self.len_chars(), Some(goal));
                }
                steps -= 1;
            }
        }
        row = row_i as usize;
        let line = line as usize;
        let text = self.line_text(line);
        let target = wrap.col_at(&text, row, goal);
        let c = layout::char_at_col(&text, target, wrap.tab);
        (self.line_start(line) + c, Some(goal))
    }

    /// Add a caret on the line above or below the outermost one.
    pub fn add_cursor(&mut self, down: bool, wrap: Wrap) {
        let edge = if down { *self.sels.last().expect("never empty") } else { self.sels[0] };
        let (head, goal) = self.vertical(edge.head, edge.goal, if down { 1 } else { -1 }, wrap);
        if self.char_to_line(head) == self.char_to_line(edge.head) {
            return;
        }
        let mut sels = self.sels.clone();
        sels.push(Sel { anchor: head, head, goal });
        self.set_sels(sels);
    }

    /// The word around a position.
    pub fn word_at(&self, pos: usize) -> (usize, usize) {
        let n = self.len_chars();
        let is_word = |p: usize| p < n && class(self.rope.char(p)) == CharClass::Word;
        let (mut a, mut b) = (pos, pos);
        while a > 0 && is_word(a - 1) {
            a -= 1;
        }
        while is_word(b) {
            b += 1;
        }
        (a, b)
    }

    /// Ctrl+D: select the word under the caret, then add the next match.
    pub fn select_next_occurrence(&mut self) {
        let p = *self.sels.last().expect("never empty");
        if p.is_empty() {
            let (a, b) = self.word_at(p.head);
            if a < b {
                let mut sels = self.sels.clone();
                sels.pop();
                sels.push(Sel::new(a, b));
                self.set_sels(sels);
            }
            return;
        }
        let needle = self.slice(p.start(), p.end());
        let text = self.text();
        let from = self.rope.char_to_byte(p.end());
        let found = text[from..]
            .find(&needle)
            .map(|i| from + i)
            .or_else(|| text[..from].find(&needle));
        if let Some(b) = found {
            let a = self.rope.byte_to_char(b);
            let e = a + needle.chars().count();
            if !self.sels.iter().any(|s| s.start() == a) {
                let mut sels = self.sels.clone();
                sels.push(Sel::new(a, e));
                self.set_sels(sels);
            }
        }
    }

    // ------------------------------------------------------------ editing

    /// Type or insert text over every selection.
    pub fn insert(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        let kind = if s.chars().count() == 1 && s != "\n" { EditKind::Typing } else { EditKind::Other };
        // A closing bracket typed on a blank line outdents it.
        let electric = matches!(s, "}" | "]" | ")");
        let changes = self
            .sels
            .iter()
            .map(|sel| {
                if electric && sel.is_empty() {
                    let (line, col) = self.line_col(sel.head);
                    let before = self.slice(self.line_start(line), sel.head);
                    if col > 0 && before.chars().all(|c| c == ' ' || c == '\t') {
                        let keep = outdented(&before, self.indent);
                        return (self.line_start(line), sel.head, keep + s);
                    }
                }
                (sel.start(), sel.end(), s.to_string())
            })
            .collect();
        self.transact(changes, After::EndOfInsert, kind);
    }

    /// Paste: one line per caret when the counts match; whole lines copied
    /// with nothing selected go above the current line.
    pub fn paste(&mut self, s: &str) {
        let s = s.replace("\r\n", "\n").replace('\r', "\n");
        let lines: Vec<&str> = s.split('\n').collect();
        if self.sels.len() > 1 && lines.len() == self.sels.len() {
            let changes = self.sels.iter().zip(lines).map(|(sel, t)| (sel.start(), sel.end(), t.to_string())).collect();
            self.transact(changes, After::EndOfInsert, EditKind::Other);
            return;
        }
        if self.line_clip.as_deref() == Some(s.as_str()) && !self.has_selection() {
            let changes = self
                .sels
                .iter()
                .map(|sel| {
                    let ls = self.line_start(self.char_to_line(sel.head));
                    (ls, ls, s.clone())
                })
                .collect();
            self.transact(changes, After::Map, EditKind::Other);
            return;
        }
        self.insert(&s);
    }

    /// Copy: the selections, or whole lines when nothing is selected.
    pub fn copy(&mut self) -> String {
        if self.has_selection() {
            self.line_clip = None;
            return self.selected_text();
        }
        let mut out = String::new();
        for (a, b) in self.line_blocks() {
            out.push_str(&self.slice(self.line_start(a), self.line_start(b + 1)));
            if !out.ends_with('\n') {
                out.push('\n');
            }
        }
        self.line_clip = Some(out.clone());
        out
    }

    pub fn cut(&mut self) -> String {
        let text = self.copy();
        if self.has_selection() {
            let changes = self.sels.iter().map(|s| (s.start(), s.end(), String::new())).collect();
            self.transact(changes, After::EndOfInsert, EditKind::Other);
        } else {
            self.delete_lines();
        }
        text
    }

    /// Enter: keep the line's indentation, add a level after an opening
    /// bracket (or `:` when `colon_indents`), and split `{|}` over lines.
    pub fn newline(&mut self, colon_indents: bool) {
        let unit = self.indent.unit();
        let mut sels = Vec::new();
        let mut changes = Vec::new();
        let mut shift: isize = 0;
        for sel in &self.sels {
            let line = self.char_to_line(sel.start());
            let ls = self.line_start(line);
            let lead: String = self.slice(ls, sel.start()).chars().take_while(|c| *c == ' ' || *c == '\t').collect();
            let before = self.slice(ls, sel.start());
            let last = before.trim_end().chars().last();
            let next = self.char_at(sel.end());
            let opens = matches!(last, Some('{' | '[' | '('))
                || (colon_indents && last == Some(':'));
            let mut text = format!("\n{lead}");
            if opens {
                text.push_str(&unit);
            }
            let caret = text.chars().count();
            let closes = matches!((last, next), (Some('{'), Some('}')) | (Some('['), Some(']')) | (Some('('), Some(')')));
            if closes {
                text.push('\n');
                text.push_str(&lead);
            }
            // Drop trailing spaces left before the break.
            let trimmed_start = ls + before.trim_end().chars().count().max(lead.chars().count().min(before.chars().count()));
            let start = if before.trim().is_empty() { sel.start() } else { trimmed_start.min(sel.start()) };
            let new_start = (start as isize + shift) as usize;
            sels.push(Sel::caret(new_start + caret));
            shift += text.chars().count() as isize - (sel.end() - start) as isize;
            changes.push((start, sel.end(), text));
        }
        self.transact(changes, After::Exactly(sels), EditKind::Other);
    }

    /// Ctrl+Enter: start a new line below the caret's line (above with
    /// `above`) without splitting it.
    pub fn open_line(&mut self, above: bool, colon_indents: bool) {
        self.single(true);
        let line = self.char_to_line(self.primary().head);
        if above {
            let ls = self.line_start(line);
            let lead: String = self.line_text(line).chars().take_while(|c| *c == ' ' || *c == '\t').collect();
            let caret = ls + lead.chars().count();
            self.transact(vec![(ls, ls, format!("{lead}\n"))], After::Exactly(vec![Sel::caret(caret)]), EditKind::Other);
        } else {
            self.set_caret(self.line_end(line));
            self.newline(colon_indents);
        }
    }

    /// Backspace. In leading spaces it removes back to the previous tab
    /// stop; with `word`, to the start of the word.
    pub fn backspace(&mut self, word: bool) {
        let changes = self
            .sels
            .iter()
            .map(|sel| {
                if !sel.is_empty() {
                    return (sel.start(), sel.end(), String::new());
                }
                let p = sel.head;
                if word {
                    return (self.word_left(p), p, String::new());
                }
                let (line, col) = self.line_col(p);
                let ls = self.line_start(line);
                let before = self.slice(ls, p);
                if !self.indent.tabs && col > 0 && before.chars().all(|c| c == ' ') {
                    let w = self.indent.width.max(1);
                    let back = match col % w {
                        0 => w,
                        r => r,
                    };
                    return (p - back, p, String::new());
                }
                (p.saturating_sub(1), p, String::new())
            })
            .collect();
        self.transact(changes, After::EndOfInsert, EditKind::Delete);
    }

    pub fn delete(&mut self, word: bool) {
        let n = self.len_chars();
        let changes = self
            .sels
            .iter()
            .map(|sel| {
                if !sel.is_empty() {
                    (sel.start(), sel.end(), String::new())
                } else if word {
                    (sel.head, self.word_right(sel.head), String::new())
                } else {
                    (sel.head, (sel.head + 1).min(n), String::new())
                }
            })
            .collect();
        self.transact(changes, After::EndOfInsert, EditKind::Delete);
    }

    /// Tab: indent selected lines, or insert indentation at the caret.
    pub fn tab(&mut self, wrap_tab: usize) {
        let multi_line = self.sels.iter().any(|s| {
            let (a, b) = self.sel_lines(s);
            a != b
        });
        if multi_line {
            self.indent_lines();
            return;
        }
        let changes = self
            .sels
            .iter()
            .map(|sel| {
                let text = if self.indent.tabs {
                    "\t".to_string()
                } else {
                    let (line, col) = self.line_col(sel.start());
                    let dcol = layout::display_col(&self.line_text(line), col, wrap_tab);
                    let w = self.indent.width.max(1);
                    " ".repeat(w - dcol % w)
                };
                (sel.start(), sel.end(), text)
            })
            .collect();
        self.transact(changes, After::EndOfInsert, EditKind::Typing);
    }

    pub fn indent_lines(&mut self) {
        let unit = self.indent.unit();
        let mut changes = Vec::new();
        for (a, b) in self.line_blocks() {
            for l in a..=b {
                if self.line_len(l) > 0 {
                    let ls = self.line_start(l);
                    changes.push((ls, ls, unit.clone()));
                }
            }
        }
        self.transact(changes, After::Map, EditKind::Other);
    }

    pub fn outdent_lines(&mut self) {
        let mut changes = Vec::new();
        for (a, b) in self.line_blocks() {
            for l in a..=b {
                let ls = self.line_start(l);
                let text = self.line_text(l);
                let n = if text.starts_with('\t') {
                    1
                } else {
                    text.chars().take(self.indent.width.max(1)).take_while(|c| *c == ' ').count()
                };
                if n > 0 {
                    changes.push((ls, ls + n, String::new()));
                }
            }
        }
        self.transact(changes, After::Map, EditKind::Other);
    }

    /// Toggle line comments on the selected lines (block comments for
    /// languages without line comments).
    pub fn toggle_comment(&mut self, style: CommentStyle) {
        if let Some(tok) = style.line {
            self.toggle_line_comment(tok);
        } else if let Some((open, close)) = style.block {
            self.toggle_block_comment(open, close);
        }
    }

    fn toggle_line_comment(&mut self, tok: &str) {
        let mut lines = Vec::new();
        for (a, b) in self.line_blocks() {
            lines.extend(a..=b);
        }
        let texts: Vec<(usize, String)> = lines.iter().map(|&l| (l, self.line_text(l))).collect();
        let nonblank: Vec<&(usize, String)> = texts.iter().filter(|(_, t)| !t.trim().is_empty()).collect();
        if nonblank.is_empty() {
            return;
        }
        let commented = nonblank.iter().all(|(_, t)| t.trim_start().starts_with(tok));
        let tok_len = tok.chars().count();
        let mut changes = Vec::new();
        if commented {
            for (l, t) in &nonblank {
                let lead = t.chars().take_while(|c| c.is_whitespace()).count();
                let rest: String = t.chars().skip(lead + tok_len).collect();
                let n = tok_len + usize::from(rest.starts_with(' '));
                let at = self.line_start(*l) + lead;
                changes.push((at, at + n, String::new()));
            }
        } else {
            let col = nonblank
                .iter()
                .map(|(_, t)| t.chars().take_while(|c| c.is_whitespace()).count())
                .min()
                .unwrap_or(0);
            for (l, _) in &nonblank {
                let at = self.line_start(*l) + col;
                changes.push((at, at, format!("{tok} ")));
            }
        }
        self.transact(changes, After::Map, EditKind::Other);
    }

    fn toggle_block_comment(&mut self, open: &str, close: &str) {
        let changes = self
            .sels
            .iter()
            .map(|s| {
                let (a, b) = if s.is_empty() {
                    let l = self.char_to_line(s.head);
                    let t = self.line_text(l);
                    let lead = t.chars().take_while(|c| c.is_whitespace()).count();
                    let trail = t.chars().rev().take_while(|c| c.is_whitespace()).count();
                    let ls = self.line_start(l);
                    (ls + lead, (ls + t.chars().count() - trail).max(ls + lead))
                } else {
                    (s.start(), s.end())
                };
                let text = self.slice(a, b);
                let new = match text.strip_prefix(open).and_then(|t| t.strip_suffix(close)) {
                    Some(inner) => {
                        let inner = inner.strip_prefix(' ').unwrap_or(inner);
                        inner.strip_suffix(' ').unwrap_or(inner).to_string()
                    }
                    None => format!("{open} {text} {close}"),
                };
                (a, b, new)
            })
            .collect();
        self.transact(changes, After::Map, EditKind::Other);
    }

    /// Move the selected lines up or down by one.
    pub fn move_lines(&mut self, up: bool) {
        let first = self.line_blocks()[0].0;
        let last = self.line_blocks().last().expect("never empty").1;
        if (up && first == 0) || (!up && last + 1 >= self.len_lines()) {
            return;
        }
        let (a, b) = if up { (first - 1, last) } else { (first, last + 1) };
        let start = self.line_start(a);
        let end = self.line_start(b + 1);
        let region = self.slice(start, end);
        let trailing_nl = region.ends_with('\n');
        let mut lines: Vec<&str> = region.strip_suffix('\n').unwrap_or(&region).split('\n').collect();
        let moved_len: usize;
        if up {
            let prev = lines.remove(0);
            moved_len = prev.chars().count() + 1;
            lines.push(prev);
        } else {
            let next = lines.pop().expect("at least two lines");
            moved_len = next.chars().count() + 1;
            lines.insert(0, next);
        }
        let mut new = lines.join("\n");
        if trailing_nl {
            new.push('\n');
        }
        let sels = self
            .sels
            .iter()
            .map(|s| {
                let f = |p: usize| if up { p - moved_len } else { p + moved_len };
                Sel::new(f(s.anchor), f(s.head))
            })
            .collect();
        self.transact(vec![(start, end, new)], After::Exactly(sels), EditKind::Other);
    }

    /// Duplicate the selected lines below themselves.
    pub fn duplicate_lines(&mut self) {
        let mut changes = Vec::new();
        let mut sels = Vec::new();
        let mut shift = 0usize;
        let blocks = self.line_blocks();
        for (a, b) in blocks {
            let start = self.line_start(a);
            let end = self.line_end(b);
            let block = self.slice(start, end);
            let len = block.chars().count() + 1;
            changes.push((end, end, format!("\n{block}")));
            for s in self.sels.iter().filter(|s| s.start() >= start && s.end() <= end) {
                sels.push(Sel::new(s.anchor + shift + len, s.head + shift + len));
            }
            shift += len;
        }
        self.transact(changes, After::Exactly(sels), EditKind::Other);
    }

    pub fn delete_lines(&mut self) {
        let mut changes = Vec::new();
        for (a, b) in self.line_blocks() {
            let mut start = self.line_start(a);
            let end = self.line_start(b + 1);
            // The last line has no break after it: take the one before.
            if end == self.len_chars() && !self.slice(start, end).ends_with('\n') && start > 0 {
                start -= 1;
            }
            changes.push((start, end, String::new()));
        }
        self.transact(changes, After::EndOfInsert, EditKind::Other);
        let sels = self
            .sels
            .iter()
            .map(|s| {
                let l = self.char_to_line(s.head);
                let t = self.line_text(l);
                Sel::caret(self.line_start(l) + t.chars().take_while(|c| c.is_whitespace()).count())
            })
            .collect();
        self.set_sels(sels);
    }

    /// Replace the given char ranges (sorted) with `with`, as one change.
    pub fn replace_ranges(&mut self, ranges: &[(usize, usize)], with: impl Fn(usize) -> String) -> usize {
        let changes: Vec<_> = ranges.iter().enumerate().map(|(i, &(a, b))| (a, b, with(i))).collect();
        let n = changes.len();
        self.transact(changes, After::Map, EditKind::Other);
        n
    }

    /// Trim trailing whitespace on every line.
    pub fn trim_trailing_whitespace(&mut self) {
        let mut changes = Vec::new();
        for l in 0..self.len_lines() {
            let t = self.line_text(l);
            let trimmed = t.trim_end_matches([' ', '\t']).chars().count();
            let len = t.chars().count();
            if trimmed < len {
                let ls = self.line_start(l);
                changes.push((ls + trimmed, ls + len, String::new()));
            }
        }
        if !changes.is_empty() {
            self.transact(changes, After::Map, EditKind::Other);
        }
    }

    // ----------------------------------------------------------- brackets

    /// The bracket at or just before `pos` and its partner.
    pub fn matching_bracket(&self, pos: usize) -> Option<(usize, usize)> {
        let at = |p: usize| self.char_at(p).filter(|c| BRACKETS.iter().any(|(o, e)| o == c || e == c)).map(|_| p);
        let p = at(pos).or_else(|| pos.checked_sub(1).and_then(at))?;
        let c = self.rope.char(p);
        for (open, close) in BRACKETS {
            if c == open {
                let mut depth = 0usize;
                for (i, ch) in self.rope.chars_at(p).enumerate().take(BRACKET_SCAN) {
                    if ch == open {
                        depth += 1;
                    } else if ch == close {
                        depth -= 1;
                        if depth == 0 {
                            return Some((p, p + i));
                        }
                    }
                }
                return None;
            }
            if c == close {
                let mut depth = 0usize;
                let mut i = p + 1;
                let stop = (p + 1).saturating_sub(BRACKET_SCAN);
                while i > stop {
                    i -= 1;
                    let ch = self.rope.char(i);
                    if ch == close {
                        depth += 1;
                    } else if ch == open {
                        depth -= 1;
                        if depth == 0 {
                            return Some((p, i));
                        }
                    }
                }
                return None;
            }
        }
        None
    }
}

/// Where a position ends up after the changes in `spans`
/// (`old_start, old_end, new_start, inserted_len`, sorted).
fn map_pos(pos: usize, spans: &[(usize, usize, usize, usize)]) -> usize {
    let mut shift: isize = 0;
    for &(s, e, ns, ins) in spans {
        if pos < s || (pos == s && s != e) {
            break;
        }
        if pos >= e {
            shift = (ns + ins) as isize - e as isize;
            continue;
        }
        // Inside a replaced range.
        return ns + (pos - s).min(ins);
    }
    (pos as isize + shift) as usize
}

/// Leading whitespace with one indentation level removed.
fn outdented(lead: &str, indent: Indent) -> String {
    if let Some(rest) = lead.strip_suffix('\t') {
        return rest.to_string();
    }
    let spaces = lead.chars().rev().take_while(|c| *c == ' ').count();
    let n = spaces.min(indent.width.max(1));
    lead[..lead.len() - n].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nowrap() -> Wrap {
        Wrap { cols: None, tab: 4 }
    }

    fn buf(text: &str, caret: usize) -> Buffer {
        let mut b = Buffer::new(text);
        b.set_caret(caret);
        b
    }

    #[test]
    fn typing_groups_into_one_undo() {
        let mut b = buf("", 0);
        for c in ["a", "b", "c"] {
            b.insert(c);
        }
        assert_eq!(b.text(), "abc");
        assert!(b.is_dirty());
        b.undo();
        assert_eq!(b.text(), "");
        assert!(!b.is_dirty());
        b.redo();
        assert_eq!(b.text(), "abc");
        assert_eq!(b.primary().head, 3);
    }

    #[test]
    fn saving_seals_the_group() {
        let mut b = buf("", 0);
        b.insert("a");
        b.mark_saved();
        b.insert("b");
        assert!(b.is_dirty());
        b.undo();
        assert_eq!(b.text(), "a");
        assert!(!b.is_dirty());
    }

    #[test]
    fn multi_cursor_insert_and_undo() {
        let mut b = buf("one\ntwo\nthree", 0);
        b.add_cursor(true, nowrap());
        b.add_cursor(true, nowrap());
        assert_eq!(b.sels.len(), 3);
        b.insert("- ");
        assert_eq!(b.text(), "- one\n- two\n- three");
        b.undo();
        assert_eq!(b.text(), "one\ntwo\nthree");
    }

    #[test]
    fn newline_keeps_indent_and_splits_braces() {
        let mut b = buf("    fn x() {}", 12);
        b.newline(false);
        assert_eq!(b.text(), "    fn x() {\n        \n    }");
        assert_eq!(b.line_col(b.primary().head), (1, 8));
        let mut p = buf("if x:", 5);
        p.newline(true);
        assert_eq!(p.text(), "if x:\n    ");
    }

    #[test]
    fn open_line_above_and_below() {
        let mut b = buf("    one\ntwo", 5);
        b.open_line(false, false);
        assert_eq!(b.text(), "    one\n    \ntwo");
        b.undo();
        b.set_caret(5);
        b.open_line(true, false);
        assert_eq!(b.text(), "    \n    one\ntwo");
        assert_eq!(b.primary().head, 4);
    }

    #[test]
    fn electric_close_brace_outdents() {
        let mut b = buf("{\n        ", 10);
        b.insert("}");
        assert_eq!(b.text(), "{\n    }");
    }

    #[test]
    fn backspace_removes_an_indent_level() {
        let mut b = buf("        x", 8);
        b.indent = Indent { tabs: false, width: 4 };
        b.backspace(false);
        assert_eq!(b.text(), "    x");
        b.backspace(false);
        assert_eq!(b.text(), "x");
    }

    #[test]
    fn word_motions() {
        let mut b = buf("let foo_bar = 42;", 0);
        b.move_cursors(Motion::WordRight, false, nowrap());
        assert_eq!(b.primary().head, 3);
        b.move_cursors(Motion::WordRight, false, nowrap());
        assert_eq!(b.primary().head, 11);
        b.move_cursors(Motion::WordLeft, true, nowrap());
        assert_eq!(b.primary().head, 4);
        assert_eq!(b.selected_text(), "foo_bar");
        b.backspace(true);
        assert_eq!(b.text(), "let  = 42;");
    }

    #[test]
    fn up_down_keep_goal_column() {
        let mut b = buf("abcdef\nab\nabcdef", 5);
        b.move_cursors(Motion::Down, false, nowrap());
        assert_eq!(b.line_col(b.primary().head), (1, 2));
        b.move_cursors(Motion::Down, false, nowrap());
        assert_eq!(b.line_col(b.primary().head), (2, 5));
    }

    #[test]
    fn up_down_follow_wrapped_rows() {
        let wrap = Wrap { cols: Some(8), tab: 4 };
        let mut b = buf("abcdefghijklmnopqrst\nxy", 1);
        b.move_cursors(Motion::Down, false, wrap);
        assert_eq!(b.primary().head, 9, "second row of the same line");
        b.move_cursors(Motion::Down, false, wrap);
        assert_eq!(b.primary().head, 17);
        b.move_cursors(Motion::Down, false, wrap);
        assert_eq!(b.line_col(b.primary().head), (1, 1));
    }

    #[test]
    fn smart_home() {
        let mut b = buf("    code", 6);
        b.move_cursors(Motion::Home, false, nowrap());
        assert_eq!(b.primary().head, 4);
        b.move_cursors(Motion::Home, false, nowrap());
        assert_eq!(b.primary().head, 0);
    }

    #[test]
    fn toggle_line_comments() {
        let mut b = buf("  a\n    b\n\n  c", 0);
        b.select_all();
        let style = CommentStyle { line: Some("//"), block: None };
        b.toggle_comment(style);
        assert_eq!(b.text(), "  // a\n  //   b\n\n  // c");
        b.select_all();
        b.toggle_comment(style);
        assert_eq!(b.text(), "  a\n    b\n\n  c");
    }

    #[test]
    fn toggle_block_comment_on_line() {
        let mut b = buf("  body { }", 3);
        let style = CommentStyle { line: None, block: Some(("/*", "*/")) };
        b.toggle_comment(style);
        assert_eq!(b.text(), "  /* body { } */");
        b.toggle_comment(style);
        assert_eq!(b.text(), "  body { }");
    }

    #[test]
    fn indent_and_outdent_selection() {
        let mut b = buf("a\nb\n", 0);
        b.set_sels(vec![Sel::new(0, 3)]);
        b.tab(4);
        assert_eq!(b.text(), "    a\n    b\n");
        b.outdent_lines();
        assert_eq!(b.text(), "a\nb\n");
        let mut t = buf("ab", 1);
        t.tab(4);
        assert_eq!(t.text(), "a   b", "spaces to the next tab stop");
    }

    #[test]
    fn move_and_duplicate_lines() {
        let mut b = buf("one\ntwo\nthree", 5);
        b.move_lines(true);
        assert_eq!(b.text(), "two\none\nthree");
        assert_eq!(b.line_col(b.primary().head), (0, 1));
        b.move_lines(false);
        b.move_lines(false);
        assert_eq!(b.text(), "one\nthree\ntwo");
        b.duplicate_lines();
        assert_eq!(b.text(), "one\nthree\ntwo\ntwo");
        assert_eq!(b.line_col(b.primary().head), (3, 1));
        b.delete_lines();
        assert_eq!(b.text(), "one\nthree\ntwo");
    }

    #[test]
    fn brackets_match_both_ways() {
        let b = buf("f(a[1], {b})", 0);
        assert_eq!(b.matching_bracket(1), Some((1, 11)));
        assert_eq!(b.matching_bracket(12), Some((11, 1)));
        assert_eq!(b.matching_bracket(4), Some((3, 5)), "caret right after `[`");
        assert_eq!(b.matching_bracket(0), None);
    }

    #[test]
    fn copy_whole_line_and_paste_above() {
        let mut b = buf("a\nb", 2);
        let clip = b.copy();
        assert_eq!(clip, "b\n");
        b.set_caret(0);
        b.paste(&clip);
        assert_eq!(b.text(), "b\na\nb");
    }

    #[test]
    fn paste_distributes_over_cursors() {
        let mut b = buf("x\ny", 1);
        b.add_cursor(true, nowrap());
        b.paste("1\n2");
        assert_eq!(b.text(), "x1\ny2");
    }

    #[test]
    fn select_next_occurrence_adds_selections() {
        let mut b = buf("foo bar foo", 1);
        b.select_next_occurrence();
        assert_eq!(b.selected_text(), "foo");
        b.select_next_occurrence();
        assert_eq!(b.sels.len(), 2);
        b.insert("x");
        assert_eq!(b.text(), "x bar x");
    }

    #[test]
    fn indentation_is_detected() {
        let two = Rope::from_str("a\n  b\n    c\n  d\n");
        assert_eq!(Indent::detect(&two), Some(Indent { tabs: false, width: 2 }));
        let tabs = Rope::from_str("a\n\tb\n\t\tc\n");
        assert_eq!(Indent::detect(&tabs).map(|i| i.tabs), Some(true));
        assert_eq!(Indent::detect(&Rope::from_str("a\nb\n")), None);
    }

    #[test]
    fn changed_lines_are_reported() {
        let mut b = buf("a\nb\nc", 4);
        b.take_changed();
        b.insert("x");
        assert_eq!(b.take_changed(), Some(Changed { first: 2, last: 2, old_lines: 3 }));
        assert_eq!(b.take_changed(), None);
        b.set_caret(0);
        b.insert("1\n2\n");
        assert_eq!(b.take_changed(), Some(Changed { first: 0, last: 2, old_lines: 3 }));
        // Two cursors far apart, one adding lines: the range covers both.
        let mut m = buf("a\nb\nc\nd\ne", 0);
        m.set_sels(vec![Sel::caret(2), Sel::caret(8)]);
        m.take_changed();
        m.insert("\n");
        let c = m.take_changed().unwrap();
        assert_eq!((c.first, c.old_lines), (1, 5));
        assert_eq!(c.last, 6, "the second edit's line, shifted by the first");
        m.undo();
        let c = m.take_changed().unwrap();
        assert_eq!((c.first, c.last, c.old_lines), (1, 4, 7));
    }

    #[test]
    fn trims_trailing_whitespace() {
        let mut b = buf("a  \nb\t\nc", 0);
        b.trim_trailing_whitespace();
        assert_eq!(b.text(), "a\nb\nc");
    }
}
