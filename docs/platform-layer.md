# Platform layer

`app` depends on one platform crate: `platform`. Under it:

```
platform-core   OS-neutral: value types, geometry, i18n catalogs, palette,
                time/logging, and policy (region model, source rules,
                tray model).
platform        glue: the only crate `app` uses. Wires `platform-core` to the
                Windows backend and holds the few modules that need both.
platform-windows  Win32: window/tray/timers, screen capture, GDI painters,
                  registry, dialogs, clipboard, sounds, devices.
```

Rule of thumb:

- decides what the user sees or does → `platform-core`;
- talks to the operating system → `platform-windows`;
- needs both → `platform`.

`platform-core` never names a Win32 type or calls the operating system.

## Region: policy and renderers

`region` is split so that the selection rules exist once:

- `platform-core::region` — the state machine: roles/tools, selection geometry,
  annotations, layout, hit-testing, labels. It never draws and never reads OS
  state; it consumes pointer/key events and produces a draw scene.
- `platform-windows::region` — the native GDI host: a topmost window, the memory
  DC painter, the text editor with IME, the save dialog, screen grabs.
- `crates/app/src/overlay.rs` + `crates/app/ui/overlay.slint` — the Slint overlay
  that consumes the same policy and renders it with Slint.

Both renderers are available and the hidden setting `region_renderer` in
`HKCU\Software\CaptureButton` selects one: `0` (default) = GDI, `1` = Slint.
The value is read at startup; it does not appear in the Settings window.

## Status

The split is in place. `platform-core` holds every OS-neutral type and rule:
value types, frame helpers, geometry, translated catalogs, the palette, time,
logging, output folders and names, capture-source rules, the tray menu model and
the whole area-selection model. `platform` composes it with the Windows backend
and owns camera, audio, updates, settings storage and the `screen`/`region` glue.
`platform-windows` is Win32 only.

The GDI overlay is the default and keeps the behaviour it had; the Slint overlay
reuses `platform_core::region::Model` and falls back to GDI if it cannot start.

Known gaps in the Slint overlay, all cosmetic or input-edge: the text box cannot
be moved or resized by its frame handles, button widths with a caption are
estimated (Slint does not expose text metrics to the model), there is no hint
shadow or text-editor corner markers, the window is not excluded from screen
capture, and a HiDPI window may show one frame at the wrong scale for ~20 ms
while it settles. The saved picture is identical to the GDI one because both
bake annotations through the same code.
