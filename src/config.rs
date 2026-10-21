//! Preferences (`~/.config/m3-text/config.toml`) and the recent files and
//! folders (`~/.local/state/m3-text/recent.json`).

use material_3_ui::theme::ThemeSettings;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EditorSettings {
    pub font_size: f32,
    pub tab_size: usize,
    pub insert_spaces: bool,
    /// Use the indentation a file already has.
    pub detect_indent: bool,
    pub word_wrap: bool,
    pub show_whitespace: bool,
    pub line_numbers: bool,
    pub highlight_line: bool,
    pub trim_trailing_whitespace: bool,
    /// End the file with a line break when saving.
    pub final_newline: bool,
    pub autosave: bool,
    pub autosave_delay_secs: u64,
    /// Reload files changed by other programs when they have no unsaved
    /// changes, instead of asking.
    pub auto_reload: bool,
    pub show_hidden_files: bool,
    pub show_ignored_files: bool,
}

impl Default for EditorSettings {
    fn default() -> Self {
        Self {
            font_size: 14.0,
            tab_size: 4,
            insert_spaces: true,
            detect_indent: true,
            word_wrap: false,
            show_whitespace: false,
            line_numbers: true,
            highlight_line: true,
            trim_trailing_whitespace: false,
            final_newline: false,
            autosave: false,
            autosave_delay_secs: 2,
            auto_reload: true,
            show_hidden_files: false,
            show_ignored_files: false,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub appearance: ThemeSettings,
    pub editor: EditorSettings,
    pub show_sidebar: bool,
}

impl Config {
    pub fn path() -> PathBuf {
        material_3_ui::sys::paths::config_home().join("m3-text/config.toml")
    }

    pub fn load() -> Self {
        Self::load_from(&Self::path())
    }

    pub fn load_from(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_else(|| Config { show_sidebar: true, ..Default::default() })
    }

    pub fn save(&self) -> Result<(), String> {
        self.save_to(&Self::path())
    }

    pub fn save_to(&self, path: &Path) -> Result<(), String> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        let s = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, s).map_err(|e| e.to_string())
    }
}

const RECENT_MAX: usize = 20;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Recent {
    pub files: Vec<PathBuf>,
    pub folders: Vec<PathBuf>,
}

impl Recent {
    pub fn path() -> PathBuf {
        material_3_ui::sys::paths::state_dir("m3-text").join("recent.json")
    }

    pub fn load() -> Self {
        std::fs::read(Self::path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let p = Self::path();
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if let Ok(b) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(p, b);
        }
    }

    fn push(list: &mut Vec<PathBuf>, p: &Path) {
        list.retain(|x| x != p);
        list.insert(0, p.to_path_buf());
        list.truncate(RECENT_MAX);
    }

    pub fn add_file(&mut self, p: &Path) {
        Self::push(&mut self.files, p);
    }

    pub fn add_folder(&mut self, p: &Path) {
        Self::push(&mut self.folders, p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trips_and_fills_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        std::fs::write(&p, "[editor]\ntab_size = 2\n[appearance]\nseed_color = \"#FF0000\"\n").unwrap();
        let c = Config::load_from(&p);
        assert_eq!(c.editor.tab_size, 2);
        assert_eq!(c.editor.font_size, 14.0);
        assert_eq!(c.appearance.seed_color, "#FF0000");
        c.save_to(&p).unwrap();
        assert_eq!(Config::load_from(&p), c);
        assert!(Config::load_from(&dir.path().join("missing.toml")).show_sidebar);
    }

    #[test]
    fn recent_is_deduplicated_and_capped() {
        let mut r = Recent::default();
        for i in 0..30 {
            r.add_file(Path::new(&format!("/f{i}")));
        }
        r.add_file(Path::new("/f10"));
        assert_eq!(r.files.len(), RECENT_MAX);
        assert_eq!(r.files[0], PathBuf::from("/f10"));
        assert_eq!(r.files.iter().filter(|p| **p == Path::new("/f10")).count(), 1);
    }
}
