#![cfg_attr(windows, windows_subsystem = "windows")]
#![allow(unused_must_use)]
#![deny(clippy::undocumented_unsafe_blocks)]

mod camera;
mod capture;
mod geom;
mod i18n;
mod logging;
mod media;
mod outline;
mod region;
mod settings;
mod theme;
mod ui;
mod update;

use crate::i18n::{tr, trf};
pub(crate) use media::AudioSettings;
use media::{Format, Rect};
pub(crate) use settings::{test_mode, wide, CaptureSettings, ProfileRef, Settings};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;
use std::time::Instant;
use ui::*;
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::Controls::{
    DRAWITEMSTRUCT, MEASUREITEMSTRUCT, ODS_DISABLED, ODS_GRAYED, ODS_SELECTED, ODT_MENU,
};
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const CLASS: PCWSTR = w!("CaptureButton.window");
const TITLE: PCWSTR = w!("Capture Button");
const REPOSITORY: &str = "https://github.com/machine202502/capture-button";
const WM_TRAY: u32 = WM_APP + 1;
const WM_REGION: u32 = WM_APP + 2;
const WM_ACTIVATE_APP: u32 = WM_APP + 3;
const TIMER_ID: usize = 1;
const TRAY_RETRY_TIMER: usize = 3;
const TRAY_RETRY_LIMIT: u32 = 30;
const ID_TRAY_AREA: i32 = 200;
const ID_TRAY_SHOT: i32 = 201;
const ID_TRAY_VIDEO: i32 = 202;
const ID_TRAY_AUDIO: i32 = 203;
const ID_TRAY_STOP: i32 = 204;
const ID_TRAY_EXIT: i32 = 205;
const ID_TRAY_OPEN: i32 = 206;

static mut APP: *mut App = std::ptr::null_mut();
/// The shell's broadcast message; Explorer restarts drop every tray icon until
/// the owner answers this by adding it again.
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

/// The executable's icon resource. Windows identifies it by the integer 1, not by a string.
#[allow(clippy::manual_dangling_ptr)]
pub(crate) fn module_icon() -> PCWSTR {
    PCWSTR(1 as *const u16)
}

#[cfg(test)]
fn print_window_frame(hwnd: HWND) -> media::Frame {
    use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
    unsafe {
        // SAFETY: The HWND belongs to a window this process created. The RECT is a writable local.

        // A centered window runs off the monitor and PrintWindow keeps only the visible part.
        SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_NOZORDER | SWP_NOSIZE | SWP_NOACTIVATE,
        )
        .unwrap();
        UpdateWindow(hwnd);
        let mut rect = RECT::default();
        GetWindowRect(hwnd, &mut rect).unwrap();
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        let window_dc = GetWindowDC(hwnd);
        let memory = CreateCompatibleDC(window_dc);
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = width;
        info.bmiHeader.biHeight = -height;
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB.0;
        let mut bits = std::ptr::null_mut();
        let bitmap =
            CreateDIBSection(window_dc, &info, DIB_RGB_COLORS, &mut bits, None, 0).unwrap();
        let old = SelectObject(memory, bitmap);
        PrintWindow(hwnd, memory, PRINT_WINDOW_FLAGS(2)).ok();
        let mut data = vec![0u8; width as usize * height as usize * 4];
        std::ptr::copy_nonoverlapping(bits.cast::<u8>(), data.as_mut_ptr(), data.len());
        for pixel in data.as_chunks_mut::<4>().0 {
            pixel[3] = 255;
        }
        SelectObject(memory, old);
        DeleteObject(bitmap);
        DeleteDC(memory);
        ReleaseDC(hwnd, window_dc);
        media::Frame {
            width: width as u32,
            height: height as u32,
            data,
        }
    }
}

pub(crate) fn history_stamp(
    day: u16,
    month: u16,
    year: u16,
    hour: u16,
    minute: u16,
    second: u16,
) -> String {
    let name = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .get(month.saturating_sub(1) as usize)
    .map(|month| tr(month))
    .unwrap_or_else(|| "?".to_string());
    let time = format!("{hour:02}:{minute:02}:{second:02}");
    trf(
        "{day} {month} {year}, {time}",
        &[
            ("day", &day.to_string()),
            ("month", &name),
            ("year", &year.to_string()),
            ("time", &time),
        ],
    )
}

fn accepts_command(mode: Mode, id: i32, region_open: bool) -> bool {
    let screenshot = matches!(
        id,
        ID_SHOT_SCREEN | ID_SHOT_SOURCE | ID_SHOT_REGION | ID_TRAY_SHOT | ID_TRAY_AREA
    );
    if screenshot && (region_open || mode == Mode::Saving || mode == Mode::Selecting) {
        return false;
    }
    mode == Mode::Idle
        || (mode == Mode::Recording && screenshot)
        || matches!(
            id,
            ID_STOP
                | ID_TRAY_STOP
                | ID_TRAY_EXIT
                | ID_TRAY_OPEN
                | ID_STATUS
                | ID_ABOUT
                | ui::ID_ABOUT_CLOSE
                | ui::ID_ABOUT_RECORDINGS
                | ui::ID_ABOUT_LOGS
                | ui::ID_ABOUT_GITHUB
                | ui::ID_ABOUT_UPDATE
                | ui::ID_COPY_OUTPUT
                | ui::ID_OPEN_OUTPUT
                | ui::ID_SOURCE_CLOSE
                | ui::ID_CAMERA_CLOSE
        )
        || (ui::SOURCE_ROW_BASE..ui::CAMERA_ROW_BASE).contains(&id)
        || (ui::CAMERA_ROW_BASE..600).contains(&id)
        || ui::history_click(id).is_some()
}

fn hide_while_active(from_window: bool, setting_on: bool) -> bool {
    from_window && setting_on
}

/// The window comes back when a recording that hid it ends, including a stop from the tray.
/// A promise made while selecting the area is kept even though the window is already hidden.
fn remember_restore(promised: bool, visible_now: bool) -> bool {
    promised || visible_now
}

fn show_recording_outline(highlight: bool, region: bool) -> bool {
    highlight && region
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RecordingKind {
    AreaVideo,
    AreaAnimation,
    Video,
    Animation,
    Audio,
    Camera,
}

/// Whether the picture stored on a profile is part of this take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Take {
    Picture,
    Sound,
}

/// A control that can start a recording, and the profile tied to that control.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StartPlace {
    CaptureScreen,
    CaptureSource,
    CaptureRegion,
    CaptureAudio,
    ShotRegion,
    TrayArea,
    TrayVideo,
    TrayAudio,
}

fn command_place(id: i32) -> Option<StartPlace> {
    Some(match id {
        ID_CAPTURE_SCREEN => StartPlace::CaptureScreen,
        ID_CAPTURE_SOURCE => StartPlace::CaptureSource,
        ID_CAPTURE_REGION => StartPlace::CaptureRegion,
        ID_CAPTURE_AUDIO => StartPlace::CaptureAudio,
        ID_SHOT_REGION => StartPlace::ShotRegion,
        ID_TRAY_AREA => StartPlace::TrayArea,
        ID_TRAY_VIDEO => StartPlace::TrayVideo,
        ID_TRAY_AUDIO => StartPlace::TrayAudio,
        _ => return None,
    })
}

fn start_plan(place: StartPlace) -> (ProfileRef, Take) {
    match place {
        StartPlace::CaptureScreen
        | StartPlace::CaptureSource
        | StartPlace::CaptureRegion
        | StartPlace::ShotRegion => (ProfileRef::Capture, Take::Picture),
        StartPlace::CaptureAudio => (ProfileRef::Capture, Take::Sound),
        StartPlace::TrayArea => (ProfileRef::Area, Take::Picture),
        StartPlace::TrayVideo => (ProfileRef::Video, Take::Picture),
        StartPlace::TrayAudio => (ProfileRef::Audio, Take::Sound),
    }
}

fn overlay_for(place: StartPlace) -> region::Overlay {
    match place {
        StartPlace::TrayArea => region::Overlay::Tray,
        StartPlace::ShotRegion => region::Overlay::Screenshot,
        _ => region::Overlay::Capture,
    }
}

/// Video in the tray area overlay records the whole screen with tray video recording.
fn area_video_place() -> StartPlace {
    StartPlace::TrayVideo
}

fn place_for_source(source: ProfileRef) -> StartPlace {
    match source {
        ProfileRef::Capture => StartPlace::CaptureRegion,
        ProfileRef::Area => StartPlace::TrayArea,
        ProfileRef::Video => StartPlace::TrayVideo,
        ProfileRef::Audio => StartPlace::TrayAudio,
    }
}

fn recording_kind_for(audio_only: bool, region: bool, format: Format) -> RecordingKind {
    let animation = format != Format::Mkv;
    if audio_only {
        RecordingKind::Audio
    } else if region && animation {
        RecordingKind::AreaAnimation
    } else if region {
        RecordingKind::AreaVideo
    } else if animation {
        RecordingKind::Animation
    } else {
        RecordingKind::Video
    }
}

fn recording_tip(kind: RecordingKind, seconds: u64) -> String {
    let name = match kind {
        RecordingKind::AreaVideo => tr("area video"),
        RecordingKind::AreaAnimation => tr("area animation"),
        RecordingKind::Video => tr("video"),
        RecordingKind::Animation => tr("animation"),
        RecordingKind::Audio => tr("audio"),
        RecordingKind::Camera => tr("camera video"),
    };
    trf(
        "Capture Button — recording {kind} {time}",
        &[
            ("kind", &name),
            ("time", &format!("{:02}:{:02}", seconds / 60, seconds % 60)),
        ],
    )
}

fn tray_window_label(shown: bool) -> String {
    tr(if shown { "Close window" } else { "Open window" })
}

fn tray_item_enabled(mode: Mode, id: i32) -> bool {
    match id {
        ID_TRAY_AREA | ID_TRAY_SHOT => mode == Mode::Idle || mode == Mode::Recording,
        ID_TRAY_VIDEO | ID_TRAY_AUDIO => mode == Mode::Idle,
        ID_TRAY_STOP => mode == Mode::Recording,
        _ => true,
    }
}

#[cfg(test)]
fn test_mark_recording(app: &mut App) {
    app.mode = Mode::Recording;
    app.session = None;
    app.update_state();
}

fn history_line_now() -> String {
    let time = unsafe {
        // SAFETY: The call only reads the system clock into a value the API writes.
        windows::Win32::System::SystemInformation::GetLocalTime()
    };
    history_stamp(
        time.wDay,
        time.wMonth,
        time.wYear,
        time.wHour,
        time.wMinute,
        time.wSecond,
    )
}

fn app<'a>() -> &'a mut App {
    unsafe {
        // SAFETY: APP was stored in WM_CREATE and is cleared only after the window is destroyed. Callers run on the UI thread.
        &mut *APP
    }
}

pub(crate) fn notify_region_closed() {
    unsafe {
        // SAFETY: The window handle belongs to this program. The message carries no borrowed pointer.

        if !APP.is_null() {
            PostMessageW((*APP).hwnd, WM_REGION, WPARAM(0), LPARAM(0)).ok();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    Idle,
    Selecting,
    Recording,
    Saving,
    /// A camera photo is being taken on a worker thread. Every capture button
    /// stays disabled until the photo is on disk.
    Photo,
}

impl Mode {
    fn activity(self) -> geom::Activity {
        match self {
            Self::Idle => geom::Activity::Idle,
            Self::Selecting => geom::Activity::Selecting,
            Self::Recording => geom::Activity::Recording,
            Self::Saving | Self::Photo => geom::Activity::Saving,
        }
    }
}

fn low(value: usize) -> u16 {
    (value & 0xffff) as u16
}

fn high(value: usize) -> u16 {
    ((value >> 16) & 0xffff) as u16
}

fn command_id(wparam: usize) -> i32 {
    low(wparam) as i32
}

fn notification(wparam: usize) -> u16 {
    high(wparam)
}

struct App {
    hwnd: HWND,
    controls: Controls,
    settings: Settings,
    mode: Mode,
    session: Option<media::Session>,
    finalizer: Option<media::Finalizer>,
    live_gain: Option<Arc<AtomicU32>>,
    live_gain_id: Option<i32>,
    live_cursor: bool,
    started: Option<Instant>,
    saving_started: Option<Instant>,
    tray: NOTIFYICONDATAW,
    tray_menu: HMENU,
    base_icon: HICON,
    status_icon: HICON,
    tray_added: bool,
    tray_retry: u32,
    icon_mode: Option<Mode>,
    surface: HBRUSH,
    restore_visible: bool,
    hide_region: bool,
    pending_window: bool,
    pending_resilient: bool,
    pending_camera: Option<crate::camera::CameraIndex>,
    pending_cameras: Vec<crate::camera::CameraIndex>,
    pending_camera_action: Option<bool>,
    camera_photo: Option<Receiver<Result<PathBuf, String>>>,
    quit_after_save: bool,
    last_second: u64,
    outline: outline::RegionOutline,
    record_rect: Option<Rect>,
    region_outline: bool,
    region_source: ProfileRef,
    recording_kind: RecordingKind,
    source_screenshot: bool,
    saved_path: Option<PathBuf>,
    history: Vec<(PathBuf, String)>,
    menu_icons: Vec<HBITMAP>,
}

impl App {
    fn new(hwnd: HWND) -> Self {
        let settings = Settings::load();
        Self {
            hwnd,
            controls: Controls::new(),
            settings,
            mode: Mode::Idle,
            session: None,
            finalizer: None,
            live_gain: None,
            live_gain_id: None,
            live_cursor: true,
            started: None,
            saving_started: None,
            tray: NOTIFYICONDATAW::default(),
            tray_menu: HMENU::default(),
            base_icon: HICON::default(),
            status_icon: HICON::default(),
            tray_added: false,
            tray_retry: 0,
            icon_mode: None,
            surface: unsafe {
                // SAFETY: The color is a plain COLORREF, stored for the process lifetime.
                CreateSolidBrush(theme::SURFACE)
            },
            restore_visible: false,
            hide_region: true,
            pending_window: false,
            pending_resilient: false,
            pending_camera: None,
            pending_cameras: Vec::new(),
            pending_camera_action: None,
            camera_photo: None,
            quit_after_save: false,
            last_second: u64::MAX,
            outline: outline::RegionOutline::new(),
            record_rect: None,
            region_outline: false,
            region_source: ProfileRef::Capture,
            recording_kind: RecordingKind::Video,
            source_screenshot: false,
            saved_path: None,
            history: Vec::new(),
            menu_icons: Vec::new(),
        }
    }

    fn build(&mut self) {
        ui::attach(self.hwnd);
        self.controls.build(&mut self.settings);
        self.history = settings::load_history(ui::HISTORY_COUNT as usize);
        self.refresh_history();
        self.build_tray();
        self.update_state();
    }

    fn build_tray(&mut self) {
        self.tray_menu = unsafe {
            // SAFETY: The menu is stored on the window and destroyed with it.
            CreatePopupMenu()
        }
        .unwrap();
        let mut items = vec![(ID_TRAY_OPEN, tr("Open window"), MenuGlyph::Window)];
        unsafe {
            // SAFETY: The menu belongs to this window, and the item text is NUL-terminated for the duration of the call.

            AppendMenuW(
                self.tray_menu,
                MF_STRING,
                ID_TRAY_OPEN as usize,
                i18n::pcw("Open window"),
            )
        }
        .unwrap();
        unsafe {
            // SAFETY: The menu belongs to this window, and the item text is NUL-terminated for the duration of the call.
            AppendMenuW(self.tray_menu, MF_SEPARATOR, 0, None)
        }
        .unwrap();
        for (id, text, glyph) in [
            (ID_TRAY_AREA, tr("Select area"), MenuGlyph::Area),
            (ID_TRAY_SHOT, tr("Screenshot screen"), MenuGlyph::Shot),
            (ID_TRAY_VIDEO, tr("Capture video"), MenuGlyph::Video),
            (ID_TRAY_AUDIO, tr("Capture audio"), MenuGlyph::Audio),
            (ID_TRAY_STOP, tr("Stop capture"), MenuGlyph::Stop),
        ] {
            unsafe {
                // SAFETY: The menu belongs to this window, and the item text is NUL-terminated for the duration of the call.

                AppendMenuW(
                    self.tray_menu,
                    MF_STRING,
                    id as usize,
                    PCWSTR(wide(&text).as_ptr()),
                )
            }
            .unwrap();
            items.push((id, text, glyph));
        }
        unsafe {
            // SAFETY: The menu belongs to this window, and the item text is NUL-terminated for the duration of the call.
            AppendMenuW(self.tray_menu, MF_SEPARATOR, 0, None)
        }
        .unwrap();
        unsafe {
            // SAFETY: The menu belongs to this window, and the item text is NUL-terminated for the duration of the call.
            AppendMenuW(
                self.tray_menu,
                MF_STRING,
                ID_TRAY_EXIT as usize,
                i18n::pcw("Exit"),
            )
        }
        .unwrap();
        items.push((ID_TRAY_EXIT, tr("Exit"), MenuGlyph::Exit));
        for (id, _, glyph) in items {
            let bitmap = menu_bitmap(glyph);
            self.menu_icons.push(bitmap);
            let info = MENUITEMINFOW {
                cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
                fMask: MIIM_FTYPE | MIIM_DATA,
                fType: MFT_OWNERDRAW,
                dwItemData: bitmap.0 as usize,
                ..Default::default()
            };
            unsafe {
                // SAFETY: The menu and the bitmap in the info struct both belong to this window.

                SetMenuItemInfoW(self.tray_menu, id as u32, false, &info).ok();
            }
        }
        let mut nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: 1,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: WM_TRAY,
            hIcon: unsafe {
                // SAFETY: The module is this process, and the icon id is the resource embedded in the executable.
                LoadIconW(GetModuleHandleW(None).unwrap(), module_icon())
            }
            .unwrap_or_default(),
            ..Default::default()
        };
        nid.szTip[..wide("Capture Button").len()].copy_from_slice(&wide("Capture Button"));
        self.base_icon = nid.hIcon;
        self.status_icon = nid.hIcon;
        self.tray = nid;
        let background = MENUINFO {
            cbSize: std::mem::size_of::<MENUINFO>() as u32,
            fMask: MIM_BACKGROUND | MIM_APPLYTOSUBMENUS,
            dwStyle: MNS_NOCHECK,
            hbrBack: self.surface,
            ..Default::default()
        };
        unsafe {
            // SAFETY: The menu and the brush both belong to this window.
            SetMenuInfo(self.tray_menu, &background).ok();
        }
        if !self.add_tray_icon() {
            self.schedule_tray_retry();
        }
    }

    /// Adds the balloon's icon to the current shell. Explorer owns the icon, so
    /// this must be repeatable: it is called at startup, after any failure, and
    /// when Explorer announces it has restarted.
    fn add_tray_icon(&mut self) -> bool {
        self.tray.hWnd = self.hwnd;
        self.tray.uID = 1;
        self.tray.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        self.tray.uCallbackMessage = WM_TRAY;
        self.tray.hIcon = self.status_icon;
        let added = unsafe {
            // SAFETY: The struct describes the tray icon owned by this window,
            // and every field the shell reads is filled above.
            Shell_NotifyIconW(NIM_ADD, &self.tray)
        }
        .as_bool();
        if added {
            self.tray_added = true;
            self.tray_retry = 0;
            unsafe {
                // SAFETY: This timer id belongs to this window. Killing an
                // inactive timer is harmless.
                KillTimer(self.hwnd, TRAY_RETRY_TIMER)
            }
            .ok();
            true
        } else {
            self.tray_added = false;
            false
        }
    }

    fn schedule_tray_retry(&mut self) {
        if self.tray_added || self.tray_retry >= TRAY_RETRY_LIMIT {
            return;
        }
        self.tray_retry += 1;
        unsafe {
            // SAFETY: The HWND is the main window, and this timer id is killed
            // as soon as the icon is accepted.
            SetTimer(self.hwnd, TRAY_RETRY_TIMER, 1_000, None)
        };
    }

    fn retry_tray(&mut self) {
        if self.tray_added {
            unsafe {
                // SAFETY: The HWND is the main window, and this timer belongs to it.
                KillTimer(self.hwnd, TRAY_RETRY_TIMER)
            }
            .ok();
            return;
        }
        // Rebuild the status icon before the add so the shell receives the icon
        // for the mode the app is in right now.
        self.icon_mode = None;
        self.update_state();
    }

    fn readd_tray(&mut self) {
        // Forget both the shell ownership and the cached status icon. The next
        // set_tray_status creates the icon for the current mode and then adds it.
        self.tray_added = false;
        self.icon_mode = None;
        self.update_state();
    }

    fn set_tray_status(&mut self, text: &str) {
        self.tray.szTip = [0; 128];
        let text = wide(text);
        // A user-editable locale can make this string longer than the fixed
        // NOTIFYICONDATAW field. Cut it instead of panicking (release aborts),
        // and keep the last unit as the NUL terminator.
        let limit = self.tray.szTip.len().saturating_sub(1);
        let length = text.len().min(limit);
        self.tray.szTip[..length].copy_from_slice(&text[..length]);
        self.tray.uFlags = NIF_TIP | NIF_ICON;
        if self.icon_mode != Some(self.mode) {
            let icon = tray_status_icon(self.base_icon, self.mode);
            if self.status_icon != self.base_icon {
                unsafe {
                    // SAFETY: The struct describes the tray icon owned by this window, and its size field is set.
                    DestroyIcon(self.status_icon)
                }
                .ok();
            }
            self.status_icon = icon;
            self.icon_mode = Some(self.mode);
        }
        self.tray.hIcon = self.status_icon;
        let mut tried_add = false;
        if !self.tray_added && self.tray_retry < TRAY_RETRY_LIMIT {
            tried_add = true;
            if !self.add_tray_icon() {
                self.schedule_tray_retry();
            }
        }
        let added = unsafe {
            // SAFETY: The struct describes the tray icon owned by this window, and its size field is set.
            Shell_NotifyIconW(NIM_MODIFY, &self.tray)
        }
        .as_bool();
        if added {
            // NIM_MODIFY succeeding proves the shell owns this icon, even if we
            // had marked it missing.
            self.tray_added = true;
            self.tray_retry = 0;
        } else {
            // The shell forgot this icon (usually an Explorer restart that raced
            // the TaskbarCreated broadcast). Add it again, once.
            self.tray_added = false;
            if !tried_add && self.tray_retry < TRAY_RETRY_LIMIT && !self.add_tray_icon() {
                self.schedule_tray_retry();
            }
        }
    }

    fn command(&mut self, id: i32, notification: u16) {
        logging::info(format!("command {id} notification {notification}"));
        if id == ID_TAB || id == ID_SETTINGS_TAB {
            if matches!(self.mode, Mode::Idle | Mode::Recording) {
                self.controls.show_page(if id == ID_TAB { 0 } else { 1 });
            }
            return;
        }

        // Slint's Toggle already updates the model before it posts this command.
        // Native BN_CLICKED is 0, so toggling again here would undo every click.
        if id == ID_MIC_DEVICE
            || id == ID_TRAY_AREA_DEVICE
            || id == ID_TRAY_VIDEO_DEVICE
            || id == ID_TRAY_AUDIO_DEVICE
        {
            if notification == CBN_DROPDOWN as u16 {
                self.controls.refresh_devices(id);
            }
            if notification == CBN_SELCHANGE as u16 {
                self.controls.read_ui(&mut self.settings, true);
            }
            return;
        }
        if id == ID_LANGUAGE {
            if notification == 1 {
                self.controls.read_ui(&mut self.settings, true);
                self.apply_language();
            }
            return;
        }
        if !accepts_command(self.mode, id, region::active()) {
            return;
        }
        if let Some(place) = command_place(id) {
            self.begin(place);
            return;
        }
        match id {
            ID_REFRESH => self.controls.refresh_all_devices(&mut self.settings),
            ID_ABOUT => {
                self.controls.show_about(true);
                update::check(self.controls.clone());
            }
            ui::ID_ABOUT_UPDATE => update::install(self.controls.clone()),
            ui::ID_ABOUT_CLOSE => self.controls.show_about(false),
            ui::ID_ABOUT_RECORDINGS => {
                if let Ok(path) = media::output_dir() {
                    let _ = std::process::Command::new("explorer.exe").arg(path).spawn();
                }
            }
            ui::ID_ABOUT_LOGS => logging::open_dir(),
            ui::ID_ABOUT_GITHUB => {
                let url = wide(REPOSITORY);
                unsafe {
                    // SAFETY: The URL is NUL-terminated and the owner window belongs to this program.
                    ShellExecuteW(
                        self.hwnd,
                        w!("open"),
                        PCWSTR(url.as_ptr()),
                        None,
                        None,
                        SW_SHOWNORMAL,
                    );
                }
            }
            ID_PRESET if notification == CBN_SELCHANGE as u16 => {
                self.controls.apply_preset(&mut self.settings)
            }
            ID_TRAY_AREA_PRESET if notification == CBN_SELCHANGE as u16 => {
                self.controls.apply_tray_preset(&mut self.settings, true)
            }
            ID_TRAY_VIDEO_PRESET if notification == CBN_SELCHANGE as u16 => {
                self.controls.apply_tray_preset(&mut self.settings, false)
            }
            110..=112 => {
                self.controls.set_format_index((id - ID_FORMAT) as usize);
                self.settings.ui.format = Format::from_index((id - ID_FORMAT) as usize);
                self.controls.load_main_video(&self.settings.ui);
                self.controls.read_ui(&mut self.settings, true);
                self.controls.refresh_formats();
            }
            160..=162 => {
                self.controls
                    .set_area_format_index((id - ID_TRAY_AREA_FORMAT) as usize);
                self.settings.tray_area.format =
                    Format::from_index((id - ID_TRAY_AREA_FORMAT) as usize);
                self.controls.load_tray(&self.settings, true);
                self.controls.read_ui(&mut self.settings, true);
                self.controls.refresh_formats();
            }
            170..=172 => {
                self.controls
                    .set_video_format_index((id - ID_TRAY_VIDEO_FORMAT) as usize);
                self.settings.tray_video.format =
                    Format::from_index((id - ID_TRAY_VIDEO_FORMAT) as usize);
                self.controls.load_tray(&self.settings, false);
                self.controls.read_ui(&mut self.settings, true);
                self.controls.refresh_formats();
            }
            ID_RESOLUTION
            | 163..=168
            | 173..=178
            | ID_SYSTEM_AUDIO
            | ID_MIC
            | ID_AUTOSTART..=ID_CARD
            | ID_SCREENSHOT_SOUND
            | ID_THREADS
            | ID_SCALE
            | ID_CURSOR
            | ID_TRAY_AREA_CURSOR
            | ID_TRAY_VIDEO_CURSOR
            | ID_TRAY_AUDIO_SYSTEM
            | ID_TRAY_AUDIO_MIC => {
                self.controls.read_ui(&mut self.settings, true);
                self.sync_outline();
            }
            ID_SHOT_SCREEN | ID_TRAY_SHOT => self.screenshot_screen(),
            ID_SHOT_SOURCE => self.capture_source(true),
            ui::ID_SOURCE_CLOSE => self.controls.close_sources(),
            ui::ID_COPY_OUTPUT => self.copy_output(),
            ui::ID_OPEN_OUTPUT => self.open_output(),
            ui::ID_CAMERA_PHOTO => self.camera_action(true),
            ui::ID_CAMERA_VIDEO => self.camera_action(false),
            ui::ID_CAMERA_CLOSE => self.controls.close_cameras(),
            id if (ui::CAMERA_ROW_BASE..600).contains(&id) => {
                self.finish_camera((id - ui::CAMERA_ROW_BASE) as usize)
            }
            id if (ui::SOURCE_ROW_BASE..ui::CAMERA_ROW_BASE).contains(&id) => {
                self.finish_source((id - ui::SOURCE_ROW_BASE) as usize)
            }
            ID_STOP | ID_TRAY_STOP => self.stop_capture(),
            ID_TRAY_OPEN => {
                if self.window_shown() {
                    self.hide_window();
                } else {
                    self.show_window();
                }
            }
            ID_TRAY_EXIT => self.request_exit(),
            ID_STATUS if notification == 0 => self.open_saved(),
            id if notification == 0 => {
                if let Some((index, click)) = ui::history_click(id) {
                    if let Some(path) = self.history.get(index).map(|(path, _)| path.clone()) {
                        match click {
                            ui::HistoryClick::Label => self.copy_path(id, &path),
                            ui::HistoryClick::CopyFile => self.copy_file(id, &path),
                            ui::HistoryClick::Reveal => self.reveal(&path),
                            ui::HistoryClick::Open => self.open_file(&path),
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn set_mic_gain(&mut self, id: i32, value: u32, persist: bool) {
        let value = value.min(ui::MIC_GAIN_MAX);
        let Some(source) = ui::profile_for_microphone(id) else {
            return;
        };
        self.settings.audio_mut(source).gain = value;
        self.controls.set_microphone_gain(id, value);
        if self.live_gain_id == Some(id) {
            if let Some(gain) = &self.live_gain {
                gain.store(value, Ordering::SeqCst);
            }
        }
        if persist {
            self.settings.save();
        }
    }

    fn capture_source(&mut self, screenshot: bool) {
        self.controls.read_ui(&mut self.settings, true);
        let sources = capture::list_sources();
        if sources.is_empty() {
            self.error(&tr("No capture sources were found"));
            return;
        }
        self.source_screenshot = screenshot;
        let monitors = sources.iter().filter(|source| source.monitor).count();
        self.controls.show_sources(
            sources.into_iter().map(|source| source.label).collect(),
            monitors,
        );
    }

    fn camera_action(&mut self, photo: bool) {
        if self.mode != Mode::Idle || self.camera_photo.is_some() {
            return;
        }
        self.controls.read_ui(&mut self.settings, true);
        let cameras = match camera::list() {
            Ok(cameras) => cameras,
            Err(error) => {
                self.error(&error);
                return;
            }
        };
        if cameras.is_empty() {
            self.error(&tr("No cameras were found"));
            return;
        }
        if cameras.len() == 1 {
            let index = cameras[0].index.clone();
            self.start_camera(&index, photo);
            return;
        }
        self.pending_camera_action = Some(photo);
        self.pending_cameras = cameras.iter().map(|camera| camera.index.clone()).collect();
        self.controls
            .show_cameras(cameras.into_iter().map(|camera| camera.name).collect());
    }

    fn finish_camera(&mut self, index: usize) {
        self.controls.close_cameras();
        let photo = self.pending_camera_action.take().unwrap_or(false);
        let camera = if index < self.pending_cameras.len() {
            Some(self.pending_cameras[index].clone())
        } else {
            None
        };
        self.pending_cameras.clear();
        if let Some(camera) = camera {
            self.start_camera(&camera, photo);
        }
    }

    fn start_camera(&mut self, index: &crate::camera::CameraIndex, photo: bool) {
        if self.mode != Mode::Idle || self.camera_photo.is_some() {
            return;
        }
        if photo {
            self.controls.read_ui(&mut self.settings, true);
            let index = index.clone();
            let (sender, receiver) = std::sync::mpsc::channel();
            let started = std::thread::Builder::new()
                .name("CaptureButton-photo".into())
                .spawn(move || {
                    sender.send(camera::photo(&index)).ok();
                });
            match started {
                Ok(_) => {
                    self.camera_photo = Some(receiver);
                    self.mode = Mode::Photo;
                    self.update_state();
                    unsafe {
                        // SAFETY: The HWND is the main window, and this timer id
                        // is the one killed when the photo finishes.
                        SetTimer(self.hwnd, TIMER_ID, 100, None)
                    };
                }
                Err(error) => self.error(&error.to_string()),
            }
            return;
        }
        self.pending_camera = Some(index.clone());
        self.start_capture(ProfileRef::Capture, Take::Picture, None, false, false, None);
    }

    fn finish_camera_photo(&mut self, result: Result<PathBuf, String>) {
        unsafe {
            // SAFETY: The HWND is the main window, and this timer was armed when
            // the photo started.
            KillTimer(self.hwnd, TIMER_ID)
        }
        .ok();
        self.camera_photo = None;
        self.saving_started = None;
        self.mode = Mode::Idle;
        match result {
            Ok(path) => self.screenshot_saved(path),
            Err(error) => self.error(&error),
        }
        self.update_state();
        if !self.window_shown() {
            trim_working_set();
        }
        if self.quit_after_save {
            self.request_exit();
        }
    }

    fn finish_source(&mut self, index: usize) {
        self.controls.close_sources();
        let Some(chosen) = capture::choose_source_index(index) else {
            return;
        };
        let screenshot = self.source_screenshot;
        match chosen {
            capture::Chosen::Monitor(rect) => {
                if screenshot {
                    self.save_rect(rect);
                } else {
                    self.pending_resilient = true;
                    let (source, take) = start_plan(StartPlace::CaptureSource);
                    self.start_capture(source, take, Some(rect), true, false, None);
                }
            }
            capture::Chosen::Window(rect) => {
                if screenshot {
                    match capture::grab_staged_window() {
                        Ok(frame) => self.save_frame(frame),
                        Err(error) => self.error(&error),
                    }
                } else {
                    self.pending_window = true;
                    let (source, take) = start_plan(StartPlace::CaptureSource);
                    self.start_capture(source, take, Some(rect), true, false, None);
                }
            }
        }
    }

    fn save_rect(&mut self, rect: Rect) {
        let saved = if self.mode == Mode::Recording {
            media::save_display_screenshot(rect, None)
        } else {
            media::save_screenshot(rect, None)
        };
        match saved {
            Ok(path) => self.screenshot_saved(path),
            Err(error) => self.error(&error),
        }
    }

    fn save_frame(&mut self, frame: media::Frame) {
        let saved = media::output_path("Image", "png").and_then(|path| {
            media::save_frame(&frame, &path)?;
            Ok(path)
        });
        match saved {
            Ok(path) => self.screenshot_saved(path),
            Err(error) => self.error(&error),
        }
    }

    fn screenshot_screen(&mut self) {
        self.controls.read_ui(&mut self.settings, true);
        let recording = self.mode == Mode::Recording;
        if recording {
            self.outline.hide();
        }
        let saved = if recording {
            media::save_display_screenshot(media::cursor_monitor_rect(), None)
        } else {
            media::save_screenshot(media::cursor_monitor_rect(), None)
        };
        match saved {
            Ok(path) => self.screenshot_saved(path),
            Err(error) => self.error(&error),
        }
        if recording {
            self.sync_outline();
        }
    }

    fn begin(&mut self, place: StartPlace) {
        logging::info(format!("capture begin {place:?}"));
        match place {
            StartPlace::CaptureSource => self.capture_source(false),
            StartPlace::ShotRegion => self.open_region(place, true),
            StartPlace::TrayArea => self.open_region(place, false),
            StartPlace::CaptureRegion => self.open_region(place, true),
            StartPlace::CaptureScreen | StartPlace::CaptureAudio => {
                let (source, take) = start_plan(place);
                self.start_capture(source, take, None, true, false, None);
            }
            StartPlace::TrayVideo | StartPlace::TrayAudio => {
                let (source, take) = start_plan(place);
                self.start_capture(source, take, None, false, false, None);
            }
        }
    }

    fn open_region(&mut self, place: StartPlace, hide: bool) {
        let overlay = overlay_for(place);
        if region::active() || self.mode == Mode::Saving || self.mode == Mode::Selecting {
            return;
        }
        if !overlay.annotates() && self.mode != Mode::Idle {
            return;
        }
        let recording = self.mode == Mode::Recording;
        self.hide_region = hide;
        self.controls.read_ui(&mut self.settings, true);
        if !recording {
            self.conceal_for_ui(hide);
        }
        if recording {
            self.outline.hide();
        }
        if !recording {
            self.mode = Mode::Selecting;
            self.update_state();
        }
        let (source, _) = start_plan(place);
        self.region_source = source;
        if let Err(error) = region::open(overlay, !recording) {
            self.error(&error);
            if recording {
                self.sync_outline();
            } else {
                self.mode = Mode::Idle;
                self.restore_window();
                self.update_state();
            }
        }
    }

    fn region_done(&mut self) {
        if !region::active() {
            return;
        }
        let resume = self.mode == Mode::Recording || self.mode == Mode::Saving;
        match region::take_outcome() {
            Some(region::Outcome::Record(rect, format)) => {
                if resume {
                    return;
                }
                self.mode = Mode::Idle;
                let (source, take) = start_plan(place_for_source(self.region_source));
                self.start_capture(source, take, Some(rect), self.hide_region, true, format);
            }
            Some(region::Outcome::Screen) => {
                if resume {
                    return;
                }
                self.mode = Mode::Idle;
                let (source, take) = start_plan(area_video_place());
                self.start_capture(source, take, None, self.hide_region, false, None);
            }
            Some(region::Outcome::Saved(path)) => {
                if !resume {
                    self.mode = Mode::Idle;
                }
                self.screenshot_saved(path);
                if self.mode == Mode::Idle {
                    self.restore_window();
                }
                self.update_state();
                if self.mode == Mode::Recording {
                    self.sync_outline();
                }
            }
            Some(region::Outcome::Cancel) | None => {
                if !resume {
                    self.mode = Mode::Idle;
                }
                if self.mode == Mode::Idle {
                    self.restore_window();
                }
                self.update_state();
                if self.mode == Mode::Recording {
                    self.sync_outline();
                }
            }
        }
    }

    fn recording_for(
        &self,
        source: ProfileRef,
        take: Take,
        force_format: Option<Format>,
    ) -> settings::RecordingProfile {
        let picture = matches!(take, Take::Picture);
        match force_format {
            Some(format) => self.settings.applied_as(source, picture, format),
            None => self.settings.applied(source, picture),
        }
    }

    fn start_capture(
        &mut self,
        source: ProfileRef,
        take: Take,
        rect: Option<Rect>,
        hide: bool,
        region: bool,
        force_format: Option<Format>,
    ) {
        if self.mode != Mode::Idle || self.session.is_some() {
            return;
        }
        self.controls.read_ui(&mut self.settings, true);
        let recording = self.recording_for(source, take, force_format);
        if recording.video().is_none() && !recording.audio().system && !recording.audio().microphone
        {
            self.error(&tr(
                "Enable System audio or Microphone before capturing audio",
            ));
            return;
        }
        let (profile, audio_only) = recording.session();
        let camera = self.pending_camera.take();
        self.conceal_for_ui(hide);
        self.region_outline = region && !audio_only && camera.is_none();
        self.record_rect = if audio_only || camera.is_some() {
            None
        } else {
            Some(rect.unwrap_or_else(media::cursor_monitor_rect))
        };
        let window = self.pending_window && camera.is_none();
        let resilient = camera.is_none() && (self.pending_resilient || window);
        self.pending_window = false;
        self.pending_resilient = false;
        let recorded_format = profile.format;
        self.live_cursor = profile.cursor;
        let mic_gain = Arc::new(AtomicU32::new(profile.audio.gain));
        let gain_id = ui::microphone_control(source);
        let options = media::RecordOptions {
            profile,
            rect: self.record_rect,
            audio_only,
            threads: self.settings.threads,
            window,
            camera: camera.clone(),
            resilient,
            show_no_signal: self.settings.show_no_signal,
            show_card: self.settings.show_card,
            cursor: self.live_cursor,
            mic_gain: mic_gain.clone(),
        };
        match media::Session::start(options) {
            Ok(session) => {
                self.session = Some(session);
                self.live_gain = Some(mic_gain);
                self.live_gain_id = Some(gain_id);
                self.started = Some(Instant::now());
                self.last_second = u64::MAX;
                self.recording_kind = if camera.is_some() {
                    RecordingKind::Camera
                } else {
                    recording_kind_for(audio_only, region, recorded_format)
                };
                self.mode = Mode::Recording;
                self.update_state();
                self.sync_outline();
                unsafe {
                    // SAFETY: The HWND is the main window, and this timer id is the one later killed.
                    SetTimer(self.hwnd, TIMER_ID, 200, None)
                };
            }
            Err(error) => {
                self.live_gain = None;
                self.live_gain_id = None;
                self.record_rect = None;
                self.region_outline = false;
                self.outline.hide();
                self.error(&error);
                self.restore_window();
            }
        }
    }

    fn sync_outline(&mut self) {
        if region::active() {
            self.outline.hide();
            return;
        }
        let rect = if show_recording_outline(self.settings.highlight_region, self.region_outline)
            && self.mode == Mode::Recording
        {
            self.record_rect
        } else {
            None
        };
        let elapsed = self
            .started
            .map(|start| start.elapsed().as_millis())
            .unwrap_or(0);
        self.outline.sync(rect, elapsed);
    }

    fn stop_capture(&mut self) {
        if self.mode != Mode::Recording {
            return;
        }
        unsafe {
            // SAFETY: The HWND is the main window, and this timer id is the one later killed.
            KillTimer(self.hwnd, TIMER_ID)
        }
        .ok();
        self.live_gain = None;
        self.record_rect = None;
        self.region_outline = false;
        self.outline.hide();
        let Some(session) = self.session.take() else {
            return;
        };
        self.mode = Mode::Saving;
        self.saving_started = Some(Instant::now());
        self.update_state();
        match session.stop() {
            Ok(finalizer) => {
                self.finalizer = Some(finalizer);
                unsafe {
                    // SAFETY: The HWND is the main window, and this timer id is the one later killed.
                    SetTimer(self.hwnd, TIMER_ID, 100, None)
                };
            }
            Err(error) => self.finish_error(&error),
        }
    }

    fn tick(&mut self) {
        match self.mode {
            Mode::Recording => {
                let elapsed = self.started.map(|s| s.elapsed().as_secs()).unwrap_or(0);
                if elapsed != self.last_second {
                    self.last_second = elapsed;
                    self.controls.set_text(
                        ID_STOP,
                        &trf(
                            "Stop capture  {time}",
                            &[("time", &format!("{:02}:{:02}", elapsed / 60, elapsed % 60))],
                        ),
                    );
                    self.set_tray_status(&recording_tip(self.recording_kind, elapsed));
                }
                self.sync_outline();
                let failed = self
                    .session
                    .as_mut()
                    .and_then(|session| session.tick().err());
                if let Some(error) = failed {
                    self.stop_capture();
                    self.finish_error(&error);
                } else if self
                    .session
                    .as_ref()
                    .is_some_and(|session| session.finished())
                {
                    self.stop_capture();
                }
            }
            Mode::Photo => {
                let disconnected = tr("The camera did not return a photo");
                let finished = match self.camera_photo.as_ref().map(|photo| photo.try_recv()) {
                    Some(Ok(result)) => Some(result),
                    Some(Err(TryRecvError::Disconnected)) => Some(Err(disconnected)),
                    _ => None,
                };
                if let Some(result) = finished {
                    self.finish_camera_photo(result);
                }
            }
            Mode::Saving => {
                let elapsed = self
                    .saving_started
                    .map(|s| s.elapsed().as_secs())
                    .unwrap_or(0);
                self.controls.set_text(
                    ID_STOP,
                    &trf(
                        "Saving…  {time}",
                        &[("time", &format!("{:02}:{:02}", elapsed / 60, elapsed % 60))],
                    ),
                );
                let result = self.finalizer.as_mut().map(|f| f.step());
                match result {
                    Some(Ok(Some(path))) => {
                        unsafe {
                            // SAFETY: The HWND is the main window, and this timer
                            // was armed when the save started. Without this the
                            // idle app would wake ten times a second forever.
                            KillTimer(self.hwnd, TIMER_ID)
                        }
                        .ok();
                        self.finalizer = None;
                        self.mode = Mode::Idle;
                        self.saving_started = None;
                        self.completed(path);
                        if self.settings.completion_sound {
                            media::completion_sound(self.settings.sound_volume);
                        }
                        self.restore_window();
                        self.update_state();
                        if !self.window_shown() {
                            trim_working_set();
                        }
                        if self.quit_after_save {
                            self.request_exit();
                        }
                    }
                    Some(Ok(None)) => {}
                    Some(Err(error)) => self.finish_error(&error),
                    None => {}
                }
            }
            _ => {}
        }
    }

    fn finish_error(&mut self, error: &str) {
        unsafe {
            // SAFETY: The HWND is the main window, and this timer id is the one SetTimer used.
            KillTimer(self.hwnd, TIMER_ID)
        }
        .ok();
        self.session = None;
        self.finalizer = None;
        self.live_gain = None;
        self.mode = Mode::Idle;
        self.saved_path = None;
        self.error(error);
        self.restore_window();
        self.update_state();
        if self.quit_after_save {
            self.request_exit();
        }
    }

    fn completed(&mut self, path: PathBuf) {
        logging::info(format!("saved {}", path.display()));
        self.controls.set_text(
            ID_STATUS,
            &trf("Saved: {path}", &[("path", &path.display().to_string())]),
        );
        self.saved_path = Some(path.clone());
        self.history.insert(0, (path.clone(), history_line_now()));
        self.history.truncate(ui::HISTORY_COUNT as usize);
        settings::save_history(&self.history);
        self.refresh_history();
        if self.settings.clipboard {
            if let Err(error) = media::copy_file_to_clipboard(&path) {
                logging::warn(format!("copy file to clipboard failed: {error}"));
            }
        }
    }

    /// A screenshot or camera photo reached the disk. The completion sound
    /// belongs to recordings; pictures get the screenshot sound.
    fn screenshot_saved(&mut self, path: PathBuf) {
        if self.settings.screenshot_sound {
            media::screenshot_sound(self.settings.sound_volume);
        }
        self.completed(path);
    }

    fn refresh_history(&self) {
        let lines: Vec<String> = self
            .history
            .iter()
            .map(|(path, when)| {
                trf(
                    "{when}   {file}",
                    &[
                        ("when", when),
                        (
                            "file",
                            path.file_name()
                                .and_then(|name| name.to_str())
                                .unwrap_or("file"),
                        ),
                    ],
                )
            })
            .collect();
        self.controls.set_history(&lines);
    }

    fn open_saved(&mut self) {
        let Some(path) = self.saved_path.clone() else {
            return;
        };
        self.open_path(&path);
    }

    fn open_path(&mut self, path: &std::path::Path) {
        self.copy_text(path);
        self.reveal(path);
    }

    fn open_output(&mut self) {
        let result = media::output_dir().and_then(|dir| {
            std::process::Command::new("explorer.exe")
                .arg(dir)
                .spawn()
                .map(|_| ())
                .map_err(|error| error.to_string())
        });
        match result {
            Ok(()) => logging::info("opened output folder"),
            Err(error) => self.error(&error),
        }
    }

    fn copy_output(&mut self) {
        let result = media::output_dir()
            .and_then(|dir| media::copy_text_to_clipboard(&dir.display().to_string()));
        match result {
            Ok(()) => logging::info("copied output folder"),
            Err(error) => self.error(&error),
        }
    }

    fn copy_text(&mut self, path: &std::path::Path) {
        if let Err(error) = media::copy_text_to_clipboard(&path.display().to_string()) {
            self.error(&error);
        }
    }

    fn copy_path(&mut self, id: i32, path: &std::path::Path) {
        match media::copy_text_to_clipboard(&path.display().to_string()) {
            Ok(()) => {
                logging::info(format!("copied path {}", path.display()));
                self.controls.acknowledge_copy(id);
            }
            Err(error) => self.error(&error),
        }
    }

    fn copy_file(&mut self, id: i32, path: &std::path::Path) {
        match media::copy_file_to_clipboard(path) {
            Ok(()) => {
                logging::info(format!("copied file {}", path.display()));
                self.controls.acknowledge_copy(id);
            }
            Err(error) => {
                logging::warn(format!("copy file failed: {error}"));
                self.error(&error);
            }
        }
    }

    fn open_file(&mut self, path: &std::path::Path) {
        if let Err(error) = media::open_file(path) {
            self.error(&error);
        }
    }

    fn reveal(&mut self, path: &std::path::Path) {
        if let Err(error) = media::reveal_in_folder(path) {
            self.error(&error);
        }
    }

    fn error(&self, text: &str) {
        logging::error(text);
        self.controls.set_text(ID_STATUS, text);
        unsafe {
            // SAFETY: The HWND is a window or child control this program created and still owns.

            MessageBoxW(
                self.hwnd,
                PCWSTR(wide(text).as_ptr()),
                TITLE,
                MB_OK | MB_ICONERROR,
            )
        };
    }

    fn conceal_for_ui(&mut self, from_window: bool) {
        if !hide_while_active(from_window, self.settings.hide_active) {
            return;
        }
        let visible = ui::visible();
        self.restore_visible = remember_restore(self.restore_visible, visible);
        ui::hide();
    }

    fn restore_window(&mut self) {
        if self.restore_visible {
            self.restore_visible = false;
            self.show_window();
        }
    }

    fn window_shown(&self) -> bool {
        ui::visible()
    }

    fn apply_language(&mut self) {
        i18n::init(&self.settings.ui_language);
        ui::relabel(&self.controls);
        self.refresh_tray_texts();
        self.update_state();
        logging::info(format!("language: {}", self.settings.ui_language));
    }

    fn refresh_tray_texts(&mut self) {
        let items = [
            (ID_TRAY_AREA, "Select area"),
            (ID_TRAY_VIDEO, "Record video"),
            (ID_TRAY_AUDIO, "Record audio"),
            (ID_TRAY_SHOT, "Screenshot screen"),
            (ID_TRAY_STOP, "Stop capture"),
            (ID_TRAY_EXIT, "Exit"),
        ];
        for (id, key) in items {
            let mut text = wide(&tr(key));
            let info = MENUITEMINFOW {
                cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
                fMask: MIIM_STRING,
                dwTypeData: PWSTR(text.as_mut_ptr()),
                cch: text.len() as u32,
                ..Default::default()
            };
            unsafe {
                // SAFETY: The text buffer lives through the call and the menu is owned by this window.
                SetMenuItemInfoW(self.tray_menu, id as u32, false, &info).ok();
            }
        }
        self.sync_window_item();
    }

    fn sync_window_item(&self) {
        let mut label = wide(&tray_window_label(self.window_shown()));
        let info = MENUITEMINFOW {
            cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
            fMask: MIIM_STRING,
            dwTypeData: PWSTR(label.as_mut_ptr()),
            cch: label.len() as u32,
            ..Default::default()
        };
        unsafe {
            // SAFETY: The menu belongs to this window. The label lives until SetMenuItemInfoW returns, and only the text is replaced.
            SetMenuItemInfoW(self.tray_menu, ID_TRAY_OPEN as u32, false, &info).ok();
        }
    }

    fn show_window(&self) {
        logging::info("window shown");
        ui::show(&self.controls);
    }

    fn hide_window(&mut self) {
        // The window may be closed while a slider still has an unsaved value.
        self.controls.read_ui(&mut self.settings, true);
        logging::info("window hidden");
        ui::hide();
        if self.mode == Mode::Idle {
            // Nothing is playing: give the working set back to the system. The
            // message loop still waits with zero wakeups, and the pages fault
            // back in only when the window is opened again.
            trim_working_set();
        }
    }

    fn request_exit(&mut self) {
        if self.mode == Mode::Recording {
            self.quit_after_save = true;
            self.stop_capture();
            return;
        }
        if matches!(self.mode, Mode::Saving | Mode::Photo) {
            self.quit_after_save = true;
            return;
        }
        self.controls.read_ui(&mut self.settings, true);
        ui::hide();
        if region::active() {
            region::cancel();
        }
        unsafe {
            // SAFETY: The struct describes the tray icon owned by this window, and its size field is set.
            Shell_NotifyIconW(NIM_DELETE, &self.tray)
        }
        .ok();
        self.tray_added = false;
        if self.status_icon != self.base_icon {
            unsafe {
                // SAFETY: The icon was created for this window and is no longer the one in use.
                DestroyIcon(self.status_icon)
            }
            .ok();
        }
        unsafe {
            // SAFETY: The menu belongs to the main window, and the id is one of its items.
            DestroyWindow(self.hwnd)
        }
        .ok();
    }

    fn drain_ui(&mut self) {
        for request in ui::take_requests() {
            match request {
                ui::UiRequest::Command { id, notification } => self.command(id, notification),
                ui::UiRequest::Slider { id, value, persist } => self.slider(id, value, persist),
                ui::UiRequest::CloseRequested => {
                    if self.settings.close_tray {
                        self.hide_window();
                    } else {
                        self.request_exit();
                    }
                }
            }
        }
    }

    fn slider(&mut self, id: i32, value: u32, persist: bool) {
        if ui::profile_for_microphone(id).is_some() {
            self.set_mic_gain(id, value, persist);
            self.controls.read_ui(&mut self.settings, persist);
            return;
        }
        self.controls.set_slider(id, value);
        let fps = value.clamp(5, 60);
        self.controls.set_text(
            if id == 164 {
                ID_TRAY_AREA_FPS_VALUE
            } else if id == 174 {
                ID_TRAY_VIDEO_FPS_VALUE
            } else {
                ID_FPS_VALUE
            },
            &trf("{fps} FPS", &[("fps", &fps.to_string())]),
        );
        self.controls.read_ui(&mut self.settings, persist);
    }

    fn update_state(&mut self) {
        let idle = self.mode == Mode::Idle;
        self.controls.set_idle(idle, self.mode == Mode::Recording);
        self.controls.set_saving(self.mode == Mode::Saving);
        if self.mode == Mode::Idle {
            self.controls.set_text(ID_STOP, &tr("Stop capture"));
        }
        for id in [
            ID_TRAY_AREA,
            ID_TRAY_SHOT,
            ID_TRAY_VIDEO,
            ID_TRAY_AUDIO,
            ID_TRAY_STOP,
        ] {
            unsafe {
                // SAFETY: The menu belongs to the main window, and the id is one of its items.
                EnableMenuItem(
                    self.tray_menu,
                    id as u32,
                    MF_BYCOMMAND
                        | if tray_item_enabled(self.mode, id) {
                            MF_ENABLED
                        } else {
                            MF_GRAYED
                        },
                )
            };
        }
        let elapsed = self
            .started
            .map(|start| start.elapsed().as_secs())
            .unwrap_or(0);
        let tip = match self.mode {
            Mode::Recording => recording_tip(self.recording_kind, elapsed),
            Mode::Saving => tr("Capture Button — saving"),
            Mode::Photo => tr("Capture Button — camera"),
            _ => "Capture Button".into(),
        };
        self.set_tray_status(&tip);
    }
}

// SAFETY: Windows calls this with a window of this class. The body is wrapped so
// that a panic in a paint or layout path cannot terminate a tray app that is
// meant to run for months; the release profile unwinds instead of aborting.
unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        wnd_proc_inner(hwnd, message, wparam, lparam)
    })) {
        Ok(result) => result,
        Err(_) => {
            // Never leave a windowless process blocked in GetMessage if a panic
            // happened during teardown.
            if message == WM_DESTROY {
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
    }
}

unsafe fn wnd_proc_inner(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let taskbar = TASKBAR_CREATED.load(Ordering::Relaxed);
    if taskbar != 0 && message == taskbar {
        if !APP.is_null() {
            app().readd_tray();
        }
        return LRESULT(0);
    }
    if APP.is_null() && message != WM_CREATE {
        return DefWindowProcW(hwnd, message, wparam, lparam);
    }
    match message {
        WM_CREATE => {
            APP = Box::into_raw(Box::new(App::new(hwnd)));
            app().build();
            LRESULT(0)
        }
        WM_COMMAND => {
            app().command(command_id(wparam.0), notification(wparam.0));
            LRESULT(0)
        }
        WM_UI => {
            app().drain_ui();
            LRESULT(0)
        }
        WM_MEASUREITEM => {
            let measure = unsafe {
                // SAFETY: Windows passes a MEASUREITEMSTRUCT for an owner-drawn menu item of this window.
                &mut *(lparam.0 as *mut MEASUREITEMSTRUCT)
            };
            if measure.CtlType == ODT_MENU {
                measure_tray_item(measure);
            }
            LRESULT(1)
        }
        WM_DRAWITEM => {
            let kind = unsafe {
                // SAFETY: Windows passes a DRAWITEMSTRUCT for a control or menu item of this window.
                (*((lparam.0) as *const DRAWITEMSTRUCT)).CtlType
            };
            if kind == ODT_MENU {
                draw_tray_item(lparam);
            }
            LRESULT(1)
        }
        WM_TIMER => {
            if wparam.0 == TRAY_RETRY_TIMER {
                app().retry_tray();
            } else if wparam.0 == ui::HISTORY_ACK_TIMER {
                app().controls.clear_history_ack();
                unsafe {
                    // SAFETY: The HWND is the main window, and this timer id is the copy acknowledgement.
                    KillTimer(hwnd, ui::HISTORY_ACK_TIMER)
                }
                .ok();
            } else {
                app().tick();
            }
            LRESULT(0)
        }
        WM_TRAY => {
            match low(lparam.0 as usize) as u32 {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK | 1024 | 1025 => {
                    match geom::tray_click(app().mode.activity(), app().settings.tray_capture) {
                        geom::TrayClick::Stop => app().stop_capture(),
                        geom::TrayClick::OpenRegion => app().begin(StartPlace::TrayArea),
                        geom::TrayClick::Show => app().show_window(),
                        geom::TrayClick::Ignore => {}
                    }
                }
                WM_RBUTTONUP | WM_CONTEXTMENU => {
                    let mut point = POINT::default();
                    GetCursorPos(&mut point).ok();
                    SetForegroundWindow(hwnd);
                    app().sync_window_item();
                    TrackPopupMenu(
                        app().tray_menu,
                        TPM_RIGHTBUTTON,
                        point.x,
                        point.y,
                        0,
                        hwnd,
                        None,
                    );
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_REGION => {
            // A posted notification can outlive the overlay (a double click on
            // Copy, or a stale message). Only consume an outcome while one exists.
            if region::active() {
                app().region_done();
            }
            LRESULT(0)
        }
        WM_ACTIVATE_APP => {
            app().show_window();
            LRESULT(0)
        }
        WM_CLOSE => {
            if app().settings.close_tray {
                app().hide_window();
            } else {
                app().request_exit();
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            KillTimer(hwnd, TIMER_ID).ok();
            KillTimer(hwnd, TRAY_RETRY_TIMER).ok();
            ui::hide();
            app().outline.hide();
            region::shutdown();
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

enum MenuGlyph {
    Window,
    Area,
    Shot,
    Video,
    Audio,
    Stop,
    Exit,
}

fn measure_tray_item(measure: &mut MEASUREITEMSTRUCT) {
    let mut text = [0u16; 80];
    let len = unsafe {
        // SAFETY: The menu belongs to this window. The buffer outlives the call.
        GetMenuStringW(
            app().tray_menu,
            measure.itemID,
            Some(&mut text),
            MF_BYCOMMAND,
        )
    }
    .max(0) as usize;
    let screen = unsafe {
        // SAFETY: The screen DC is released before this function returns.
        GetDC(None)
    };
    let font = unsafe {
        // SAFETY: The stock font is not destroyed.
        GetStockObject(DEFAULT_GUI_FONT)
    };
    let old = unsafe {
        // SAFETY: The font is selected only for this measurement and then restored.
        SelectObject(screen, font)
    };
    let mut extent = SIZE::default();
    unsafe {
        // SAFETY: The text length does not include the trailing NUL, and the buffer holds those units.
        GetTextExtentPoint32W(screen, &text[..len], &mut extent).ok();
        SelectObject(screen, old);
        ReleaseDC(None, screen);
    }
    let icon = 32i32;
    measure.itemWidth = (extent.cx + icon + 28).max(icon + 28) as u32;
    measure.itemHeight = (icon + 8) as u32;
}

fn draw_tray_item(lparam: LPARAM) {
    let item = unsafe {
        // SAFETY: Windows passes a DRAWITEMSTRUCT for an owner-drawn menu item of this window.
        &*(lparam.0 as *const DRAWITEMSTRUCT)
    };
    let selected = (item.itemState.0 & ODS_SELECTED.0) != 0;
    let grayed = (item.itemState.0 & (ODS_GRAYED.0 | ODS_DISABLED.0)) != 0;
    let background = if selected {
        theme::ACCENT
    } else {
        theme::SURFACE
    };
    let ink = if grayed {
        theme::DISABLED
    } else if selected {
        theme::ON_ACCENT
    } else {
        theme::TEXT
    };
    unsafe {
        // SAFETY: The DC belongs to the menu item being drawn. The brush is deleted before return.
        let brush = CreateSolidBrush(background);
        FillRect(item.hDC, &item.rcItem, brush);
        DeleteObject(brush);
        let bitmap = HBITMAP(item.itemData as *mut std::ffi::c_void);
        if !bitmap.is_invalid() {
            let memory = CreateCompatibleDC(item.hDC);
            let old = SelectObject(memory, bitmap);
            let icon = 32i32;
            let x = item.rcItem.left + 8;
            let y = item.rcItem.top + (item.rcItem.bottom - item.rcItem.top - icon) / 2;
            TransparentBlt(item.hDC, x, y, icon, icon, memory, 0, 0, icon, icon, 0).ok();
            SelectObject(memory, old);
            DeleteDC(memory);
        }
        let mut text = [0u16; 80];
        let len = GetMenuStringW(app().tray_menu, item.itemID, Some(&mut text), MF_BYCOMMAND).max(0)
            as usize;
        let mut rect = item.rcItem;
        rect.left += 8 + 32 + 8;
        SetBkMode(item.hDC, TRANSPARENT);
        SetTextColor(item.hDC, ink);
        DrawTextW(
            item.hDC,
            &mut text[..len],
            &mut rect,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE,
        );
    }
}

fn menu_bitmap(glyph: MenuGlyph) -> HBITMAP {
    let logical = 16i32;
    let scale = 2i32;
    let size = logical * scale;
    let mut small = vec![0u8; (logical * logical * 4) as usize];
    paint_menu_glyph(&mut small, logical, glyph);
    let mut info: BITMAPINFO = unsafe {
        // SAFETY: Every field the API reads, including the size, is written before the struct is used.
        std::mem::zeroed()
    };
    info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    info.bmiHeader.biWidth = size;
    info.bmiHeader.biHeight = -size;
    info.bmiHeader.biPlanes = 1;
    info.bmiHeader.biBitCount = 32;
    info.bmiHeader.biCompression = BI_RGB.0;
    let mut bits = std::ptr::null_mut();
    let screen = unsafe {
        // SAFETY: The bitmap header has its size and bit depth set. The bits pointer is used only for that byte count.
        GetDC(None)
    };
    let bitmap = unsafe {
        // SAFETY: The bitmap header has its size and bit depth set. The bits pointer is used only for that byte count.

        CreateDIBSection(screen, &info, DIB_RGB_COLORS, &mut bits, None, 0).unwrap_or_default()
    };
    unsafe {
        // SAFETY: This DC was obtained with GetDC in the same call and is released once.
        ReleaseDC(None, screen)
    };
    if bits.is_null() {
        return bitmap;
    }
    let pixels = unsafe {
        // SAFETY: The pointer addresses a live buffer, and the length is exactly that buffer's size.
        std::slice::from_raw_parts_mut(bits.cast::<u8>(), (size * size * 4) as usize)
    };
    for y in 0..logical {
        for x in 0..logical {
            let src = ((y * logical + x) * 4) as usize;
            for dy in 0..scale {
                for dx in 0..scale {
                    let dst = (((y * scale + dy) * size + x * scale + dx) * 4) as usize;
                    pixels[dst..dst + 4].copy_from_slice(&small[src..src + 4]);
                }
            }
        }
    }
    bitmap
}

fn paint_menu_glyph(pixels: &mut [u8], size: i32, glyph: MenuGlyph) {
    let sea = theme::bgra(theme::ACCENT);
    let red = theme::bgra(theme::ALERT);
    let put = |pixels: &mut [u8], x: i32, y: i32, color: [u8; 4]| {
        if (0..size).contains(&x) && (0..size).contains(&y) {
            let index = ((y * size + x) * 4) as usize;
            pixels[index..index + 4].copy_from_slice(&color);
        }
    };
    match glyph {
        MenuGlyph::Window => {
            for x in 3..13 {
                put(pixels, x, 3, sea);
                put(pixels, x, 12, sea);
            }
            for y in 3..13 {
                put(pixels, 3, y, sea);
                put(pixels, 12, y, sea);
            }
            for x in 3..13 {
                put(pixels, x, 6, sea);
            }
        }
        MenuGlyph::Area => {
            for (x, y) in [
                (3, 3),
                (4, 3),
                (3, 4),
                (11, 3),
                (12, 3),
                (12, 4),
                (3, 11),
                (3, 12),
                (4, 12),
                (11, 12),
                (12, 11),
                (12, 12),
            ] {
                put(pixels, x, y, sea);
            }
        }
        MenuGlyph::Shot => {
            for x in 3..13 {
                put(pixels, x, 4, sea);
                put(pixels, x, 12, sea);
            }
            for y in 4..13 {
                put(pixels, 3, y, sea);
                put(pixels, 12, y, sea);
            }
            for (x, y) in [(7, 7), (8, 7), (7, 8), (8, 8)] {
                put(pixels, x, y, sea);
            }
        }
        MenuGlyph::Video => stamp_circle(pixels, size, 8, 8, 5, sea),
        MenuGlyph::Audio => {
            for y in 6..11 {
                put(pixels, 4, y, sea);
            }
            for y in 4..13 {
                put(pixels, 7, y, sea);
            }
            for y in 2..15 {
                put(pixels, 10, y, sea);
            }
        }
        MenuGlyph::Stop => {
            for y in 4..12 {
                for x in 4..12 {
                    put(pixels, x, y, red);
                }
            }
        }
        MenuGlyph::Exit => {
            for i in 0..8 {
                put(pixels, 4 + i, 4 + i, red);
                put(pixels, 11 - i, 4 + i, red);
            }
        }
    }
}

fn stamp_circle(pixels: &mut [u8], size: i32, cx: i32, cy: i32, radius: i32, color: [u8; 4]) {
    let radius_sq = radius * radius;
    for y in (cy - radius)..(cy + radius + 1) {
        for x in (cx - radius)..(cx + radius + 1) {
            if x < 0 || y < 0 || x >= size || y >= size {
                continue;
            }
            let dx = x - cx;
            let dy = y - cy;
            if dx * dx + dy * dy > radius_sq {
                continue;
            }
            let index = ((y * size + x) * 4) as usize;
            pixels[index..index + 4].copy_from_slice(&color);
        }
    }
}

fn tray_status_icon(base: HICON, mode: Mode) -> HICON {
    if mode == Mode::Idle || mode == Mode::Selecting || base.is_invalid() {
        return base;
    }
    unsafe {
        // SAFETY: The color is a plain COLORREF. The brush is stored for the process or deleted in this block.

        let screen = GetDC(None);
        let dc = CreateCompatibleDC(screen);
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = 32;
        info.bmiHeader.biHeight = -32;
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB.0;
        let mut bits = std::ptr::null_mut();
        let Ok(bitmap) = CreateDIBSection(screen, &info, DIB_RGB_COLORS, &mut bits, None, 0) else {
            DeleteDC(dc);
            ReleaseDC(None, screen);
            return base;
        };
        let old = SelectObject(dc, bitmap);
        let brush = CreateSolidBrush(theme::SURFACE);
        FillRect(
            dc,
            &RECT {
                left: 0,
                top: 0,
                right: 32,
                bottom: 32,
            },
            brush,
        );
        DrawIconEx(dc, 0, 0, base, 32, 32, 0, None, DI_NORMAL).ok();
        GdiFlush();
        let pixels = std::slice::from_raw_parts_mut(bits.cast::<u8>(), 32 * 32 * 4);
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel[3] = 255;
        }
        let (fill, edge) = if mode == Mode::Recording {
            (
                theme::bgra(theme::BADGE_RECORDING),
                theme::bgra(theme::BADGE_RECORDING_EDGE),
            )
        } else {
            (
                theme::bgra(theme::BADGE_SAVING),
                theme::bgra(theme::BADGE_SAVING_EDGE),
            )
        };
        stamp_circle(pixels, 32, 23, 23, 8, edge);
        stamp_circle(pixels, 32, 23, 23, 6, fill);
        SelectObject(dc, old);
        DeleteObject(brush);
        let mask_bits = [0u8; 128];
        let mask = CreateBitmap(32, 32, 1, 1, Some(mask_bits.as_ptr().cast()));
        let icon_info = ICONINFO {
            fIcon: true.into(),
            hbmColor: bitmap,
            hbmMask: mask,
            ..Default::default()
        };
        let icon = CreateIconIndirect(&icon_info).unwrap_or(base);
        DeleteObject(bitmap);
        DeleteObject(mask);
        DeleteDC(dc);
        ReleaseDC(None, screen);
        icon
    }
}

/// Returns the unused pages of a hidden, idle process to the system. This does
/// not reduce commit (the EXE is still mapped), but it keeps the working set of
/// a tray-only instance small until the window is opened again.
fn trim_working_set() {
    unsafe {
        // SAFETY: The handle is the current process, and both sizes are SIZE_T(-1),
        // which asks the system to trim the working set.
        SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX).ok();
    }
}

fn main() {
    update::init();
    unsafe {
        // SAFETY: The window handle belongs to this program. The message carries no borrowed pointer.

        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2).ok();
        let _ = windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
        );
        // Registered before the window exists, so the first Explorer restart is
        // already covered.
        TASKBAR_CREATED.store(
            RegisterWindowMessageW(w!("TaskbarCreated")),
            Ordering::Relaxed,
        );
        let name = if test_mode() {
            w!("Local\\CaptureButton.test.single.instance")
        } else {
            w!("Local\\CaptureButton.single.instance")
        };
        let mutex = CreateMutexW(None, false, name).unwrap_or_default();
        if !mutex.0.is_null() && GetLastError() == ERROR_ALREADY_EXISTS {
            if let Ok(hwnd) = FindWindowW(CLASS, TITLE) {
                PostMessageW(hwnd, WM_ACTIVATE_APP, WPARAM(0), LPARAM(0)).ok();
            }
            return;
        }
        i18n::init(&Settings::load().ui_language);
        logging::init();
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            logging::error(format!("panic: {info}"));
            hook(info);
        }));
        media::init();
        let instance = GetModuleHandleW(None).unwrap();
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
            lpfnWndProc: Some(wnd_proc),
            hInstance: instance.into(),
            hIcon: LoadIconW(instance, module_icon()).unwrap_or_default(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap(),
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassW(&class);
        region::register(instance.into());
        let minimized = std::env::args().any(|a| a == "--minimized");
        let window = match CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS,
            TITLE,
            WS_POPUP,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            1,
            1,
            None,
            None,
            instance,
            None,
        ) {
            Ok(window) => window,
            Err(error) => {
                let text = wide(&trf(
                    "Capture Button could not start: {error}",
                    &[("error", &error.to_string())],
                ));
                MessageBoxW(None, PCWSTR(text.as_ptr()), TITLE, MB_OK | MB_ICONERROR);
                return;
            }
        };
        let _ = window;
        if !minimized {
            app().show_window();
        } else {
            trim_working_set();
        }
        let mut message = MSG::default();
        loop {
            let result = GetMessageW(&mut message, None, 0, 0).0;
            if result == 0 || result == -1 {
                break;
            }
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        if !APP.is_null() {
            drop(Box::from_raw(APP));
            APP = std::ptr::null_mut();
        }
        CloseHandle(mutex).ok();
        windows::Win32::System::Com::CoUninitialize();
    }
}

#[cfg(test)]
mod ui_tests;
