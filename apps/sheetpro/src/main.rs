//! SheetPro desktop app.
//!
//! `sheetpro [book.xlsx] [--control-port 7878] [--write-icon icon.png [--icon-size 1024]]`

mod desktop;
mod forms;
mod icon;

use serde_json::json;
use sp_engine::Session;
use sp_engine::settings;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::mpsc;

pub type ControlRx = mpsc::Receiver<(String, mpsc::Sender<String>)>;

/// The app's context, set at startup: the control server wakes the app with it, so requests
/// are answered while the window is minimized or the screen is locked (eframe then runs no
/// UI pass, only `App::logic`, and only when a repaint is requested).
static CONTROL_WAKER: std::sync::OnceLock<egui::Context> = std::sync::OnceLock::new();

fn start_control_server(port: u16) -> std::io::Result<ControlRx> {
    // Loopback only: the control channel never listens on a network interface.
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    let (tx, rx) = mpsc::channel::<(String, mpsc::Sender<String>)>();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let tx = tx.clone();
            std::thread::spawn(move || {
                let Ok(read_half) = stream.try_clone() else {
                    return;
                };
                let mut write_half = stream;
                for line in BufReader::new(read_half).lines().map_while(Result::ok) {
                    let (rtx, rrx) = mpsc::channel();
                    if tx.send((line, rtx)).is_err() {
                        return;
                    }
                    if let Some(ctx) = CONTROL_WAKER.get() {
                        ctx.request_repaint();
                    }
                    let out = rrx.recv().unwrap_or_else(|_| {
                        json!({"ok": false, "error": "app closed"}).to_string()
                    });
                    if writeln!(write_half, "{out}").is_err() {
                        return;
                    }
                }
            });
        }
    });
    Ok(rx)
}

/// Crash reports: write the panic message and a backtrace to the reports folder, then run
/// the default hook (stderr).
fn install_panic_hook(dir: Option<PathBuf>) {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(dir) = &dir {
            let msg = format!(
                "{info}\n\nbacktrace:\n{}",
                std::backtrace::Backtrace::force_capture()
            );
            settings::log_line(&format!("panic: {info}"));
            if let Ok(p) = sp_engine::commands::app_cmds::write_crash_report(dir, &msg) {
                eprintln!("sheetpro: crash report written to {}", p.display());
            }
        }
        default(info);
    }));
}

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut port: Option<u16> = None;
    let mut files: Vec<String> = Vec::new();
    let mut write_icon: Option<PathBuf> = None;
    let mut icon_size = 1024u32;
    let mut i = 0;
    while let Some(a) = args.get(i) {
        match a.as_str() {
            "--control-port" => {
                port = args.get(i + 1).and_then(|p| p.parse().ok());
                i += 2;
                continue;
            }
            "--write-icon" => {
                write_icon = args.get(i + 1).map(PathBuf::from);
                i += 2;
                continue;
            }
            "--icon-size" => {
                icon_size = args.get(i + 1).and_then(|p| p.parse().ok()).unwrap_or(1024);
                i += 2;
                continue;
            }
            "--version" => {
                println!("sheetpro {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            // macOS passes a process serial number when launched from Finder.
            s if s.starts_with("-psn_") => {}
            s => files.push(s.to_string()),
        }
        i += 1;
    }
    if let Some(p) = write_icon {
        if let Err(e) = icon::write_png(&p, icon_size) {
            eprintln!("sheetpro: {e}");
            std::process::exit(1);
        }
        return Ok(());
    }

    let app_dir = std::env::var_os("SHEETPRO_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(settings::platform_app_dir);
    if let Some(d) = &app_dir {
        settings::init_app_dir(d.clone());
    }
    install_panic_hook(app_dir.clone());
    settings::log_line(&format!(
        "start sheetpro {} pid {} ({} {})",
        env!("CARGO_PKG_VERSION"),
        std::process::id(),
        std::env::consts::OS,
        std::env::consts::ARCH
    ));

    let prefs = settings::current();
    let mut session = Session::new();
    prefs.apply_to_new(&mut session);
    for f in &files {
        match session.execute("file.open", &json!({ "path": f })) {
            Ok(_) => sp_engine::commands::app_cmds::note_recent(std::path::Path::new(f)),
            Err(e) => {
                eprintln!("sheetpro: {e}");
                settings::log_line(&format!("open {f}: {e}"));
            }
        }
    }
    let control = match port.map(start_control_server) {
        Some(Ok(rx)) => Some(rx),
        Some(Err(e)) => {
            eprintln!("sheetpro: control server: {e}");
            None
        }
        None => None,
    };
    #[allow(unused_mut)]
    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_title("SheetPro")
            .with_icon(icon::icon_data(256)),
        ..Default::default()
    };
    // macOS: winit's default app menu binds Cmd+Q to an immediate terminate, which would skip
    // the unsaved-changes prompt; without it Cmd+Q reaches the app like any other shortcut.
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::EventLoopBuilderExtMacOS;
        options.event_loop_builder = Some(Box::new(|b| {
            b.with_default_menu(false);
        }));
    }
    let result = eframe::run_native(
        "SheetPro",
        options,
        Box::new(move |cc| {
            let _ = CONTROL_WAKER.set(cc.egui_ctx.clone());
            Ok(Box::new(desktop::Desktop::new(
                session, control, app_dir, prefs,
            )))
        }),
    );
    settings::log_line("exit");
    result
}
