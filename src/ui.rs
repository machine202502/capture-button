//! The application window is a Slint component. It lives on its own thread and
//! talks to the capture engine through a small shared model plus posted
//! requests. Closing the window tears the component down; the tray thread stays
//! asleep with no timers and no child windows.

#![allow(dead_code)]

use crate::i18n::{tr, trf};
use crate::{media, AudioSettings, CaptureSettings, ProfileRef, Settings};
use media::{Format, VideoSettings};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::mpsc::{self, Sender, TryRecvError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::Graphics::Gdi::{
    RedrawWindow, HRGN, RDW_ALLCHILDREN, RDW_INVALIDATE, RDW_UPDATENOW,
};
use windows::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, EnumWindows, GetClientRect, GetSystemMetrics, GetWindowLongPtrW,
    GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible, PeekMessageW,
    PostMessageW, PostThreadMessageW, SetWindowLongPtrW, SetWindowPos, TranslateMessage,
    WaitMessage, GWL_EXSTYLE, GWL_STYLE, MINMAXINFO, MSG, PM_REMOVE, SIZE_RESTORED, SM_CXSCREEN,
    SM_CYSCREEN, SWP_FRAMECHANGED, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, WINDOWPOS, WM_ACTIVATE,
    WM_APP, WM_GETMINMAXINFO, WM_SHOWWINDOW, WM_SIZE, WM_WINDOWPOSCHANGING, WS_MAXIMIZEBOX,
    WS_MINIMIZEBOX, WS_SYSMENU, WS_THICKFRAME,
};

slint::include_modules!();

pub const WM_UI: u32 = WM_APP + 4;

pub const ID_TAB: i32 = 100;
pub const ID_SETTINGS_TAB: i32 = 101;
pub const ID_REFRESH: i32 = 102;
pub const ID_ABOUT: i32 = 103;
pub const ID_ABOUT_CLOSE: i32 = 281;
pub const ID_ABOUT_RECORDINGS: i32 = 282;
pub const ID_ABOUT_LOGS: i32 = 283;
pub const ID_ABOUT_GITHUB: i32 = 284;
pub const ID_ABOUT_UPDATE: i32 = 287;
pub const ID_SOURCE_CLOSE: i32 = 285;
pub const ID_COPY_OUTPUT: i32 = 286;
pub const ID_OPEN_OUTPUT: i32 = 292;
pub const ID_CAMERA_PHOTO: i32 = 289;
pub const ID_CAMERA_VIDEO: i32 = 290;
pub const ID_CAMERA_CLOSE: i32 = 291;
pub const CAMERA_ROW_BASE: i32 = 500;
pub const SOURCE_ROW_BASE: i32 = 400;
pub const ID_FORMAT: i32 = 110;
pub const ID_PRESET: i32 = 120;
pub const ID_RESOLUTION: i32 = 121;
pub const ID_FPS: i32 = 122;
pub const ID_FPS_VALUE: i32 = 123;
pub const ID_SYSTEM_AUDIO: i32 = 124;
pub const ID_MIC: i32 = 125;
pub const ID_MIC_DEVICE: i32 = 126;
pub const ID_SHOT_SCREEN: i32 = 130;
pub const ID_SHOT_SOURCE: i32 = 147;
pub const ID_SHOT_REGION: i32 = 131;
pub const ID_CAPTURE_SCREEN: i32 = 132;
pub const ID_CAPTURE_SOURCE: i32 = 148;
pub const ID_CAPTURE_REGION: i32 = 133;
pub const ID_CAPTURE_AUDIO: i32 = 134;
pub const ID_STOP: i32 = 135;
pub const ID_STATUS: i32 = 136;
pub const ID_HISTORY: i32 = 210;
pub const ID_HISTORY_COPY: i32 = 230;
pub const ID_HISTORY_REVEAL: i32 = 250;
pub const ID_HISTORY_OPEN: i32 = 270;
pub const HISTORY_ACK_TIMER: usize = 2;
pub const HISTORY_COUNT: i32 = 12;
pub const ID_HEADER_BAND: i32 = 104;
pub const HEADER_HEIGHT: i32 = 64;
pub const WATERLINE: i32 = 2;
pub const VIEWPORT_TOP: i32 = 74;
pub const ID_SHOT_LABEL: i32 = 137;
pub const ID_CAPTURE_LABEL: i32 = 138;
pub const ID_AUDIO_LABEL: i32 = 139;
pub const ID_STOP_LABEL: i32 = 140;
pub const ID_AUTOSTART: i32 = 150;
pub const ID_START_MINIMIZED: i32 = 151;
pub const ID_HIDE_ACTIVE: i32 = 152;
pub const ID_CLOSE_TRAY: i32 = 153;
pub const ID_CLIPBOARD: i32 = 154;
pub const ID_TRAY_CAPTURE: i32 = 155;
pub const ID_HIGHLIGHT: i32 = 156;
pub const ID_NOTIFY: i32 = 157;
pub const ID_NO_SIGNAL: i32 = 158;
pub const ID_CARD: i32 = 159;
pub const ID_TRAY_AREA_FORMAT: i32 = 160;
pub const ID_TRAY_AREA_FPS_VALUE: i32 = 165;
pub const ID_TRAY_AREA_DEVICE: i32 = 168;
pub const ID_TRAY_AREA_PRESET: i32 = 169;
pub const ID_TRAY_VIDEO_FORMAT: i32 = 170;
pub const ID_TRAY_VIDEO_FPS_VALUE: i32 = 175;
pub const ID_TRAY_VIDEO_DEVICE: i32 = 178;
pub const ID_TRAY_VIDEO_PRESET: i32 = 179;
pub const ID_THREADS: i32 = 180;
pub const ID_AREA_HEADING: i32 = 181;
pub const ID_VIDEO_HEADING: i32 = 182;
pub const ID_THREADS_HEADING: i32 = 183;
pub const ID_SCALE_HEADING: i32 = 184;
pub const ID_SCALE: i32 = 185;
pub const ID_CURSOR: i32 = 186;
pub const ID_TRAY_AREA_CURSOR: i32 = 187;
pub const ID_TRAY_VIDEO_CURSOR: i32 = 188;
pub const ID_TRAY_AUDIO_SYSTEM: i32 = 190;
pub const ID_TRAY_AUDIO_MIC: i32 = 191;
pub const ID_TRAY_AUDIO_DEVICE: i32 = 192;
pub const ID_TRAY_AUDIO_HEADING: i32 = 193;
pub const ID_MIC_GAIN: i32 = 194;
pub const ID_TRAY_AREA_GAIN: i32 = 195;
pub const ID_TRAY_VIDEO_GAIN: i32 = 196;
pub const ID_TRAY_AUDIO_GAIN: i32 = 197;
pub const ID_MIC_GAIN_VALUE: i32 = 198;
pub const ID_TRAY_AREA_GAIN_VALUE: i32 = 199;
pub const ID_TRAY_VIDEO_GAIN_VALUE: i32 = 207;
pub const ID_LANGUAGE_HEADING: i32 = 222;
pub const ID_LANGUAGE: i32 = 223;
pub const ID_SOUND_VOLUME: i32 = 226;
pub const ID_SCREENSHOT_SOUND: i32 = 227;
pub const ID_TRAY_AUDIO_GAIN_VALUE: i32 = 208;
pub const MIC_GAIN_MAX: u32 = 300;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum HistoryClick {
    Label,
    CopyFile,
    Reveal,
    Open,
}

pub fn history_id(id: i32) -> bool {
    (ID_HISTORY..ID_HISTORY + HISTORY_COUNT).contains(&id)
}

pub fn is_checkbox(id: i32) -> bool {
    matches!(
        id,
        ID_SYSTEM_AUDIO | ID_MIC | ID_TRAY_AUDIO_SYSTEM | ID_TRAY_AUDIO_MIC | ID_AUTOSTART
            ..=ID_CARD
                | ID_CURSOR
                | ID_TRAY_AREA_CURSOR
                | ID_TRAY_VIDEO_CURSOR
                | 166
                | 167
                | 176
                | 177
    )
}

pub fn is_trackbar(id: i32) -> bool {
    matches!(
        id,
        ID_FPS
            | ID_MIC_GAIN
            | ID_TRAY_AREA_GAIN
            | ID_TRAY_VIDEO_GAIN
            | ID_TRAY_AUDIO_GAIN
            | 164
            | 174
    )
}

pub fn is_mic_gain(id: i32) -> bool {
    matches!(
        id,
        ID_MIC_GAIN | ID_TRAY_AREA_GAIN | ID_TRAY_VIDEO_GAIN | ID_TRAY_AUDIO_GAIN
    )
}

pub fn is_mic_gain_value(id: i32) -> bool {
    matches!(
        id,
        ID_MIC_GAIN_VALUE
            | ID_TRAY_AREA_GAIN_VALUE
            | ID_TRAY_VIDEO_GAIN_VALUE
            | ID_TRAY_AUDIO_GAIN_VALUE
    )
}

pub fn microphone_control(source: ProfileRef) -> i32 {
    match source {
        ProfileRef::Capture => ID_MIC_GAIN,
        ProfileRef::Area => ID_TRAY_AREA_GAIN,
        ProfileRef::Video => ID_TRAY_VIDEO_GAIN,
        ProfileRef::Audio => ID_TRAY_AUDIO_GAIN,
    }
}

pub fn profile_for_microphone(id: i32) -> Option<ProfileRef> {
    match id {
        ID_MIC_GAIN => Some(ProfileRef::Capture),
        ID_TRAY_AREA_GAIN => Some(ProfileRef::Area),
        ID_TRAY_VIDEO_GAIN => Some(ProfileRef::Video),
        ID_TRAY_AUDIO_GAIN => Some(ProfileRef::Audio),
        _ => None,
    }
}

pub fn gain_value_id(id: i32) -> i32 {
    match id {
        ID_MIC_GAIN => ID_MIC_GAIN_VALUE,
        ID_TRAY_AREA_GAIN => ID_TRAY_AREA_GAIN_VALUE,
        ID_TRAY_VIDEO_GAIN => ID_TRAY_VIDEO_GAIN_VALUE,
        ID_TRAY_AUDIO_GAIN => ID_TRAY_AUDIO_GAIN_VALUE,
        _ => ID_MIC_GAIN_VALUE,
    }
}

pub fn history_click(id: i32) -> Option<(usize, HistoryClick)> {
    let index = |base| {
        let index = id - base;
        (0..HISTORY_COUNT)
            .contains(&index)
            .then_some(index as usize)
    };
    if let Some(index) = index(ID_HISTORY) {
        Some((index, HistoryClick::Label))
    } else if let Some(index) = index(ID_HISTORY_COPY) {
        Some((index, HistoryClick::CopyFile))
    } else if let Some(index) = index(ID_HISTORY_REVEAL) {
        Some((index, HistoryClick::Reveal))
    } else {
        index(ID_HISTORY_OPEN).map(|index| (index, HistoryClick::Open))
    }
}

#[derive(Debug)]
pub enum UiRequest {
    Command { id: i32, notification: u16 },
    Slider { id: i32, value: u32, persist: bool },
    CloseRequested,
}

enum UiCommand {
    Show(Controls),
}

/// Wakes the UI thread out of `WaitMessage` when a command is queued. It is a
/// thread message (no window), so the Slint event loop simply ignores it while
/// the window is open.
const WM_UI_WAKE: u32 = WM_APP + 5;

static UI_REQUESTS: Mutex<Vec<UiRequest>> = Mutex::new(Vec::new());
static UI_HWND: AtomicIsize = AtomicIsize::new(0);
static UI_TID: AtomicU32 = AtomicU32::new(0);
static UI_VISIBLE: AtomicBool = AtomicBool::new(false);
static UI_WEAK: Mutex<Option<slint::Weak<MainWindow>>> = Mutex::new(None);
static UI_TX: OnceLock<Sender<UiCommand>> = OnceLock::new();

pub fn attach(hwnd: HWND) {
    UI_HWND.store(hwnd.0 as isize, Ordering::Relaxed);
}

/// Slint's software renderer can come back from a minimize with an empty
/// back buffer on Windows. The window still exists, but DWM shows the old
/// desktop through it until another window forces a paint. Subclassing the
/// Slint HWND lets us invalidate it on restore without a polling timer.
fn install_paint_guard() {
    let mut found = 0isize;
    unsafe {
        // SAFETY: EnumWindows only calls the callback with top-level window
        // handles; `found` outlives the enumeration.
        let _ = EnumWindows(
            Some(find_slint_window),
            LPARAM(&mut found as *mut isize as isize),
        );
    }
    if found != 0 {
        let installed = unsafe {
            // SAFETY: The handle belongs to this process and stays alive while
            // the Slint event loop runs.
            SetWindowSubclass(HWND(found as *mut _), Some(paint_subclass), 1, 0)
        };
        crate::logging::info(format!(
            "paint guard {found} installed {}",
            installed.as_bool()
        ));
        let hwnd = HWND(found as *mut _);
        let style = unsafe {
            // SAFETY: The handle is the Slint window found above.
            GetWindowLongPtrW(hwnd, GWL_STYLE)
        } as u32;
        let ex_style = unsafe {
            // SAFETY: The handle is the Slint window found above.
            GetWindowLongPtrW(hwnd, GWL_EXSTYLE)
        } as u32;
        let resizable =
            style | WS_THICKFRAME.0 | WS_MAXIMIZEBOX.0 | WS_MINIMIZEBOX.0 | WS_SYSMENU.0;
        if resizable != style {
            unsafe {
                // SAFETY: The style belongs to the Slint window; the replacement keeps its other bits.
                SetWindowLongPtrW(hwnd, GWL_STYLE, resizable as isize);
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED,
                );
            }
        }
        crate::logging::info(format!(
            "window style {style:#x} ex {ex_style:#x} style_changed {}",
            resizable != style
        ));
        unsafe {
            // SAFETY: The handle is the Slint window found above.
            let _ = RedrawWindow(
                hwnd,
                None,
                HRGN::default(),
                RDW_INVALIDATE | RDW_UPDATENOW | RDW_ALLCHILDREN,
            );
        }
    } else {
        crate::logging::warn("paint guard: no Slint window");
    }
}

unsafe extern "system" fn find_slint_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: hwnd was supplied by EnumWindows and is only queried.
    if !unsafe { IsWindowVisible(hwnd).as_bool() } {
        return BOOL(1);
    }
    let mut owner = 0u32;
    // SAFETY: The window handle comes from EnumWindows and the pid pointer is a writable local.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut owner)) };
    // SAFETY: GetCurrentProcessId has no preconditions.
    if owner != unsafe { GetCurrentProcessId() } {
        return BOOL(1);
    }
    let mut title = [0u16; 32];
    // SAFETY: The title buffer is valid for the duration of the call.
    let length = unsafe { GetWindowTextW(hwnd, &mut title) };
    if String::from_utf16_lossy(&title[..length as usize]) != "Capture Button" {
        return BOOL(1);
    }
    let mut rect = Default::default();
    // SAFETY: The RECT is a writable local and hwnd is a valid top-level window.
    if unsafe { GetWindowRect(hwnd, &mut rect) }.is_err() {
        return BOOL(1);
    }
    let area = (rect.right - rect.left) as i64 * (rect.bottom - rect.top) as i64;
    if area > 10_000 {
        // SAFETY: EnumWindows passes the pointer to a live isize local as lparam.
        unsafe {
            *(lparam.0 as *mut isize) = hwnd.0 as isize;
        }
        return BOOL(0);
    }
    BOOL(1)
}

unsafe extern "system" fn paint_subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    if message == WM_GETMINMAXINFO {
        unsafe {
            // SAFETY: Windows passes a writable MINMAXINFO for this message.
            let info = &mut *(lparam.0 as *mut MINMAXINFO);
            info.ptMinTrackSize = POINT { x: 560, y: 520 };
            info.ptMaxTrackSize = POINT {
                x: GetSystemMetrics(SM_CXSCREEN),
                y: GetSystemMetrics(SM_CYSCREEN),
            };
        }
    }
    if message == WM_WINDOWPOSCHANGING {
        unsafe {
            // SAFETY: Windows passes a writable WINDOWPOS for this message.
            let pos = &mut *(lparam.0 as *mut WINDOWPOS);
            if pos.flags.0 & SWP_NOSIZE.0 == 0 {
                pos.cx = pos.cx.clamp(560, GetSystemMetrics(SM_CXSCREEN));
                pos.cy = pos.cy.clamp(520, GetSystemMetrics(SM_CYSCREEN));
            }
        }
    }
    // SAFETY: The subclass chain and all message arguments come from Windows.
    let result = unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
    let restored = message == WM_SIZE && wparam.0 as u32 == SIZE_RESTORED;
    let shown = message == WM_SHOWWINDOW && wparam.0 != 0;
    if restored || shown || message == WM_ACTIVATE {
        if restored || shown {
            crate::logging::info(format!("paint nudge message {message}"));
        }
        if let Some(ui) = UI_WEAK
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|weak| weak.upgrade())
        {
            if message == WM_SIZE {
                let mut rect = Default::default();
                if unsafe {
                    // SAFETY: The RECT is a writable local for this window.
                    GetClientRect(hwnd, &mut rect)
                }
                .is_ok()
                {
                    let width = (rect.right - rect.left).max(0) as u32;
                    let height = (rect.bottom - rect.top).max(0) as u32;
                    let current = ui.window().size();
                    if width > 0
                        && height > 0
                        && (current.width != width || current.height != height)
                    {
                        crate::logging::info(format!(
                            "resize adapter {}x{} -> {width}x{height}",
                            current.width, current.height
                        ));
                        ui.window()
                            .set_size(slint::PhysicalSize::new(width, height));
                    }
                }
            }
            ui.window().request_redraw();
        }
        unsafe {
            // SAFETY: The handle is the subclassed Slint window.
            let _ = RedrawWindow(
                hwnd,
                None,
                HRGN::default(),
                RDW_INVALIDATE | RDW_UPDATENOW | RDW_ALLCHILDREN,
            );
            // SAFETY: DwmFlush only waits for the compositor; it has no pointers.
            let _ = DwmFlush();
        }
    }
    result
}

pub fn take_requests() -> Vec<UiRequest> {
    std::mem::take(&mut *UI_REQUESTS.lock().unwrap_or_else(|e| e.into_inner()))
}

fn post_request(request: UiRequest) {
    UI_REQUESTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(request);
    let raw = UI_HWND.load(Ordering::Relaxed);
    if raw != 0 {
        unsafe {
            // SAFETY: The handle was stored by the tray window and is only read
            // here; posting a message to a window of this process is safe.
            PostMessageW(
                HWND(raw as *mut std::ffi::c_void),
                WM_UI,
                WPARAM(0),
                LPARAM(0),
            )
            .ok();
        }
    }
}

pub fn visible() -> bool {
    UI_VISIBLE.load(Ordering::Relaxed)
}

pub fn show(controls: &Controls) {
    if visible() {
        activate();
        return;
    }
    let tx = UI_TX.get_or_init(|| {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("CaptureButton-ui".into())
            .spawn(move || ui_thread(rx))
            .expect("start the UI thread");
        tx
    });
    UI_VISIBLE.store(true, Ordering::Relaxed);
    tx.send(UiCommand::Show(controls.clone())).ok();
    wake_ui_thread();
}

/// The UI thread parks in `WaitMessage`, not in `recv`, so posting a command
/// needs a nudge. The message itself carries nothing.
fn wake_ui_thread() {
    let thread = UI_TID.load(Ordering::Relaxed);
    if thread != 0 {
        unsafe {
            // SAFETY: The id belongs to this process's UI thread, and a posted
            // message carries no pointer.
            let _ = PostThreadMessageW(thread, WM_UI_WAKE, WPARAM(0), LPARAM(0));
        }
    }
}

/// Rebuild every translated string on the live window. The language catalog is
/// reloaded by the caller first; this only pushes the new labels into Slint.
pub fn relabel(controls: &Controls) {
    let weak = UI_WEAK.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let Some(weak) = weak else {
        return;
    };
    let controls = controls.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = weak.upgrade() {
            sync_labels(&ui);
            refresh_now(&ui, &controls);
        }
    });
}

pub fn hide() {
    if !visible() {
        return;
    }
    UI_VISIBLE.store(false, Ordering::Relaxed);
    let weak = UI_WEAK.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(weak) = weak {
        slint::invoke_from_event_loop(move || {
            if let Some(ui) = weak.upgrade() {
                let _ = ui.hide();
            }
            let _ = slint::quit_event_loop();
        })
        .ok();
    }
}

fn activate() {
    let weak = UI_WEAK.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(weak) = weak {
        slint::invoke_from_event_loop(move || {
            if let Some(ui) = weak.upgrade() {
                let _ = ui.show();
            }
        })
        .ok();
    }
}

fn ui_thread(rx: mpsc::Receiver<UiCommand>) {
    UI_TID.store(
        unsafe {
            // SAFETY: GetCurrentThreadId has no preconditions.
            GetCurrentThreadId()
        },
        Ordering::Relaxed,
    );
    loop {
        let Some(UiCommand::Show(controls)) = next_command(&rx) else {
            return;
        };
        let ui = match MainWindow::new() {
            Ok(ui) => ui,
            Err(error) => {
                UI_VISIBLE.store(false, Ordering::Relaxed);
                eprintln!("Capture Button: could not create the window: {error}");
                continue;
            }
        };
        bind(&ui, &controls);
        sync_labels(&ui);
        *UI_WEAK.lock().unwrap_or_else(|e| e.into_inner()) = Some(ui.as_weak());
        refresh_now(&ui, &controls);
        let weak = ui.as_weak();
        ui.window().on_close_requested(move || {
            if let Some(ui) = weak.upgrade() {
                let _ = ui.hide();
            }
            let _ = slint::quit_event_loop();
            UI_VISIBLE.store(false, Ordering::Relaxed);
            post_request(UiRequest::CloseRequested);
            slint::CloseRequestResponse::HideWindow
        });
        UI_VISIBLE.store(true, Ordering::Relaxed);
        let _ = ui.show();
        let factor = ui.window().scale_factor();
        let size = ui.get_ui_scale();
        ui.window().set_size(slint::PhysicalSize::new(
            (760.0 * size * factor) as u32,
            (700.0 * size * factor) as u32,
        ));
        let width = ui.window().size().width as i32;
        let screen = unsafe {
            // SAFETY: GetSystemMetrics only reads a process-wide integer metric.
            GetSystemMetrics(SM_CXSCREEN)
        };
        let x = ((screen - width).max(0) / 2).max(0);
        ui.window()
            .set_position(slint::PhysicalPosition::new(x, 40));
        let guard_timer = slint::Timer::default();
        guard_timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_millis(50),
            install_paint_guard,
        );
        let result = ui.run();
        UI_VISIBLE.store(false, Ordering::Relaxed);
        *UI_WEAK.lock().unwrap_or_else(|e| e.into_inner()) = None;
        if let Err(error) = result {
            eprintln!("Capture Button: UI loop stopped: {error}");
        }
    }
}

/// Waits for the next `Show` command without blocking this thread's Win32
/// queue. Slint keeps the winit event loop, and with it winit's small hidden
/// thread-event window, alive after `ui.run()` returns. If the thread then sat
/// in `recv`, nothing would answer that window, Windows would flag it as hung
/// and DWM would draw its "ghost" as a black square in the screen corner.
fn next_command(rx: &mpsc::Receiver<UiCommand>) -> Option<UiCommand> {
    loop {
        match rx.try_recv() {
            Ok(command) => return Some(command),
            Err(TryRecvError::Disconnected) => return None,
            Err(TryRecvError::Empty) => {}
        }
        unsafe {
            // SAFETY: WaitMessage waits on this thread's own queue; the messages
            // drained below are only the ones already addressed to it.
            let _ = WaitMessage();
            let mut message = MSG::default();
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }
}

fn sync_labels(ui: &MainWindow) {
    let labels = ui.global::<Labels>();
    labels.set_idle_state(tr("Idle").into());
    labels.set_recording_state(tr("Recording").into());
    labels.set_saving_state(tr("Saving").into());
    labels.set_capture(tr("Capture").into());
    labels.set_settings(tr("Settings").into());
    labels.set_refresh(tr("Refresh").into());
    labels.set_about(tr("About").into());
    labels.set_hero_note(tr("Screen, region and audio, ready when you are.").into());
    labels.set_screenshot(tr("Screenshot").into());
    labels.set_record_screen(tr("Record screen").into());
    labels.set_format(tr("Format").into());
    labels.set_saved_next(tr("Saved next to the executable").into());
    labels.set_picture(tr("Picture").into());
    labels.set_preset(tr("Preset").into());
    labels.set_resolution(tr("Resolution").into());
    labels.set_frame_rate(tr("Frame rate").into());
    labels.set_audio(tr("Audio").into());
    labels.set_audio_capture(tr("Audio capture").into());
    labels.set_camera(tr("Camera").into());
    labels.set_camera_photo(tr("Camera photo").into());
    labels.set_camera_video(tr("Camera video").into());
    labels.set_camera_choose(tr("Choose camera").into());
    labels.set_system_audio(tr("System audio").into());
    labels.set_microphone(tr("Microphone").into());
    labels.set_show_cursor(tr("Show the mouse cursor").into());
    labels.set_quick_actions(tr("Quick actions").into());
    labels.set_area_capture(tr("Area capture").into());
    labels.set_choose_source(tr("Choose source").into());
    labels.set_capture_audio(tr("Capture audio").into());
    labels.set_screenshot_screen(tr("Screenshot screen").into());
    labels.set_screenshot_area(tr("Screenshot area").into());
    labels.set_recent(tr("Recent").into());
    labels.set_no_captures(tr("No captures yet. The last twelve files will appear here.").into());
    labels.set_folder(tr("Folder").into());
    labels.set_open(tr("Open").into());
    labels.set_preferences(tr("Preferences").into());
    labels.set_start_with_windows(tr("Start with Windows").into());
    labels.set_start_minimized(tr("Start minimized with Windows").into());
    labels.set_hide_active(tr("Hide the window while capturing").into());
    labels.set_close_tray(tr("Close button hides to the tray").into());
    labels.set_clipboard(tr("Copy the saved file to the clipboard").into());
    labels.set_tray_capture(tr("Tray left-click starts area selection").into());
    labels.set_highlight(tr("Show a red frame around the area").into());
    labels.set_completion_sound(tr("Completion sound").into());
    labels.set_sounds(tr("Sounds").into());
    labels.set_volume(tr("Volume").into());
    labels.set_play_on_save(tr("Play when a recording is saved").into());
    labels.set_play_on_shot(tr("Play for screenshots and photos").into());
    labels.set_no_signal(tr("Keep recording when the signal is lost").into());
    labels.set_show_card(tr("Show color bars instead of the frame").into());
    labels.set_tray_area(tr("Tray area").into());
    labels.set_tray_video(tr("Tray video").into());
    labels.set_tray_audio(tr("Tray audio").into());
    labels.set_performance(tr("Performance").into());
    labels.set_encoder_threads(tr("Encoder threads").into());
    labels.set_interface_scale(tr("Interface scale").into());
    labels.set_language(tr("Language").into());
    labels.set_quality(tr("Quality").into());
    labels.set_balance(tr("Balance").into());
    labels.set_speed(tr("Speed").into());
    labels.set_custom(tr("Custom").into());
    labels.set_auto(tr("Auto").into());
    labels.set_copy(tr("Copy").into());
    labels.set_copied(tr("Copied").into());
    labels.set_screenshot_source(tr("Screenshot source").into());
    labels.set_screenshots(tr("Screenshots").into());
    labels.set_video_recording(tr("Video recording").into());
    labels.set_close(tr("Close").into());
    labels.set_about_note(tr("Capture Button records the screen, a region or sound and keeps the last twelve files in the folder below.").into());
    labels.set_licenses(tr("Licenses").into());
    labels.set_license_text(tr("FFmpeg and x264 are distributed under the GNU GPL v2 or later. Icons from Phosphor Icons are MIT licensed. Other library and crate notices are in NOTICE and THIRD-PARTY.txt next to the program.").into());
    labels.set_github("GitHub".into());
    labels.set_open_recordings(tr("Open recordings").into());
    labels.set_open_logs(tr("Open logs").into());
    labels.set_about_version(format!("Capture Button {}", env!("CARGO_PKG_VERSION")).into());
    let output = media::output_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|error| error);
    labels.set_output_path(trf("Folder: {path}", &[("path", &output)]).into());
}

fn bind(ui: &MainWindow, controls: &Controls) {
    ui.on_act(move |id| {
        post_request(UiRequest::Command {
            id,
            notification: 0,
        })
    });

    let toggle = controls.clone();
    ui.on_toggle(move |id| {
        toggle.toggle(id);
        post_request(UiRequest::Command {
            id,
            notification: 0,
        });
    });

    let choose = controls.clone();
    ui.on_choose(move |id, value| {
        if let Some(index) = choose.index_for_label(id, value.as_str()) {
            choose.select(id, index);
            post_request(UiRequest::Command {
                id,
                notification: 1,
            });
        }
    });

    let slide = controls.clone();
    ui.on_slide(move |id, value, persist| {
        slide.set_slider(id, value as u32);
        post_request(UiRequest::Slider {
            id,
            value: value as u32,
            persist,
        });
    });

    let history = controls.clone();
    ui.on_history_click(move |id| {
        post_request(UiRequest::Command {
            id,
            notification: 0,
        })
    });
    let _ = history;
}

fn refresh_now(ui: &MainWindow, controls: &Controls) {
    let model = controls.model.lock().unwrap_or_else(|e| e.into_inner());
    sync(ui, &model);
}

fn refresh(controls: &Controls) {
    if !visible() {
        return;
    }
    let weak = UI_WEAK.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(weak) = weak {
        let controls = controls.clone();
        slint::invoke_from_event_loop(move || {
            if let Some(ui) = weak.upgrade() {
                if let Ok(model) = controls.model.lock() {
                    sync(&ui, &model);
                }
            }
        })
        .ok();
    }
}

fn sync(ui: &MainWindow, model: &Model) {
    ui.set_page(model.page as i32);
    ui.set_format(model.format as i32);
    ui.set_preset(model.preset as i32);
    ui.set_resolution(model.resolution as i32);
    ui.set_fps(model.fps[0] as i32);
    ui.set_area_format(model.area_format as i32);
    ui.set_area_preset(model.area_preset as i32);
    ui.set_area_resolution(model.area_resolution as i32);
    ui.set_area_fps(model.fps[1] as i32);
    ui.set_video_preset(model.video_preset as i32);
    ui.set_video_resolution(model.video_resolution as i32);
    ui.set_video_fps(model.fps[2] as i32);
    ui.set_threads(model.threads as i32);
    ui.set_scale(model.scale_index as i32);
    ui.set_language(model.language as i32);
    ui.set_system_audio(model.checked(ID_SYSTEM_AUDIO));
    ui.set_microphone(model.checked(ID_MIC));
    ui.set_cursor(model.checked(ID_CURSOR));
    ui.set_area_system(model.checked(166));
    ui.set_area_mic(model.checked(167));
    ui.set_area_cursor(model.checked(ID_TRAY_AREA_CURSOR));
    ui.set_video_system(model.checked(176));
    ui.set_video_mic(model.checked(177));
    ui.set_video_cursor(model.checked(ID_TRAY_VIDEO_CURSOR));
    ui.set_audio_system(model.checked(ID_TRAY_AUDIO_SYSTEM));
    ui.set_audio_mic(model.checked(ID_TRAY_AUDIO_MIC));
    ui.set_gain(model.gains[0] as i32);
    ui.set_area_gain(model.gains[1] as i32);
    ui.set_video_gain(model.gains[2] as i32);
    ui.set_audio_gain(model.gains[3] as i32);
    ui.set_autostart(model.checked(ID_AUTOSTART));
    ui.set_start_minimized(model.checked(ID_START_MINIMIZED));
    ui.set_hide_active(model.checked(ID_HIDE_ACTIVE));
    ui.set_close_tray(model.checked(ID_CLOSE_TRAY));
    ui.set_clipboard(model.checked(ID_CLIPBOARD));
    ui.set_tray_capture(model.checked(ID_TRAY_CAPTURE));
    ui.set_highlight(model.checked(ID_HIGHLIGHT));
    ui.set_sound(model.checked(ID_NOTIFY));
    ui.set_sound_volume(model.sound_volume as i32);
    ui.set_shot_sound(model.checked(ID_SCREENSHOT_SOUND));
    ui.set_no_signal(model.checked(ID_NO_SIGNAL));
    ui.set_show_card(model.checked(ID_CARD));
    ui.set_capture_device(model.device_index[0] as i32);
    ui.set_area_device(model.device_index[1] as i32);
    ui.set_video_device(model.device_index[2] as i32);
    ui.set_audio_device(model.device_index[3] as i32);
    ui.set_status(model.status.clone().into());
    ui.set_status_saved(model.status_saved);
    ui.set_stop_label(model.stop_label.clone().into());
    ui.set_idle(model.idle);
    ui.set_recording(model.recording);
    ui.set_saving(model.saving);
    ui.set_about_open(model.about_open);
    let (update_label, update_enabled, update_primary) = match model.update_state {
        UpdateState::Idle | UpdateState::Checking => (tr("Update"), false, false),
        UpdateState::Available => (tr("Update"), true, true),
        UpdateState::Downloading(percent) => (
            trf(
                "Downloading {percent}%",
                &[("percent", &percent.to_string())],
            ),
            false,
            true,
        ),
        UpdateState::Installing => (tr("Installing…"), false, true),
        UpdateState::UpToDate | UpdateState::Error | UpdateState::Unsupported => {
            (tr("Update"), false, false)
        }
    };
    ui.set_update_label(update_label.into());
    ui.set_update_enabled(update_enabled);
    ui.set_update_primary(update_primary);
    ui.set_source_open(model.source_open);
    ui.set_camera_open(model.camera_open);
    if model.camera_open {
        ui.set_camera_items(slint::ModelRc::new(slint::VecModel::from(
            model
                .camera_items
                .iter()
                .cloned()
                .map(slint::SharedString::from)
                .collect::<Vec<_>>(),
        )));
    }
    ui.set_source_monitors(model.source_monitors as i32);
    ui.set_history_ack(model.history_ack.unwrap_or(-1));
    ui.set_source_monitor_items(slint::ModelRc::new(slint::VecModel::from(
        model
            .source_monitor_items
            .iter()
            .cloned()
            .map(slint::SharedString::from)
            .collect::<Vec<_>>(),
    )));
    ui.set_source_window_items(slint::ModelRc::new(slint::VecModel::from(
        model
            .source_window_items
            .iter()
            .cloned()
            .map(slint::SharedString::from)
            .collect::<Vec<_>>(),
    )));
    ui.set_about_recordings(
        trf(
            "Recordings: {path}",
            &[(
                "path",
                &media::output_dir()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|error| error),
            )],
        )
        .into(),
    );
    ui.set_about_logs(
        trf(
            "Logs: {path}",
            &[("path", &crate::logging::dir().display().to_string())],
        )
        .into(),
    );

    let mut names = vec![slint::SharedString::from(tr("System"))];
    names.extend(
        crate::i18n::available()
            .into_iter()
            .map(|(_, name)| slint::SharedString::from(name)),
    );
    ui.set_language_names(slint::ModelRc::new(slint::VecModel::from(names)));

    ui.set_capture_devices(slint::ModelRc::new(slint::VecModel::from(device_names(
        &model.devices[0],
    ))));
    ui.set_area_devices(slint::ModelRc::new(slint::VecModel::from(device_names(
        &model.devices[1],
    ))));
    ui.set_video_devices(slint::ModelRc::new(slint::VecModel::from(device_names(
        &model.devices[2],
    ))));
    ui.set_audio_devices(slint::ModelRc::new(slint::VecModel::from(device_names(
        &model.devices[3],
    ))));
    ui.set_history(slint::ModelRc::new(slint::VecModel::from(
        model
            .history
            .iter()
            .cloned()
            .map(slint::SharedString::from)
            .collect::<Vec<_>>(),
    )));
    ui.set_ui_scale(1.0);
}

fn device_names(devices: &[media::AudioDevice]) -> Vec<slint::SharedString> {
    let mut names = vec![slint::SharedString::from(tr("Auto"))];
    names.extend(
        devices
            .iter()
            .map(|device| slint::SharedString::from(device.name.clone())),
    );
    names
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UpdateState {
    #[default]
    Idle,
    Checking,
    Available,
    Downloading(u8),
    Installing,
    UpToDate,
    Error,
    Unsupported,
}

#[derive(Default)]
struct Model {
    page: usize,
    format: usize,
    area_format: usize,
    video_format: usize,
    preset: usize,
    resolution: usize,
    area_preset: usize,
    area_resolution: usize,
    video_preset: usize,
    video_resolution: usize,
    fps: [u32; 3],
    checks: HashMap<i32, bool>,
    threads: usize,
    scale_index: usize,
    language: usize,
    sound_volume: u32,
    gains: [u32; 4],
    devices: [Vec<media::AudioDevice>; 4],
    device_index: [usize; 4],
    history: Vec<String>,
    history_ack: Option<i32>,
    status: String,
    status_saved: bool,
    stop_label: String,
    idle: bool,
    recording: bool,
    saving: bool,
    scale_percent: u32,
    scale_setting: u32,
    about_open: bool,
    update_state: UpdateState,
    source_open: bool,
    camera_open: bool,
    camera_items: Vec<String>,
    source_monitor_items: Vec<String>,
    source_window_items: Vec<String>,
    source_monitors: usize,
}

#[derive(Clone)]
pub struct Controls {
    model: Arc<Mutex<Model>>,
}

impl Controls {
    pub fn new() -> Self {
        Self {
            model: Arc::new(Mutex::new(Model::default())),
        }
    }

    pub fn build(&mut self, settings: &mut Settings) {
        self.load_ui(settings);
        self.update(|model| {
            model.status = String::new();
            model.status_saved = false;
            model.stop_label = tr("Stop capture");
            model.idle = true;
            model.recording = false;
        });
    }

    fn update(&self, f: impl FnOnce(&mut Model)) {
        {
            let mut model = self.model.lock().unwrap_or_else(|e| e.into_inner());
            f(&mut model);
        }
        refresh(self);
    }

    pub fn page(&self) -> usize {
        self.model.lock().unwrap_or_else(|e| e.into_inner()).page
    }

    pub fn scale_percent(&self) -> u32 {
        self.model
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .scale_percent
    }

    pub fn set_format_index(&self, index: usize) {
        self.update(|model| model.format = index);
    }

    pub fn set_area_format_index(&self, index: usize) {
        self.update(|model| model.area_format = index);
    }

    pub fn set_video_format_index(&self, index: usize) {
        self.update(|model| model.video_format = index);
    }

    pub fn show_page(&mut self, page: usize) {
        self.update(|model| model.page = page);
    }

    pub fn show_about(&self, open: bool) {
        self.update(|model| model.about_open = open);
    }

    pub fn set_update_state(&self, state: UpdateState) {
        self.update(|model| model.update_state = state);
    }

    pub fn show_sources(&self, items: Vec<String>, monitors: usize) {
        self.update(|model| {
            let mut items = items;
            let windows = items.split_off(monitors.min(items.len()));
            model.source_monitor_items = items;
            model.source_window_items = windows;
            model.source_monitors = monitors;
            model.source_open =
                !model.source_monitor_items.is_empty() || !model.source_window_items.is_empty();
        });
    }

    pub fn close_sources(&self) {
        self.update(|model| model.source_open = false);
    }

    pub fn show_cameras(&self, items: Vec<String>) {
        self.update(|model| {
            model.camera_items = items;
            model.camera_open = !model.camera_items.is_empty();
        });
    }

    pub fn close_cameras(&self) {
        self.update(|model| model.camera_open = false);
    }

    pub fn set_idle(&self, idle: bool, recording: bool) {
        self.update(|model| {
            model.idle = idle;
            model.recording = recording;
        });
    }

    pub fn set_saving(&self, saving: bool) {
        self.update(|model| model.saving = saving);
    }

    pub fn set_text(&self, id: i32, text: &str) {
        self.update(|model| match id {
            ID_STOP => model.stop_label = text.to_string(),
            ID_STATUS => {
                model.status = text.to_string();
                model.status_saved = text.starts_with(tr("Saved:").as_str());
            }
            _ => {}
        });
    }

    pub fn set_history(&self, lines: &[String]) {
        self.update(|model| {
            model.history = lines.iter().take(HISTORY_COUNT as usize).cloned().collect()
        });
    }

    pub fn acknowledge_copy(&self, id: i32) {
        self.update(|model| model.history_ack = Some(id));
        let controls = self.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(800));
            controls.clear_history_ack();
        });
    }

    pub fn clear_history_ack(&self) {
        self.update(|model| model.history_ack = None);
    }

    pub fn check(&mut self, id: i32, checked: bool) {
        self.update(|model| {
            model.checks.insert(id, checked);
        });
    }

    pub fn checked(&self, id: i32) -> bool {
        self.model
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .checks
            .get(&id)
            .copied()
            .unwrap_or(false)
    }

    pub fn toggle(&self, id: i32) {
        let checked = !self.checked(id);
        self.update(|model| {
            model.checks.insert(id, checked);
        });
    }

    pub fn select(&self, id: i32, index: usize) {
        self.update(|model| match id {
            ID_PRESET => model.preset = index,
            ID_RESOLUTION => model.resolution = index,
            ID_TRAY_AREA_PRESET => model.area_preset = index,
            163 => model.area_resolution = index,
            ID_TRAY_VIDEO_PRESET => model.video_preset = index,
            173 => model.video_resolution = index,
            ID_THREADS => model.threads = index,
            ID_SCALE => model.scale_index = index,
            ID_LANGUAGE => model.language = index,
            ID_MIC_DEVICE => model.device_index[0] = index,
            ID_TRAY_AREA_DEVICE => model.device_index[1] = index,
            ID_TRAY_VIDEO_DEVICE => model.device_index[2] = index,
            ID_TRAY_AUDIO_DEVICE => model.device_index[3] = index,
            _ => {}
        });
    }

    pub fn selected(&self, id: i32) -> usize {
        let model = self.model.lock().unwrap_or_else(|e| e.into_inner());
        match id {
            ID_PRESET => model.preset,
            ID_RESOLUTION => model.resolution,
            ID_TRAY_AREA_PRESET => model.area_preset,
            163 => model.area_resolution,
            ID_TRAY_VIDEO_PRESET => model.video_preset,
            173 => model.video_resolution,
            ID_THREADS => model.threads,
            ID_SCALE => model.scale_index,
            ID_LANGUAGE => model.language,
            ID_MIC_DEVICE => model.device_index[0],
            ID_TRAY_AREA_DEVICE => model.device_index[1],
            ID_TRAY_VIDEO_DEVICE => model.device_index[2],
            ID_TRAY_AUDIO_DEVICE => model.device_index[3],
            _ => 0,
        }
    }

    fn index_for_label(&self, id: i32, label: &str) -> Option<usize> {
        match id {
            ID_PRESET | ID_TRAY_AREA_PRESET | ID_TRAY_VIDEO_PRESET => {
                ["Quality", "Balance", "Speed", "Custom"]
                    .iter()
                    .position(|item| tr(item) == label)
            }
            ID_RESOLUTION | 163 | 173 => ["2160p", "1440p", "1080p", "720p", "480p", "360p"]
                .iter()
                .position(|item| *item == label),
            ID_THREADS => ["Auto", "1", "2", "4"]
                .iter()
                .position(|item| tr(item) == label),
            ID_SCALE => [
                "Auto", "50%", "75%", "100%", "125%", "150%", "175%", "200%", "225%", "250%",
                "300%",
            ]
            .iter()
            .position(|item| tr(item) == label),
            ID_LANGUAGE => {
                let names = crate::i18n::available();
                if label == tr("System") {
                    Some(0)
                } else {
                    names
                        .iter()
                        .position(|(_, name)| name == label)
                        .map(|index| index + 1)
                }
            }
            ID_MIC_DEVICE | ID_TRAY_AREA_DEVICE | ID_TRAY_VIDEO_DEVICE | ID_TRAY_AUDIO_DEVICE => {
                let slot = device_slot(id);
                let model = self.model.lock().unwrap_or_else(|e| e.into_inner());
                if label == tr("Auto") {
                    Some(0)
                } else {
                    model.devices[slot]
                        .iter()
                        .position(|device| device.name == label)
                        .map(|index| index + 1)
                }
            }
            _ => None,
        }
    }

    pub fn set_slider(&self, id: i32, value: u32) {
        self.update(|model| match id {
            ID_FPS => model.fps[0] = value.clamp(5, 60),
            164 => model.fps[1] = value.clamp(5, 60),
            174 => model.fps[2] = value.clamp(5, 60),
            ID_SOUND_VOLUME => model.sound_volume = value.min(100),
            ID_MIC_GAIN => model.gains[0] = value.min(MIC_GAIN_MAX),
            ID_TRAY_AREA_GAIN => model.gains[1] = value.min(MIC_GAIN_MAX),
            ID_TRAY_VIDEO_GAIN => model.gains[2] = value.min(MIC_GAIN_MAX),
            ID_TRAY_AUDIO_GAIN => model.gains[3] = value.min(MIC_GAIN_MAX),
            _ => {}
        });
    }

    pub fn set_microphone_gain(&mut self, id: i32, gain: u32) {
        self.set_slider(id, gain);
    }

    pub fn microphone_gain(&self, id: i32) -> u32 {
        let model = self.model.lock().unwrap_or_else(|e| e.into_inner());
        match id {
            ID_MIC_GAIN => model.gains[0],
            ID_TRAY_AREA_GAIN => model.gains[1],
            ID_TRAY_VIDEO_GAIN => model.gains[2],
            ID_TRAY_AUDIO_GAIN => model.gains[3],
            _ => 0,
        }
    }

    pub fn refresh_formats(&self) {}

    pub fn load_ui(&mut self, settings: &Settings) {
        self.update(|model| {
            model.format = settings.ui.format.index();
            model.area_format = settings.tray_area.format.index();
            model.video_format = Format::Mkv.index();
            model.load_main_video(&settings.ui);
            model.load_tray(settings, true);
            model.load_tray(settings, false);
            let devices = media::microphones();
            for slot in 0..4 {
                model.devices[slot] = devices.clone();
            }
            model.load_audio(0, &settings.ui.audio);
            model.load_audio(1, &settings.tray_area.audio);
            model.load_audio(2, &settings.tray_video.audio);
            model.load_audio(3, &settings.tray_audio);
            for (id, value) in [
                (ID_AUTOSTART, settings.autostart),
                (ID_START_MINIMIZED, settings.start_minimized),
                (ID_HIDE_ACTIVE, settings.hide_active),
                (ID_CLOSE_TRAY, settings.close_tray),
                (ID_CLIPBOARD, settings.clipboard),
                (ID_TRAY_CAPTURE, settings.tray_capture),
                (ID_HIGHLIGHT, settings.highlight_region),
                (ID_NOTIFY, settings.completion_sound),
                (ID_SCREENSHOT_SOUND, settings.screenshot_sound),
                (ID_NO_SIGNAL, settings.show_no_signal),
                (ID_CARD, settings.show_card),
                (ID_CURSOR, settings.ui.cursor),
                (ID_TRAY_AREA_CURSOR, settings.tray_area.cursor),
                (ID_TRAY_VIDEO_CURSOR, settings.tray_video.cursor),
            ] {
                model.checks.insert(id, value);
            }
            model.threads = match settings.threads {
                1 => 1,
                2 => 2,
                4 => 3,
                _ => 0,
            };
            model.scale_index = scale_index(settings.scale);
            model.language = language_index(&settings.ui_language);
            model.sound_volume = settings.sound_volume.min(100);
            model.apply_scale(settings);
        });
    }

    pub fn load_main_video(&mut self, settings: &CaptureSettings) {
        self.update(|model| model.load_main_video(settings));
    }

    pub fn load_tray(&mut self, settings: &Settings, area: bool) {
        self.update(|model| model.load_tray(settings, area));
    }

    pub fn read_ui(&mut self, settings: &mut Settings, persist: bool) {
        {
            let model = self.model.lock().unwrap_or_else(|e| e.into_inner());
            model.read_ui(settings, persist);
        }
        refresh(self);
    }

    pub fn refresh_devices(&mut self, id: i32) {
        let devices = media::microphones();
        self.update(|model| {
            let slot = device_slot(id);
            let selected_id = model.devices[slot]
                .get(model.device_index[slot].saturating_sub(1))
                .map(|device| device.id.clone());
            model.devices[slot] = devices;
            model.device_index[slot] = selected_id
                .and_then(|id| {
                    model.devices[slot]
                        .iter()
                        .position(|device| device.id == id)
                        .map(|index| index + 1)
                })
                .unwrap_or(0);
        });
    }

    pub fn refresh_all_devices(&mut self, settings: &mut Settings) {
        for id in [
            ID_MIC_DEVICE,
            ID_TRAY_AREA_DEVICE,
            ID_TRAY_VIDEO_DEVICE,
            ID_TRAY_AUDIO_DEVICE,
        ] {
            self.refresh_devices(id);
        }
        self.read_ui(settings, true);
    }

    pub fn apply_preset(&mut self, settings: &mut Settings) {
        let format = Format::from_index(self.selected_format());
        if self.selected(ID_PRESET) == 3 {
            let video = settings.ui.video[format.index()];
            self.select(ID_PRESET, preset_index(format, video));
            return;
        }
        let value = preset_value(format, self.selected(ID_PRESET));
        settings.ui.video[format.index()] = value;
        self.load_main_video(&settings.ui);
        self.read_ui(settings, true);
    }

    pub fn apply_tray_preset(&mut self, settings: &mut Settings, area: bool) {
        let format_index = if area {
            self.selected_area_format()
        } else {
            Format::Mkv.index()
        };
        let format = Format::from_index(format_index);
        let base = if area {
            ID_TRAY_AREA_PRESET
        } else {
            ID_TRAY_VIDEO_PRESET
        };
        if self.selected(base) == 3 {
            let current = if area {
                settings.tray_area.video[format_index]
            } else {
                settings.tray_video.video[format_index]
            };
            self.select(base, preset_index(format, current));
            return;
        }
        let value = preset_value(format, self.selected(base));
        if area {
            settings.tray_area.video[format_index] = value;
        } else {
            settings.tray_video.video[format_index] = value;
        }
        self.load_tray(settings, area);
        self.read_ui(settings, true);
    }

    pub fn set_interface_scale(&mut self, settings: &mut Settings, scale: u32) {
        settings.scale = scale;
        self.select(ID_SCALE, scale_index(scale));
        self.update(|model| model.apply_scale(settings));
        settings.save();
    }

    pub fn apply_scale(&mut self, settings: &Settings) {
        self.update(|model| model.apply_scale(settings));
    }

    pub fn selected_format(&self) -> usize {
        self.model.lock().unwrap_or_else(|e| e.into_inner()).format
    }

    pub fn selected_area_format(&self) -> usize {
        self.model
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .area_format
    }

    pub fn selected_video_format(&self) -> usize {
        self.model
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .video_format
    }
}

fn device_slot(id: i32) -> usize {
    match id {
        ID_TRAY_AREA_DEVICE => 1,
        ID_TRAY_VIDEO_DEVICE => 2,
        ID_TRAY_AUDIO_DEVICE => 3,
        _ => 0,
    }
}

impl Model {
    fn checked(&self, id: i32) -> bool {
        self.checks.get(&id).copied().unwrap_or(false)
    }

    fn apply_scale(&mut self, settings: &Settings) {
        let percent = interface_scale(settings.scale);
        self.scale_percent = percent;
        self.scale_setting = settings.scale;
    }

    fn load_main_video(&mut self, settings: &CaptureSettings) {
        let video = settings.video[settings.format.index()];
        self.resolution = resolution_index(video.height);
        self.fps[0] = video.fps;
        self.preset = preset_index(settings.format, video);
    }

    fn load_tray(&mut self, settings: &Settings, area: bool) {
        let profile = if area {
            &settings.tray_area
        } else {
            &settings.tray_video
        };
        let format_index = if area {
            profile.format.index()
        } else {
            Format::Mkv.index()
        };
        let video = profile.video[format_index];
        if area {
            self.area_format = format_index;
            self.area_resolution = resolution_index(video.height);
            self.fps[1] = video.fps;
            self.area_preset = preset_index(profile.format, video);
        } else {
            self.video_format = Format::Mkv.index();
            self.video_resolution = resolution_index(video.height);
            self.fps[2] = video.fps;
            self.video_preset = preset_index(Format::Mkv, video);
        }
    }

    fn load_audio(&mut self, slot: usize, audio: &AudioSettings) {
        self.checks.insert(
            match slot {
                0 => ID_SYSTEM_AUDIO,
                1 => 166,
                2 => 176,
                _ => ID_TRAY_AUDIO_SYSTEM,
            },
            audio.system,
        );
        self.checks.insert(
            match slot {
                0 => ID_MIC,
                1 => 167,
                2 => 177,
                _ => ID_TRAY_AUDIO_MIC,
            },
            audio.microphone,
        );
        self.gains[slot] = audio.gain.min(MIC_GAIN_MAX);
        self.device_index[slot] = if audio.device == "Auto" {
            0
        } else {
            self.devices[slot]
                .iter()
                .position(|device| device.id == audio.device)
                .map(|index| index + 1)
                .unwrap_or(0)
        };
    }

    fn audio_from(&self, slot: usize, previous: &AudioSettings) -> AudioSettings {
        let selected = self.device_index[slot];
        let device = if selected == 0 {
            "Auto".to_string()
        } else {
            self.devices[slot]
                .get(selected - 1)
                .map(|device| device.id.clone())
                .unwrap_or_else(|| previous.device.clone())
        };
        AudioSettings {
            system: self.checked(match slot {
                0 => ID_SYSTEM_AUDIO,
                1 => 166,
                2 => 176,
                _ => ID_TRAY_AUDIO_SYSTEM,
            }),
            microphone: self.checked(match slot {
                0 => ID_MIC,
                1 => 167,
                2 => 177,
                _ => ID_TRAY_AUDIO_MIC,
            }),
            device,
            gain: previous.gain,
        }
    }

    fn read_tray(&self, settings: &Settings, area: bool) -> CaptureSettings {
        let previous = if area {
            &settings.tray_area
        } else {
            &settings.tray_video
        };
        let format_index = if area {
            self.area_format
        } else {
            Format::Mkv.index()
        };
        let format = Format::from_index(format_index);
        let resolution = if area {
            self.area_resolution
        } else {
            self.video_resolution
        };
        let fps = if area { self.fps[1] } else { self.fps[2] };
        let video = VideoSettings {
            height: resolution_value(resolution),
            fps: fps.clamp(5, 60),
        };
        let mut profile = previous.clone();
        profile.format = format;
        profile.video[format_index] = video;
        profile.audio = self.audio_from(if area { 1 } else { 2 }, &previous.audio);
        profile.cursor = self.checked(if area {
            ID_TRAY_AREA_CURSOR
        } else {
            ID_TRAY_VIDEO_CURSOR
        });
        profile
    }

    fn read_ui(&self, settings: &mut Settings, persist: bool) {
        let format = Format::from_index(self.format);
        settings.ui.format = format;
        settings.ui.video[format.index()] = VideoSettings {
            height: resolution_value(self.resolution),
            fps: self.fps[0].clamp(5, 60),
        };
        settings.ui.audio = self.audio_from(0, &settings.ui.audio);
        settings.ui.cursor = self.checked(ID_CURSOR);
        settings.tray_area = self.read_tray(settings, true);
        settings.tray_video = self.read_tray(settings, false);
        settings.tray_audio = self.audio_from(3, &settings.tray_audio);
        settings.autostart = self.checked(ID_AUTOSTART);
        settings.start_minimized = self.checked(ID_START_MINIMIZED);
        settings.hide_active = self.checked(ID_HIDE_ACTIVE);
        settings.show_no_signal = self.checked(ID_NO_SIGNAL);
        settings.show_card = self.checked(ID_CARD);
        settings.close_tray = self.checked(ID_CLOSE_TRAY);
        settings.clipboard = self.checked(ID_CLIPBOARD);
        settings.tray_capture = self.checked(ID_TRAY_CAPTURE);
        settings.highlight_region = self.checked(ID_HIGHLIGHT);
        settings.completion_sound = self.checked(ID_NOTIFY);
        settings.sound_volume = self.sound_volume.min(100);
        settings.screenshot_sound = self.checked(ID_SCREENSHOT_SOUND);
        settings.threads = match self.threads {
            1 => 1,
            2 => 2,
            3 => 4,
            _ => 0,
        };
        settings.scale = scale_value(self.scale_index);
        settings.ui_language = language_value(self.language);
        if persist {
            settings.save();
        }
    }
}

fn interface_scale(setting: u32) -> u32 {
    crate::geom::interface_scale(setting, media::cursor_monitor_rect().h)
}

fn scale_index(value: u32) -> usize {
    [0, 50, 75, 100, 125, 150, 175, 200, 225, 250, 300]
        .iter()
        .position(|v| *v == value)
        .unwrap_or(0)
}

fn scale_value(index: usize) -> u32 {
    [0, 50, 75, 100, 125, 150, 175, 200, 225, 250, 300]
        .get(index)
        .copied()
        .unwrap_or(0)
}

fn preset_index(format: Format, video: VideoSettings) -> usize {
    match (format, video.height, video.fps) {
        (Format::Mkv, 1080, 60) => 0,
        (Format::Mkv, 1080, 30) => 1,
        (Format::Mkv, 720, 30) => 2,
        (_, 720, 60) => 0,
        (_, 480, 30) => 1,
        (_, 360, 30) => 2,
        _ => 3,
    }
}

fn preset_value(format: Format, index: usize) -> VideoSettings {
    match (format, index) {
        (Format::Mkv, 0) => VideoSettings {
            height: 1080,
            fps: 60,
        },
        (Format::Mkv, 1) => VideoSettings {
            height: 1080,
            fps: 30,
        },
        (Format::Mkv, 2) => VideoSettings {
            height: 720,
            fps: 30,
        },
        (_, 0) => VideoSettings {
            height: 720,
            fps: 60,
        },
        (_, 1) => VideoSettings {
            height: 480,
            fps: 30,
        },
        _ => VideoSettings {
            height: 360,
            fps: 30,
        },
    }
}

fn resolution_index(height: u32) -> usize {
    [2160, 1440, 1080, 720, 480, 360]
        .iter()
        .position(|v| *v == height)
        .unwrap_or(3)
}

fn resolution_value(index: usize) -> u32 {
    [2160, 1440, 1080, 720, 480, 360]
        .get(index)
        .copied()
        .unwrap_or(1080)
}

fn language_index(code: &str) -> usize {
    if code.is_empty() {
        return 0;
    }
    crate::i18n::available()
        .iter()
        .position(|(item, _)| item == code)
        .map(|index| index + 1)
        .unwrap_or(0)
}

fn language_value(index: usize) -> String {
    if index == 0 {
        return String::new();
    }
    crate::i18n::available()
        .get(index - 1)
        .map(|(code, _)| code.clone())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_round_trip() {
        for format in [Format::Mkv, Format::Gif, Format::Webp] {
            for index in 0..3 {
                let video = preset_value(format, index);
                assert_eq!(preset_index(format, video), index);
            }
        }
    }

    #[test]
    fn resolution_index_clamps_to_known_values() {
        assert_eq!(resolution_value(resolution_index(1080)), 1080);
        assert_eq!(resolution_index(1234), 3);
    }

    #[test]
    fn labels_map_back_to_the_model() {
        let controls = Controls::new();
        assert_eq!(controls.index_for_label(ID_PRESET, "Balance"), Some(1));
        assert_eq!(controls.index_for_label(ID_RESOLUTION, "720p"), Some(3));
    }

    #[test]
    fn controls_write_back_into_settings() {
        let mut controls = Controls::new();
        let mut settings = Settings::default();
        controls.load_ui(&settings);
        controls.set_format_index(Format::Gif.index());
        controls.select(ID_RESOLUTION, 3);
        controls.set_slider(ID_FPS, 45);
        controls.check(ID_SYSTEM_AUDIO, true);
        controls.check(ID_MIC, true);
        controls.set_slider(ID_SOUND_VOLUME, 35);
        controls.check(ID_SCREENSHOT_SOUND, false);
        controls.read_ui(&mut settings, false);
        assert_eq!(settings.ui.format, Format::Gif);
        assert_eq!(settings.ui.video[Format::Gif.index()].height, 720);
        assert_eq!(settings.ui.video[Format::Gif.index()].fps, 45);
        assert!(settings.ui.audio.system);
        assert!(settings.ui.audio.microphone);
        assert_eq!(settings.sound_volume, 35);
        assert!(!settings.screenshot_sound);
    }

    #[test]
    fn applying_a_preset_updates_the_picture() {
        let mut controls = Controls::new();
        let mut settings = Settings::default();
        controls.load_ui(&settings);
        controls.select(ID_PRESET, 1);
        controls.apply_preset(&mut settings);
        assert_eq!(
            settings.ui.video[Format::Mkv.index()],
            VideoSettings {
                height: 1080,
                fps: 30
            }
        );
    }

    #[test]
    fn scale_labels_round_trip() {
        assert_eq!(scale_index(50), 1);
        assert_eq!(scale_index(75), 2);
        assert_eq!(scale_value(1), 50);
        assert_eq!(scale_value(2), 75);
        assert_eq!(scale_index(125), 4);
        assert_eq!(scale_value(4), 125);
    }
}
