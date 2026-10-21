//! Window layout: sidebar, tabs, find bar, editor (+ Markdown preview),
//! status bar, and the overlays.

mod find;
mod overlays;
mod sidebar;
mod status;
mod tabs;

use crate::app::{Action, App};
use crate::document::External;
use crate::editor::{self, Decorations, EditorOptions};
use crate::syntax;
use crate::ui_ext;
use egui::{pos2, vec2, CornerRadius, Frame, Margin, Rect, Ui, UiBuilder};
use material_3_ui::common;
use material_3_ui::icons::icons::*;
use material_3_ui::widgets::{self, ButtonKind};
use std::time::Instant;

pub fn draw(app: &mut App, ui: &mut Ui) {
    let p = app.theme.palette().clone();
    let ctx = ui.ctx().clone();
    let surface = Frame::new().fill(p.surface);
    if app.config.show_sidebar {
        egui::Panel::left("sidebar")
            .default_size(260.0)
            .size_range(180.0..=560.0)
            .resizable(true)
            .show_separator_line(false)
            .frame(surface.inner_margin(Margin { left: 12, right: 4, top: 12, bottom: 12 }))
            .show(ui, |ui| sidebar::show(app, ui));
    }
    let left = if app.config.show_sidebar { 4 } else { 12 };
    egui::CentralPanel::default()
        .frame(surface.inner_margin(Margin { left, right: 12, top: 12, bottom: 12 }))
        .show(ui, |ui| {
            Frame::new()
                .fill(p.surface_container_low)
                .corner_radius(CornerRadius::same(24))
                .show(ui, |ui| {
                    ui.set_min_size(ui.available_size());
                    main_area(app, ui);
                });
        });
    overlays::show(app, &ctx);
    let fired = app.toasts.show(&ctx, &p, 52.0);
    app.actions.extend(fired);
}

fn main_area(app: &mut App, ui: &mut Ui) {
    let full = ui.available_rect_before_wrap();
    let tabs_rect = Rect::from_min_size(full.min, vec2(full.width(), 52.0));
    ui.scope_builder(UiBuilder::new().max_rect(tabs_rect.shrink2(vec2(12.0, 8.0))), |ui| tabs::show(app, ui));
    if app.docs.is_empty() {
        let body = Rect::from_min_max(pos2(full.min.x, tabs_rect.max.y), full.max);
        ui.scope_builder(UiBuilder::new().max_rect(body), |ui| welcome(app, ui));
        return;
    }
    let status_rect = Rect::from_min_max(pos2(full.min.x + 12.0, full.max.y - 34.0), pos2(full.max.x - 12.0, full.max.y - 4.0));
    let mut body = Rect::from_min_max(pos2(full.min.x, tabs_rect.max.y), pos2(full.max.x, status_rect.min.y - 2.0));

    // Banners and the find bar take room from the top of the body.
    let top = ui.scope_builder(UiBuilder::new().max_rect(body.shrink2(vec2(12.0, 0.0))), |ui| {
        banners(app, ui);
        if app.find.open {
            find::show(app, ui);
        }
        ui.min_rect().height()
    });
    body.min.y += top.inner;
    ui.scope_builder(UiBuilder::new().max_rect(body), |ui| editor_area(app, ui));
    ui.scope_builder(UiBuilder::new().max_rect(status_rect), |ui| status::show(app, ui));
}

fn banners(app: &mut App, ui: &mut Ui) {
    let Some(doc) = app.doc() else { return };
    let id = doc.id;
    match doc.external {
        Some(External::Changed) => {
            let dirty = doc.buffer.is_dirty();
            let text = if dirty {
                "This file changed on disk, and you have unsaved changes."
            } else {
                "This file changed on disk."
            };
            common::banner(ui, ICON_SYNC_PROBLEM, text, false, |ui| {
                if widgets::button(ui, ButtonKind::Filled, None, "Reload").clicked() {
                    app.actions.push(Action::Reload(id));
                }
                if widgets::button(ui, ButtonKind::Text, None, "Keep mine").clicked() {
                    app.actions.push(Action::KeepMine(id));
                }
            });
        }
        Some(External::Deleted) => {
            common::banner(ui, ICON_WARNING, "This file was deleted or moved. Saving will create it again.", true, |ui| {
                if widgets::button(ui, ButtonKind::Text, None, "Save").clicked() {
                    app.actions.push(Action::Save(id));
                }
            });
        }
        None => {}
    }
}

fn editor_area(app: &mut App, ui: &mut Ui) {
    let p = app.theme.palette().clone();
    let settings = app.config.editor.clone();
    let find_open = app.find.open && !app.find.query.text.is_empty();
    let query = app.find.query.clone();
    let current = app.find.current;
    let overlay = app.dialog.is_some() || app.quick.is_some() || app.goto.is_some();
    if app.had_overlay && !overlay {
        if let Some(d) = app.docs.get_mut(app.active) {
            d.view.focus = true;
        }
    }
    app.had_overlay = overlay;
    let Some(doc) = app.docs.get_mut(app.active) else { return };
    let opts = EditorOptions {
        font_size: settings.font_size,
        tab: if doc.buffer.indent.tabs { doc.buffer.indent.width.max(1) } else { settings.tab_size.max(1) },
        wrap: settings.word_wrap,
        show_whitespace: settings.show_whitespace,
        line_numbers: settings.line_numbers,
        highlight_line: settings.highlight_line,
        colon_indents: syntax::colon_indents(doc.highlight.name()),
        comment: syntax::comment_style(doc.highlight.name()),
    };
    let matches = if find_open { doc.find_matches(&query).matches.clone() } else { Vec::new() };
    let deco = Decorations { matches: &matches, current: current.filter(|&i| i < matches.len()) };

    let rect = ui.available_rect_before_wrap();
    let show_preview = doc.preview && doc.is_markdown();
    let (editor_rect, preview_rect) = if show_preview {
        let mid = rect.center().x;
        (Rect::from_min_max(rect.min, pos2(mid - 4.0, rect.max.y)), Some(Rect::from_min_max(pos2(mid + 4.0, rect.min.y + 4.0), pos2(rect.max.x - 12.0, rect.max.y - 4.0))))
    } else {
        (rect, None)
    };
    let id = egui::Id::unique(("editor", doc.id));
    // Dialogs and pickers own the keyboard while they're open.
    if overlay {
        ui.memory_mut(|m| m.surrender_focus(id));
    }
    let out = ui
        .scope_builder(UiBuilder::new().max_rect(editor_rect), |ui| {
            editor::show(ui, id, &mut doc.buffer, &mut doc.highlight, &mut doc.view, &deco, &opts, &p)
        })
        .inner;
    if out.changed {
        doc.last_edit = Some(Instant::now());
        if app.find.current.is_some() {
            app.find.current = None;
        }
    }
    // The parser thread asks for repaints as it goes.
    editor::pump_highlight(ui.ctx(), &doc.buffer, &mut doc.highlight);
    if let Some(r) = preview_rect {
        let text = doc.buffer.text();
        let mut links = Vec::new();
        ui.scope_builder(UiBuilder::new().max_rect(r), |ui| {
            Frame::new().fill(p.surface_container).corner_radius(CornerRadius::same(18)).show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                egui::ScrollArea::vertical().id_salt(("preview", doc.id)).auto_shrink([false, false]).show(ui, |ui| {
                    Frame::new().inner_margin(Margin::symmetric(24, 18)).show(ui, |ui| {
                        crate::markdown::show(ui, &text, 14.5, &mut |url| links.push(url));
                    });
                });
            });
        });
        app.actions.extend(links.into_iter().map(Action::OpenLink));
    }
}

fn welcome(app: &mut App, ui: &mut Ui) {
    let p = app.theme.palette().clone();
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.add_space(ui.available_height().min(800.0) * 0.12);
        ui.vertical_centered(|ui| {
            ui.set_max_width(460.0);
            widgets::icon(ui, ICON_EDIT_NOTE, 56.0, p.primary);
            ui.add_space(8.0);
            ui.label(widgets::title_text("Material 3 Text", 28.0).color(p.on_surface));
            ui.label(ui_ext::dim("Open a file or a folder to get started.", 14.0, &p));
            ui.add_space(18.0);
            ui.horizontal(|ui| {
                let w = 3.0 * 140.0 + 16.0;
                ui.add_space(((ui.available_width() - w) / 2.0).max(0.0));
                if widgets::button(ui, ButtonKind::Filled, Some(ICON_NOTE_ADD), "New file").clicked() {
                    app.actions.push(Action::New);
                }
                if widgets::button(ui, ButtonKind::Tonal, Some(ICON_FILE_OPEN), "Open file").clicked() {
                    app.actions.push(Action::OpenDialog);
                }
                if widgets::button(ui, ButtonKind::Tonal, Some(ICON_FOLDER_OPEN), "Open folder").clicked() {
                    app.actions.push(Action::OpenFolderDialog);
                }
            });
            ui.add_space(24.0);
        });
        ui.vertical_centered(|ui| {
            ui.set_max_width(520.0);
            recent_lists(app, ui, 8);
        });
    });
}

/// Recent files and folders as clickable rows.
pub(crate) fn recent_lists(app: &mut App, ui: &mut Ui, max: usize) {
    let files: Vec<_> = app.recent.files.iter().filter(|f| f.exists()).take(max).cloned().collect();
    let folders: Vec<_> = app.recent.folders.iter().filter(|f| f.is_dir()).take(max).cloned().collect();
    for (title, list, folder) in [("Recent files", files, false), ("Recent folders", folders, true)] {
        if list.is_empty() {
            continue;
        }
        ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
            widgets::section_header(ui, title);
            for path in list {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let dir = path.parent().map(material_3_ui::sys::paths::tilde).unwrap_or_default();
                let icon = ui_ext::file_icon(&name, folder, false);
                let resp = common::list_row(ui, icon, false, &name, &dir, |_| {});
                if resp.clicked() {
                    app.actions.push(if folder {
                        Action::OpenFolder(path.clone())
                    } else {
                        Action::Open(crate::cli::OpenRequest { path: path.clone(), line: None, col: None })
                    });
                }
            }
        });
    }
}
