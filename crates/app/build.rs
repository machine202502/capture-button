use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Put the catalogs next to the built executable, so `cargo run` and a plain
/// `cargo build` see the same `locales/` folder the installer ships. Packaging
/// still copies the executable alone, so the portable build stays English-only.
fn copy_locales() {
    println!("cargo:rerun-if-changed=../../locales");
    let Ok(out_dir) = std::env::var("OUT_DIR").map(PathBuf::from) else {
        return;
    };
    // OUT_DIR is <target>/<profile>/build/<package>-<hash>/out. Find the
    // `build` component instead of counting parents: the exact depth differs
    // between Cargo layouts and a wrong count silently loses the catalogs.
    let Some(build_dir) = out_dir
        .ancestors()
        .find(|path| path.file_name().is_some_and(|name| name == "build"))
    else {
        return;
    };
    let Some(profile_dir) = build_dir.parent() else {
        return;
    };
    let source = Path::new("../../locales");
    if !source.is_dir() {
        return;
    }
    let dest = profile_dir.join("locales");
    let _ = std::fs::create_dir_all(&dest);
    let Ok(entries) = std::fs::read_dir(source) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name() else {
            continue;
        };
        let _ = std::fs::copy(&path, dest.join(name));
    }
}

fn main() {
    slint_build::compile("ui/main.slint").unwrap();
    slint_build::compile("ui/overlay.slint").unwrap();
    println!("cargo:rerun-if-env-changed=FFMPEG_DIR");
    println!("cargo:rerun-if-changed=../../assets/CaptureButton.ico");
    println!("cargo:rerun-if-changed=../../assets/app.manifest");
    copy_locales();
    #[cfg(windows)]
    {
        let mut resource = winres::WindowsResource::new();
        resource.set_icon("../../assets/CaptureButton.ico");
        resource.set_manifest_file("../../assets/app.manifest");
        resource.set("ProductName", "Capture Button");
        resource.set("FileDescription", "Capture Button");
        resource.set(
            "Comments",
            "https://github.com/machine202502/capture-button — includes FFmpeg and x264 under the GNU GPL v2 or later (https://ffmpeg.org/legal.html) and Phosphor Icons under the MIT License (https://phosphoricons.com). Other notices are in THIRD-PARTY.txt.",
        );
        resource.set("OriginalFilename", "CaptureButton.exe");
        resource.compile().unwrap();
    }
    let Some(dir) = std::env::var_os("FFMPEG_DIR").map(PathBuf::from) else {
        return;
    };
    // FFMPEG_DIR is conventionally given relative to the workspace root, but the
    // build script's working directory is the package root (crates/app). Resolve
    // a relative prefix against the workspace root.
    let dir = if dir.is_absolute() {
        dir
    } else {
        let manifest = std::env::var_os("CARGO_MANIFEST_DIR")
            .map(PathBuf::from)
            .unwrap_or_default();
        manifest.join("..").join("..").join(dir)
    };
    let lib_dir = dir.join("lib");
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    let ffmpeg = [
        "avcodec",
        "avdevice",
        "avfilter",
        "avformat",
        "avutil",
        "swresample",
        "swscale",
    ];
    let mut libs = BTreeSet::new();
    if let Ok(entries) = std::fs::read_dir(lib_dir.join("pkgconfig")) {
        for entry in entries.flatten() {
            let Ok(text) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            for line in text.lines() {
                let Some(rest) = line
                    .strip_prefix("Libs.private:")
                    .or_else(|| line.strip_prefix("Libs:"))
                else {
                    continue;
                };
                for flag in rest.split_whitespace() {
                    if let Some(name) = flag.strip_prefix("-l") {
                        if !ffmpeg.contains(&name) {
                            libs.insert(name.to_string());
                        }
                    }
                }
            }
        }
    }
    for name in libs {
        if lib_dir.join(format!("{name}.lib")).is_file() {
            println!("cargo:rustc-link-lib=static={name}");
        } else {
            println!("cargo:rustc-link-lib={name}");
        }
    }
}
