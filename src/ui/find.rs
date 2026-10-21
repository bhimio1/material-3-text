//! The find / replace bar above the editor.

use crate::app::{select_match, App};
use crate::ui_ext;
use egui::{vec2, CornerRadius, Frame, Key, Margin, Ui};
use material_3_ui::icons::icons::*;
use material_3_ui::widgets::{self, ButtonKind};

pub fn show(app: &mut App, ui: &mut Ui) {
    let p = app.theme.palette().clone();
    let Some(doc_id) = app.doc().map(|d| d.id) else { return };
    let field_id = egui::Id::unique("find-field");
    let replace_id = egui::Id::unique("replace-field");
    let mut close = false;
    let mut step: isize = 0;
    let mut replace_one = false;
    let mut replace_all = false;

    ui.add_space(4.0);
    Frame::new()
        .fill(p.surface_container_high)
        .corner_radius(CornerRadius::same(18))
        .inner_margin(Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let expand = if app.find.replace_open { ICON_EXPAND_MORE } else { ICON_CHEVRON_RIGHT };
                if widgets::icon_button_sized(ui, expand, "Toggle replace (Ctrl+H)", true, 30.0, false).clicked() {
                    app.find.replace_open = !app.find.replace_open;
                }
                let w = (ui.available_width() - 330.0).clamp(160.0, 420.0);
                let resp = ui_ext::field(ui, &mut app.find.query.text, "Find", w, field_id);
                if std::mem::take(&mut app.find.focus) {
                    resp.request_focus();
                    if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), field_id) {
                        let n = app.find.query.text.chars().count();
                        state.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(n))));
                        state.store(ui.ctx(), field_id);
                    }
                }
                if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    step = if ui.input(|i| i.modifiers.shift) { -1 } else { 1 };
                    resp.request_focus();
                }
                if (resp.has_focus() || resp.lost_focus()) && ui.input(|i| i.key_pressed(Key::Escape)) {
                    close = true;
                }
                let q = &mut app.find.query;
                for (icon, tip, on) in [
                    (ICON_MATCH_CASE, "Match case", &mut q.case),
                    (ICON_MATCH_WORD, "Whole word", &mut q.whole_word),
                    (ICON_REGULAR_EXPRESSION, "Regular expression", &mut q.regex),
                ] {
                    if widgets::icon_button_sized(ui, icon, tip, true, 30.0, *on).clicked() {
                        *on = !*on;
                    }
                }
                // Count, and where we are.
                let query = app.find.query.clone();
                let doc = app.docs.iter_mut().find(|d| d.id == doc_id).expect("active");
                let cache = doc.find_matches(&query);
                let label = if let Some(e) = &cache.error {
                    e.clone()
                } else if query.text.is_empty() {
                    String::new()
                } else if cache.matches.is_empty() {
                    "No results".into()
                } else {
                    let total = format!("{}{}", cache.matches.len(), if cache.truncated { "+" } else { "" });
                    match app.find.current {
                        Some(i) if i < cache.matches.len() => format!("{} of {total}", i + 1),
                        _ => format!("{total} found"),
                    }
                };
                let color = if cache.error.is_some() { p.error } else { p.on_surface_variant };
                ui.add_sized(vec2(84.0, 24.0), egui::Label::new(egui::RichText::new(label).size(12.5).color(color)).truncate());
                if widgets::icon_button_sized(ui, ICON_ARROW_UPWARD, "Previous (Shift+Enter)", true, 30.0, false).clicked() {
                    step = -1;
                }
                if widgets::icon_button_sized(ui, ICON_ARROW_DOWNWARD, "Next (Enter)", true, 30.0, false).clicked() {
                    step = 1;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if widgets::icon_button_sized(ui, ICON_CLOSE, "Close (Esc)", true, 30.0, false).clicked() {
                        close = true;
                    }
                });
            });
            if app.find.replace_open {
                ui.horizontal(|ui| {
                    ui.add_space(34.0);
                    let w = (ui.available_width() - 330.0).clamp(160.0, 420.0);
                    let resp = ui_ext::field(ui, &mut app.find.replace, if app.find.query.regex { "Replace ($1 for groups)" } else { "Replace" }, w, replace_id);
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        replace_one = true;
                        resp.request_focus();
                    }
                    if widgets::button(ui, ButtonKind::Tonal, None, "Replace").clicked() {
                        replace_one = true;
                    }
                    if widgets::button(ui, ButtonKind::Text, None, "Replace all").clicked() {
                        replace_all = true;
                    }
                });
            }
        });

    // Typing in the find field jumps to the first match from the caret.
    let key = (doc_id, app.find.query.clone());
    if app.find.last.as_ref() != Some(&key) {
        app.find.last = Some(key);
        app.find.current = None;
        if !app.find.query.text.is_empty() && step == 0 {
            if let Some(doc) = app.doc_mut_active() {
                let start = doc.buffer.primary().start();
                doc.buffer.set_sels(vec![crate::buffer::Sel::caret(start)]);
            }
            step = 1;
        }
    }
    if step != 0 {
        app.find_step(step);
    }
    let query = app.find.query.clone();
    let with = app.find.replace.clone();
    if replace_one {
        let current = app.find.current;
        if let Some(doc) = app.doc_mut_active() {
            match current {
                Some(i) if doc.replace_match(&query, i, &with) => {
                    // Move on to the next match after the replacement.
                    let caret = doc.buffer.primary().head;
                    let next = doc.find_matches(&query).matches.iter().position(|m| m.0 >= caret);
                    let all = doc.find_matches(&query).matches.clone();
                    let next = next.or(if all.is_empty() { None } else { Some(0) });
                    if let Some(n) = next {
                        select_match(doc, all[n]);
                    }
                    app.find.current = next;
                }
                _ => app.find_step(1),
            }
        }
    }
    if replace_all {
        if let Some(doc) = app.doc_mut_active() {
            let n = doc.replace_all(&query, &with);
            app.find.current = None;
            app.toasts.success(format!("Replaced {n} {}", if n == 1 { "match" } else { "matches" }));
        }
    }
    if close {
        app.find.open = false;
        app.find.current = None;
        if let Some(d) = app.doc_mut_active() {
            d.view.focus = true;
        }
    }
    ui.add_space(4.0);
}
