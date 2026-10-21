//! The tab strip: one pill per document, drag to reorder, middle-click to
//! close, and the app menu.

use crate::app::{Action, App, Dialog};
use egui::{pos2, vec2, CornerRadius, Rect, Sense, Stroke, Ui};
use material_3_ui::icons::icons::*;
use material_3_ui::widgets;

const TAB_H: f32 = 36.0;

pub fn show(app: &mut App, ui: &mut Ui) {
    let p = app.theme.palette().clone();
    let full = ui.available_rect_before_wrap();
    let buttons_w = 84.0;
    let strip = Rect::from_min_max(full.min, pos2(full.max.x - buttons_w, full.max.y));
    let actions = Rect::from_min_max(pos2(strip.max.x, full.min.y), full.max);

    ui.scope_builder(egui::UiBuilder::new().max_rect(strip), |ui| {
        egui::ScrollArea::horizontal()
            .id_salt("tabs")
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let n = app.docs.len();
                    for i in 0..n {
                        tab(app, ui, i, &p);
                    }
                });
            });
    });

    ui.scope_builder(
        egui::UiBuilder::new().max_rect(actions).layout(egui::Layout::right_to_left(egui::Align::Center)),
        |ui| {
            let menu = widgets::icon_button(ui, ICON_MORE_VERT, "Menu", true);
            egui::Popup::menu(&menu).width(280.0).show(|ui| app_menu(app, ui));
            if widgets::icon_button(ui, ICON_ADD, "New file (Ctrl+N)", true).clicked() {
                app.actions.push(Action::New);
            }
        },
    );
}

fn tab(app: &mut App, ui: &mut Ui, i: usize, p: &material_3_ui::theme::M3Palette) {
    let doc = &app.docs[i];
    let id = doc.id;
    let active = i == app.active;
    let title = doc.title();
    let dirty = doc.is_dirty();
    let path = doc.path.clone();
    let font = widgets::title_font(13.5);
    let galley = widgets::truncated_galley(ui, &title, font, p.on_surface, 180.0);
    let w = galley.size().x + 26.0 + 30.0 + 12.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(w, TAB_H), Sense::click_and_drag());
    let close_rect = Rect::from_center_size(pos2(rect.right() - 18.0, rect.center().y), vec2(24.0, 24.0));
    let close_resp = ui.interact(close_rect, egui::Id::unique(("tab-close", id)), Sense::click());

    let (bg, fg) = if active {
        (p.secondary_container, p.on_secondary_container)
    } else if resp.hovered() {
        (p.on_surface.gamma_multiply(0.08), p.on_surface)
    } else {
        (egui::Color32::TRANSPARENT, p.on_surface_variant)
    };
    ui.painter().rect_filled(rect, CornerRadius::same(18), bg);
    widgets::paint_icon(ui, widgets::file_icon(&title, false, false), pos2(rect.left() + 18.0, rect.center().y), 17.0, if active { fg } else { p.on_surface_variant });
    let galley = widgets::truncated_galley(ui, &title, widgets::title_font(13.5), fg, 180.0);
    ui.painter().galley(pos2(rect.left() + 32.0, rect.center().y - galley.size().y / 2.0), galley, fg);
    let hover_close = close_resp.hovered() || resp.hovered();
    if dirty && !hover_close {
        ui.painter().circle_filled(close_rect.center(), 4.5, if active { fg } else { p.primary });
    } else if hover_close || active {
        if close_resp.hovered() {
            ui.painter().circle_filled(close_rect.center(), 12.0, p.on_surface.gamma_multiply(0.12));
        }
        widgets::paint_icon(ui, ICON_CLOSE, close_rect.center(), 16.0, fg);
    }

    if close_resp.clicked() || resp.middle_clicked() {
        app.actions.push(Action::Close(id));
    } else if resp.clicked() || resp.drag_started() {
        app.actions.push(Action::Activate(i));
    }
    let resp = match &path {
        Some(p) => resp.on_hover_text(material_3_ui::sys::paths::tilde(p)),
        None => resp,
    };

    // Drag to reorder.
    if resp.dragged() {
        resp.dnd_set_drag_payload(i);
    }
    if let Some(from) = resp.dnd_hover_payload::<usize>() {
        let after = ui.input(|inp| inp.pointer.hover_pos()).is_some_and(|pos| pos.x > rect.center().x);
        let x = if after { rect.right() + 2.0 } else { rect.left() - 2.0 };
        if *from != i {
            ui.painter().line_segment([pos2(x, rect.top() + 4.0), pos2(x, rect.bottom() - 4.0)], Stroke::new(2.0, p.primary));
        }
        if let Some(from) = resp.dnd_release_payload::<usize>() {
            let to = if after { i + 1 } else { i };
            app.actions.push(Action::MoveTab(*from, to));
        }
    }

    resp.context_menu(|ui| {
        if widgets::menu_item(ui, Some(ICON_CLOSE), "Close", "Ctrl+W", false) {
            app.actions.push(Action::Close(id));
        }
        if widgets::menu_item(ui, None, "Close others", "", false) {
            app.actions.push(Action::CloseOthers(id));
        }
        if widgets::menu_item(ui, None, "Close to the right", "", false) {
            app.actions.push(Action::CloseRight(id));
        }
        if let Some(path) = &path {
            ui.separator();
            if widgets::menu_item(ui, Some(ICON_CONTENT_COPY), "Copy path", "", false) {
                app.actions.push(Action::CopyPath(path.clone()));
            }
            if app.workspace.as_ref().is_some_and(|w| path.starts_with(&w.root)) && widgets::menu_item(ui, Some(ICON_FOLDER_OPEN), "Reveal in sidebar", "", false) {
                app.actions.push(Action::Reveal(path.clone()));
            }
            if widgets::menu_item(ui, Some(ICON_FOLDER), "Show in Files", "", false) {
                app.actions.push(Action::ShowInFiles(path.clone()));
            }
        }
    });
}

fn app_menu(app: &mut App, ui: &mut Ui) {
    let id = app.doc().map(|d| d.id);
    let mut push = |a: Action| app.actions.push(a);
    if widgets::menu_item(ui, Some(ICON_NOTE_ADD), "New file", "Ctrl+N", false) {
        push(Action::New);
    }
    if widgets::menu_item(ui, Some(ICON_FILE_OPEN), "Open file…", "Ctrl+O", false) {
        push(Action::OpenDialog);
    }
    if widgets::menu_item(ui, Some(ICON_FOLDER_OPEN), "Open folder…", "Ctrl+Shift+O", false) {
        push(Action::OpenFolderDialog);
    }
    if widgets::menu_item(ui, Some(ICON_HISTORY), "Reopen closed tab", "Ctrl+Shift+T", false) {
        push(Action::ReopenClosed);
    }
    ui.separator();
    if let Some(id) = id {
        if widgets::menu_item(ui, Some(ICON_SAVE), "Save", "Ctrl+S", false) {
            push(Action::Save(id));
        }
        if widgets::menu_item(ui, Some(ICON_SAVE_AS), "Save as…", "Ctrl+Shift+S", false) {
            push(Action::SaveAs(id));
        }
        if widgets::menu_item(ui, None, "Save all", "Ctrl+Alt+S", false) {
            push(Action::SaveAll);
        }
        if widgets::menu_item(ui, Some(ICON_REFRESH), "Reload from disk", "F5", false) {
            push(Action::Reload(id));
        }
        ui.separator();
    }
    let mut find = false;
    let mut replace = false;
    if widgets::menu_item(ui, Some(ICON_SEARCH), "Find", "Ctrl+F", false) {
        find = true;
    }
    if widgets::menu_item(ui, Some(ICON_FIND_REPLACE), "Replace", "Ctrl+H", false) {
        replace = true;
    }
    let mut folder_search = false;
    if app.workspace.is_some() && widgets::menu_item(ui, Some(ICON_DOCUMENT_SEARCH), "Find in folder", "Ctrl+Shift+F", false) {
        folder_search = true;
    }
    let mut quick = false;
    if widgets::menu_item(ui, Some(ICON_TOPIC), "Quick open", "Ctrl+P", false) {
        quick = true;
    }
    let mut goto = false;
    if widgets::menu_item(ui, Some(ICON_KEYBOARD_RETURN), "Go to line", "Ctrl+G", false) {
        goto = true;
    }
    ui.separator();
    let e = &mut app.config.editor;
    let mut changed = false;
    if widgets::menu_item(ui, Some(ICON_WRAP_TEXT), "Word wrap", "Alt+Z", e.word_wrap) {
        e.word_wrap = !e.word_wrap;
        changed = true;
    }
    if widgets::menu_item(ui, Some(ICON_VISIBILITY), "Show whitespace", "", e.show_whitespace) {
        e.show_whitespace = !e.show_whitespace;
        changed = true;
    }
    let markdown = app.doc().is_some_and(|d| d.is_markdown());
    let preview = app.doc().is_some_and(|d| d.preview);
    if markdown && widgets::menu_item(ui, Some(ICON_VERTICAL_SPLIT), "Markdown preview", "Ctrl+Shift+M", preview) {
        if let Some(d) = app.doc_mut_active() {
            d.preview = !d.preview;
        }
    }
    let side = app.config.show_sidebar;
    if widgets::menu_item(ui, Some(if side { ICON_LEFT_PANEL_CLOSE } else { ICON_LEFT_PANEL_OPEN }), "Sidebar", "Ctrl+B", side) {
        app.config.show_sidebar = !side;
        changed = true;
    }
    ui.separator();
    if widgets::menu_item(ui, Some(ICON_TUNE), "Preferences", "Ctrl+,", false) {
        app.dialog = Some(Dialog::Preferences);
    }
    if widgets::menu_item(ui, Some(ICON_CLOSE), "Quit", "Ctrl+Q", false) {
        app.actions.push(Action::Quit);
    }
    if changed {
        app.save_config();
    }
    if find || replace {
        app.find.open = true;
        app.find.replace_open |= replace;
        app.find.focus = true;
    }
    if folder_search {
        app.config.show_sidebar = true;
        app.side = crate::app::SideMode::Search;
        app.folder.focus = true;
    }
    if quick {
        app.quick = Some(crate::app::QuickOpen { query: String::new(), selected: 0 });
    }
    if goto {
        app.goto = Some(String::new());
    }
}
