//! Material 3 Text: a light code and text editor in the Material 3 family.

mod app;
mod buffer;
mod cli;
mod config;
mod document;
mod editor;
mod encoding;
mod files;
mod layout;
mod markdown;
mod search;
mod swap;
mod syntax;
mod tree;
mod ui;
mod ui_ext;

use std::io::Read;

fn main() -> eframe::Result {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"));
    let args = cli::parse(std::env::args().skip(1), &cwd, &|p| p.exists(), &|p| p.is_dir());
    if args.help {
        print!("{}", cli::USAGE);
        return Ok(());
    }
    if args.version {
        println!("m3-text {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let stdin = args.stdin.then(|| {
        let mut s = String::new();
        let _ = std::io::stdin().read_to_string(&mut s);
        s
    });
    eframe::run_native(
        "Material 3 Text",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_app_id("material_3_text")
                .with_title("Material 3 Text")
                .with_inner_size([1180.0, 780.0])
                .with_min_inner_size([520.0, 360.0]),
            ..Default::default()
        },
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, args, stdin)))),
    )
}
