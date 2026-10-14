//! The app icon, drawn in code (original artwork: a rounded tile with a small grid).
//! Used for the window icon and, via `sheetpro --write-icon out.png`, for the macOS
//! `.icns` built by `cargo xtask package`.

/// Straight RGBA pixels of a `size`×`size` icon.
pub fn rgba(size: u32) -> Vec<u8> {
    let n = size.max(16);
    let s = n as f32;
    let mut px = Vec::with_capacity((n * n * 4) as usize);
    let bg_top = [38.0, 104.0, 168.0];
    let bg_bot = [22.0, 70.0, 125.0];
    let margin = s * 0.06;
    let radius = s * 0.20;
    // Sheet area inside the tile.
    let (gx0, gy0, gx1, gy1) = (s * 0.20, s * 0.22, s * 0.80, s * 0.80);
    let line = (s * 0.018).max(1.0);
    for y in 0..n {
        for x in 0..n {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let cov = rounded_rect_coverage(fx, fy, margin, margin, s - margin, s - margin, radius);
            if cov <= 0.0 {
                px.extend_from_slice(&[0, 0, 0, 0]);
                continue;
            }
            let t = ((fy - margin) / (s - 2.0 * margin)).clamp(0.0, 1.0);
            let mut c = [
                bg_top[0] + (bg_bot[0] - bg_top[0]) * t,
                bg_top[1] + (bg_bot[1] - bg_top[1]) * t,
                bg_top[2] + (bg_bot[2] - bg_top[2]) * t,
            ];
            if fx >= gx0 && fx <= gx1 && fy >= gy0 && fy <= gy1 {
                // White sheet, header row tinted, 4 columns × 5 rows of grid lines.
                let header = fy < gy0 + (gy1 - gy0) / 5.0;
                c = if header {
                    [214.0, 230.0, 245.0]
                } else {
                    [255.0, 255.0, 255.0]
                };
                let col_w = (gx1 - gx0) / 4.0;
                let row_h = (gy1 - gy0) / 5.0;
                let on_v = ((fx - gx0) % col_w) < line || (gx1 - fx) < line;
                let on_h = ((fy - gy0) % row_h) < line || (gy1 - fy) < line;
                // Highlighted "active cell" in the second row, second column.
                let active = fx > gx0 + col_w
                    && fx < gx0 + 2.0 * col_w
                    && fy > gy0 + row_h
                    && fy < gy0 + 2.0 * row_h;
                if active {
                    c = [255.0, 196.0, 64.0];
                }
                if on_v || on_h {
                    c = [120.0, 150.0, 180.0];
                }
            }
            let a = (cov * 255.0).round().clamp(0.0, 255.0) as u8;
            px.extend_from_slice(&[c[0] as u8, c[1] as u8, c[2] as u8, a]);
        }
    }
    px
}

/// Anti-aliased coverage (0..1) of pixel centre (x, y) in a rounded rectangle.
fn rounded_rect_coverage(x: f32, y: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> f32 {
    let cx = x.clamp(x0 + r, x1 - r);
    let cy = y.clamp(y0 + r, y1 - r);
    let d = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt();
    (r + 0.5 - d).clamp(0.0, 1.0)
}

pub fn icon_data(size: u32) -> egui::IconData {
    let n = size.max(16);
    egui::IconData {
        rgba: rgba(n),
        width: n,
        height: n,
    }
}

/// Write the icon as a PNG (for packaging).
pub fn write_png(path: &std::path::Path, size: u32) -> Result<(), String> {
    let n = size.clamp(16, 2048);
    let img = image::RgbaImage::from_raw(n, n, rgba(n)).ok_or("icon buffer size mismatch")?;
    img.save(path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn icon_has_transparent_corners_and_opaque_centre() {
        let n = 64u32;
        let px = super::rgba(n);
        assert_eq!(px.len(), (n * n * 4) as usize);
        assert_eq!(px[3], 0);
        let centre = ((n / 2 * n + n / 2) * 4 + 3) as usize;
        assert_eq!(px[centre], 255);
    }
}
