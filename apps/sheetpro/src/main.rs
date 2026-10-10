//! SheetPro desktop app.
//!
//! `sheetpro [book.xlsx] [--control-port 7878]`

use serde_json::json;
use sp_engine::Session;
use sp_engine::control::{Action, handle_line};
use sp_ui::App;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::mpsc;

struct Desktop {
    app: App,
    /// Control-channel requests from the TCP thread, each with a reply channel.
    control: Option<mpsc::Receiver<(String, mpsc::Sender<String>)>>,
}

impl eframe::App for Desktop {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if let Some(rx) = &self.control {
            while let Ok((line, reply)) = rx.try_recv() {
                let (out, action) = handle_line(&mut self.app.session, &line);
                if action == Action::Quit {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
                let _ = reply.send(out);
            }
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(50));
        }
        let title = self.app.session.title();
        ui.ctx()
            .send_viewport_cmd(egui::ViewportCommand::Title(title));
        self.app.show(ui);
        if self.app.session.quit_requested {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

fn start_control_server(
    port: u16,
) -> std::io::Result<mpsc::Receiver<(String, mpsc::Sender<String>)>> {
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

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut session = Session::new();
    let mut port: Option<u16> = None;
    let mut i = 0;
    while let Some(a) = args.get(i) {
        if a == "--control-port" {
            port = args.get(i + 1).and_then(|p| p.parse().ok());
            i += 2;
            continue;
        }
        if let Err(e) = session.execute("file.open", &json!({ "path": a })) {
            eprintln!("sheetpro: {e}");
        }
        i += 1;
    }
    let control = match port.map(start_control_server) {
        Some(Ok(rx)) => Some(rx),
        Some(Err(e)) => {
            eprintln!("sheetpro: control server: {e}");
            None
        }
        None => None,
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_title("SheetPro"),
        ..Default::default()
    };
    eframe::run_native(
        "SheetPro",
        options,
        Box::new(move |_cc| {
            Ok(Box::new(Desktop {
                app: App::new(session),
                control,
            }))
        }),
    )
}
