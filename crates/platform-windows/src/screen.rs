//! Screen and window capture through Windows Graphics Capture, plus the
//! monitor/window enumeration the app's source picker uses.
//!
//! The public surface is deliberately small and value-based: the app stores an
//! opaque [`Window`] or [`Monitor`] and a [`ScreenCapture`] handle, but never
//! touches `windows_capture` itself. A future backend provides the same types.

use platform_core::frame::fit;
use platform_core::i18n::{tr, trf};
use platform_core::screen::{crop_for, source_row, window_state_usable, MonitorInfo};
use platform_core::types::{Frame, Rect};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint, HDC, HMONITOR, MONITORINFO,
    MONITORINFOEXW, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowPlacement, IsIconic, IsWindow, IsWindowVisible, SW_MINIMIZE, SW_SHOWMINIMIZED,
    SW_SHOWMINNOACTIVE, WINDOWPLACEMENT,
};
use windows_capture::capture::{CaptureControl, Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame as WgcFrame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::monitor::Monitor as WgcMonitor;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};
use windows_capture::window::Window as WgcWindow;

/// An opaque monitor handle the app can store and hand back.
#[derive(Clone, Copy)]
pub struct Monitor(WgcMonitor);

/// An opaque window handle the app can store and hand back.
#[derive(Clone, Copy)]
pub struct Window(WgcWindow);

struct Slot {
    frame: Mutex<Option<Frame>>,
    arrived: Condvar,
}

struct Handler {
    slot: Arc<Slot>,
    crop: Option<(u32, u32, u32, u32)>,
    scratch: Vec<u8>,
}

impl GraphicsCaptureApiHandler for Handler {
    type Flags = (Arc<Slot>, Option<(u32, u32, u32, u32)>);
    type Error = String;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        let (slot, crop) = ctx.flags;
        Ok(Self {
            slot,
            crop,
            scratch: Vec::new(),
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut WgcFrame,
        _control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        // Read the frame's own capture time before touching its pixels: the
        // pool hands frames over late, and the loop needs that delay to place
        // the picture where it actually happened.
        let age_ns = frame_age_ns(frame);
        let buffer = match self.crop {
            Some((x, y, w, h)) => {
                let max_w = frame.width();
                let max_h = frame.height();
                let x = x.min(max_w.saturating_sub(2));
                let y = y.min(max_h.saturating_sub(2));
                let w = (w.min(max_w - x)) & !1;
                let h = (h.min(max_h - y)) & !1;
                if w < 2 || h < 2 {
                    frame.buffer().map_err(|error| error.to_string())?
                } else {
                    frame
                        .buffer_crop(x, y, x + w, y + h)
                        .map_err(|error| error.to_string())?
                }
            }
            None => frame.buffer().map_err(|error| error.to_string())?,
        };
        let width = buffer.width();
        let height = buffer.height();
        let mut data = buffer.as_nopadding_buffer(&mut self.scratch).to_vec();
        // A restore or resize can report a size the bytes do not cover. Keeping
        // that frame would panic in the release build and leave the file unindexed.
        if data.len() != width as usize * height as usize * 4 || width < 2 || height < 2 {
            return Ok(());
        }
        for pixel in data.as_chunks_mut::<4>().0 {
            pixel[3] = 255;
        }
        let mut guard = self
            .slot
            .frame
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *guard = Some(Frame {
            width,
            height,
            data,
            age_ns,
        });
        self.slot.arrived.notify_all();
        Ok(())
    }
}

/// A frame's age right now: the performance counter minus the frame's own
/// system-relative timestamp. Both count from the same boot epoch.
fn frame_age_ns(frame: &WgcFrame) -> u64 {
    let stamp = match frame.timestamp() {
        Ok(stamp) => stamp.Duration,
        Err(error) => {
            warn_once(&format!("frame timestamp unavailable: {error}"));
            return 0;
        }
    };
    let mut counter = 0i64;
    let mut frequency = 0i64;
    // SAFETY: Both calls write into locals and have no other preconditions.
    unsafe {
        QueryPerformanceCounter(&mut counter).ok();
        QueryPerformanceFrequency(&mut frequency).ok();
    }
    age_ns_from_qpc(counter, frequency, stamp)
}

/// Converts the two clocks into an age in nanoseconds. The performance counter
/// is in frequency ticks, the frame stamp in 100-ns units; a stamp ahead of the
/// counter (clock skew) or a missing clock means "unknown", which is zero.
fn age_ns_from_qpc(counter: i64, frequency: i64, stamp_100ns: i64) -> u64 {
    if counter <= 0 || frequency <= 0 || stamp_100ns <= 0 {
        return 0;
    }
    let now_ns = (i128::from(counter) * 1_000_000_000 / i128::from(frequency)) as u64;
    let stamp_ns = (stamp_100ns as u128 * 100) as u64;
    now_ns.saturating_sub(stamp_ns)
}

/// Logs the first failure of the frame clock only: a broken timestamp would
/// otherwise repeat on every frame.
fn warn_once(message: &str) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WARNED: AtomicBool = AtomicBool::new(false);
    if !WARNED.swap(true, Ordering::Relaxed) {
        platform_core::logging::warn(message);
    }
}

#[cfg(test)]
mod tests {
    use super::age_ns_from_qpc;

    #[test]
    fn the_frame_age_is_the_counter_minus_its_stamp() {
        // 5 s on a 10 MHz counter, with a stamp 200 ms older.
        assert_eq!(age_ns_from_qpc(50_000_000, 10_000_000, 48_000_000), 200_000_000);
    }

    #[test]
    fn a_stamp_ahead_of_the_counter_is_zero_not_a_huge_age() {
        assert_eq!(age_ns_from_qpc(50_000_000, 10_000_000, 51_000_000), 0);
    }

    #[test]
    fn a_missing_clock_reports_unknown() {
        assert_eq!(age_ns_from_qpc(50_000_000, 0, 48_000_000), 0);
        assert_eq!(age_ns_from_qpc(50_000_000, 10_000_000, 0), 0);
        assert_eq!(age_ns_from_qpc(0, 10_000_000, 48_000_000), 0);
    }
}

/// A running Windows Graphics Capture session.
pub struct ScreenCapture {
    control: Option<CaptureControl<Handler, String>>,
    slot: Arc<Slot>,
    window: Option<WgcWindow>,
}

impl ScreenCapture {
    fn start(
        monitor: WgcMonitor,
        crop: Option<(u32, u32, u32, u32)>,
        fps: u32,
        cursor: bool,
    ) -> Result<(Self, Frame), String> {
        Self::start_with_cursor(
            monitor,
            crop,
            fps,
            if cursor {
                CursorCaptureSettings::WithCursor
            } else {
                CursorCaptureSettings::WithoutCursor
            },
        )
    }

    fn start_with_cursor(
        monitor: WgcMonitor,
        crop: Option<(u32, u32, u32, u32)>,
        fps: u32,
        cursor: CursorCaptureSettings,
    ) -> Result<(Self, Frame), String> {
        let slot = Arc::new(Slot {
            frame: Mutex::new(None),
            arrived: Condvar::new(),
        });
        let interval = Duration::from_secs_f64(1.0 / fps.max(1) as f64);
        let settings = |border| {
            Settings::new(
                monitor,
                cursor,
                border,
                SecondaryWindowSettings::Exclude,
                MinimumUpdateIntervalSettings::Custom(interval),
                DirtyRegionSettings::Default,
                ColorFormat::Bgra8,
                (slot.clone(), crop),
            )
        };
        let control = Handler::start_free_threaded(settings(DrawBorderSettings::WithoutBorder))
            .or_else(|_| Handler::start_free_threaded(settings(DrawBorderSettings::Default)))
            .map_err(|error| {
                trf(
                    "Windows Graphics Capture: {error}",
                    &[("error", &error.to_string())],
                )
            })?;
        let capture = Self {
            control: Some(control),
            slot: slot.clone(),
            window: None,
        };
        let frame = wait_frame(&slot, Duration::from_secs(3))?;
        Ok((capture, frame))
    }

    fn start_window(window: WgcWindow, fps: u32, cursor: bool) -> Result<(Self, Frame), String> {
        let slot = Arc::new(Slot {
            frame: Mutex::new(None),
            arrived: Condvar::new(),
        });
        let interval = Duration::from_secs_f64(1.0 / fps.max(1) as f64);
        let settings = |border| {
            Settings::new(
                window,
                if cursor {
                    CursorCaptureSettings::WithCursor
                } else {
                    CursorCaptureSettings::WithoutCursor
                },
                border,
                SecondaryWindowSettings::Exclude,
                MinimumUpdateIntervalSettings::Custom(interval),
                DirtyRegionSettings::Default,
                ColorFormat::Bgra8,
                (slot.clone(), None),
            )
        };
        let control = Handler::start_free_threaded(settings(DrawBorderSettings::WithoutBorder))
            .or_else(|_| Handler::start_free_threaded(settings(DrawBorderSettings::Default)))
            .map_err(|error| format!("Windows Graphics Capture: {error}"))?;
        let capture = Self {
            control: Some(control),
            slot: slot.clone(),
            window: Some(window),
        };
        let frame = wait_frame(&slot, Duration::from_secs(3))?;
        Ok((capture, frame))
    }

    pub fn capture_alive(&self) -> bool {
        self.control
            .as_ref()
            .is_some_and(|control| !control.is_finished())
    }

    pub fn available(&self) -> bool {
        self.capture_alive() && self.window.is_none_or(window_usable_raw)
    }

    pub fn latest(&self) -> Option<Frame> {
        self.slot
            .frame
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
    }

    pub fn stop(&mut self) {
        if let Some(control) = self.control.take() {
            let _ = control.stop();
        }
    }
}

impl Drop for ScreenCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Starts a capture of one monitor. `crop` is the sub-rectangle within that
/// monitor, or `None` for the whole monitor.
pub fn start_monitor(
    monitor: Monitor,
    crop: Option<(u32, u32, u32, u32)>,
    fps: u32,
    cursor: bool,
) -> Result<(ScreenCapture, Frame), String> {
    ScreenCapture::start(monitor.0, crop, fps, cursor)
}

pub fn start_window(window: Window, fps: u32, cursor: bool) -> Result<(ScreenCapture, Frame), String> {
    ScreenCapture::start_window(window.0, fps, cursor)
}

fn wait_frame(slot: &Slot, timeout: Duration) -> Result<Frame, String> {
    let mut guard = slot.frame.lock().unwrap_or_else(|error| error.into_inner());
    let start = Instant::now();
    while guard.is_none() {
        let remaining = timeout.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            return Err(tr("Windows Graphics Capture did not deliver a frame"));
        }
        let (next, status) = slot
            .arrived
            .wait_timeout(guard, remaining)
            .unwrap_or_else(|error| error.into_inner());
        guard = next;
        if status.timed_out() && guard.is_none() {
            return Err("Windows Graphics Capture did not deliver a frame".into());
        }
    }
    guard
        .as_ref()
        .cloned()
        .ok_or_else(|| "Windows Graphics Capture did not deliver a frame".into())
}

pub fn monitor_of(rect: Rect) -> (Monitor, i32, i32, u32, u32) {
    // SAFETY: any screen point is a valid query. `info.cbSize` is set before the call, and the returned handle is only used as an HMONITOR value.
    unsafe {
        let point = POINT {
            x: rect.x + rect.w as i32 / 2,
            y: rect.y + rect.h as i32 / 2,
        };
        let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        GetMonitorInfoW(monitor, &mut info).ok();
        let origin_x = info.rcMonitor.left;
        let origin_y = info.rcMonitor.top;
        let width = (info.rcMonitor.right - info.rcMonitor.left).max(0) as u32;
        let height = (info.rcMonitor.bottom - info.rcMonitor.top).max(0) as u32;
        (
            Monitor(WgcMonitor::from_raw_hmonitor(monitor.0)),
            origin_x,
            origin_y,
            width,
            height,
        )
    }
}

/// A copy of the screen that does not open a second capture session. A screenshot
/// never contains the pointer, whatever the profile says.
pub fn grab_display(rect: Rect) -> Result<Frame, String> {
    let mut rect = rect;
    rect.w &= !1;
    rect.h &= !1;
    if rect.w == 0 || rect.h == 0 {
        return Err(tr("The capture area is empty"));
    }
    crate::os::grab_without_cursor(rect)
}

pub fn grab_still(rect: Rect) -> Result<Frame, String> {
    let mut rect = rect;
    rect.w &= !1;
    rect.h &= !1;
    let (monitor, origin_x, origin_y, monitor_w, monitor_h) = monitor_of(rect);
    match ScreenCapture::start(
        monitor.0,
        crop_for(rect, origin_x, origin_y, monitor_w, monitor_h),
        30,
        false,
    ) {
        Ok((mut capture, frame)) => {
            capture.stop();
            Ok(fit(frame, rect.w, rect.h))
        }
        Err(_) => crate::os::grab_without_cursor(rect),
    }
}

static STAGED: Mutex<Option<WgcWindow>> = Mutex::new(None);

pub fn take_window() -> Option<Window> {
    STAGED
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take()
        .map(Window)
}

#[derive(Clone, Copy)]
pub enum Chosen {
    Monitor(Rect),
    Window(Rect),
}

pub struct SourceEntry {
    pub label: String,
    pub monitor: bool,
    chosen: Chosen,
    window: Option<WgcWindow>,
}

/// The sources offered by the in-app picker, in a stable order.
pub fn list_sources() -> Vec<SourceEntry> {
    let monitors = monitor_list();
    let mut windows = WgcWindow::enumerate().unwrap_or_default();
    windows.retain(listed_window);
    let mut entries = Vec::new();
    for (index, monitor) in monitors.iter().enumerate() {
        entries.push(SourceEntry {
            label: format!(
                "{} {}  -  {}x{}",
                tr("Monitor"),
                index + 1,
                monitor.w,
                monitor.h
            ),
            chosen: Chosen::Monitor(*monitor),
            monitor: true,
            window: None,
        });
    }
    for window in windows {
        let title = window.title().unwrap_or_else(|_| "Window".into());
        let (w, h) = window
            .rect()
            .map(|rect| {
                (
                    (rect.right - rect.left).max(0) as u32,
                    (rect.bottom - rect.top).max(0) as u32,
                )
            })
            .unwrap_or((0, 0));
        entries.push(SourceEntry {
            label: format!("{}  -  {title}  -  {w}x{h}", tr("Window")),
            chosen: Chosen::Window(Rect {
                x: window.rect().map(|r| r.left).unwrap_or(0),
                y: window.rect().map(|r| r.top).unwrap_or(0),
                w,
                h,
            }),
            monitor: false,
            window: Some(window),
        });
    }
    entries
}

pub fn choose_source_index(index: usize) -> Option<Chosen> {
    let entries = list_sources();
    let entry = entries.get(index)?;
    if let Some(window) = entry.window {
        *STAGED.lock().unwrap_or_else(|error| error.into_inner()) = Some(window);
    }
    Some(entry.chosen)
}

pub fn grab_staged_window() -> Result<Frame, String> {
    let window = take_window()
        .ok_or_else(|| "The window is no longer available".to_string())?
        .0;
    let (mut capture, frame) = ScreenCapture::start_window(window, 30, false)?;
    capture.stop();
    Ok(frame)
}

pub fn window_usable(window: Window) -> bool {
    window_usable_raw(window.0)
}

fn window_usable_raw(window: WgcWindow) -> bool {
    let hwnd = HWND(window.as_raw_hwnd());
    let (width, height) = window
        .rect()
        .map(|rect| (rect.right - rect.left, rect.bottom - rect.top))
        .unwrap_or((0, 0));
    window_state_usable(
        exists(hwnd),
        visible(hwnd),
        iconic(hwnd),
        cloaked(hwnd),
        minimized(hwnd),
        width,
        height,
    )
}

fn exists(hwnd: HWND) -> bool {
    unsafe {
        // SAFETY: The handle comes from the window being recorded. The call only checks that it still exists.
        IsWindow(hwnd)
    }
    .as_bool()
}

fn visible(hwnd: HWND) -> bool {
    unsafe {
        // SAFETY: The handle comes from the window being recorded. The call only reads its visibility.
        IsWindowVisible(hwnd)
    }
    .as_bool()
}

fn minimized(hwnd: HWND) -> bool {
    let mut place = WINDOWPLACEMENT {
        length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
        ..Default::default()
    };
    let read = unsafe {
        // SAFETY: The handle comes from the window being recorded, and place is a writable WINDOWPLACEMENT with its size set.
        GetWindowPlacement(hwnd, &mut place)
    };
    if read.is_err() {
        return false;
    }
    let cmd = place.showCmd;
    cmd == SW_SHOWMINIMIZED.0 as u32
        || cmd == SW_MINIMIZE.0 as u32
        || cmd == SW_SHOWMINNOACTIVE.0 as u32
}

fn listed_window(window: &WgcWindow) -> bool {
    let title = window.title().unwrap_or_default();
    let (width, height) = window
        .rect()
        .map(|rect| (rect.right - rect.left, rect.bottom - rect.top))
        .unwrap_or((0, 0));
    let hwnd = HWND(window.as_raw_hwnd());
    source_row(iconic(hwnd), cloaked(hwnd), width, height, &title)
}

fn iconic(hwnd: HWND) -> bool {
    unsafe {
        // SAFETY: The handle comes from the window list or the window being recorded. The call only reads its state.
        IsIconic(hwnd)
    }
    .as_bool()
}

fn cloaked(hwnd: HWND) -> bool {
    let mut value = 0u32;
    let read = unsafe {
        // SAFETY: The handle is a window, and value is a writable u32 of the size passed in.
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            std::ptr::addr_of_mut!(value).cast(),
            std::mem::size_of::<u32>() as u32,
        )
    };
    read.is_ok() && value != 0
}

/// The monitors of the desktop, in the enumerator's order, as the Settings
/// window lists them. A monitor's id is its device name, which Windows keeps
/// stable across restarts and reconnects.
/// MONITORINFOF_PRIMARY: this monitor is the main display. The windows 0.58
/// bindings do not export the constant, so it is spelled out here.
const MONITORINFOF_PRIMARY: u32 = 1;

pub fn monitors() -> Vec<MonitorInfo> {
    // SAFETY: The callback's Vec stays alive until EnumDisplayMonitors returns.
    unsafe extern "system" fn collect(
        monitor: HMONITOR,
        _: HDC,
        _: *mut RECT,
        data: LPARAM,
    ) -> BOOL {
        let mut info = MONITORINFOEXW {
            monitorInfo: MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFOEXW>() as u32,
                ..Default::default()
            },
            ..Default::default()
        };
        // SAFETY: The monitor handle came from the enumerator and cbSize is that
        // of the extended structure, which starts with MONITORINFO.
        if unsafe {
            GetMonitorInfoW(
                monitor,
                std::ptr::addr_of_mut!(info).cast::<MONITORINFO>(),
            )
        }
        .as_bool()
        {
            let name = String::from_utf16_lossy(
                &info.szDevice[..info
                    .szDevice
                    .iter()
                    .position(|unit| *unit == 0)
                    .unwrap_or(info.szDevice.len())],
            );
            let rect = info.monitorInfo.rcMonitor;
            let primary = info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0;
            // SAFETY: data is the Vec passed into EnumDisplayMonitors and it
            // outlives this callback.
            unsafe { &mut *(data.0 as *mut Vec<MonitorInfo>) }.push(MonitorInfo {
                id: name.clone(),
                name: name.strip_prefix(r"\\.\").unwrap_or(&name).to_string(),
                width: (rect.right - rect.left).max(0) as u32,
                height: (rect.bottom - rect.top).max(0) as u32,
                primary,
                x: rect.left,
                y: rect.top,
            });
        }
        BOOL(1)
    }
    let mut monitors = Vec::new();
    unsafe {
        // SAFETY: collect only writes into monitors, which lives until this call returns.
        EnumDisplayMonitors(
            None,
            None,
            Some(collect),
            LPARAM(&mut monitors as *mut Vec<MonitorInfo> as isize),
        )
    }
    .ok();
    monitors
}

/// Whether the Settings window can offer a monitor choice: EnumDisplayMonitors
/// works on every Windows session.
pub fn monitors_supported() -> bool {
    true
}

fn monitor_list() -> Vec<Rect> {
    // SAFETY: The callback's Vec stays alive until EnumDisplayMonitors returns.
    unsafe extern "system" fn collect(
        monitor: HMONITOR,
        _: HDC,
        _: *mut RECT,
        data: LPARAM,
    ) -> BOOL {
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if unsafe {
            // SAFETY: The monitor handle came from the enumerator and cbSize is set.
            GetMonitorInfoW(monitor, &mut info)
        }
        .as_bool()
        {
            let rect = info.rcMonitor;
            unsafe {
                // SAFETY: data is the Vec passed into EnumDisplayMonitors and it outlives this callback.
                &mut *(data.0 as *mut Vec<Rect>)
            }
            .push(Rect {
                x: rect.left,
                y: rect.top,
                w: (rect.right - rect.left).max(0) as u32,
                h: (rect.bottom - rect.top).max(0) as u32,
            });
        }
        BOOL(1)
    }
    let mut rects = Vec::new();
    unsafe {
        // SAFETY: collect only writes into rects, which lives until this call returns.
        EnumDisplayMonitors(
            None,
            None,
            Some(collect),
            LPARAM(&mut rects as *mut Vec<Rect> as isize),
        )
    }
    .ok();
    rects
}
