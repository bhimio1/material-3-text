//! A small text helper for the UI.

use egui::RichText;
use material_3_ui::theme::M3Palette;

/// A small label in the secondary text color.
pub fn dim(text: impl Into<String>, size: f32, p: &M3Palette) -> RichText {
    RichText::new(text).size(size).color(p.on_surface_variant)
}
