//! Syntax highlighting with syntect (plus bat's extra syntaxes via
//! two-face).
//!
//! syntect classifies text into our own token kinds through a fixed
//! "token theme" (each kind is encoded as a color), and kinds are turned
//! into Material colors only when painting. So a wallpaper change recolors
//! code instantly, without re-highlighting anything.
//!
//! Parsing is incremental: the parser state is checkpointed every
//! [`CHUNK`] lines, an edit drops only the checkpoints after it, and a
//! background thread extends the checkpoints over a snapshot of the rope,
//! so huge files open instantly and get colored as the parser catches up.
//! The lines on screen are parsed on the UI thread from the nearest
//! checkpoint.

use crate::buffer::{Changed, CommentStyle};
use material_3_ui::code::TOKEN_SCOPES;
use ropey::Rope;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use syntect::highlighting::{
    Color, FontStyle, HighlightState, Highlighter, RangedHighlightIterator, ScopeSelectors, StyleModifier, Theme,
    ThemeItem, ThemeSettings,
};
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};

/// Lines between parser checkpoints.
pub const CHUNK: usize = 128;
/// Longer lines are shown without highlighting.
const MAX_LINE: usize = 8_000;
/// Lines parsed above a not-yet-reached line to guess its colors.
const APPROX_LEAD: usize = 40;

pub use material_3_ui::code::{Span, Tok};

pub fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(two_face::syntax::extra_newlines)
}

/// The classification theme: each token kind's id is its red channel.
fn token_theme() -> &'static Theme {
    static THEME: OnceLock<Theme> = OnceLock::new();
    THEME.get_or_init(|| {
        use std::str::FromStr;
        let item = |scope: &str, tok: Tok| ThemeItem {
            scope: ScopeSelectors::from_str(scope).unwrap_or_default(),
            style: StyleModifier { foreground: Some(Color { r: tok as u8, g: 0, b: 0, a: 255 }), background: None, font_style: None },
        };
        Theme {
            name: Some("m3-tokens".into()),
            author: None,
            settings: ThemeSettings { foreground: Some(Color { r: 0, g: 0, b: 0, a: 255 }), ..Default::default() },
            scopes: TOKEN_SCOPES.iter().map(|(scope, tok)| item(scope, *tok)).collect(),
        }
    })
}

// -------------------------------------------------------------- languages

/// Comment markers by syntax name.
pub fn comment_style(syntax: &str) -> CommentStyle {
    let line = |t: &'static str| CommentStyle { line: Some(t), block: None };
    match syntax {
        "Rust" | "C" | "C++" | "Go" | "Java" | "JavaScript" | "TypeScript" | "TypeScriptReact" | "JavaScript (Babel)"
        | "C#" | "Swift" | "Kotlin" | "Scala" | "Dart" | "QML" | "Zig" | "PHP" | "Groovy" | "D" | "Protocol Buffer"
        | "GLSL" | "JSON" | "jsonnet" | "Objective-C" | "Objective-C++" | "SCSS" | "Less" | "Sass" | "Odin" | "V"
        | "Solidity" | "Hyprlang" => CommentStyle { line: Some("//"), block: Some(("/*", "*/")) },
        "Lua" | "SQL" | "Haskell" | "Elm" | "Ada" | "VHDL" => line("--"),
        "Lisp" | "Clojure" | "Scheme" | "Racket" | "Assembly (x86_64)" | "NASM" => line(";"),
        "LaTeX" | "TeX" | "Erlang" | "MATLAB" | "BibTeX" => line("%"),
        "VimL" => line("\""),
        "CSS" => CommentStyle { line: None, block: Some(("/*", "*/")) },
        "HTML" | "XML" | "Markdown" | "MultiMarkdown" | "SVG" => CommentStyle { line: None, block: Some(("<!--", "-->")) },
        "Plain Text" => CommentStyle::default(),
        _ => line("#"),
    }
}

/// Languages where a trailing `:` opens a block.
pub fn colon_indents(syntax: &str) -> bool {
    matches!(syntax, "Python" | "YAML" | "Nim" | "CoffeeScript" | "Cython")
}

/// Pick a syntax for a file from its name, extension or first line.
pub fn detect(path: Option<&Path>, first_line: &str) -> &'static SyntaxReference {
    let set = syntaxes();
    let by_name = |n: &str| set.find_syntax_by_name(n);
    if let Some(path) = path {
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        let special = match (name.as_str(), ext.as_str()) {
            (_, "conf" | "cfg" | "service" | "socket" | "timer" | "mount" | "target" | "network" | "netdev" | "link") => by_name("INI"),
            (_, "jsonc" | "json5") => by_name("JSON"),
            (_, "mjs" | "cjs" | "jsx") => by_name("JavaScript"),
            (_, "zsh" | "ksh") => by_name("Bourne Again Shell (bash)"),
            ("config", _) if path.parent().is_some_and(|p| p.ends_with(".ssh")) => by_name("SSH Config"),
            (".env" | ".envrc" | "APKBUILD" | "makepkg.conf" | ".xinitrc" | ".xprofile", _) => by_name("Bourne Again Shell (bash)"),
            ("pacman.conf" | "paru.conf" | "mpv.conf" | ".npmrc", _) => by_name("INI"),
            _ => None,
        };
        if let Some(s) = special {
            return s;
        }
        if let Some(s) = set.find_syntax_by_extension(&name) {
            return s;
        }
        if !ext.is_empty() {
            if let Some(s) = set.find_syntax_by_extension(&ext) {
                return s;
            }
        }
    }
    set.find_syntax_by_first_line(first_line).unwrap_or_else(|| set.find_syntax_plain_text())
}

/// The languages offered in the status bar picker, sorted by name.
pub fn language_names() -> Vec<&'static str> {
    let mut v: Vec<&str> = syntaxes()
        .syntaxes()
        .iter()
        .filter(|s| !s.hidden && !s.file_extensions.is_empty() || s.name == "Plain Text")
        .map(|s| s.name.as_str())
        .collect();
    v.sort_by_key(|s| s.to_lowercase());
    v.dedup();
    v
}

pub fn by_name(name: &str) -> &'static SyntaxReference {
    syntaxes().find_syntax_by_name(name).unwrap_or_else(|| syntaxes().find_syntax_plain_text())
}

// ------------------------------------------------------------ highlighter

/// The parser's state and the scopes open at the start of a line.
type State = (ParseState, ScopeStack);

/// Incremental highlighting of one document.
pub struct Highlight {
    syntax: &'static SyntaxReference,
    plain: bool,
    /// `checkpoints[k]`: the state at the start of line `k * CHUNK`.
    checkpoints: Vec<State>,
    /// Where parsing last stopped, to continue cheaply down the screen.
    cursor: Option<(usize, State)>,
    lines: HashMap<usize, Arc<Vec<Span>>>,
    /// Guesses for lines past the parsed part (see [`Highlight::line`]).
    approx: HashMap<usize, Arc<Vec<Span>>>,
    approx_cursor: Option<(usize, State)>,
    worker: Option<Worker>,
    /// Checkpoints from before an edit that kept the line count, starting
    /// at index `stash_from`; usable past line `stash_edits`.
    stash: Vec<State>,
    stash_from: usize,
    stash_edits: usize,
}

/// A parser state moved to or from the background thread.
struct Sendable(State);

// SAFETY: with the Oniguruma backend a `ParseState` can hold a capture
// `Region`, a heap block behind a raw pointer that's only reachable through
// this value. Cloning deep-copies it, so each state is owned by exactly one
// thread at a time; nothing is shared.
unsafe impl Send for Sendable {}

impl Sendable {
    fn into_inner(self) -> State {
        self.0
    }
}

/// A background thread extending the checkpoints.
struct Worker {
    rx: flume::Receiver<(usize, Sendable)>,
    cancel: Arc<AtomicBool>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

fn initial(syntax: &SyntaxReference) -> State {
    (ParseState::new(syntax), ScopeStack::new())
}

fn highlighter() -> &'static Highlighter<'static> {
    static HL: OnceLock<Highlighter<'static>> = OnceLock::new();
    HL.get_or_init(|| Highlighter::new(token_theme()))
}

impl Highlight {
    pub fn new(syntax: &'static SyntaxReference) -> Self {
        let plain = syntax.name == "Plain Text";
        Self { syntax, plain, checkpoints: vec![initial(syntax)], cursor: None, lines: HashMap::new(), approx: HashMap::new(), approx_cursor: None, worker: None, stash: Vec::new(), stash_from: 0, stash_edits: 0 }
    }

    pub fn syntax(&self) -> &'static SyntaxReference {
        self.syntax
    }

    pub fn name(&self) -> &'static str {
        self.syntax.name.as_str()
    }

    /// Forget everything from `line` on (after an edit there).
    ///
    /// When the edit kept the line count, the dropped checkpoints are kept
    /// aside: once re-parsing reaches a checkpoint past every edit with the
    /// same state as before, the rest of the old ones are still right, so
    /// typing in a huge file doesn't re-parse all of it.
    pub fn invalidate(&mut self, c: Changed, lines_now: usize) {
        let line = c.first;
        let keep = line / CHUNK + 1;
        if self.checkpoints.len() > keep || self.worker.is_some() {
            // The worker's snapshot is out of date.
            self.worker = None;
        }
        if lines_now == c.old_lines {
            if self.stash.is_empty() && self.checkpoints.len() > keep {
                self.stash = self.checkpoints.split_off(keep);
                self.stash_from = keep;
                self.stash_edits = 0;
            }
            self.stash_edits = self.stash_edits.max(c.last + 1);
        } else {
            self.stash.clear();
        }
        self.checkpoints.truncate(keep);
        if self.cursor.as_ref().is_some_and(|(l, _)| *l > line) {
            self.cursor = None;
        }
        self.lines.retain(|l, _| *l < line);
        self.approx.clear();
        self.approx_cursor = None;
    }

    /// Lines with a known parser state (everything before this is cheap).
    pub fn frontier(&self) -> usize {
        (self.checkpoints.len() - 1) * CHUNK
    }

    /// Take the checkpoints the background parser has made, and start it
    /// when there's more of the file to parse. Returns true while working.
    pub fn pump(&mut self, rope: &Rope, ctx: &egui::Context) -> bool {
        if self.plain {
            return false;
        }
        if let Some(w) = &self.worker {
            let mut converged = false;
            for (k, Sendable(state)) in w.rx.try_iter() {
                if k != self.checkpoints.len() {
                    continue;
                }
                // Back in step with the parse from before the edit?
                let old = k.checked_sub(self.stash_from).and_then(|i| self.stash.get(i));
                if k * CHUNK >= self.stash_edits && old == Some(&state) {
                    let i = k - self.stash_from;
                    self.checkpoints.extend(self.stash.drain(i..));
                    converged = true;
                    break;
                }
                self.checkpoints.push(state);
            }
            if converged {
                self.stash.clear();
                self.worker = None;
            } else if w.rx.is_disconnected() && w.rx.is_empty() {
                self.worker = None;
            }
        }
        if self.frontier() + CHUNK > rope.len_lines() {
            self.stash.clear();
        }
        let n = rope.len_lines();
        if self.worker.is_none() && self.frontier() + CHUNK <= n {
            let (tx, rx) = flume::unbounded();
            let cancel = Arc::new(AtomicBool::new(false));
            let stop = cancel.clone();
            let rope = rope.clone();
            let syntax = self.syntax;
            let mut k = self.checkpoints.len() - 1;
            let start = Sendable(self.checkpoints[k].clone());
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let mut state = start.into_inner();
                while (k + 1) * CHUNK <= rope.len_lines() {
                    for l in k * CHUNK..(k + 1) * CHUNK {
                        if stop.load(Ordering::Relaxed) {
                            return;
                        }
                        parse_line(&rope, l, &mut state, syntax, false);
                    }
                    k += 1;
                    if tx.send((k, Sendable(state.clone()))).is_err() {
                        return;
                    }
                    if k.is_multiple_of(64) {
                        ctx.request_repaint();
                    }
                }
                ctx.request_repaint();
            });
            self.worker = Some(Worker { rx, cancel });
        }
        self.worker.is_some()
    }

    /// Parse ahead on this thread until `upto` has a checkpoint.
    #[cfg(test)]
    pub fn advance(&mut self, rope: &Rope, upto: usize) {
        let upto = upto.min(rope.len_lines());
        while self.frontier() + CHUNK <= upto {
            let from = self.frontier();
            let mut state = self.checkpoints.last().expect("never empty").clone();
            for l in from..from + CHUNK {
                parse_line(rope, l, &mut state, self.syntax, false);
            }
            self.checkpoints.push(state);
        }
    }

    /// The spans of a line. Lines the background parser hasn't reached yet
    /// get a guess, parsed from a little above them.
    pub fn line(&mut self, rope: &Rope, line: usize) -> Arc<Vec<Span>> {
        match self.exact(rope, line) {
            Some(s) => s,
            None => self.approximate(rope, line),
        }
    }

    fn approximate(&mut self, rope: &Rope, line: usize) -> Arc<Vec<Span>> {
        if let Some(s) = self.approx.get(&line) {
            return s.clone();
        }
        let (mut at, mut state) = match self.approx_cursor.take() {
            Some((l, s)) if l <= line && line - l < 2 * CHUNK => (l, s),
            _ => (line.saturating_sub(APPROX_LEAD), initial(self.syntax)),
        };
        while at <= line {
            let spans = Arc::new(parse_line(rope, at, &mut state, self.syntax, true));
            self.approx.insert(at, spans);
            at += 1;
        }
        self.approx_cursor = Some((at, state));
        if self.approx.len() > 2000 {
            self.approx.retain(|l, _| l.abs_diff(line) < 300);
        }
        self.approx[&line].clone()
    }

    /// The spans of a line, or `None` while the parser hasn't reached it.
    fn exact(&mut self, rope: &Rope, line: usize) -> Option<Arc<Vec<Span>>> {
        if self.plain || line >= rope.len_lines() {
            return Some(Arc::new(Vec::new()));
        }
        if let Some(s) = self.lines.get(&line) {
            return Some(s.clone());
        }
        let k = (line / CHUNK).min(self.checkpoints.len() - 1);
        if line - k * CHUNK > CHUNK {
            return None;
        }
        let (mut at, mut state) = match self.cursor.take() {
            Some((l, s)) if k * CHUNK <= l && l <= line => (l, s),
            _ => (k * CHUNK, self.checkpoints[k].clone()),
        };
        while at < line {
            let spans = parse_line(rope, at, &mut state, self.syntax, true);
            if line - at < 64 {
                self.lines.insert(at, Arc::new(spans));
            }
            at += 1;
        }
        let spans = Arc::new(parse_line(rope, line, &mut state, self.syntax, true));
        self.lines.insert(line, spans.clone());
        self.cursor = Some((line + 1, state));
        if self.lines.len() > 4000 {
            self.lines.retain(|l, _| l.abs_diff(line) < 400);
        }
        Some(spans)
    }
}

/// Move `state` past a line; with `classify`, also return its spans.
fn parse_line(rope: &Rope, line: usize, state: &mut State, syntax: &SyntaxReference, classify: bool) -> Vec<Span> {
    let slice = rope.line(line);
    if slice.len_chars() > MAX_LINE {
        return Vec::new();
    }
    let text = slice.to_string();
    let Ok(ops) = state.0.parse_line(&text, syntaxes()) else {
        // A broken grammar state: start over for the rest of the file.
        *state = initial(syntax);
        return Vec::new();
    };
    let spans = if classify { spans_from(&text, &ops, &state.1) } else { Vec::new() };
    for (_, op) in &ops {
        let _ = state.1.apply(op);
    }
    spans
}

fn spans_from(text: &str, ops: &[(usize, syntect::parsing::ScopeStackOp)], stack: &ScopeStack) -> Vec<Span> {
    let hl = highlighter();
    let mut hs = HighlightState::new(hl, stack.clone());
    let mut out: Vec<Span> = Vec::new();
    let mut chars = 0usize;
    for (style, piece, _) in RangedHighlightIterator::new(&mut hs, ops, text, hl) {
        let n = piece.trim_end_matches('\n').chars().count();
        let tok = Tok::from_u8(style.foreground.r);
        let tok = if tok == Tok::Plain && style.font_style.contains(FontStyle::ITALIC) { Tok::Emphasis } else { tok };
        if n > 0 && tok != Tok::Plain {
            match out.last_mut() {
                Some(last) if last.tok == tok && last.end == chars => last.end += n,
                _ => out.push(Span { start: chars, end: chars + n, tok }),
            }
        }
        chars += n;
    }
    out
}

/// Highlight a standalone snippet (Markdown code blocks). Spans per line.
pub fn snippet(code: &str, lang: &str) -> Vec<Vec<Span>> {
    let set = syntaxes();
    let syntax = set
        .find_syntax_by_token(lang)
        .or_else(|| set.find_syntax_by_extension(lang))
        .unwrap_or_else(|| set.find_syntax_plain_text());
    let mut state = initial(syntax);
    syntect::util::LinesWithEndings::from(code)
        .map(|line| match state.0.parse_line(line, set) {
            Ok(ops) => {
                let spans = if line.len() <= MAX_LINE { spans_from(line, &ops, &state.1) } else { Vec::new() };
                for (_, op) in &ops {
                    let _ = state.1.apply(op);
                }
                spans
            }
            Err(_) => Vec::new(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(h: &mut Highlight, rope: &Rope, line: usize) -> Vec<(String, Tok)> {
        let text = rope.line(line).to_string();
        h.line(rope, line)
            .iter()
            .map(|s| (text.chars().skip(s.start).take(s.end - s.start).collect(), s.tok))
            .collect()
    }

    #[test]
    fn detects_languages() {
        let d = |p: &str| detect(Some(Path::new(p)), "").name.as_str();
        assert_eq!(d("/x/main.rs"), "Rust");
        assert_eq!(d("/x/Shell.qml"), "QML");
        assert_eq!(d("/x/Cargo.toml"), "TOML");
        assert_eq!(d("/x/hyprland.conf"), "INI");
        assert_eq!(d("/x/PKGBUILD"), "Bourne Again Shell (bash)");
        assert_eq!(d("/x/app.ts"), "TypeScript");
        assert_eq!(d("/x/init.lua"), "Lua");
        assert_eq!(d("/x/README.md"), "Markdown");
        assert_eq!(d("/home/u/.ssh/config"), "SSH Config");
        assert_eq!(detect(Some(Path::new("/x/script")), "#!/usr/bin/env python3").name, "Python");
        assert_eq!(detect(None, "hello").name, "Plain Text");
        for name in ["C", "C++", "Go", "Python", "JavaScript", "YAML", "JSON", "CSS", "HTML", "INI"] {
            assert_eq!(by_name(name).name, name);
        }
        assert!(language_names().contains(&"Rust"));
    }

    #[test]
    fn classifies_rust_tokens() {
        let rope = Rope::from_str("// hi\nfn main() { let x = \"s\"; 42 }\n");
        let mut h = Highlight::new(detect(Some(Path::new("a.rs")), ""));
        assert_eq!(toks(&mut h, &rope, 0), vec![("// hi".into(), Tok::Comment)]);
        let t = toks(&mut h, &rope, 1);
        assert!(t.contains(&("fn".into(), Tok::Keyword)), "{t:?}");
        assert!(t.contains(&("main".into(), Tok::Function)), "{t:?}");
        assert!(t.iter().any(|(s, k)| s.contains('s') && *k == Tok::String), "{t:?}");
        assert!(t.contains(&("42".into(), Tok::Number)), "{t:?}");
    }

    #[test]
    fn block_comment_state_carries_across_lines_and_edits() {
        let mut text = String::from("/*\n");
        for _ in 0..300 {
            text.push_str("still comment\n");
        }
        text.push_str("*/\nfn x() {}\n");
        let mut rope = Rope::from_str(&text);
        let mut h = Highlight::new(by_name("Rust"));
        assert!(h.exact(&rope, 250).is_none(), "not parsed that far yet");
        // Meanwhile it's guessed from a few lines above (wrongly, here).
        assert!(toks(&mut h, &rope, 250).iter().all(|(_, k)| *k != Tok::Comment));
        h.advance(&rope, 302);
        assert_eq!(toks(&mut h, &rope, 250), vec![("still comment".into(), Tok::Comment)]);
        assert!(toks(&mut h, &rope, 302).contains(&("fn".into(), Tok::Keyword)));
        // Remove the opening `/*`: the rest is code now.
        rope.remove(0..2);
        h.invalidate(Changed { first: 0, last: 0, old_lines: rope.len_lines() }, rope.len_lines());
        h.advance(&rope, 302);
        assert!(toks(&mut h, &rope, 250).iter().all(|(_, k)| *k != Tok::Comment));
    }

    #[test]
    fn background_parser_catches_up() {
        let mut text = String::new();
        for i in 0..2000 {
            text.push_str(&format!("fn f{i}() {{ let x = {i}; }}\n"));
        }
        let rope = Rope::from_str(&text);
        let mut h = Highlight::new(by_name("Rust"));
        let ctx = egui::Context::default();
        let start = std::time::Instant::now();
        while h.pump(&rope, &ctx) {
            std::thread::sleep(std::time::Duration::from_millis(5));
            assert!(start.elapsed().as_secs() < 30);
        }
        assert!(h.frontier() + CHUNK > 2000);
        assert!(toks(&mut h, &rope, 1990).contains(&("fn".into(), Tok::Keyword)));
        // An edit restarts it from there.
        h.invalidate(Changed { first: 500, last: 500, old_lines: 2001 }, 2001);
        assert_eq!(h.frontier(), 384);
        // Same line count: the old checkpoints come back once re-parsing
        // agrees with them again.
        let t = std::time::Instant::now();
        while h.pump(&rope, &ctx) {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(h.frontier() + CHUNK > 2000);
        assert!(h.stash.is_empty());
        assert!(t.elapsed().as_secs() < 30);
        // A real change of state is followed to the end.
        let mut text2 = String::from("/*\n");
        text2.push_str(&text);
        let rope2 = Rope::from_str(&text2);
        h.invalidate(Changed { first: 0, last: 1, old_lines: 2001 }, rope2.len_lines());
        while h.pump(&rope2, &ctx) {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(toks(&mut h, &rope2, 1990).iter().all(|(_, k)| *k == Tok::Comment));
    }

    #[test]
    fn comment_styles() {
        assert_eq!(comment_style("Rust").line, Some("//"));
        assert_eq!(comment_style("Python").line, Some("#"));
        assert_eq!(comment_style("Lua").line, Some("--"));
        assert_eq!(comment_style("CSS").line, None);
        assert!(colon_indents("Python") && !colon_indents("Rust"));
    }

    #[test]
    fn snippets_highlight() {
        let lines = snippet("let x = 1;\n", "rust");
        assert!(lines[0].iter().any(|s| s.tok == Tok::Keyword));
    }
}
#[cfg(test)]
mod bench {
    #[test]
    #[ignore]
    fn parse_speed() {
        let text = std::fs::read_to_string(std::env::var("BENCH_FILE").unwrap()).unwrap();
        let rope = ropey::Rope::from_str(&text);
        let mut h = super::Highlight::new(super::by_name("Rust"));
        let t = std::time::Instant::now();
        h.advance(&rope, 50_000);
        eprintln!("50k lines: {:?}", t.elapsed());
    }
}
