//! `cargo xtask package`: a release build of the desktop app, laid out for trial users.
//!
//! - macOS: `dist/SheetPro.app` (Info.plist, binary, `.icns` drawn by `sheetpro
//!   --write-icon`, ad-hoc signed when `codesign` exists; not notarized) plus
//!   `dist/SheetPro-<version>-macos-<arch>.zip` (via `ditto`).
//! - Elsewhere: `dist/sheetpro-<version>-<os>-<arch>/` with the binary and docs.
//!
//! Then `cargo xtask smoke` runs on the bundled binary (skip with `SP_SKIP_SMOKE=1`).

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const BUNDLE_ID: &str = "org.sheetpro.SheetPro";
/// (file name in the .iconset, pixel size)
const ICONSET: [(&str, u32); 10] = [
    ("icon_16x16.png", 16),
    ("icon_16x16@2x.png", 32),
    ("icon_32x32.png", 32),
    ("icon_32x32@2x.png", 64),
    ("icon_128x128.png", 128),
    ("icon_128x128@2x.png", 256),
    ("icon_256x256.png", 256),
    ("icon_256x256@2x.png", 512),
    ("icon_512x512.png", 512),
    ("icon_512x512@2x.png", 1024),
];

fn target_dir(root: &Path) -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .map(|p| if p.is_absolute() { p } else { root.join(p) })
        .unwrap_or_else(|| root.join("target"))
}

fn tool_exists(name: &str) -> bool {
    Command::new("which")
        .arg(name)
        .output()
        .is_ok_and(|o| o.status.success())
}

fn run(cmd: &mut Command) -> Result<()> {
    let status = cmd.status().with_context(|| format!("running {cmd:?}"))?;
    if !status.success() {
        bail!("{cmd:?} failed");
    }
    Ok(())
}

fn copy_docs(root: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    for f in [
        "README.md",
        "ATTRIBUTION.md",
        "LICENSE",
        "LICENSE-MIT",
        "LICENSE-APACHE",
    ] {
        let src = root.join(f);
        if src.exists() {
            std::fs::copy(&src, dest.join(f)).with_context(|| format!("copying {f}"))?;
        }
    }
    Ok(())
}

/// The Info.plist of the bundle.
pub fn info_plist(version: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>SheetPro</string>
  <key>CFBundleDisplayName</key><string>SheetPro</string>
  <key>CFBundleIdentifier</key><string>{BUNDLE_ID}</string>
  <key>CFBundleExecutable</key><string>sheetpro</string>
  <key>CFBundleIconFile</key><string>SheetPro</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>{version}</string>
  <key>CFBundleVersion</key><string>{version}</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSHumanReadableCopyright</key><string>SheetPro contributors. MIT OR Apache-2.0. Not affiliated with Microsoft.</string>
</dict>
</plist>
"#
    )
}

fn macos_bundle(root: &Path, bin: &Path, dist: &Path) -> Result<PathBuf> {
    let app = dist.join("SheetPro.app");
    if app.exists() {
        std::fs::remove_dir_all(&app).context("removing the old bundle")?;
    }
    let contents = app.join("Contents");
    let macos = contents.join("MacOS");
    let res = contents.join("Resources");
    std::fs::create_dir_all(&macos)?;
    std::fs::create_dir_all(&res)?;
    std::fs::copy(bin, macos.join("sheetpro")).context("copying the binary")?;
    std::fs::write(contents.join("Info.plist"), info_plist(VERSION))?;
    std::fs::write(contents.join("PkgInfo"), "APPL????")?;
    copy_docs(root, &res)?;

    // Icon: PNGs from the app itself, packed by iconutil.
    let iconset = dist.join("SheetPro.iconset");
    let _ = std::fs::remove_dir_all(&iconset);
    std::fs::create_dir_all(&iconset)?;
    for (name, size) in ICONSET {
        run(Command::new(bin)
            .arg("--write-icon")
            .arg(iconset.join(name))
            .arg("--icon-size")
            .arg(size.to_string()))?;
    }
    if tool_exists("iconutil") {
        run(Command::new("iconutil")
            .args(["-c", "icns", "-o"])
            .arg(res.join("SheetPro.icns"))
            .arg(&iconset))?;
    } else {
        println!("package: iconutil not found; bundle has no .icns (the window icon still works)");
    }
    let _ = std::fs::remove_dir_all(&iconset);

    if tool_exists("codesign") {
        // Ad-hoc signature so the bundle's seal matches its contents (Apple silicon
        // refuses to run unsigned code). Not a Developer ID signature: not notarized.
        run(Command::new("codesign")
            .args(["--force", "--deep", "--sign", "-"])
            .arg(&app))?;
    }
    if tool_exists("ditto") {
        let zip = dist.join(format!(
            "SheetPro-{VERSION}-macos-{}.zip",
            std::env::consts::ARCH
        ));
        let _ = std::fs::remove_file(&zip);
        run(Command::new("ditto")
            .args(["-c", "-k", "--keepParent"])
            .arg(&app)
            .arg(&zip))?;
        println!("package: {}", zip.display());
    }
    Ok(app)
}

fn plain_folder(root: &Path, bin: &Path, dist: &Path) -> Result<PathBuf> {
    let dir = dist.join(format!(
        "sheetpro-{VERSION}-{}-{}",
        std::env::consts::OS,
        std::env::consts::ARCH
    ));
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir_all(&dir)?;
    let name = bin
        .file_name()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("sheetpro"));
    std::fs::copy(bin, dir.join(name)).context("copying the binary")?;
    copy_docs(root, &dir)?;
    Ok(dir)
}

pub fn run_package() -> Result<()> {
    let root = crate::root();
    crate::cargo(&["build", "--release", "-p", "sheetpro"])?;
    let exe = if cfg!(windows) {
        "sheetpro.exe"
    } else {
        "sheetpro"
    };
    let bin = target_dir(&root).join("release").join(exe);
    if !bin.exists() {
        bail!("release binary not found at {}", bin.display());
    }
    let dist = root.join("dist");
    std::fs::create_dir_all(&dist)?;
    let out = if cfg!(target_os = "macos") {
        macos_bundle(&root, &bin, &dist)?
    } else {
        plain_folder(&root, &bin, &dist)?
    };
    println!("package: {}", out.display());
    if std::env::var_os("SP_SKIP_SMOKE").is_some_and(|v| !v.is_empty()) {
        println!("package: smoke test skipped (SP_SKIP_SMOKE)");
        return Ok(());
    }
    crate::smoke::run(&crate::smoke::packaged_binary(&root))
}

#[cfg(test)]
mod tests {
    #[test]
    fn plist_names_the_binary_and_version() {
        let p = super::info_plist("1.2.3");
        assert!(p.contains("<key>CFBundleExecutable</key><string>sheetpro</string>"));
        assert!(p.contains("<string>1.2.3</string>"));
        assert!(!p.contains("Excel"));
    }
}
