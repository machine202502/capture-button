# Capture Button

Windows program for screen, region, and audio capture. It records MKV, GIF, and WebP, takes screenshots, and can keep running from the tray.

The source in this repository is under the MIT license. A release build statically links FFmpeg and x264 under the GNU GPL v2 or later, so the distributed executable is covered by the GPL. `COPYING` is that license. The overlay icons are Phosphor Icons under the MIT license (`assets/Phosphor.LICENSE`). `NOTICE` names these obligations. The generated `THIRD-PARTY.txt` adds the copyright text for the linked codecs and for the Rust crates, and it travels with the zip and the installer. Source and downloads: https://github.com/machine202502/capture-button

## Interface language

English is compiled into the executable, so the program is fully usable with no
locale files at all. The portable zip and the installer also ship the three
catalogs.

Russian and Simplified Chinese are plain JSON catalogs in the `locales/` folder
next to `CaptureButton.exe`; the installer ships `ru.json`, `en.json` and `zh.json`.
A translation can be fixed or added without rebuilding: drop another
`<code>.json` into that folder, or into `%LOCALAPPDATA%\CaptureButton\locales`, and it
appears in **Settings в†’ Language**. "System" follows the Windows display language
and falls back to the built-in English when no catalog matches. The language is
applied immediately.

## Build

Windows x64, a stable Rust toolchain, and a static FFmpeg install from `vcpkg.json` (codecs: H.264, WebP, MP3). Point `FFMPEG_DIR` at that prefix. It must contain `lib` and `lib/pkgconfig`.

Camera frames that arrive as MJPEG are decoded by the Rust `image` crate, not by FFmpeg. The slim FFmpeg build leaves the `mjpeg` decoder out, so the camera path must not go back to `avcodec`.

```powershell
$env:FFMPEG_DIR = "C:\path\to\vcpkg_installed\x64-windows-static"
cargo build --release --bin CaptureButton
```

The executable is `target\release\CaptureButton.exe`. Icons, the no-signal card, and the completion sound are compiled into it.

The workspace is split by responsibility, and `app` depends on one platform
crate only:

- `crates/app` — the Slint window, the capture engine and FFmpeg encoding;
- `crates/platform` — the crate the app uses: composes the neutral layer with
  the selected backend (camera, audio, updates, screen and region glue);
- `crates/platform-core` — OS-neutral types and policy, including the whole
  area-selection model; no Win32 there;
- `crates/platform-windows` — the Win32 backend: window, tray, capture,
  registry, dialogs, and the native GDI area overlay.

[`docs/platform-layer.md`](docs/platform-layer.md) describes the contract and
how to add another OS.

`scripts\package.ps1` builds that executable, then writes a portable zip, a
per-user MSI installer (with the standard Windows Installer wizard) and the
update feed into `dist\` (and `site\updates\`). Packaging needs the .NET 8 SDK
and the `vpk` tool:

```powershell
dotnet tool install -g vpk
```

Pushing a new `version` in `Cargo.toml` to `master` runs the same packaging on
GitHub and publishes the files when that version has no release yet.

## Updates

The installed app checks `https://machine202502.github.io/capture-button/updates`
when **About** opens. If a newer version is published, the update button turns
green; pressing it downloads the Velopack package and restarts the app. Portable
builds update manually.

The feed and its `.nupkg` packages are copied from the GitHub release into the
site by `.github/workflows/site.yml`. Existing Inno Setup 0.3.x installs must
install 0.4.2 once by hand; later versions update from the About window.

0.4.x is an MSI. Uninstall it through **Settings → Apps** (or `msiexec /x` on
the MSI). Do not delete the install folder and do not use
`Update.exe --uninstall`: Velopack rejects that command for MSI installs.

## Test

Use the same `FFMPEG_DIR`.

```powershell
cargo test --workspace
```

That runs the unit tests of every crate. The end-to-end checks live in `crates/app/tests/e2e.rs` and are ignored:

```powershell
cargo test --test e2e -- --ignored
```

They drive the hidden native window and capture the visible Slint window. Set
`EZCAP_UPDATE_SNAPSHOTS` to any value to replace the images in
`crates/app/tests/snapshots` and to write the `site/assets/about.png` and
`site/assets/capture-controls.png` sources; only `crates/app/tests/snapshots` is
compared on a normal run.

