use super::*;

#[test]
fn each_start_place_records_with_the_profile_tied_to_it() {
    use super::media::VideoSettings;

    let mut settings = Settings::default();
    settings.ui.format = Format::Gif;
    settings.ui.video[Format::Gif.index()] = VideoSettings {
        height: 360,
        fps: 12,
    };
    settings.ui.audio.device = "capture".into();
    settings.ui.audio.gain = 11;
    settings.tray_area.format = Format::Webp;
    settings.tray_area.video[Format::Webp.index()] = VideoSettings {
        height: 480,
        fps: 18,
    };
    settings.tray_area.audio.device = "area".into();
    settings.tray_area.audio.gain = 22;
    settings.tray_video.format = Format::Mkv;
    settings.tray_video.video[Format::Mkv.index()] = VideoSettings {
        height: 1440,
        fps: 48,
    };
    settings.tray_video.audio.device = "video".into();
    settings.tray_video.audio.gain = 33;
    settings.tray_audio.device = "audio".into();
    settings.tray_audio.gain = 44;

    let started = [
        (ID_CAPTURE_SCREEN, "capture", Some((360, 12))),
        (ID_CAPTURE_SOURCE, "capture", Some((360, 12))),
        (ID_CAPTURE_REGION, "capture", Some((360, 12))),
        (ID_CAPTURE_AUDIO, "capture", None),
        (ID_SHOT_REGION, "capture", Some((360, 12))),
        (ID_TRAY_AREA, "area", Some((480, 18))),
        (ID_TRAY_VIDEO, "video", Some((1440, 48))),
        (ID_TRAY_AUDIO, "audio", None),
    ];
    for (id, device, video) in started {
        let place =
            command_place(id).unwrap_or_else(|| panic!("control {id} does not start a recording"));
        let (source, take) = start_plan(place);
        let profile = settings.applied(source, take == Take::Picture);
        assert_eq!(profile.audio().device, device, "{place:?}");
        assert_eq!(
            profile.audio().gain,
            match device {
                "capture" => 11,
                "area" => 22,
                "video" => 33,
                "audio" => 44,
                _ => 0,
            }
        );
        assert_eq!(
            profile.video().map(|item| (item.height, item.fps)),
            video,
            "{place:?}"
        );
    }
    assert!(command_place(ID_TRAY_SHOT).is_none());
    assert!(command_place(ID_TRAY_STOP).is_none());

    assert_eq!(
        overlay_for(StartPlace::CaptureRegion),
        region::Overlay::Capture
    );
    assert_eq!(
        overlay_for(StartPlace::ShotRegion),
        region::Overlay::Screenshot
    );
    assert_eq!(overlay_for(StartPlace::TrayArea), region::Overlay::Tray);
    assert!(!region::Overlay::Capture.shows_video());
    assert!(!region::Overlay::Screenshot.shows_video());
    assert!(region::Overlay::Tray.shows_video());
    let (full_screen, take) = start_plan(area_video_place());
    let profile = settings.applied(full_screen, take == Take::Picture);
    assert_eq!(full_screen, ProfileRef::Video);
    assert_eq!(profile.audio().device, "video");
    assert_eq!(
        profile.video().map(|item| (item.height, item.fps)),
        Some((1440, 48))
    );

    for source in [ProfileRef::Capture, ProfileRef::Area] {
        let (resolved, take) = start_plan(place_for_source(source));
        let profile = settings.applied(resolved, take == Take::Picture);
        let device = if source == ProfileRef::Area {
            "area"
        } else {
            "capture"
        };
        assert_eq!(resolved, source);
        assert_eq!(take, Take::Picture);
        assert_eq!(profile.audio().device, device, "{source:?}");
    }
}

#[test]
fn tray_badge_stays_red_while_recording_and_orange_while_saving() {
    let recording = theme::bgra(theme::BADGE_RECORDING);
    let saving = theme::bgra(theme::BADGE_SAVING);
    assert!(
        recording[2] > 200 && recording[0] < 60 && recording[1] < 80,
        "recording badge is red, got {recording:?}"
    );
    assert!(
        saving[2] > 200 && saving[1] > 100 && saving[0] < 60,
        "saving badge is orange, got {saving:?}"
    );
    assert_eq!(recording, [46, 59, 226, 255]);
    assert_eq!(theme::bgra(theme::BADGE_RECORDING_EDGE), [18, 26, 142, 255]);
    assert_eq!(saving, [36, 154, 242, 255]);
    assert_eq!(theme::bgra(theme::BADGE_SAVING_EDGE), [12, 92, 165, 255]);
}

#[test]
fn the_tray_tip_names_area_video_and_audio() {
    assert_eq!(
        recording_kind_for(false, false, Format::Mkv),
        RecordingKind::Video
    );
    assert_eq!(
        recording_kind_for(false, false, Format::Gif),
        RecordingKind::Animation
    );
    assert_eq!(
        recording_kind_for(false, false, Format::Webp),
        RecordingKind::Animation
    );
    assert_eq!(
        recording_kind_for(false, true, Format::Mkv),
        RecordingKind::AreaVideo
    );
    assert_eq!(
        recording_kind_for(false, true, Format::Gif),
        RecordingKind::AreaAnimation
    );
    assert_eq!(
        recording_kind_for(false, true, Format::Webp),
        RecordingKind::AreaAnimation
    );
    assert_eq!(
        recording_kind_for(true, false, Format::Gif),
        RecordingKind::Audio
    );
    assert_eq!(
        recording_tip(RecordingKind::AreaVideo, 75),
        "Capture Button — recording area video 01:15"
    );
    assert_eq!(
        recording_tip(RecordingKind::AreaAnimation, 75),
        "Capture Button — recording area animation 01:15"
    );
    assert_eq!(
        recording_tip(RecordingKind::Video, 5),
        "Capture Button — recording video 00:05"
    );
    assert_eq!(
        recording_tip(RecordingKind::Animation, 5),
        "Capture Button — recording animation 00:05"
    );
    assert_eq!(
        recording_tip(RecordingKind::Audio, 0),
        "Capture Button — recording audio 00:00"
    );
}

#[test]
fn the_red_outline_is_only_for_a_region_recording() {
    assert!(show_recording_outline(true, true));
    assert!(!show_recording_outline(true, false));
    assert!(!show_recording_outline(false, true));
    assert!(!show_recording_outline(false, false));
}

#[test]
fn reset_scale_stays_available_from_the_tray() {
    for mode in [Mode::Idle, Mode::Recording, Mode::Saving, Mode::Selecting] {
        assert!(
            accepts_command(mode, ID_TRAY_RESET_SCALE, false),
            "{mode:?}"
        );
    }
}

#[test]
fn the_tray_window_item_closes_an_open_window_and_opens_a_hidden_one() {
    assert_eq!(tray_window_label(true), "Close window");
    assert_eq!(tray_window_label(false), "Open window");
}

#[test]
fn history_stamp_reads_as_a_date_and_time() {
    assert_eq!(
        history_stamp(4, 11, 2026, 22, 56, 7),
        "4 Nov 2026, 22:56:07"
    );
}

#[test]
fn screenshots_stay_available_for_video_animation_and_audio() {
    for kind in ["video", "gif", "webp", "audio"] {
        for id in [
            ID_SHOT_SCREEN,
            ID_SHOT_SOURCE,
            ID_SHOT_REGION,
            ID_TRAY_SHOT,
            ID_TRAY_AREA,
        ] {
            assert!(
                accepts_command(Mode::Recording, id, false),
                "{kind} must still take a screenshot"
            );
        }
        for id in [
            ID_CAPTURE_SCREEN,
            ID_CAPTURE_SOURCE,
            ID_CAPTURE_REGION,
            ID_CAPTURE_AUDIO,
            ID_TRAY_VIDEO,
            ID_TRAY_AUDIO,
        ] {
            assert!(
                !accepts_command(Mode::Recording, id, false),
                "{kind} must not start a second recording"
            );
        }
        assert!(accepts_command(Mode::Recording, ID_STOP, false), "{kind}");
        assert!(
            accepts_command(Mode::Recording, ID_STATUS, false),
            "{kind} status link"
        );
        assert!(
            accepts_command(Mode::Recording, ID_HISTORY, false)
                && accepts_command(Mode::Recording, ID_HISTORY_COPY, false)
                && accepts_command(Mode::Recording, ID_HISTORY_REVEAL, false)
                && accepts_command(Mode::Recording, ID_HISTORY_OPEN, false),
            "{kind} history link"
        );
        assert!(
            !accepts_command(Mode::Saving, ID_SHOT_SCREEN, false),
            "{kind} while the file is still being saved"
        );
        assert!(
            !accepts_command(Mode::Recording, ID_SHOT_REGION, true),
            "{kind} while an area is already open"
        );
        assert!(
            tray_item_enabled(Mode::Recording, ID_TRAY_SHOT),
            "{kind} tray screenshot"
        );
        assert!(
            tray_item_enabled(Mode::Recording, ID_TRAY_AREA),
            "{kind} tray area"
        );
        assert!(
            !tray_item_enabled(Mode::Recording, ID_TRAY_VIDEO),
            "{kind} tray video"
        );
        assert!(
            !tray_item_enabled(Mode::Recording, ID_TRAY_AUDIO),
            "{kind} tray audio"
        );
        assert!(
            tray_item_enabled(Mode::Recording, ID_TRAY_STOP),
            "{kind} tray stop"
        );
    }
}

fn window_visible() -> bool {
    unsafe {
        // SAFETY: The HWND is the test window created for this process.
        IsWindowVisible(app().hwnd).as_bool()
    }
}
