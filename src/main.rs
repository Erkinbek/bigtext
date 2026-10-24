//! BigText desktop application entry point.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod bench_ui;
mod view;

use std::path::PathBuf;
use std::time::Instant;

fn main() -> eframe::Result {
    let process_start = Instant::now();
    let mut files = Vec::new();
    let mut bench: Option<(PathBuf, PathBuf)> = None;
    let mut screenshot: Option<PathBuf> = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(a) = args.next() {
        match a.to_str() {
            Some("--bench-ui") => {
                let file = args.next().map(PathBuf::from).expect("--bench-ui needs a file");
                bench = Some((file, PathBuf::from("bench-ui-report.txt")));
            }
            Some("--screenshot") => screenshot = args.next().map(PathBuf::from),
            Some("--report") => {
                if let (Some(b), Some(p)) = (bench.as_mut(), args.next()) {
                    b.1 = PathBuf::from(p);
                }
            }
            _ => files.push(PathBuf::from(a)),
        }
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("BigText")
            .with_inner_size([1200.0, 800.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "BigText",
        options,
        Box::new(move |cc| {
            let mut app = app::BigTextApp::new(cc, files, process_start);
            if let Some((file, report)) = bench {
                let mut b = bench_ui::BenchUi::new(file, report);
                b.screenshot = screenshot;
                app.bench = Some(b);
            }
            Ok(Box::new(app))
        }),
    )
}
