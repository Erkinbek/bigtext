//! BigText desktop application entry point.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod bench_ui;
mod self_test;
mod view;

use std::path::PathBuf;
use std::time::Instant;

fn main() -> eframe::Result {
    let process_start = Instant::now();
    let mut files = Vec::new();
    let mut bench: Option<(PathBuf, PathBuf)> = None;
    let mut screenshot: Option<PathBuf> = None;
    let mut scenario = bench_ui::Scenario::Scroll;
    let mut self_test: Option<PathBuf> = None;
    let mut tour: Option<PathBuf> = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(a) = args.next() {
        match a.to_str() {
            Some("--bench-ui") => {
                let file = args.next().map(PathBuf::from).expect("--bench-ui needs a file");
                bench = Some((file, PathBuf::from("bench-ui-report.txt")));
            }
            Some("--screenshot") => screenshot = args.next().map(PathBuf::from),
            Some("--self-test") => self_test = Some(PathBuf::from("self-test-report.txt")),
            Some("--screenshots") => tour = Some(args.next().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("docs/img"))),
            Some("--scenario") => {
                let s = args.next().and_then(|s| s.into_string().ok()).unwrap_or_default();
                scenario = match s.split_once(':') {
                    Some(("search", p)) => bench_ui::Scenario::Search(p.to_string()),
                    _ if s == "edit" => bench_ui::Scenario::Edit,
                    _ if s == "view" => bench_ui::Scenario::View { hex: false, wrap: false },
                    _ if s == "wrap" => bench_ui::Scenario::View { hex: false, wrap: true },
                    _ if s == "hex" => bench_ui::Scenario::View { hex: true, wrap: false },
                    _ if s == "close-search" => bench_ui::Scenario::CloseDuring { replace: false },
                    _ if s == "close-replace" => bench_ui::Scenario::CloseDuring { replace: true },
                    _ => bench_ui::Scenario::Scroll,
                };
            }
            Some("--report") if self_test.is_some() => self_test = args.next().map(PathBuf::from),
            Some("--report") => {
                if let (Some(b), Some(p)) = (bench.as_mut(), args.next()) {
                    b.1 = PathBuf::from(p);
                }
            }
            _ => files.push(PathBuf::from(a)),
        }
    }

    // The screenshot tour opens its big file itself, after the demo files are created.
    let tour_file = tour.as_ref().map(|_| files.pop().unwrap_or_else(|| PathBuf::from("testdata/random-lines-10g.txt")));

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("BigText")
        .with_inner_size([1200.0, 800.0])
        .with_drag_and_drop(true);
    // Without an icon eframe shows its own egui logo in the title bar and taskbar.
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon-256.png")) {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    let result = eframe::run_native(
        "BigText",
        options,
        Box::new(move |cc| {
            let persist = bench.is_none() && self_test.is_none() && tour.is_none();
            let mut app = app::BigTextApp::new(cc, files, process_start, persist);
            if let Some((file, report)) = bench {
                let mut b = bench_ui::BenchUi::new(file, report);
                b.screenshot = screenshot.clone();
                b.scenario = scenario;
                app.bench = Some(b);
            }
            if let Some(report) = self_test {
                let dir = screenshot.as_ref().and_then(|p| p.parent().map(|d| d.to_path_buf()));
                app.self_test = Some(self_test::SelfTest::new(report, dir));
            }
            if let (Some(dir), Some(file)) = (tour, tour_file) {
                let _ = std::fs::create_dir_all(&dir);
                app.self_test = Some(self_test::SelfTest::tour(dir, file));
            }
            Ok(Box::new(app))
        }),
    );
    if let Err(e) = &result {
        // A release build on Windows has no console, so without this the window just never appears.
        rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title("BigText")
            .set_description(format!(
                "BigText could not open its window:\n\n{e}\n\nBigText draws with OpenGL 2.0 or newer; updating the graphics driver usually helps."
            ))
            .set_buttons(rfd::MessageButtons::Ok)
            .show();
    }
    result
}
