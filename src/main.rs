#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod catalog;
mod detect;
mod elevate;
mod install;
mod inuse;
mod log;
mod source;
mod state;
mod ui;
mod version;

use eframe::egui;

fn main() -> eframe::Result {
    let catalog = catalog::load().expect("embedded catalog.toml is invalid");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([960.0, 600.0])
            .with_min_inner_size([640.0, 360.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Restock",
        options,
        Box::new(|cc| Ok(Box::new(ui::RestockApp::new(cc, catalog)))),
    )
}
