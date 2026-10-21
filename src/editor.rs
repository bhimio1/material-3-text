//! The editor widget: a virtualized view over a [`Buffer`].
//!
//! Only the rows on screen are laid out and painted, so a file with
//! hundreds of thousands of lines scrolls as smoothly as a short one. Row
//! geometry comes from [`Metrics`] (each line's display width, and with
//! word wrap the rows before each line), updated from the first changed
//! line after an edit.

use crate::buffer::{Buffer, Changed, CommentStyle, Motion, Sel};
use crate::layout::{self, Expanded, Wrap};
use crate::syntax::{Highlight, Span, Tok};
use egui::text::{CCursor, LayoutJob, TextFormat};
use egui::{pos2, vec2, Align2, Color32, CornerRadius, Event, FontId, Id, Key, Modifiers, Rect, Sense, Stroke, Ui};
use material_3_ui::theme::M3Palette;
use std::time::{Duration, Instant};

/// Lines longer than this (in columns) are laid out only where visible
/// when not wrapping.
const CLIP_COLS: usize = 1500;
const GUTTER_PAD: f32 = 14.0;
const TEXT_PAD: f32 = 8.0;

#[derive(Clone, Copy, Debug)]
pub struct EditorOptions {
    pub font_size: f32,
    pub tab: usize,
    pub wrap: bool,
    pub show_whitespace: bool,
    pub line_numbers: bool,
    pub highlight_line: bool,
    pub colon_indents: bool,
    pub comment: CommentStyle,
}

/// Display widths of every line and, when wrapping, row offsets.
#[derive(Default)]
pub struct Metrics {
    widths: Vec<u32>,
    /// Visual rows of each line (wrap only).
    rows: Vec<u32>,
    /// `rows_before[l]`: visual rows above line `l` (wrap only).
    rows_before: Vec<u32>,
    total_rows: usize,
    max_width: usize,
    tab: usize,
    wrap_cols: Option<usize>,
    /// Measure every line again.
    full: bool,
    rows_stale: bool,
}

/// Widths (and rows, when wrapping) of `count` lines from `first`.
fn measure(buf: &Buffer, first: usize, count: usize, tab: usize, wrap: Option<usize>) -> (Vec<u32>, Vec<u32>) {
    let tab = tab.max(1);
    let mut widths = Vec::with_capacity(count);
    let mut rows = Vec::with_capacity(if wrap.is_some() { count } else { 0 });
    for line in buf.rope().lines_at(first.min(buf.len_lines())).take(count) {
        let mut col = 0usize;
        for c in line.chars() {
            match c {
                '\n' => break,
                '\t' => col += tab - col % tab,
                _ => col += 1,
            }
        }
        widths.push(col as u32);
        if let Some(w) = wrap {
            let n = if col <= w { 1 } else { layout::row_starts(line.chars(), tab, w).len() };
            rows.push(n as u32);
        }
    }
    (widths, rows)
}

impl Metrics {
    /// Re-measure only the lines an edit touched.
    pub fn apply(&mut self, c: Changed, buf: &Buffer) {
        let n = buf.len_lines();
        if self.full || self.widths.len() != c.old_lines || self.tab == 0 {
            self.full = true;
            return;
        }
        let last = c.last.min(n - 1);
        let first = c.first.min(last);
        let old_end = (last as isize + 1 - (n as isize - c.old_lines as isize)).clamp(first as isize, self.widths.len() as isize) as usize;
        let (widths, rows) = measure(buf, first, last - first + 1, self.tab, self.wrap_cols);
        // The widest line only grows between full measurements.
        self.max_width = self.max_width.max(widths.iter().copied().max().unwrap_or(0) as usize);
        self.widths.splice(first..old_end, widths);
        if self.wrap_cols.is_some() {
            self.rows.splice(first..old_end.min(self.rows.len()), rows);
        }
        if self.widths.len() != n || (self.wrap_cols.is_some() && self.rows.len() != n) {
            self.full = true;
        }
        self.rows_stale = true;
    }

    fn update(&mut self, buf: &Buffer, wrap: Wrap) {
        let n = buf.len_lines();
        if self.tab != wrap.tab || self.widths.len() != n || self.wrap_cols != wrap.cols {
            self.full = true;
        }
        if std::mem::take(&mut self.full) {
            self.tab = wrap.tab;
            self.wrap_cols = wrap.cols;
            (self.widths, self.rows) = measure(buf, 0, n, wrap.tab, wrap.cols);
            self.max_width = self.widths.iter().copied().max().unwrap_or(0) as usize;
            self.rows_stale = true;
        }
        if std::mem::take(&mut self.rows_stale) {
            self.rows_before.clear();
            if wrap.cols.is_some() {
                self.rows_before.reserve(n + 1);
                let mut acc = 0u32;
                for r in &self.rows {
                    self.rows_before.push(acc);
                    acc += r;
                }
                self.rows_before.push(acc);
                self.total_rows = acc as usize;
            } else {
                self.total_rows = n;
            }
        }
    }

    fn rows_before(&self, line: usize) -> usize {
        if self.wrap_cols.is_some() {
            self.rows_before.get(line).copied().unwrap_or(self.total_rows as u32) as usize
        } else {
            line
        }
    }

    /// The line at a visual row, and the row within it.
    fn line_at_row(&self, row: usize) -> (usize, usize) {
        if self.wrap_cols.is_none() {
            let line = row.min(self.widths.len().saturating_sub(1));
            return (line, 0);
        }
        let line = self.rows_before.partition_point(|&r| r as usize <= row).saturating_sub(1);
        let line = line.min(self.widths.len().saturating_sub(1));
        (line, row - self.rows_before[line] as usize)
    }
}

#[derive(Clone, Copy, PartialEq)]
enum DragMode {
    Char,
    Word(usize, usize),
    Line(usize),
}

/// Per-document view state.
#[derive(Default)]
pub struct ViewState {
    pub metrics: Metrics,
    /// Scroll the primary caret into view next frame.
    pub reveal: bool,
    /// Center this line next frame (go to line, search results).
    pub center_line: Option<usize>,
    drag: Option<DragMode>,
    last_press: Option<(Instant, usize, u8)>,
    page_rows: usize,
    pub focus: bool,
    /// Wrap width last frame, for keyboard movement.
    wrap_cols: Option<usize>,
    /// First and last line on screen last frame.
    pub visible: (usize, usize),
}

/// Extra things to draw: find matches (char ranges, sorted).
#[derive(Default)]
pub struct Decorations<'a> {
    pub matches: &'a [(usize, usize)],
    pub current: Option<usize>,
}

#[derive(Default)]
pub struct EditorOutput {
    pub changed: bool,
    pub moved: bool,
    pub focused: bool,
}

struct Geometry {
    font: FontId,
    char_w: f32,
    row_h: f32,
    wrap: Wrap,
}

#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut Ui,
    id: Id,
    buf: &mut Buffer,
    hl: &mut Highlight,
    view: &mut ViewState,
    deco: &Decorations,
    opts: &EditorOptions,
    p: &M3Palette,
) -> EditorOutput {
    let mut out = EditorOutput::default();
    let font = FontId::monospace(opts.font_size);
    let (char_w, row_h) = ui.fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
    let row_h = (row_h * 1.35).round();
    let digits = buf.len_lines().to_string().len().max(3);
    let gutter_w = if opts.line_numbers { digits as f32 * char_w + GUTTER_PAD * 2.0 } else { GUTTER_PAD };
    let outer = ui.available_rect_before_wrap();
    let text_w = outer.width() - gutter_w - TEXT_PAD * 2.0 - 14.0;
    let wrap_cols = opts.wrap.then(|| ((text_w / char_w).floor() as usize).max(8));
    let wrap = Wrap { cols: wrap_cols, tab: opts.tab };
    view.wrap_cols = wrap_cols;
    view.metrics.update(buf, wrap);
    let g = Geometry { font, char_w, row_h, wrap };

    // Keyboard first, so this frame paints the result.
    let focused = ui.memory(|m| m.has_focus(id));
    if focused {
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(id, egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: false })
        });
        let (changed, moved) = handle_keys(ui, buf, view, opts, wrap);
        out.changed |= changed;
        out.moved |= moved;
        if changed || moved {
            view.reveal = true;
        }
        if changed {
            if let Some(c) = buf.take_changed() {
                hl.invalidate(c, buf.len_lines());
                view.metrics.apply(c, buf);
            }
            view.metrics.update(buf, wrap);
        }
    }
    out.focused = focused;

    let content_w = if opts.wrap {
        0.0
    } else {
        view.metrics.max_width as f32 * char_w + TEXT_PAD * 2.0 + char_w * 4.0
    };
    let content_h = view.metrics.total_rows as f32 * row_h + outer.height() * 0.5;

    let gutter_rect = Rect::from_min_size(outer.min, vec2(gutter_w, outer.height()));
    let text_rect = Rect::from_min_max(pos2(outer.min.x + gutter_w, outer.min.y), outer.max);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(text_rect).id_salt(id.with("scroll")));
    let scroll = if opts.wrap { egui::ScrollArea::vertical() } else { egui::ScrollArea::both() };
    let mut scrolled = egui::Vec2::ZERO;
    scroll
        .id_salt(id.with("area"))
        .auto_shrink([false, false])
        .show_viewport(&mut child, |ui, viewport| {
            let origin = ui.max_rect().min;
            scrolled = viewport.min.to_vec2();
            let size = vec2(content_w.max(viewport.width()), content_h.max(viewport.height()));
            let rect = Rect::from_min_size(origin, size);
            ui.allocate_rect(rect, Sense::hover());
            let resp = ui.interact(ui.clip_rect().intersect(rect), id, Sense::click_and_drag());
            resp.clone().on_hover_cursor(egui::CursorIcon::Text);
            view.page_rows = ((viewport.height() / row_h) as usize).saturating_sub(2).max(1);

            if handle_pointer(ui, &resp, buf, view, &g, origin) {
                out.moved = true;
            }
            reveal(ui, buf, view, &g, origin);

            let first_row = (viewport.min.y / row_h).floor().max(0.0) as usize;
            let last_row = (viewport.max.y / row_h).ceil() as usize + 1;
            let last = last_row.min(view.metrics.total_rows.max(1) - 1);
            view.visible = (view.metrics.line_at_row(first_row).0, view.metrics.line_at_row(last).0);
            paint_rows(ui, buf, hl, view, deco, opts, &g, p, origin, viewport, first_row, last_row, ui.memory(|m| m.has_focus(id)));
        });
    // The offset the rows were painted with, so the gutter matches them.
    let offset_y = scrolled.y;
    paint_gutter(ui, buf, view, opts, &g, p, gutter_rect, offset_y);

    // IME: tell the platform where the caret is.
    if ui.memory(|m| m.has_focus(id)) {
        let (cx, cy) = caret_xy(buf, view, &g, buf.primary().head);
        let to_global = ui.ctx().layer_transform_to_global(ui.layer_id()).unwrap_or_default();
        let screen = pos2(text_rect.min.x + TEXT_PAD + cx - scrolled.x, text_rect.min.y + cy - offset_y);
        let cursor_rect = Rect::from_min_size(screen, vec2(2.0, row_h));
        ui.output_mut(|o| {
            o.ime = Some(egui::output::IMEOutput {
                purpose: egui::IMEPurpose::Normal,
                rect: to_global * text_rect,
                cursor_rect: to_global * cursor_rect,
                should_interrupt_composition: false,
            })
        });
    }
    if view.focus {
        ui.memory_mut(|m| m.request_focus(id));
        view.focus = false;
    }
    ui.advance_cursor_after_rect(outer);
    out
}

/// Keep the background highlighter going; true while it's working.
pub fn pump_highlight(ctx: &egui::Context, buf: &Buffer, hl: &mut Highlight) -> bool {
    hl.pump(buf.rope(), ctx)
}

/// Content-space position of a caret (x from the text start).
fn caret_xy(buf: &Buffer, view: &ViewState, g: &Geometry, pos: usize) -> (f32, f32) {
    let (line, col) = buf.line_col(pos);
    let text = buf.line_text(line);
    let dcol = layout::display_col(&text, col, g.wrap.tab);
    let (row, in_row) = g.wrap.row_of(&text, dcol);
    let y = (view.metrics.rows_before(line) + row) as f32 * g.row_h;
    (in_row as f32 * g.char_w, y)
}

fn reveal(ui: &mut Ui, buf: &Buffer, view: &mut ViewState, g: &Geometry, origin: egui::Pos2) {
    if let Some(line) = view.center_line.take() {
        let y = view.metrics.rows_before(line.min(buf.len_lines().saturating_sub(1))) as f32 * g.row_h;
        let r = Rect::from_min_size(origin + vec2(0.0, y), vec2(1.0, g.row_h));
        ui.scroll_to_rect(r, Some(egui::Align::Center));
        view.reveal = false;
        // Also bring the caret's column into view.
        let (x, _) = caret_xy(buf, view, g, buf.primary().head);
        let r = Rect::from_min_size(origin + vec2(x + TEXT_PAD, y), vec2(2.0, g.row_h));
        ui.scroll_to_rect(r.expand2(vec2(g.char_w * 8.0, 0.0)), None);
        return;
    }
    if !std::mem::take(&mut view.reveal) {
        return;
    }
    let (x, y) = caret_xy(buf, view, g, buf.primary().head);
    let r = Rect::from_min_size(origin + vec2(x + TEXT_PAD, y), vec2(2.0, g.row_h));
    ui.scroll_to_rect(r.expand2(vec2(g.char_w * 6.0, g.row_h * 2.0)), None);
}

// ------------------------------------------------------------------ input

fn handle_keys(ui: &mut Ui, buf: &mut Buffer, view: &mut ViewState, opts: &EditorOptions, wrap: Wrap) -> (bool, bool) {
    let (events, mut alt) = ui.input(|i| (i.events.clone(), i.modifiers.alt && !i.modifiers.command));
    let (mut changed, mut moved) = (false, false);
    let before = buf.version();
    for ev in events {
        match ev {
            // Alt+key shortcuts also arrive as text on X11; don't type it.
            Event::Text(t) if !alt => {
                if !t.is_empty() && t != "\t" && !t.chars().all(char::is_control) {
                    buf.insert(&t);
                }
            }
            Event::Ime(egui::ImeEvent::Commit(t)) => {
                if !t.is_empty() {
                    buf.insert(&t);
                }
            }
            Event::Copy => {
                if buf.has_selection() || buf.len_chars() > 0 {
                    let t = buf.copy();
                    ui.ctx().copy_text(t);
                }
            }
            Event::Cut => {
                let t = buf.cut();
                ui.ctx().copy_text(t);
            }
            Event::Paste(t) => buf.paste(&t),
            Event::Key { key, pressed, modifiers, .. } => {
                // Several keys can arrive in one frame: follow Alt per event.
                alt = modifiers.alt && !modifiers.command;
                if pressed && key_action(buf, view, opts, wrap, key, modifiers) {
                    moved = true;
                }
            }
            _ => {}
        }
    }
    if buf.version() != before {
        changed = true;
    }
    (changed, moved)
}

/// One key press. Returns true when it was handled (moved or edited).
fn key_action(buf: &mut Buffer, view: &mut ViewState, opts: &EditorOptions, wrap: Wrap, key: Key, m: Modifiers) -> bool {
    let ctrl = m.command;
    let shift = m.shift;
    let alt = m.alt;
    let mv = |buf: &mut Buffer, motion: Motion| buf.move_cursors(motion, shift, wrap);
    match key {
        Key::ArrowUp | Key::ArrowDown if alt && !ctrl && !shift => buf.move_lines(key == Key::ArrowUp),
        Key::ArrowUp | Key::ArrowDown if (ctrl && alt) || (shift && alt) => buf.add_cursor(key == Key::ArrowDown, wrap),
        Key::ArrowLeft => mv(buf, if ctrl || alt { Motion::WordLeft } else { Motion::Left }),
        Key::ArrowRight => mv(buf, if ctrl || alt { Motion::WordRight } else { Motion::Right }),
        Key::ArrowUp if !ctrl => mv(buf, Motion::Up),
        Key::ArrowDown if !ctrl => mv(buf, Motion::Down),
        Key::Home => mv(buf, if ctrl { Motion::DocStart } else { Motion::Home }),
        Key::End => mv(buf, if ctrl { Motion::DocEnd } else { Motion::End }),
        Key::PageUp => mv(buf, Motion::PageUp(view.page_rows)),
        Key::PageDown => mv(buf, Motion::PageDown(view.page_rows)),
        Key::Enter if ctrl => buf.open_line(shift, opts.colon_indents),
        Key::Enter => buf.newline(opts.colon_indents),
        Key::Backspace => buf.backspace(ctrl),
        Key::Delete if shift && !ctrl => {
            buf.delete_lines();
        }
        Key::Delete => buf.delete(ctrl),
        Key::Tab if shift => buf.outdent_lines(),
        Key::Tab if !ctrl => buf.tab(opts.tab),
        Key::Escape if buf.sels.len() > 1 || buf.has_selection() => buf.single(buf.sels.len() == 1),
        Key::Z if ctrl && shift => {
            buf.redo();
        }
        Key::Z if ctrl => {
            buf.undo();
        }
        Key::Y if ctrl => {
            buf.redo();
        }
        Key::A if ctrl => buf.select_all(),
        Key::L if ctrl => buf.select_line(),
        Key::D if ctrl && shift => buf.duplicate_lines(),
        Key::D if ctrl => buf.select_next_occurrence(),
        Key::K if ctrl && shift => buf.delete_lines(),
        Key::Slash if ctrl => buf.toggle_comment(opts.comment),
        Key::CloseBracket if ctrl => buf.indent_lines(),
        Key::OpenBracket if ctrl => buf.outdent_lines(),
        Key::M if ctrl => {
            if let Some((_, other)) = buf.matching_bracket(buf.primary().head) {
                buf.set_caret(other);
            }
        }
        _ => return false,
    }
    true
}

fn handle_pointer(ui: &mut Ui, resp: &egui::Response, buf: &mut Buffer, view: &mut ViewState, g: &Geometry, origin: egui::Pos2) -> bool {
    let (pressed, mods, pointer) = ui.input(|i| (i.pointer.primary_pressed(), i.modifiers, i.pointer.interact_pos()));
    let Some(pointer) = pointer else { return false };
    if pressed && resp.hovered() {
        resp.request_focus();
        let pos = hit(ui, buf, view, g, pointer - origin);
        let now = Instant::now();
        let count = match view.last_press {
            Some((t, p, c)) if now.duration_since(t) < Duration::from_millis(450) && p == pos => c % 3 + 1,
            _ => 1,
        };
        view.last_press = Some((now, pos, count));
        match count {
            1 if mods.shift => {
                let a = buf.primary().anchor;
                buf.set_sels(vec![Sel::new(a, pos)]);
                view.drag = Some(DragMode::Char);
            }
            1 if mods.alt || mods.command => {
                let mut sels = buf.sels.clone();
                sels.push(Sel::caret(pos));
                buf.set_sels(sels);
                buf.seal();
                view.drag = None;
            }
            1 => {
                buf.set_caret(pos);
                view.drag = Some(DragMode::Char);
            }
            2 => {
                let (a, b) = buf.word_at(pos);
                buf.set_sels(vec![Sel::new(a, b)]);
                view.drag = Some(DragMode::Word(a, b));
            }
            _ => {
                let line = buf.char_to_line(pos);
                buf.set_sels(vec![Sel::new(buf.line_start(line), buf.line_start(line + 1))]);
                view.drag = Some(DragMode::Line(line));
            }
        }
        return true;
    }
    if resp.dragged() {
        if let Some(mode) = view.drag {
            let pos = hit(ui, buf, view, g, pointer - origin);
            let anchor = buf.primary().anchor;
            let sel = match mode {
                DragMode::Char => Sel::new(anchor, pos),
                DragMode::Word(a, b) => {
                    let (wa, wb) = buf.word_at(pos);
                    if pos < a { Sel::new(b, wa) } else { Sel::new(a, wb.max(b)) }
                }
                DragMode::Line(l) => {
                    let pl = buf.char_to_line(pos);
                    if pl < l {
                        Sel::new(buf.line_start(l + 1), buf.line_start(pl))
                    } else {
                        Sel::new(buf.line_start(l), buf.line_start(pl + 1))
                    }
                }
            };
            if sel != buf.primary() {
                buf.set_sels(vec![sel]);
            }
            // Keep scrolling while dragging past the edges.
            let clip = ui.clip_rect();
            if !clip.contains(pointer) {
                view.reveal = true;
            }
            return true;
        }
    }
    if !ui.input(|i| i.pointer.primary_down()) {
        view.drag = None;
    }
    false
}

/// Char position under a content-space point.
fn hit(ui: &Ui, buf: &Buffer, view: &ViewState, g: &Geometry, at: egui::Vec2) -> usize {
    if at.y < 0.0 {
        return 0;
    }
    let row = (at.y / g.row_h).floor() as usize;
    if row >= view.metrics.total_rows {
        return buf.len_chars();
    }
    let (line, row_in) = view.metrics.line_at_row(row);
    let text = buf.line_text(line);
    let exp = layout::expand(&text, g.wrap.tab, false);
    let disp_len = exp.cols.last().copied().unwrap_or(0);
    let (a, b) = layout::row_range(&g.wrap.starts(&text), disp_len, row_in);
    let x = (at.x - TEXT_PAD).max(0.0);
    let (clip_a, clip_b) = clip_range(a, b, x - 200.0 * g.char_w, x + 200.0 * g.char_w, g.char_w);
    let galley = ui.painter().layout_job(plain_job(&exp.text, clip_a, clip_b, &g.font));
    let rel = x - (clip_a - a) as f32 * g.char_w;
    let mut d = clip_a + galley.cursor_from_pos(vec2(rel, g.row_h * 0.5)).index.0;
    // Clicking past the end of a wrapped row lands at its end.
    if g.wrap.cols.is_some() && d >= b && b < disp_len {
        d = b.saturating_sub(1).max(a);
    }
    buf.line_start(line) + layout::char_at_col(&text, d, g.wrap.tab)
}

/// The display range to lay out for a row, clipped horizontally for very
/// long unwrapped lines.
fn clip_range(a: usize, b: usize, x0: f32, x1: f32, char_w: f32) -> (usize, usize) {
    if b - a <= CLIP_COLS {
        return (a, b);
    }
    let from = a + ((x0 / char_w).floor().max(0.0) as usize).min(b - a);
    let to = (a + (x1 / char_w).ceil().max(0.0) as usize).clamp(from, b);
    (from, to)
}

fn plain_job(text: &str, a: usize, b: usize, font: &FontId) -> LayoutJob {
    let s: String = text.chars().skip(a).take(b - a).collect();
    LayoutJob::simple_singleline(s, font.clone(), Color32::WHITE)
}

// --------------------------------------------------------------- painting

/// A row's text with token colors, for display columns `a..b`.
#[allow(clippy::too_many_arguments)]
fn row_job(exp: &Expanded, src: &str, spans: &[Span], a: usize, b: usize, g: &Geometry, p: &M3Palette, show_ws: bool) -> LayoutJob {
    let mut job = LayoutJob::default();
    let disp: Vec<char> = exp.text.chars().collect();
    let ws_color = p.outline_variant;
    let mut span_i = 0;
    let mut piece = String::new();
    let mut piece_fmt: Option<(Color32, bool, bool)> = None;
    let flush = |job: &mut LayoutJob, piece: &mut String, fmt: Option<(Color32, bool, bool)>| {
        if let Some((color, italics, underline)) = fmt {
            if !piece.is_empty() {
                let mut f = TextFormat::simple(g.font.clone(), color);
                f.italics = italics;
                if underline {
                    f.underline = Stroke::new(1.0, color.gamma_multiply(0.6));
                }
                job.append(piece, 0.0, f);
                piece.clear();
            }
        }
    };
    for (i, c) in src.chars().enumerate() {
        let (c0, c1) = (exp.cols[i], exp.cols[i + 1]);
        if c1 <= a {
            continue;
        }
        if c0 >= b {
            break;
        }
        while span_i < spans.len() && spans[span_i].end <= i {
            span_i += 1;
        }
        let tok = spans.get(span_i).filter(|s| s.start <= i).map_or(Tok::Plain, |s| s.tok);
        let fmt = if show_ws && (c == ' ' || c == '\t') {
            (ws_color, false, false)
        } else {
            (tok.color(p), tok.italic(), tok.underline())
        };
        if piece_fmt != Some(fmt) {
            flush(&mut job, &mut piece, piece_fmt);
            piece_fmt = Some(fmt);
        }
        for ch in disp.iter().take(c1.min(b)).skip(c0.max(a)) {
            piece.push(*ch);
        }
    }
    flush(&mut job, &mut piece, piece_fmt);
    job.wrap.max_width = f32::INFINITY;
    job
}

#[allow(clippy::too_many_arguments)]
fn paint_rows(
    ui: &mut Ui,
    buf: &Buffer,
    hl: &mut Highlight,
    view: &ViewState,
    deco: &Decorations,
    opts: &EditorOptions,
    g: &Geometry,
    p: &M3Palette,
    origin: egui::Pos2,
    viewport: Rect,
    first_row: usize,
    last_row: usize,
    focused: bool,
) {
    let painter = ui.painter().clone();
    let rope = buf.rope();
    let primary = buf.primary();
    let primary_line = buf.char_to_line(primary.head);
    let bracket = buf.matching_bracket(primary.head);
    let sel_fill = if focused { p.primary.gamma_multiply(0.28) } else { p.on_surface.gamma_multiply(0.12) };
    let x0 = origin.x + TEXT_PAD;
    let (mut line, mut row_in) = view.metrics.line_at_row(first_row.min(view.metrics.total_rows.saturating_sub(1)));
    let mut row = first_row;
    let n_lines = buf.len_lines();
    // Matches overlapping the screen.
    let screen_start = buf.line_start(line);
    let mut match_i = deco.matches.partition_point(|m| m.1 < screen_start);

    while row < last_row && line < n_lines {
        let text = buf.line_text(line);
        let ls = buf.line_start(line);
        let le = ls + text.chars().count();
        let exp = layout::expand(&text, g.wrap.tab, opts.show_whitespace);
        let disp_len = *exp.cols.last().unwrap_or(&0);
        let spans = hl.line(rope, line);
        let starts = g.wrap.starts(&text);
        let rows_here = starts.len();

        while row_in < rows_here && row < last_row {
            let y = origin.y + row as f32 * g.row_h;
            let (a, b) = layout::row_range(&starts, disp_len, row_in);
            let (ca, cb) = if g.wrap.cols.is_none() {
                clip_range(a, b, viewport.min.x - TEXT_PAD - g.char_w * 4.0, viewport.max.x, g.char_w)
            } else {
                (a, b)
            };
            let job = row_job(&exp, &text, &spans, ca, cb, g, p, opts.show_whitespace);
            let galley = painter.layout_job(job);
            let gx = x0 + (ca - a) as f32 * g.char_w;
            let last_row_of_line = row_in + 1 == rows_here;
            // x of a display column in this row.
            let col_x = |d: usize| -> f32 {
                if d <= ca {
                    x0 + (d.saturating_sub(a)) as f32 * g.char_w
                } else if d >= cb {
                    gx + galley.rect.width() + (d - cb) as f32 * g.char_w
                } else {
                    gx + galley.pos_from_cursor(CCursor::new(d - ca)).min.x
                }
            };
            let row_rect = |xa: f32, xb: f32| Rect::from_min_max(pos2(xa, y), pos2(xb, y + g.row_h));

            if opts.highlight_line && line == primary_line && !buf.has_selection() && buf.sels.len() == 1 {
                painter.rect_filled(
                    Rect::from_min_max(pos2(viewport.min.x + origin.x, y), pos2(viewport.max.x + origin.x, y + g.row_h)),
                    CornerRadius::ZERO,
                    p.on_surface.gamma_multiply(if focused { 0.05 } else { 0.03 }),
                );
            }
            // Find matches.
            let mut mi = match_i;
            while let Some(&(ms, me)) = deco.matches.get(mi) {
                if ms > le {
                    break;
                }
                if me >= ls {
                    let ds = exp.cols[ms.max(ls) - ls];
                    let de = exp.cols[me.min(le) - ls];
                    let (s, e) = (ds.max(a), de.min(b));
                    if s < e || (s == e && ds == de) {
                        let current = deco.current == Some(mi);
                        let r = row_rect(col_x(s), col_x(e).max(col_x(s) + 2.0));
                        painter.rect_filled(r, CornerRadius::same(3), if current { p.tertiary.gamma_multiply(0.55) } else { p.tertiary_container.gamma_multiply(0.75) });
                        if current {
                            painter.rect_stroke(r, CornerRadius::same(3), Stroke::new(1.0, p.tertiary), egui::StrokeKind::Inside);
                        }
                    }
                }
                mi += 1;
            }
            // Selections.
            for s in &buf.sels {
                if s.is_empty() || s.end() < ls || s.start() > le {
                    continue;
                }
                let cs = s.start().max(ls) - ls;
                let ce = s.end().min(le) - ls;
                let (ds, de) = (exp.cols[cs].max(a), exp.cols[ce].min(b));
                let through_eol = s.end() > le && last_row_of_line;
                if ds < de || through_eol {
                    let xa = col_x(ds);
                    let mut xb = col_x(de.max(ds));
                    if through_eol {
                        xb += g.char_w * 0.6;
                    }
                    painter.rect_filled(row_rect(xa, xb), CornerRadius::same(2), sel_fill);
                }
            }
            // Matching brackets.
            if let Some((b1, b2)) = bracket {
                for bp in [b1, b2] {
                    if bp >= ls && bp < le {
                        let d = exp.cols[bp - ls];
                        if d >= a && d < b {
                            let r = row_rect(col_x(d), col_x(d + 1)).shrink2(vec2(0.0, 1.0));
                            painter.rect_stroke(r, CornerRadius::same(3), Stroke::new(1.0, p.primary.gamma_multiply(0.8)), egui::StrokeKind::Inside);
                        }
                    }
                }
            }
            let text_y = y + (g.row_h - galley.rect.height()) * 0.5;
            painter.galley(pos2(gx, text_y), galley.clone(), p.on_surface);
            // Carets.
            if focused {
                for s in &buf.sels {
                    if s.head < ls || s.head > le {
                        continue;
                    }
                    let d = exp.cols[s.head - ls];
                    let on_row = (d >= a && d < b) || (d == b && last_row_of_line) || (d == b && b == a);
                    if on_row {
                        let x = col_x(d);
                        painter.rect_filled(Rect::from_min_size(pos2(x - 1.0, y + 2.0), vec2(2.0, g.row_h - 4.0)), CornerRadius::same(1), p.primary);
                    }
                }
            }
            row += 1;
            row_in += 1;
        }
        // Skip matches that ended on this line.
        while deco.matches.get(match_i).is_some_and(|m| m.1 <= le) {
            match_i += 1;
        }
        line += 1;
        row_in = 0;
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_gutter(ui: &Ui, buf: &Buffer, view: &ViewState, opts: &EditorOptions, g: &Geometry, p: &M3Palette, rect: Rect, offset_y: f32) {
    if !opts.line_numbers {
        return;
    }
    let painter = ui.painter_at(rect);
    let primary_line = buf.char_to_line(buf.primary().head);
    let sel_lines: Vec<(usize, usize)> = buf.sels.iter().map(|s| (buf.char_to_line(s.start()), buf.char_to_line(s.end()))).collect();
    let first_row = (offset_y / g.row_h).floor().max(0.0) as usize;
    let last_row = ((offset_y + rect.height()) / g.row_h).ceil() as usize + 1;
    let (mut line, row_in) = view.metrics.line_at_row(first_row.min(view.metrics.total_rows.saturating_sub(1)));
    if row_in > 0 {
        line += 1;
    }
    let num_font = FontId::monospace(g.font.size * 0.88);
    while line < buf.len_lines() {
        let row = view.metrics.rows_before(line);
        if row > last_row {
            break;
        }
        let y = rect.min.y + row as f32 * g.row_h - offset_y;
        let active = line == primary_line || sel_lines.iter().any(|(a, b)| (*a..=*b).contains(&line));
        let color = if line == primary_line { p.primary } else if active { p.on_surface_variant } else { p.outline };
        painter.text(
            pos2(rect.max.x - GUTTER_PAD, y + g.row_h * 0.5),
            Align2::RIGHT_CENTER,
            (line + 1).to_string(),
            num_font.clone(),
            color,
        );
        line += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_track_rows_with_wrap() {
        let mut buf = Buffer::new("short\n0123456789012345678901234\n\tx");
        let mut m = Metrics::default();
        let wrap = Wrap { cols: Some(10), tab: 4 };
        m.update(&buf, wrap);
        assert_eq!(m.widths, vec![5, 25, 5]);
        assert_eq!(m.rows, vec![1, 3, 1]);
        assert_eq!(m.total_rows, 1 + 3 + 1);
        assert_eq!(m.line_at_row(0), (0, 0));
        assert_eq!(m.line_at_row(2), (1, 1));
        assert_eq!(m.line_at_row(4), (2, 0));
        assert_eq!(m.rows_before(2), 4);

        buf.take_changed();
        buf.set_caret(0);
        buf.insert(&"y".repeat(10));
        m.apply(buf.take_changed().unwrap(), &buf);
        m.update(&buf, wrap);
        assert_eq!(m.widths[0], 15);
        assert_eq!(m.total_rows, 2 + 3 + 1);

        let nowrap = Wrap { cols: None, tab: 4 };
        m.update(&buf, nowrap);
        assert_eq!(m.total_rows, 3);
        assert_eq!(m.max_width, 25);
        assert_eq!(m.line_at_row(1), (1, 0));
    }

    #[test]
    fn metrics_splice_matches_a_full_measure() {
        let mut buf = Buffer::new("a\nbb\nccc\ndddd\neeeee\n");
        let mut m = Metrics::default();
        let wrap = Wrap { cols: None, tab: 4 };
        m.update(&buf, wrap);
        buf.take_changed();
        // Insert lines, delete lines, edit with two cursors, undo.
        buf.set_caret(2);
        buf.insert("xx\nyyy\n");
        m.apply(buf.take_changed().unwrap(), &buf);
        buf.set_sels(vec![Sel::new(0, 9)]);
        buf.backspace(false);
        m.apply(buf.take_changed().unwrap(), &buf);
        buf.set_sels(vec![Sel::caret(0), Sel::caret(buf.len_chars())]);
        buf.insert("\tq\n");
        m.apply(buf.take_changed().unwrap(), &buf);
        buf.undo();
        m.apply(buf.take_changed().unwrap(), &buf);
        m.update(&buf, wrap);
        assert!(!m.full);
        assert_eq!(m.widths, measure(&buf, 0, buf.len_lines(), 4, None).0, "{:?}", buf.text());
    }

    #[test]
    fn long_lines_are_clipped_to_the_view() {
        assert_eq!(clip_range(0, 100, 0.0, 500.0, 10.0), (0, 100));
        assert_eq!(clip_range(0, 10_000, 1000.0, 2000.0, 10.0), (100, 200));
        assert_eq!(clip_range(0, 10_000, -50.0, 20.0, 10.0), (0, 2));
    }
}
#[cfg(test)]
mod bench {
    use super::*;

    #[test]
    #[ignore]
    fn metrics_speed() {
        let text = std::fs::read_to_string(std::env::var("BENCH_FILE").unwrap()).unwrap();
        let mut buf = Buffer::new(&text);
        let mut m = Metrics::default();
        let wrap = Wrap { cols: None, tab: 4 };
        m.update(&buf, wrap);
        buf.take_changed();
        let t = std::time::Instant::now();
        for _ in 0..10 {
            buf.set_caret(5);
            buf.insert("x");
            m.apply(buf.take_changed().unwrap(), &buf);
            m.update(&buf, wrap);
        }
        eprintln!("edit at top, no wrap: {:?} per keystroke", t.elapsed() / 10);
        let wrap = Wrap { cols: Some(80), tab: 4 };
        let t = std::time::Instant::now();
        for _ in 0..10 {
            buf.insert("x");
            m.apply(buf.take_changed().unwrap(), &buf);
            m.update(&buf, wrap);
        }
        eprintln!("edit at top, wrap: {:?} per keystroke", t.elapsed() / 10);
        let t = std::time::Instant::now();
        let s = buf.text();
        eprintln!("to_string: {:?} ({} bytes)", t.elapsed(), s.len());
    }
}
