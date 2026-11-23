//! Screen capture and the list of capture sources, through X11.
//!
//! A pure Wayland session cannot be captured by an ordinary client, so the
//! capture calls return an explicit error there instead of a black frame. Under
//! XWayland the desktop is visible to X11 clients and everything here works.

use platform_core::types::{Frame, Rect};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use x11rb::connection::Connection;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat, Window as XWindow};

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
        while guard.is_none() || guard.as_ref().is_some_and(|(generation, _)| *generation <= self.seen)
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
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for ScreenCapture {
    fn drop(&mut self) {
        self.stop();
    }
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

fn monitors() -> Vec<Rect> {
    let Ok((conn, screen_num)) = x11rb::connect(None) else {
        return vec![screen_rect()];
    };
    let root = conn.setup().roots[screen_num].root;
    if let Ok(cookie) = conn.randr_get_monitors(root, true) {
        if let Ok(reply) = cookie.reply() {
            if !reply.monitors.is_empty() {
                return reply
                    .monitors
                    .iter()
                    .map(|monitor| Rect {
                        x: monitor.x as i32,
                        y: monitor.y as i32,
                        w: monitor.width as u32,
                        h: monitor.height as u32,
                    })
                    .collect();
            }
        }
    }
    vec![screen_rect()]
}

/// The monitors of the X screen, used by the region model.
pub fn monitor_rects() -> Vec<Rect> {
    monitors()
}

/// The monitor that contains the pointer, if X11 can be reached.
pub fn monitor_under_pointer() -> Option<Rect> {
    let (conn, screen_num) = x11rb::connect(None).ok()?;
    let root = conn.setup().roots[screen_num].root;
    let pointer = conn.query_pointer(root).ok()?.reply().ok()?;
    let point = (pointer.root_x as i32, pointer.root_y as i32);
    monitors().into_iter().find(|rect| {
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
    let (conn, screen_num) = x11rb::connect(None)
        .map_err(|error| format!("cannot open the X11 display: {error}"))?;
    let screen = &conn.setup().roots[screen_num];
    let root = screen.root;
    let (red_mask, green_mask, blue_mask) = visual_masks(&conn, screen_num)?;
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

    let bits = conn.setup().pixmap_formats.iter().find(|format| {
        format.depth == screen.root_depth
    });
    let bytes_per_pixel = bits.map(|format| format.bits_per_pixel as usize / 8).unwrap_or(4);
    let line = reply.data.len() / rect.h as usize;
    if bytes_per_pixel < 3 || line < bytes_per_pixel * rect.w as usize {
        return Err(format!(
            "unsupported X11 pixel layout: {bytes_per_pixel} bytes per pixel, {line} bytes per line"
        ));
    }
    let little = conn.setup().image_byte_order == x11rb::protocol::xproto::ImageOrder::LSB_FIRST;
    let mut data = vec![0u8; rect.w as usize * rect.h as usize * 4];
    for y in 0..rect.h as usize {
        for x in 0..rect.w as usize {
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
                let raw = [reply.data[offset], reply.data[offset + 1], reply.data[offset + 2], 0];
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
                (((pixel & mask) >> shift) as u32 * 255 / max.max(1)) as u8
            };
            let target = (y * rect.w as usize + x) * 4;
            data[target] = component(blue_mask);
            data[target + 1] = component(green_mask);
            data[target + 2] = component(red_mask);
            data[target + 3] = 255;
        }
    }
    Ok(Frame {
        width: rect.w,
        height: rect.h,
        data,
    })
}

fn visual_masks(conn: &impl Connection, screen_num: usize) -> Result<(u32, u32, u32), String> {
    let screen = &conn.setup().roots[screen_num];
    for depth in &screen.allowed_depths {
        for visual in &depth.visuals {
            if visual.visual_id == screen.root_visual {
                return Ok((visual.red_mask, visual.green_mask, visual.blue_mask));
            }
        }
    }
    Err("the root visual was not found".into())
}

pub fn grab_display(rect: Rect) -> Result<Frame, String> {
    grab_region(rect)
}

pub fn grab_still(rect: Rect) -> Result<Frame, String> {
    grab_region(rect)
}

/// The monitor that contains the middle of `rect`, with its origin and size.
pub fn monitor_of(rect: Rect) -> (Monitor, i32, i32, u32, u32) {
    let all = monitors();
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

/// Starts a capture thread that grabs the monitor (optionally cropped) at `fps`.
pub fn start_monitor(
    monitor: Monitor,
    crop: Option<(u32, u32, u32, u32)>,
    fps: u32,
    _cursor: bool,
) -> Result<(ScreenCapture, Frame), String> {
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
    let thread_slot = Arc::clone(&slot);
    let thread_stop = Arc::clone(&stop);
    let interval = Duration::from_millis(1000 / fps.max(1) as u64);
    let thread = std::thread::Builder::new()
        .name("capture-button-x11".into())
        .spawn(move || {
            let mut generation = 0u64;
            let mut next = Instant::now();
            while !thread_stop.load(Ordering::Relaxed) {
                next += interval;
                let wait = next.saturating_duration_since(Instant::now());
                if !wait.is_zero() {
                    std::thread::sleep(wait);
                }
                if thread_stop.load(Ordering::Relaxed) {
                    break;
                }
                if let Ok(frame) = grab_region(rect) {
                    generation += 1;
                    let mut guard = thread_slot
                        .frame
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    *guard = Some((generation, frame));
                    thread_slot.arrived.notify_all();
                }
            }
        })
        .map_err(|error| error.to_string())?;
    Ok((
        ScreenCapture {
            stop,
            slot,
            seen: 0,
            thread: Some(thread),
        },
        first,
    ))
}

/// Window capture is not implemented on Linux yet; the picker offers monitors.
pub fn start_window(_window: Window, _fps: u32, _cursor: bool) -> Result<(ScreenCapture, Frame), String> {
    Err("window capture is not supported on Linux yet".into())
}

pub fn take_window() -> Option<Window> {
    None
}

pub fn grab_staged_window() -> Result<Frame, String> {
    Err("window capture is not supported on Linux yet".into())
}

pub fn window_usable(_window: Window) -> bool {
    false
}

pub enum Chosen {
    Monitor(Rect),
    Window(Rect),
}

pub struct SourceEntry {
    pub label: String,
    pub monitor: bool,
    chosen: Chosen,
}

static SOURCES: std::sync::OnceLock<Mutex<Vec<SourceEntry>>> = std::sync::OnceLock::new();

fn source_slot() -> &'static Mutex<Vec<SourceEntry>> {
    SOURCES.get_or_init(|| Mutex::new(Vec::new()))
}

/// The sources offered by the in-app picker: the monitors of the X screen.
pub fn list_sources() -> Vec<SourceEntry> {
    let mut sources = Vec::new();
    for (index, area) in monitors().into_iter().enumerate() {
        sources.push(SourceEntry {
            label: format!("Display {index} — {}×{}", area.w, area.h),
            monitor: true,
            chosen: Chosen::Monitor(area),
        });
    }
    *source_slot()
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = sources
        .iter()
        .map(|entry| SourceEntry {
            label: entry.label.clone(),
            monitor: entry.monitor,
            chosen: match entry.chosen {
                Chosen::Monitor(rect) => Chosen::Monitor(rect),
                Chosen::Window(rect) => Chosen::Window(rect),
            },
        })
        .collect();
    sources
}

pub fn choose_source_index(index: usize) -> Option<Chosen> {
    let guard = source_slot()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    guard.get(index).map(|entry| match entry.chosen {
        Chosen::Monitor(rect) => Chosen::Monitor(rect),
        Chosen::Window(rect) => Chosen::Window(rect),
    })
}
