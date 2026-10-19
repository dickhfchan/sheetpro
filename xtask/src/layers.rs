//! Dependency-layer rule: a crate may depend only on strictly lower layers (plus the
//! explicitly allowed same-layer edge sp_ops → sp_formula). UI crates only in L6/apps.

use anyhow::{Result, bail};

const LAYERS: &[(&str, u8)] = &[
    ("sp_core", 0),
    ("sp_model", 1),
    ("sp_formula", 2),
    ("sp_ops", 2),
    ("sp_render", 3),
    ("sp_io", 4),
    ("sp_engine", 5),
    ("sp_ui", 6),
    ("sheetpro", 7),
    ("sp", 7),
];

const ALLOWED_SAME_LAYER: &[(&str, &str)] = &[("sp_ops", "sp_formula")];
const UI_CRATES: &[&str] = &[
    "egui",
    "eframe",
    "egui_kittest",
    "egui-wgpu",
    "egui_wgpu",
    "winit",
    "wgpu",
    // `image` is allowed: sp_render (L3) decodes pictures with PNG/JPEG/GIF/BMP only.
];

fn layer(name: &str) -> Option<u8> {
    LAYERS.iter().find(|(n, _)| *n == name).map(|(_, l)| *l)
}

pub fn run() -> Result<()> {
    let root = crate::root();
    let mut problems = Vec::new();
    let mut dirs = Vec::new();
    for d in ["crates", "apps"] {
        for e in std::fs::read_dir(root.join(d))? {
            dirs.push(e?.path());
        }
    }
    for dir in dirs {
        let manifest = dir.join("Cargo.toml");
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            continue;
        };
        let doc: toml::Table = toml::from_str(&text)?;
        let name = doc
            .get("package")
            .and_then(|p| p.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or_default()
            .to_string();
        let Some(my) = layer(&name) else {
            problems.push(format!("{name}: not registered in xtask/src/layers.rs"));
            continue;
        };
        let deps = doc
            .get("dependencies")
            .and_then(|d| d.as_table())
            .cloned()
            .unwrap_or_default();
        for dep in deps.keys() {
            if UI_CRATES.contains(&dep.as_str()) && my < 6 {
                problems.push(format!(
                    "{name} (L{my}) depends on UI crate {dep}; UI libraries belong in L6+"
                ));
            }
            if let Some(dl) = layer(dep)
                && dl >= my
                && !ALLOWED_SAME_LAYER.contains(&(name.as_str(), dep.as_str()))
            {
                problems.push(format!(
                    "{name} (L{my}) depends on {dep} (L{dl}): may only depend on lower layers"
                ));
            }
        }
    }
    if problems.is_empty() {
        println!("layers: ok ({} crates)", LAYERS.len());
        Ok(())
    } else {
        for p in &problems {
            eprintln!("layers: {p}");
        }
        bail!("{} layer violation(s)", problems.len())
    }
}
