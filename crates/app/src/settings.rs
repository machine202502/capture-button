//! The settings model. Persistence goes through `platform::store`, so this
//! file stays OS-neutral; the backend decides where the values live.

use crate::media::{self, AudioSettings, Format, VideoSettings};
use platform::shell::{hotkey_defs, HotkeyAction, HotkeyBinding, HotkeyChoice};
use platform::store::Store;

#[derive(Clone)]
pub struct CaptureSettings {
    pub format: Format,
    pub video: [VideoSettings; 3],
    pub audio: AudioSettings,
    pub cursor: bool,
    /// Monitor id for a full-screen recording; `"auto"` follows the primary.
    pub monitor: String,
}

impl CaptureSettings {
    pub fn new(format: Format) -> Self {
        Self {
            format,
            video: [
                VideoSettings {
                    height: 1080,
                    fps: 60,
                },
                VideoSettings {
                    height: 480,
                    fps: 30,
                },
                VideoSettings {
                    height: 480,
                    fps: 30,
                },
            ],
            audio: AudioSettings::default(),
            cursor: true,
            monitor: "auto".into(),
        }
    }

    fn profile_for(&self, format: Format) -> crate::media::Profile {
        crate::media::Profile {
            format,
            video: self.video[format.index()],
            audio: self.audio.clone(),
            cursor: self.cursor,
        }
    }

    fn as_recording(&self) -> RecordingProfile {
        self.as_recording_for(self.format)
    }

    fn as_recording_for(&self, format: Format) -> RecordingProfile {
        let picture = self.profile_for(format);
        RecordingProfile::Picture {
            format: picture.format,
            video: picture.video,
            audio: picture.audio,
            cursor: picture.cursor,
        }
    }
}

/// Which of the four saved profiles a recording starts from.
///
/// Capture, area, and video each remember a picture and the audio that goes with it.
/// Audio remembers only audio.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileRef {
    Capture,
    Area,
    Video,
    Audio,
}

impl ProfileRef {
    pub const ALL: [Self; 4] = [Self::Capture, Self::Area, Self::Video, Self::Audio];

    /// The slot this profile has in the app's per-profile level and preview
    /// tables, in `ALL` order.
    pub fn index(self) -> usize {
        match self {
            Self::Capture => 0,
            Self::Area => 1,
            Self::Video => 2,
            Self::Audio => 3,
        }
    }

    /// The audio settings of this profile.
    pub fn audio(self, settings: &Settings) -> &AudioSettings {
        match self {
            Self::Capture => &settings.ui.audio,
            Self::Area => &settings.tray_area.audio,
            Self::Video => &settings.tray_video.audio,
            Self::Audio => &settings.tray_audio,
        }
    }
}

/// What one recording is made of. An audio recording has no picture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordingProfile {
    Picture {
        format: Format,
        video: VideoSettings,
        audio: AudioSettings,
        cursor: bool,
    },
    Audio(AudioSettings),
}

impl RecordingProfile {
    pub fn audio(&self) -> &AudioSettings {
        match self {
            Self::Picture { audio, .. } | Self::Audio(audio) => audio,
        }
    }

    pub fn video(&self) -> Option<VideoSettings> {
        match self {
            Self::Picture { video, .. } => Some(*video),
            Self::Audio(_) => None,
        }
    }

    /// Drop the picture and keep this profile's audio. An audio profile stays as it is.
    pub fn without_picture(self) -> Self {
        match self {
            Self::Picture { audio, .. } | Self::Audio(audio) => Self::Audio(audio),
        }
    }

    /// Split into the encoder profile and whether the picture is recorded.
    /// The placeholder picture on an audio recording is not taken from any saved profile.
    pub fn session(self) -> (crate::media::Profile, bool) {
        match self {
            Self::Picture {
                format,
                video,
                audio,
                cursor,
            } => (
                crate::media::Profile {
                    format,
                    video,
                    audio,
                    cursor,
                },
                false,
            ),
            Self::Audio(audio) => (
                crate::media::Profile {
                    format: Format::Mkv,
                    video: VideoSettings { height: 0, fps: 30 },
                    audio,
                    cursor: false,
                },
                true,
            ),
        }
    }
}

#[derive(Clone)]
pub struct Settings {
    pub ui: CaptureSettings,
    pub tray_area: CaptureSettings,
    pub tray_video: CaptureSettings,
    pub tray_audio: AudioSettings,
    pub autostart: bool,
    pub start_minimized: bool,
    pub hide_active: bool,
    pub close_tray: bool,
    pub clipboard: bool,
    pub tray_capture: bool,
    pub highlight_region: bool,
    pub completion_sound: bool,
    /// Volume in percent for both sounds; 100 is the level of the WAV assets.
    pub sound_volume: u32,
    pub screenshot_sound: bool,
    /// Whether the volume sliders show a live level while the window is idle.
    /// It keeps one metering-only stream per enabled input open.
    pub show_idle_level: bool,
    pub show_no_signal: bool,
    pub show_card: bool,
    pub threads: u32,
    /// Which renderer draws the area-selection overlay: `0` is the native
    /// overlay (GDI on Windows), `1` the Slint one. Hidden from the Settings
    /// window; it is set through the store and picked up on the next start.
    pub region_renderer: u32,
    /// Interface language code; empty follows the Windows language.
    pub ui_language: String,
    /// The saved combination of every hotkey action, in `HOTKEY_DEFS` order.
    /// An empty combination means the action is unbound.
    pub hotkeys: Vec<HotkeyBinding>,
    /// Whether the desktop's hotkey dialog has already been answered once on a
    /// session that owns the combinations. It keeps a deliberate "no hotkeys"
    /// from turning back into a proposal at every start.
    pub hotkeys_configured: bool,
    /// Folders chosen for each kind of file; empty slots use the default folder.
    pub folders: media::OutputFolders,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            ui: CaptureSettings::new(Format::Mkv),
            tray_area: CaptureSettings::new(Format::Gif),
            tray_video: CaptureSettings::new(Format::Mkv),
            tray_audio: AudioSettings::default(),
            autostart: false,
            start_minimized: false,
            hide_active: false,
            close_tray: false,
            clipboard: true,
            tray_capture: true,
            highlight_region: true,
            completion_sound: true,
            sound_volume: 50,
            screenshot_sound: true,
            show_idle_level: true,
            show_no_signal: true,
            show_card: true,
            threads: 0,
            region_renderer: u32::from(platform::region::default_renderer().id),
            ui_language: String::new(),
            hotkeys: hotkey_defs()
                .iter()
                .map(|def| HotkeyBinding::unset(def.action))
                .collect(),
            hotkeys_configured: false,
            folders: media::OutputFolders::EMPTY,
        }
    }
}

impl Settings {
    /// Loads the settings. `choice` says how this session chooses the global
    /// combinations: a combination the desktop owns is kept as the desktop
    /// reported it, while a field-captured one is normalized to its canonical
    /// label and dropped when it no longer parses.
    pub fn load(choice: HotkeyChoice) -> Self {
        let mut settings = Self::default();
        let Some(registry) = Store::open() else {
            return settings;
        };
        settings.ui_language = registry.string("ui_language", "");
        settings.hotkeys_configured = registry.dword("hotkey_configured", 0) != 0;
        // The per-action keys are the same ones earlier versions wrote
        // (`hotkey_area`, `hotkey_stop`), so an upgrade keeps them.
        settings.hotkeys = hotkey_defs()
            .iter()
            .map(|def| HotkeyBinding {
                action: def.action,
                combination: saved_hotkey(choice, registry.string(def.settings_key, "")),
            })
            .collect();
        settings.ui.format = Format::from_index(registry.dword("format", 0) as usize);
        for (index, name) in ["mkv", "gif", "webp"].iter().enumerate() {
            settings.ui.video[index].height =
                registry.dword(&format!("{name}_height"), settings.ui.video[index].height);
            settings.ui.video[index].fps = registry
                .dword(&format!("{name}_fps"), settings.ui.video[index].fps)
                .clamp(5, 60);
        }
        load_audio(&registry, "ui", &mut settings.ui.audio);
        settings.ui.cursor = registry.dword("ui_cursor", 1) != 0;
        settings.ui.monitor = registry.string("ui_monitor", "auto");
        load_profile(&registry, "area", &mut settings.tray_area);
        load_profile(&registry, "video", &mut settings.tray_video);
        // Tray video has no format choice anymore: it is always MKV. Tray area
        // remembers one picture setting per format; the three toggle buttons in
        // Settings choose which one is being edited.
        settings.tray_video.format = Format::Mkv;
        load_audio(&registry, "audio", &mut settings.tray_audio);
        settings.autostart = registry.dword("autostart", 0) != 0;
        settings.start_minimized = registry.dword("start_minimized", 0) != 0;
        settings.hide_active = registry.dword("hide_active", 0) != 0;
        settings.close_tray = registry.dword("close_tray", 0) != 0;
        settings.clipboard = registry.dword("clipboard", 1) != 0;
        settings.tray_capture = registry.dword("tray_capture", 1) != 0;
        settings.highlight_region = registry.dword("highlight_region", 1) != 0;
        settings.completion_sound = registry.dword("completion_sound", 1) != 0;
        settings.sound_volume = registry.dword("sound_volume", 50).min(100);
        settings.screenshot_sound = registry.dword("screenshot_sound", 1) != 0;
        settings.show_idle_level = registry.dword("idle_level", 1) != 0;
        settings.show_no_signal = registry.dword("show_no_signal", 1) != 0;
        settings.show_card = registry.dword("show_card", 1) != 0;
        settings.threads = registry.dword("threads", 0);
        settings.region_renderer = registry.dword(
            "region_renderer",
            u32::from(platform::region::default_renderer().id),
        );
        for category in media::OutputCategory::ALL {
            let value = registry.string(folder_key(category), "");
            if !value.is_empty() {
                settings
                    .folders
                    .set(category, Some(std::path::PathBuf::from(value)));
            }
        }
        for source in ProfileRef::ALL {
            validate_audio_device(settings.audio_mut(source));
        }
        let system_sources = platform::audio::system_sources();
        for source in ProfileRef::ALL {
            validate_system_device(settings.audio_mut(source), &system_sources);
        }
        settings
    }

    pub fn recording(&self, source: ProfileRef) -> RecordingProfile {
        match source {
            ProfileRef::Capture => self.ui.as_recording(),
            ProfileRef::Area => self.tray_area.as_recording(),
            // Tray video has no format choice; an old saved format must not leak in.
            ProfileRef::Video => self.tray_video.as_recording_for(Format::Mkv),
            ProfileRef::Audio => RecordingProfile::Audio(self.tray_audio.clone()),
        }
    }

    /// The same profile with its picture forced to `format` for one recording. The
    /// saved format and the per-format picture settings stay where they are.
    pub fn recording_as(&self, source: ProfileRef, format: Format) -> RecordingProfile {
        match source {
            ProfileRef::Capture => self.ui.as_recording_for(format),
            ProfileRef::Area => self.tray_area.as_recording_for(format),
            ProfileRef::Video => self.tray_video.as_recording_for(Format::Mkv),
            ProfileRef::Audio => RecordingProfile::Audio(self.tray_audio.clone()),
        }
    }

    /// The profile a start button applies. An audio profile has no picture to keep,
    /// so asking it for one still returns audio only.
    pub fn applied(&self, source: ProfileRef, picture: bool) -> RecordingProfile {
        let profile = self.recording(source);
        if picture {
            profile
        } else {
            profile.without_picture()
        }
    }

    pub fn applied_as(
        &self,
        source: ProfileRef,
        picture: bool,
        format: Format,
    ) -> RecordingProfile {
        let profile = self.recording_as(source, format);
        if picture {
            profile
        } else {
            profile.without_picture()
        }
    }

    pub fn audio_mut(&mut self, source: ProfileRef) -> &mut AudioSettings {
        match source {
            ProfileRef::Capture => &mut self.ui.audio,
            ProfileRef::Area => &mut self.tray_area.audio,
            ProfileRef::Video => &mut self.tray_video.audio,
            ProfileRef::Audio => &mut self.tray_audio,
        }
    }

    /// The saved combination of `action`; empty means unbound.
    pub fn hotkey(&self, action: HotkeyAction) -> &str {
        self.hotkeys
            .iter()
            .find(|binding| binding.action == action)
            .map(|binding| binding.combination.as_str())
            .unwrap_or_default()
    }

    /// Stores the combination of `action`, adding its row when the settings do
    /// not have one yet.
    pub fn set_hotkey(&mut self, action: HotkeyAction, combination: String) {
        match self
            .hotkeys
            .iter_mut()
            .find(|binding| binding.action == action)
        {
            Some(binding) => binding.combination = combination,
            None => self.hotkeys.push(HotkeyBinding {
                action,
                combination,
            }),
        }
    }

    /// Adopts a set the backend reported, leaving other actions untouched.
    pub fn set_hotkeys(&mut self, bindings: &[HotkeyBinding]) {
        for binding in bindings {
            self.set_hotkey(binding.action, binding.combination.clone());
        }
    }

    pub fn save(&self) {
        let Some(registry) = Store::open() else {
            return;
        };
        registry.set_dword("format", self.ui.format.index() as u32);
        for (index, name) in ["mkv", "gif", "webp"].iter().enumerate() {
            registry.set_dword(&format!("{name}_height"), self.ui.video[index].height);
            registry.set_dword(&format!("{name}_fps"), self.ui.video[index].fps);
        }
        save_audio(&registry, "ui", &self.ui.audio);
        registry.set_dword("ui_cursor", self.ui.cursor as u32);
        registry.set_string("ui_monitor", &self.ui.monitor);
        save_profile(&registry, "area", &self.tray_area);
        save_profile(&registry, "video", &self.tray_video);
        save_audio(&registry, "audio", &self.tray_audio);
        registry.set_dword("autostart", self.autostart as u32);
        registry.set_dword("start_minimized", self.start_minimized as u32);
        registry.set_dword("hide_active", self.hide_active as u32);
        registry.set_dword("close_tray", self.close_tray as u32);
        registry.set_dword("clipboard", self.clipboard as u32);
        registry.set_dword("tray_capture", self.tray_capture as u32);
        registry.set_dword("highlight_region", self.highlight_region as u32);
        registry.set_dword("completion_sound", self.completion_sound as u32);
        registry.set_dword("sound_volume", self.sound_volume.min(100));
        registry.set_dword("screenshot_sound", self.screenshot_sound as u32);
        registry.set_dword("idle_level", self.show_idle_level as u32);
        registry.set_dword("show_no_signal", self.show_no_signal as u32);
        registry.set_dword("show_card", self.show_card as u32);
        registry.set_dword("region_renderer", self.region_renderer);
        registry.set_dword("threads", self.threads);
        registry.set_string("ui_language", &self.ui_language);
        registry.set_dword("hotkey_configured", self.hotkeys_configured as u32);
        for def in hotkey_defs() {
            let combination = self.hotkey(def.action);
            registry.set_string(def.settings_key, combination);
        }
        for category in media::OutputCategory::ALL {
            let value = self
                .folders
                .get(category)
                .map(|path| path.display().to_string())
                .unwrap_or_default();
            registry.set_string(folder_key(category), &value);
        }
        platform::store::update_autostart(self.autostart, self.start_minimized);
    }
}

fn folder_key(category: media::OutputCategory) -> &'static str {
    match category {
        media::OutputCategory::Screenshots => "folder_screenshots",
        media::OutputCategory::Video => "folder_video",
        media::OutputCategory::Camera => "folder_camera",
        media::OutputCategory::Sound => "folder_sound",
        media::OutputCategory::Default => "folder_default",
    }
}

fn load_audio(registry: &Store, prefix: &str, audio: &mut AudioSettings) {
    audio.system = registry.dword(&format!("{prefix}_system"), 1) != 0;
    audio.microphone = registry.dword(&format!("{prefix}_microphone"), 1) != 0;
    audio.device = registry.string(&format!("{prefix}_device"), "Auto");
    audio.gain = registry
        .dword(&format!("{prefix}_gain"), audio.gain)
        .clamp(0, media::GAIN_MAX);
    audio.system_device = registry.string(&format!("{prefix}_system_device"), "auto");
    audio.system_gain = registry
        .dword(&format!("{prefix}_system_gain"), audio.system_gain)
        .clamp(0, media::GAIN_MAX);
}

fn save_audio(registry: &Store, prefix: &str, audio: &AudioSettings) {
    registry.set_dword(&format!("{prefix}_system"), audio.system as u32);
    registry.set_dword(&format!("{prefix}_microphone"), audio.microphone as u32);
    registry.set_string(&format!("{prefix}_device"), &audio.device);
    registry.set_dword(&format!("{prefix}_gain"), audio.gain);
    registry.set_string(&format!("{prefix}_system_device"), &audio.system_device);
    registry.set_dword(&format!("{prefix}_system_gain"), audio.system_gain);
}

fn validate_audio_device(audio: &mut AudioSettings) {
    if audio.device != "Auto" && media::device_name(&audio.device).is_none() {
        audio.device = "Auto".into();
    }
}

/// Keeps a saved sink that the current server still knows. An empty source list
/// means the backend cannot enumerate (there is no PulseAudio/PipeWire server,
/// or the Windows enumeration is not implemented yet), so the saved id is left
/// alone instead of being wiped by a temporary outage.
fn validate_system_device(audio: &mut AudioSettings, sources: &[platform::audio::AudioSource]) {
    if sources.is_empty() || audio.system_device == "auto" {
        return;
    }
    if !sources
        .iter()
        .any(|source| source.id == audio.system_device)
    {
        audio.system_device = "auto".into();
    }
}

/// Drops a saved hotkey the shared parser no longer accepts, so the UI never
/// shows text that the backend cannot register.
fn valid_hotkey(text: String) -> String {
    if text.trim().is_empty() {
        return String::new();
    }
    platform::shell::parse_hotkey(&text)
        .map(|hotkey| hotkey.label())
        .unwrap_or_default()
}

/// The text a saved hotkey keeps on this session.
fn saved_hotkey(choice: HotkeyChoice, text: String) -> String {
    let text = text.trim();
    if text.is_empty() {
        return String::new();
    }
    match choice {
        // The desktop owns the combination and reports a description meant to
        // be rendered, so it is stored and shown exactly as reported.
        HotkeyChoice::SystemDialog => text.to_string(),
        HotkeyChoice::Field => valid_hotkey(text.to_string()),
    }
}

fn load_profile(registry: &Store, prefix: &str, profile: &mut CaptureSettings) {
    profile.format = Format::from_index(
        registry.dword(&format!("{prefix}_format"), profile.format.index() as u32) as usize,
    );
    for (index, name) in ["mkv", "gif", "webp"].iter().enumerate() {
        let legacy_height = if profile.format.index() == index {
            registry.dword(&format!("{prefix}_height"), profile.video[index].height)
        } else {
            profile.video[index].height
        };
        let legacy_fps = if profile.format.index() == index {
            registry.dword(&format!("{prefix}_fps"), profile.video[index].fps)
        } else {
            profile.video[index].fps
        };
        profile.video[index].height =
            registry.dword(&format!("{prefix}_{name}_height"), legacy_height);
        profile.video[index].fps = registry
            .dword(&format!("{prefix}_{name}_fps"), legacy_fps)
            .clamp(5, 60);
    }
    load_audio(registry, prefix, &mut profile.audio);
    profile.cursor = registry.dword(&format!("{prefix}_cursor"), 1) != 0;
    profile.monitor = registry.string(&format!("{prefix}_monitor"), "auto");
}

fn save_profile(registry: &Store, prefix: &str, profile: &CaptureSettings) {
    registry.set_dword(&format!("{prefix}_format"), profile.format.index() as u32);
    for (index, name) in ["mkv", "gif", "webp"].iter().enumerate() {
        registry.set_dword(
            &format!("{prefix}_{name}_height"),
            profile.video[index].height,
        );
        registry.set_dword(&format!("{prefix}_{name}_fps"), profile.video[index].fps);
    }
    save_audio(registry, prefix, &profile.audio);
    registry.set_dword(&format!("{prefix}_cursor"), profile.cursor as u32);
    registry.set_string(&format!("{prefix}_monitor"), &profile.monitor);
}

pub fn load_history(limit: usize) -> Vec<(std::path::PathBuf, String)> {
    let Some(registry) = Store::open() else {
        return Vec::new();
    };
    decode_history(&registry.string("history", ""), limit)
}

pub fn save_history(items: &[(std::path::PathBuf, String)]) {
    let Some(registry) = Store::open() else {
        return;
    };
    registry.set_string("history", &encode_history(items));
}

pub fn encode_history(items: &[(std::path::PathBuf, String)]) -> String {
    items
        .iter()
        .map(|(path, stamp)| format!("{stamp}\u{1f}{}", path.display()))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn decode_history(text: &str, limit: usize) -> Vec<(std::path::PathBuf, String)> {
    text.lines()
        .filter_map(|line| {
            let (stamp, path) = line.split_once('\u{1f}')?;
            if stamp.is_empty() || path.is_empty() {
                return None;
            }
            Some((std::path::PathBuf::from(path), stamp.to_string()))
        })
        .take(limit)
        .collect()
}

#[cfg(test)]
mod recording_profiles {
    use super::{ProfileRef, Settings};
    use crate::media::{Format, VideoSettings};
    use platform::shell::{hotkey_defs, HotkeyAction, HotkeyBinding, HotkeyChoice};

    fn picture(height: u32, fps: u32) -> VideoSettings {
        VideoSettings { height, fps }
    }

    #[test]
    fn sounds_start_at_half_volume() {
        assert_eq!(Settings::default().sound_volume, 50);
    }

    #[test]
    fn video_records_the_primary_monitor_until_another_one_is_chosen() {
        let mut settings = Settings::default();
        assert_eq!(settings.tray_video.monitor, "auto");
        settings.tray_video.monitor = "DP-1".into();
        assert_eq!(settings.tray_video.monitor, "DP-1");
        assert_eq!(settings.tray_area.monitor, "auto");
    }

    #[test]
    fn system_audio_starts_at_unity_with_the_default_output() {
        let settings = Settings::default();
        for source in ProfileRef::ALL {
            let audio = settings.recording(source).audio().clone();
            assert_eq!(audio.system_device, "auto", "{source:?}");
            assert_eq!(audio.system_gain, 100, "{source:?}");
            assert_eq!(audio.gain, 100, "{source:?}");
        }
    }

    #[test]
    fn the_system_and_microphone_volumes_are_independent() {
        let mut settings = Settings::default();
        settings.ui.audio.gain = 120;
        settings.ui.audio.system_gain = 40;
        assert_eq!(settings.ui.audio.gain, 120);
        assert_eq!(settings.ui.audio.system_gain, 40);
        settings.ui.audio.system_gain = 200;
        assert_eq!(settings.ui.audio.gain, 120, "the microphone is untouched");
        settings.ui.audio.gain = 0;
        assert_eq!(
            settings.ui.audio.system_gain, 200,
            "the system audio is untouched"
        );
    }

    #[test]
    fn an_invalid_hotkey_is_not_kept() {
        assert_eq!(super::valid_hotkey("Ctrl+Shift+A".into()), "Ctrl+Shift+A");
        assert_eq!(super::valid_hotkey("ctrl+f9".into()), "Ctrl+F9");
        assert_eq!(super::valid_hotkey("".into()), "");
        assert_eq!(super::valid_hotkey("Shift+A".into()), "");
        assert_eq!(super::valid_hotkey("Ctrl+Nonsense".into()), "");
    }

    #[test]
    fn a_hotkey_the_desktop_owns_is_kept_as_reported() {
        let reported = "Press <Shift><Control>F12";
        assert_eq!(
            super::saved_hotkey(HotkeyChoice::SystemDialog, reported.into()),
            reported
        );
        assert_eq!(
            super::saved_hotkey(HotkeyChoice::SystemDialog, "   ".into()),
            ""
        );
        // A field-captured setting still drops what the parser rejects, so the
        // fallback path never shows text it cannot register.
        assert_eq!(
            super::saved_hotkey(HotkeyChoice::Field, reported.into()),
            ""
        );
        assert_eq!(
            super::saved_hotkey(HotkeyChoice::Field, "ctrl+f12".into()),
            "Ctrl+F12"
        );
    }

    #[test]
    fn a_hotkey_is_stored_per_action_from_the_table() {
        let mut settings = Settings::default();
        for def in hotkey_defs() {
            assert_eq!(settings.hotkey(def.action), "", "every row starts unset");
        }
        settings.set_hotkey(HotkeyAction::Stop, "Alt+F9".into());
        assert_eq!(settings.hotkey(HotkeyAction::Stop), "Alt+F9");
        assert_eq!(settings.hotkey(HotkeyAction::Area), "");
        // A reported set replaces the rows it names and keeps the others.
        settings.set_hotkeys(&[HotkeyBinding {
            action: HotkeyAction::Area,
            combination: "Ctrl+Shift+F12".into(),
        }]);
        assert_eq!(settings.hotkey(HotkeyAction::Area), "Ctrl+Shift+F12");
        assert_eq!(settings.hotkey(HotkeyAction::Stop), "Alt+F9");
    }

    #[test]
    fn each_picture_profile_uses_its_own_format_and_microphone() {
        let mut settings = Settings::default();
        settings.ui.format = Format::Gif;
        settings.ui.video[Format::Gif.index()] = picture(720, 24);
        settings.ui.audio.gain = 110;
        settings.tray_area.format = Format::Webp;
        settings.tray_area.video[Format::Webp.index()] = picture(480, 15);
        settings.tray_area.audio.gain = 120;
        settings.tray_video.format = Format::Mkv;
        settings.tray_video.video[Format::Mkv.index()] = picture(1080, 60);
        settings.tray_video.audio.gain = 130;
        settings.tray_audio.gain = 140;

        let capture = settings.applied(ProfileRef::Capture, true);
        let (capture_session, capture_audio_only) = capture.clone().session();
        assert!(!capture_audio_only);
        assert_eq!(capture_session.format, Format::Gif);
        assert_eq!(capture.video(), Some(picture(720, 24)));
        assert_eq!(capture.audio().gain, 110);
        assert!(capture_session.cursor);
        settings.ui.cursor = false;
        settings.tray_area.cursor = true;
        let capture_picture = settings.applied(ProfileRef::Capture, true).session().0;
        let area_picture = settings.recording(ProfileRef::Area).session().0;
        assert!(!capture_picture.cursor);
        assert!(area_picture.cursor);

        let capture_audio = settings.applied(ProfileRef::Capture, false);
        let (_, sound_only) = capture_audio.clone().session();
        assert!(sound_only);
        assert_eq!(capture_audio.video(), None);
        assert_eq!(capture_audio.audio().gain, 110);

        let area = settings.recording(ProfileRef::Area);
        let (area_session, area_audio_only) = area.clone().session();
        assert!(!area_audio_only);
        assert_eq!(area_session.format, Format::Webp);
        assert_eq!(area.video(), Some(picture(480, 15)));
        assert_eq!(area.audio().gain, 120);

        let video = settings.recording(ProfileRef::Video);
        let (video_session, video_audio_only) = video.clone().session();
        assert!(!video_audio_only);
        assert_eq!(video.video(), Some(picture(1080, 60)));
        assert_eq!(video_session.video, picture(1080, 60));
        assert_eq!(video.audio().gain, 130);

        let audio = settings.applied(ProfileRef::Audio, true);
        let (_, audio_only) = audio.clone().session();
        assert!(audio_only);
        assert_eq!(audio.video(), None);
        assert_eq!(audio.audio().gain, 140);
    }

    #[test]
    fn every_picture_profile_keeps_its_own_pointer_for_recording() {
        let mut settings = Settings::default();
        settings.ui.cursor = true;
        settings.tray_area.cursor = false;
        settings.tray_video.cursor = true;

        let capture = settings.applied(ProfileRef::Capture, true).session().0;
        let area = settings.applied(ProfileRef::Area, true).session().0;
        let video = settings.applied(ProfileRef::Video, true).session().0;
        assert!(capture.cursor);
        assert!(!area.cursor);
        assert!(video.cursor);
    }

    #[test]
    fn forcing_a_format_uses_that_formats_picture_without_saving_it() {
        let mut settings = Settings::default();
        settings.tray_area.format = Format::Gif;
        settings.tray_area.video[Format::Gif.index()] = picture(480, 15);
        settings.tray_area.video[Format::Mkv.index()] = picture(1080, 60);
        settings.tray_area.audio.gain = 120;
        settings.tray_area.cursor = false;

        let (forced, audio_only) = settings
            .applied_as(ProfileRef::Area, true, Format::Mkv)
            .session();
        assert!(!audio_only);
        assert_eq!(forced.format, Format::Mkv);
        assert_eq!(forced.video, picture(1080, 60));
        assert_eq!(forced.audio.gain, 120);
        assert!(!forced.cursor);

        assert_eq!(settings.tray_area.format, Format::Gif);
        assert_eq!(
            settings.tray_area.video[Format::Mkv.index()],
            picture(1080, 60)
        );
    }

    #[test]
    fn tray_video_records_mkv_even_with_a_stale_saved_format() {
        let mut settings = Settings::default();
        settings.tray_video.format = Format::Gif;
        settings.tray_video.video[Format::Mkv.index()] = picture(1080, 60);

        let (profile, audio_only) = settings.applied(ProfileRef::Video, true).session();
        assert!(!audio_only);
        assert_eq!(profile.format, Format::Mkv);
        assert_eq!(profile.video, picture(1080, 60));
    }

    #[test]
    fn every_area_format_keeps_its_own_picture() {
        let mut settings = Settings::default();
        settings.tray_area.video[Format::Mkv.index()] = picture(1080, 60);
        settings.tray_area.video[Format::Gif.index()] = picture(480, 15);
        settings.tray_area.video[Format::Webp.index()] = picture(720, 25);

        for (format, expected) in [
            (Format::Mkv, picture(1080, 60)),
            (Format::Gif, picture(480, 15)),
            (Format::Webp, picture(720, 25)),
        ] {
            let (profile, audio_only) = settings
                .applied_as(ProfileRef::Area, true, format)
                .session();
            assert!(!audio_only);
            assert_eq!(profile.format, format);
            assert_eq!(profile.video, expected);
        }
    }

    #[test]
    fn an_audio_recording_does_not_borrow_another_profiles_picture() {
        let mut tall = Settings::default();
        tall.tray_video.video[Format::Mkv.index()] = picture(2160, 60);
        tall.tray_audio.system = false;
        tall.tray_audio.gain = 180;
        let mut small = Settings::default();
        small.tray_video.video[Format::Mkv.index()] = picture(360, 15);
        small.tray_audio.system = false;
        small.tray_audio.gain = 180;

        let (tall_session, tall_audio_only) = tall.applied(ProfileRef::Audio, true).session();
        let (small_session, small_audio_only) = small.applied(ProfileRef::Audio, false).session();
        assert!(tall_audio_only && small_audio_only);
        assert_eq!(tall_session.video, small_session.video);
        assert_ne!(tall_session.video.height, 2160);
        assert_eq!(tall_session.audio.gain, 180);
        assert!(!tall_session.audio.system);

        let (video, audio_only) = tall.applied(ProfileRef::Video, true).session();
        assert!(!audio_only);
        assert_eq!(video.video, picture(2160, 60));
        assert_ne!(video.audio.gain, 180);
    }
}

#[cfg(test)]
mod history_store {
    use super::{decode_history, encode_history};
    use std::path::PathBuf;

    #[test]
    fn the_visible_history_survives_a_restart() {
        let items = vec![
            (
                PathBuf::from(r"C:\Pictures\CaptureButton\Video_1.mkv"),
                "4 Nov 2026, 22:56:07".into(),
            ),
            (
                PathBuf::from(r"C:\Pictures\CaptureButton\Shot 2.png"),
                "4 Nov 2026, 22:01:00".into(),
            ),
        ];
        let restored = decode_history(&encode_history(&items), 12);
        assert_eq!(restored, items);
        let extra: Vec<_> = (0..20)
            .map(|index| {
                (
                    PathBuf::from(format!("file-{index}.png")),
                    format!("stamp-{index}"),
                )
            })
            .collect();
        assert_eq!(decode_history(&encode_history(&extra), 12).len(), 12);
    }
}
