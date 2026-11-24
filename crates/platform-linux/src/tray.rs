//! The Linux tray icon: a KDE/freedesktop StatusNotifierItem.
//!
//! The menu, the tooltip and the recording badge are the same ones the Windows
//! backend publishes: [`TrayEntry`] becomes a `com.canonical.dbusmenu` menu,
//! [`set_status`] replaces the tooltip text, and [`TrayBadge::Recording`] adds
//! the red dot to the bottom-right corner of the icon. `ksni` runs the D-Bus
//! service on its own thread, so the application loop never blocks on it.
//!
//! The icon pixmaps are ARGB32 in network byte order (`A`, `R`, `G`, `B`), the
//! format the StatusNotifierItem specification asks for, and they are not
//! premultiplied: the SVG/PNG images every other indicator sends use straight
//! alpha, and the hosts expect that.
//!
//! The tray is optional. A session without a D-Bus or without a
//! `org.kde.StatusNotifierWatcher` (stock GNOME without the AppIndicator
//! extension) fails to register the item; the failure is logged and the
//! application keeps running from its window.

use ksni::blocking::{Handle, TrayMethods};
use ksni::menu::{MenuItem, StandardItem};
use ksni::{Icon, ToolTip, Tray as KsniTray};
use platform_core::logging;
use platform_core::palette;
use platform_core::tray::{TrayBadge, TrayEntry};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

/// The project icon, embedded so the tray does not depend on the working
/// directory or on where the application is installed.
const ICON: &[u8] = include_bytes!("../../../assets/CaptureButton-64.png");

/// The menu row the application last published.
enum Row {
    Item {
        id: i32,
        text: String,
        enabled: bool,
    },
    Separator,
}

/// Everything the tray shows, updated through the `ksni` handle.
///
/// The handle owns this model, so an update can change it and the service
/// diffs the menu and emits the new properties in one step.
struct Model {
    status: String,
    badge: TrayBadge,
    rows: Vec<Row>,
    /// The decoded project icon without a badge, used as the base of every
    /// status icon.
    base: image::RgbaImage,
    /// The ARGB32 pixmap for the current badge; rebuilding it is only done when
    /// the badge changes, not on every menu update.
    icon: Icon,
}

impl Model {
    fn new(rows: Vec<Row>) -> Self {
        let base = match image::load_from_memory(ICON) {
            Ok(image) => image.to_rgba8(),
            Err(error) => {
                logging::warn(format!("tray: could not decode the icon: {error}"));
                image::RgbaImage::new(1, 1)
            }
        };
        let icon = render_icon(&base, TrayBadge::Idle);
        Self {
            status: "Capture Button".to_string(),
            badge: TrayBadge::Idle,
            rows,
            base,
            icon,
        }
    }
}

/// The `ksni::Tray` implementation. Its methods run on the D-Bus thread.
struct TrayItem {
    model: Arc<Mutex<Model>>,
    command: Arc<dyn Fn(i32) + Send + Sync>,
    activate: Arc<dyn Fn() + Send + Sync>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

impl TrayItem {
    fn set_text(&mut self, id: i32, text: &str) {
        let mut model = lock(&self.model);
        for row in &mut model.rows {
            if let Row::Item {
                id: row_id,
                text: row_text,
                ..
            } = row
            {
                if *row_id == id {
                    *row_text = text.to_string();
                    return;
                }
            }
        }
    }

    fn set_item_enabled(&mut self, id: i32, enabled: bool) {
        let mut model = lock(&self.model);
        for row in &mut model.rows {
            if let Row::Item {
                id: row_id,
                enabled: row_enabled,
                ..
            } = row
            {
                if *row_id == id {
                    *row_enabled = enabled;
                    return;
                }
            }
        }
    }

    fn set_status(&mut self, status: &str, badge: TrayBadge) {
        let mut model = lock(&self.model);
        model.status = status.to_string();
        if model.badge != badge {
            model.badge = badge;
            model.icon = render_icon(&model.base, badge);
        }
    }
}

impl KsniTray for TrayItem {
    fn id(&self) -> String {
        "CaptureButton".to_string()
    }

    fn title(&self) -> String {
        lock(&self.model).status.clone()
    }

    fn tool_tip(&self) -> ToolTip {
        let model = lock(&self.model);
        ToolTip {
            icon_pixmap: vec![model.icon.clone()],
            title: model.status.clone(),
            description: String::new(),
            ..Default::default()
        }
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        vec![lock(&self.model).icon.clone()]
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let model = lock(&self.model);
        model
            .rows
            .iter()
            .map(|row| match row {
                Row::Separator => MenuItem::Separator,
                Row::Item { id, text, enabled } => {
                    let command = self.command.clone();
                    let id = *id;
                    MenuItem::Standard(StandardItem {
                        label: text.clone(),
                        enabled: *enabled,
                        activate: Box::new(move |_| command(id)),
                        ..Default::default()
                    })
                }
            })
            .collect()
    }

    /// A click on the icon itself (the StatusNotifierItem `Activate` method).
    ///
    /// Which gesture reaches this depends on the host: KDE and most SNI hosts
    /// send it for a left click, GNOME's AppIndicator extension only for a
    /// double click (a single click there opens the menu). The application runs
    /// the same tray-click policy in both cases.
    fn activate(&mut self, x: i32, y: i32) {
        logging::info(format!("tray: activate at {x},{y}"));
        (self.activate)();
    }
}

/// The running item, if the desktop accepted it.
static TRAY: OnceLock<Mutex<Option<Handle<TrayItem>>>> = OnceLock::new();

fn tray() -> &'static Mutex<Option<Handle<TrayItem>>> {
    TRAY.get_or_init(|| Mutex::new(None))
}

fn current() -> Option<Handle<TrayItem>> {
    lock(tray()).clone()
}

/// Publishes (or replaces) the menu and spawns the StatusNotifierItem.
///
/// The `activate` closure is kept in the signature for the shell backend, which
/// still offers it; the icon click now goes through [`TRAY_LEFT_CLICK`] so the
/// application can run the shared tray-click policy for it.
pub fn build(
    entries: &[TrayEntry],
    command: Box<dyn Fn(i32) + Send + Sync>,
    activate: Box<dyn Fn() + Send + Sync>,
) {
    let rows = entries
        .iter()
        .map(|entry| match entry {
            TrayEntry::Separator => Row::Separator,
            TrayEntry::Item { id, text, .. } => Row::Item {
                id: *id,
                text: text.clone(),
                enabled: true,
            },
        })
        .collect();
    let model = Arc::new(Mutex::new(Model::new(rows)));

    if let Some(handle) = current() {
        // Only one StatusNotifierItem per process: refresh the one the desktop
        // already knows instead of registering a second bus name.
        handle.update(|item| item.model = model);
        return;
    }

    let item = TrayItem {
        model,
        command: Arc::from(command),
        activate: Arc::from(activate),
    };
    match item.spawn() {
        Ok(handle) => {
            *lock(tray()) = Some(handle);
            logging::info("tray: StatusNotifierItem registered");
        }
        Err(error) => {
            // No session bus, or nothing runs `org.kde.StatusNotifierWatcher`.
            // The window is the only surface then, and that still works.
            logging::warn(format!("tray: StatusNotifierItem unavailable: {error}"));
        }
    }
}

pub fn set_text(id: i32, text: &str) {
    if let Some(handle) = current() {
        handle.update(|item| item.set_text(id, text));
    }
}

pub fn set_item_enabled(id: i32, enabled: bool) {
    if let Some(handle) = current() {
        handle.update(|item| item.set_item_enabled(id, enabled));
    }
}

pub fn set_status(text: &str, badge: TrayBadge) {
    if let Some(handle) = current() {
        handle.update(|item| item.set_status(text, badge));
    }
}

/// Removes the item from the panel and stops the D-Bus service.
pub fn remove() {
    let handle = lock(tray()).take();
    if let Some(handle) = handle {
        handle.shutdown().wait();
        logging::info("tray: StatusNotifierItem removed");
    }
}

// ---- icon ----------------------------------------------------------------

/// Renders the project icon for `badge` as a ksni ARGB32 pixmap.
fn render_icon(base: &image::RgbaImage, badge: TrayBadge) -> Icon {
    let mut pixels = base.clone();
    if badge == TrayBadge::Recording {
        // The Windows tray draws the badge for a 32 px icon and scales it with
        // the icon: a dark edge circle at (23, 23) with radius 8, and the red
        // fill with radius 6. The project icon is 256 px, so unit = 8.
        let unit = pixels.width() as f32 / 32.0;
        let (cx, cy) = (23.0 * unit, 23.0 * unit);
        stamp_circle(
            &mut pixels,
            cx,
            cy,
            8.0 * unit,
            rgb(palette::DEFAULT.BADGE_RECORDING_EDGE),
        );
        stamp_circle(
            &mut pixels,
            cx,
            cy,
            6.0 * unit,
            rgb(palette::DEFAULT.BADGE_RECORDING),
        );
    }
    argb(&pixels)
}

/// The palette stores `0x00BBGGRR`, so red is the low byte.
fn rgb(color: u32) -> [u8; 3] {
    [color as u8, (color >> 8) as u8, (color >> 16) as u8]
}

/// Converts straight RGBA to ARGB32 network byte order (alpha first).
fn argb(pixels: &image::RgbaImage) -> Icon {
    let mut data = Vec::with_capacity((pixels.width() * pixels.height() * 4) as usize);
    for pixel in pixels.pixels() {
        data.extend_from_slice(&[pixel[3], pixel[0], pixel[1], pixel[2]]);
    }
    Icon {
        width: pixels.width() as i32,
        height: pixels.height() as i32,
        data,
    }
}

/// Fills a circle with one-pixel analytic anti-aliasing.
fn stamp_circle(pixels: &mut image::RgbaImage, cx: f32, cy: f32, radius: f32, color: [u8; 3]) {
    let left = (cx - radius - 1.0).floor().max(0.0) as u32;
    let top = (cy - radius - 1.0).floor().max(0.0) as u32;
    let right = ((cx + radius + 1.0).ceil() as u32).min(pixels.width());
    let bottom = ((cy + radius + 1.0).ceil() as u32).min(pixels.height());
    for y in top..bottom {
        for x in left..right {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            let distance = (dx * dx + dy * dy).sqrt();
            let coverage = (radius - distance + 0.5).clamp(0.0, 1.0);
            if coverage > 0.0 {
                blend(pixels.get_pixel_mut(x, y), color, coverage);
            }
        }
    }
}

/// Source-over compositing of a straight-alpha color onto a straight-alpha
/// pixel. The transparent corners of the project icon keep their color when
/// the badge crosses them.
fn blend(pixel: &mut image::Rgba<u8>, color: [u8; 3], coverage: f32) {
    let source = coverage.clamp(0.0, 1.0);
    let destination = pixel[3] as f32 / 255.0;
    let alpha = source + destination * (1.0 - source);
    if alpha <= 0.0 {
        pixel.0 = [0, 0, 0, 0];
        return;
    }
    for (channel, value) in color.iter().enumerate() {
        let blended = *value as f32 * source + pixel[channel] as f32 * destination * (1.0 - source);
        pixel[channel] = (blended / alpha).round().clamp(0.0, 255.0) as u8;
    }
    pixel[3] = (alpha * 255.0).round() as u8;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(icon: &Icon, x: u32, y: u32) -> [u8; 4] {
        let offset = ((y * icon.width as u32 + x) * 4) as usize;
        [
            icon.data[offset],
            icon.data[offset + 1],
            icon.data[offset + 2],
            icon.data[offset + 3],
        ]
    }

    #[test]
    fn pixmaps_are_argb32_network_byte_order() {
        let mut pixels = image::RgbaImage::new(1, 1);
        pixels.put_pixel(0, 0, image::Rgba([10, 20, 30, 40]));
        let icon = argb(&pixels);
        assert_eq!(icon.data, vec![40, 10, 20, 30]);
    }

    #[test]
    fn the_recording_badge_is_an_opaque_red_dot_in_the_corner() {
        let base = image::RgbaImage::new(64, 64);
        let idle = render_icon(&base, TrayBadge::Idle);
        let recording = render_icon(&base, TrayBadge::Recording);
        assert_ne!(idle.data, recording.data);
        // 32-unit geometry: the dot sits at (23, 23) with radius 6, so a 64 px
        // icon puts its opaque center at (46, 46).
        let [a, r, g, b] = pixel(&recording, 46, 46);
        assert_eq!(a, 255, "the badge must be opaque");
        assert!(
            r > 150 && g < 120 && b < 120,
            "the recording badge must stay red: {r},{g},{b}"
        );
        // The idle icon has no ink in that transparent corner.
        assert_eq!(pixel(&idle, 46, 46), [0, 0, 0, 0]);
        // The top-left corner stays untouched.
        assert_eq!(pixel(&recording, 4, 4), [0, 0, 0, 0]);
    }
}
