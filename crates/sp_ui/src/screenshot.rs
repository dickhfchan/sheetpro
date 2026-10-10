//! Offscreen rendering of the real shell to PNG (egui_kittest + wgpu), so agents can verify
//! UI changes by looking at an image.

use crate::App;
use egui_kittest::Harness;
use sp_engine::Session;
use std::path::Path;

pub fn render_png(session: Session, size: (f32, f32), out: &Path) -> Result<(), String> {
    let app = App::new(session);
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
