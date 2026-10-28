//! BigText desktop application entry point.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod bench_ui;
mod cli;
mod self_test;
mod view;

use std::path::PathBuf;
use std::time::Instant;

/// Shows a message on the console and, in a Windows release build (which has no console), in a
/// dialog.
fn tell(title: &str, text: &str, error: bool) {
    if error {
        eprintln!("{text}");
    } else {
        println!("{text}");
    }
    if cfg!(all(windows, not(debug_assertions))) {
        rfd::MessageDialog::new()
            .set_level(if error { rfd::MessageLevel::Error } else { rfd::MessageLevel::Info })
            .set_title(title)
            .set_description(text)
            .set_buttons(rfd::MessageButtons::Ok)
            .show();
    }
}

/// Copies standard input into a temporary file on a thread and returns the file, which the
/// editor then follows as it grows.
fn stdin_to_temp() -> std::io::Result<PathBuf> {
    let dir = bigtext::decompress::temp_dir().join(format!("{}-stdin", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("stdin.txt");
    let mut file = std::fs::File::create(&path)?;
    std::thread::Builder::new().name("stdin".into()).spawn(move || {
        let _ = std::io::copy(&mut std::io::stdin().lock(), &mut file);
    })?;
    Ok(path)
}

fn main() -> eframe::Result {
    let process_start = Instant::now();
    let mut user = cli::UserArgs::default();
    let mut errors = Vec::new();
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
            Some(s) => {
                if let Err(e) = cli::take(&mut user, s, &mut || args.next().and_then(|x| x.into_string().ok()), &|p| p.exists()) {
                    errors.push(e);
                }
            }
            // A path that is not valid Unicode.
            None => user.files.push((PathBuf::from(a), None)),
        }
    }
    if user.help {
        tell("BigText", cli::USAGE, false);
        return Ok(());
    }
    if !errors.is_empty() {
        tell("BigText", &format!("{}\n\n{}", errors.join("\n"), cli::USAGE), true);
        std::process::exit(2);
    }

    // The screenshot tour opens its big file itself, after the demo files are created.
    let tour_file = tour.as_ref().map(|_| user.files.pop().map(|f| f.0).unwrap_or_else(|| PathBuf::from("testdata/random-lines-10g.txt")));
    let stdin = match user.stdin.then(stdin_to_temp) {
        Some(Ok(p)) => Some(p),
        Some(Err(e)) => {
            tell("BigText", &format!("Could not read standard input: {e}"), true);
            None
        }
        None => None,
    };
    let start = app::StartOptions { files: user.files, find: user.find, follow: user.follow, stdin };

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
            let mut app = app::BigTextApp::new(cc, start, process_start, persist);
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
