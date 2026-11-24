# Capture Button

Windows and Linux program for screen, region, and audio capture. It records MKV, GIF, and WebP, takes screenshots, and can keep running from the tray.

The source in this repository is under the MIT license. A release build statically links FFmpeg and x264 under the GNU GPL v2 or later, so the distributed executable is covered by the GPL. `COPYING` is that license. The overlay icons are Phosphor Icons under the MIT license (`assets/Phosphor.LICENSE`). `NOTICE` names these obligations. The generated `THIRD-PARTY.txt` adds the copyright text for the linked codecs and for the Rust crates, and it travels with the zip and the installer. Source and downloads: https://github.com/machine202502/capture-button

## Interface language

English is compiled into the executable, so the program is fully usable with no
locale files at all. The portable zip and the installer also ship the three
catalogs.

Russian and Simplified Chinese are plain JSON catalogs in the `locales/` folder
next to `CaptureButton.exe`; the installer ships `ru.json`, `en.json` and `zh.json`.
A translation can be fixed or added without rebuilding: drop another
`<code>.json` into that folder, or into `%LOCALAPPDATA%\CaptureButton\locales`, and it
appears in **Settings РІвЂ вЂ™ Language**. "System" follows the Windows display language
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

- `crates/app` вЂ” the Slint window, the capture engine and FFmpeg encoding;
- `crates/platform` вЂ” the crate the app uses: composes the neutral layer with
  the selected backend (camera, audio, updates, screen and region glue);
- `crates/platform-core` вЂ” OS-neutral types and policy, including the whole
  area-selection model; no Win32 there;
- `crates/platform-windows` вЂ” the Win32 backend: window, tray, capture,
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

## Linux

Linux x86_64 only. The release is built and tested on Ubuntu 24.04, so it needs
glibc 2.39 or newer: Ubuntu 24.04 and later, Debian 13 and later. The app runs
on X11 and Wayland; on Wayland screen capture goes through the XDG desktop
portal and PipeWire. Runtime dependencies are PipeWire and ALSA, fontconfig and
freetype, and the X11/XWayland libraries.

A release attaches two Linux files:

- `CaptureButton-<version>-linux-x64.AppImage` is portable and self-updating:
  run the file once, and later versions replace it in place.
- `CaptureButton-<version>-linux-x64.deb` installs the program under
  `/usr/lib/capture-button` with a `/usr/bin/capture-button` symlink, a desktop
  entry, and the icon.

Playing a recording in Totem needs `gstreamer1.0-libav`, which supplies the
H.264 and AAC decoders a stock Ubuntu does not ship, plus
`gstreamer1.0-plugins-good` for the Matroska demuxer. The `.deb` only
recommends them, and apt installs recommends by default; for the AppImage
install them separately
(`sudo apt install gstreamer1.0-libav gstreamer1.0-plugins-good`). A player
with its own decoders, such as VLC, does not need them.

System audio is recorded through `parec`, so it needs `pulseaudio-utils`
(a dependency of the `.deb`; for the AppImage install it separately). Settings
lists the `pactl` sinks, so a recording can follow the current output or use a
specific one; the system-audio volume is separate from the microphone volume.

Global hotkeys (Select area and Stop recording) use an X11 passive grab. On a
Wayland session that grab lives on XWayland, so the section is shown when
XWayland is available; the compositor forwards keys to it while an X11 window
has focus. Without XWayland (or on a native-Wayland-only setup) the section is
hidden. The `.deb` and AppImage need no extra packages for them.

The tray-video profile can record one chosen monitor on X11; on Wayland the
portal picker chooses the source, so the monitor row is hidden. Area selection
still covers every monitor.

**Open** hands a capture to the desktop's own application for its type, so the
desktop defaults decide what a saved file opens in. A stock Ubuntu can leave
PNG and GIF with ImageMagick's `display`, which paints a GIF's delta frames
green, and animated WebP with Shotwell, which shows a black picture. Set
normal defaults once:

```bash
xdg-mime default vlc.desktop video/x-matroska video/mp4 video/webm
xdg-mime default vlc.desktop audio/mpeg audio/ogg audio/x-wav audio/mp4
xdg-mime default org.gnome.eog.desktop image/png image/gif image/webp image/svg+xml
```

VLC plays the H.264/AAC MKV with its own decoders; Eye of GNOME (`eog`) or
Loupe show the screenshots, GIFs and animated WebP.

`scripts/package-linux.sh` builds both files and the Velopack update feed into
`dist/`. It needs the FFmpeg 8.1 prefix in `FFMPEG_DIR` (the same codec set as
the Windows build), the .NET 8 SDK with `vpk`, and `dpkg-deb`.

## Updates

The installed app checks `https://machine202502.github.io/capture-button/updates`
when **About** opens. If a newer version is published, the update button turns
green; pressing it downloads the Velopack package and restarts the app. Portable
builds update manually.

The feed and its `.nupkg` packages are copied from the GitHub release into the
site by `.github/workflows/site.yml`. Existing Inno Setup 0.3.x installs must
install 0.4.2 once by hand; later versions update from the About window.

0.4.x is an MSI. Uninstall it through **Settings в†’ Apps** (or `msiexec /x` on
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
