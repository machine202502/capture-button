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
use platform::geom::{Activity, TrayClick};
use platform::i18n::{tr, trf};
use platform::shell::{
    HotkeyAction, HotkeyBinding, HotkeyChoice, HotkeyEvent, ShellEvent, TrayBadge, TrayEntry,
    TrayGlyph, ID_TRAY_AREA, ID_TRAY_AUDIO, ID_TRAY_EXIT, ID_TRAY_OPEN, ID_TRAY_SHOT, ID_TRAY_STOP,
    ID_TRAY_VIDEO, LEVEL_TIMER, TIMER_ID,
};
pub(crate) use settings::{CaptureSettings, ProfileRef, Settings};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

const REPOSITORY: &str = "https://github.com/machine202502/capture-button";
const WEBSITE: &str = "https://machine202502.github.io/capture-button/";

/// Combo-box notification codes the Slint pickers post: 1 = selection changed,
/// 7 = the dropdown just opened.
const CBN_SELCHANGE: u16 = 1;
const CBN_DROPDOWN: u16 = 7;

/// Where the gain scale is neutral: 100% passes the input through unchanged.
const UNITY_GAIN: u32 = 100;

/// How long the red clip dot stays lit after a peak reaches full scale.
const CLIP_HOLD: Duration = Duration::from_millis(1500);

/// Arms or fires the above-100% warning for one gain slider.
///
/// A release at or below 100% arms the next crossing. A release above it warns
/// once and then stays quiet until the slider returns to 100%. A saved value
/// above 100% starts disarmed, so launching the app does not pop the warning by
/// itself.
fn gain_warning_step(warned: &mut bool, value: u32) -> bool {
    if value <= UNITY_GAIN {
        *warned = true;
        false
    } else if *warned {
        *warned = false;
        true
    } else {
        false
    }
}

/// Converts a percent gain to decibels; 100% is unity and 0 dB.
fn gain_db(gain: u32) -> f32 {
    if gain == 0 {
        f32::NEG_INFINITY
    } else {
        20.0 * (gain as f32 / 100.0).log10()
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
                | ui::ID_ABOUT_SITE
                | ui::ID_ABOUT_LOGS
                | ui::ID_ABOUT_GITHUB
                | ui::ID_ABOUT_UPDATE
                | ui::ID_SOURCE_CLOSE
                | ui::ID_GAIN_WARNING_OK
                | ui::ID_GAIN_WARNING_CANCEL
                | ui::ID_CAMERA_CLOSE
        )
        || ui::folder_control(id).is_some()
        || (ui::SOURCE_ROW_BASE..ui::CAMERA_ROW_BASE).contains(&id)
        || (ui::CAMERA_ROW_BASE..600).contains(&id)
        || ui::history_click(id).is_some()
}

/// The profile whose audio on/off switches `id` is.
fn audio_toggle_profile(id: i32) -> Option<ProfileRef> {
    match id {
        ID_SYSTEM_AUDIO | ID_MIC => Some(ProfileRef::Capture),
        166 | 167 => Some(ProfileRef::Area),
        176 | 177 => Some(ProfileRef::Video),
        ID_TRAY_AUDIO_SYSTEM | ID_TRAY_AUDIO_MIC => Some(ProfileRef::Audio),
        _ => None,
    }
}

/// Logs a warning once per process. A capture that cannot reach the clipboard
/// fails on every save; one line says the same thing as a hundred.
fn warn_once(text: &str) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WARNED: AtomicBool = AtomicBool::new(false);
    if !WARNED.swap(true, Ordering::Relaxed) {
        platform::logging::warn(text);
    }
}

fn hide_while_active(from_window: bool, setting_on: bool) -> bool {
    from_window && setting_on
}

/// Drops the photo worker and returns to `Idle`. A camera photo ends in this
/// state whether it succeeded, failed, or never answered, because the capture
/// panel is only enabled in `Idle`.
fn end_photo(mode: &mut Mode, photo: &mut Option<Receiver<Result<PathBuf, String>>>) {
    *photo = None;
    *mode = Mode::Idle;
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

/// The rectangle a full-screen video records.
///
/// The tray video profile may name a monitor; `"auto"` keeps the primary one.
/// An area or a chosen source already carries its own rectangle and is never
/// re-mapped, and audio or camera recordings have no monitor at all.
fn recording_rect(
    source: ProfileRef,
    rect: Option<Rect>,
    audio_only: bool,
    camera: bool,
    monitors: &[platform::screen::MonitorInfo],
    requested: &str,
) -> Option<Rect> {
    if source == ProfileRef::Video && rect.is_none() && !audio_only && !camera {
        platform::screen::monitor_rect(monitors, requested)
    } else {
        rect
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

/// The two metering-only inputs behind one profile's audio card.
#[derive(Default)]
struct PreviewPair {
    microphone: Option<platform::audio::AudioPreview>,
    system: Option<platform::audio::AudioPreview>,
}

/// A gain above 100% that the slider shows but the app has not applied.
///
/// The slider stays where the user left it; the sound and the settings keep
/// `applied` until the warning is answered. The button commits `value`, closing
/// the warning any other way puts `previous` back on the slider.
struct PendingGain {
    profile: ProfileRef,
    system: bool,
    id: i32,
    value: u32,
    previous: u32,
    /// The gain that is actually in effect while the warning waits.
    applied: u32,
}

struct App {
    controls: Controls,
    settings: Settings,
    mode: Mode,
    session: Option<media::Session>,
    finalizer: Option<media::Finalizer>,
    live_gain: Option<Arc<AtomicU32>>,
    live_gain_id: Option<i32>,
    live_system_gain: Option<Arc<AtomicU32>>,
    live_system_gain_id: Option<i32>,
    /// Peak-hold per profile, `[microphone, system]` in `ProfileRef::ALL` order.
    meters: [[platform::audio::Meter; 2]; 4],
    levels: [[f32; 2]; 4],
    /// The last time each input reached full scale, for the red clip dot.
    clip_until: [[Option<Instant>; 2]; 4],
    /// Whether the next release above 100% should show the warning. `false`
    /// means the user has already been warned and has not returned to 100%.
    gain_warned: [[bool; 2]; 4],
    /// The slider being dragged and the value it started from, so a release
    /// that needs confirmation can put the old value back.
    gain_drag: Option<(i32, u32)>,
    /// The gain the open warning asks about; nothing is applied until it is
    /// accepted.
    pending_gain: Option<PendingGain>,
    levels_at: Instant,
    /// Metering-only inputs, one pair per profile; only the visible page's
    /// profiles are open.
    previews: [PreviewPair; 4],
    /// The profile of the take in progress, whose bars show the real recording.
    recording_source: Option<ProfileRef>,
    level_timer: bool,
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
        let settings = Settings::load(platform::shell::hotkey_choice());
        media::set_output_folders(&settings.folders);
        let gain_warned = std::array::from_fn(|slot| {
            let audio = ProfileRef::ALL[slot].audio(&settings);
            [audio.gain <= UNITY_GAIN, audio.system_gain <= UNITY_GAIN]
        });
        Self {
            controls: Controls::new(),
            settings,
            mode: Mode::Idle,
            session: None,
            finalizer: None,
            live_gain: None,
            live_gain_id: None,
            live_system_gain: None,
            live_system_gain_id: None,
            meters: Default::default(),
            levels: [[0.0; 2]; 4],
            clip_until: [[None; 2]; 4],
            gain_warned,
            gain_drag: None,
            pending_gain: None,
            levels_at: Instant::now(),
            previews: Default::default(),
            recording_source: None,
            level_timer: false,
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
        self.apply_hotkeys();
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
            ShellEvent::Region => self.region_done(),
            ShellEvent::Ui => self.drain_ui(),
            ShellEvent::RefreshTray => self.update_state(),
            ShellEvent::Activate => self.show_window(),
            ShellEvent::Close => self.on_close(),
            ShellEvent::Hotkey(action) => self.on_hotkey(action),
            ShellEvent::Hotkeys(event) => self.on_hotkeys(event),
            ShellEvent::Quit => {}
        }
    }

    fn on_hotkey(&mut self, action: HotkeyAction) {
        platform::logging::info(format!("hotkey {action:?}"));
        match action {
            HotkeyAction::Area => self.command(ID_TRAY_AREA, 0),
            HotkeyAction::Stop => self.stop_capture(),
        }
    }

    /// Takes the answer of a hotkey request the desktop answered in its dialog.
    ///
    /// The settings are updated only with what the desktop reported, except
    /// that a cancelled request leaves the previous set in place.
    fn on_hotkeys(&mut self, event: HotkeyEvent) {
        match event {
            HotkeyEvent::Pending => {
                platform::logging::info("hotkeys: waiting for the system dialog");
                self.controls.set_hotkeys_pending(true);
            }
            HotkeyEvent::Applied(bindings) => {
                platform::logging::info("hotkeys: the desktop applied the set");
                self.controls.set_hotkeys_pending(false);
                self.settings.set_hotkeys(&bindings);
                self.settings.hotkeys_configured = true;
                self.settings.save();
                self.controls.set_hotkeys(&bindings);
                self.log_hotkeys();
            }
            HotkeyEvent::Failed { message, bindings } => {
                self.controls.set_hotkeys_pending(false);
                self.settings.set_hotkeys(&bindings);
                self.settings.save();
                self.controls.set_hotkeys(&bindings);
                platform::logging::info(format!("hotkeys: {message}"));
                self.controls.set_text(ID_STATUS, &message);
                self.log_hotkeys();
            }
            HotkeyEvent::Unavailable => {
                platform::logging::info(
                    "hotkeys: the desktop portal is not available; using the fallback",
                );
                self.controls
                    .set_hotkey_choice(platform::shell::hotkey_choice());
                self.controls
                    .set_hotkeys_supported(platform::shell::hotkeys_supported());
                self.apply_hotkeys();
            }
        }
    }

    fn on_timer(&mut self, id: usize) {
        if id == platform::shell::HISTORY_ACK_TIMER {
            self.controls.clear_history_ack();
            platform::shell::kill_timer(platform::shell::HISTORY_ACK_TIMER);
        } else if id == LEVEL_TIMER {
            self.update_levels();
        } else {
            self.tick();
        }
    }

    /// Moves the level meters next to the audio gain sliders.
    ///
    /// While a take records, the recording profile shows the take's peaks,
    /// which the capture stream already publishes after its live gain. When the
    /// window is open on a page with audio cards, each card's metering-only
    /// preview is read at the device level and the profile's gain is added in
    /// decibels, so the meter shows what the recording would receive. Every
    /// other profile stays at zero. The peak hold and fall live in
    /// [`platform::audio::Meter`].
    fn update_levels(&mut self) {
        let elapsed = self.levels_at.elapsed();
        self.levels_at = Instant::now();
        if let Some(source) = self.recording_source {
            let (microphone, system) = platform::audio::levels();
            self.advance_profile(source, microphone, system, elapsed);
            for profile in ProfileRef::ALL {
                if profile != source {
                    self.advance_profile(profile, f32::NEG_INFINITY, f32::NEG_INFINITY, elapsed);
                }
            }
        } else {
            for profile in ProfileRef::ALL {
                let slot = profile.index();
                let audio = profile.audio(&self.settings);
                let microphone_gain = gain_db(audio.gain);
                let system_gain = gain_db(audio.system_gain);
                let microphone = self.previews[slot]
                    .microphone
                    .as_ref()
                    .map_or(f32::NEG_INFINITY, |preview| {
                        preview.level() + microphone_gain
                    });
                let system = self.previews[slot]
                    .system
                    .as_ref()
                    .map_or(f32::NEG_INFINITY, |preview| preview.level() + system_gain);
                self.advance_profile(profile, microphone, system, elapsed);
            }
        }
        let clips = self.clips_now();
        self.controls.set_levels(&self.levels, &clips);
    }

    fn advance_profile(
        &mut self,
        profile: ProfileRef,
        microphone: f32,
        system: f32,
        elapsed: Duration,
    ) {
        let slot = profile.index();
        self.levels[slot][0] = self.meters[slot][0].update(microphone, elapsed);
        self.levels[slot][1] = self.meters[slot][1].update(system, elapsed);
        let now = Instant::now();
        for (kind, db) in [microphone, system].into_iter().enumerate() {
            if db >= 0.0 {
                self.clip_until[slot][kind] = Some(now + CLIP_HOLD);
            }
        }
    }

    /// The clip dots to draw: a peak reached full scale within [`CLIP_HOLD`].
    fn clips_now(&self) -> [[bool; 2]; 4] {
        let now = Instant::now();
        std::array::from_fn(|slot| {
            std::array::from_fn(|kind| self.clip_until[slot][kind].is_some_and(|until| now < until))
        })
    }

    /// Empties every bar; no device is open outside a take or a page preview.
    fn reset_levels(&mut self) {
        for profile in ProfileRef::ALL {
            self.reset_profile_levels(profile);
        }
        self.levels_at = Instant::now();
        let clips = self.clips_now();
        self.controls.set_levels(&self.levels, &clips);
    }

    fn reset_profile_levels(&mut self, profile: ProfileRef) {
        let slot = profile.index();
        self.meters[slot] = [platform::audio::Meter::new(), platform::audio::Meter::new()];
        self.levels[slot] = [0.0; 2];
        self.clip_until[slot] = [None, None];
    }

    /// Starts the twenty-per-second meter timer; showing the window again
    /// must not spawn a second one.
    fn start_level_timer(&mut self) {
        if self.level_timer {
            return;
        }
        self.level_timer = true;
        platform::shell::start_timer(LEVEL_TIMER, 50);
    }

    fn stop_level_timer(&mut self) {
        if !self.level_timer {
            return;
        }
        self.level_timer = false;
        platform::shell::kill_timer(LEVEL_TIMER);
    }

    /// The profiles whose audio cards are on screen: the capture profile on the
    /// Capture page, the three tray profiles on the Settings page.
    fn preview_profiles(&self) -> &'static [ProfileRef] {
        if self.controls.page() == 1 {
            &[ProfileRef::Area, ProfileRef::Video, ProfileRef::Audio]
        } else {
            &[ProfileRef::Capture]
        }
    }

    /// Opens the metering-only inputs behind the visible page's audio cards.
    /// A device that refuses (missing, busy) only leaves its bar at zero; a take
    /// opens it again on its own.
    fn open_previews(&mut self) {
        if !self.settings.show_idle_level || !self.window_shown() {
            return;
        }
        for profile in self.preview_profiles() {
            self.open_preview_pair(*profile);
        }
    }

    fn open_preview_pair(&mut self, profile: ProfileRef) {
        let slot = profile.index();
        let microphone = profile.audio(&self.settings).microphone;
        let system = profile.audio(&self.settings).system;
        if self.previews[slot].microphone.is_none() && microphone {
            let device = self.microphone_device_for(profile);
            match platform::audio::open_preview(false, device.as_deref()) {
                Ok(preview) => self.previews[slot].microphone = Some(preview),
                Err(error) => platform::logging::warn(format!("idle level: {error}")),
            }
        }
        if self.previews[slot].system.is_none() && system {
            let device = self.system_device_for(profile);
            match platform::audio::open_preview(true, device.as_deref()) {
                Ok(preview) => self.previews[slot].system = Some(preview),
                Err(error) => platform::logging::warn(format!("idle level: {error}")),
            }
        }
        platform::logging::info("idle level: preview open");
    }

    /// Releases one section's metering inputs; a take and a preview never share
    /// one, and a device change must not leave the old capture running.
    fn close_preview_pair(&mut self, profile: ProfileRef) {
        let slot = profile.index();
        let microphone = self.previews[slot].microphone.take();
        let system = self.previews[slot].system.take();
        if microphone.is_some() || system.is_some() {
            platform::logging::info("idle level: preview closed");
            self.reset_profile_levels(profile);
        }
    }

    fn close_previews(&mut self) {
        for profile in ProfileRef::ALL {
            self.close_preview_pair(profile);
        }
    }

    fn refresh_previews(&mut self) {
        self.close_previews();
        self.open_previews();
    }

    /// Restarts one section after its device or its inputs changed. The other
    /// sections keep their own streams.
    fn refresh_previews_for(&mut self, profile: ProfileRef) {
        self.close_preview_pair(profile);
        if self.settings.show_idle_level
            && self.window_shown()
            && self.mode != Mode::Recording
            && self.preview_profiles().contains(&profile)
        {
            self.open_preview_pair(profile);
        }
    }

    fn microphone_device_for(&self, profile: ProfileRef) -> Option<String> {
        let id = profile.audio(&self.settings).device.as_str();
        if id == "Auto" {
            None
        } else {
            media::device_name(id)
        }
    }

    fn system_device_for(&self, profile: ProfileRef) -> Option<String> {
        let id = profile.audio(&self.settings).system_device.as_str();
        if id.eq_ignore_ascii_case("auto") {
            None
        } else {
            Some(id.to_string())
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
                self.refresh_previews();
            }
            return;
        }

        // Slint's Toggle already updates the model before it posts this command.
        // Native BN_CLICKED is 0, so toggling again here would undo every click.
        if id == ID_MIC_DEVICE
            || id == ID_TRAY_AREA_DEVICE
            || id == ID_TRAY_VIDEO_DEVICE
            || id == ID_TRAY_AUDIO_DEVICE
            || id == ID_SYSTEM_DEVICE
            || id == ID_TRAY_AREA_SYSTEM_DEVICE
            || id == ID_TRAY_VIDEO_SYSTEM_DEVICE
            || id == ID_TRAY_AUDIO_SYSTEM_DEVICE
        {
            if notification == CBN_DROPDOWN {
                self.controls.refresh_devices(id);
            }
            if notification == CBN_SELCHANGE {
                self.read_ui(true);
                if let Some(profile) = ui::profile_for_microphone_device(id)
                    .or_else(|| ui::profile_for_system_device(id))
                {
                    self.refresh_previews_for(profile);
                }
            }
            return;
        }
        if id == ID_TRAY_VIDEO_MONITOR {
            if notification == CBN_DROPDOWN {
                self.controls.refresh_monitors();
            }
            if notification == CBN_SELCHANGE {
                self.read_ui(true);
            }
            return;
        }
        if id == ID_LANGUAGE {
            if notification == 1 {
                self.read_ui(true);
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
            ui::ID_GAIN_WARNING_OK => self.accept_gain_warning(),
            ui::ID_GAIN_WARNING_CANCEL => self.dismiss_gain_warning(),
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
                self.read_ui(true);
                self.controls.refresh_formats();
            }
            160..=162 => {
                self.controls
                    .set_area_format_index((id - ID_TRAY_AREA_FORMAT) as usize);
                self.settings.tray_area.format =
                    Format::from_index((id - ID_TRAY_AREA_FORMAT) as usize);
                self.controls.load_tray(&self.settings, true);
                self.read_ui(true);
                self.controls.refresh_formats();
            }
            170..=172 => {
                self.controls
                    .set_video_format_index((id - ID_TRAY_VIDEO_FORMAT) as usize);
                self.settings.tray_video.format =
                    Format::from_index((id - ID_TRAY_VIDEO_FORMAT) as usize);
                self.controls.load_tray(&self.settings, false);
                self.read_ui(true);
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
            | ID_TRAY_AUDIO_MIC
            | ID_IDLE_LEVEL => {
                self.read_ui(true);
                if id == ID_IDLE_LEVEL {
                    self.refresh_previews();
                } else if let Some(profile) = audio_toggle_profile(id) {
                    self.refresh_previews_for(profile);
                }
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
            ui::ID_CAMERA_CLOSE => {
                // Closing the picker is a cancellation: nothing may stay queued
                // for a camera that was never chosen.
                self.pending_camera_action = None;
                self.pending_cameras.clear();
                self.controls.close_cameras();
            }
            id if (ui::CAMERA_ROW_BASE..600).contains(&id) => {
                self.finish_camera((id - ui::CAMERA_ROW_BASE) as usize)
            }
            id if (ui::SOURCE_ROW_BASE..ui::CAMERA_ROW_BASE).contains(&id) => {
                self.finish_source((id - ui::SOURCE_ROW_BASE) as usize)
            }
            ID_STOP | ID_TRAY_STOP => self.stop_capture(),
            // The item always shows the window; closing is the window's own
            // close button.
            ID_TRAY_OPEN => self.show_window(),
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

    /// Reads the window into the settings.
    ///
    /// A gain that is waiting for the warning is left at the value that is
    /// actually applied, so saving the settings from elsewhere (the tray, a
    /// hotkey, closing the window) cannot commit it behind the card's back.
    fn read_ui(&mut self, persist: bool) {
        let held = self
            .pending_gain
            .as_ref()
            .map(|pending| (pending.profile, pending.system, pending.applied));
        self.controls
            .read_ui(&mut self.settings, persist && held.is_none());
        if let Some((profile, system, applied)) = held {
            let audio = self.settings.audio_mut(profile);
            if system {
                audio.system_gain = applied;
            } else {
                audio.gain = applied;
            }
        }
    }

    /// Writes one gain into the settings, the window and a running take.
    ///
    /// `persist` saves the settings to the store; a drag passes `false` and the
    /// release saves once.
    fn set_gain(&mut self, profile: ProfileRef, system: bool, id: i32, value: u32, persist: bool) {
        let value = value.min(ui::MIC_GAIN_MAX);
        if system {
            self.settings.audio_mut(profile).system_gain = value;
            self.controls.set_system_gain(id, value);
            if self.live_system_gain_id == Some(id) {
                if let Some(gain) = &self.live_system_gain {
                    gain.store(value, Ordering::SeqCst);
                }
            }
        } else {
            self.settings.audio_mut(profile).gain = value;
            self.controls.set_microphone_gain(id, value);
            if self.live_gain_id == Some(id) {
                if let Some(gain) = &self.live_gain {
                    gain.store(value, Ordering::SeqCst);
                }
            }
        }
        if persist {
            self.settings.save();
        }
    }

    /// Rebuilds the global key bindings from the saved settings.
    ///
    /// With [`HotkeyChoice::SystemDialog`] the backend asks the desktop, and the
    /// answer arrives later as a `ShellEvent::Hotkeys`; on Wayland that dialog
    /// is the only path. The fallback grabs the keys on an X11 connection, and
    /// without one the capability check keeps the Settings section hidden and
    /// this returns early.
    fn apply_hotkeys(&mut self) {
        if !platform::shell::hotkeys_supported() {
            return;
        }
        self.log_hotkeys();
        if let Err(error) = platform::shell::register_hotkeys(&self.hotkey_set()) {
            platform::logging::warn(format!("hotkeys: {error}"));
            self.controls.set_text(ID_STATUS, &error);
        }
    }

    /// The set to hand the backend.
    ///
    /// A session where the desktop owns the combinations opens its dialog once,
    /// and an empty set would give it nothing to offer. Until that dialog has
    /// been answered, an unset row is proposed with the table's default
    /// combination; after it, an empty row is a deliberate "no hotkey" and stays
    /// empty.
    fn hotkey_set(&self) -> Vec<HotkeyBinding> {
        let propose = platform::shell::hotkey_choice() == HotkeyChoice::SystemDialog
            && !self.settings.hotkeys_configured;
        self.settings
            .hotkeys
            .iter()
            .map(|binding| {
                if binding.is_set() || !propose {
                    binding.clone()
                } else {
                    HotkeyBinding {
                        action: binding.action,
                        combination: binding.action.def().default_trigger.to_string(),
                    }
                }
            })
            .collect()
    }

    /// Asks the desktop for a new combination of `action`, keeping the rest of
    /// the set. The answer arrives as a `ShellEvent::Hotkeys`.
    fn change_hotkey(&mut self, action: HotkeyAction) {
        if !platform::shell::hotkeys_supported() {
            return;
        }
        self.log_hotkeys();
        let mut bindings = self.hotkey_set();
        // The row the user asked about has to be in the set for the desktop to
        // ask again; a cleared row is proposed with its default combination.
        if let Some(binding) = bindings.iter_mut().find(|binding| binding.action == action) {
            if !binding.is_set() {
                binding.combination = action.def().default_trigger.to_string();
            }
        }
        if let Err(error) = platform::shell::change_hotkeys(action, &bindings) {
            platform::logging::warn(format!("hotkeys: {error}"));
            self.controls.set_text(ID_STATUS, &error);
        }
    }

    fn log_hotkeys(&self) {
        let saved = platform::shell::hotkey_defs()
            .iter()
            .map(|def| format!("{}={:?}", def.id, self.settings.hotkey(def.action)))
            .collect::<Vec<_>>()
            .join(" ");
        platform::logging::info(format!(
            "hotkeys: {:?} supported={} {saved}",
            platform::shell::hotkey_choice(),
            platform::shell::hotkeys_supported()
        ));
    }

    fn capture_source(&mut self, screenshot: bool) {
        self.read_ui(true);
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
        self.read_ui(true);
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

    /// Leaves the photo mode whatever happened to the worker: a failure, a
    /// missing answer or a finished frame. Every capture button is gated on
    /// `Mode::Idle`, so an error that stayed in `Mode::Photo` would disable the
    /// whole capture panel until the application restarted.
    fn end_photo(&mut self) {
        end_photo(&mut self.mode, &mut self.camera_photo);
        self.saving_started = None;
    }

    fn start_camera(&mut self, index: &camera::CameraIndex, photo: bool) {
        if self.mode != Mode::Idle || self.camera_photo.is_some() {
            return;
        }
        if photo {
            self.read_ui(true);
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
        self.end_photo();
        // The buttons come back before the failure is reported: the report is a
        // modal dialog, and the state must not depend on it being dismissed.
        self.update_state();
        match result {
            Ok(path) => self.screenshot_saved(path),
            Err(error) => self.error(&error),
        }
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
        self.read_ui(true);
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
        self.read_ui(true);
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
        let renderer = platform::region::renderer_by_setting(self.settings.region_renderer);
        if renderer.kind == platform::region::RendererKind::Slint {
            // The selected renderer is drawn by the application. It must never
            // make the overlay unusable: anything that goes wrong here leaves
            // the work to the native overlay below.
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
                    None => {
                        match media::output_path(media::OutputCategory::Screenshots, "Image", "png")
                        {
                            Ok(path) => path,
                            Err(error) => {
                                self.error(&error);
                                return;
                            }
                        }
                    }
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
        self.read_ui(true);
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
        let monitors =
            if source == ProfileRef::Video && rect.is_none() && !audio_only && camera.is_none() {
                platform::screen::monitors()
            } else {
                Vec::new()
            };
        let rect = recording_rect(
            source,
            rect,
            audio_only,
            camera.is_some(),
            &monitors,
            &self.settings.tray_video.monitor,
        );
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
        let system_gain = Arc::new(AtomicU32::new(profile.audio.system_gain));
        let system_gain_id = ui::system_gain_control(source);
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
            system_gain: system_gain.clone(),
        };
        // A preview holds the same device; release it before the take opens it.
        self.close_previews();
        match media::Session::start(options) {
            Ok(session) => {
                self.session = Some(session);
                self.live_gain = Some(mic_gain);
                self.live_gain_id = Some(gain_id);
                self.live_system_gain = Some(system_gain);
                self.live_system_gain_id = Some(system_gain_id);
                self.started = Some(Instant::now());
                self.last_second = u64::MAX;
                self.recording_kind = if camera.is_some() {
                    RecordingKind::Camera
                } else {
                    recording_kind_for(audio_only, region, recorded_format)
                };
                self.mode = Mode::Recording;
                self.recording_source = Some(source);
                self.update_state();
                self.sync_outline();
                platform::shell::start_timer(TIMER_ID, 200);
                // Twenty updates a second: the level bars read the peaks the
                // audio callbacks publish and keep their own peak hold.
                self.start_level_timer();
            }
            Err(error) => {
                self.live_gain = None;
                self.live_gain_id = None;
                self.live_system_gain = None;
                self.live_system_gain_id = None;
                self.recording_source = None;
                self.reset_levels();
                self.refresh_previews();
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
        if !self.window_shown() {
            self.stop_level_timer();
        }
        self.recording_source = None;
        self.reset_levels();
        self.live_gain = None;
        self.live_system_gain = None;
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
                        self.refresh_previews();
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
        self.live_system_gain = None;
        self.mode = Mode::Idle;
        self.saved_path = None;
        self.restore_window();
        self.update_state();
        self.refresh_previews();
        self.error(error);
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
                warn_once(&format!("copy file to clipboard failed: {error}"));
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
        // The dialog blocks the thread it is shown from (`zenity` on Linux waits
        // until the user closes it, and a hidden window offers nothing to
        // close). Reporting from the loop would freeze the capture panel while
        // the dialog is up, so it runs beside the loop and the state the caller
        // has already restored stays live.
        let text = text.to_string();
        std::thread::Builder::new()
            .name("CaptureButton-error".into())
            .spawn(move || platform::shell::show_error(&text))
            .ok();
    }

    fn conceal_for_ui(&mut self, from_window: bool) {
        if !hide_while_active(from_window, self.settings.hide_active) {
            return;
        }
        let visible = ui::visible();
        self.restore_visible = remember_restore(self.restore_visible, visible);
        ui::hide();
        self.stop_level_timer();
        self.close_previews();
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
            (ID_TRAY_OPEN, "Open window"),
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
    }

    fn show_window(&mut self) {
        platform::logging::info("window shown");
        platform::shell::restore_app_window();
        ui::show(&self.controls);
        self.start_level_timer();
        self.refresh_previews();
    }

    fn hide_window(&mut self) {
        // The window may be closed while a slider still has an unsaved value.
        self.read_ui(true);
        platform::logging::info("window hidden");
        ui::hide();
        self.stop_level_timer();
        self.close_previews();
        if self.mode == Mode::Idle {
            // Nothing is playing: give the working set back to the system. The
            // message loop still waits with zero wakeups, and the pages fault
            // back in only when the window is opened again.
            platform::shell::trim_working_set();
        }
    }

    fn request_exit(&mut self) {
        self.stop_level_timer();
        self.close_previews();
        if self.mode == Mode::Recording {
            self.quit_after_save = true;
            self.stop_capture();
            return;
        }
        if matches!(self.mode, Mode::Saving | Mode::Photo) {
            self.quit_after_save = true;
            return;
        }
        self.read_ui(true);
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
                ui::UiRequest::HotkeysChanged => {
                    self.read_ui(true);
                    self.apply_hotkeys();
                }
                ui::UiRequest::HotkeyChange(action) => {
                    self.read_ui(true);
                    self.change_hotkey(action);
                }
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
        if let Some(profile) = ui::profile_for_microphone(id) {
            self.gain_slider(profile, false, id, value, persist);
            return;
        }
        if let Some(profile) = ui::profile_for_system_gain(id) {
            self.gain_slider(profile, true, id, value, persist);
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
        self.read_ui(persist);
    }

    /// Moves one gain slider and decides whether the new value may be applied.
    ///
    /// A value up to 100% follows the drag and takes effect at once. Above 100%
    /// the slider still follows the pointer, but the sound keeps the gain that
    /// is already applied. The first release there keeps the value on the slider
    /// and asks: [`ID_GAIN_WARNING_OK`] applies it, and closing the warning any
    /// other way rolls the slider back to where the gesture started.
    fn gain_slider(
        &mut self,
        profile: ProfileRef,
        system: bool,
        id: i32,
        value: u32,
        persist: bool,
    ) {
        let value = value.min(ui::MIC_GAIN_MAX);
        let slot = profile.index();
        let kind = usize::from(system);
        if !persist {
            if self.gain_drag.is_none() {
                let audio = profile.audio(&self.settings);
                let previous = if system {
                    audio.system_gain
                } else {
                    audio.gain
                };
                self.gain_drag = Some((id, previous));
            }
            if value > UNITY_GAIN && self.gain_warned[slot][kind] {
                // Move the slider only: the sound keeps the gain that was
                // applied last until the warning is answered.
                if system {
                    self.controls.set_system_gain(id, value);
                } else {
                    self.controls.set_microphone_gain(id, value);
                }
                return;
            }
            self.set_gain(profile, system, id, value, false);
            self.read_ui(false);
            return;
        }
        let previous = match self.gain_drag.take() {
            Some((drag_id, previous)) if drag_id == id => previous,
            _ => value,
        };
        if gain_warning_step(&mut self.gain_warned[slot][kind], value) {
            let audio = profile.audio(&self.settings);
            let applied = if system {
                audio.system_gain
            } else {
                audio.gain
            };
            self.pending_gain = Some(PendingGain {
                profile,
                system,
                id,
                value,
                previous,
                applied,
            });
            self.controls.show_gain_warning(value);
            return;
        }
        self.set_gain(profile, system, id, value, true);
        self.read_ui(true);
    }

    /// Commits the gain the warning was holding back.
    fn accept_gain_warning(&mut self) {
        if let Some(pending) = self.pending_gain.take() {
            self.set_gain(
                pending.profile,
                pending.system,
                pending.id,
                pending.value,
                true,
            );
            self.read_ui(true);
        }
        self.controls.close_gain_warning();
    }

    /// Drops the held gain and rolls the slider back to where the drag started.
    fn dismiss_gain_warning(&mut self) {
        if let Some(pending) = self.pending_gain.take() {
            self.set_gain(
                pending.profile,
                pending.system,
                pending.id,
                pending.previous,
                false,
            );
            self.read_ui(false);
            // The slider is back at or below 100%, so the next crossing warns
            // again.
            self.gain_warned[pending.profile.index()][usize::from(pending.system)] =
                pending.previous <= UNITY_GAIN;
        }
        self.controls.close_gain_warning();
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
    platform::i18n::init(&Settings::load(platform::shell::hotkey_choice()).ui_language);
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

#[cfg(test)]
mod photo_tests {
    use super::*;

    /// A camera photo that was started and has not answered yet.
    fn running_photo() -> (Mode, Option<Receiver<Result<PathBuf, String>>>) {
        let (_worker, answer) = std::sync::mpsc::channel();
        (Mode::Photo, Some(answer))
    }

    /// Every button that starts a capture, and the tray rows that do the same.
    const CAPTURE_COMMANDS: [i32; 7] = [
        ID_SHOT_SCREEN,
        ID_SHOT_SOURCE,
        ID_SHOT_REGION,
        ID_TRAY_AREA,
        ID_TRAY_SHOT,
        ui::ID_CAMERA_PHOTO,
        ui::ID_CAMERA_VIDEO,
    ];

    #[test]
    fn a_photo_that_is_running_disables_every_capture_command() {
        let (mode, photo) = running_photo();
        assert!(photo.is_some());
        for id in CAPTURE_COMMANDS {
            assert!(!accepts_command(mode, id, false), "{id}");
        }
    }

    /// A camera failure reaches the loop as an `Err` from the worker, and the
    /// state must leave `Mode::Photo` even though a modal error dialog follows.
    #[test]
    fn a_failed_photo_returns_to_idle_and_reenables_every_capture_command() {
        let (mut mode, mut photo) = running_photo();
        end_photo(&mut mode, &mut photo);
        assert_eq!(mode, Mode::Idle);
        assert!(photo.is_none());
        for id in CAPTURE_COMMANDS {
            assert!(accepts_command(mode, id, false), "{id}");
            assert!(tray_item_enabled(mode, id), "{id}");
        }
    }

    fn monitor(id: &str, x: i32, width: u32, primary: bool) -> platform::screen::MonitorInfo {
        platform::screen::MonitorInfo {
            id: id.into(),
            name: id.into(),
            width,
            height: 800,
            primary,
            x,
            y: 0,
        }
    }

    #[test]
    fn a_full_screen_video_takes_the_chosen_monitor_but_an_area_keeps_its_rect() {
        let monitors = [
            monitor("LEFT", 0, 640, false),
            monitor("RIGHT", 640, 1280, true),
        ];
        assert_eq!(
            recording_rect(ProfileRef::Video, None, false, false, &monitors, "LEFT"),
            Some(Rect {
                x: 0,
                y: 0,
                w: 640,
                h: 800
            })
        );
        assert_eq!(
            recording_rect(ProfileRef::Video, None, false, false, &monitors, "auto"),
            Some(Rect {
                x: 640,
                y: 0,
                w: 1280,
                h: 800
            })
        );
        let area = Rect {
            x: 10,
            y: 20,
            w: 300,
            h: 200,
        };
        assert_eq!(
            recording_rect(
                ProfileRef::Video,
                Some(area),
                false,
                false,
                &monitors,
                "LEFT"
            ),
            Some(area)
        );
        assert_eq!(
            recording_rect(ProfileRef::Capture, None, false, false, &monitors, "LEFT"),
            None
        );
        assert_eq!(
            recording_rect(ProfileRef::Video, None, true, false, &monitors, "LEFT"),
            None
        );
    }
}
