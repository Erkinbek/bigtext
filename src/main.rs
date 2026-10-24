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
    let mut args = std::env::args_os().skip(1);
    while let Some(a) = args.next() {
        match a.to_str() {
            Some("--bench-ui") => {
                let file = args.next().map(PathBuf::from).expect("--bench-ui needs a file");
                bench = Some((file, PathBuf::from("bench-ui-report.txt")));
            }
            Some("--screenshot") => screenshot = args.next().map(PathBuf::from),
            Some("--self-test") => self_test = Some(PathBuf::from("self-test-report.txt")),
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
            let persist = bench.is_none() && self_test.is_none();
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
            Ok(Box::new(app))
        }),
    )
}
