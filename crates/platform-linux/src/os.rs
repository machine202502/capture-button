//! Files, time, clipboard and dialogs on Linux.

use platform_core::time::LocalTime;
use platform_core::types::{AudioDevice, Frame, OutputCategory, OutputFolders, Rect};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The app's per-user configuration directory.
pub fn config_dir() -> PathBuf {
    base_dir("XDG_CONFIG_HOME", ".config").join("capture-button")
}

/// Where the log file lives.
pub fn log_dir() -> PathBuf {
    base_dir("XDG_STATE_HOME", ".local/state")
        .join("capture-button")
        .join("logs")
}

fn base_dir(variable: &str, fallback: &str) -> PathBuf {
    if let Some(value) = std::env::var_os(variable) {
        let path = PathBuf::from(value);
        if path.is_absolute() {
            return path;
        }
    }
    home().join(fallback)
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

pub fn open_log_dir() {
    let _ = open_file(&log_dir());
}

/// The current local time, used in file names and log lines.
pub fn local_time() -> LocalTime {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    #[cfg(unix)]
    {
        let now = seconds.as_secs() as libc::time_t;
        let mut broken = unsafe {
            // SAFETY: The pointer is a valid, writable `tm` owned by this frame.
            std::mem::zeroed::<libc::tm>()
        };
        unsafe {
            // SAFETY: `now` and `broken` are valid locals; localtime_r writes
            // into `broken` and never keeps the pointers.
            libc::localtime_r(&now, &mut broken);
        }
        LocalTime {
            year: (broken.tm_year + 1900) as u16,
            month: (broken.tm_mon + 1) as u8,
            day: broken.tm_mday as u8,
            hour: broken.tm_hour as u8,
            minute: broken.tm_min as u8,
            second: broken.tm_sec as u8,
            millis: seconds.subsec_millis() as u16,
        }
    }
    #[cfg(not(unix))]
    {
        // Only for a host-side `cargo check`; the Linux build uses the branch above.
        let time = seconds.as_secs() % 86_400;
        LocalTime {
            year: 1970,
            month: 1,
            day: 1,
            hour: (time / 3600) as u8,
            minute: ((time % 3600) / 60) as u8,
            second: (time % 60) as u8,
            millis: seconds.subsec_millis() as u16,
        }
    }
}

/// The monitor under the pointer.
pub fn cursor_monitor_rect() -> Rect {
    crate::screen::monitor_under_pointer().unwrap_or_else(crate::screen::primary_monitor)
}

/// The whole screen (or one monitor) without the pointer baked in.
pub fn grab(rect: Rect) -> Result<Frame, String> {
    crate::screen::grab_region(rect)
}

pub fn grab_without_cursor(rect: Rect) -> Result<Frame, String> {
    crate::screen::grab_region(rect)
}

static FOLDERS: OnceLock<OutputFolders> = OnceLock::new();

pub fn set_output_folders(folders: &OutputFolders) {
    platform_core::paths::set_folders(folders);
    let _ = FOLDERS.set(folders.clone());
}

fn configured(category: OutputCategory) -> Option<PathBuf> {
    platform_core::paths::configured(category)
}

/// The folder every category uses until one is chosen: `~/Documents/CaptureButton`,
/// the same default as on Windows.
pub fn output_dir() -> Result<PathBuf, String> {
    platform_core::paths::ensure_dir(&default_output_dir(&home()))
}

fn default_output_dir(home: &Path) -> PathBuf {
    home.join("Documents").join("CaptureButton")
}

/// The folder a file of `category` belongs in: the one chosen for it, or the
/// default folder. A folder chosen for `Default` covers every category that has
/// none of its own.
pub fn output_dir_for(category: OutputCategory) -> Result<PathBuf, String> {
    output_dir_in(&home(), category)
}

fn output_dir_in(home: &Path, category: OutputCategory) -> Result<PathBuf, String> {
    match configured(category) {
        Some(dir) => platform_core::paths::ensure_dir(&dir),
        None => platform_core::paths::ensure_dir(&default_output_dir(home)),
    }
}

pub fn output_path(
    category: OutputCategory,
    kind: &str,
    extension: &str,
) -> Result<PathBuf, String> {
    let dir = output_dir_for(category)?;
    platform_core::paths::unique_path(&dir, kind, extension, local_time())
}

/// Puts text into the session clipboard.
///
/// `wl-copy` serves the Wayland clipboard and `xclip` the X11 one. A Wayland
/// session runs XWayland too, so both are considered, in that order, and only a
/// tool the session can actually reach is started: asking for a program that is
/// not installed fails before anything is attempted.
pub fn copy_text_to_clipboard(text: &str) -> Result<(), String> {
    copy_to_clipboard(text.as_bytes(), None)
}

/// Puts a file reference into the clipboard as a `text/uri-list`.
pub fn copy_file_to_clipboard(path: &Path) -> Result<(), String> {
    let uri = format!("file://{}\r\n", path.display());
    copy_to_clipboard(uri.as_bytes(), Some("text/uri-list"))
}

fn copy_to_clipboard(input: &[u8], mime: Option<&str>) -> Result<(), String> {
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some_and(|value| !value.is_empty());
    let x11 = std::env::var_os("DISPLAY").is_some_and(|value| !value.is_empty());
    let mut last_error = None;
    if wayland && on_path("wl-copy") {
        let mut args = Vec::new();
        if let Some(mime) = mime {
            args.extend(["-t", mime]);
        }
        match command_with_stdin("wl-copy", &args, input) {
            Ok(()) => return Ok(()),
            Err(error) => last_error = Some(error),
        }
    }
    if x11 && on_path("xclip") {
        let mut args = vec!["-selection", "clipboard"];
        if let Some(mime) = mime {
            args.extend(["-t", mime]);
        }
        match command_with_stdin("xclip", &args, input) {
            Ok(()) => return Ok(()),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        "no clipboard tool is available: install wl-clipboard on Wayland or xclip on X11".into()
    }))
}

/// Whether `program` can be started from this session's `PATH`.
fn on_path(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|directory| directory.join(program).is_file())
    })
}

/// Opens `path` in the session's application for that kind of file.
///
/// `gio open` is the opener of a Wayland session: it asks the desktop over
/// D-Bus, needs no X display and returns once the application is started.
/// `xdg-open` is tried next for a session without `gio`. A handler that
/// refuses to start is not reported as a silent success: the error carries
/// the tool and the first line the handler wrote to stderr.
///
/// ImageMagick's `display` is an X11 program, and a desktop with no preferred
/// image application can pick it as the handler for images. It does not
/// composite GIF delta frames, so a recording opens with a green background.
/// Shotwell is the handler for WebP here but only decodes still images, so an
/// animated WebP opens as a black picture. Those are desktop defaults to fix,
/// not for this function to second-guess: the app opens every file through the
/// session's own handler.
pub fn open_file(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Err(format!("{}: no such file", path.display()));
    }
    let mut failure = None;
    for (program, arguments) in [("gio", &["open"][..]), ("xdg-open", &[][..])] {
        if !on_path(program) {
            continue;
        }
        match run_opener(program, arguments, path) {
            Ok(()) => return Ok(()),
            Err(error) => failure = Some(error),
        }
    }
    Err(failure.unwrap_or_else(|| {
        format!(
            "cannot open {}: neither gio nor xdg-open is installed",
            path.display()
        )
    }))
}

/// How long an opener gets to report a failure before the program it started
/// is treated as the application the user asked for. It only has to cover the
/// start of `gio`/`xdg-open`; the handler they launch may run for hours.
const OPENER_GRACE: Duration = Duration::from_millis(1500);

fn run_opener(program: &str, arguments: &[&str], path: &Path) -> Result<(), String> {
    // The handler the opener starts (a player, a file manager) inherits the
    // opener's stderr and can outlive it. `Command::output` waited for that
    // stream to close, so opening a saved take froze the window until the
    // player was closed. Collect the opener's stderr in a file instead and
    // never wait for the handler.
    let log = std::env::temp_dir().join(format!(
        "CaptureButton-opener-{}-{}.log",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0)
    ));
    let stderr = match std::fs::File::create(&log) {
        Ok(file) => Stdio::from(file),
        Err(_) => Stdio::null(),
    };
    let mut child = match Command::new(program)
        .args(arguments)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr)
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            let _ = std::fs::remove_file(&log);
            return Err(format!("{program}: {error}"));
        }
    };
    let deadline = Instant::now() + OPENER_GRACE;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let detail = std::fs::read(&log)
                    .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                    .unwrap_or_default();
                let _ = std::fs::remove_file(&log);
                if status.success() {
                    return Ok(());
                }
                let detail = detail.lines().next().unwrap_or_default().trim();
                return Err(if detail.is_empty() {
                    format!("{program} exited with {status}")
                } else {
                    format!("{program} exited with {status}: {detail}")
                });
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                // Still running: the file is handed to a program that waits on
                // the user. The stderr copy has done its job; unlink it now —
                // the child keeps writing to the open inode — and reap the
                // program from its own thread so the caller stays live.
                let _ = std::fs::remove_file(&log);
                let _ = std::thread::Builder::new()
                    .name("CaptureButton-opener".into())
                    .spawn(move || {
                        let _ = child.wait();
                    });
                return Ok(());
            }
            Err(error) => {
                let _ = std::fs::remove_file(&log);
                return Err(format!("{program}: {error}"));
            }
        }
    }
}

pub fn reveal_in_folder(path: &Path) -> Result<(), String> {
    let folder = path.parent().unwrap_or(path);
    open_file(folder)
}

pub fn screenshot_sound(volume: u32) {
    static SOUND: &[u8] = include_bytes!("../../../assets/shutter.wav");
    play_wav(SOUND, volume);
}

pub fn completion_sound(volume: u32) {
    static SOUND: &[u8] = include_bytes!("../../../assets/recording-saved.wav");
    play_wav(SOUND, volume);
}

/// Plays an embedded WAV through the sound server at `volume` percent.
///
/// The same two files and the same PCM scaling as on Windows go out through
/// `paplay`, `pw-play` or `aplay` — whichever the session runs. The scaled copy
/// lives in a temporary file for the length of the call, and the playback runs
/// on its own thread so a capture is never delayed by the sound.
fn play_wav(wav: &[u8], volume: u32) {
    let data = wav_at_volume(wav, volume.min(100));
    let name = format!(
        "capture-button-sound-{}-{}.wav",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0)
    );
    std::thread::Builder::new()
        .name("CaptureButton-sound".into())
        .spawn(move || {
            let path = std::env::temp_dir().join(name);
            if std::fs::write(&path, &data).is_err() {
                return;
            }
            for player in ["paplay", "pw-play", "aplay"] {
                let played = Command::new(player)
                    .arg(&path)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
                if matches!(played, Ok(status) if status.success()) {
                    break;
                }
            }
            let _ = std::fs::remove_file(&path);
        })
        .ok();
}

/// The WAV image with its 16-bit PCM body scaled to `volume` percent.
fn wav_at_volume(wav: &[u8], volume: u32) -> Vec<u8> {
    let mut data = wav.to_vec();
    if volume >= 100 {
        return data;
    }
    let Some((start, length)) = wav_pcm16(wav) else {
        return data;
    };
    let end = start + length;
    for sample in data[start..end].as_chunks_mut::<2>().0 {
        let value = i16::from_le_bytes([sample[0], sample[1]]) as i32;
        sample.copy_from_slice(&((value * volume as i32 / 100) as i16).to_le_bytes());
    }
    data
}

/// Locates the 16-bit PCM samples in a RIFF/WAVE image.
fn wav_pcm16(wav: &[u8]) -> Option<(usize, usize)> {
    if wav.len() < 12 || &wav[..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
        return None;
    }
    let mut offset = 12;
    let mut bits = 0u16;
    let mut pcm = false;
    while offset + 8 <= wav.len() {
        let id = &wav[offset..offset + 4];
        let length = u32::from_le_bytes([
            wav[offset + 4],
            wav[offset + 5],
            wav[offset + 6],
            wav[offset + 7],
        ]) as usize;
        let body = offset + 8;
        if body + length > wav.len() {
            return None;
        }
        match id {
            b"fmt " if length >= 16 => {
                let tag = u16::from_le_bytes([wav[body], wav[body + 1]]);
                bits = u16::from_le_bytes([wav[body + 14], wav[body + 15]]);
                pcm = tag == 1;
            }
            b"data" if pcm && bits == 16 => return Some((body, length)),
            _ => {}
        }
        offset = body + length + (length & 1);
    }
    None
}

/// Input devices as cpal sees them (ALSA/PipeWire on Linux).
pub fn microphones() -> Vec<AudioDevice> {
    use cpal::traits::HostTrait;

    let host = cpal::default_host();
    let Ok(devices) = host.input_devices() else {
        return Vec::new();
    };
    // `Display` is the same representation the capture side matches with
    // `device.to_string() == name`, so using it here keeps them in step.
    devices
        .map(|device| AudioDevice {
            id: device.to_string(),
            name: device.to_string(),
        })
        .collect()
}

pub fn device_name(id: &str) -> Option<String> {
    microphones()
        .into_iter()
        .find(|device| device.id == id)
        .map(|device| device.name)
}

fn command_with_stdin(program: &str, args: &[&str], input: &[u8]) -> Result<(), String> {
    use std::io::Write;

    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("{program}: {error}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(input)
            .map_err(|error| format!("{program}: {error}"))?;
    }
    let status = child
        .wait_with_output()
        .map_err(|error| format!("{program}: {error}"))?;
    if status.status.success() {
        Ok(())
    } else {
        Err(format!("{program} exited with {}", status.status))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// The folder settings are process-wide, so the tests that change them
    /// take turns.
    static FOLDERS: Mutex<()> = Mutex::new(());

    fn scratch(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("CaptureButton-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn the_default_folder_is_documents_capture_button() {
        assert_eq!(
            default_output_dir(Path::new("/home/e2e")),
            Path::new("/home/e2e/Documents/CaptureButton")
        );
    }

    #[test]
    fn an_unconfigured_category_uses_the_default_folder() {
        let _guard = FOLDERS.lock().unwrap_or_else(|error| error.into_inner());
        platform_core::paths::set_folders(&OutputFolders::EMPTY);
        let home = scratch("os-default");
        let expected = home.join("Documents/CaptureButton");
        for category in OutputCategory::ALL {
            let dir = output_dir_in(&home, category).expect("the folder is created");
            assert_eq!(dir, expected, "{category:?}");
            assert!(dir.is_dir(), "{category:?}");
        }
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn a_chosen_folder_moves_only_that_category() {
        let _guard = FOLDERS.lock().unwrap_or_else(|error| error.into_inner());
        let home = scratch("os-custom");
        let chosen = scratch("os-chosen");
        let expected = home.join("Documents/CaptureButton");

        let mut folders = OutputFolders::EMPTY;
        folders.set(OutputCategory::Video, Some(chosen.clone()));
        platform_core::paths::set_folders(&folders);
        assert_eq!(output_dir_in(&home, OutputCategory::Video).unwrap(), chosen);
        for category in [
            OutputCategory::Screenshots,
            OutputCategory::Camera,
            OutputCategory::Sound,
            OutputCategory::Default,
        ] {
            assert_eq!(
                output_dir_in(&home, category).unwrap(),
                expected,
                "{category:?}"
            );
        }

        platform_core::paths::set_folders(&OutputFolders::EMPTY);
        assert_eq!(
            output_dir_in(&home, OutputCategory::Video).unwrap(),
            expected
        );
        std::fs::remove_dir_all(&home).unwrap();
        std::fs::remove_dir_all(&chosen).unwrap();
    }

    #[test]
    fn a_chosen_default_folder_covers_every_category() {
        let _guard = FOLDERS.lock().unwrap_or_else(|error| error.into_inner());
        let home = scratch("os-default-custom");
        let chosen = scratch("os-default-chosen");
        let mut folders = OutputFolders::EMPTY;
        folders.set(OutputCategory::Default, Some(chosen.clone()));
        platform_core::paths::set_folders(&folders);
        for category in OutputCategory::ALL {
            assert_eq!(
                output_dir_in(&home, category).unwrap(),
                chosen,
                "{category:?}"
            );
        }
        platform_core::paths::set_folders(&OutputFolders::EMPTY);
        std::fs::remove_dir_all(&home).unwrap();
        std::fs::remove_dir_all(&chosen).unwrap();
    }

    /// A handler that keeps the opener's stderr open used to freeze the whole
    /// window: the old `Command::output` waited for that stream. The opener
    /// must return as soon as the program it started has exited.
    #[test]
    fn an_opener_that_hands_the_file_to_a_handler_returns() {
        let started = Instant::now();
        let result = run_opener("sh", &["-c", "sleep 3 & exit 0"], Path::new("ignored"));
        assert!(result.is_ok(), "{result:?}");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the opener waited for the background handler: {:?}",
            started.elapsed()
        );
    }

    /// An opener that itself blocks (a browser started by `xdg-open`) is
    /// detached after the grace period instead of keeping the caller.
    #[test]
    fn an_opener_that_keeps_running_is_detached() {
        let started = Instant::now();
        let result = run_opener("sh", &["-c", "sleep 3"], Path::new("ignored"));
        assert!(result.is_ok(), "{result:?}");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "the opener was not detached: {:?}",
            started.elapsed()
        );
    }

    /// A fast, real failure is still reported with the handler's first line.
    #[test]
    fn a_failing_opener_reports_its_stderr() {
        let error = run_opener("sh", &["-c", "echo boom >&2; exit 3"], Path::new("ignored"))
            .expect_err("a non-zero exit is a failure");
        assert!(error.contains("boom"), "{error}");
        assert!(error.contains("exited with"), "{error}");
    }
}
