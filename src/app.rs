//! App state, document management and the frame loop.

use crate::buffer::{Indent, Sel};
use crate::cli::OpenRequest;
use crate::config::{Config, Recent};
use crate::document::{Doc, External};
use crate::encoding::{LineEnding, TextEncoding};
use crate::files::{self, LoadError, Stamp};
use crate::search::{FolderSearch, Query, WalkOptions};
use crate::swap::{self, SwapEntry};
use crate::tree::{self, Tree};
use material_3_ui::keys::take_key;
use egui::{Key, Modifiers};
use material_3_ui::sys::task::Task;
use material_3_ui::theme::ThemeManager;
use material_3_ui::toasts::Toasts;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Requests from the UI, handled after the frame is drawn.
#[derive(Clone, Debug)]
pub enum Action {
    New,
    OpenDialog,
    OpenFolderDialog,
    Open(OpenRequest),
    OpenFolder(PathBuf),
    CloseFolder,
    Save(u64),
    SaveAs(u64),
    SaveAll,
    SaveAsRoot(u64),
    Close(u64),
    Discard(u64),
    CloseOthers(u64),
    CloseRight(u64),
    ReopenClosed,
    Activate(usize),
    MoveTab(usize, usize),
    Reload(u64),
    KeepMine(u64),
    SetEncoding(u64, TextEncoding),
    Reinterpret(u64, &'static encoding_rs::Encoding),
    SetLineEnding(u64, LineEnding),
    SetIndent(u64, Indent),
    ConvertIndent(u64, Indent),
    SetLanguage(u64, String),
    Recover(SwapEntry),
    DiscardSwap(PathBuf),
    NewEntry { dir: PathBuf, folder: bool },
    Rename(PathBuf),
    Delete(PathBuf),
    CopyPath(PathBuf),
    ShowInFiles(PathBuf),
    Reveal(PathBuf),
    RefreshTree,
    FolderSearch,
    ReplaceInFolder,
    OpenLink(String),
    Quit,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SideMode {
    Files,
    Search,
}

pub enum Dialog {
    ConfirmClose(u64),
    ConfirmQuit,
    SaveAs { id: u64, path: String, overwrite: bool },
    SaveAsRoot(u64),
    Recovery(Vec<SwapEntry>),
    NewEntry { dir: PathBuf, folder: bool, name: String, error: Option<String> },
    Rename { path: PathBuf, name: String, error: Option<String> },
    Delete(PathBuf),
    ReplaceInFolder,
    Preferences,
}

pub struct FindBar {
    pub open: bool,
    pub replace_open: bool,
    pub query: Query,
    pub replace: String,
    pub current: Option<usize>,
    pub focus: bool,
    pub last: Option<(u64, Query)>,
}

pub struct FolderSearchUi {
    pub query: Query,
    pub replace: String,
    pub show_replace: bool,
    pub run: Option<FolderSearch>,
    pub collapsed: HashSet<PathBuf>,
    pub focus: bool,
    pub error: Option<String>,
}

pub struct QuickOpen {
    pub query: String,
    pub selected: usize,
}

pub struct Workspace {
    pub root: PathBuf,
    pub tree: Tree,
    pub index: Task<Vec<PathBuf>>,
    pub selected: Option<PathBuf>,
    watcher: Option<notify::RecommendedWatcher>,
    watched: HashSet<PathBuf>,
    events: flume::Receiver<()>,
    refresh_at: Option<Instant>,
}

impl Workspace {
    fn open(root: &Path, opts: WalkOptions, ctx: &egui::Context) -> Self {
        let (tx, events) = flume::unbounded();
        let repaint = ctx.clone();
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res {
                if !ev.kind.is_access() {
                    let _ = tx.send(());
                    repaint.request_repaint_after(Duration::from_millis(300));
                }
            }
        })
        .ok();
        let mut ws = Self {
            root: root.to_path_buf(),
            tree: Tree::open(root, opts),
            index: Task::default(),
            selected: None,
            watcher,
            watched: HashSet::new(),
            events,
            refresh_at: None,
        };
        ws.watch(root);
        ws
    }

    pub fn watch(&mut self, dir: &Path) {
        use notify::Watcher;
        if self.watched.insert(dir.to_path_buf()) {
            if let Some(w) = &mut self.watcher {
                let _ = w.watch(dir, notify::RecursiveMode::NonRecursive);
            }
        }
    }

    pub fn toggle(&mut self, dir: &Path) {
        self.tree.toggle(dir);
        self.watch(dir);
    }

    /// Re-read the tree shortly after the file system changed.
    fn poll(&mut self) {
        if self.events.try_iter().count() > 0 {
            self.refresh_at = Some(Instant::now() + Duration::from_millis(250));
        }
        if self.refresh_at.is_some_and(|t| Instant::now() >= t) {
            self.refresh_at = None;
            self.tree.refresh();
            // The quick-open index is rebuilt next time it's needed.
            self.index = Task::default();
        }
    }

    pub fn ensure_index(&mut self, ctx: &egui::Context) {
        let root = self.root.clone();
        let opts = self.tree.opts;
        self.index.ensure(ctx, move || tree::index(&root, opts, 200_000));
    }
}

/// What a file dialog running in the background returns.
enum Picked {
    Files(Vec<PathBuf>),
    Folder(PathBuf),
    SaveAs(u64, PathBuf),
    Nothing,
}

pub struct App {
    pub config: Config,
    pub theme: ThemeManager,
    pub toasts: Toasts<Action>,
    pub recent: Recent,
    pub docs: Vec<Doc>,
    pub active: usize,
    next_id: u64,
    untitled_n: usize,
    pub workspace: Option<Workspace>,
    pub side: SideMode,
    pub find: FindBar,
    pub folder: FolderSearchUi,
    pub quick: Option<QuickOpen>,
    pub goto: Option<String>,
    pub dialog: Option<Dialog>,
    pub actions: Vec<Action>,
    picker: Task<Picked>,
    root_save: Task<(u64, PathBuf, Result<Stamp, String>)>,
    last_disk_check: Instant,
    title: String,
    closed: Vec<PathBuf>,
    allow_close: bool,
    pub lang_filter: String,
    swap_dir: PathBuf,
    pub had_overlay: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext, args: crate::cli::Args, stdin: Option<String>) -> Self {
        let config = Config::load();
        let theme = ThemeManager::new(&cc.egui_ctx, &config.appearance);
        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
        // Load the grammars while the window opens.
        std::thread::spawn(|| {
            let _ = crate::syntax::syntaxes();
        });
        let mut app = Self {
            config,
            theme,
            toasts: Toasts::default(),
            recent: Recent::load(),
            docs: Vec::new(),
            active: 0,
            next_id: 1,
            untitled_n: 0,
            workspace: None,
            side: SideMode::Files,
            find: FindBar { open: false, replace_open: false, query: Query::default(), replace: String::new(), current: None, focus: false, last: None },
            folder: FolderSearchUi { query: Query::default(), replace: String::new(), show_replace: false, run: None, collapsed: HashSet::new(), focus: false, error: None },
            quick: None,
            goto: None,
            dialog: None,
            actions: Vec::new(),
            picker: Task::default(),
            root_save: Task::default(),
            last_disk_check: Instant::now(),
            title: String::new(),
            closed: Vec::new(),
            allow_close: false,
            lang_filter: String::new(),
            swap_dir: swap::dir(),
            had_overlay: false,
        };
        if let Some(folder) = &args.folder {
            app.open_folder(folder, &cc.egui_ctx);
        }
        for req in args.files {
            app.open(req);
        }
        if let Some(text) = stdin {
            let id = app.new_doc();
            let doc = app.doc_mut(id).expect("just made");
            doc.buffer.insert(&text);
            doc.buffer.set_caret(0);
            doc.detect_language();
        }
        if app.docs.is_empty() && app.workspace.is_none() {
            app.new_doc();
        }
        let orphans = swap::orphans(&app.swap_dir);
        if !orphans.is_empty() {
            app.dialog = Some(Dialog::Recovery(orphans));
        }
        app
    }

    // ------------------------------------------------------------ docs

    pub fn doc(&self) -> Option<&Doc> {
        self.docs.get(self.active)
    }

    pub fn doc_mut_active(&mut self) -> Option<&mut Doc> {
        self.docs.get_mut(self.active)
    }

    pub fn doc_mut(&mut self, id: u64) -> Option<&mut Doc> {
        self.docs.iter_mut().find(|d| d.id == id)
    }

    fn index_of(&self, id: u64) -> Option<usize> {
        self.docs.iter().position(|d| d.id == id)
    }

    pub fn new_doc(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.untitled_n += 1;
        let mut doc = Doc::untitled(id, self.untitled_n, &self.config.editor);
        doc.view.focus = true;
        self.docs.push(doc);
        self.active = self.docs.len() - 1;
        id
    }

    /// The pristine "Untitled" tab, which opening a file replaces.
    fn pristine_untitled(&self) -> Option<usize> {
        (self.docs.len() == 1)
            .then_some(0)
            .filter(|&i| self.docs[i].path.is_none() && self.docs[i].buffer.len_chars() == 0 && !self.docs[i].buffer.can_undo())
    }

    pub fn open(&mut self, req: OpenRequest) {
        let path = std::path::absolute(&req.path).unwrap_or(req.path.clone());
        let real = files::resolve(&path);
        if let Some(i) = self.docs.iter().position(|d| d.path.as_ref().is_some_and(|p| files::resolve(p) == real)) {
            self.active = i;
            self.goto_pos(req.line, req.col);
            return;
        }
        let id = self.next_id;
        let doc = match files::load(&path) {
            Ok(loaded) => {
                let mut doc = Doc::from_file(id, &path, loaded, &self.config.editor);
                if doc.lossy {
                    self.toasts.info(format!("Some bytes in {} weren't valid {}", doc.title(), doc.encoding.label()));
                }
                if doc.mixed_endings {
                    self.toasts.info(format!("{} mixed line endings; they'll be saved as {}", doc.title(), doc.line_ending.label()));
                }
                doc.view.focus = true;
                doc
            }
            Err(LoadError::Io(_)) if !path.exists() && path.parent().is_some_and(Path::is_dir) => {
                // A new file: saving creates it.
                let mut doc = Doc::untitled(id, 0, &self.config.editor);
                doc.path = Some(path.clone());
                doc.detect_language();
                doc.view.focus = true;
                doc
            }
            Err(e) => {
                self.toasts.error(format!("Couldn't open {}: {e}", path.display()));
                return;
            }
        };
        self.next_id += 1;
        self.recent.add_file(&path);
        self.recent.save();
        if let Some(i) = self.pristine_untitled() {
            self.docs.remove(i);
        }
        self.docs.push(doc);
        self.active = self.docs.len() - 1;
        if let Some(ws) = &mut self.workspace {
            ws.selected = Some(path.clone());
        }
        self.goto_pos(req.line, req.col);
    }

    /// Put the caret at a 1-based line and column of the active document.
    pub fn goto_pos(&mut self, line: Option<usize>, col: Option<usize>) {
        let Some(line) = line else { return };
        let Some(doc) = self.doc_mut_active() else { return };
        let l = line.saturating_sub(1).min(doc.buffer.len_lines().saturating_sub(1));
        let c = col.map_or(0, |c| c.saturating_sub(1)).min(doc.buffer.line_len(l));
        let pos = doc.buffer.line_start(l) + c;
        doc.buffer.set_caret(pos);
        doc.view.center_line = Some(l);
        doc.view.focus = true;
    }

    pub fn open_folder(&mut self, path: &Path, ctx: &egui::Context) {
        let path = std::path::absolute(path).unwrap_or(path.to_path_buf());
        if !path.is_dir() {
            self.toasts.error(format!("{} isn't a folder", path.display()));
            return;
        }
        self.workspace = Some(Workspace::open(&path, self.walk_opts(), ctx));
        self.config.show_sidebar = true;
        self.recent.add_folder(&path);
        self.recent.save();
        self.folder.run = None;
    }

    pub fn walk_opts(&self) -> WalkOptions {
        WalkOptions { hidden: self.config.editor.show_hidden_files, ignored: self.config.editor.show_ignored_files }
    }

    pub fn save_config(&mut self) {
        if let Err(e) = self.config.save() {
            self.toasts.error(format!("Couldn't save preferences: {e}"));
        }
    }

    /// Save a document. Returns false when it needs a path or root first.
    pub fn save(&mut self, id: u64) -> bool {
        let settings = self.config.editor.clone();
        let Some(doc) = self.doc_mut(id) else { return false };
        let Some(path) = doc.path.clone() else {
            self.ask_save_as(id);
            return false;
        };
        let bytes = match doc.encode(&settings) {
            Ok(b) => b,
            Err(e) => {
                self.toasts.error(e);
                return false;
            }
        };
        if doc.readonly {
            self.dialog = Some(Dialog::SaveAsRoot(id));
            return false;
        }
        match files::save(&path, &bytes) {
            Ok(stamp) => {
                doc.saved(&path, stamp);
                self.recent.add_file(&path);
                self.recent.save();
                true
            }
            Err(files::SaveError::NeedsRoot) => {
                doc.readonly = true;
                self.dialog = Some(Dialog::SaveAsRoot(id));
                false
            }
            Err(e) => {
                self.toasts.error(format!("Couldn't save {}: {e}", path.display()));
                false
            }
        }
    }

    fn save_to(&mut self, id: u64, path: PathBuf) {
        if let Some(doc) = self.doc_mut(id) {
            let old = doc.path.replace(path.clone());
            doc.readonly = !files::writable(&path);
            if !self.save(id) {
                // Keep the old name when saving there failed.
                let needs_root = matches!(self.dialog, Some(Dialog::SaveAsRoot(_)));
                if let Some(doc) = self.doc_mut(id) {
                    if !needs_root {
                        doc.path = old;
                    }
                }
            } else if let Some(ws) = &mut self.workspace {
                if path.starts_with(&ws.root) {
                    ws.refresh_at = Some(Instant::now());
                }
            }
        }
    }

    fn ask_save_as(&mut self, id: u64) {
        let Some(doc) = self.docs.iter().find(|d| d.id == id) else { return };
        let path = match &doc.path {
            Some(p) => p.clone(),
            None => {
                let dir = self.workspace.as_ref().map(|w| w.root.clone()).or_else(|| std::env::current_dir().ok()).unwrap_or_else(material_3_ui::sys::paths::home);
                dir.join(format!("{}.txt", doc.title().to_lowercase().replace(' ', "-")))
            }
        };
        let path = material_3_ui::sys::paths::tilde(&path);
        self.dialog = Some(Dialog::SaveAs { id, path, overwrite: false });
    }

    fn close(&mut self, id: u64, force: bool) {
        let Some(i) = self.index_of(id) else { return };
        if !force && self.docs[i].is_dirty() {
            self.active = i;
            self.dialog = Some(Dialog::ConfirmClose(id));
            return;
        }
        let mut doc = self.docs.remove(i);
        doc.drop_swap();
        if let Some(p) = doc.path {
            self.closed.push(p);
        }
        if self.active >= i && self.active > 0 {
            self.active -= 1;
        }
        self.active = self.active.min(self.docs.len().saturating_sub(1));
        if let Some(d) = self.docs.get_mut(self.active) {
            d.view.focus = true;
        }
    }

    // --------------------------------------------------------- actions

    pub fn handle(&mut self, action: Action, ctx: &egui::Context) {
        match action {
            Action::New => {
                self.new_doc();
            }
            Action::OpenDialog => {
                let dir = self.dialog_dir();
                self.picker.spawn(ctx, move || {
                    rfd::FileDialog::new().set_directory(dir).set_title("Open files").pick_files().map_or(Picked::Nothing, Picked::Files)
                });
            }
            Action::OpenFolderDialog => {
                let dir = self.dialog_dir();
                self.picker.spawn(ctx, move || {
                    rfd::FileDialog::new().set_directory(dir).set_title("Open folder").pick_folder().map_or(Picked::Nothing, Picked::Folder)
                });
            }
            Action::Open(req) => self.open(req),
            Action::OpenFolder(p) => self.open_folder(&p, ctx),
            Action::CloseFolder => {
                self.workspace = None;
                self.folder.run = None;
            }
            Action::Save(id) => {
                self.save(id);
            }
            Action::SaveAs(id) => self.ask_save_as(id),
            Action::SaveAll => {
                let ids: Vec<u64> = self.docs.iter().filter(|d| d.is_dirty()).map(|d| d.id).collect();
                for id in ids {
                    if !self.save(id) {
                        break;
                    }
                }
            }
            Action::SaveAsRoot(id) => {
                let settings = self.config.editor.clone();
                let Some(doc) = self.doc_mut(id) else { return };
                let Some(path) = doc.path.clone() else { return };
                match doc.encode(&settings) {
                    Ok(bytes) => {
                        self.root_save.spawn(ctx, move || {
                            let r = material_3_ui::fs::pkexec_write(&path, &bytes, false)
                                .and_then(|()| files::stamp(&files::resolve(&path)).ok_or_else(|| "couldn't read the file back".to_string()));
                            (id, path, r)
                        });
                    }
                    Err(e) => self.toasts.error(e),
                }
            }
            Action::Close(id) => self.close(id, false),
            Action::Discard(id) => self.close(id, true),
            Action::CloseOthers(id) => {
                let ids: Vec<u64> = self.docs.iter().map(|d| d.id).filter(|d| *d != id).collect();
                for other in ids {
                    if self.dialog.is_some() {
                        break;
                    }
                    self.close(other, false);
                }
            }
            Action::CloseRight(id) => {
                if let Some(i) = self.index_of(id) {
                    let ids: Vec<u64> = self.docs[i + 1..].iter().map(|d| d.id).collect();
                    for other in ids {
                        if self.dialog.is_some() {
                            break;
                        }
                        self.close(other, false);
                    }
                }
            }
            Action::ReopenClosed => {
                if let Some(p) = self.closed.pop() {
                    self.open(OpenRequest { path: p, line: None, col: None });
                }
            }
            Action::Activate(i) => {
                if i < self.docs.len() {
                    self.active = i;
                    self.docs[i].view.focus = true;
                }
            }
            Action::MoveTab(from, to) => {
                if from < self.docs.len() && to <= self.docs.len() && from != to {
                    let doc = self.docs.remove(from);
                    let to = if to > from { to - 1 } else { to };
                    self.docs.insert(to, doc);
                    self.active = to;
                }
            }
            Action::Reload(id) => {
                let Some(doc) = self.doc_mut(id) else { return };
                let Some(path) = doc.path.clone() else { return };
                match files::load(&path) {
                    Ok(l) => doc.reload(l),
                    Err(e) => self.toasts.error(format!("Couldn't reload: {e}")),
                }
            }
            Action::KeepMine(id) => {
                if let Some(doc) = self.doc_mut(id) {
                    doc.external = None;
                    doc.stamp = doc.path.as_deref().and_then(files::stamp);
                    doc.buffer.mark_modified();
                }
            }
            Action::SetEncoding(id, enc) => {
                if let Some(doc) = self.doc_mut(id) {
                    if doc.encoding != enc {
                        doc.encoding = enc;
                        doc.buffer.mark_modified();
                    }
                }
            }
            Action::Reinterpret(id, enc) => {
                if let Some(doc) = self.doc_mut(id) {
                    if doc.buffer.is_dirty() {
                        self.toasts.error("Save or undo your changes before reopening with another encoding");
                    } else if !doc.reinterpret(enc) {
                        self.toasts.error("This document wasn't read from a file");
                    }
                }
            }
            Action::SetLineEnding(id, le) => {
                if let Some(doc) = self.doc_mut(id) {
                    if doc.line_ending != le {
                        doc.line_ending = le;
                        doc.mixed_endings = false;
                        doc.buffer.mark_modified();
                    }
                }
            }
            Action::SetIndent(id, indent) => {
                if let Some(doc) = self.doc_mut(id) {
                    doc.buffer.indent = indent;
                }
            }
            Action::ConvertIndent(id, indent) => {
                if let Some(doc) = self.doc_mut(id) {
                    convert_indent(doc, indent);
                }
            }
            Action::SetLanguage(id, name) => {
                if let Some(doc) = self.doc_mut(id) {
                    doc.set_language(&name);
                }
            }
            Action::Recover(entry) => {
                let id = match entry.data.path.clone() {
                    Some(p) => {
                        self.open(OpenRequest { path: p.clone(), line: None, col: None });
                        self.docs.iter().find(|d| d.path.as_deref() == Some(&p)).map(|d| d.id)
                    }
                    None => Some(self.new_doc()),
                };
                if let Some(doc) = id.and_then(|id| self.doc_mut(id)) {
                    doc.buffer.replace_all_text(&entry.data.text);
                    doc.sync();
                    doc.swap_file = Some(entry.file.clone());
                    if doc.path.is_none() {
                        doc.detect_language();
                    }
                    self.toasts.success(format!("Recovered {}", entry.data.title));
                }
                // Rewritten under this process's name on the next change.
                swap::remove(&entry.file);
                if let Some(doc) = id.and_then(|id| self.doc_mut(id)) {
                    doc.swap_file = None;
                    doc.swapped_version = u64::MAX;
                    doc.last_edit = Some(Instant::now());
                }
            }
            Action::DiscardSwap(file) => swap::remove(&file),
            Action::NewEntry { dir, folder } => {
                self.dialog = Some(Dialog::NewEntry { dir, folder, name: String::new(), error: None });
            }
            Action::Rename(path) => {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                self.dialog = Some(Dialog::Rename { path, name, error: None });
            }
            Action::Delete(path) => self.dialog = Some(Dialog::Delete(path)),
            Action::CopyPath(path) => {
                ctx.copy_text(path.display().to_string());
                self.toasts.info("Path copied");
            }
            Action::ShowInFiles(path) => {
                let dir = if path.is_dir() { path } else { path.parent().map(Path::to_path_buf).unwrap_or(path) };
                let fm = if material_3_ui::sys::cmd::exists("m3fm") { "m3fm" } else { "xdg-open" };
                if let Err(e) = material_3_ui::sys::cmd::spawn_detached(fm, &[&dir.to_string_lossy()]) {
                    self.toasts.error(e);
                }
            }
            Action::Reveal(path) => {
                if let Some(ws) = &mut self.workspace {
                    ws.tree.reveal(&path);
                    let mut dir = path.parent();
                    while let Some(d) = dir {
                        if !d.starts_with(&ws.root) {
                            break;
                        }
                        ws.watch(d);
                        dir = d.parent();
                    }
                    ws.selected = Some(path);
                    self.config.show_sidebar = true;
                    self.side = SideMode::Files;
                }
            }
            Action::RefreshTree => {
                if let Some(ws) = &mut self.workspace {
                    ws.tree.opts = WalkOptions { hidden: self.config.editor.show_hidden_files, ignored: self.config.editor.show_ignored_files };
                    ws.tree.refresh();
                    ws.index = Task::default();
                }
            }
            Action::FolderSearch => self.start_folder_search(ctx),
            Action::ReplaceInFolder => self.replace_in_folder(),
            Action::OpenLink(url) => {
                let _ = material_3_ui::sys::cmd::spawn_detached("xdg-open", &[&url]);
            }
            Action::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
        }
    }

    fn dialog_dir(&self) -> PathBuf {
        self.doc()
            .and_then(|d| d.path.as_ref())
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .or_else(|| self.workspace.as_ref().map(|w| w.root.clone()))
            .unwrap_or_else(material_3_ui::sys::paths::home)
    }

    pub fn pick_save_path(&mut self, id: u64, current: &str, ctx: &egui::Context) {
        let p = PathBuf::from(current);
        let dir = p.parent().map(Path::to_path_buf).unwrap_or_else(|| self.dialog_dir());
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.picker.spawn(ctx, move || {
            rfd::FileDialog::new()
                .set_directory(dir)
                .set_file_name(name)
                .set_title("Save as")
                .save_file()
                .map_or(Picked::Nothing, |p| Picked::SaveAs(id, p))
        });
    }

    pub fn finish_save_as(&mut self, id: u64, path: PathBuf) {
        self.dialog = None;
        self.save_to(id, path);
    }

    fn start_folder_search(&mut self, ctx: &egui::Context) {
        let Some(ws) = &self.workspace else { return };
        self.folder.error = None;
        match self.folder.query.compile() {
            Ok(Some(re)) => {
                let repaint = ctx.clone();
                self.folder.collapsed.clear();
                self.folder.run = Some(FolderSearch::start(ws.root.clone(), self.folder.query.clone(), re, self.walk_opts(), move || {
                    repaint.request_repaint()
                }));
            }
            Ok(None) => self.folder.run = None,
            Err(e) => self.folder.error = Some(e),
        }
    }

    fn replace_in_folder(&mut self) {
        let Some(run) = &self.folder.run else { return };
        let query = run.query.clone();
        let Ok(Some(re)) = query.compile() else { return };
        let paths: Vec<PathBuf> = run.results.iter().map(|f| f.path.clone()).collect();
        let with = self.folder.replace.clone();
        let (mut files_n, mut total, mut failed) = (0usize, 0usize, Vec::new());
        for path in paths {
            let real = files::resolve(&path);
            let open = self.docs.iter_mut().find(|d| d.path.as_ref().is_some_and(|p| files::resolve(p) == real));
            let r = match open {
                Some(doc) => Ok(doc.replace_all(&query, &with)),
                None => crate::search::replace_in_file(&path, &re, &with, query.regex),
            };
            match r {
                Ok(n) if n > 0 => {
                    files_n += 1;
                    total += n;
                }
                Ok(_) => {}
                Err(e) => failed.push(format!("{}: {e}", path.display())),
            }
        }
        self.folder.run = None;
        self.toasts.success(format!("Replaced {total} matches in {files_n} files (open files aren't saved yet)"));
        for f in failed.into_iter().take(3) {
            self.toasts.error(f);
        }
    }

    // ---------------------------------------------------- background

    /// Timers: swap files, autosave, disk checks, finished tasks.
    fn tick(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        if let Some(picked) = self.picker.take() {
            match picked {
                Picked::Files(files) => {
                    for path in files {
                        self.open(OpenRequest { path, line: None, col: None });
                    }
                }
                Picked::Folder(p) => self.open_folder(&p, ctx),
                Picked::SaveAs(id, p) => {
                    if let Some(Dialog::SaveAs { id: did, path, .. }) = &mut self.dialog {
                        if *did == id {
                            *path = p.display().to_string();
                        }
                    }
                }
                Picked::Nothing => {}
            }
        }
        if let Some((id, path, result)) = self.root_save.take() {
            match result {
                Ok(stamp) => {
                    if let Some(doc) = self.doc_mut(id) {
                        doc.saved(&path, stamp);
                        doc.readonly = true;
                    }
                    self.toasts.success(format!("Saved {} as administrator", path.display()));
                }
                Err(e) => self.toasts.error(format!("Couldn't save as administrator: {e}")),
            }
        }
        if let Some(ws) = &mut self.workspace {
            ws.poll();
        }
        if let Some(run) = &mut self.folder.run {
            run.poll();
        }

        let autosave = self.config.editor.autosave;
        let delay = Duration::from_secs(self.config.editor.autosave_delay_secs.max(1));
        let mut to_save = Vec::new();
        for doc in &mut self.docs {
            let idle = doc.last_edit.map(|t| now.duration_since(t));
            if doc.is_dirty() && doc.buffer.version() != doc.swapped_version && idle.is_some_and(|d| d > Duration::from_millis(1500)) {
                let file = doc.swap_file.clone().unwrap_or_else(|| swap::file_for(&self.swap_dir, doc.path.as_deref(), doc.id));
                let data = doc.swap_data();
                std::thread::spawn({
                    let file = file.clone();
                    move || {
                        let _ = swap::write(&file, &data);
                    }
                });
                doc.swap_file = Some(file);
                doc.swapped_version = doc.buffer.version();
            } else if !doc.is_dirty() && doc.swap_file.is_some() {
                doc.drop_swap();
            }
            if autosave && doc.path.is_some() && !doc.readonly && doc.external.is_none() && doc.buffer.is_dirty() && idle.is_some_and(|d| d > delay) {
                to_save.push(doc.id);
            }
        }
        for id in to_save {
            self.save(id);
        }

        let focused_now = ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::WindowFocused(true))));
        if focused_now || now.duration_since(self.last_disk_check) > Duration::from_millis(1500) {
            self.last_disk_check = now;
            let auto_reload = self.config.editor.auto_reload;
            let mut reloaded = Vec::new();
            for doc in &mut self.docs {
                if doc.check_disk() && doc.external == Some(External::Changed) && auto_reload && !doc.buffer.is_dirty() {
                    if let Some(path) = doc.path.clone() {
                        if let Ok(l) = files::load(&path) {
                            doc.reload(l);
                            reloaded.push(doc.title());
                        }
                    }
                }
            }
            for t in reloaded {
                self.toasts.info(format!("{t} changed on disk and was reloaded"));
            }
        }
        ctx.request_repaint_after(Duration::from_millis(1000));
    }

    // --------------------------------------------------------- input

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let c = Modifiers::COMMAND;
        let cs = Modifiers::COMMAND | Modifiers::SHIFT;
        let id = self.doc().map(|d| d.id);
        let mut act = |a: Action| self.actions.push(a);
        if take_key(ctx, cs, Key::S) {
            if let Some(id) = id {
                act(Action::SaveAs(id));
            }
        }
        if take_key(ctx, c, Key::S) {
            if let Some(id) = id {
                act(Action::Save(id));
            }
        }
        if take_key(ctx, Modifiers::COMMAND | Modifiers::ALT, Key::S) {
            act(Action::SaveAll);
        }
        if take_key(ctx, c, Key::N) {
            act(Action::New);
        }
        if take_key(ctx, cs, Key::O) {
            act(Action::OpenFolderDialog);
        }
        if take_key(ctx, c, Key::O) {
            act(Action::OpenDialog);
        }
        if take_key(ctx, c, Key::W) {
            if let Some(id) = id {
                act(Action::Close(id));
            }
        }
        if take_key(ctx, cs, Key::T) {
            act(Action::ReopenClosed);
        }
        if take_key(ctx, c, Key::Q) {
            act(Action::Quit);
        }
        let n = self.docs.len();
        if n > 0 {
            if take_key(ctx, c, Key::Tab) || take_key(ctx, c, Key::PageDown) {
                self.actions.push(Action::Activate((self.active + 1) % n));
            }
            if take_key(ctx, cs, Key::Tab) || take_key(ctx, c, Key::PageUp) {
                self.actions.push(Action::Activate((self.active + n - 1) % n));
            }
            for (i, k) in [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9].into_iter().enumerate() {
                if take_key(ctx, Modifiers::ALT, k) {
                    self.actions.push(Action::Activate(if i == 8 { n - 1 } else { i.min(n - 1) }));
                }
            }
        }
        if take_key(ctx, c, Key::P) {
            self.quick = Some(QuickOpen { query: String::new(), selected: 0 });
        }
        if take_key(ctx, c, Key::G) {
            self.goto = Some(String::new());
        }
        if take_key(ctx, cs, Key::F) {
            if self.workspace.is_some() {
                self.config.show_sidebar = true;
                self.side = SideMode::Search;
                self.folder.focus = true;
                if let Some(sel) = self.selected_single_line() {
                    self.folder.query.text = sel;
                }
            } else {
                self.toasts.info("Open a folder to search across files");
            }
        }
        let find = take_key(ctx, c, Key::F);
        let replace = take_key(ctx, c, Key::H);
        if find || replace {
            self.find.open = true;
            self.find.replace_open |= replace;
            self.find.focus = true;
            if let Some(sel) = self.selected_single_line() {
                self.find.query.text = sel;
            }
        }
        if take_key(ctx, Modifiers::NONE, Key::F3) {
            self.find_step(1);
        }
        if take_key(ctx, Modifiers::SHIFT, Key::F3) {
            self.find_step(-1);
        }
        if take_key(ctx, c, Key::B) {
            self.config.show_sidebar = !self.config.show_sidebar;
            self.save_config();
        }
        if take_key(ctx, cs, Key::M) {
            if let Some(d) = self.doc_mut_active() {
                d.preview = !d.preview;
            }
        }
        if take_key(ctx, Modifiers::ALT, Key::Z) {
            self.config.editor.word_wrap = !self.config.editor.word_wrap;
            self.save_config();
        }
        if take_key(ctx, c, Key::Plus) || take_key(ctx, c, Key::Equals) || take_key(ctx, cs, Key::Equals) {
            self.config.editor.font_size = (self.config.editor.font_size + 1.0).min(40.0);
            self.save_config();
        }
        if take_key(ctx, c, Key::Minus) {
            self.config.editor.font_size = (self.config.editor.font_size - 1.0).max(8.0);
            self.save_config();
        }
        if take_key(ctx, c, Key::Num0) {
            self.config.editor.font_size = crate::config::EditorSettings::default().font_size;
            self.save_config();
        }
        if take_key(ctx, c, Key::Comma) {
            self.dialog = Some(Dialog::Preferences);
        }
        if take_key(ctx, Modifiers::NONE, Key::F5) {
            if let Some(id) = id {
                self.actions.push(Action::Reload(id));
            }
        }
    }

    fn selected_single_line(&self) -> Option<String> {
        let doc = self.doc()?;
        let s = doc.buffer.primary();
        (!s.is_empty() && s.end() - s.start() < 500).then(|| doc.buffer.slice(s.start(), s.end())).filter(|t| !t.contains('\n'))
    }

    /// Select the next (or previous) find match.
    pub fn find_step(&mut self, dir: isize) {
        let query = self.find.query.clone();
        let Some(doc) = self.docs.get_mut(self.active) else { return };
        let caret = doc.buffer.primary();
        let matches = doc.find_matches(&query).matches.clone();
        if matches.is_empty() {
            self.find.current = None;
            return;
        }
        let i = match self.find.current.filter(|&i| matches.get(i).is_some_and(|m| m.0 == caret.start() && m.1 == caret.end())) {
            Some(i) => (i as isize + dir).rem_euclid(matches.len() as isize) as usize,
            None if dir >= 0 => matches.iter().position(|m| m.0 >= caret.end()).unwrap_or(0),
            None => matches.iter().rposition(|m| m.1 <= caret.start()).unwrap_or(matches.len() - 1),
        };
        select_match(doc, matches[i]);
        self.find.current = Some(i);
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let title = match self.doc() {
            Some(d) => format!("{}{} — Material 3 Text", if d.is_dirty() { "• " } else { "" }, d.title()),
            None => "Material 3 Text".into(),
        };
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }

    fn handle_close_request(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.viewport().close_requested()) || self.allow_close {
            return;
        }
        if self.docs.iter().any(Doc::is_dirty) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.dialog = Some(Dialog::ConfirmQuit);
        } else {
            for d in &mut self.docs {
                d.drop_swap();
            }
        }
    }

    /// Save everything for quitting; false if something still needs input.
    pub fn save_all_for_quit(&mut self) -> bool {
        let ids: Vec<u64> = self.docs.iter().filter(|d| d.is_dirty()).map(|d| d.id).collect();
        for id in ids {
            if !self.save(id) {
                return false;
            }
        }
        true
    }

    pub fn quit_now(&mut self, ctx: &egui::Context) {
        for d in &mut self.docs {
            d.drop_swap();
        }
        self.allow_close = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

pub fn select_match(doc: &mut Doc, (a, b): (usize, usize)) {
    doc.buffer.set_sels(vec![Sel::new(a, b)]);
    doc.buffer.seal();
    let line = doc.buffer.char_to_line(a);
    doc.view.center_line = Some(line);
}

/// Re-indent leading whitespace from the current style to `to`.
fn convert_indent(doc: &mut Doc, to: Indent) {
    let from = doc.buffer.indent;
    let mut changes = Vec::new();
    for l in 0..doc.buffer.len_lines() {
        let text = doc.buffer.line_text(l);
        let lead: String = text.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
        if lead.is_empty() {
            continue;
        }
        let width = crate::layout::width(&lead, from.width.max(1));
        let levels = width / to.width.max(1);
        let rest = width % to.width.max(1);
        let new = if to.tabs { "\t".repeat(levels) + &" ".repeat(rest) } else { " ".repeat(width) };
        if new != lead {
            let ls = doc.buffer.line_start(l);
            changes.push((ls, ls + lead.chars().count(), new));
        }
    }
    doc.buffer.transact(changes, crate::buffer::After::Map, crate::buffer::EditKind::Other);
    doc.buffer.indent = to;
    doc.sync();
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.theme.poll(&self.config.appearance) {
            ctx.request_repaint();
        }
        if let Some(e) = self.theme.take_error() {
            self.toasts.error(e);
        }
        self.handle_close_request(&ctx);
        if self.dialog.is_none() && self.quick.is_none() && self.goto.is_none() {
            self.shortcuts(&ctx);
        }
        self.tick(&ctx);
        crate::ui::draw(self, ui);
        for a in std::mem::take(&mut self.actions) {
            self.handle(a, &ctx);
        }
        self.update_title(&ctx);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        self.theme.palette().surface.to_normalized_gamma_f32()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_spaces_to_tabs_and_back() {
        let mut d = Doc::untitled(1, 1, &crate::config::EditorSettings::default());
        d.buffer = crate::buffer::Buffer::new("a\n    b\n        c\n");
        d.buffer.indent = Indent { tabs: false, width: 4 };
        convert_indent(&mut d, Indent { tabs: true, width: 4 });
        assert_eq!(d.buffer.text(), "a\n\tb\n\t\tc\n");
        convert_indent(&mut d, Indent { tabs: false, width: 2 });
        assert_eq!(d.buffer.text(), "a\n    b\n        c\n", "tabs expand at the old width");
    }
}
