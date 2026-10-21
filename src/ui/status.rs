//! The status bar: position and selection, indentation, encoding, line
//! endings and language, each with a menu to change it.

use crate::app::{Action, App};
use crate::buffer::Indent;
use crate::encoding::{self, LineEnding};
use crate::ui_ext;
use egui::{Layout, Ui};
use material_3_ui::icons::icons::*;
use material_3_ui::widgets;

pub fn show(app: &mut App, ui: &mut Ui) {
    let p = app.theme.palette().clone();
    let Some(doc) = app.doc() else { return };
    let id = doc.id;
    let b = &doc.buffer;
    let s = b.primary();
    let (line, col) = b.line_col(s.head);
    let col_display = crate::layout::display_col(&b.line_text(line), col, b.indent.width.max(1)) + 1;
    let mut pos = format!("Ln {}, Col {}", line + 1, col_display);
    if b.sels.len() > 1 {
        pos.push_str(&format!("  ·  {} cursors", b.sels.len()));
    } else if !s.is_empty() {
        let chars = s.end() - s.start();
        let lines = b.char_to_line(s.end()) - b.char_to_line(s.start()) + 1;
        pos.push_str(&format!("  ·  {chars} selected{}", if lines > 1 { format!(" ({lines} lines)") } else { String::new() }));
    }
    let indent = b.indent;
    let enc = doc.encoding;
    let le = doc.line_ending;
    let lang = doc.highlight.name().to_string();
    let readonly = doc.readonly;
    let highlighting = doc.highlight.frontier() + crate::syntax::CHUNK < doc.view.visible.1 && lang != "Plain Text";

    ui.horizontal_centered(|ui| {
        if widgets::chip(ui, &pos, "Go to line (Ctrl+G)").clicked() {
            app.goto = Some(String::new());
        }
        if readonly {
            widgets::icon(ui, ICON_ADMIN_PANEL_SETTINGS, 16.0, p.tertiary).on_hover_text("You can't write this file; saving asks for an administrator password");
        }
        if highlighting {
            ui.label(ui_ext::dim("Highlighting…", 12.0, &p));
        }
        ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
            // Language.
            let resp = widgets::chip(ui, &lang, "Language");
            egui::Popup::menu(&resp).width(260.0).show(|ui| {
                let fid = egui::Id::unique("lang-filter");
                let f = widgets::field(ui, &mut app.lang_filter, "Filter languages", 240.0, fid);
                if !f.has_focus() && app.lang_filter.is_empty() {
                    f.request_focus();
                }
                egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                    let filter = app.lang_filter.to_lowercase();
                    for name in crate::syntax::language_names() {
                        if !filter.is_empty() && !name.to_lowercase().contains(&filter) {
                            continue;
                        }
                        if widgets::menu_item(ui, None, name, "", name == lang) {
                            app.actions.push(Action::SetLanguage(id, name.to_string()));
                            app.lang_filter.clear();
                        }
                    }
                });
            });
            // Line endings.
            let resp = widgets::chip(ui, le.label(), "Line endings");
            egui::Popup::menu(&resp).width(200.0).show(|ui| {
                for l in LineEnding::ALL {
                    let name = match l {
                        LineEnding::Lf => "LF (Linux, macOS)",
                        LineEnding::CrLf => "CRLF (Windows)",
                        LineEnding::Cr => "CR (classic Mac)",
                    };
                    if widgets::menu_item(ui, None, name, "", l == le) {
                        app.actions.push(Action::SetLineEnding(id, l));
                    }
                }
            });
            // Encoding.
            let resp = widgets::chip(ui, &enc.label(), "Encoding");
            egui::Popup::menu(&resp).width(300.0).show(|ui| {
                ui.label(ui_ext::dim("Save with encoding", 12.0, &p));
                egui::ScrollArea::vertical().id_salt("enc-save").max_height(220.0).show(ui, |ui| {
                    for (e, name) in encoding::choices() {
                        if widgets::menu_item(ui, None, name, "", e == enc) {
                            app.actions.push(Action::SetEncoding(id, e));
                        }
                    }
                });
                ui.separator();
                ui.label(ui_ext::dim("Reopen with encoding", 12.0, &p));
                egui::ScrollArea::vertical().id_salt("enc-reopen").max_height(180.0).show(ui, |ui| {
                    let mut seen = Vec::new();
                    for (e, name) in encoding::choices() {
                        if seen.contains(&e.encoding) {
                            continue;
                        }
                        seen.push(e.encoding);
                        if widgets::menu_item(ui, Some(ICON_REFRESH), name.trim_end_matches(" with BOM"), "", false) {
                            app.actions.push(Action::Reinterpret(id, e.encoding));
                        }
                    }
                });
            });
            // Indentation.
            let label = if indent.tabs { format!("Tabs: {}", indent.width) } else { format!("Spaces: {}", indent.width) };
            let resp = widgets::chip(ui, &label, "Indentation");
            egui::Popup::menu(&resp).width(240.0).show(|ui| {
                for (tabs, width) in [(false, 2), (false, 4), (false, 8), (true, 4), (true, 8)] {
                    let i = Indent { tabs, width };
                    let name = if tabs { format!("Tabs (width {width})") } else { format!("Spaces: {width}") };
                    if widgets::menu_item(ui, None, &name, "", i == indent) {
                        app.actions.push(Action::SetIndent(id, i));
                    }
                }
                ui.separator();
                let to_tabs = Indent { tabs: true, width: indent.width };
                let to_spaces = Indent { tabs: false, width: indent.width };
                if widgets::menu_item(ui, Some(ICON_FORMAT_INDENT_INCREASE), "Convert indentation to tabs", "", false) {
                    app.actions.push(Action::ConvertIndent(id, to_tabs));
                }
                if widgets::menu_item(ui, Some(ICON_FORMAT_INDENT_INCREASE), "Convert indentation to spaces", "", false) {
                    app.actions.push(Action::ConvertIndent(id, to_spaces));
                }
            });
        });
    });
}
