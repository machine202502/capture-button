#!/usr/bin/env bash
# Build Capture Button for Linux x64 and package it as a Velopack AppImage and
# a Debian package.
#
#   scripts/package-linux.sh               build release, pack, copy the AppImage,
#                                          the .deb and the feed into dist/
#   scripts/package-linux.sh --skip-build  pack the existing target/release/CaptureButton
#   scripts/package-linux.sh --debug       pack target/debug/CaptureButton instead of a
#                                          release build (for machines short on disk)
#   scripts/package-linux.sh --no-publish  do not copy the feed into site/updates
#
# Needs the FFmpeg development files the Linux backend links against, plus the
# .NET 8 SDK, vpk (`dotnet tool install -g vpk`) and dpkg-deb. The AppImage is
# the portable distribution: Velopack has no Linux installer, so running the
# file is the install. On update, UpdateNix replaces the AppImage at the path it
# was run from; there is no directory under ~/.local/share to look at. The .deb
# installs to /usr/lib/capture-button with a /usr/bin/capture-button symlink,
# a desktop entry, and the icon.
set -euo pipefail

usage() {
    cat <<'EOF'
Usage: scripts/package-linux.sh [--skip-build] [--debug] [--no-publish]

  --skip-build   pack the existing target/release/CaptureButton
  --debug        pack target/debug/CaptureButton and do not build in release
                 mode (the debug binary is much larger but needs no build space)
  --no-publish   leave site/updates untouched
EOF
}

build=1
profile=release
publish=1
for arg in "$@"; do
    case "$arg" in
        --skip-build) build=0 ;;
        --debug) profile=debug; build=0 ;;
        --no-publish) publish=0 ;;
        -h | --help) usage; exit 0 ;;
        *)
            echo "unknown option: $arg" >&2
            usage >&2
            exit 2
            ;;
    esac
done

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

version="$(grep -m1 -E '^[[:space:]]*version[[:space:]]*=' Cargo.toml 2>/dev/null \
    | sed -E 's/.*=[[:space:]]*"([^"]+)".*/\1/' || true)"
case "$version" in
    [0-9]*) ;;
    *)
        echo "error: could not read the [workspace.package] version from Cargo.toml" >&2
        exit 1
        ;;
esac

if ! command -v vpk >/dev/null 2>&1; then
    echo "error: vpk was not found. Install the .NET 8 SDK and run: dotnet tool install -g vpk" >&2
    exit 1
fi
if ! command -v dpkg-deb >/dev/null 2>&1; then
    echo "error: dpkg-deb was not found; install dpkg to build the .deb" >&2
    exit 1
fi

binary="$root/target/$profile/CaptureButton"
if [ "$build" -eq 1 ]; then
    available="$(df -Pk "$root" | awk 'NR == 2 { print $4 }')"
    if [ -n "$available" ] && [ "$available" -lt 1572864 ]; then
        echo "    warning: only $((available / 1024)) MiB free; a release build needs ~1.5 GiB."
        echo "             Use --debug or --skip-build if the build fails with ENOSPC."
    fi
    echo "==> Building Capture Button $version (release)"
    cargo build --release -p app
fi
if [ ! -f "$binary" ]; then
    echo "error: the build did not produce $binary" >&2
    echo "       Run without --skip-build/--debug, or build that profile first." >&2
    exit 1
fi
echo "    binary $binary ($(du -h "$binary" | cut -f1))"

stage="$root/dist/linux/CaptureButton-$version-linux-x64"
echo "==> Staging $stage"
rm -rf "$stage"
mkdir -p "$stage"
install -m 755 "$binary" "$stage/CaptureButton"

# The build script copies the catalogs next to the profile directory; ship them
# from the same profile when they are there and stay English-only when not.
if [ -d "$root/target/$profile/locales" ]; then
    mkdir -p "$stage/locales"
    cp -a "$root/target/$profile/locales/." "$stage/locales/"
else
    echo "    note: target/$profile/locales is missing, the package ships English only"
fi

# The GPL text belongs with the binary; the AppImage and the .deb both carry it.
for licence in COPYING NOTICE; do
    if [ -f "$root/$licence" ]; then
        install -m 644 "$root/$licence" "$stage/$licence"
    fi
done

# assets/CaptureButton-64.png is the 256x256 transparent icon. Fall back to the
# largest PNG in assets/ (file size stands in for dimensions, so no image
# tooling is needed).
icon="$root/assets/CaptureButton-64.png"
if [ ! -f "$icon" ]; then
    icon="$(find "$root/assets" -maxdepth 1 -type f -name '*.png' -printf '%s %p\n' \
        | sort -rn | head -n1 | cut -d' ' -f2- || true)"
fi
if [ ! -f "$icon" ]; then
    echo "error: no icon PNG found in assets/" >&2
    exit 1
fi

out="$root/dist/velopack"
rm -rf "$out"
mkdir -p "$out"

echo "==> vpk pack (channel linux)"
vpk pack \
    --packId CaptureButtonApp \
    --packVersion "$version" \
    --packDir "$stage" \
    --mainExe CaptureButton \
    --channel linux \
    --packTitle "Capture Button" \
    --packAuthors "machine202502" \
    --icon "$icon" \
    --outputDir "$out"

appimage="$out/CaptureButtonApp.AppImage"
feed="$out/releases.linux.json"
for file in "$appimage" "$feed"; do
    if [ ! -f "$file" ]; then
        echo "error: vpk pack did not produce $file" >&2
        exit 1
    fi
done

# vpk names the AppImage without a version because it replaces itself in place;
# the download copy carries the version so releases do not overwrite each other.
dist_image="$root/dist/CaptureButton-$version-linux-x64.AppImage"
cp -a "$appimage" "$dist_image"
chmod +x "$dist_image"
cp -a "$feed" "$root/dist/releases.linux.json"

echo "==> Building the .deb"
debroot="$root/dist/deb/capture-button"
deb="$root/dist/CaptureButton-$version-linux-x64.deb"
rm -rf "$root/dist/deb"
mkdir -p "$debroot/DEBIAN" \
    "$debroot/usr/lib/capture-button" \
    "$debroot/usr/bin" \
    "$debroot/usr/share/applications" \
    "$debroot/usr/share/icons/hicolor/256x256/apps" \
    "$debroot/usr/share/doc/capture-button"
install -m 755 "$binary" "$debroot/usr/lib/capture-button/CaptureButton"
if [ -d "$stage/locales" ]; then
    mkdir -p "$debroot/usr/lib/capture-button/locales"
    cp -a "$stage/locales/." "$debroot/usr/lib/capture-button/locales/"
fi
for licence in COPYING NOTICE; do
    if [ -f "$stage/$licence" ]; then
        install -m 644 "$stage/$licence" "$debroot/usr/share/doc/capture-button/$licence"
    fi
done
ln -s ../lib/capture-button/CaptureButton "$debroot/usr/bin/capture-button"
install -m 644 "$icon" \
    "$debroot/usr/share/icons/hicolor/256x256/apps/capture-button.png"

desktop="$root/assets/capture-button.desktop"
if [ ! -f "$desktop" ]; then
    echo "error: assets/capture-button.desktop is missing; the .deb needs it" >&2
    exit 1
fi
install -m 644 "$desktop" "$debroot/usr/share/applications/capture-button.desktop"

# Runtime dependencies follow the release binary's ldd, plus pulseaudio-utils:
# system audio is recorded through parec because ALSA/cpal cannot see the
# output monitor, so it is part of the recording feature and belongs in
# Depends. Ubuntu 24.04's t64 packages provide the old names (libasound2,
# libpipewire-0.3-0, libpng16-16), so these Debian-style names also resolve
# there. X11 and Wayland are loaded with dlopen, so ldd cannot see them.
depends="libc6 (>= 2.39), libgcc-s1, libasound2, libpipewire-0.3-0, pulseaudio-utils, \
libfontconfig1, libfreetype6, zlib1g, libbz2-1.0, libexpat1, libpng16-16, libbrotli1, \
libx11-6, libxcursor1, libxrandr2, libxi6, libxkbcommon0, libxkbcommon-x11-0, \
libwayland-client0, libwayland-cursor0, libwayland-egl1"
# The app does not decode during playback, so GStreamer is only recommended:
# the system player (Totem) needs libav for H.264/AAC and plugins-good for
# Matroska, while VLC brings its own decoders. apt installs Recommends by
# default, and the package is still installable without them.
recommends="gstreamer1.0-libav, gstreamer1.0-plugins-good"
installed_size="$(du -sk "$debroot/usr" | awk '{ print $1 }')"
cat >"$debroot/DEBIAN/control" <<EOF
Package: capture-button
Version: $version
Architecture: amd64
Maintainer: machine202502 <machine202502@users.noreply.github.com>
Installed-Size: $installed_size
Section: graphics
Priority: optional
Homepage: https://github.com/machine202502/capture-button
Depends: $depends
Recommends: $recommends
Description: Record the screen, a window, or a region on Linux
 Capture Button records the screen, a window, or a selected region to MP4,
 MKV, GIF, WebP or MP3 and takes screenshots. It runs on X11 and Wayland and
 needs PipeWire for Wayland screen capture.
EOF
(cd "$debroot" && find usr -type f -exec md5sum {} + >DEBIAN/md5sums)
rm -f "$deb"
dpkg-deb --build --root-owner-group "$debroot" "$deb" >/dev/null
if ! dpkg-deb --info "$deb" >/dev/null; then
    echo "error: dpkg-deb produced an invalid package: $deb" >&2
    exit 1
fi
rm -rf "$root/dist/deb"
rm -rf "$stage"

if [ "$publish" -eq 1 ]; then
    site_updates="$root/site/updates"
    mkdir -p "$site_updates"
    cp -a "$feed" "$site_updates/releases.linux.json"
    # Linux packages carry a -linux- segment, so they never collide with the
    # Windows `CaptureButtonApp-<version>-full.nupkg` already in this folder.
    find "$out" -maxdepth 1 -type f -name '*.nupkg' -exec cp -a {} "$site_updates/" \;
    echo "    feed    $site_updates/releases.linux.json"
fi

echo
echo "AppImage: $dist_image ($(du -h "$dist_image" | cut -f1))"
echo "Deb:      $deb ($(du -h "$deb" | cut -f1))"
echo "Feed:     $root/dist/releases.linux.json"
for package in "$out"/*.nupkg; do
    echo "Package:  $package ($(du -h "$package" | cut -f1))"
done
if command -v sha256sum >/dev/null 2>&1; then
    echo "SHA256:   $(sha256sum "$dist_image" | cut -d' ' -f1)  $(basename "$dist_image")"
    echo "SHA256:   $(sha256sum "$deb" | cut -d' ' -f1)  $(basename "$deb")"
fi
