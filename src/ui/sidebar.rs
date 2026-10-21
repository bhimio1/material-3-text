//! The sidebar: the folder's file tree, or search across the folder.

use crate::app::{Action, App, Dialog, SideMode};
use crate::cli::OpenRequest;
use crate::ui_ext;
use egui::{pos2, vec2, Align2, CornerRadius, FontId, Key, Sense, Ui};
use material_3_ui::icons::icons::*;
use material_3_ui::widgets::{self, ButtonKind};
use std::path::{Path, PathBuf};

const ROW_H: f32 = 28.0;

pub fn show(app: &mut App, ui: &mut Ui) {
    let p = app.theme.palette().clone();
    ui.horizontal(|ui| {
        widgets::icon(ui, ICON_EDIT_NOTE, 26.0, p.primary);
        ui.label(widgets::title_text("Text", 20.0).color(p.on_surface));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if widgets::icon_button_sized(ui, ICON_LEFT_PANEL_CLOSE, "Hide sidebar (Ctrl+B)", true, 32.0, false).clicked() {
                app.config.show_sidebar = false;
                app.save_config();
            }
        });
    });
    ui.add_space(8.0);
    if app.workspace.is_none() {
        no_folder(app, ui);
        return;
    }
    widgets::segmented(ui, &mut app.side, &[(SideMode::Files, ICON_FOLDER, "Files"), (SideMode::Search, ICON_SEARCH, "Search")]);
    ui.add_space(6.0);
    match app.side {
        SideMode::Files => files(app, ui),
        SideMode::Search => search(app, ui),
    }
}

fn no_folder(app: &mut App, ui: &mut Ui) {
    let p = app.theme.palette().clone();
    if widgets::button(ui, ButtonKind::Tonal, Some(ICON_FOLDER_OPEN), "Open folder").clicked() {
        app.actions.push(Action::OpenFolderDialog);
    }
    ui.add_space(6.0);
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        widgets::section_header(ui, "Open");
        let active = app.active;
        let rows: Vec<(usize, String, bool)> = app.docs.iter().enumerate().map(|(i, d)| (i, d.title(), d.is_dirty())).collect();
        for (i, title, dirty) in rows {
            let resp = tree_row(ui, 0, widgets::file_icon(&title, false, false), &title, i == active, dirty, None);
            if resp.clicked() {
                app.actions.push(Action::Activate(i));
            }
        }
        if app.docs.is_empty() {
            ui.label(ui_ext::dim("No open files", 13.0, &p));
        }
        super::recent_lists(app, ui, 6);
    });
}

/// One row of the tree: indent, optional chevron, icon, name.
fn tree_row(ui: &mut Ui, depth: usize, icon: material_3_ui::icons::MaterialIcon, name: &str, selected: bool, dirty: bool, chevron: Option<bool>) -> egui::Response {
    let p = widgets::palette(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
    let fill = if selected {
        p.secondary_container
    } else if resp.hovered() {
        p.on_surface.gamma_multiply(0.07)
    } else {
        egui::Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, CornerRadius::same(14), fill);
    let fg = if selected { p.on_secondary_container } else { p.on_surface };
    let mut x = rect.left() + 8.0 + depth as f32 * 14.0;
    if let Some(open) = chevron {
        widgets::paint_icon(ui, if open { ICON_EXPAND_MORE } else { ICON_CHEVRON_RIGHT }, pos2(x + 8.0, rect.center().y), 18.0, p.on_surface_variant);
    }
    x += 18.0;
    widgets::paint_icon(ui, icon, pos2(x + 9.0, rect.center().y), 18.0, if chevron.is_some() { p.primary } else { p.on_surface_variant });
    x += 24.0;
    let max = rect.right() - x - if dirty { 20.0 } else { 6.0 };
    let g = widgets::truncated_galley(ui, name, FontId::proportional(13.5), fg, max);
    ui.painter().galley(pos2(x, rect.center().y - g.size().y / 2.0), g, fg);
    if dirty {
        ui.painter().circle_filled(pos2(rect.right() - 12.0, rect.center().y), 4.0, p.primary);
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn files(app: &mut App, ui: &mut Ui) {
    let p = app.theme.palette().clone();
    let ws = app.workspace.as_ref().expect("checked");
    let root = ws.root.clone();
    let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| root.display().to_string());
    ui.horizontal(|ui| {
        ui.label(widgets::title_text(name.to_uppercase(), 12.0).color(p.on_surface_variant)).on_hover_text(material_3_ui::sys::paths::tilde(&root));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let more = widgets::icon_button_sized(ui, ICON_MORE_VERT, "More", true, 28.0, false);
            egui::Popup::menu(&more).width(240.0).show(|ui| {
                let e = &mut app.config.editor;
                let mut changed = false;
                if widgets::menu_item(ui, Some(ICON_VISIBILITY), "Show hidden files", "", e.show_hidden_files) {
                    e.show_hidden_files = !e.show_hidden_files;
                    changed = true;
                }
                if widgets::menu_item(ui, Some(ICON_VISIBILITY), "Show ignored files", "", e.show_ignored_files) {
                    e.show_ignored_files = !e.show_ignored_files;
                    changed = true;
                }
                if changed {
                    app.actions.push(Action::RefreshTree);
                    app.save_config();
                }
                ui.separator();
                if widgets::menu_item(ui, Some(ICON_FOLDER), "Show in Files", "", false) {
                    app.actions.push(Action::ShowInFiles(root.clone()));
                }
                if widgets::menu_item(ui, Some(ICON_FOLDER_OPEN), "Open another folder…", "Ctrl+Shift+O", false) {
                    app.actions.push(Action::OpenFolderDialog);
                }
                if widgets::menu_item(ui, Some(ICON_CLOSE), "Close folder", "", false) {
                    app.actions.push(Action::CloseFolder);
                }
            });
            if widgets::icon_button_sized(ui, ICON_REFRESH, "Refresh", true, 28.0, false).clicked() {
                app.actions.push(Action::RefreshTree);
            }
            if widgets::icon_button_sized(ui, ICON_CREATE_NEW_FOLDER, "New folder", true, 28.0, false).clicked() {
                app.actions.push(Action::NewEntry { dir: root.clone(), folder: true });
            }
            if widgets::icon_button_sized(ui, ICON_NOTE_ADD, "New file", true, 28.0, false).clicked() {
                app.actions.push(Action::NewEntry { dir: root.clone(), folder: false });
            }
        });
    });
    ui.add_space(4.0);

    let active_path = app.doc().and_then(|d| d.path.clone());
    let dirty: Vec<PathBuf> = app.docs.iter().filter(|d| d.is_dirty()).filter_map(|d| d.path.clone()).collect();
    let ws = app.workspace.as_mut().expect("checked");
    let rows: Vec<(usize, PathBuf, String, bool, bool)> = ws
        .tree
        .rows()
        .into_iter()
        .map(|(depth, n)| (depth, n.path.clone(), n.name.clone(), n.is_dir, n.expanded))
        .collect();
    let selected = ws.selected.clone();
    let mut toggle: Option<PathBuf> = None;
    let mut open: Option<PathBuf> = None;
    let mut menu_actions = Vec::new();
    let mut space_resp = None;
    egui::ScrollArea::vertical().id_salt("tree").auto_shrink([false, false]).show_rows(ui, ROW_H, rows.len().max(1), |ui, range| {
        ui.spacing_mut().item_spacing.y = 0.0;
        if rows.is_empty() {
            ui.label(ui_ext::dim("This folder is empty", 13.0, &p));
            return;
        }
        for (depth, path, name, is_dir, expanded) in &rows[range] {
            let is_sel = selected.as_ref() == Some(path) || (!is_dir && active_path.as_ref() == Some(path));
            let icon = widgets::file_icon(name, *is_dir, *expanded);
            let resp = tree_row(ui, *depth, icon, name, is_sel, dirty.contains(path), is_dir.then_some(*expanded));
            if resp.clicked() {
                if *is_dir {
                    toggle = Some(path.clone());
                } else {
                    open = Some(path.clone());
                }
            }
            resp.context_menu(|ui| entry_menu(ui, path, *is_dir, &mut menu_actions));
        }
        // Right-click on the empty space below: new file in the root.
        let rest = ui.available_rect_before_wrap();
        if rest.height() > 0.0 {
            space_resp = Some(ui.interact(rest, egui::Id::unique("tree-space"), Sense::click()));
        }
    });
    if let Some(r) = space_resp {
        r.context_menu(|ui| {
            if widgets::menu_item(ui, Some(ICON_NOTE_ADD), "New file", "", false) {
                menu_actions.push(Action::NewEntry { dir: root.clone(), folder: false });
            }
            if widgets::menu_item(ui, Some(ICON_CREATE_NEW_FOLDER), "New folder", "", false) {
                menu_actions.push(Action::NewEntry { dir: root.clone(), folder: true });
            }
        });
    }
    if let Some(t) = toggle {
        ws.toggle(&t);
        ws.selected = Some(t);
    }
    if let Some(o) = open {
        ws.selected = Some(o.clone());
        app.actions.push(Action::Open(OpenRequest { path: o, line: None, col: None }));
    }
    app.actions.extend(menu_actions);
}

fn entry_menu(ui: &mut Ui, path: &Path, is_dir: bool, out: &mut Vec<Action>) {
    let dir = if is_dir { path.to_path_buf() } else { path.parent().map(Path::to_path_buf).unwrap_or_default() };
    if widgets::menu_item(ui, Some(ICON_NOTE_ADD), "New file", "", false) {
        out.push(Action::NewEntry { dir: dir.clone(), folder: false });
    }
    if widgets::menu_item(ui, Some(ICON_CREATE_NEW_FOLDER), "New folder", "", false) {
        out.push(Action::NewEntry { dir, folder: true });
    }
    ui.separator();
    if widgets::menu_item(ui, Some(ICON_DRIVE_FILE_RENAME_OUTLINE), "Rename", "", false) {
        out.push(Action::Rename(path.to_path_buf()));
    }
    if widgets::menu_item(ui, Some(ICON_DELETE), "Move to trash", "", false) {
        out.push(Action::Delete(path.to_path_buf()));
    }
    ui.separator();
    if widgets::menu_item(ui, Some(ICON_CONTENT_COPY), "Copy path", "", false) {
        out.push(Action::CopyPath(path.to_path_buf()));
    }
    if widgets::menu_item(ui, Some(ICON_FOLDER), "Show in Files", "", false) {
        out.push(Action::ShowInFiles(path.to_path_buf()));
    }
}

fn search(app: &mut App, ui: &mut Ui) {
    let p = app.theme.palette().clone();
    let w = ui.available_width();
    let fid = egui::Id::unique("folder-search-field");
    let resp = widgets::field(ui, &mut app.folder.query.text, "Search in folder", w, fid);
    if std::mem::take(&mut app.folder.focus) {
        resp.request_focus();
    }
    let mut run = resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
    ui.horizontal(|ui| {
        let q = &mut app.folder.query;
        for (icon, tip, on) in [
            (ICON_MATCH_CASE, "Match case", &mut q.case),
            (ICON_MATCH_WORD, "Whole word", &mut q.whole_word),
            (ICON_REGULAR_EXPRESSION, "Regular expression", &mut q.regex),
        ] {
            if widgets::icon_button_sized(ui, icon, tip, true, 30.0, *on).clicked() {
                *on = !*on;
                run = true;
            }
        }
        if widgets::icon_button_sized(ui, ICON_FIND_REPLACE, "Replace", true, 30.0, app.folder.show_replace).clicked() {
            app.folder.show_replace = !app.folder.show_replace;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if widgets::icon_button_sized(ui, ICON_SEARCH, "Search (Enter)", true, 30.0, false).clicked() {
                run = true;
            }
        });
    });
    if app.folder.show_replace {
        widgets::field(ui, &mut app.folder.replace, "Replace with", w, egui::Id::unique("folder-replace-field"));
        let can = app.folder.run.as_ref().is_some_and(|r| r.done && r.total > 0);
        ui.add_space(4.0);
        if widgets::button_enabled(ui, ButtonKind::Tonal, Some(ICON_FIND_REPLACE), "Replace all", can).clicked() {
            app.dialog = Some(Dialog::ReplaceInFolder);
        }
    }
    if run {
        app.actions.push(Action::FolderSearch);
    }
    if let Some(e) = &app.folder.error {
        ui.label(egui::RichText::new(e).color(p.error).size(12.5));
    }
    let Some(results) = &app.folder.run else {
        ui.add_space(8.0);
        ui.label(ui_ext::dim("Searches every file in the folder, skipping what .gitignore excludes.", 12.5, &p));
        return;
    };
    let status = if !results.done {
        format!("Searching… {} matches", results.total)
    } else if results.total == 0 {
        format!("No results in {} files", results.searched)
    } else {
        format!(
            "{}{} matches in {} files",
            results.total,
            if results.truncated { "+" } else { "" },
            results.results.len()
        )
    };
    ui.add_space(4.0);
    ui.label(ui_ext::dim(status, 12.5, &p));
    ui.add_space(4.0);
    let root = app.workspace.as_ref().map(|w| w.root.clone()).unwrap_or_default();
    let mut open: Option<OpenRequest> = None;
    let mut toggle: Option<PathBuf> = None;
    egui::ScrollArea::vertical().id_salt("folder-results").auto_shrink([false, false]).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for file in &results.results {
            let rel = file.path.strip_prefix(&root).unwrap_or(&file.path);
            let name = rel.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let collapsed = app.folder.collapsed.contains(&file.path);
            let resp = tree_row(ui, 0, widgets::file_icon(&name, false, false), &name, false, false, Some(!collapsed));
            let dir = rel.parent().map(|d| d.display().to_string()).unwrap_or_default();
            let count = file.hits.len().to_string();
            ui.painter().text(pos2(resp.rect.right() - 10.0, resp.rect.center().y), Align2::RIGHT_CENTER, count, FontId::proportional(11.5), p.on_surface_variant);
            let resp = if dir.is_empty() { resp } else { resp.on_hover_text(rel.display().to_string()) };
            if resp.clicked() {
                toggle = Some(file.path.clone());
            }
            if collapsed {
                continue;
            }
            for hit in &file.hits {
                let (rect, r) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::click());
                if r.hovered() {
                    ui.painter().rect_filled(rect, CornerRadius::same(12), p.on_surface.gamma_multiply(0.07));
                }
                let x = rect.left() + 30.0;
                let ln = format!("{}", hit.line + 1);
                ui.painter().text(pos2(x, rect.center().y), Align2::LEFT_CENTER, &ln, FontId::monospace(11.0), p.outline);
                let mut job = egui::text::LayoutJob::default();
                let f = |c| egui::TextFormat::simple(FontId::proportional(12.5), c);
                job.append(&hit.preview[..hit.range.start], 0.0, f(p.on_surface_variant));
                let mut m = f(p.on_tertiary_container);
                m.background = p.tertiary_container;
                job.append(&hit.preview[hit.range.clone()], 0.0, m);
                job.append(&hit.preview[hit.range.end..], 0.0, f(p.on_surface_variant));
                job.wrap = egui::text::TextWrapping::truncate_at_width(rect.right() - x - 40.0);
                let g = ui.painter().layout_job(job);
                ui.painter().galley(pos2(x + 34.0, rect.center().y - g.size().y / 2.0), g, p.on_surface);
                if r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    open = Some(OpenRequest { path: file.path.clone(), line: Some(hit.line + 1), col: Some(hit.col + 1) });
                }
            }
        }
    });
    if let Some(t) = toggle {
        if !app.folder.collapsed.remove(&t) {
            app.folder.collapsed.insert(t);
        }
    }
    if let Some(req) = open {
        app.actions.push(Action::Open(req));
        // Select the match itself once the file is open.
        app.find.query = app.folder.run.as_ref().map(|r| r.query.clone()).unwrap_or_default();
    }
}
