#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod app;

fn main() -> eframe::Result {
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/tailout-256.png"))
        .expect("Embedded Tailout icon must be a valid PNG");
    let options = eframe::NativeOptions {
        centered: true,
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Tailout")
            .with_icon(icon)
            .with_inner_size([1360.0, 920.0])
            .with_min_inner_size([1100.0, 760.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Tailout",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}
