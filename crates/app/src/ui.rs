//! The application window is a Slint component. It lives on its own thread and
//! talks to the capture engine through a small shared model plus posted
//! requests. Closing the window tears the component down; the tray thread stays
//! asleep with no timers and no child windows.

#![allow(dead_code)]

use crate::{media, AudioSettings, CaptureSettings, ProfileRef, Settings};
use media::{Format, VideoSettings};
use platform::i18n::{tr, trf};
use platform::shell::{hotkey_defs, HotkeyAction, HotkeyBinding};
use platform::UpdateState;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender, TryRecvError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

// The area overlay is compiled into its own module (`overlay`), so the main
// window's types are included here explicitly instead of through
// `slint::include_modules!`, which only names the last compiled file.
include!(concat!(env!("OUT_DIR"), "/main.rs"));

pub const ID_TAB: i32 = 100;
pub const ID_SETTINGS_TAB: i32 = 101;
pub const ID_REFRESH: i32 = 102;
pub const ID_ABOUT: i32 = 103;
pub const ID_ABOUT_CLOSE: i32 = 281;
pub const ID_ABOUT_SITE: i32 = 282;
pub const ID_ABOUT_LOGS: i32 = 283;
pub const ID_ABOUT_GITHUB: i32 = 284;
pub const ID_ABOUT_UPDATE: i32 = 287;
pub const ID_SOURCE_CLOSE: i32 = 285;
pub const ID_GAIN_WARNING_OK: i32 = 288;
pub const ID_GAIN_WARNING_CANCEL: i32 = 292;
pub const ID_CAMERA_PHOTO: i32 = 289;
pub const ID_CAMERA_VIDEO: i32 = 290;
pub const ID_CAMERA_CLOSE: i32 = 291;
pub const ID_FOLDER_DEFAULT: i32 = 300;
pub const ID_FOLDER_SCREENSHOTS: i32 = 301;
pub const ID_FOLDER_VIDEO: i32 = 302;
pub const ID_FOLDER_CAMERA: i32 = 303;
pub const ID_FOLDER_AUDIO: i32 = 304;
pub const ID_FOLDER_DEFAULT_RESET: i32 = 310;
pub const ID_FOLDER_SCREENSHOTS_RESET: i32 = 311;
pub const ID_FOLDER_VIDEO_RESET: i32 = 312;
pub const ID_FOLDER_CAMERA_RESET: i32 = 313;
pub const ID_FOLDER_AUDIO_RESET: i32 = 314;
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
pub const ID_REGION_RENDERER: i32 = 228;
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
pub const ID_IDLE_LEVEL: i32 = 246;
pub const ID_SCREENSHOT_SOUND: i32 = 227;
pub const ID_TRAY_AUDIO_GAIN_VALUE: i32 = 208;
pub const MIC_GAIN_MAX: u32 = media::GAIN_MAX;
pub const ID_SYSTEM_DEVICE: i32 = 229;
pub const ID_TRAY_AREA_SYSTEM_DEVICE: i32 = 232;
pub const ID_TRAY_VIDEO_SYSTEM_DEVICE: i32 = 233;
pub const ID_TRAY_AUDIO_SYSTEM_DEVICE: i32 = 234;
pub const ID_SYSTEM_GAIN: i32 = 235;
pub const ID_TRAY_AREA_SYSTEM_GAIN: i32 = 236;
pub const ID_TRAY_VIDEO_SYSTEM_GAIN: i32 = 237;
pub const ID_TRAY_AUDIO_SYSTEM_GAIN: i32 = 238;
pub const ID_TRAY_VIDEO_MONITOR: i32 = 242;

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
        ID_SYSTEM_AUDIO
            | ID_MIC
            | ID_TRAY_AUDIO_SYSTEM
            | ID_TRAY_AUDIO_MIC
            | ID_IDLE_LEVEL
            | ID_AUTOSTART
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
            | ID_SYSTEM_GAIN
            | ID_TRAY_AREA_SYSTEM_GAIN
            | ID_TRAY_VIDEO_SYSTEM_GAIN
            | ID_TRAY_AUDIO_SYSTEM_GAIN
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

/// The profile whose microphone dropdown `id` is.
pub fn profile_for_microphone_device(id: i32) -> Option<ProfileRef> {
    match id {
        ID_MIC_DEVICE => Some(ProfileRef::Capture),
        ID_TRAY_AREA_DEVICE => Some(ProfileRef::Area),
        ID_TRAY_VIDEO_DEVICE => Some(ProfileRef::Video),
        ID_TRAY_AUDIO_DEVICE => Some(ProfileRef::Audio),
        _ => None,
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

pub fn system_gain_control(source: ProfileRef) -> i32 {
    match source {
        ProfileRef::Capture => ID_SYSTEM_GAIN,
        ProfileRef::Area => ID_TRAY_AREA_SYSTEM_GAIN,
        ProfileRef::Video => ID_TRAY_VIDEO_SYSTEM_GAIN,
        ProfileRef::Audio => ID_TRAY_AUDIO_SYSTEM_GAIN,
    }
}

pub fn profile_for_system_gain(id: i32) -> Option<ProfileRef> {
    match id {
        ID_SYSTEM_GAIN => Some(ProfileRef::Capture),
        ID_TRAY_AREA_SYSTEM_GAIN => Some(ProfileRef::Area),
        ID_TRAY_VIDEO_SYSTEM_GAIN => Some(ProfileRef::Video),
        ID_TRAY_AUDIO_SYSTEM_GAIN => Some(ProfileRef::Audio),
        _ => None,
    }
}

fn system_device_slot(id: i32) -> Option<usize> {
    match id {
        ID_SYSTEM_DEVICE => Some(0),
        ID_TRAY_AREA_SYSTEM_DEVICE => Some(1),
        ID_TRAY_VIDEO_SYSTEM_DEVICE => Some(2),
        ID_TRAY_AUDIO_SYSTEM_DEVICE => Some(3),
        _ => None,
    }
}

pub fn profile_for_system_device(id: i32) -> Option<ProfileRef> {
    match system_device_slot(id)? {
        0 => Some(ProfileRef::Capture),
        1 => Some(ProfileRef::Area),
        2 => Some(ProfileRef::Video),
        _ => Some(ProfileRef::Audio),
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

/// The file kind a folder control changes, and whether it resets that kind to
/// the default folder instead of opening the picker.
pub fn folder_control(id: i32) -> Option<(media::OutputCategory, bool)> {
    const ROWS: [media::OutputCategory; 5] = [
        media::OutputCategory::Default,
        media::OutputCategory::Screenshots,
        media::OutputCategory::Video,
        media::OutputCategory::Camera,
        media::OutputCategory::Sound,
    ];
    let (base, reset) = match id {
        ID_FOLDER_DEFAULT..=ID_FOLDER_AUDIO => (ID_FOLDER_DEFAULT, false),
        ID_FOLDER_DEFAULT_RESET..=ID_FOLDER_AUDIO_RESET => (ID_FOLDER_DEFAULT_RESET, true),
        _ => return None,
    };
    ROWS.get((id - base) as usize)
        .copied()
        .map(|category| (category, reset))
}

#[derive(Debug)]
pub enum UiRequest {
    Command {
        id: i32,
        notification: u16,
    },
    Slider {
        id: i32,
        value: u32,
        persist: bool,
    },
    /// A hotkey row changed in the window; the application persists it.
    HotkeysChanged,
    CloseRequested,
}

enum UiCommand {
    Show(Controls),
    /// The area overlay the main window cannot host, because it is hidden.
    /// Boxed: the model carries the frozen screen, which dwarfs a `Show`.
    Overlay(Box<platform::region::Model>),
}

static UI_REQUESTS: Mutex<Vec<UiRequest>> = Mutex::new(Vec::new());
static UI_VISIBLE: AtomicBool = AtomicBool::new(false);
static UI_WEAK: Mutex<Option<slint::Weak<MainWindow>>> = Mutex::new(None);
static UI_TX: OnceLock<Sender<UiCommand>> = OnceLock::new();

/// Slint's software renderer can come back from a minimize with an empty back
/// buffer on Windows. The platform subclasses the Slint HWND and nudges it on
/// restore/show/size; this forwards those nudges to the live window.
fn install_paint_guard() {
    platform::shell::install_paint_guard(Box::new(|event| {
        let weak = UI_WEAK.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let Some(weak) = weak else { return };
        if let Some(ui) = weak.upgrade() {
            if let platform::shell::PaintEvent::Size(width, height) = event {
                let current = ui.window().size();
                if width > 0 && height > 0 && (current.width != width || current.height != height) {
                    ui.window()
                        .set_size(slint::PhysicalSize::new(width, height));
                }
            }
            ui.window().request_redraw();
        }
    }));
}

pub fn take_requests() -> Vec<UiRequest> {
    std::mem::take(&mut *UI_REQUESTS.lock().unwrap_or_else(|e| e.into_inner()))
}

fn post_request(request: UiRequest) {
    UI_REQUESTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(request);
    platform::shell::wake();
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
    platform::shell::wake_ui_thread();
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
    platform::shell::register_ui_thread();
    loop {
        let Some(command) = next_command(&rx) else {
            return;
        };
        let controls = match command {
            // The window is hidden, so the overlay drives its own event loop
            // here and the thread goes back to parking when it closes.
            UiCommand::Overlay(model) => {
                crate::overlay::run(*model);
                continue;
            }
            UiCommand::Show(controls) => controls,
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
        let screen = platform::shell::screen_width();
        let x = ((screen - width).max(0) / 2).max(0);
        ui.window()
            .set_position(slint::PhysicalPosition::new(x, 40));
        platform::shell::activate_app_window();
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

/// Opens the Slint area overlay on this process's Slint UI thread.
///
/// The UI thread is in one of two states, and both have to work:
///
/// * the main window is visible, so its `ui.run()` event loop can create and
///   show the overlay inside itself (`invoke_from_event_loop`);
/// * the window is hidden, so the thread is parked in `next_command` with no
///   event loop running: the model goes through the command channel, the thread
///   is woken, runs the overlay's own loop, and parks again afterwards.
///
/// Returns false when there is no UI thread to host the overlay (the app was
/// started hidden and never opened its window), so the caller can fall back to
/// the native overlay.
pub fn open_region_overlay(model: platform::region::Model) -> bool {
    let Some(tx) = UI_TX.get() else {
        return false;
    };
    if visible() {
        return slint::invoke_from_event_loop(move || crate::overlay::spawn(model)).is_ok();
    }
    if tx.send(UiCommand::Overlay(Box::new(model))).is_err() {
        return false;
    }
    wake_ui_thread();
    true
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
        platform::shell::wait_for_ui_message();
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
    labels.set_select_area(tr("Select area").into());
    labels.set_recent(tr("Recent").into());
    labels.set_no_captures(tr("No captures yet. The last twelve files will appear here.").into());
    labels.set_folder(tr("Folder").into());
    labels.set_open(tr("Open").into());
    labels.set_preferences(tr("Preferences").into());
    labels.set_hotkeys(tr("Hotkeys").into());
    labels.set_stop_recording(tr("Stop recording").into());
    labels.set_not_set(tr("Not set").into());
    labels.set_press_keys(tr("Press keys…").into());
    labels.set_start_with_windows(tr("Start with Windows").into());
    labels.set_start_minimized(tr("Start minimized with Windows").into());
    labels.set_hide_active(tr("Hide the window while capturing").into());
    labels.set_close_tray(tr("Close button hides to the tray").into());
    labels.set_clipboard(tr("Copy the saved file to the clipboard").into());
    labels.set_tray_capture(tr("Clicking the tray icon starts area selection").into());
    labels.set_tray_capture_hint(platform::shell::tray_click_hint().into());
    labels.set_highlight(tr("Show a red frame around the area").into());
    labels.set_completion_sound(tr("Completion sound").into());
    labels.set_sounds(tr("Sounds").into());
    labels.set_volume(tr("Volume").into());
    labels.set_gain_warning_title(tr("Volume above 100%").into());
    labels.set_gain_warning_ok(tr("Got it").into());
    labels.set_play_on_save(tr("Play when a recording is saved").into());
    labels.set_play_on_shot(tr("Play for screenshots and photos").into());
    labels.set_show_level_idle(tr("Show level in Settings").into());
    labels.set_no_signal(tr("Keep recording when the signal is lost").into());
    labels.set_show_card(tr("Show color bars instead of the frame").into());
    labels.set_tray_area(tr("Tray area").into());
    labels.set_tray_video(tr("Tray video").into());
    labels.set_monitor(tr("Monitor").into());
    labels.set_tray_audio(tr("Tray audio").into());
    labels.set_misc(tr("Miscellaneous").into());
    labels.set_overlay_renderer(tr("Overlay renderer").into());
    labels.set_encoder_threads(tr("Encoder threads").into());
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
    labels.set_video_animations(tr("Video and animation").into());
    labels.set_close(tr("Close").into());
    labels.set_about_note(tr("Capture Button records the screen, a region or sound and keeps the last twelve files in the folder below.").into());
    labels.set_licenses(tr("Licenses").into());
    labels.set_license_text(tr("FFmpeg and x264 are distributed under the GNU GPL v2 or later. Icons from Phosphor Icons are MIT licensed. Other library and crate notices are in NOTICE and THIRD-PARTY.txt next to the program.").into());
    labels.set_github("GitHub".into());
    labels.set_site(tr("Site").into());
    labels.set_open_logs(tr("Open logs").into());
    labels.set_file_locations(tr("File locations").into());
    labels.set_default_folder(tr("Default folder").into());
    labels.set_change(tr("Change").into());
    labels.set_reset(tr("Reset").into());
    labels.set_about_version(format!("Capture Button {}", env!("CARGO_PKG_VERSION")).into());
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

    let weak = ui.as_weak();
    let mut hotkeys = controls.clone();
    ui.on_hotkey_key(move |action, text, ctrl, alt, shift, meta| {
        let Some(action) = HotkeyAction::from_index(action.max(0) as usize) else {
            return;
        };
        let Some(hotkey) =
            platform::shell::hotkey_from_press(text.as_str(), ctrl, alt, shift, meta)
        else {
            return;
        };
        let Some(ui) = weak.upgrade() else {
            return;
        };
        ui.set_hotkey_capture(-1);
        hotkeys.set_hotkey(action, &hotkey.label());
        post_request(UiRequest::HotkeysChanged);
    });

    let weak = ui.as_weak();
    let mut hotkeys = controls.clone();
    ui.on_hotkey_clear(move |action| {
        let Some(action) = HotkeyAction::from_index(action.max(0) as usize) else {
            return;
        };
        hotkeys.set_hotkey(action, "");
        if let Some(ui) = weak.upgrade() {
            ui.set_hotkey_capture(-1);
        }
        post_request(UiRequest::HotkeysChanged);
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
    ui.set_region_renderer(model.renderer_index as i32);
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
    ui.set_mic_level(model.levels[0][0]);
    ui.set_system_level(model.levels[0][1]);
    ui.set_area_mic_level(model.levels[1][0]);
    ui.set_area_system_level(model.levels[1][1]);
    ui.set_video_mic_level(model.levels[2][0]);
    ui.set_video_system_level(model.levels[2][1]);
    ui.set_audio_mic_level(model.levels[3][0]);
    ui.set_audio_system_level(model.levels[3][1]);
    ui.set_mic_clip(model.clips[0][0]);
    ui.set_system_clip(model.clips[0][1]);
    ui.set_area_mic_clip(model.clips[1][0]);
    ui.set_area_system_clip(model.clips[1][1]);
    ui.set_video_mic_clip(model.clips[2][0]);
    ui.set_video_system_clip(model.clips[2][1]);
    ui.set_audio_mic_clip(model.clips[3][0]);
    ui.set_audio_system_clip(model.clips[3][1]);
    ui.set_gain(model.gains[0] as i32);
    ui.set_area_gain(model.gains[1] as i32);
    ui.set_video_gain(model.gains[2] as i32);
    ui.set_audio_gain(model.gains[3] as i32);
    ui.set_system_gain(model.system_gains[0] as i32);
    ui.set_area_system_gain(model.system_gains[1] as i32);
    ui.set_video_system_gain(model.system_gains[2] as i32);
    ui.set_audio_system_gain(model.system_gains[3] as i32);
    ui.set_autostart(model.checked(ID_AUTOSTART));
    ui.set_start_minimized(model.checked(ID_START_MINIMIZED));
    ui.set_hide_active(model.checked(ID_HIDE_ACTIVE));
    ui.set_close_tray(model.checked(ID_CLOSE_TRAY));
    ui.set_clipboard(model.checked(ID_CLIPBOARD));
    ui.set_tray_capture(model.checked(ID_TRAY_CAPTURE));
    ui.set_highlight(model.checked(ID_HIGHLIGHT));
    ui.set_outline_supported(platform::outline::supported());
    ui.set_sound(model.checked(ID_NOTIFY));
    ui.set_show_idle_level(model.checked(ID_IDLE_LEVEL));
    ui.set_sound_volume(model.sound_volume as i32);
    ui.set_shot_sound(model.checked(ID_SCREENSHOT_SOUND));
    ui.set_no_signal(model.checked(ID_NO_SIGNAL));
    ui.set_show_card(model.checked(ID_CARD));
    ui.set_capture_device(model.device_index[0] as i32);
    ui.set_area_device(model.device_index[1] as i32);
    ui.set_video_device(model.device_index[2] as i32);
    ui.set_audio_device(model.device_index[3] as i32);
    ui.set_system_device(model.system_device_index[0] as i32);
    ui.set_area_system_device(model.system_device_index[1] as i32);
    ui.set_video_system_device(model.system_device_index[2] as i32);
    ui.set_audio_system_device(model.system_device_index[3] as i32);
    ui.set_hotkey_rows(slint::ModelRc::new(slint::VecModel::from(hotkey_rows(
        model,
    ))));
    ui.set_hotkeys_supported(model.hotkeys_supported);
    ui.set_monitors_supported(model.monitors_supported);
    ui.set_video_monitor(model.monitor_index as i32);
    ui.set_status(model.status.clone().into());
    ui.set_status_saved(model.status_saved);
    ui.set_stop_label(model.stop_label.clone().into());
    ui.set_idle(model.idle);
    ui.set_recording(model.recording);
    ui.set_saving(model.saving);
    ui.set_about_open(model.about_open);
    ui.set_gain_warning(model.gain_warning);
    ui.global::<Labels>().set_gain_warning_body(
        trf(
            "Gain {gain}% can overload the sound and cause distortion. Press “Got it” to apply it.",
            &[("gain", &model.gain_warning_value.to_string())],
        )
        .into(),
    );
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
        UpdateState::Installing => (tr("InstallingвЂ¦"), false, true),
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
    ui.set_folder_default(model.folders[4].clone().into());
    ui.set_folder_screenshots(model.folders[0].clone().into());
    ui.set_folder_video(model.folders[1].clone().into());
    ui.set_folder_camera(model.folders[2].clone().into());
    ui.set_folder_audio(model.folders[3].clone().into());
    ui.set_default_folder_custom(model.folder_custom[4]);
    ui.set_screenshot_folder_custom(model.folder_custom[0]);
    ui.set_video_folder_custom(model.folder_custom[1]);
    ui.set_camera_folder_custom(model.folder_custom[2]);
    ui.set_audio_folder_custom(model.folder_custom[3]);

    let mut names = vec![slint::SharedString::from(tr("System"))];
    names.extend(
        platform::i18n::available()
            .into_iter()
            .map(|(_, name)| slint::SharedString::from(name)),
    );
    ui.set_language_names(slint::ModelRc::new(slint::VecModel::from(names)));

    let names = platform::region::overlay_renderers()
        .available
        .iter()
        .map(|renderer| slint::SharedString::from(renderer.name))
        .collect::<Vec<_>>();
    ui.set_region_renderer_names(slint::ModelRc::new(slint::VecModel::from(names)));

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
    ui.set_system_devices(slint::ModelRc::new(slint::VecModel::from(
        system_device_names(&model.system_devices),
    )));
    ui.set_monitor_names(slint::ModelRc::new(slint::VecModel::from(monitor_names(
        &model.monitors,
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

fn system_device_names(devices: &[platform::audio::AudioSource]) -> Vec<slint::SharedString> {
    let mut names = vec![slint::SharedString::from(tr("Auto (current output)"))];
    names.extend(
        devices
            .iter()
            .map(|device| slint::SharedString::from(device.name.clone())),
    );
    names
}
fn monitor_index(monitors: &[platform::screen::MonitorInfo], id: &str) -> usize {
    if id.trim().is_empty() || id.eq_ignore_ascii_case("auto") {
        0
    } else {
        monitors
            .iter()
            .position(|monitor| monitor.id == id)
            .map(|index| index + 1)
            .unwrap_or(0)
    }
}

fn monitor_names(monitors: &[platform::screen::MonitorInfo]) -> Vec<slint::SharedString> {
    let mut names = vec![slint::SharedString::from(tr("Auto (primary)"))];
    names.extend(monitors.iter().map(|monitor| {
        slint::SharedString::from(format!(
            "{} ({}x{})",
            monitor.name, monitor.width, monitor.height
        ))
    }));
    names
}

/// The Settings rows of the hotkey table: one per action, in table order.
fn hotkey_rows(model: &Model) -> Vec<HotkeyRow> {
    hotkey_defs()
        .iter()
        .map(|def| {
            let value = model
                .hotkeys
                .iter()
                .find(|binding| binding.action == def.action)
                .map(|binding| binding.combination.as_str())
                .unwrap_or_default();
            HotkeyRow {
                action: def.action.index() as i32,
                label: tr(def.label_key).into(),
                value: value.into(),
            }
        })
        .collect()
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
    renderer_index: usize,
    language: usize,
    sound_volume: u32,
    gains: [u32; 4],
    system_gains: [u32; 4],
    folders: [String; 5],
    folder_custom: [bool; 5],
    devices: [Vec<media::AudioDevice>; 4],
    device_index: [usize; 4],
    system_devices: Vec<platform::audio::AudioSource>,
    system_device_index: [usize; 4],
    monitors: Vec<platform::screen::MonitorInfo>,
    monitors_supported: bool,
    hotkeys_supported: bool,
    hotkeys: Vec<HotkeyBinding>,
    monitor_index: usize,
    history: Vec<String>,
    history_ack: Option<i32>,
    status: String,
    status_saved: bool,
    stop_label: String,
    idle: bool,
    recording: bool,
    saving: bool,
    /// Live level per profile: `[microphone, system]` in `ProfileRef::ALL` order.
    levels: [[f32; 2]; 4],
    /// A peak reached full scale since the last meter reset, per profile.
    clips: [[bool; 2]; 4],
    about_open: bool,
    gain_warning: bool,
    /// The held gain the warning asks about, shown in its text.
    gain_warning_value: u32,
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

    /// Shows the warning for a gain the release is holding back.
    ///
    /// The slider stays where the user left it: the value is only dropped when
    /// the warning is closed without the button.
    pub fn show_gain_warning(&self, gain: u32) {
        self.update(|model| {
            model.gain_warning = true;
            model.gain_warning_value = gain;
        });
    }

    pub fn close_gain_warning(&self) {
        self.update(|model| model.gain_warning = false);
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

    /// Moves the separate level meters next to the gain sliders.
    ///
    /// `levels` are `0..=1` bar positions from the recording timer and `clips`
    /// say which meter saw full scale. Only these properties are pushed: a full
    /// `sync` here would rebuild the hotkey rows twenty times a second.
    pub fn set_levels(&self, levels: &[[f32; 2]; 4], clips: &[[bool; 2]; 4]) {
        let levels = std::array::from_fn(|profile| {
            [
                levels[profile][0].clamp(0.0, 1.0),
                levels[profile][1].clamp(0.0, 1.0),
            ]
        });
        let clips = *clips;
        {
            let mut model = self.model.lock().unwrap_or_else(|e| e.into_inner());
            model.levels = levels;
            model.clips = clips;
        }
        if !visible() {
            return;
        }
        let weak = UI_WEAK.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let Some(weak) = weak else {
            return;
        };
        slint::invoke_from_event_loop(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_mic_level(levels[0][0]);
                ui.set_system_level(levels[0][1]);
                ui.set_area_mic_level(levels[1][0]);
                ui.set_area_system_level(levels[1][1]);
                ui.set_video_mic_level(levels[2][0]);
                ui.set_video_system_level(levels[2][1]);
                ui.set_audio_mic_level(levels[3][0]);
                ui.set_audio_system_level(levels[3][1]);
                ui.set_mic_clip(clips[0][0]);
                ui.set_system_clip(clips[0][1]);
                ui.set_area_mic_clip(clips[1][0]);
                ui.set_area_system_clip(clips[1][1]);
                ui.set_video_mic_clip(clips[2][0]);
                ui.set_video_system_clip(clips[2][1]);
                ui.set_audio_mic_clip(clips[3][0]);
                ui.set_audio_system_clip(clips[3][1]);
            }
        })
        .ok();
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
            ID_REGION_RENDERER => model.renderer_index = index,
            ID_LANGUAGE => model.language = index,
            ID_MIC_DEVICE => model.device_index[0] = index,
            ID_TRAY_AREA_DEVICE => model.device_index[1] = index,
            ID_TRAY_VIDEO_DEVICE => model.device_index[2] = index,
            ID_TRAY_AUDIO_DEVICE => model.device_index[3] = index,
            ID_SYSTEM_DEVICE => model.system_device_index[0] = index,
            ID_TRAY_AREA_SYSTEM_DEVICE => model.system_device_index[1] = index,
            ID_TRAY_VIDEO_SYSTEM_DEVICE => model.system_device_index[2] = index,
            ID_TRAY_AUDIO_SYSTEM_DEVICE => model.system_device_index[3] = index,
            ID_TRAY_VIDEO_MONITOR => model.monitor_index = index,
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
            ID_REGION_RENDERER => model.renderer_index,
            ID_LANGUAGE => model.language,
            ID_MIC_DEVICE => model.device_index[0],
            ID_TRAY_AREA_DEVICE => model.device_index[1],
            ID_TRAY_VIDEO_DEVICE => model.device_index[2],
            ID_TRAY_AUDIO_DEVICE => model.device_index[3],
            ID_SYSTEM_DEVICE => model.system_device_index[0],
            ID_TRAY_AREA_SYSTEM_DEVICE => model.system_device_index[1],
            ID_TRAY_VIDEO_SYSTEM_DEVICE => model.system_device_index[2],
            ID_TRAY_AUDIO_SYSTEM_DEVICE => model.system_device_index[3],
            ID_TRAY_VIDEO_MONITOR => model.monitor_index,
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
            ID_REGION_RENDERER => platform::region::overlay_renderers()
                .available
                .iter()
                .position(|renderer| renderer.name == label),
            ID_LANGUAGE => {
                let names = platform::i18n::available();
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
            ID_TRAY_VIDEO_MONITOR => {
                let model = self.model.lock().unwrap_or_else(|e| e.into_inner());
                if label == tr("Auto (primary)") {
                    Some(0)
                } else {
                    model
                        .monitors
                        .iter()
                        .position(|monitor| {
                            format!("{} ({}x{})", monitor.name, monitor.width, monitor.height)
                                == label
                        })
                        .map(|index| index + 1)
                }
            }
            ID_SYSTEM_DEVICE
            | ID_TRAY_AREA_SYSTEM_DEVICE
            | ID_TRAY_VIDEO_SYSTEM_DEVICE
            | ID_TRAY_AUDIO_SYSTEM_DEVICE => {
                system_device_slot(id)?;
                let model = self.model.lock().unwrap_or_else(|e| e.into_inner());
                if label == tr("Auto (current output)") {
                    Some(0)
                } else {
                    model
                        .system_devices
                        .iter()
                        .position(|source| source.name == label)
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
            ID_SYSTEM_GAIN => model.system_gains[0] = value.min(MIC_GAIN_MAX),
            ID_TRAY_AREA_SYSTEM_GAIN => model.system_gains[1] = value.min(MIC_GAIN_MAX),
            ID_TRAY_VIDEO_SYSTEM_GAIN => model.system_gains[2] = value.min(MIC_GAIN_MAX),
            ID_TRAY_AUDIO_SYSTEM_GAIN => model.system_gains[3] = value.min(MIC_GAIN_MAX),
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

    pub fn set_system_gain(&mut self, id: i32, gain: u32) {
        self.set_slider(id, gain);
    }

    pub fn system_gain(&self, id: i32) -> u32 {
        let model = self.model.lock().unwrap_or_else(|e| e.into_inner());
        match id {
            ID_SYSTEM_GAIN => model.system_gains[0],
            ID_TRAY_AREA_SYSTEM_GAIN => model.system_gains[1],
            ID_TRAY_VIDEO_SYSTEM_GAIN => model.system_gains[2],
            ID_TRAY_AUDIO_SYSTEM_GAIN => model.system_gains[3],
            _ => 0,
        }
    }

    pub fn set_hotkey(&mut self, action: HotkeyAction, value: &str) {
        self.update(|model| {
            match model
                .hotkeys
                .iter_mut()
                .find(|binding| binding.action == action)
            {
                Some(binding) => binding.combination = value.to_string(),
                None => model.hotkeys.push(HotkeyBinding {
                    action,
                    combination: value.to_string(),
                }),
            }
        });
    }

    pub fn hotkey(&self, action: HotkeyAction) -> String {
        self.model
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .hotkeys
            .iter()
            .find(|binding| binding.action == action)
            .map(|binding| binding.combination.clone())
            .unwrap_or_default()
    }

    /// Adopts a set of hotkey bindings.
    pub fn set_hotkeys(&self, bindings: &[HotkeyBinding]) {
        self.update(|model| {
            for binding in bindings {
                match model
                    .hotkeys
                    .iter_mut()
                    .find(|saved| saved.action == binding.action)
                {
                    Some(saved) => saved.combination = binding.combination.clone(),
                    None => model.hotkeys.push(binding.clone()),
                }
            }
        });
    }

    pub fn set_hotkeys_supported(&self, supported: bool) {
        self.update(|model| model.hotkeys_supported = supported);
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
            model.devices = std::array::from_fn(|_| devices.clone());
            model.system_devices = platform::audio::system_sources();
            model.hotkeys_supported = platform::shell::hotkeys_supported();
            model.hotkeys = settings.hotkeys.clone();
            model.monitors_supported = platform::screen::monitors_supported();
            model.monitors = platform::screen::monitors();
            model.monitor_index = monitor_index(&model.monitors, &settings.tray_video.monitor);
            model.load_audio(0, &settings.ui.audio);
            model.load_audio(1, &settings.tray_area.audio);
            model.load_audio(2, &settings.tray_video.audio);
            model.load_audio(3, &settings.tray_audio);
            model.load_folders(&settings.folders);
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
                (ID_IDLE_LEVEL, settings.show_idle_level),
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
            let chosen = platform::region::renderer_by_setting(settings.region_renderer);
            model.renderer_index = platform::region::overlay_renderers()
                .available
                .iter()
                .position(|renderer| renderer.id == chosen.id)
                .unwrap_or(0);
            model.language = language_index(&settings.ui_language);
            model.sound_volume = settings.sound_volume.min(100);
        });
    }

    pub fn load_main_video(&mut self, settings: &CaptureSettings) {
        self.update(|model| model.load_main_video(settings));
    }

    pub fn load_tray(&mut self, settings: &Settings, area: bool) {
        self.update(|model| model.load_tray(settings, area));
    }

    pub fn set_folders(&self, folders: &media::OutputFolders) {
        self.update(|model| model.load_folders(folders));
    }

    pub fn read_ui(&mut self, settings: &mut Settings, persist: bool) {
        {
            let model = self.model.lock().unwrap_or_else(|e| e.into_inner());
            model.read_ui(settings, persist);
        }
        refresh(self);
    }

    pub fn refresh_devices(&mut self, id: i32) {
        if let Some(slot) = system_device_slot(id) {
            let sources = platform::audio::system_sources();
            self.update(|model| {
                let selected_id = model
                    .system_devices
                    .get(model.system_device_index[slot].saturating_sub(1))
                    .map(|source| source.id.clone());
                model.system_devices = sources;
                model.system_device_index[slot] = selected_id
                    .and_then(|id| {
                        model
                            .system_devices
                            .iter()
                            .position(|source| source.id == id)
                            .map(|index| index + 1)
                    })
                    .unwrap_or(0);
            });
            return;
        }
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

    pub fn refresh_monitors(&mut self) {
        if !platform::screen::monitors_supported() {
            return;
        }
        let monitors = platform::screen::monitors();
        self.update(|model| {
            let selected_id = model
                .monitor_index
                .checked_sub(1)
                .and_then(|index| model.monitors.get(index))
                .map(|monitor| monitor.id.clone());
            model.monitors = monitors;
            model.monitor_index = selected_id
                .map(|id| monitor_index(&model.monitors, &id))
                .unwrap_or(0);
        });
    }

    pub fn refresh_all_devices(&mut self, settings: &mut Settings) {
        for id in [
            ID_MIC_DEVICE,
            ID_TRAY_AREA_DEVICE,
            ID_TRAY_VIDEO_DEVICE,
            ID_TRAY_AUDIO_DEVICE,
            ID_SYSTEM_DEVICE,
            ID_TRAY_AREA_SYSTEM_DEVICE,
            ID_TRAY_VIDEO_SYSTEM_DEVICE,
            ID_TRAY_AUDIO_SYSTEM_DEVICE,
        ] {
            self.refresh_devices(id);
        }
        self.refresh_monitors();
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

    fn load_main_video(&mut self, settings: &CaptureSettings) {
        let video = settings.video[settings.format.index()];
        self.resolution = resolution_index(video.height);
        self.fps[0] = video.fps;
        self.preset = preset_index(settings.format, video);
    }

    fn load_folders(&mut self, folders: &media::OutputFolders) {
        for category in media::OutputCategory::ALL {
            let index = category.index();
            self.folder_custom[index] = folders.is_custom(category);
            self.folders[index] = media::output_dir_for(category)
                .map(|path| path.display().to_string())
                .unwrap_or_else(|error| error);
        }
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
        self.system_gains[slot] = audio.system_gain.min(MIC_GAIN_MAX);
        self.device_index[slot] = if audio.device == "Auto" {
            0
        } else {
            self.devices[slot]
                .iter()
                .position(|device| device.id == audio.device)
                .map(|index| index + 1)
                .unwrap_or(0)
        };
        self.system_device_index[slot] = if audio.system_device == "auto" {
            0
        } else {
            self.system_devices
                .iter()
                .position(|source| source.id == audio.system_device)
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
        let selected_system = self.system_device_index[slot];
        let system_device = if selected_system == 0 {
            "auto".to_string()
        } else {
            self.system_devices
                .get(selected_system - 1)
                .map(|source| source.id.clone())
                .unwrap_or_else(|| previous.system_device.clone())
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
            system_device,
            system_gain: previous.system_gain,
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
        profile.monitor = if area || !platform::screen::monitors_supported() {
            previous.monitor.clone()
        } else {
            self.monitor_index
                .checked_sub(1)
                .and_then(|index| self.monitors.get(index))
                .map(|monitor| monitor.id.clone())
                .unwrap_or_else(|| "auto".into())
        };
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
        settings.show_idle_level = self.checked(ID_IDLE_LEVEL);
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
        let renderer = platform::region::overlay_renderers()
            .available
            .get(self.renderer_index)
            .copied()
            .unwrap_or_else(platform::region::default_renderer);
        settings.region_renderer = u32::from(renderer.id);
        settings.ui_language = language_value(self.language);
        settings.hotkeys = self.hotkeys.clone();
        if persist {
            settings.save();
        }
    }
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
    platform::i18n::available()
        .iter()
        .position(|(item, _)| item == code)
        .map(|index| index + 1)
        .unwrap_or(0)
}

fn language_value(index: usize) -> String {
    if index == 0 {
        return String::new();
    }
    platform::i18n::available()
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
    fn loading_settings_puts_the_saved_hotkeys_into_the_model() {
        let mut settings = Settings::default();
        settings.set_hotkey(HotkeyAction::Area, "Ctrl+Shift+A".into());
        settings.set_hotkey(HotkeyAction::Stop, "Alt+F9".into());
        let mut controls = Controls::new();
        controls.load_ui(&settings);
        assert_eq!(controls.hotkey(HotkeyAction::Area), "Ctrl+Shift+A");
        assert_eq!(controls.hotkey(HotkeyAction::Stop), "Alt+F9");
    }

    #[test]
    fn the_hotkey_rows_follow_the_platform_table() {
        let mut settings = Settings::default();
        settings.set_hotkey(HotkeyAction::Stop, "Alt+F9".into());
        let mut controls = Controls::new();
        controls.load_ui(&settings);
        let model = controls.model.lock().unwrap_or_else(|e| e.into_inner());
        let rows = hotkey_rows(&model);
        assert_eq!(rows.len(), hotkey_defs().len());
        for (index, row) in rows.iter().enumerate() {
            let def = &hotkey_defs()[index];
            assert_eq!(row.action, def.action.index() as i32);
            assert_eq!(row.label.as_str(), tr(def.label_key));
            assert_eq!(
                row.value.as_str(),
                if def.action == HotkeyAction::Stop {
                    "Alt+F9"
                } else {
                    ""
                }
            );
        }
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
}
