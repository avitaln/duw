mod app;
mod scan;

use eframe::egui;

fn main() -> eframe::Result {
    let initial = std::env::args().nth(1);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("duw — disk usage")
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([760.0, 480.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "duw",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, initial)))),
    )
}
