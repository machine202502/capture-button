# Platform layer

`app` depends on one platform crate: `platform`. Under it:

```
platform-core   OS-neutral: value types, geometry, i18n catalogs, palette,
                time/logging, and policy shared by every OS (region model,
                source rules, tray model).
platform        glue: the only crate `app` uses. Wires `platform-core` to the
                selected backend and holds the few modules that need both.
platform-windows  Win32: window/tray/timers, screen capture, GDI painters,
                  registry, dialogs, clipboard, sounds, devices.
platform-linux    (not written yet) implements the same modules as
                  platform-windows and is selected in platform/src/lib.rs.
```

Rule of thumb:

- decides what the user sees or does → `platform-core`;
- talks to the operating system → `platform-<os>`;
- needs both → `platform`.

## Region: policy and renderers

`region` is split so that the selection rules exist once:

- `platform-core::region` — the state machine: roles/tools, selection geometry,
  annotations, layout, hit-testing, labels. It never draws and never reads OS
  state; it consumes pointer/key events and produces a draw scene.
- `platform-windows::region` — the native GDI host: a topmost window, the memory
  DC painter, the text editor with IME, the save dialog, screen grabs.
- `crates/app/src/overlay.rs` + `crates/app/ui/overlay.slint` — the Slint overlay that consumes the same policy and
  renders it with Slint, so every non-Windows OS gets the same overlay and the
  same look.

On Windows both renderers are available and the hidden setting `region_renderer`
in `HKCU\Software\CaptureButton` selects one: `0` (default) = GDI, `1` = Slint.
The value is read at startup; it does not appear in the Settings window.

## Status

The split is in place. `platform-core` holds every OS-neutral type and rule:
value types, frame helpers, geometry, translated catalogs, the palette, time,
logging, output folders and names, capture-source rules, the tray menu model and
the whole area-selection model. `platform` composes it with the backend and owns
camera, audio, updates, settings storage and the `screen`/`region` glue.
`platform-windows` is Win32 only. `platform-linux` remains to be written against
the same module set.

Windows has both overlay renderers behind the hidden `region_renderer` setting
(see AGENTS.md). The GDI overlay is the default and keeps the behaviour it had;
the Slint overlay reuses `platform_core::region::Model` and falls back to GDI if
it cannot start.

Known gaps in the Slint overlay, all cosmetic or input-edge: the text box cannot
be moved or resized by its frame handles, button widths with a caption are
estimated (Slint does not expose text metrics to the model), there is no hint
shadow or text-editor corner markers, the window is not excluded from screen
capture, and a HiDPI window may show one frame at the wrong scale for ~20 ms
while it settles. The saved picture is identical to the GDI one because both
bake annotations through the same code.

## Adding an OS

1. Add `platform-<os>` with the modules `platform` re-exports.
2. Implement the OS calls its modules need (windows, screens, tray, dialogs,
   registry-equivalent store, clipboard, sounds, devices).
3. Point `platform/src/lib.rs` at it; `platform-core` and `app` stay unchanged.

Ports that have no direct equivalent (Wayland window lists, tray on GNOME,
global hotkeys) are expected to report "unsupported" rather than fake a result;
the application must degrade gracefully.

## Linux: what Wayland cannot do

The Linux backend is built and runs on both X11 and Wayland. It draws the area
overlay with the Slint renderer, captures through
`org.freedesktop.portal.ScreenCast` on Wayland, and publishes the tray as a
StatusNotifierItem. Three limits are properties of Wayland, not bugs in the
application; they were measured on the e2e stand (Ubuntu 24.04, GNOME 46,
mutter) and are recorded here so nobody re-opens them as application defects.

### The shell's panel and dock stay above the area overlay

The Slint overlay is an ordinary `xdg_toplevel`. It asks for fullscreen before
the surface is mapped (`xdg_toplevel.set_fullscreen`, confirmed with
`WAYLAND_DEBUG=1`) and mutter answers with a 1920×1080 configure, but GNOME
hides its panel and dock only for the *activated* fullscreen window. A Wayland
client cannot activate a window by itself: it needs an `xdg_activation_v1` token
tied to a recent input serial, and neither Slint nor winit exposes that request.
Opening the overlay from the tray therefore leaves the panel and the dock on
top; a native GNOME app in fullscreen (`gnome-terminal --full-screen`) hides
both on the same session, which is what places the gap in our window type rather
than in the session.

Removing the panel for good needs the overlay in a layer above the shell, i.e.
`wlr-layer-shell`/`gtk-layer-shell`; Slint has no layer-shell window. X11 is not
affected: there the overlay sets `_NET_WM_STATE_FULLSCREEN` and
`_NET_WM_STATE_ABOVE` and covers both. This is the one visual difference between
the two sessions.

### The frozen frame comes from the portal

The overlay draws a still of the screen. On X11 that is an XGetImage call; on
Wayland it is another ScreenCast request, so the first area selection after a
fresh start shows the portal's share dialog (later ones reuse the stored
`restore_token` and freeze immediately). If that request is cancelled, the
overlay cannot open and the application reports
`the native overlay is not available on Linux; use the Slint overlay` — an
honest failure rather than an overlay over a stale frame.

### The tray item only opens the window

The tray's window item is always "Open window": its click restores, raises and
focuses the main window and never hides it. The item used to toggle into
"Close window" when the platform could report that the window was in front, but
that state was only observable on some desktops (Wayland reports neither the
focus nor the minimized state) and the item proved unreliable, so the toggle is
gone everywhere.

Closing the window stays where it belongs: the window's own close button (which
hides to the tray when “Close button hides to the tray” is on, otherwise quits),
and Exit in the tray menu. The platform layer still offers
`platform::shell::tray_click_hint()` so the settings page can explain what a
click on the icon does on that desktop.

### The recording outline goes through XWayland

The blinking frame around the recorded area is a shaped, click-through X11
window (`platform-linux/src/outline.rs`): an override-redirect window whose
bounding shape is only the perimeter and whose input shape is empty, so clicks
reach the desktop through the frame. On Wayland the same window is drawn through
XWayland, which composes it above the Wayland windows and the GNOME panel. The
application needs a reachable X11 display (`DISPLAY` and `XAUTHORITY`, which a
GNOME session gives its applications) and the Shape extension; without XWayland
the frame is skipped with one warning and Settings hides the row. The "above the
panel" behaviour was measured on mutter 46.2 and is compositor behaviour, not a
protocol guarantee. On plain X11 the frame is skipped as well when the server
does not offer Shape, rather than drawing a window that would swallow clicks.

### The source picker belongs to the desktop

`screencast`'s monitor/window list is drawn by `xdg-desktop-portal-gnome`, not
by the application: the app sends `SelectSources` with the `types` the take
allows (`Monitor` for a screenshot, `Monitor | Window` for a recording) and the
portal lists what its backend offers. On the e2e VM the single QEMU connector
`Virtual-1` has no name, so the portal labels it "Unknown Display"; the
"Application Window" tab lists the windows. An empty list therefore means the
portal could not enumerate sources, and the application never fabricates one.
