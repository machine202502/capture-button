//! Screen capture and the list of capture sources.
//!
//! On an X11 session everything here reads the root window through X11. On a
//! Wayland session an ordinary client cannot read the screen at all, so the
//! capture calls ask `org.freedesktop.portal.ScreenCast` for a PipeWire node
//! (see [`crate::portal`]) and consume it (see [`crate::pipewire`]). The
//! public API stays synchronous and keeps the X11 shape: the portal's own
//! picker chooses the monitor or window, so the handles the app passes in are
//! only placeholders on that path.

use crate::pipewire::{self, PipeWireCapture};
use crate::portal::{self, PortalStream, SourceKind};
use platform_core::screen::MonitorInfo;
use platform_core::types::{Frame, Rect};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use x11rb::connection::{Connection, RequestConnection as _};
use x11rb::protocol::composite::{self, ConnectionExt as _, Redirect};
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ConnectionExt as _, ImageFormat, MapState, Window as XWindow,
};
use x11rb::rust_connection::RustConnection;

/// An opaque monitor handle the app can store and hand back.
#[derive(Clone, Copy, Debug)]
pub struct Monitor(pub Rect);

/// An opaque window handle the app can store and hand back.
#[derive(Clone, Copy, Debug)]
pub struct Window(pub XWindow);

struct Slot {
    frame: Mutex<Option<(u64, Frame)>>,
    arrived: Condvar,
}

pub struct ScreenCapture {
    stop: Arc<AtomicBool>,
    slot: Arc<Slot>,
    seen: u64,
    thread: Option<std::thread::JoinHandle<()>>,
    /// Set on the portal path: stops the PipeWire stream and waits for it.
    pipewire: Option<PipeWireCapture>,
    /// Keeps the portal session (and therefore the PipeWire node) alive.
    portal: Option<portal::Lease>,
}

impl ScreenCapture {
    pub fn capture_alive(&self) -> bool {
        !self.stop.load(Ordering::Relaxed)
    }

    pub fn available(&self) -> bool {
        self.slot
            .frame
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_some()
    }

    pub fn latest(&mut self) -> Option<Frame> {
        let mut guard = self
            .slot
            .frame
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some((generation, frame)) = guard.take() {
            self.seen = generation;
            Some(frame)
        } else {
            None
        }
    }

    pub fn frame(&mut self) -> Result<Frame, String> {
        let mut guard = self
            .slot
            .frame
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let deadline = Instant::now() + Duration::from_millis(500);
        while guard.is_none()
            || guard
                .as_ref()
                .is_some_and(|(generation, _)| *generation <= self.seen)
        {
            if self.stop.load(Ordering::Relaxed) {
                return Err("the capture stopped".into());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err("no frame arrived".into());
            }
            let (next, timeout) = self
                .slot
                .arrived
                .wait_timeout(guard, left)
                .unwrap_or_else(|error| error.into_inner());
            guard = next;
            if timeout.timed_out() && guard.is_none() {
                return Err("no frame arrived".into());
            }
        }
        let (generation, frame) = guard.take().expect("checked above");
        self.seen = generation;
        Ok(frame)
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.slot.arrived.notify_all();
        if let Some(mut pipewire) = self.pipewire.take() {
            pipewire.stop();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        // Closing the portal session also tears down the PipeWire node, so do
        // it after the stream has disconnected.
        self.portal.take();
    }
}

impl Drop for ScreenCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Runs `grab` at `fps` until the stop flag is set, publishing each frame.
fn spawn_capture<G>(
    slot: Arc<Slot>,
    stop: Arc<AtomicBool>,
    fps: u32,
    mut grab: G,
) -> Result<std::thread::JoinHandle<()>, String>
where
    G: FnMut() -> Result<Frame, String> + Send + 'static,
{
    let interval = Duration::from_millis(1000 / fps.max(1) as u64);
    std::thread::Builder::new()
        .name("capture-button-x11".into())
        .spawn(move || {
            let mut generation = 0u64;
            let mut next = Instant::now();
            while !stop.load(Ordering::Relaxed) {
                next += interval;
                let wait = next.saturating_duration_since(Instant::now());
                if !wait.is_zero() {
                    std::thread::sleep(wait);
                }
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                if let Ok(frame) = grab() {
                    generation += 1;
                    let mut guard = slot.frame.lock().unwrap_or_else(|error| error.into_inner());
                    *guard = Some((generation, frame));
                    slot.arrived.notify_all();
                }
            }
        })
        .map_err(|error| error.to_string())
}

fn screen_rect() -> Rect {
    match x11rb::connect(None) {
        Ok((conn, screen_num)) => {
            let setup = conn.setup();
            let screen = &setup.roots[screen_num];
            Rect {
                x: 0,
                y: 0,
                w: screen.width_in_pixels as u32,
                h: screen.height_in_pixels as u32,
            }
        }
        Err(_) => Rect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        },
    }
}

/// The whole (virtual) screen, used when RandR cannot answer.
pub fn primary_monitor() -> Rect {
    screen_rect()
}

/// The monitors of the X screen with the names and the primary flag RandR
/// reports. A Wayland client cannot enumerate the compositor's outputs; the
/// portal's own picker chooses the source there, so the list is empty.
pub fn monitors() -> Vec<MonitorInfo> {
    if wayland_session() {
        return Vec::new();
    }
    let Ok((conn, screen_num)) = x11rb::connect(None) else {
        return vec![fallback_monitor()];
    };
    let root = conn.setup().roots[screen_num].root;
    if let Ok(cookie) = conn.randr_get_monitors(root, true) {
        if let Ok(reply) = cookie.reply() {
            if !reply.monitors.is_empty() {
                return reply
                    .monitors
                    .iter()
                    .enumerate()
                    .map(|(index, monitor)| {
                        let name = conn
                            .get_atom_name(monitor.name)
                            .ok()
                            .and_then(|cookie| cookie.reply().ok())
                            .map(|reply| String::from_utf8_lossy(&reply.name).into_owned())
                            .filter(|name| !name.is_empty())
                            .unwrap_or_else(|| format!("Monitor {}", index + 1));
                        MonitorInfo {
                            id: name.clone(),
                            name,
                            width: u32::from(monitor.width),
                            height: u32::from(monitor.height),
                            primary: monitor.primary,
                            x: i32::from(monitor.x),
                            y: i32::from(monitor.y),
                        }
                    })
                    .collect();
            }
        }
    }
    vec![fallback_monitor()]
}

/// Whether the Settings window can offer a monitor choice.
pub fn monitors_supported() -> bool {
    !wayland_session() && x11rb::connect(None).is_ok()
}

fn wayland_session() -> bool {
    let wayland_display =
        std::env::var_os("WAYLAND_DISPLAY").is_some_and(|value| !value.is_empty());
    let wayland_session = std::env::var("XDG_SESSION_TYPE")
        .map(|value| value.eq_ignore_ascii_case("wayland"))
        .unwrap_or(false);
    wayland_display || wayland_session
}

/// The one-monitor answer when RandR cannot describe the X screen.
fn fallback_monitor() -> MonitorInfo {
    let rect = screen_rect();
    MonitorInfo {
        id: "screen".into(),
        name: "Screen".into(),
        width: rect.w,
        height: rect.h,
        primary: true,
        x: rect.x,
        y: rect.y,
    }
}

/// The monitors of the X screen, used by the region model.
pub fn monitor_rects() -> Vec<Rect> {
    monitors().iter().map(MonitorInfo::rect).collect()
}

/// The monitor that contains the pointer, if X11 can be reached.
pub fn monitor_under_pointer() -> Option<Rect> {
    let (conn, screen_num) = x11rb::connect(None).ok()?;
    let root = conn.setup().roots[screen_num].root;
    let pointer = conn.query_pointer(root).ok()?.reply().ok()?;
    let point = (pointer.root_x as i32, pointer.root_y as i32);
    monitor_rects().into_iter().find(|rect| {
        point.0 >= rect.x
            && point.0 < rect.x + rect.w as i32
            && point.1 >= rect.y
            && point.1 < rect.y + rect.h as i32
    })
}

/// Grabs a rectangle of the root window.
pub fn grab_region(rect: Rect) -> Result<Frame, String> {
    if rect.w < 2 || rect.h < 2 {
        return Err("the capture area is empty".into());
    }
    let (conn, screen_num) =
        x11rb::connect(None).map_err(|error| format!("cannot open the X11 display: {error}"))?;
    let screen = &conn.setup().roots[screen_num];
    let root = screen.root;
    let masks = visual_masks(&conn, screen_num)?;
    let reply = conn
        .get_image(
            ImageFormat::Z_PIXMAP,
            root,
            rect.x as i16,
            rect.y as i16,
            rect.w as u16,
            rect.h as u16,
            !0,
        )
        .map_err(|error| error.to_string())?
        .reply()
        .map_err(|error| format!("X11 GetImage failed: {error}"))?;

    frame_from_image(&conn, &reply, rect.w, rect.h, masks)
}

/// Turns a raw `ZPixmap` reply into the BGRA frame the rest of the app expects.
fn frame_from_image(
    conn: &impl Connection,
    reply: &x11rb::protocol::xproto::GetImageReply,
    width: u32,
    height: u32,
    masks: (u32, u32, u32),
) -> Result<Frame, String> {
    if width < 2 || height < 2 {
        return Err("the capture area is empty".into());
    }
    let (red_mask, green_mask, blue_mask) = masks;
    let bytes_per_pixel = conn
        .setup()
        .pixmap_formats
        .iter()
        .find(|format| format.depth == reply.depth)
        .map(|format| format.bits_per_pixel as usize / 8)
        .unwrap_or(4);
    let line = reply.data.len() / height as usize;
    if bytes_per_pixel < 3 || line < bytes_per_pixel * width as usize {
        return Err(format!(
            "unsupported X11 pixel layout: {bytes_per_pixel} bytes per pixel, {line} bytes per line"
        ));
    }
    let little = conn.setup().image_byte_order == x11rb::protocol::xproto::ImageOrder::LSB_FIRST;
    let mut data = vec![0u8; width as usize * height as usize * 4];
    for y in 0..height as usize {
        for x in 0..width as usize {
            let offset = y * line + x * bytes_per_pixel;
            let pixel = if bytes_per_pixel >= 4 {
                let raw = [
                    reply.data[offset],
                    reply.data[offset + 1],
                    reply.data[offset + 2],
                    reply.data[offset + 3],
                ];
                if little {
                    u32::from_le_bytes(raw)
                } else {
                    u32::from_be_bytes(raw)
                }
            } else {
                let raw = [
                    reply.data[offset],
                    reply.data[offset + 1],
                    reply.data[offset + 2],
                    0,
                ];
                if little {
                    u32::from_le_bytes(raw)
                } else {
                    u32::from_be_bytes(raw)
                }
            };
            let component = |mask: u32| -> u8 {
                if mask == 0 {
                    return 0;
                }
                let shift = mask.trailing_zeros();
                let max = mask >> shift;
                (((pixel & mask) >> shift) * 255 / max.max(1)) as u8
            };
            let target = (y * width as usize + x) * 4;
            data[target] = component(blue_mask);
            data[target + 1] = component(green_mask);
            data[target + 2] = component(red_mask);
            data[target + 3] = 255;
        }
    }
    Ok(Frame {
        width,
        height,
        data,
        age_ns: 0,
    })
}

fn visual_masks_for(
    conn: &impl Connection,
    screen_num: usize,
    visual_id: u32,
) -> Option<(u32, u32, u32)> {
    let screen = &conn.setup().roots[screen_num];
    for depth in &screen.allowed_depths {
        for visual in &depth.visuals {
            if visual.visual_id == visual_id {
                return Some((visual.red_mask, visual.green_mask, visual.blue_mask));
            }
        }
    }
    None
}

fn visual_masks(conn: &impl Connection, screen_num: usize) -> Result<(u32, u32, u32), String> {
    let visual_id = conn.setup().roots[screen_num].root_visual;
    visual_masks_for(conn, screen_num, visual_id)
        .ok_or_else(|| "the root visual was not found".to_string())
}

pub fn grab_display(rect: Rect) -> Result<Frame, String> {
    if portal::session_is_wayland() {
        // Screenshots never show the pointer, matching the X11 backend and
        // the app's "Include pointer applies to recordings" rule. They also
        // ask for a monitor: the area overlay's frozen frame and the model's
        // coordinates are tied to one monitor.
        return grab_portal(SourceKind::Monitor, false);
    }
    grab_region(rect)
}

pub fn grab_still(rect: Rect) -> Result<Frame, String> {
    if portal::session_is_wayland() {
        return grab_portal(SourceKind::Monitor, false);
    }
    grab_region(rect)
}

/// The monitor that contains the middle of `rect`, with its origin and size.
pub fn monitor_of(rect: Rect) -> (Monitor, i32, i32, u32, u32) {
    let all = monitor_rects();
    let center = (rect.x + rect.w as i32 / 2, rect.y + rect.h as i32 / 2);
    let monitor = all
        .into_iter()
        .find(|area| {
            center.0 >= area.x
                && center.0 < area.x + area.w as i32
                && center.1 >= area.y
                && center.1 < area.y + area.h as i32
        })
        .unwrap_or_else(primary_monitor);
    (Monitor(monitor), monitor.x, monitor.y, monitor.w, monitor.h)
}

impl Monitor {
    pub fn rect(self) -> Rect {
        self.0
    }
}

/// One live capture through the portal: the first frame and the running
/// PipeWire stream. The portal session stays open until the returned handle is
/// stopped.
fn start_portal(
    kind: SourceKind,
    crop: Option<(u32, u32, u32, u32)>,
    fps: u32,
    cursor: bool,
) -> Result<(ScreenCapture, Frame), String> {
    let source = pick_portal(kind, cursor)?;
    let (fd, node_id, lease) = source.into_parts();
    let slot = Arc::new(Slot {
        frame: Mutex::new(None),
        arrived: Condvar::new(),
    });
    let stop = Arc::new(AtomicBool::new(false));
    let publish = Arc::clone(&slot);
    let mut generation = 0u64;
    let (pipewire, first) = pipewire::start(fd, node_id, fps, move |frame| {
        let frame = crop_frame(frame, crop);
        generation += 1;
        let mut guard = publish
            .frame
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *guard = Some((generation, frame));
        publish.arrived.notify_all();
    })
    .map_err(|error| format!("the PipeWire capture failed: {error}"))?;
    // `first` bypasses the callback, so crop it here too; the callback may
    // already have published a newer frame, which is left in place.
    let first = crop_frame(first, crop);
    let mut guard = slot.frame.lock().unwrap_or_else(|error| error.into_inner());
    if guard.is_none() {
        *guard = Some((0, first.clone()));
    }
    drop(guard);
    slot.arrived.notify_all();
    Ok((
        ScreenCapture {
            stop,
            slot,
            seen: 0,
            thread: None,
            pipewire: Some(pipewire),
            portal: Some(lease),
        },
        first,
    ))
}

/// Crops a captured frame to the app's capture area. The portal chooses the
/// source, so the offsets are relative to the frame; a source of a different
/// size is clipped instead of failing the capture.
fn crop_frame(frame: Frame, crop: Option<(u32, u32, u32, u32)>) -> Frame {
    let Some((x, y, w, h)) = crop else {
        return frame;
    };
    if frame.width < 2 || frame.height < 2 {
        return frame;
    }
    let left = x.min(frame.width - 2);
    let top = y.min(frame.height - 2);
    let width = (w.max(2) & !1).min(frame.width - left) & !1;
    let height = (h.max(2) & !1).min(frame.height - top) & !1;
    if width < 2 || height < 2 {
        return frame;
    }
    if width == frame.width && height == frame.height {
        return frame;
    }
    let mut data = vec![0u8; width as usize * height as usize * 4];
    for row in 0..height as usize {
        let source = (top as usize + row) * frame.width as usize * 4 + left as usize * 4;
        let target = row * width as usize * 4;
        let row_bytes = width as usize * 4;
        if source + row_bytes > frame.data.len() || target + row_bytes > data.len() {
            return frame;
        }
        data[target..target + row_bytes].copy_from_slice(&frame.data[source..source + row_bytes]);
    }
    Frame {
        width,
        height,
        data,
        age_ns: 0,
    }
}

fn pick_portal(kind: SourceKind, cursor: bool) -> Result<PortalStream, String> {
    portal::pick_source(kind, cursor).map_err(|error| error.message())
}

fn grab_portal(kind: SourceKind, cursor: bool) -> Result<Frame, String> {
    let source = pick_portal(kind, cursor)?;
    let (fd, node_id, lease) = source.into_parts();
    let (mut capture, frame) = pipewire::start(fd, node_id, 1, |_| {})
        .map_err(|error| format!("the PipeWire capture failed: {error}"))?;
    capture.stop();
    drop(lease);
    Ok(frame)
}

/// Starts a capture thread that grabs the monitor (optionally cropped) at `fps`.
pub fn start_monitor(
    monitor: Monitor,
    crop: Option<(u32, u32, u32, u32)>,
    fps: u32,
    cursor: bool,
) -> Result<(ScreenCapture, Frame), String> {
    if portal::session_is_wayland() {
        // The portal picker decides which monitor is shared; the handle the
        // app chose in its own list is not a monitor id the portal can use.
        // The area selection is still applied to the captured frames.
        return start_portal(SourceKind::MonitorOrWindow, crop, fps, cursor);
    }
    let area = monitor.0;
    let rect = match crop {
        Some((x, y, w, h)) => Rect {
            x: area.x + x as i32,
            y: area.y + y as i32,
            w: w.max(2) & !1,
            h: h.max(2) & !1,
        },
        None => area,
    };
    let first = grab_region(rect)?;
    let slot = Arc::new(Slot {
        frame: Mutex::new(Some((0, first.clone()))),
        arrived: Condvar::new(),
    });
    let stop = Arc::new(AtomicBool::new(false));
    let thread = spawn_capture(Arc::clone(&slot), Arc::clone(&stop), fps, move || {
        grab_region(rect)
    })?;
    Ok((
        ScreenCapture {
            stop,
            slot,
            seen: 0,
            thread: Some(thread),
            pipewire: None,
            portal: None,
        },
        first,
    ))
}

/// A live XComposite redirection of the root's windows plus the named pixmap of
/// the one window being captured. Its contents stay available while the window
/// is covered by others.
struct WindowSession {
    conn: RustConnection,
    root: XWindow,
    window: XWindow,
    pixmap: u32,
    masks: (u32, u32, u32),
    redirected: bool,
}

impl WindowSession {
    fn open(conn: RustConnection, screen_num: usize, window: XWindow) -> Result<Self, String> {
        let root = conn.setup().roots[screen_num].root;
        if window == x11rb::NONE || window == root {
            return Err("the window cannot be captured".into());
        }
        if conn
            .extension_information(composite::X11_EXTENSION_NAME)
            .map_err(|error| error.to_string())?
            .is_none()
        {
            return Err("the XComposite extension is not available".into());
        }
        conn.composite_query_version(0, 4)
            .map_err(|error| error.to_string())?
            .reply()
            .map_err(|error| format!("XComposite QueryVersion failed: {error}"))?;
        let geometry = conn
            .get_geometry(window)
            .map_err(|error| error.to_string())?
            .reply()
            .map_err(|error| format!("X11 GetGeometry failed: {error}"))?;
        if geometry.width < 2 || geometry.height < 2 {
            return Err("the window has no visible area".into());
        }
        let attributes = conn
            .get_window_attributes(window)
            .map_err(|error| error.to_string())?
            .reply()
            .map_err(|error| format!("X11 GetWindowAttributes failed: {error}"))?;
        let masks = visual_masks_for(&conn, screen_num, attributes.visual)
            .or_else(|| visual_masks(&conn, screen_num).ok())
            .ok_or_else(|| "the window visual was not found".to_string())?;
        // With Automatic update the server keeps mirroring the redirected
        // windows to the screen, so the desktop stays visible while we capture.
        let redirected = conn
            .composite_redirect_subwindows(root, Redirect::AUTOMATIC)
            .map_err(|error| error.to_string())?
            .check()
            .is_ok();
        let pixmap = conn.generate_id().map_err(|error| error.to_string())?;
        if let Err(error) = conn
            .composite_name_window_pixmap(window, pixmap)
            .map_err(|error| error.to_string())?
            .check()
        {
            if redirected {
                let _ = conn.composite_unredirect_subwindows(root, Redirect::AUTOMATIC);
            }
            return Err(format!("XComposite cannot capture the window: {error}"));
        }
        Ok(Self {
            conn,
            root,
            window,
            pixmap,
            masks,
            redirected,
        })
    }

    fn grab(&mut self) -> Result<Frame, String> {
        let geometry = self
            .conn
            .get_geometry(self.window)
            .map_err(|error| error.to_string())?
            .reply()
            .map_err(|error| format!("X11 GetGeometry failed: {error}"))?;
        if geometry.width < 2 || geometry.height < 2 {
            return Err("the window has no visible area".into());
        }
        let reply = self
            .conn
            .get_image(
                ImageFormat::Z_PIXMAP,
                self.pixmap,
                0,
                0,
                geometry.width,
                geometry.height,
                !0,
            )
            .map_err(|error| error.to_string())?
            .reply()
            .map_err(|error| format!("X11 GetImage failed: {error}"))?;
        frame_from_image(
            &self.conn,
            &reply,
            u32::from(geometry.width),
            u32::from(geometry.height),
            self.masks,
        )
    }
}

impl Drop for WindowSession {
    fn drop(&mut self) {
        let _ = self.conn.free_pixmap(self.pixmap);
        if self.redirected {
            let _ = self
                .conn
                .composite_unredirect_subwindows(self.root, Redirect::AUTOMATIC);
        }
        let _ = self.conn.flush();
    }
}

/// Starts a capture thread that follows one window at `fps`, even while another
/// window covers it.
pub fn start_window(
    window: Window,
    fps: u32,
    cursor: bool,
) -> Result<(ScreenCapture, Frame), String> {
    if portal::session_is_wayland() {
        // There is no X11 window id to follow on Wayland; the portal picker
        // asks the user for the window instead.
        let _ = window;
        return start_portal(SourceKind::Window, None, fps, cursor);
    }
    let (conn, screen_num) =
        x11rb::connect(None).map_err(|error| format!("cannot open the X11 display: {error}"))?;
    let mut session = WindowSession::open(conn, screen_num, window.0)?;
    let first = session.grab()?;
    let slot = Arc::new(Slot {
        frame: Mutex::new(Some((0, first.clone()))),
        arrived: Condvar::new(),
    });
    let stop = Arc::new(AtomicBool::new(false));
    let thread = spawn_capture(Arc::clone(&slot), Arc::clone(&stop), fps, move || {
        session.grab()
    })?;
    Ok((
        ScreenCapture {
            stop,
            slot,
            seen: 0,
            thread: Some(thread),
            pipewire: None,
            portal: None,
        },
        first,
    ))
}

static STAGED: Mutex<Option<XWindow>> = Mutex::new(None);

/// Takes the window chosen in the source picker, if any.
pub fn take_window() -> Option<Window> {
    STAGED
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take()
        .map(Window)
}

fn staged_window() -> Option<XWindow> {
    STAGED
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .as_ref()
        .copied()
}

/// Grabs one frame of the window chosen in the source picker.
pub fn grab_staged_window() -> Result<Frame, String> {
    let window = staged_window().ok_or_else(|| "The window is no longer available".to_string())?;
    let (conn, screen_num) =
        x11rb::connect(None).map_err(|error| format!("cannot open the X11 display: {error}"))?;
    let mut session = WindowSession::open(conn, screen_num, window)?;
    session.grab()
}

/// A window is usable while it still exists, is mapped and is not minimized.
pub fn window_usable(window: Window) -> bool {
    let Ok((conn, _)) = x11rb::connect(None) else {
        return false;
    };
    let Some(atoms) = Atoms::new(&conn) else {
        return false;
    };
    let mapped = conn
        .get_window_attributes(window.0)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .is_some_and(|attributes| attributes.map_state == MapState::VIEWABLE);
    mapped && !window_hidden(&conn, window.0, &atoms)
}

#[derive(Clone, Copy)]
pub enum Chosen {
    Monitor(Rect),
    Window(Rect),
}

#[derive(Clone)]
pub struct SourceEntry {
    pub label: String,
    pub monitor: bool,
    chosen: Chosen,
    window: Option<XWindow>,
}

static SOURCES: std::sync::OnceLock<Mutex<Vec<SourceEntry>>> = std::sync::OnceLock::new();

fn source_slot() -> &'static Mutex<Vec<SourceEntry>> {
    SOURCES.get_or_init(|| Mutex::new(Vec::new()))
}

/// The sources offered by the in-app picker: the monitors of the X screen, then
/// its visible windows.
///
/// On Wayland there is no window list and a click cannot point at one monitor:
/// the portal's own picker is the only selector. The list then holds a single
/// entry that opens it, so the existing list/choose UI keeps working unchanged.
pub fn list_sources() -> Vec<SourceEntry> {
    if portal::session_is_wayland() {
        let sources = vec![portal_source()];
        *source_slot()
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = sources.clone();
        return sources;
    }
    let mut sources: Vec<SourceEntry> = monitors()
        .into_iter()
        .enumerate()
        .map(|(index, info)| {
            let area = info.rect();
            SourceEntry {
                label: format!("Display {index} - {}x{}", area.w, area.h),
                monitor: true,
                chosen: Chosen::Monitor(area),
                window: None,
            }
        })
        .collect();
    sources.extend(window_sources());
    *source_slot()
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = sources.clone();
    sources
}

/// The one entry the Wayland source list shows. Choosing it opens the portal
/// picker, which decides between a monitor and a window; the rectangle is only
/// a stand-in for the parts of the app that need a capture area before the
/// real frame size is known.
fn portal_source() -> SourceEntry {
    SourceEntry {
        label: "Screen or window…".into(),
        monitor: true,
        chosen: Chosen::Monitor(primary_monitor()),
        window: None,
    }
}

pub fn choose_source_index(index: usize) -> Option<Chosen> {
    let (chosen, window) = {
        let guard = source_slot()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let entry = guard.get(index)?;
        (entry.chosen, entry.window)
    };
    let Some(window) = window else {
        return Some(chosen);
    };
    *STAGED.lock().unwrap_or_else(|error| error.into_inner()) = Some(window);
    // The picker may have been open while the window moved, so report where it
    // is now rather than where it was when the list was built.
    if let Ok((conn, screen_num)) = x11rb::connect(None) {
        if let Some(rect) = window_rect(&conn, screen_num, window) {
            return Some(Chosen::Window(rect));
        }
    }
    Some(chosen)
}

fn window_sources() -> Vec<SourceEntry> {
    let Ok((conn, screen_num)) = x11rb::connect(None) else {
        return Vec::new();
    };
    let root = conn.setup().roots[screen_num].root;
    let Some(atoms) = Atoms::new(&conn) else {
        return Vec::new();
    };
    let mut sources = Vec::new();
    for window in client_windows(&conn, root, &atoms) {
        if let Some(entry) = window_source(&conn, screen_num, window, &atoms) {
            sources.push(entry);
        }
    }
    sources
}

fn window_source(
    conn: &impl Connection,
    screen_num: usize,
    window: XWindow,
    atoms: &Atoms,
) -> Option<SourceEntry> {
    let attributes = conn.get_window_attributes(window).ok()?.reply().ok()?;
    if attributes.override_redirect || attributes.map_state != MapState::VIEWABLE {
        return None;
    }
    if window_hidden(conn, window, atoms) {
        return None;
    }
    let geometry = conn.get_geometry(window).ok()?.reply().ok()?;
    // Input-only windows have no depth, and an empty window cannot be captured.
    if geometry.depth == 0 || geometry.width == 0 || geometry.height == 0 {
        return None;
    }
    if is_own_window(conn, window, atoms) {
        return None;
    }
    let label = window_title(conn, window, atoms)?;
    let rect = window_rect(conn, screen_num, window)?;
    Some(SourceEntry {
        label,
        monitor: false,
        chosen: Chosen::Window(rect),
        window: Some(window),
    })
}

/// The client windows in stacking order, from `_NET_CLIENT_LIST` when the window
/// manager maintains it and from the window tree otherwise.
fn client_windows(conn: &impl Connection, root: XWindow, atoms: &Atoms) -> Vec<XWindow> {
    ewmh_client_windows(conn, root, atoms).unwrap_or_else(|| tree_client_windows(conn, root, atoms))
}

fn ewmh_client_windows(
    conn: &impl Connection,
    root: XWindow,
    atoms: &Atoms,
) -> Option<Vec<XWindow>> {
    let reply = conn
        .get_property(
            false,
            root,
            atoms.client_list,
            AtomEnum::WINDOW,
            0,
            u32::MAX,
        )
        .ok()?
        .reply()
        .ok()?;
    let windows: Vec<XWindow> = reply.value32()?.collect();
    if windows.is_empty() {
        None
    } else {
        Some(windows)
    }
}

fn tree_client_windows(conn: &impl Connection, root: XWindow, atoms: &Atoms) -> Vec<XWindow> {
    let children = match conn
        .query_tree(root)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
    {
        Some(reply) => reply.children,
        None => return Vec::new(),
    };
    let mut windows = Vec::new();
    for child in children {
        if window_title(conn, child, atoms).is_some() {
            windows.push(child);
        } else if let Some(reply) = conn
            .query_tree(child)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
        {
            // A reparenting window manager wraps each client in a frame; the
            // client is the frame's child.
            for nested in reply.children {
                if window_title(conn, nested, atoms).is_some() {
                    windows.push(nested);
                }
            }
        }
    }
    windows
}

struct Atoms {
    client_list: Atom,
    net_wm_name: Atom,
    net_wm_state: Atom,
    state_hidden: Atom,
    utf8_string: Atom,
    wm_class: Atom,
}

impl Atoms {
    fn new(conn: &impl Connection) -> Option<Self> {
        let client_list = conn
            .intern_atom(false, b"_NET_CLIENT_LIST")
            .ok()?
            .reply()
            .ok()?
            .atom;
        let net_wm_name = conn
            .intern_atom(false, b"_NET_WM_NAME")
            .ok()?
            .reply()
            .ok()?
            .atom;
        let net_wm_state = conn
            .intern_atom(false, b"_NET_WM_STATE")
            .ok()?
            .reply()
            .ok()?
            .atom;
        let state_hidden = conn
            .intern_atom(false, b"_NET_WM_STATE_HIDDEN")
            .ok()?
            .reply()
            .ok()?
            .atom;
        let utf8_string = conn
            .intern_atom(false, b"UTF8_STRING")
            .ok()?
            .reply()
            .ok()?
            .atom;
        let wm_class = conn
            .intern_atom(false, b"WM_CLASS")
            .ok()?
            .reply()
            .ok()?
            .atom;
        Some(Self {
            client_list,
            net_wm_name,
            net_wm_state,
            state_hidden,
            utf8_string,
            wm_class,
        })
    }
}

/// Minimized windows and windows on another desktop stay mapped on some window
/// managers, but EWMH marks them hidden.
fn window_hidden(conn: &impl Connection, window: XWindow, atoms: &Atoms) -> bool {
    let Some(reply) = conn
        .get_property(false, window, atoms.net_wm_state, AtomEnum::ATOM, 0, 64)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
    else {
        return false;
    };
    reply
        .value32()
        .is_some_and(|mut states| states.any(|state| state == atoms.state_hidden))
}

/// The window title: `_NET_WM_NAME` when it is UTF-8, `WM_NAME` otherwise.
fn window_title(conn: &impl Connection, window: XWindow, atoms: &Atoms) -> Option<String> {
    if let Some(reply) = conn
        .get_property(false, window, atoms.net_wm_name, atoms.utf8_string, 0, 1024)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
    {
        if reply.type_ == atoms.utf8_string {
            if let Ok(text) = String::from_utf8(reply.value) {
                let text = text.trim().to_string();
                if !text.is_empty() {
                    return Some(text);
                }
            }
        }
    }
    let reply = conn
        .get_property(false, window, AtomEnum::WM_NAME, AtomEnum::ANY, 0, 1024)
        .ok()?
        .reply()
        .ok()?;
    if reply.value.is_empty() {
        return None;
    }
    let text = String::from_utf8_lossy(&reply.value).trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn is_own_window(conn: &impl Connection, window: XWindow, atoms: &Atoms) -> bool {
    let Some(reply) = conn
        .get_property(false, window, atoms.wm_class, AtomEnum::STRING, 0, 1024)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
    else {
        return false;
    };
    let text = String::from_utf8_lossy(&reply.value);
    text.split('\0').any(|part| part == "CaptureButton")
}

/// The window's content rectangle in root coordinates, with the window manager
/// decorations accounted for.
fn window_rect(conn: &impl Connection, screen_num: usize, window: XWindow) -> Option<Rect> {
    let geometry = conn.get_geometry(window).ok()?.reply().ok()?;
    if geometry.width == 0 || geometry.height == 0 {
        return None;
    }
    let root = conn.setup().roots[screen_num].root;
    let translated = conn
        .translate_coordinates(window, root, 0, 0)
        .ok()?
        .reply()
        .ok()?;
    Some(Rect {
        x: i32::from(translated.dst_x),
        y: i32::from(translated.dst_y),
        w: u32::from(geometry.width),
        h: u32::from(geometry.height),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbered(width: u32, height: u32) -> Frame {
        let mut data = Vec::new();
        for y in 0..height {
            for x in 0..width {
                data.extend_from_slice(&[x as u8, y as u8, 0, 255]);
            }
        }
        Frame {
            width,
            height,
            data,
            age_ns: 0,
        }
    }

    #[test]
    fn a_portal_crop_selects_the_requested_rectangle() {
        let frame = crop_frame(numbered(6, 4), Some((2, 1, 3, 2)));
        assert_eq!((frame.width, frame.height), (2, 2));
        // The even size rounds 3 down to 2; the origin is exact.
        assert_eq!(&frame.data[0..4], &[2, 1, 0, 255]);
        assert_eq!(&frame.data[4..8], &[3, 1, 0, 255]);
        assert_eq!(&frame.data[8..12], &[2, 2, 0, 255]);
    }

    #[test]
    fn a_crop_outside_the_frame_is_clipped_or_ignored() {
        let frame = crop_frame(numbered(4, 4), Some((10, 10, 2, 2)));
        assert_eq!((frame.width, frame.height), (2, 2));
        assert_eq!(&frame.data[0..4], &[2, 2, 0, 255]);
        let whole = crop_frame(numbered(4, 4), Some((0, 0, 8, 8)));
        assert_eq!((whole.width, whole.height), (4, 4));
    }
}
