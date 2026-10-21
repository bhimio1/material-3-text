//! Quick open, go to line, and the dialogs.

use crate::app::{Action, App, Dialog};
use crate::cli::OpenRequest;
use crate::tree;
use crate::ui_ext;
use egui::{pos2, vec2, CornerRadius, FontId, Frame, Key, Margin, RichText, Sense};
use material_3_ui::common;
use material_3_ui::icons::icons::*;
use material_3_ui::prefs;
use material_3_ui::theme::M3Palette;
use material_3_ui::widgets::{self, ButtonKind};
use std::path::{Path, PathBuf};

pub fn show(app: &mut App, ctx: &egui::Context) {
    if app.quick.is_some() {
        quick_open(app, ctx);
    }
    if app.goto.is_some() {
        goto_line(app, ctx);
    }
    if app.dialog.is_some() {
        dialog(app, ctx);
    }
}

/// A floating panel near the top of the window.
fn palette_panel<R>(ctx: &egui::Context, p: &M3Palette, id: &str, width: f32, add: impl FnOnce(&mut egui::Ui) -> R) -> (R, bool) {
    let screen = ctx.content_rect();
    let w = width.min(screen.width() - 32.0);
    let inner = egui::Area::new(egui::Id::unique(id))
        .order(egui::Order::Foreground)
        .fixed_pos(pos2(screen.center().x - w / 2.0, screen.top() + 64.0))
        .show(ctx, |ui| {
            Frame::new()
                .fill(p.surface_container_high)
                .corner_radius(CornerRadius::same(24))
                .inner_margin(Margin::same(12))
                .shadow(egui::Shadow { offset: [0, 8], blur: 28, spread: 0, color: egui::Color32::from_black_alpha(80) })
                .show(ui, |ui| {
                    ui.set_width(w - 24.0);
                    add(ui)
                })
                .inner
        });
    let clicked_outside = ctx.input(|i| i.pointer.any_pressed())
        && ctx.input(|i| i.pointer.interact_pos()).is_some_and(|pos| !inner.response.rect.contains(pos));
    (inner.inner, clicked_outside)
}

struct Candidate {
    path: PathBuf,
    shown: String,
}

fn quick_open(app: &mut App, ctx: &egui::Context) {
    let p = app.theme.palette().clone();
    // Candidates: the folder's files, else open and recent files.
    let mut cands: Vec<Candidate> = Vec::new();
    let mut indexing = false;
    if let Some(ws) = &mut app.workspace {
        ws.ensure_index(ctx);
        indexing = ws.index.running();
        if let Some(files) = &ws.index.value {
            cands = files.iter().map(|rel| Candidate { path: ws.root.join(rel), shown: rel.display().to_string() }).collect();
        }
    } else {
        let mut seen = Vec::new();
        for path in app.docs.iter().filter_map(|d| d.path.clone()).chain(app.recent.files.iter().cloned()) {
            if !seen.contains(&path) && path.exists() {
                cands.push(Candidate { shown: material_3_ui::sys::paths::tilde(&path), path: path.clone() });
                seen.push(path);
            }
        }
    }
    let q = app.quick.as_mut().expect("checked");
    let query = q.query.clone();
    // `:42` jumps to a line instead.
    let goto_line = query.strip_prefix(':').and_then(|n| n.trim().parse::<usize>().ok());
    let shown_paths: Vec<PathBuf> = cands.iter().map(|c| PathBuf::from(&c.shown)).collect();
    let results: Vec<(usize, Vec<usize>)> = if query.is_empty() || goto_line.is_some() {
        (0..cands.len().min(50)).map(|i| (i, Vec::new())).collect()
    } else {
        tree::fuzzy(&shown_paths, &query, 50)
    };
    let mut selected = q.selected.min(results.len().saturating_sub(1));
    let (up, down, enter, esc) = ctx.input(|i| (i.key_pressed(Key::ArrowUp), i.key_pressed(Key::ArrowDown), i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
    if up {
        selected = selected.saturating_sub(1);
    }
    if down && selected + 1 < results.len() {
        selected += 1;
    }
    let mut chosen: Option<usize> = None;
    let ((), outside) = palette_panel(ctx, &p, "quick-open", 640.0, |ui| {
        let w = ui.available_width();
        let fid = egui::Id::unique("quick-field");
        let resp = ui_ext::field(ui, &mut q.query, "Go to file (type :N for a line)", w, fid);
        resp.request_focus();
        ui.add_space(6.0);
        if indexing && cands.is_empty() {
            common::loading(ui, "Indexing files…");
        } else if results.is_empty() {
            ui.label(ui_ext::dim(if cands.is_empty() { "No files yet" } else { "No matching files" }, 13.0, &p));
        }
        egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (row, (i, idx)) in results.iter().enumerate() {
                let c = &cands[*i];
                let (rect, r) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
                let sel = row == selected;
                if sel || r.hovered() {
                    ui.painter().rect_filled(rect, CornerRadius::same(14), if sel { p.secondary_container } else { p.on_surface.gamma_multiply(0.06) });
                }
                if sel && (up || down) {
                    ui.scroll_to_rect(rect, None);
                }
                let name_start = c.shown.rfind('/').map_or(0, |i| i + 1);
                let name = &c.shown[name_start..];
                widgets::paint_icon(ui, ui_ext::file_icon(name, false, false), pos2(rect.left() + 20.0, rect.center().y), 20.0, p.on_surface_variant);
                let mut job = egui::text::LayoutJob::default();
                let name_chars = c.shown[..name_start].chars().count();
                for (ci, ch) in name.chars().enumerate() {
                    let hit = idx.contains(&(name_chars + ci));
                    let mut f = egui::TextFormat::simple(widgets::title_font(14.0), if hit { p.primary } else { p.on_surface });
                    if hit {
                        f.underline = egui::Stroke::new(1.0, p.primary);
                    }
                    job.append(&ch.to_string(), 0.0, f);
                }
                let dir = c.shown[..name_start].trim_end_matches('/');
                if !dir.is_empty() {
                    job.append(&format!("   {dir}"), 0.0, egui::TextFormat::simple(FontId::proportional(12.5), p.on_surface_variant));
                }
                job.wrap = egui::text::TextWrapping::truncate_at_width(rect.width() - 52.0);
                let g = ui.painter().layout_job(job);
                ui.painter().galley(pos2(rect.left() + 42.0, rect.center().y - g.size().y / 2.0), g, p.on_surface);
                if r.clicked() {
                    chosen = Some(*i);
                }
            }
        });
    });
    q.selected = selected;
    if enter {
        if let Some(n) = goto_line {
            app.quick = None;
            app.goto_pos(Some(n), None);
            return;
        }
        chosen = results.get(selected).map(|r| r.0);
    }
    if let Some(i) = chosen {
        app.actions.push(Action::Open(OpenRequest { path: cands[i].path.clone(), line: None, col: None }));
        app.quick = None;
    } else if esc || outside {
        app.quick = None;
        if let Some(d) = app.doc_mut_active() {
            d.view.focus = true;
        }
    }
}

fn goto_line(app: &mut App, ctx: &egui::Context) {
    let p = app.theme.palette().clone();
    let lines = app.doc().map_or(0, |d| d.buffer.len_lines());
    let text = app.goto.as_mut().expect("checked");
    let (enter, esc) = ctx.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
    let ((), outside) = palette_panel(ctx, &p, "goto", 360.0, |ui| {
        let w = ui.available_width();
        let resp = ui_ext::field(ui, text, &format!("Line[:column], 1–{lines}"), w, egui::Id::unique("goto-field"));
        resp.request_focus();
    });
    if enter {
        let mut parts = text.trim().splitn(2, [':', ',']);
        let line = parts.next().and_then(|s| s.trim().parse::<usize>().ok());
        let col = parts.next().and_then(|s| s.trim().parse::<usize>().ok());
        app.goto = None;
        app.goto_pos(line, col);
    } else if esc || outside {
        app.goto = None;
        if let Some(d) = app.doc_mut_active() {
            d.view.focus = true;
        }
    }
}

fn title_of(app: &App, id: u64) -> String {
    app.docs.iter().find(|d| d.id == id).map(|d| d.title()).unwrap_or_default()
}

fn dialog(app: &mut App, ctx: &egui::Context) {
    let p = app.theme.palette().clone();
    let Some(d) = app.dialog.take() else { return };
    let mut keep = true;
    match d {
        Dialog::ConfirmClose(id) => {
            let title = title_of(app, id);
            common::modal(ctx, &p, "close", ICON_SAVE, &format!("Save changes to {title}?"), |ui| {
                ui.label(RichText::new("Your changes will be lost if you don't save them.").color(p.on_surface_variant));
                common::dialog_buttons(ui, |ui| {
                    if widgets::button(ui, ButtonKind::Filled, None, "Save").clicked() {
                        keep = false;
                        if app.save(id) {
                            app.actions.push(Action::Close(id));
                        }
                    }
                    if widgets::button(ui, ButtonKind::Text, None, "Don't save").clicked() {
                        keep = false;
                        app.actions.push(Action::Discard(id));
                    }
                    if widgets::button(ui, ButtonKind::Text, None, "Cancel").clicked() {
                        keep = false;
                    }
                });
            });
            if keep {
                app.dialog = Some(Dialog::ConfirmClose(id));
            }
        }
        Dialog::ConfirmQuit => {
            let dirty: Vec<String> = app.docs.iter().filter(|d| d.is_dirty()).map(|d| d.title()).collect();
            common::modal(ctx, &p, "quit", ICON_SAVE, "Save changes before quitting?", |ui| {
                ui.label(RichText::new(format!("Unsaved: {}", dirty.join(", "))).color(p.on_surface_variant));
                common::dialog_buttons(ui, |ui| {
                    if widgets::button(ui, ButtonKind::Filled, None, "Save all").clicked() {
                        keep = false;
                        if app.save_all_for_quit() {
                            app.quit_now(ctx);
                        }
                    }
                    if widgets::button(ui, ButtonKind::Danger, None, "Quit without saving").clicked() {
                        keep = false;
                        app.quit_now(ctx);
                    }
                    if widgets::button(ui, ButtonKind::Text, None, "Cancel").clicked() {
                        keep = false;
                    }
                });
            });
            if keep {
                app.dialog = Some(Dialog::ConfirmQuit);
            }
        }
        Dialog::SaveAs { id, mut path, mut overwrite } => {
            let mut browse = false;
            let mut confirm = false;
            common::modal(ctx, &p, "save-as", ICON_SAVE_AS, &format!("Save {}", title_of(app, id)), |ui| {
                ui.set_width(460.0);
                let resp = ui_ext::field(ui, &mut path, "Path", 460.0, egui::Id::unique("save-as-path"));
                if !resp.has_focus() && !overwrite {
                    resp.request_focus();
                }
                if resp.changed() {
                    overwrite = false;
                }
                // (Checked directly: the field keeps its focus here, so it never
                // reports losing it to Enter.)
                if ui.input(|i| i.key_pressed(Key::Enter)) {
                    confirm = true;
                }
                let target = material_3_ui::sys::paths::expand(path.trim());
                if overwrite {
                    ui.label(RichText::new(format!("{} already exists. Replace it?", target.display())).color(p.error));
                }
                common::dialog_buttons(ui, |ui| {
                    if widgets::button(ui, ButtonKind::Filled, None, if overwrite { "Replace" } else { "Save" }).clicked() {
                        confirm = true;
                    }
                    if widgets::button(ui, ButtonKind::Text, None, "Cancel").clicked() {
                        keep = false;
                    }
                    if widgets::button(ui, ButtonKind::Text, Some(ICON_FOLDER_OPEN), "Browse…").clicked() {
                        browse = true;
                    }
                });
            });
            if browse {
                app.pick_save_path(id, &path, ctx);
            }
            if confirm {
                let target = material_3_ui::sys::paths::expand(path.trim());
                let same = app.docs.iter().find(|d| d.id == id).and_then(|d| d.path.clone()).as_deref() == Some(target.as_path());
                if path.trim().is_empty() || target.is_dir() {
                    app.toasts.error("Enter a file name");
                } else if target.exists() && !overwrite && !same {
                    overwrite = true;
                } else {
                    keep = false;
                    app.finish_save_as(id, target);
                }
            }
            if keep && app.dialog.is_none() {
                app.dialog = Some(Dialog::SaveAs { id, path, overwrite });
            }
        }
        Dialog::SaveAsRoot(id) => {
            let path = app.docs.iter().find(|d| d.id == id).and_then(|d| d.path.clone()).unwrap_or_default();
            common::modal(ctx, &p, "root", ICON_ADMIN_PANEL_SETTINGS, "Save as administrator?", |ui| {
                ui.set_max_width(440.0);
                ui.add(egui::Label::new(RichText::new(format!("You don't have permission to write {}. Saving it needs your password.", path.display())).color(p.on_surface_variant)).wrap());
                common::dialog_buttons(ui, |ui| {
                    if widgets::button(ui, ButtonKind::Filled, Some(ICON_ADMIN_PANEL_SETTINGS), "Save as administrator").clicked() {
                        keep = false;
                        app.actions.push(Action::SaveAsRoot(id));
                    }
                    if widgets::button(ui, ButtonKind::Text, None, "Save as…").clicked() {
                        keep = false;
                        app.actions.push(Action::SaveAs(id));
                    }
                    if widgets::button(ui, ButtonKind::Text, None, "Cancel").clicked() {
                        keep = false;
                    }
                });
            });
            if keep {
                app.dialog = Some(Dialog::SaveAsRoot(id));
            }
        }
        Dialog::Recovery(mut entries) => {
            let mut remove: Option<usize> = None;
            let mut all = false;
            let mut discard_all = false;
            common::modal(ctx, &p, "recovery", ICON_RESTORE, "Recover unsaved changes?", |ui| {
                ui.set_width(480.0);
                ui.label(RichText::new("Material 3 Text closed without saving these. Recover them, or discard the copies.").color(p.on_surface_variant));
                ui.add_space(8.0);
                egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                    for (i, e) in entries.iter().enumerate() {
                        let sub = match &e.data.path {
                            Some(path) => material_3_ui::sys::paths::tilde(path),
                            None => "Untitled".into(),
                        };
                        let age = crate::swap::now_secs().saturating_sub(e.data.saved);
                        let sub = format!("{} · {sub}", ago(age));
                        common::list_row(ui, ICON_HISTORY, true, &e.data.title, &sub, |ui| {
                            if widgets::button(ui, ButtonKind::Text, None, "Discard").clicked() {
                                app.actions.push(Action::DiscardSwap(e.file.clone()));
                                remove = Some(i);
                            }
                            if widgets::button(ui, ButtonKind::Tonal, None, "Recover").clicked() {
                                app.actions.push(Action::Recover(e.clone()));
                                remove = Some(i);
                            }
                        });
                    }
                });
                common::dialog_buttons(ui, |ui| {
                    if widgets::button(ui, ButtonKind::Filled, None, "Recover all").clicked() {
                        all = true;
                    }
                    if widgets::button(ui, ButtonKind::Text, None, "Discard all").clicked() {
                        discard_all = true;
                    }
                    if widgets::button(ui, ButtonKind::Text, None, "Later").clicked() {
                        keep = false;
                    }
                });
            });
            if let Some(i) = remove {
                entries.remove(i);
            }
            if all {
                app.actions.extend(entries.drain(..).map(Action::Recover));
            }
            if discard_all {
                app.actions.extend(entries.drain(..).map(|e| Action::DiscardSwap(e.file)));
            }
            if keep && !entries.is_empty() {
                app.dialog = Some(Dialog::Recovery(entries));
            }
        }
        Dialog::NewEntry { dir, folder, mut name, mut error } => {
            let mut confirm = false;
            let title = if folder { "New folder" } else { "New file" };
            common::modal(ctx, &p, "new-entry", if folder { ICON_CREATE_NEW_FOLDER } else { ICON_NOTE_ADD }, title, |ui| {
                ui.set_width(380.0);
                ui.label(ui_ext::dim(format!("In {}", material_3_ui::sys::paths::tilde(&dir)), 12.5, &p));
                let resp = ui_ext::field(ui, &mut name, "Name", 380.0, egui::Id::unique("new-entry-name"));
                resp.request_focus();
                // (Checked directly: the field keeps its focus here, so it never
                // reports losing it to Enter.)
                if ui.input(|i| i.key_pressed(Key::Enter)) {
                    confirm = true;
                }
                if let Some(e) = &error {
                    ui.label(RichText::new(e).color(p.error));
                }
                common::dialog_buttons(ui, |ui| {
                    if widgets::button(ui, ButtonKind::Filled, None, "Create").clicked() {
                        confirm = true;
                    }
                    if widgets::button(ui, ButtonKind::Text, None, "Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                        keep = false;
                    }
                });
            });
            if confirm {
                match create_entry(&dir, &name, folder) {
                    Ok(path) => {
                        keep = false;
                        app.actions.push(Action::RefreshTree);
                        app.actions.push(Action::Reveal(path.clone()));
                        if !folder {
                            app.actions.push(Action::Open(OpenRequest { path, line: None, col: None }));
                        }
                    }
                    Err(e) => error = Some(e),
                }
            }
            if keep {
                app.dialog = Some(Dialog::NewEntry { dir, folder, name, error });
            }
        }
        Dialog::Rename { path, mut name, mut error } => {
            let mut confirm = false;
            common::modal(ctx, &p, "rename", ICON_DRIVE_FILE_RENAME_OUTLINE, "Rename", |ui| {
                ui.set_width(380.0);
                let resp = ui_ext::field(ui, &mut name, "New name", 380.0, egui::Id::unique("rename-name"));
                resp.request_focus();
                // (Checked directly: the field keeps its focus here, so it never
                // reports losing it to Enter.)
                if ui.input(|i| i.key_pressed(Key::Enter)) {
                    confirm = true;
                }
                if let Some(e) = &error {
                    ui.label(RichText::new(e).color(p.error));
                }
                common::dialog_buttons(ui, |ui| {
                    if widgets::button(ui, ButtonKind::Filled, None, "Rename").clicked() {
                        confirm = true;
                    }
                    if widgets::button(ui, ButtonKind::Text, None, "Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                        keep = false;
                    }
                });
            });
            if confirm {
                match tree::valid_name(&name).map_err(str::to_string).and_then(|n| {
                    let to = path.with_file_name(n);
                    if to.exists() && to != path {
                        return Err(format!("{n} already exists"));
                    }
                    std::fs::rename(&path, &to).map(|_| to).map_err(|e| e.to_string())
                }) {
                    Ok(to) => {
                        keep = false;
                        // Open documents follow the rename.
                        for d in &mut app.docs {
                            if let Some(dp) = d.path.clone() {
                                if let Ok(rest) = dp.strip_prefix(&path) {
                                    d.path = Some(if rest.as_os_str().is_empty() { to.clone() } else { to.join(rest) });
                                    d.stamp = d.path.as_deref().and_then(crate::files::stamp);
                                    if !d.language_pinned {
                                        d.detect_language();
                                    }
                                }
                            }
                        }
                        app.actions.push(Action::RefreshTree);
                    }
                    Err(e) => error = Some(e),
                }
            }
            if keep {
                app.dialog = Some(Dialog::Rename { path, name, error });
            }
        }
        Dialog::Delete(path) => {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            common::modal(ctx, &p, "delete", ICON_DELETE, &format!("Move {name} to the trash?"), |ui| {
                if path.is_dir() {
                    ui.label(RichText::new("The folder and everything in it will be moved.").color(p.on_surface_variant));
                }
                common::dialog_buttons(ui, |ui| {
                    if widgets::button(ui, ButtonKind::Danger, Some(ICON_DELETE), "Move to trash").clicked() {
                        keep = false;
                        match tree::delete(&path) {
                            Ok(()) => app.actions.push(Action::RefreshTree),
                            Err(e) => app.toasts.error(format!("Couldn't delete {name}: {e}")),
                        }
                    }
                    if widgets::button(ui, ButtonKind::Text, None, "Cancel").clicked() {
                        keep = false;
                    }
                });
            });
            if keep {
                app.dialog = Some(Dialog::Delete(path));
            }
        }
        Dialog::ReplaceInFolder => {
            let (n, files) = app.folder.run.as_ref().map_or((0, 0), |r| (r.total, r.results.len()));
            let with = app.folder.replace.clone();
            common::modal(ctx, &p, "replace-folder", ICON_FIND_REPLACE, "Replace in folder?", |ui| {
                ui.set_max_width(420.0);
                ui.add(egui::Label::new(RichText::new(format!("Replace {n} matches in {files} files with “{with}”? Files that are open are changed in their tabs, the rest are saved right away.")).color(p.on_surface_variant)).wrap());
                common::dialog_buttons(ui, |ui| {
                    if widgets::button(ui, ButtonKind::Filled, None, "Replace all").clicked() {
                        keep = false;
                        app.actions.push(Action::ReplaceInFolder);
                    }
                    if widgets::button(ui, ButtonKind::Text, None, "Cancel").clicked() {
                        keep = false;
                    }
                });
            });
            if keep {
                app.dialog = Some(Dialog::ReplaceInFolder);
            }
        }
        Dialog::Preferences => {
            let mut close = false;
            let screen = ctx.content_rect();
            egui::Modal::new(egui::Id::unique("prefs"))
                .backdrop_color(p.scrim.gamma_multiply(0.4))
                .frame(Frame::new().fill(p.surface_container_low).corner_radius(CornerRadius::same(28)).inner_margin(Margin::same(8)))
                .show(ctx, |ui| {
                    ui.set_width((screen.width() - 80.0).min(760.0));
                    ui.set_height((screen.height() - 80.0).min(720.0));
                    let top = ui.max_rect().right_top();
                    let r = egui::Rect::from_center_size(top + vec2(-28.0, 28.0), vec2(36.0, 36.0));
                    common::page(ui, ICON_TUNE, "Preferences", "Saved to ~/.config/m3-text/config.toml.", |ui| {
                        if prefs_body(app, ui) {
                            app.save_config();
                        }
                    });
                    let resp = ui.interact(r, egui::Id::unique("prefs-close"), Sense::click());
                    if resp.hovered() {
                        ui.painter().circle_filled(r.center(), 18.0, p.on_surface.gamma_multiply(0.08));
                    }
                    widgets::paint_icon(ui, ICON_CLOSE, r.center(), 20.0, p.on_surface_variant);
                    if resp.clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                        close = true;
                    }
                });
            if !close {
                app.dialog = Some(Dialog::Preferences);
            }
        }
    }
}

fn ago(secs: u64) -> String {
    match secs {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86399 => format!("{} h ago", secs / 3600),
        _ => format!("{} days ago", secs / 86400),
    }
}

fn create_entry(dir: &Path, name: &str, folder: bool) -> Result<PathBuf, String> {
    let name = tree::valid_name(name)?;
    let path = dir.join(name);
    if path.exists() {
        return Err(format!("{name} already exists"));
    }
    if folder {
        std::fs::create_dir_all(&path).map_err(|e| e.to_string())?;
    } else {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::File::create(&path).map_err(|e| e.to_string())?;
    }
    Ok(path)
}

/// The preferences cards. Returns true when something changed.
fn prefs_body(app: &mut App, ui: &mut egui::Ui) -> bool {
    let mut changed = false;
    if prefs::appearance_card(ui, &mut app.config.appearance, &app.theme) {
        app.theme.apply(&app.config.appearance);
        changed = true;
    }
    let e = &mut app.config.editor;
    common::card(ui, "Editor", |ui| {
        common::row(ui, "Font size", "Ctrl+= and Ctrl+- change it too.", |ui| {
            changed |= ui.add(egui::Slider::new(&mut e.font_size, 8.0..=32.0).step_by(1.0)).changed();
        });
        ui.separator();
        common::row(ui, "Tab width", "", |ui| {
            changed |= ui.add(egui::Slider::new(&mut e.tab_size, 1..=8)).changed();
        });
        ui.separator();
        for (title, desc, val) in [
            ("Insert spaces", "Tab inserts spaces instead of a tab character.", &mut e.insert_spaces),
            ("Detect indentation", "Follow the indentation a file already uses.", &mut e.detect_indent),
            ("Word wrap", "Alt+Z", &mut e.word_wrap),
            ("Show whitespace", "Spaces as dots and tabs as arrows.", &mut e.show_whitespace),
            ("Line numbers", "", &mut e.line_numbers),
            ("Highlight current line", "", &mut e.highlight_line),
        ] {
            common::row(ui, title, desc, |ui| {
                changed |= widgets::switch(ui, val).changed();
            });
            ui.separator();
        }
    });
    common::card(ui, "Saving", |ui| {
        for (title, desc, val) in [
            ("Autosave", "Save files a moment after you stop typing.", &mut e.autosave),
            ("Reload changed files", "Files changed by other programs reload when you have no unsaved changes.", &mut e.auto_reload),
            ("Trim trailing whitespace", "On save.", &mut e.trim_trailing_whitespace),
            ("End with a newline", "Add a final line break on save.", &mut e.final_newline),
        ] {
            common::row(ui, title, desc, |ui| {
                changed |= widgets::switch(ui, val).changed();
            });
            ui.separator();
        }
        common::row(ui, "Autosave delay", "Seconds after the last change.", |ui| {
            changed |= ui.add(egui::Slider::new(&mut e.autosave_delay_secs, 1..=30)).changed();
        });
        common::note(ui, "Unsaved changes are also kept in ~/.local/state/m3-text/swap, so they can be recovered after a crash.");
    });
    common::card(ui, "Files", |ui| {
        for (title, desc, val) in [
            ("Show hidden files", "In the sidebar, quick open and folder search.", &mut e.show_hidden_files),
            ("Show ignored files", "Files excluded by .gitignore.", &mut e.show_ignored_files),
        ] {
            common::row(ui, title, desc, |ui| {
                if widgets::switch(ui, val).changed() {
                    changed = true;
                    app.actions.push(Action::RefreshTree);
                }
            });
            ui.separator();
        }
    });
    common::card(ui, "Keyboard", |ui| {
        let keys = [
            ("Quick open / go to file", "Ctrl+P"),
            ("Find / replace", "Ctrl+F / Ctrl+H"),
            ("Find in folder", "Ctrl+Shift+F"),
            ("Go to line", "Ctrl+G"),
            ("Toggle comment", "Ctrl+/"),
            ("Add cursor above / below", "Shift+Alt+↑ / ↓"),
            ("Select next occurrence", "Ctrl+D"),
            ("Move line up / down", "Alt+↑ / ↓"),
            ("Duplicate / delete line", "Ctrl+Shift+D / Ctrl+Shift+K"),
            ("Indent / outdent", "Tab / Shift+Tab, Ctrl+] / Ctrl+["),
            ("Jump to matching bracket", "Ctrl+M"),
            ("Markdown preview", "Ctrl+Shift+M"),
        ];
        egui::Grid::new("keys").num_columns(2).spacing([24.0, 8.0]).show(ui, |ui| {
            for (what, key) in keys {
                ui.label(what);
                ui.label(RichText::new(key).monospace());
                ui.end_row();
            }
        });
    });
    changed
}
