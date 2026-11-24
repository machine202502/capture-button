# Capture Button project rules

This file applies to work in this repository. It does not change the global
agent rules; it only records the project-specific workflow.

## Every completed change

1. Bump at least the patch version in `Cargo.toml` and `installer/CaptureButton.iss`; let `Cargo.lock` follow. The app and the installer must report the new version.
2. Regenerate or verify the UI snapshots in `crates/app/tests/snapshots` and the site screenshots in `site/assets/img`; update every image that changed.
3. Run the project's checks and tests and show the commands and their results: `cargo test --workspace`, `cargo test --test e2e -- --ignored`, and `cargo clippy --workspace --all-targets -- -D warnings`.
4. Build the release installer so `dist` contains the latest `CaptureButton-<version>-windows-x64-setup.msi`. Set `FFMPEG_DIR` to `vcpkg_installed\x64-windows-static` and run `scripts\package.ps1`. Packaging uses Velopack, so it needs the .NET 8 SDK and `vpk` (`dotnet tool install -g vpk`). It also writes the update feed to `site\updates\`. The user needs the installer in `dist`, not the bare executable.
5. Never commit or push unless the user explicitly asks.

An explicit version instruction (patch, minor, major, or "fix the version")
overrides the automatic bump for that one change only. On the next change,
resume the automatic patch bump unless the user explicitly says otherwise
again.

## Platform layer

The application depends on one platform crate, `platform`. Under it:

- `crates/platform-core` — OS-neutral vocabulary and policy: value types,
  `frame`, `geom`, translated catalogs, the palette, local time, logging, output
  folders and file names, the capture-source rules, the tray menu model, and the
  whole area-selection model (layout, hit-testing, annotation shapes, keys).
  Nothing here calls the operating system.
- `crates/platform` — the crate the app uses. It composes `platform-core` with
  the backend and holds the modules that need both: `camera` (nokhwa),
  `audio` (cpal), `update` (Velopack), the `i18n`/`logging` initialisation, and
  the `screen`/`region` glue.
- `crates/platform-windows` — the backend: host window and message loop,
  tray, timers, screen capture, registry, dialogs, clipboard, sounds,
  devices, and the native GDI area overlay.

The application is Windows-only; `platform-core` and `app` must not gain backend
types or calls.

### Platform code never leaks into `platform-core`

`platform-core` stays OS-neutral. Forbidden there:

- any compile-time backend selection;
- backend vocabulary: handles, API names, constants;
- conversions into a platform representation.

Keep the shared vocabulary and policy in `platform-core` (for example `Key`)
and let the backend convert it to its own representation. A platform type in the
core signature is the same leak even when the body is trivial.

Lead agents must state this rule in every task brief and check the returned
diff for violations before accepting it; a delegated agent working as a lead
for others passes the rule down the same way.

Rule of thumb: what the user sees or does is decided in `platform-core`; what
talks to the OS is `platform-windows`; anything that needs both is glue in
`platform`.

### Area overlay renderers

The overlay policy is `platform_core::region::Model`; a renderer only draws it
and feeds pointer/key events back. The build has two:

- `platform-windows/src/region.rs` — the native GDI overlay, the default. The
  frozen frame and the dim stay plain GDI (one fast blit); the chrome and the
  annotation strokes go through `platform-windows/src/gdiplus.rs`, which draws
  them anti-aliased so they match the Slint overlay;
- `crates/app/src/overlay.rs` + `crates/app/ui/overlay.slint` — the Slint
  overlay, which renders the same policy with Slint.

Settings → **Performance and overlay** → **Overlay renderer** selects it. The
build offers both and defaults to GDI. The stored value is `region_renderer` in
the settings store (`0` = GDI, `1` = Slint); a saved value naming a renderer the
build does not have falls back to the platform default. The GDI path must keep
working when the Slint one is selected, and the Slint path must fall back to GDI
(with a log line) if it cannot start.

## Snapshots and e2e

`crates/app/tests/e2e.rs` is the single end-to-end and snapshot tool. It drives
the hidden native `CaptureButton.window` with `WM_COMMAND` and captures the
visible Slint window with `PrintWindow`.

Regenerate the images:

```powershell
$env:FFMPEG_DIR = "vcpkg_installed\x64-windows-static"
$env:EZCAP_UPDATE_SNAPSHOTS = "1"
cargo test --test e2e ui_snapshots -- --ignored
```

This writes `crates/app/tests/snapshots/about-popup.png`,
`crates/app/tests/snapshots/main-capture-100.png`,
`crates/app/tests/snapshots/main-settings-100.png`, and the full-window sources
`site/assets/about.png` and `site/assets/capture-controls.png` (1124×1000 at
this display). The site itself ships resized copies in `site/assets/img`; update
them from those sources when the window layout changes, and adjust the
`width`/`height` attributes in the three `site/*/index.html` files if the
dimensions change.

Run the e2e checks with `cargo test --test e2e -- --ignored`. Without
`EZCAP_UPDATE_SNAPSHOTS`, `ui_snapshots` compares `crates/app/tests/snapshots`
pixel by pixel and writes `*.actual.png` on a mismatch; the site sources are
written only in update mode.

`.tools/` is a gitignored scratch area for task-local utilities. It is not part
of the official project, its scripts may be stale, and agents must not rely on
it as the snapshot or build path. Put new one-off tools there; use
`crates/app/tests/e2e.rs` and `scripts/package.ps1` for the official work.

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

`crates/app/tests/e2e.rs` intentionally skips the network check in `--test-mode`
unless `CAPTURE_BUTTON_UPDATE_FEED` points at a local feed. The 0.3.x Inno Setup
install is not a Velopack install: 0.4.x has to be installed once manually,
then later versions update from About.
