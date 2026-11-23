#![cfg_attr(windows, windows_subsystem = "windows")]
#![allow(unused_must_use)]
#![deny(clippy::undocumented_unsafe_blocks)]

mod camera;
mod capture;
mod media;
mod overlay;
mod settings;
mod theme;
mod ui;

use crate::ui::*;
pub(crate) use media::AudioSettings;
use media::{Format, Rect};
pub(crate) use settings::{CaptureSettings, ProfileRef, Settings};
use platform::geom::{Activity, TrayClick};
use platform::i18n::{tr, trf};
use platform::shell::{
    ID_TRAY_AREA, ID_TRAY_AUDIO, ID_TRAY_EXIT, ID_TRAY_OPEN, ID_TRAY_SHOT, ID_TRAY_STOP,
    ID_TRAY_VIDEO, ShellEvent, TrayBadge, TrayEntry, TrayGlyph, TIMER_ID,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;
use std::time::Instant;

const REPOSITORY: &str = "https://github.com/machine202502/capture-button";
const WEBSITE: &str = "https://machine202502.github.io/capture-button/";

/// Combo-box notification codes the Slint pickers post: 1 = selection changed,
/// 7 = the dropdown just opened.
const CBN_SELCHANGE: u16 = 1;
const CBN_DROPDOWN: u16 = 7;

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
                | ui::ID_ABOUT_SITE
                | ui::ID_ABOUT_LOGS
                | ui::ID_ABOUT_GITHUB
                | ui::ID_ABOUT_UPDATE
                | ui::ID_SOURCE_CLOSE
                | ui::ID_CAMERA_CLOSE
        )
        || ui::folder_control(id).is_some()
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

fn overlay_for(place: StartPlace) -> platform::region::Overlay {
    match place {
        StartPlace::TrayArea => platform::region::Overlay::Tray,
        StartPlace::ShotRegion => platform::region::Overlay::Screenshot,
        _ => platform::region::Overlay::Capture,
    }
}

/// The frozen screen and the model the Slint overlay draws.
///
/// The native overlay grabs the screen itself; a renderer in the application
/// process has to do it here and hand the pixels over. `None` when the screen
/// cannot be frozen, so the caller can leave the grabbing to the native overlay.
fn region_model(
    overlay: platform::region::Overlay,
    allow_record: bool,
) -> Option<platform::region::Model> {
    let monitor = platform::os::cursor_monitor_rect();
    let frame = if allow_record {
        platform::screen::grab_still(monitor)
    } else {
        platform::screen::grab_display(monitor)
    };
    let frame = match frame {
        Ok(frame) => frame,
        Err(error) => {
            platform::logging::warn(format!(
                "area overlay: could not freeze the screen: {error}"
            ));
            return None;
        }
    };
    let mut model =
        platform::region::Model::new(overlay, allow_record, frame, monitor, theme::PALETTE);
    model.monitors = platform::region::monitor_rects();
    if model.monitors.is_empty() {
        model.monitors.push(monitor);
    }
    model.current_monitor = model
        .monitors
        .iter()
        .position(|rect| *rect == monitor)
        .unwrap_or(0);
    Some(model)
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
        ui::ID_CAMERA_PHOTO | ui::ID_CAMERA_VIDEO => mode == Mode::Idle,
        ID_TRAY_STOP => mode == Mode::Recording,
        _ => true,
    }
}

fn history_line_now() -> String {
    let (day, month, year, hour, minute, second) = platform::os::local_time().parts();
    history_stamp(day, month, year, hour, minute, second)
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
    fn activity(self) -> Activity {
        match self {
            Self::Idle => Activity::Idle,
            Self::Selecting => Activity::Selecting,
            Self::Recording => Activity::Recording,
            Self::Saving | Self::Photo => Activity::Saving,
        }
    }
}

struct App {
    controls: Controls,
    settings: Settings,
    mode: Mode,
    /// State of the main window captured when the tray menu opened.
    tray_window_open: bool,
    session: Option<media::Session>,
    finalizer: Option<media::Finalizer>,
    live_gain: Option<Arc<AtomicU32>>,
    live_gain_id: Option<i32>,
    live_cursor: bool,
    started: Option<Instant>,
    saving_started: Option<Instant>,
    restore_visible: bool,
    hide_region: bool,
    pending_window: bool,
    pending_resilient: bool,
    pending_camera: Option<camera::CameraIndex>,
    pending_cameras: Vec<camera::CameraIndex>,
    pending_camera_action: Option<bool>,
    camera_photo: Option<Receiver<Result<PathBuf, String>>>,
    quit_after_save: bool,
    last_second: u64,
    outline: platform::outline::RegionOutline,
    record_rect: Option<Rect>,
    region_outline: bool,
    region_source: ProfileRef,
    recording_kind: RecordingKind,
    source_screenshot: bool,
    saved_path: Option<PathBuf>,
    history: Vec<(PathBuf, String)>,
}

impl App {
    fn new() -> Self {
        let settings = Settings::load();
        media::set_output_folders(&settings.folders);
        Self {
            controls: Controls::new(),
            settings,
            mode: Mode::Idle,
            tray_window_open: false,
            session: None,
            finalizer: None,
            live_gain: None,
            live_gain_id: None,
            live_cursor: true,
            started: None,
            saving_started: None,
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
            outline: platform::outline::RegionOutline::new(theme::PALETTE),
            record_rect: None,
            region_outline: false,
            region_source: ProfileRef::Capture,
            recording_kind: RecordingKind::Video,
            source_screenshot: false,
            saved_path: None,
            history: Vec::new(),
        }
    }

    fn build(&mut self) {
        self.controls.build(&mut self.settings);
        self.history = settings::load_history(ui::HISTORY_COUNT as usize);
        self.refresh_history();
        self.build_tray();
        self.update_state();
    }

    fn build_tray(&mut self) {
        let items = vec![
            TrayEntry::Item {
                id: ID_TRAY_OPEN,
                text: tr("Open window"),
                glyph: TrayGlyph::Window,
            },
            TrayEntry::Separator,
            TrayEntry::Item {
                id: ID_TRAY_AREA,
                text: tr("Select area"),
                glyph: TrayGlyph::Area,
            },
            TrayEntry::Item {
                id: ID_TRAY_SHOT,
                text: tr("Screenshot screen"),
                glyph: TrayGlyph::Shot,
            },
            TrayEntry::Item {
                id: ui::ID_CAMERA_PHOTO,
                text: tr("Camera photo"),
                glyph: TrayGlyph::Camera,
            },
            TrayEntry::Item {
                id: ID_TRAY_VIDEO,
                text: tr("Capture video"),
                glyph: TrayGlyph::Video,
            },
            TrayEntry::Item {
                id: ui::ID_CAMERA_VIDEO,
                text: tr("Camera video"),
                glyph: TrayGlyph::CameraVideo,
            },
            TrayEntry::Item {
                id: ID_TRAY_AUDIO,
                text: tr("Capture audio"),
                glyph: TrayGlyph::Audio,
            },
            TrayEntry::Item {
                id: ID_TRAY_STOP,
                text: tr("Stop capture"),
                glyph: TrayGlyph::Stop,
            },
            TrayEntry::Separator,
            TrayEntry::Item {
                id: ID_TRAY_EXIT,
                text: tr("Exit"),
                glyph: TrayGlyph::Exit,
            },
        ];
        platform::shell::build_tray(&items);
    }

    fn on_event(&mut self, event: ShellEvent) {
        match event {
            ShellEvent::Command { id, notification } => self.command(id, notification),
            ShellEvent::Timer(id) => self.on_timer(id),
            ShellEvent::TrayLeft => self.on_tray_left(),
            ShellEvent::TrayContext { x, y } => {
                // The menu itself has the foreground once it opens, so the state
                // the item shows and acts on is captured right before it appears.
                self.tray_window_open = platform::shell::main_window_open();
                self.sync_window_item();
                platform::shell::show_tray_menu(x, y);
            }
            ShellEvent::Region => self.region_done(),
            ShellEvent::Ui => self.drain_ui(),
            ShellEvent::RefreshTray => self.update_state(),
            ShellEvent::Activate => self.show_window(),
            ShellEvent::Close => self.on_close(),
            ShellEvent::Quit => {}
        }
    }

    fn on_timer(&mut self, id: usize) {
        if id == platform::shell::HISTORY_ACK_TIMER {
            self.controls.clear_history_ack();
            platform::shell::kill_timer(platform::shell::HISTORY_ACK_TIMER);
        } else {
            self.tick();
        }
    }

    fn on_tray_left(&mut self) {
        match platform::geom::tray_click(self.mode.activity(), self.settings.tray_capture) {
            TrayClick::Stop => self.stop_capture(),
            TrayClick::OpenRegion => self.begin(StartPlace::TrayArea),
            TrayClick::Show => self.show_window(),
            TrayClick::Ignore => {}
        }
    }

    fn on_close(&mut self) {
        if self.settings.close_tray {
            self.hide_window();
        } else {
            self.request_exit();
        }
    }

    fn command(&mut self, id: i32, notification: u16) {
        platform::logging::info(format!("command {id} notification {notification}"));
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
            if notification == CBN_DROPDOWN {
                self.controls.refresh_devices(id);
            }
            if notification == CBN_SELCHANGE {
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
        if !accepts_command(self.mode, id, platform::region::active()) {
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
                let controls = self.controls.clone();
                platform::update::check(Box::new(move |state| {
                    controls.set_update_state(state);
                }));
            }
            ui::ID_ABOUT_UPDATE => {
                let controls = self.controls.clone();
                platform::update::install(Box::new(move |state| {
                    controls.set_update_state(state);
                }));
            }
            ui::ID_ABOUT_CLOSE => self.controls.show_about(false),
            ui::ID_ABOUT_SITE => platform::shell::open_url(WEBSITE),
            ui::ID_ABOUT_LOGS => platform::logging::open_dir(),
            ui::ID_ABOUT_GITHUB => platform::shell::open_url(REPOSITORY),
            ID_PRESET if notification == CBN_SELCHANGE => {
                self.controls.apply_preset(&mut self.settings)
            }
            ID_TRAY_AREA_PRESET if notification == CBN_SELCHANGE => {
                self.controls.apply_tray_preset(&mut self.settings, true)
            }
            ID_TRAY_VIDEO_PRESET if notification == CBN_SELCHANGE => {
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
            | ID_REGION_RENDERER
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
            id if ui::folder_control(id).is_some() => {
                let (category, reset) = ui::folder_control(id).expect("folder control");
                if reset {
                    self.set_folder(category, None);
                } else {
                    self.choose_folder(category);
                }
            }
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
                if self.tray_window_open {
                    self.hide_window();
                    self.tray_window_open = false;
                } else {
                    self.show_window();
                    self.tray_window_open = true;
                }
                self.sync_window_item();
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
        let sources = platform::screen::list_sources();
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

    fn start_camera(&mut self, index: &camera::CameraIndex, photo: bool) {
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
                    platform::shell::start_timer(TIMER_ID, 100);
                }
                Err(error) => self.error(&error.to_string()),
            }
            return;
        }
        self.pending_camera = Some(index.clone());
        self.start_capture(ProfileRef::Capture, Take::Picture, None, false, false, None);
    }

    fn finish_camera_photo(&mut self, result: Result<PathBuf, String>) {
        platform::shell::kill_timer(TIMER_ID);
        self.camera_photo = None;
        self.saving_started = None;
        self.mode = Mode::Idle;
        match result {
            Ok(path) => self.screenshot_saved(path),
            Err(error) => self.error(&error),
        }
        self.update_state();
        if !self.window_shown() {
            platform::shell::trim_working_set();
        }
        if self.quit_after_save {
            self.request_exit();
        }
    }

    fn finish_source(&mut self, index: usize) {
        self.controls.close_sources();
        let Some(chosen) = platform::screen::choose_source_index(index) else {
            return;
        };
        let screenshot = self.source_screenshot;
        match chosen {
            platform::screen::Chosen::Monitor(rect) => {
                if screenshot {
                    self.save_rect(rect);
                } else {
                    self.pending_resilient = true;
                    let (source, take) = start_plan(StartPlace::CaptureSource);
                    self.start_capture(source, take, Some(rect), true, false, None);
                }
            }
            platform::screen::Chosen::Window(rect) => {
                if screenshot {
                    match platform::screen::grab_staged_window() {
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
        let saved = media::output_path(media::OutputCategory::Screenshots, "Image", "png")
            .and_then(|path| {
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
        platform::logging::info(format!("capture begin {place:?}"));
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
        if platform::region::active() || self.mode == Mode::Saving || self.mode == Mode::Selecting {
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
        if self.settings.region_renderer == 1 {
            // The hidden setting selects the Slint renderer. It must never make
            // the overlay unusable: anything that goes wrong here leaves the
            // work to the native overlay below.
            if let Some(model) = region_model(overlay, !recording) {
                if ui::open_region_overlay(model) {
                    platform::logging::info("area overlay: slint renderer");
                    return;
                }
                platform::logging::warn(
                    "area overlay: the slint renderer is unavailable, using the native overlay",
                );
            }
        }
        if let Err(error) = platform::region::open(overlay, !recording, theme::PALETTE) {
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
        if !platform::region::active() {
            return;
        }
        let resume = self.mode == Mode::Recording || self.mode == Mode::Saving;
        match platform::region::take_outcome() {
            Some(platform::region::Outcome::Record(rect, format)) => {
                if resume {
                    return;
                }
                self.mode = Mode::Idle;
                let (source, take) = start_plan(place_for_source(self.region_source));
                self.start_capture(source, take, Some(rect), self.hide_region, true, format);
            }
            Some(platform::region::Outcome::Screen) => {
                if resume {
                    return;
                }
                self.mode = Mode::Idle;
                let (source, take) = start_plan(area_video_place());
                self.start_capture(source, take, None, self.hide_region, false, None);
            }
            Some(platform::region::Outcome::Saved(frame, path)) => {
                if !resume {
                    self.mode = Mode::Idle;
                }
                let path = match path {
                    Some(path) => path,
                    None => match media::output_path(media::OutputCategory::Screenshots, "Image", "png")
                    {
                        Ok(path) => path,
                        Err(error) => {
                            self.error(&error);
                            return;
                        }
                    },
                };
                match media::save_frame(&frame, &path) {
                    Ok(()) => self.screenshot_saved(path),
                    Err(error) => self.error(&error),
                }
                if self.mode == Mode::Idle {
                    self.restore_window();
                }
                self.update_state();
                if self.mode == Mode::Recording {
                    self.sync_outline();
                }
            }
            Some(platform::region::Outcome::Cancel) | None => {
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
                platform::shell::start_timer(TIMER_ID, 200);
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
        if platform::region::active() {
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
        platform::shell::kill_timer(TIMER_ID);
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
                platform::shell::start_timer(TIMER_ID, 100);
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
                        platform::shell::kill_timer(TIMER_ID);
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
                            platform::shell::trim_working_set();
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
        platform::shell::kill_timer(TIMER_ID);
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
        platform::logging::info(format!("saved {}", path.display()));
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
                platform::logging::warn(format!("copy file to clipboard failed: {error}"));
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

    fn choose_folder(&mut self, category: media::OutputCategory) {
        let start = media::output_dir_for(category).ok();
        let Some(path) = platform::shell::pick_folder(start.as_deref()) else {
            return;
        };
        self.set_folder(category, Some(path));
    }

    fn set_folder(&mut self, category: media::OutputCategory, path: Option<PathBuf>) {
        self.settings.folders.set(category, path);
        media::set_output_folders(&self.settings.folders);
        self.settings.save();
        self.controls.set_folders(&self.settings.folders);
    }

    fn copy_text(&mut self, path: &std::path::Path) {
        if let Err(error) = media::copy_text_to_clipboard(&path.display().to_string()) {
            self.error(&error);
        }
    }

    fn copy_path(&mut self, id: i32, path: &std::path::Path) {
        match media::copy_text_to_clipboard(&path.display().to_string()) {
            Ok(()) => {
                platform::logging::info(format!("copied path {}", path.display()));
                self.controls.acknowledge_copy(id);
            }
            Err(error) => self.error(&error),
        }
    }

    fn copy_file(&mut self, id: i32, path: &std::path::Path) {
        match media::copy_file_to_clipboard(path) {
            Ok(()) => {
                platform::logging::info(format!("copied file {}", path.display()));
                self.controls.acknowledge_copy(id);
            }
            Err(error) => {
                platform::logging::warn(format!("copy file failed: {error}"));
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
        platform::logging::error(text);
        self.controls.set_text(ID_STATUS, text);
        platform::shell::show_error(text);
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
        platform::i18n::init(&self.settings.ui_language);
        ui::relabel(&self.controls);
        self.refresh_tray_texts();
        self.update_state();
        platform::logging::info(format!("language: {}", self.settings.ui_language));
    }

    fn refresh_tray_texts(&mut self) {
        let items = [
            (ID_TRAY_AREA, "Select area"),
            (ID_TRAY_VIDEO, "Record video"),
            (ID_TRAY_AUDIO, "Record audio"),
            (ID_TRAY_SHOT, "Screenshot screen"),
            (ui::ID_CAMERA_PHOTO, "Camera photo"),
            (ui::ID_CAMERA_VIDEO, "Camera video"),
            (ID_TRAY_STOP, "Stop capture"),
            (ID_TRAY_EXIT, "Exit"),
        ];
        for (id, key) in items {
            platform::shell::set_tray_text(id, &tr(key));
        }
        self.sync_window_item();
    }

    fn sync_window_item(&self) {
        platform::shell::set_tray_text(ID_TRAY_OPEN, &tray_window_label(self.tray_window_open));
    }

    fn show_window(&self) {
        platform::logging::info("window shown");
        platform::shell::restore_app_window();
        ui::show(&self.controls);
        self.sync_window_item();
    }

    fn hide_window(&mut self) {
        // The window may be closed while a slider still has an unsaved value.
        self.controls.read_ui(&mut self.settings, true);
        platform::logging::info("window hidden");
        ui::hide();
        self.sync_window_item();
        if self.mode == Mode::Idle {
            // Nothing is playing: give the working set back to the system. The
            // message loop still waits with zero wakeups, and the pages fault
            // back in only when the window is opened again.
            platform::shell::trim_working_set();
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
        if platform::region::active() {
            platform::region::cancel();
        }
        platform::shell::remove_tray();
        platform::shell::destroy_window();
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

    fn set_tray_status(&mut self, text: &str) {
        let badge = match self.mode {
            Mode::Recording => TrayBadge::Recording,
            Mode::Saving => TrayBadge::Saving,
            _ => TrayBadge::Idle,
        };
        platform::shell::set_tray_status(text, badge);
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
            platform::shell::set_tray_item_enabled(id, tray_item_enabled(self.mode, id));
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

fn main() {
    platform::update::init();
    if !platform::shell::init() {
        return;
    }
    platform::i18n::init(&Settings::load().ui_language);
    platform::logging::init();
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        platform::logging::error(format!("panic: {info}"));
        hook(info);
    }));
    media::init();
    platform::shell::install_palette(theme::PALETTE);
    platform::shell::create_window();
    let mut app = App::new();
    app.build();
    let minimized = std::env::args().any(|a| a == "--minimized");
    if !minimized {
        app.show_window();
    } else {
        platform::shell::trim_working_set();
    }
    platform::shell::run(|event| app.on_event(event));
}

#[cfg(test)]
mod ui_tests;
