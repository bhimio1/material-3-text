//! Small general-purpose UI helpers that other Material 3 apps could use
//! too (candidates for material-3-ui).

use egui::{vec2, CornerRadius, FontId, Key, Modifiers, Response, RichText, Sense, Ui};
use material_3_ui::icons::{icons::*, MaterialIcon};
use material_3_ui::theme::M3Palette;
use material_3_ui::widgets;

/// Consume a key press with exactly these modifiers (egui's own
/// `consume_shortcut` also matches when Shift/Alt are held extra, so
/// Ctrl+S would swallow Ctrl+Shift+S).
pub fn take_key(ctx: &egui::Context, mods: Modifiers, key: Key) -> bool {
    ctx.input_mut(|i| {
        let pos = i.events.iter().position(|e| {
            matches!(e, egui::Event::Key { key: k, pressed: true, modifiers: m, .. }
                if *k == key && m.command == mods.command && m.shift == mods.shift && m.alt == mods.alt)
        });
        match pos {
            Some(p) => {
                i.events.remove(p);
                // Alt+key also arrives as typed text; that's part of the shortcut.
                if matches!(i.events.get(p), Some(egui::Event::Text(_))) {
                    i.events.remove(p);
                }
                true
            }
            None => false,
        }
    })
}

/// An icon for a file, by name.
pub fn file_icon(name: &str, is_dir: bool, expanded: bool) -> MaterialIcon {
    if is_dir {
        return if expanded { ICON_FOLDER_OPEN } else { ICON_FOLDER };
    }
    let lower = name.to_lowercase();
    let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    match ext {
        "rs" | "c" | "h" | "cc" | "cpp" | "hpp" | "go" | "py" | "java" | "kt" | "rb" | "zig" | "lua" | "qml" | "nim" | "swift" => ICON_CODE,
        "js" | "mjs" | "cjs" | "ts" | "tsx" | "jsx" => ICON_JAVASCRIPT,
        "json" | "jsonc" | "json5" => ICON_DATA_OBJECT,
        "html" | "htm" | "xml" | "svg" => ICON_HTML,
        "css" | "scss" | "sass" | "less" => ICON_CSS,
        "md" | "markdown" | "mdown" => ICON_MARKDOWN,
        "sh" | "bash" | "zsh" | "fish" => ICON_TERMINAL,
        "toml" | "yaml" | "yml" | "ini" | "conf" | "cfg" | "desktop" | "service" | "env" => ICON_SETTINGS,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "avif" => ICON_IMAGE,
        "pdf" => ICON_PICTURE_AS_PDF,
        "mp3" | "flac" | "ogg" | "wav" | "opus" => ICON_AUDIO_FILE,
        "mp4" | "mkv" | "webm" | "mov" => ICON_VIDEO_FILE,
        "zip" | "tar" | "gz" | "xz" | "zst" | "7z" | "rar" => ICON_FOLDER_ZIP,
        "lock" => ICON_LOCK,
        "sql" | "db" | "sqlite" => ICON_DATABASE,
        "txt" | "log" => ICON_ARTICLE,
        _ if lower == "pkgbuild" || lower == "makefile" || lower == "dockerfile" => ICON_DEPLOYED_CODE,
        _ => ICON_DESCRIPTION,
    }
}

/// A rounded single-line text field. Returns the TextEdit response.
pub fn field(ui: &mut Ui, text: &mut String, hint: &str, width: f32, id: egui::Id) -> Response {
    let p = widgets::palette(ui);
    egui::Frame::new()
        .fill(p.surface_container_highest)
        .corner_radius(CornerRadius::same(12))
        .inner_margin(egui::Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.add(
                egui::TextEdit::singleline(text)
                    .id(id)
                    .frame(egui::Frame::NONE)
                    .hint_text(hint)
                    .font(FontId::proportional(14.0))
                    .desired_width(width - 20.0),
            )
        })
        .inner
}

/// A compact text button for status bars: label, hover layer, tooltip.
pub fn chip(ui: &mut Ui, text: &str, tooltip: &str) -> Response {
    let p = widgets::palette(ui);
    let galley = ui.painter().layout_no_wrap(text.to_string(), FontId::proportional(12.5), p.on_surface_variant);
    let size = vec2(galley.size().x + 16.0, 24.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(12), p.on_surface.gamma_multiply(0.08));
    }
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, p.on_surface_variant);
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    if tooltip.is_empty() { resp } else { resp.on_hover_text(tooltip) }
}

/// A menu entry with an optional leading icon, a check mark when `checked`
/// and a shortcut hint. Closes the menu when clicked.
pub fn menu_item(ui: &mut Ui, icon: Option<MaterialIcon>, text: &str, shortcut: &str, checked: bool) -> bool {
    let p = widgets::palette(ui);
    // Context menus offer the whole window's width; keep them compact.
    let width = ui.available_width().clamp(220.0, 300.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 32.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(8), p.on_surface.gamma_multiply(0.08));
    }
    let mut x = rect.left() + 10.0;
    let lead = if checked { Some(ICON_CHECK) } else { icon };
    if let Some(i) = lead {
        widgets::paint_icon(ui, i, egui::pos2(x + 9.0, rect.center().y), 18.0, if checked { p.primary } else { p.on_surface_variant });
    }
    x += 28.0;
    ui.painter().text(egui::pos2(x, rect.center().y), egui::Align2::LEFT_CENTER, text, FontId::proportional(14.0), p.on_surface);
    if !shortcut.is_empty() {
        ui.painter().text(
            egui::pos2(rect.right() - 10.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            shortcut,
            FontId::proportional(12.0),
            p.on_surface_variant,
        );
    }
    let clicked = resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked();
    if clicked {
        ui.close();
    }
    clicked
}

/// A small label in the secondary text color.
pub fn dim(text: impl Into<String>, size: f32, p: &M3Palette) -> RichText {
    RichText::new(text).size(size).color(p.on_surface_variant)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icons_by_extension() {
        let same = |a: MaterialIcon, b: MaterialIcon| a.codepoint == b.codepoint;
        assert!(same(file_icon("main.rs", false, false), ICON_CODE));
        assert!(same(file_icon("README.MD", false, false), ICON_MARKDOWN));
        assert!(same(file_icon("PKGBUILD", false, false), ICON_DEPLOYED_CODE));
        assert!(same(file_icon("src", true, true), ICON_FOLDER_OPEN));
        assert!(same(file_icon("noext", false, false), ICON_DESCRIPTION));
    }
}
