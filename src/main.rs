// У релізі на Windows не показуємо чорне консольне вікно.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod audio;
mod dsp;
mod preset;
mod settings;

use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Голосомат")
            .with_inner_size([960.0, 760.0])
            .with_min_inner_size([760.0, 520.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Голосомат",
        options,
        Box::new(|cc| Ok(Box::new(app::HolosomatApp::new(cc)))),
    )
}
