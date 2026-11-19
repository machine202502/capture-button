# Capture Button project rules

This file applies to work in this repository. It does not change the global
agent rules; it only records the project-specific workflow.

## Every completed change

1. Bump at least the patch version in `Cargo.toml` and `installer/CaptureButton.iss`; let `Cargo.lock` follow. The app and the installer must report the new version.
2. Regenerate or verify the UI snapshots in `tests/snapshots` and the site screenshots in `site/assets`; update every image that changed.
3. Run the project's checks and tests and show the commands and their results.
4. Build the release installer so `dist` contains the latest `CaptureButton-<version>-windows-x64-setup.msi`. Set `FFMPEG_DIR` to `vcpkg_installed\x64-windows-static` and run `scripts\package.ps1`. Packaging uses Velopack, so it needs the .NET 8 SDK and `vpk` (`dotnet tool install -g vpk`). It also writes the update feed to `site\updates\`. The user needs the installer in `dist`, not the bare executable.
5. Never commit or push unless the user explicitly asks.

An explicit version instruction (patch, minor, major, or "fix the version")
overrides the automatic bump for that one change only. On the next change,
resume the automatic patch bump unless the user explicitly says otherwise
again.

## Snapshots and e2e

`tests/e2e.rs` is the single end-to-end and snapshot tool. It drives the hidden
native `CaptureButton.window` with `WM_COMMAND` and captures the visible Slint
window with `PrintWindow`.

Regenerate the images:

```powershell
$env:FFMPEG_DIR = "vcpkg_installed\x64-windows-static"
$env:EZCAP_UPDATE_SNAPSHOTS = "1"
cargo test --test e2e ui_snapshots -- --ignored
```

This writes `tests/snapshots/about-popup.png`,
`tests/snapshots/main-capture-100.png`, `tests/snapshots/main-settings-100.png`,
`site/assets/about.png`, and `site/assets/capture-controls.png` (1124×1000 at
this display). If the dimensions change, update the `width`/`height` attributes
in the three `site/*/index.html` files.

Run the e2e checks with `cargo test --test e2e -- --ignored`. Without
`EZCAP_UPDATE_SNAPSHOTS`, `ui_snapshots` compares pixels and writes
`*.actual.png` on a mismatch.

`.tools/` is a gitignored scratch area for task-local utilities. It is not part
of the official project, its scripts may be stale, and agents must not rely on
it as the snapshot or build path. Put new one-off tools there; use
`tests/e2e.rs` and `scripts/package.ps1` for the official work.

If the snapshot or site-screenshot tooling is missing or cannot run, stop and
ask instead of silently skipping the images.

## Updates

The installed app uses Velopack and checks
`https://machine202502.github.io/capture-button/updates` when About opens.
`scripts\package.ps1` writes `releases.win.json` and the `.nupkg` to
`site\updates\`; `.github/workflows/site.yml` downloads the same files from the
GitHub release before deploying Pages. A release must attach
`releases.win.json` and the `.nupkg` packages, or the feed cannot work.

The Velopack pack id is `CaptureButtonApp`, so the install root is
`%LocalAppData%\CaptureButtonApp`, separate from the app's
`%LocalAppData%\CaptureButton` logs/locales. Keep that split: putting logs in
the install root makes a fresh Velopack install fail because the directory
already exists.

0.4.x is an MSI. Uninstall it through Settings → Apps, or with `msiexec /x` on
the MSI. `Update.exe --uninstall` refuses MSI installs with "MSI installation
detected"; deleting the folder leaves the MSI registration and makes the next
installer offer repair instead of a fresh install.

`tests/e2e.rs` intentionally skips the network check in `--test-mode` unless
`CAPTURE_BUTTON_UPDATE_FEED` points at a local feed. The 0.3.x Inno Setup
install is not a Velopack install: 0.4.x has to be installed once manually,
then later versions update from About.
