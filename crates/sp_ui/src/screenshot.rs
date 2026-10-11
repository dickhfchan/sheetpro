//! Offscreen rendering of the real shell to PNG (egui_kittest + wgpu), so agents can verify
//! UI changes by looking at an image.

use crate::App;
use egui_kittest::Harness;
use sp_engine::Session;
use std::path::Path;

pub fn render_png(session: Session, size: (f32, f32), out: &Path) -> Result<(), String> {
    render_png_with_dialog(session, size, out, None)
}

/// Like [`render_png`], with a dialog open first (see `App::open_dialog` for the spec:
/// a name, optionally followed by a JSON object of field overrides).
pub fn render_png_with_dialog(
    session: Session,
    size: (f32, f32),
    out: &Path,
    dialog: Option<&str>,
) -> Result<(), String> {
    let mut app = App::new(session);
    if let Some(d) = dialog {
        app.open_dialog(d)?;
    }
    let mut harness = Harness::builder()
        .with_size(egui::Vec2::new(size.0, size.1))
        .wgpu()
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    // A few frames so layout settles (panels measure on the first pass).
    harness.run_steps(4);
    let img = harness.render()?;
    img.save(out)
        .map_err(|e| format!("saving {}: {e}", out.display()))
}
