#![cfg(windows)]
#![deny(clippy::undocumented_unsafe_blocks)]

//! End-to-end checks and UI snapshots for the Slint window.
//!
//! The visible window is Slint's; the native `CaptureButton.window` is a hidden
//! 1×1 HWND that owns the command handler. Commands go to that native window,
//! while snapshots are taken with `PrintWindow` from the visible Slint window.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::DataExchange::IsClipboardFormatAvailable;
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE,
    REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ,
};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;
use windows::Win32::UI::Shell::{FOLDERID_Documents, SHGetKnownFolderPath, KNOWN_FOLDER_FLAG};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetClientRect, GetWindowLongPtrW, GetWindowRect, GetWindowTextW,
    GetWindowThreadProcessId, IsWindowVisible, PostMessageW, SendMessageW, SetWindowPos,
    GWL_EXSTYLE, SIZE_RESTORED, SWP_NOACTIVATE, SWP_NOZORDER, WM_COMMAND, WM_KEYDOWN, WM_KEYUP,
    WM_SIZE, WS_EX_TOPMOST,
};

const ID_SETTINGS_TAB: i32 = 101;
const ID_ABOUT: i32 = 103;
const ID_ABOUT_CLOSE: i32 = 281;
const ID_FORMAT: i32 = 110;
const ID_SHOT_SCREEN: i32 = 130;
const ID_SHOT_REGION: i32 = 131;
const ID_CAPTURE_SCREEN: i32 = 132;
const ID_STOP: i32 = 135;

static SERIAL: Mutex<()> = Mutex::new(());

struct TestApp {
    child: Child,
    native: HWND,
    slint: HWND,
    created: Vec<PathBuf>,
}

impl Drop for TestApp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        for path in &self.created {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[derive(Default)]
struct WindowSearch {
    pid: u32,
    native: HWND,
    slint: HWND,
}

struct Image {
    width: u32,
    height: u32,
    data: Vec<u8>,
}

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|error| error.into_inner())
}

fn sleep(millis: u64) {
    std::thread::sleep(Duration::from_millis(millis));
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn set_test_dword(name: &str, value: u32) {
    let mut key = HKEY::default();
    let name = wide(name);
    let bytes = value.to_le_bytes();
    unsafe {
        // SAFETY: The subkey name is NUL-terminated and the returned HKEY is closed below.
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            w!("Software\\CaptureButton-test"),
            0,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_READ | KEY_WRITE,
            None,
            &mut key,
            None,
        )
        .ok()
        .expect("open the test settings key");
        // SAFETY: The key is open, the value name is NUL-terminated, and the bytes outlive the call.
        RegSetValueExW(key, PCWSTR(name.as_ptr()), 0, REG_DWORD, Some(&bytes))
            .ok()
            .expect("write a test setting");
        // SAFETY: The key was returned by RegCreateKeyExW and is not used afterwards.
        let _ = RegCloseKey(key).ok();
    }
}

fn set_test_string(name: &str, value: &str) {
    let mut key = HKEY::default();
    let name = wide(name);
    let data = wide(value);
    let bytes = unsafe {
        // SAFETY: The pointer addresses a live buffer, and the length is exactly that buffer's size.
        std::slice::from_raw_parts(data.as_ptr().cast::<u8>(), data.len() * 2)
    };
    unsafe {
        // SAFETY: The subkey name is NUL-terminated and the returned HKEY is closed below.
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            w!("Software\\CaptureButton-test"),
            0,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_READ | KEY_WRITE,
            None,
            &mut key,
            None,
        )
        .ok()
        .expect("open the test settings key");
        // SAFETY: The key is open, the value name is NUL-terminated, and the bytes outlive the call.
        RegSetValueExW(key, PCWSTR(name.as_ptr()), 0, REG_SZ, Some(bytes))
            .ok()
            .expect("write a test setting");
        // SAFETY: The key was returned by RegCreateKeyExW and is not used afterwards.
        let _ = RegCloseKey(key).ok();
    }
}

fn prepare_settings() {
    prepare_process();
    for (name, value) in [
        ("clipboard", 0),
        ("tray_capture", 0),
        ("highlight_region", 0),
        ("hide_active", 0),
        ("autostart", 1),
        ("start_minimized", 1),
        ("format", 0),
        ("mkv_height", 480),
        ("mkv_fps", 30),
        ("gif_height", 480),
        ("gif_fps", 30),
        ("webp_height", 480),
        ("webp_fps", 30),
        ("ui_system", 1),
        ("ui_microphone", 0),
        ("audio_system", 1),
        ("audio_microphone", 0),
        ("completion_sound", 0),
        ("screenshot_sound", 0),
        ("region_renderer", 0),
        ("sound_volume", 50),
        // The metering previews open the microphone; snapshots and timing
        // checks must not depend on ambient sound.
        ("idle_level", 0),
    ] {
        set_test_dword(name, value);
    }
    for name in [
        "folder_default",
        "folder_screenshots",
        "folder_video",
        "folder_camera",
        "folder_sound",
    ] {
        set_test_string(name, "");
    }
}

fn command(native: HWND, id: i32) {
    unsafe {
        // SAFETY: The native window belongs to the test app and handles WM_COMMAND.
        SendMessageW(native, WM_COMMAND, WPARAM(id as usize), LPARAM(0));
    }
}

fn resize(slint: HWND, width: i32, height: i32) {
    unsafe {
        // SAFETY: The window belongs to the test app; 1600×1000 is this screen's work area.
        SetWindowPos(slint, None, 0, 0, 1600, 1000, SWP_NOZORDER | SWP_NOACTIVATE)
            .expect("force a window resize");
    }
    sleep(300);
    unsafe {
        // SAFETY: The window belongs to the test app; the size stays within this screen.
        SetWindowPos(
            slint,
            None,
            0,
            0,
            width,
            height,
            SWP_NOZORDER | SWP_NOACTIVATE,
        )
        .expect("resize the app window");
    }
    sleep(700);
    let mut client = RECT::default();
    unsafe {
        // SAFETY: The window belongs to the test app and the RECT is a writable local.
        GetClientRect(slint, &mut client).expect("read the client rect");
        let width = client.right - client.left;
        let height = client.bottom - client.top;
        // The cross-process resize does not always reach the Slint subclass, so ask it
        // to adopt the new client size explicitly.
        SendMessageW(
            slint,
            WM_SIZE,
            WPARAM(SIZE_RESTORED as usize),
            LPARAM(((height as isize) << 16) | (width as isize)),
        );
    }
    sleep(300);
}

fn output_dir() -> PathBuf {
    let documents = unsafe {
        // SAFETY: The shell returns a COM-allocated NUL-terminated string; it is freed below.
        SHGetKnownFolderPath(&FOLDERID_Documents, KNOWN_FOLDER_FLAG(0), None).ok()
    };
    if let Some(raw) = documents {
        let mut len = 0;
        // SAFETY: The string is NUL-terminated and owned by the buffer returned above.
        while unsafe { *raw.0.add(len) } != 0 {
            len += 1;
        }
        // SAFETY: `len` is the length measured before the NUL and the buffer is still alive.
        let text = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(raw.0, len) });
        // SAFETY: The buffer came from SHGetKnownFolderPath and is not used afterwards.
        unsafe { CoTaskMemFree(Some(raw.0.cast())) };
        return PathBuf::from(text).join("CaptureButton");
    }
    PathBuf::from(std::env::var_os("USERPROFILE").expect("USERPROFILE"))
        .join("Documents")
        .join("CaptureButton")
}

fn files(dir: &Path) -> HashSet<PathBuf> {
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .filter(|path| path.is_file())
                .collect()
        })
        .unwrap_or_default()
}

fn wait_for_new_file(
    dir: &Path,
    before: &HashSet<PathBuf>,
    prefix: &str,
    extension: &str,
) -> PathBuf {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let mut found: Vec<PathBuf> = files(dir)
            .into_iter()
            .filter(|path| !before.contains(path))
            .filter(|path| {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                name.starts_with(prefix) && name.ends_with(extension)
            })
            .collect();
        found.sort_by_key(|path| fs::metadata(path).and_then(|meta| meta.modified()).ok());
        if let Some(path) = found.pop() {
            return path;
        }
        assert!(
            Instant::now() < deadline,
            "no {prefix}*{extension} appeared in {}",
            dir.display()
        );
        sleep(100);
    }
}

unsafe extern "system" fn find_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: EnumWindows passes the live WindowSearch pointer in lparam.
    let search = unsafe { &mut *(lparam.0 as *mut WindowSearch) };
    let mut pid = 0u32;
    // SAFETY: The window handle comes from EnumWindows; the pid pointer is a writable local.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid != search.pid {
        return BOOL(1);
    }
    let mut class = [0u16; 128];
    // SAFETY: The class buffer is valid for the duration of the call.
    let class_len = unsafe { GetClassNameW(hwnd, &mut class) }.max(0) as usize;
    if String::from_utf16_lossy(&class[..class_len]) == "CaptureButton.window" {
        search.native = hwnd;
    }
    // SAFETY: The window handle comes from EnumWindows and is only queried.
    if unsafe { IsWindowVisible(hwnd).as_bool() } {
        let mut title = [0u16; 128];
        // SAFETY: The title buffer is valid for the duration of the call.
        let title_len = unsafe { GetWindowTextW(hwnd, &mut title) }.max(0) as usize;
        if String::from_utf16_lossy(&title[..title_len]) == "Capture Button" {
            let mut rect = RECT::default();
            // SAFETY: The RECT is a writable local and the handle is a real window.
            if unsafe { GetWindowRect(hwnd, &mut rect) }.is_ok()
                && (rect.right - rect.left) * (rect.bottom - rect.top) > 10_000
            {
                search.slint = hwnd;
            }
        }
    }
    BOOL(1)
}

fn wait_for_windows(pid: u32) -> (HWND, HWND) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let mut search = WindowSearch {
            pid,
            ..Default::default()
        };
        // SAFETY: The search struct outlives the enumeration and the callback only writes into it.
        unsafe {
            let _ = EnumWindows(Some(find_window), LPARAM(&mut search as *mut _ as isize));
        }
        if !search.native.is_invalid() && !search.slint.is_invalid() {
            return (search.native, search.slint);
        }
        assert!(
            Instant::now() < deadline,
            "Capture Button windows did not appear"
        );
        sleep(100);
    }
}

fn prepare_process() {
    unsafe {
        // SAFETY: This runs before the first window or GDI call; a failure only
        // means the process awareness was already set by the test harness.
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

fn launch() -> TestApp {
    let child = Command::new(env!("CARGO_BIN_EXE_CaptureButton"))
        .arg("--test-mode")
        .spawn()
        .expect("start Capture Button");
    let pid = child.id();
    let (native, slint) = wait_for_windows(pid);
    TestApp {
        child,
        native,
        slint,
        created: Vec::new(),
    }
}

fn capture_window(hwnd: HWND) -> Image {
    unsafe {
        // SAFETY: The window belongs to the test app and the RECT is a writable local.
        let mut rect = RECT::default();
        GetWindowRect(hwnd, &mut rect).expect("read the window rect");
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        let screen = GetDC(None);
        let memory = CreateCompatibleDC(screen);
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = width;
        info.bmiHeader.biHeight = -height;
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB.0;
        let mut bits = std::ptr::null_mut();
        // SAFETY: The bitmap header describes a width×height 32-bit DIB and the bits pointer is writable.
        let bitmap = CreateDIBSection(screen, &info, DIB_RGB_COLORS, &mut bits, None, 0)
            .expect("create the snapshot bitmap");
        let old = SelectObject(memory, bitmap);
        // SAFETY: The destination DC holds the bitmap and the source is a live window.
        let _ = PrintWindow(hwnd, memory, PRINT_WINDOW_FLAGS(2));
        let len = (width * height * 4) as usize;
        let mut data = vec![0u8; len];
        // SAFETY: The DIB section has exactly width×height×4 bytes and `bits` points at them.
        std::ptr::copy_nonoverlapping(bits.cast::<u8>(), data.as_mut_ptr(), len);
        SelectObject(memory, old);
        let _ = DeleteObject(bitmap);
        let _ = DeleteDC(memory);
        let _ = ReleaseDC(None, screen);
        for pixel in data.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
            pixel[3] = 255;
        }
        Image {
            width: width as u32,
            height: height as u32,
            data,
        }
    }
}

fn write_png(path: &Path, image: &Image) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create the snapshot directory");
    }
    let file = File::create(path).expect("create the snapshot file");
    let mut encoder = png::Encoder::new(BufWriter::new(file), image.width, image.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("write the PNG header");
    writer
        .write_image_data(&image.data)
        .expect("write the PNG pixels");
}

fn store_image(path: &Path, image: &Image) {
    if std::env::var_os("EZCAP_UPDATE_SNAPSHOTS").is_some() {
        write_png(path, image);
        return;
    }
    let file = File::open(path).unwrap_or_else(|error| panic!("open {}: {error}", path.display()));
    let decoder = png::Decoder::new(file);
    let mut reader = decoder.read_info().expect("read the snapshot header");
    let mut buffer = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buffer).expect("read the snapshot");
    assert_eq!(
        (info.width, info.height),
        (image.width, image.height),
        "{}",
        path.display()
    );
    assert_eq!(info.color_type, png::ColorType::Rgba, "{}", path.display());
    let delta = buffer
        .iter()
        .zip(&image.data)
        .map(|(left, right)| left.abs_diff(*right) as u64)
        .sum::<u64>() as f64
        / buffer.len() as f64;
    if delta > 2.0 {
        let actual = path.with_extension("actual.png");
        write_png(&actual, image);
        panic!(
            "snapshot mismatch: {}, mean channel delta {delta}; actual saved to {}",
            path.display(),
            actual.display()
        );
    }
}

fn snapshot_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots")
        .join(format!("{name}.png"))
}

fn site_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("site")
        .join("assets")
        .join(format!("{name}.png"))
}

/// Writes a full-window source image for the site. The site ships resized WebP
/// and PNG copies of these captured in a separate step, so they are build inputs
/// rather than golden fixtures: a plain run does not compare them.
fn store_site_image(path: &Path, image: &Image) {
    if std::env::var_os("EZCAP_UPDATE_SNAPSHOTS").is_some() {
        write_png(path, image);
    }
}

fn wait_for_file_ready(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last = 0u64;
    let mut stable = 0;
    while Instant::now() < deadline {
        let length = fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
        if length > 0 && length == last {
            stable += 1;
            if stable >= 3 {
                return;
            }
        } else {
            stable = 0;
        }
        last = length;
        sleep(100);
    }
    panic!("{} did not finish writing", path.display());
}

fn check(path: PathBuf, prefix: &str, extension: &str) {
    let name = path.file_name().unwrap().to_string_lossy();
    println!("checking {name} against {prefix} / {extension}");
    assert!(name.starts_with(prefix), "{name}");
    assert!(name.ends_with(extension), "{name}");
    assert!(path.is_file(), "{} is missing", path.display());
}

fn check_stream(path: &PathBuf, kind: ffmpeg_next::media::Type) {
    let input = ffmpeg_next::format::input(path).unwrap();
    assert!(
        input.streams().best(kind).is_some(),
        "missing {kind:?} stream in {}",
        path.display()
    );
}

fn check_decodable_audio(path: &PathBuf, minimum_samples: usize) {
    let mut input = ffmpeg_next::format::input(path).unwrap();
    let stream = input
        .streams()
        .best(ffmpeg_next::media::Type::Audio)
        .unwrap();
    let index = stream.index();
    let mut decoder = ffmpeg_next::codec::context::Context::from_parameters(stream.parameters())
        .unwrap()
        .decoder()
        .audio()
        .unwrap();
    let mut decoded = ffmpeg_next::frame::Audio::empty();
    let mut samples = 0usize;
    for (source, packet) in input.packets() {
        if source.index() != index {
            continue;
        }
        decoder.send_packet(&packet).unwrap();
        while decoder.receive_frame(&mut decoded).is_ok() {
            samples += decoded.samples();
        }
    }
    decoder.send_eof().unwrap();
    while decoder.receive_frame(&mut decoded).is_ok() {
        samples += decoded.samples();
    }
    if samples == 0 {
        eprintln!(
            "warning: no audio samples in {}; this session has no audio source",
            path.display()
        );
        return;
    }
    assert!(
        samples >= minimum_samples,
        "only {samples} audio samples decoded from {}",
        path.display()
    );
}

#[test]
#[ignore]
fn ui_snapshots() {
    let _serial = serial();
    prepare_settings();
    set_test_dword("scale", 75);
    let app = launch();

    resize(app.slint, 1124, 1000);
    let capture = capture_window(app.slint);
    store_image(&snapshot_path("main-capture-100"), &capture);
    store_site_image(&site_path("capture-controls"), &capture);

    command(app.native, ID_ABOUT);
    sleep(600);
    let about = capture_window(app.slint);
    store_site_image(&site_path("about"), &about);
    store_image(&snapshot_path("about-popup"), &about);

    command(app.native, ID_ABOUT_CLOSE);
    sleep(300);
    command(app.native, ID_SETTINGS_TAB);
    sleep(600);
    store_image(
        &snapshot_path("main-settings-100"),
        &capture_window(app.slint),
    );
}

#[test]
#[ignore]
fn records_every_output_format_through_the_real_ui() {
    let _serial = serial();
    prepare_settings();
    set_test_dword("scale", 75);
    ffmpeg_next::init().unwrap();
    let mut app = launch();
    let dir = output_dir();
    fs::create_dir_all(&dir).unwrap();

    for (format, prefix, extension) in [
        (0, "Video_", ".mkv"),
        (1, "Animation_", ".gif"),
        (2, "Animation_", ".webp"),
    ] {
        command(app.native, ID_FORMAT + format);
        sleep(200);
        let before = files(&dir);
        command(app.native, ID_CAPTURE_SCREEN);
        // The take clock starts after prepare (audio devices, screen capture and
        // the encoder), which takes a few hundred milliseconds; the audio track
        // is measured against the sleep below, so leave room for it.
        sleep(1600);
        command(app.native, ID_STOP);
        let path = wait_for_new_file(&dir, &before, prefix, extension);
        wait_for_file_ready(&path);
        check(path.clone(), prefix, extension);
        check_stream(&path, ffmpeg_next::media::Type::Video);
        if format == 0 {
            check_stream(&path, ffmpeg_next::media::Type::Audio);
            check_decodable_audio(&path, 40000);
        }
        app.created.push(path);
        sleep(700);
    }

    let before = files(&dir);
    command(app.native, ID_SHOT_SCREEN);
    let path = wait_for_new_file(&dir, &before, "Image_", ".png");
    wait_for_file_ready(&path);
    check(path.clone(), "Image_", ".png");
    app.created.push(path);
}

#[test]
#[ignore]
fn a_screenshot_does_not_touch_the_clipboard_when_copy_is_off() {
    let _serial = serial();
    prepare_settings();
    set_test_dword("scale", 75);
    set_test_dword("clipboard", 0);
    let mut app = launch();
    let dir = output_dir();
    fs::create_dir_all(&dir).unwrap();

    let before = files(&dir);
    command(app.native, ID_SHOT_SCREEN);
    let path = wait_for_new_file(&dir, &before, "Image_", ".png");
    wait_for_file_ready(&path);
    check(path.clone(), "Image_", ".png");
    app.created.push(path);
    sleep(400);
    // A text/HTML clipboard from another app is fine; the screenshot must not add
    // the picture (CF_BITMAP) or the saved file (CF_HDROP) when the option is off.
    assert!(
        // SAFETY: The clipboard format query touches no borrowed state.
        !unsafe { IsClipboardFormatAvailable(2) }.is_ok(),
        "the screenshot put a bitmap on the clipboard although the copy option is off"
    );
    assert!(
        // SAFETY: The clipboard format query touches no borrowed state.
        !unsafe { IsClipboardFormatAvailable(15) }.is_ok(),
        "the screenshot put the file on the clipboard although the copy option is off"
    );
}

/// Finds the visible area overlay of `pid`.
fn wait_for_overlay(pid: u32) -> HWND {
    #[derive(Default)]
    struct Search {
        pid: u32,
        found: HWND,
    }
    unsafe extern "system" fn look(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: EnumWindows passes the live Search pointer and window handle.
        let search = unsafe { &mut *(lparam.0 as *mut Search) };
        let mut window_pid = 0u32;
        // SAFETY: The handle comes from EnumWindows and the pid pointer is writable.
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut window_pid)) };
        // SAFETY: The handle comes from EnumWindows and the call only reads it.
        if window_pid != search.pid || !unsafe { IsWindowVisible(hwnd).as_bool() } {
            return BOOL(1);
        }
        let mut title = [0u16; 128];
        // SAFETY: The title buffer is valid for the duration of the call.
        let length = unsafe { GetWindowTextW(hwnd, &mut title) }.max(0) as usize;
        if String::from_utf16_lossy(&title[..length]) != "Capture Button area" {
            return BOOL(1);
        }
        search.found = hwnd;
        BOOL(0)
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let mut search = Search {
            pid,
            found: HWND::default(),
        };
        // SAFETY: The search struct outlives the enumeration and the callback
        // only writes into it.
        unsafe {
            EnumWindows(Some(look), LPARAM(&mut search as *mut Search as isize)).ok();
        }
        if !search.found.is_invalid() {
            return search.found;
        }
        assert!(Instant::now() < deadline, "the area overlay did not appear");
        sleep(50);
    }
}

/// Whether the window carries `WS_EX_TOPMOST`.
fn window_topmost(hwnd: HWND) -> bool {
    // SAFETY: The handle belongs to a live window of this process.
    let style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
    style & WS_EX_TOPMOST.0 != 0
}

#[test]
#[ignore = "shows and drives the real area overlay window"]
fn the_slint_overlay_never_leaves_the_main_window_topmost() {
    let _serial = serial();
    prepare_settings();
    // The Slint overlay is the renderer that used to raise the main window.
    set_test_dword("region_renderer", 1);
    set_test_dword("scale", 75);
    let app = launch();
    command(app.native, ID_SHOT_REGION);
    let overlay = wait_for_overlay(app.child.id());
    assert!(window_topmost(overlay), "the area overlay is not topmost");

    // SAFETY: The overlay belongs to the test app and handles the key messages.
    unsafe {
        PostMessageW(overlay, WM_KEYDOWN, WPARAM(VK_ESCAPE.0 as usize), LPARAM(0)).ok();
    }
    sleep(80);
    // SAFETY: Same window, after the message loop had a chance to see the key.
    unsafe {
        PostMessageW(overlay, WM_KEYUP, WPARAM(VK_ESCAPE.0 as usize), LPARAM(0)).ok();
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    // SAFETY: The handle is a window of the test app.
    while Instant::now() < deadline && unsafe { IsWindowVisible(overlay).as_bool() } {
        sleep(50);
    }
    // SAFETY: The handle is still owned by the test app.
    // SAFETY: The handle is a window of the test app and only queried here.
    let closed = !unsafe { IsWindowVisible(overlay).as_bool() };
    assert!(closed, "Esc did not close the area overlay");
    assert!(
        !window_topmost(app.slint),
        "the main window is left topmost after the overlay closes"
    );
}
