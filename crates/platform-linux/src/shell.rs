//! The host message loop, timers and the desktop integration of Linux.
//!
//! There is no tray icon on plain GNOME, so the tray calls are accepted and do
//! nothing: the application keeps working from its window. Timers and the two
//! wake channels emulate the Win32 message loop the application was built on.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

pub use platform_core::tray::{
    hotkey_defs, hotkey_from_press, parse_hotkey, Hotkey, HotkeyAction, HotkeyBinding,
    HotkeyChoice, HotkeyEvent, HotkeyKey, PaintEvent, ShellEvent, TrayBadge, TrayEntry, TrayGlyph,
};

pub const TIMER_ID: usize = 1;
pub const HISTORY_ACK_TIMER: usize = 2;
pub const TRAY_RETRY_TIMER: usize = 3;
/// Drives the level bars inside the volume sliders while a take records.
pub const LEVEL_TIMER: usize = 4;

pub const ID_TRAY_AREA: i32 = 200;
pub const ID_TRAY_SHOT: i32 = 201;
pub const ID_TRAY_VIDEO: i32 = 202;
pub const ID_TRAY_AUDIO: i32 = 203;
pub const ID_TRAY_STOP: i32 = 204;
pub const ID_TRAY_EXIT: i32 = 205;
pub const ID_TRAY_OPEN: i32 = 206;

static QUEUE: OnceLock<(Mutex<Vec<ShellEventProxy>>, Condvar)> = OnceLock::new();
static UI_WAKE: OnceLock<(Mutex<bool>, Condvar)> = OnceLock::new();
static TIMERS: OnceLock<Mutex<HashMap<usize, u64>>> = OnceLock::new();
static TIMER_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// The thread that owns the X11 passive grabs. It is restarted when the
/// settings change, so the grabs always match the saved combinations.
static HOTKEY_WORKER: Mutex<Option<HotkeyWorker>> = Mutex::new(None);

struct HotkeyWorker {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

/// One passive X11 grab: the keycode and modifier mask of a table row.
#[derive(Clone, Copy)]
struct GrabBinding {
    keycode: u8,
    modifiers: u16,
    action: HotkeyAction,
}

/// `ShellEvent` is not `Send` because of its payload-free enum, but the queue is
/// shared between threads, so it is stored in a wrapper that is.
enum ShellEventProxy {
    Event(ShellEvent),
}

unsafe impl Send for ShellEventProxy {}

fn queue() -> &'static (Mutex<Vec<ShellEventProxy>>, Condvar) {
    QUEUE.get_or_init(|| (Mutex::new(Vec::new()), Condvar::new()))
}

fn ui_wake() -> &'static (Mutex<bool>, Condvar) {
    UI_WAKE.get_or_init(|| (Mutex::new(false), Condvar::new()))
}

fn timers() -> &'static Mutex<HashMap<usize, u64>> {
    TIMERS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn enqueue(event: ShellEvent) {
    let (queue, wake) = queue();
    queue
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .push(ShellEventProxy::Event(event));
    wake.notify_all();
}

/// Installs the palette. Linux draws no native chrome of its own, so the value
/// is only kept for future use.
pub fn install_palette(_palette: platform_core::palette::Palette) {}

/// The lock file that keeps a second copy of the application from starting.
///
/// The descriptor must stay open for the whole life of the process: `flock`
/// belongs to the open file description, and the kernel releases it when the
/// process dies, so a crash never leaves a stale lock behind.
static SINGLE_INSTANCE: Mutex<Option<File>> = Mutex::new(None);

/// `$XDG_RUNTIME_DIR` when it names an existing, absolute directory.
fn runtime_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute() && dir.is_dir())
}

/// The lock file candidates: the session runtime directory first, then the
/// per-user configuration directory. Test mode gets its own names, so a
/// `--test-mode` instance can run next to the production one; this mirrors
/// the `.test` mutex on Windows.
fn lock_paths(runtime_dir: Option<PathBuf>, config_dir: PathBuf, test_mode: bool) -> Vec<PathBuf> {
    let (runtime_name, config_name) = if test_mode {
        ("capture-button.test.lock", "single-instance.test.lock")
    } else {
        ("capture-button.lock", "single-instance.lock")
    };
    let mut paths = runtime_dir
        .map(|dir| dir.join(runtime_name))
        .into_iter()
        .collect::<Vec<_>>();
    paths.push(config_dir.join(config_name));
    paths
}

/// The socket candidates, in the same order as the lock files: the session
/// runtime directory, then the per-user configuration directory.
fn socket_paths(
    runtime_dir: Option<PathBuf>,
    config_dir: PathBuf,
    test_mode: bool,
) -> Vec<PathBuf> {
    let name = if test_mode {
        "capture-button.test.sock"
    } else {
        "capture-button.sock"
    };
    let mut paths = runtime_dir
        .map(|dir| dir.join(name))
        .into_iter()
        .collect::<Vec<_>>();
    paths.push(config_dir.join(name));
    paths
}

/// Locks `path` exclusively and without waiting.
///
/// `Ok(None)` means another descriptor already owns the lock; the file
/// returned with `Ok(Some)` holds it until it is dropped.
fn lock(path: &Path) -> std::io::Result<Option<File>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    // SAFETY: `file` owns an open descriptor for the duration of the call and
    // `flock` neither stores nor closes it.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(Some(file));
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::WouldBlock {
        Ok(None)
    } else {
        Err(error)
    }
}

/// Takes the first usable lock among `paths`.
///
/// A lock held by another process ends the search: trying the next candidate
/// would let a second copy start next to the first one. A candidate that
/// cannot be used at all is only logged вЂ” the application then starts without
/// the guard rather than not starting.
fn acquire(paths: &[PathBuf]) -> bool {
    let mut slot = SINGLE_INSTANCE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if slot.is_some() {
        return false;
    }
    for path in paths {
        match lock(path) {
            Ok(Some(file)) => {
                *slot = Some(file);
                return true;
            }
            Ok(None) => return false,
            Err(error) => platform_core::logging::warn(format!(
                "shell: cannot use the single-instance lock {}: {error}",
                path.display()
            )),
        }
    }
    platform_core::logging::warn(
        "shell: no single-instance lock file is available; starting without the guard",
    );
    true
}

/// The lock files for this process, in the order they are tried.
fn take_single_instance() -> bool {
    acquire(&lock_paths(
        runtime_dir(),
        crate::os::config_dir(),
        platform_core::runtime::test_mode(),
    ))
}

/// The request a second instance sends to the running one.
const SHOW_REQUEST: &str = "show\n";

/// The answer of the running instance; the connection closes right after it.
const SHOW_REPLY: &str = "ok\n";

/// Longest request accepted from the socket, so a stuck client cannot grow the
/// buffer while the listener thread waits for a newline.
const MAX_REQUEST: u64 = 64;

/// How long either side of the window-activation channel waits before giving
/// up: a dead or hung peer must never keep the other process from exiting.
const SHOW_TIMEOUT: Duration = Duration::from_millis(500);

/// How long the second activation request waits for the window manager to
/// finish the map or deiconify the Slint window started with the first one.
const ACTIVATE_RETRY: Duration = Duration::from_millis(150);

/// The socket this process listens on, kept so a clean exit can remove it.
static SHOW_SOCKET: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Starts the socket a second instance uses to ask for the window.
///
/// It runs after the single-instance lock, so a file found on disk belongs to a
/// predecessor that died вЂ” a stale socket refuses connections and is replaced.
/// A channel that cannot be created only costs the activation; the application
/// still starts and still refuses a second copy.
fn start_show_socket() {
    let paths = socket_paths(
        runtime_dir(),
        crate::os::config_dir(),
        platform_core::runtime::test_mode(),
    );
    for path in paths {
        let Some(listener) = bind_show_socket(&path) else {
            continue;
        };
        *SHOW_SOCKET
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(path);
        std::thread::Builder::new()
            .name("capture-button-show-socket".into())
            .spawn(move || serve_show_requests(listener, show_window_from_second_start))
            .ok();
        return;
    }
    platform_core::logging::warn(
        "shell: no window-activation socket; a second start cannot reopen the window",
    );
}

/// Binds the socket, replacing a file left behind by a killed instance.
fn bind_show_socket(path: &Path) -> Option<UnixListener> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // A live instance would hold the lock, so anything here is stale; a stale
    // socket file is still bound to nothing and would make `bind` fail.
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path).ok()?;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    Some(listener)
}

/// Serves activation requests until the process ends.
fn serve_show_requests(listener: UnixListener, notify: impl Fn()) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else {
            continue;
        };
        answer_show_request(stream, &notify);
    }
}

/// Reads one short request, asks for the window when it is `show`, and closes
/// the connection, which is how the second instance knows it was served.
fn answer_show_request(mut stream: UnixStream, notify: &impl Fn()) {
    let _ = stream.set_read_timeout(Some(SHOW_TIMEOUT));
    let _ = stream.set_write_timeout(Some(SHOW_TIMEOUT));
    let mut request = String::new();
    {
        let mut reader = BufReader::new(&mut stream).take(MAX_REQUEST);
        let _ = reader.read_line(&mut request);
    }
    if is_show_request(&request) {
        notify();
        let _ = stream.write_all(SHOW_REPLY.as_bytes());
    }
}

/// Whether `request` is the one message the socket understands.
fn is_show_request(request: &str) -> bool {
    request.trim() == "show"
}

/// Asks the application loop to bring the window up: the same `Activate` path
/// the tray "Open window" item uses.
fn show_window_from_second_start() {
    platform_core::logging::info("shell: another start asked for the window");
    enqueue(ShellEvent::Activate);
}

/// Tells the running instance to show its window.
///
/// Best effort: an absent or stale socket is skipped, and a socket that accepts
/// but never answers only costs [`SHOW_TIMEOUT`] before the second instance
/// exits as before.
fn request_show(paths: &[PathBuf]) {
    for path in paths {
        let Ok(mut stream) = UnixStream::connect(path) else {
            continue;
        };
        let _ = stream.set_read_timeout(Some(SHOW_TIMEOUT));
        let _ = stream.set_write_timeout(Some(SHOW_TIMEOUT));
        if stream.write_all(SHOW_REQUEST.as_bytes()).is_err() {
            continue;
        }
        let mut reply = String::new();
        let _ = stream.read_to_string(&mut reply);
        return;
    }
}

/// The activation request for this process's mode.
fn request_show_from_running_instance() {
    request_show(&socket_paths(
        runtime_dir(),
        crate::os::config_dir(),
        platform_core::runtime::test_mode(),
    ));
}

/// Removes the socket on a clean exit. A killed instance leaves the file, and
/// the next start replaces it in [`bind_show_socket`].
fn remove_show_socket() {
    let mut slot = SHOW_SOCKET
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Some(path) = slot.take() {
        let _ = std::fs::remove_file(path);
    }
}

/// Prepares the host. Returns `false` when another instance is already
/// running; it is asked to show its window first. An X11 display is checked
/// lazily.
///
/// The menu entry and the icon are installed in the background: an AppImage
/// does not install itself and a source-tree build has no entry, so without it
/// GNOME draws a placeholder instead of the application icon.
pub fn init() -> bool {
    if !take_single_instance() {
        request_show_from_running_instance();
        eprintln!("Capture Button is already running.");
        return false;
    }
    start_show_socket();
    std::thread::Builder::new()
        .name("capture-button-desktop-entry".into())
        .spawn(crate::desktop::install)
        .ok();
    install_window_icon();
    true
}

/// Publishes the project icon as `_NET_WM_ICON` on the windows of this process.
///
/// The Slint window does not exist yet when `init` runs: the winit backend
/// creates it on the first show, and the application can start in the tray and
/// show it minutes later. A watcher thread therefore keeps asking the window
/// manager for this process's windows and stamps the icon on the first one it
/// finds. Without the icon the dock and the window list fall back to the
/// desktop entry, and a build run from the source tree has no desktop entry,
/// so the desktop draws its placeholder.
pub fn install_window_icon() {
    let Some(icon) = window_icon_data() else {
        platform_core::logging::warn("shell: could not decode the window icon");
        return;
    };
    std::thread::Builder::new()
        .name("capture-button-window-icon".into())
        .spawn(move || set_window_icon(icon))
        .ok();
}

/// Encodes the embedded PNG as the `_NET_WM_ICON` payload: the width and the
/// height followed by straight-alpha ARGB `CARDINAL`s, which is the layout the
/// EWMH specification and the window managers expect.
fn window_icon_data() -> Option<Vec<u32>> {
    let image = image::load_from_memory(crate::desktop::ICON_PNG)
        .ok()?
        .to_rgba8();
    Some(argb_pixels(&image))
}

fn argb_pixels(image: &image::RgbaImage) -> Vec<u32> {
    let mut data = Vec::with_capacity(2 + (image.width() * image.height()) as usize);
    data.push(image.width());
    data.push(image.height());
    data.extend(image.pixels().map(|pixel| {
        let [red, green, blue, alpha] = pixel.0;
        u32::from(alpha) << 24 | u32::from(red) << 16 | u32::from(green) << 8 | u32::from(blue)
    }));
    data
}

/// Waits for a window of this process and sets `_NET_WM_ICON` on it, then ends.
fn set_window_icon(icon: Vec<u32>) {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, PropMode};

    let Ok((conn, screen_num)) = x11rb::connect(None) else {
        return;
    };
    let root = conn.setup().roots[screen_num].root;
    let intern = |name: &str| -> Option<u32> {
        conn.intern_atom(false, name.as_bytes())
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .map(|reply| reply.atom)
            .filter(|atom| *atom != 0)
    };
    let (Some(icon_atom), Some(client_list), Some(pid_atom)) = (
        intern("_NET_WM_ICON"),
        intern("_NET_CLIENT_LIST"),
        intern("_NET_WM_PID"),
    ) else {
        return;
    };
    let bytes: Vec<u8> = icon.iter().flat_map(|value| value.to_ne_bytes()).collect();
    let pid = std::process::id();
    loop {
        let windows = conn
            .get_property(false, root, client_list, AtomEnum::WINDOW, 0, 1024)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .and_then(|reply| reply.value32().map(|values| values.collect::<Vec<_>>()));
        let Some(windows) = windows else {
            return;
        };
        let mut stamped = false;
        for window in windows {
            let owner = conn
                .get_property(false, window, pid_atom, AtomEnum::CARDINAL, 0, 1)
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .and_then(|reply| reply.value32().and_then(|mut values| values.next()));
            if owner != Some(pid) {
                continue;
            }
            let _ = conn.change_property(
                PropMode::REPLACE,
                window,
                icon_atom,
                AtomEnum::CARDINAL,
                32,
                icon.len() as u32,
                &bytes,
            );
            stamped = true;
        }
        let _ = conn.flush();
        if stamped {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// The host window has no Linux equivalent; the Slint window owns the event loop.
pub fn create_window() {}

/// Ends the application loop. The Slint window belongs to the UI thread, so
/// quitting is a `Quit` event rather than a window call; `run` returns and the
/// process ends.
pub fn destroy_window() {
    enqueue(ShellEvent::Quit);
    wake_ui_thread();
    remove_show_socket();
}

/// Delivers events to the application until `Quit`.
pub fn run(mut handler: impl FnMut(ShellEvent)) {
    loop {
        let event = {
            let (queue, wake) = queue();
            let mut guard = queue.lock().unwrap_or_else(|error| error.into_inner());
            while guard.is_empty() {
                guard = wake
                    .wait_timeout(guard, Duration::from_millis(250))
                    .unwrap_or_else(|error| error.into_inner())
                    .0;
            }
            guard.remove(0)
        };
        let ShellEventProxy::Event(event) = event;
        if matches!(event, ShellEvent::Quit) {
            break;
        }
        handler(event);
    }
}

/// Publishes the menu to the StatusNotifierItem the desktop owns.
pub fn build_tray(items: &[TrayEntry]) {
    crate::tray::build(
        items,
        Box::new(|id| {
            enqueue(ShellEvent::Command {
                id,
                notification: 0,
            })
        }),
        Box::new(|| enqueue(ShellEvent::TrayLeft)),
    );
}

pub fn set_tray_item_enabled(id: i32, enabled: bool) {
    crate::tray::set_item_enabled(id, enabled);
}

pub fn set_tray_text(id: i32, text: &str) {
    crate::tray::set_text(id, text);
}

pub fn set_tray_status(text: &str, badge: TrayBadge) {
    crate::tray::set_status(text, badge);
}

/// The hint the settings page shows under the tray-click item: what a click on
/// the icon does on this desktop.
///
/// The gesture belongs to the host, not to the operating system: GNOME's
/// AppIndicator extension opens the menu on a single click and sends `Activate`
/// only on a double click, while KDE and most other StatusNotifier hosts send
/// it on a single click. The desktop tells us which one it is through
/// `XDG_CURRENT_DESKTOP`, so the text is chosen at run time.
pub fn tray_click_hint() -> String {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").ok();
    platform_core::i18n::tr(tray_click_hint_key(desktop.as_deref()))
}

/// The English source string of the hint for one `XDG_CURRENT_DESKTOP` value.
///
/// `None` or an empty value gets the neutral wording, because nothing tells us
/// which click gesture the host will use.
fn tray_click_hint_key(desktop: Option<&str>) -> &'static str {
    let Some(desktop) = desktop.filter(|value| !value.trim().is_empty()) else {
        return "Clicking the icon starts area selection (in GNOME, double-click).";
    };
    let is_gnome = desktop
        .split(':')
        .any(|part| part.trim().eq_ignore_ascii_case("GNOME"));
    if is_gnome {
        "A single click opens the menu; double-click starts area selection, right click opens the menu."
    } else {
        "Clicking the icon starts area selection, right click opens the menu."
    }
}

/// The StatusNotifierItem has a native menu; the desktop opens it itself.
pub fn show_tray_menu(_x: i32, _y: i32) {}

pub fn remove_tray() {
    crate::tray::remove();
}

/// Whether this session can provide global hotkeys at all.
///
/// `true` when the desktop portal offers `GlobalShortcuts` (Wayland: the only
/// way) or when an X11 connection exists for `XGrabKey`. Only a session with
/// neither hides the Settings section.
pub fn hotkeys_supported() -> bool {
    crate::hotkeys_portal::available() || x11_available()
}

/// Who chooses the combinations on this session.
///
/// The portal wins when it is there: it is the only way to get a global key on
/// Wayland, and grabbing the same keys through XWayland as well would make one
/// press trigger the action twice.
pub fn hotkey_choice() -> HotkeyChoice {
    if crate::hotkeys_portal::available() {
        HotkeyChoice::SystemDialog
    } else {
        HotkeyChoice::Field
    }
}

/// Applies the whole hotkey table.
///
/// With the portal the desktop applies and asks for the combinations; the
/// answer arrives later as [`ShellEvent::Hotkeys`]. Without it the X11 grabs
/// are installed here, and an empty entry releases that action.
pub fn register_hotkeys(bindings: &[HotkeyBinding]) -> Result<(), String> {
    if crate::hotkeys_portal::available() {
        return crate::hotkeys_portal::bind(bindings, None);
    }
    register_x11(bindings)
}

/// Asks the desktop for a new combination of `action`, keeping the other rows.
///
/// The portal ignores a proposed trigger for an id it already knows, so the
/// only way to reopen its dialog is to drop that id first; the module does
/// that. Without the portal the settings field captures the press instead, so
/// this is the same as applying the table.
pub fn change_hotkeys(action: HotkeyAction, bindings: &[HotkeyBinding]) -> Result<(), String> {
    if crate::hotkeys_portal::available() {
        return crate::hotkeys_portal::bind(bindings, Some(action));
    }
    let _ = action;
    register_x11(bindings)
}

/// Whether this session has an X11 connection for `XGrabKey`.
///
/// On Wayland this means XWayland: the grab lives on the XWayland root, and
/// the compositor forwards a key to XWayland while an X11 window has focus.
fn x11_available() -> bool {
    x11rb::connect(None).is_ok()
}

/// The fallback: one passive X11 grab per set row, on a background connection.
///
/// The grabs live on that connection, so applying a set restarts the listener:
/// the old connection is closed before the new one grabs its keys, which avoids
/// a stale grab colliding with the new settings.
fn register_x11(bindings: &[HotkeyBinding]) -> Result<(), String> {
    if !x11_available() {
        return Err("not supported".into());
    }
    let mut wanted = Vec::with_capacity(hotkey_defs().len());
    for def in hotkey_defs() {
        let text = bindings
            .iter()
            .find(|binding| binding.action == def.action)
            .map(|binding| binding.combination.clone())
            .unwrap_or_default();
        wanted.push((def.action, parse_setting(&text)?));
    }
    for (index, (_, left)) in wanted.iter().enumerate() {
        for (_, right) in &wanted[index + 1..] {
            if let (Some(left), Some(right)) = (left, right) {
                if left == right {
                    return Err(platform_core::i18n::tr(
                        "The same combination is set for both hotkeys",
                    ));
                }
            }
        }
    }
    stop_hotkey_worker();
    if wanted.iter().all(|(_, hotkey)| hotkey.is_none()) {
        return Ok(());
    }

    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ConnectionExt as _, GrabMode};

    let (conn, screen_num) = x11rb::connect(None)
        .map_err(|error| format!("Global hotkeys need an X11 display: {error}"))?;
    let root = conn.setup().roots[screen_num].root;
    let lock_masks = lock_modifier_masks(&conn)?;
    let mut lock_variants = vec![0u16];
    for mask in &lock_masks {
        let mut with = lock_variants.clone();
        for variant in &mut with {
            *variant |= mask;
        }
        lock_variants.append(&mut with);
    }
    let ignored = lock_variants
        .iter()
        .copied()
        .fold(0u16, |all, variant| all | variant);

    let mut grabs = Vec::new();
    for (action, hotkey) in wanted {
        let Some(hotkey) = hotkey else {
            continue;
        };
        let keycode = keycode_for(&conn, hotkey)?;
        let modifiers = modifier_mask(hotkey);
        grabs.push(GrabBinding {
            keycode,
            modifiers,
            action,
        });
    }

    for binding in &grabs {
        for variant in &lock_variants {
            conn.grab_key(
                false,
                root,
                (binding.modifiers | variant).into(),
                binding.keycode,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
            )
            .map_err(|error| format!("Global hotkeys: {error}"))?
            .check()
            .map_err(|error| {
                format!("The key combination is already taken by another application: {error}")
            })?;
        }
    }
    let _ = conn.flush();

    let stop_flag = Arc::new(AtomicBool::new(false));
    let thread_stop = stop_flag.clone();
    let handle = std::thread::Builder::new()
        .name("capture-button-hotkeys".into())
        .spawn(move || hotkey_loop(conn, grabs, ignored, thread_stop))
        .map_err(|error| format!("Global hotkeys: {error}"))?;
    *HOTKEY_WORKER
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(HotkeyWorker {
        stop: stop_flag,
        handle: Some(handle),
    });
    Ok(())
}

fn parse_setting(text: &str) -> Result<Option<Hotkey>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    parse_hotkey(text)
        .map(Some)
        .ok_or_else(|| format!("Invalid hotkey: {text}"))
}

fn stop_hotkey_worker() {
    let worker = HOTKEY_WORKER
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take();
    if let Some(mut worker) = worker {
        worker.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = worker.handle.take() {
            let _ = handle.join();
        }
    }
}

/// The X11 keysym a shared hotkey key types.
///
/// Letters are stored upper case, but a key carries both cases; the lower-case
/// keysym is the one every layout maps to that key.
fn keysym(key: HotkeyKey) -> u32 {
    match key {
        HotkeyKey::Char(key) => key.to_ascii_lowercase() as u32,
        HotkeyKey::Space => 0x20,
        HotkeyKey::Function(number) => 0xffbe + u32::from(number.saturating_sub(1)),
    }
}
fn modifier_mask(hotkey: Hotkey) -> u16 {
    use x11rb::protocol::xproto::KeyButMask;
    let mut mask = 0u16;
    if hotkey.ctrl {
        mask |= u16::from(KeyButMask::CONTROL);
    }
    if hotkey.alt {
        mask |= u16::from(KeyButMask::MOD1);
    }
    if hotkey.shift {
        mask |= u16::from(KeyButMask::SHIFT);
    }
    if hotkey.meta {
        mask |= u16::from(KeyButMask::MOD4);
    }
    mask
}

/// The keycode that carries the keysym of `hotkey`.
///
/// The server's keyboard mapping is searched instead of calling
/// `XStringToKeysym`: it keeps the backend free of another C dependency and
/// follows the layout the user actually has loaded.
fn keycode_for(conn: &impl x11rb::connection::Connection, hotkey: Hotkey) -> Result<u8, String> {
    use x11rb::protocol::xproto::ConnectionExt as _;

    let setup = conn.setup();
    let first = setup.min_keycode;
    let count = setup.max_keycode.saturating_sub(first).saturating_add(1);
    let mapping = conn
        .get_keyboard_mapping(first, count)
        .map_err(|error| format!("Global hotkeys: {error}"))?
        .reply()
        .map_err(|error| format!("Global hotkeys: {error}"))?;
    let per_keycode = usize::from(mapping.keysyms_per_keycode.max(1));
    let wanted = keysym(hotkey.key);
    mapping
        .keysyms
        .chunks(per_keycode)
        .position(|keysyms| keysyms.contains(&wanted))
        .and_then(|index| u8::try_from(index).ok())
        .and_then(|index| first.checked_add(index))
        .ok_or_else(|| format!("The key for {} is not on this keyboard", hotkey.label()))
}

/// The modifier bits CapsLock and NumLock occupy in this keymap. They are
/// ignored when a key press is matched, because a grab has to be installed for
/// every combination of them.
fn lock_modifier_masks(conn: &impl x11rb::connection::Connection) -> Result<Vec<u16>, String> {
    use x11rb::protocol::xproto::ConnectionExt as _;

    const NUM_LOCK: u32 = 0xff7f;
    const CAPS_LOCK: u32 = 0xffe5;
    let setup = conn.setup();
    let first = setup.min_keycode;
    let count = setup.max_keycode.saturating_sub(first).saturating_add(1);
    let mapping = conn
        .get_keyboard_mapping(first, count)
        .map_err(|error| format!("Global hotkeys: {error}"))?
        .reply()
        .map_err(|error| format!("Global hotkeys: {error}"))?;
    let per_keycode = usize::from(mapping.keysyms_per_keycode.max(1));
    let keysym_of = |keycode: u8| -> u32 {
        let index = usize::from(keycode.saturating_sub(first));
        mapping
            .keysyms
            .get(index * per_keycode)
            .copied()
            .unwrap_or(0)
    };
    let modifiers = conn
        .get_modifier_mapping()
        .map_err(|error| format!("Global hotkeys: {error}"))?
        .reply()
        .map_err(|error| format!("Global hotkeys: {error}"))?;
    let per_modifier = usize::from(modifiers.keycodes_per_modifier().max(1));
    let mut masks = Vec::new();
    for (index, keycodes) in modifiers.keycodes.chunks(per_modifier).enumerate() {
        let is_lock = keycodes
            .iter()
            .any(|keycode| matches!(keysym_of(*keycode), NUM_LOCK | CAPS_LOCK));
        if is_lock && index < 16 {
            masks.push(1u16 << index);
        }
    }
    Ok(masks)
}

fn hotkey_loop(
    conn: x11rb::rust_connection::RustConnection,
    bindings: Vec<GrabBinding>,
    ignored: u16,
    stop: Arc<AtomicBool>,
) {
    use x11rb::connection::Connection;

    while !stop.load(Ordering::SeqCst) {
        loop {
            match conn.poll_for_event() {
                Ok(Some(x11rb::protocol::Event::KeyPress(event))) => {
                    let state = u16::from(event.state) & !ignored;
                    if let Some(binding) = bindings.iter().find(|binding| {
                        binding.keycode == event.detail && binding.modifiers == state
                    }) {
                        enqueue(ShellEvent::Hotkey(binding.action));
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(_) => return,
            }
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}

/// Starts a repeating timer that delivers `ShellEvent::Timer(id)`.
pub fn start_timer(id: usize, ms: u32) {
    let generation = TIMER_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    timers()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(id, generation);
    let interval = Duration::from_millis(ms.max(1) as u64);
    std::thread::Builder::new()
        .name(format!("capture-button-timer-{id}"))
        .spawn(move || loop {
            std::thread::sleep(interval);
            let current = timers()
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .get(&id)
                .copied();
            if current != Some(generation) {
                break;
            }
            enqueue(ShellEvent::Timer(id));
        })
        .ok();
}

pub fn kill_timer(id: usize) {
    timers()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(&id);
}

pub fn show_error(text: &str) {
    eprintln!("{text}");
    let _ = std::process::Command::new("zenity")
        .args(["--error", "--text", text])
        .status();
}

pub fn open_url(url: &str) {
    let _ = std::process::Command::new("xdg-open").arg(url).status();
}

pub fn pick_folder(start: Option<&Path>) -> Option<PathBuf> {
    let mut command = std::process::Command::new("zenity");
    command.args(["--file-selection", "--directory"]);
    if let Some(start) = start {
        command.arg(format!("--filename={}/", start.display()));
    }
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!path.is_empty()).then(|| PathBuf::from(path))
}

pub fn trim_working_set() {}

/// Wakes the application loop so it takes the outcome a Slint overlay published.
pub fn notify_region_closed() {
    enqueue(ShellEvent::Region);
}

pub fn screen_width() -> i32 {
    crate::screen::primary_monitor().w as i32
}

pub fn register_ui_thread() {}

/// Wakes the UI thread that may be parked in [`wait_for_ui_message`].
pub fn wake_ui_thread() {
    let (flag, wake) = ui_wake();
    *flag.lock().unwrap_or_else(|error| error.into_inner()) = true;
    wake.notify_all();
}

/// Wakes the application loop, which then handles its own pending requests.
pub fn wake() {
    enqueue(ShellEvent::Ui);
}

/// Blocks the UI thread until [`wake_ui_thread`] is called.
pub fn wait_for_ui_message() {
    let (flag, wake) = ui_wake();
    let mut guard = flag.lock().unwrap_or_else(|error| error.into_inner());
    while !*guard {
        let (next, timeout) = wake
            .wait_timeout(guard, Duration::from_millis(500))
            .unwrap_or_else(|error| error.into_inner());
        guard = next;
        if timeout.timed_out() {
            break;
        }
    }
    *guard = false;
}

/// The paint guard watches the native host window; Slint owns its window here.
pub fn install_paint_guard(_callback: Box<dyn Fn(PaintEvent) + Send>) {}

/// Asks the X11 window manager to put our overlay fullscreen above everything.
///
/// The application's renderer draws a borderless window the size of the screen,
/// and on X11 a normal window still sits under the main window and the GNOME
/// panel; `_NET_WM_STATE_FULLSCREEN` plus `_NET_WM_STATE_ABOVE` is what actually
/// hides them.
pub fn raise_overlay_window() {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{
        AtomEnum, ClientMessageData, ClientMessageEvent, ConfigureWindowAux, ConnectionExt as _,
        EventMask, StackMode,
    };

    let Ok((conn, screen_num)) = x11rb::connect(None) else {
        return;
    };
    let screen = &conn.setup().roots[screen_num];
    let root = screen.root;
    let width = screen.width_in_pixels;
    let height = screen.height_in_pixels;
    let atom = |name: &str| -> u32 {
        conn.intern_atom(false, name.as_bytes())
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .map(|reply| reply.atom)
            .unwrap_or(0)
    };
    let client_list = atom("_NET_CLIENT_LIST");
    let pid_atom = atom("_NET_WM_PID");
    let state = atom("_NET_WM_STATE");
    let fullscreen = atom("_NET_WM_STATE_FULLSCREEN");
    let above = atom("_NET_WM_STATE_ABOVE");
    if client_list == 0 || pid_atom == 0 || state == 0 {
        return;
    }
    let Ok(clients) = conn.get_property(false, root, client_list, AtomEnum::WINDOW, 0, 1024) else {
        return;
    };
    let Ok(clients) = clients.reply() else {
        return;
    };
    let Some(list) = clients.value32() else {
        return;
    };
    let pid = std::process::id();
    for window in list {
        let Ok(pid_reply) = conn.get_property(false, window, pid_atom, AtomEnum::CARDINAL, 0, 4)
        else {
            continue;
        };
        let Ok(pid_reply) = pid_reply.reply() else {
            continue;
        };
        let owner = pid_reply
            .value32()
            .and_then(|mut values| values.next())
            .unwrap_or(0);
        if owner != pid {
            continue;
        }
        let Ok(geometry) = conn.get_geometry(window) else {
            continue;
        };
        let Ok(geometry) = geometry.reply() else {
            continue;
        };
        if geometry.width < width || geometry.height < height {
            continue;
        }
        for property in [fullscreen, above] {
            if property == 0 {
                continue;
            }
            let event = ClientMessageEvent {
                response_type: x11rb::protocol::xproto::CLIENT_MESSAGE_EVENT,
                format: 32,
                sequence: 0,
                window,
                type_: state,
                data: ClientMessageData::from([1u32, property, 0, 1, 0]),
            };
            let _ = conn.send_event(
                false,
                root,
                EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                event,
            );
        }
        let _ = conn.configure_window(
            window,
            &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
        );
        let _ = conn.flush();
    }
}

/// Restores the main window from the window manager, raises it and focuses it.
///
/// Called before the Slint window is shown again: a visible window is raised
/// here, a minimized one is deiconified by the activation, and a hidden one is
/// recreated by the UI thread, which calls [`activate_app_window`] itself once
/// the new window is up.
///
/// The request is repeated shortly after the first one: the Slint window is
/// shown asynchronously, and a window manager can drop an activation that
/// arrives while it is still applying a minimize or a fresh map. The second
/// request lands after that transition, and repeating it is harmless when the
/// first one already worked.
pub fn restore_app_window() {
    activate_app_window();
    std::thread::Builder::new()
        .name("capture-button-activate".into())
        .spawn(|| {
            std::thread::sleep(ACTIVATE_RETRY);
            activate_app_window();
        })
        .ok();
}

/// Brings the main window in front after the UI thread has shown it.
///
/// The window is this process's Slint window; it is found through
/// `_NET_CLIENT_LIST`, `_NET_WM_PID` and its title, skipping the full-screen
/// area overlay. `_NET_ACTIVE_WINDOW` wakes, raises and focuses it, which also
/// takes a minimized window out of the iconic state.
pub fn activate_app_window() {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{
        AtomEnum, ClientMessageData, ClientMessageEvent, ConnectionExt as _, EventMask,
    };

    let Ok((conn, screen_num)) = x11rb::connect(None) else {
        return;
    };
    let screen = &conn.setup().roots[screen_num];
    let root = screen.root;
    let screen_width = screen.width_in_pixels;
    let screen_height = screen.height_in_pixels;
    let atom = |name: &str| -> Option<u32> {
        conn.intern_atom(false, name.as_bytes())
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .map(|reply| reply.atom)
            .filter(|atom| *atom != 0)
    };
    let (Some(client_list), Some(pid_atom), Some(active)) = (
        atom("_NET_CLIENT_LIST"),
        atom("_NET_WM_PID"),
        atom("_NET_ACTIVE_WINDOW"),
    ) else {
        return;
    };
    let name_atom = atom("_NET_WM_NAME");
    let mask = EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY;
    let time = x11_server_time(&conn, root);
    let Ok(clients) = conn.get_property(false, root, client_list, AtomEnum::WINDOW, 0, 1024) else {
        return;
    };
    let Ok(clients) = clients.reply() else {
        return;
    };
    let Some(list) = clients.value32() else {
        return;
    };
    let pid = std::process::id();
    let mut main_windows = Vec::new();
    let mut fallback = Vec::new();
    for window in list {
        let Ok(reply) = conn.get_property(false, window, pid_atom, AtomEnum::CARDINAL, 0, 4) else {
            continue;
        };
        let Ok(reply) = reply.reply() else {
            continue;
        };
        let owner = reply
            .value32()
            .and_then(|mut values| values.next())
            .unwrap_or(0);
        if owner != pid {
            continue;
        }
        let Ok(geometry) = conn.get_geometry(window) else {
            continue;
        };
        let Ok(geometry) = geometry.reply() else {
            continue;
        };
        let title = name_atom
            .and_then(|name| {
                conn.get_property(false, window, name, AtomEnum::ANY, 0, 64)
                    .ok()
                    .and_then(|cookie| cookie.reply().ok())
            })
            .map(|reply| reply.value)
            .unwrap_or_default();
        if title.as_slice() == b"Capture Button" {
            main_windows.push(window);
        } else if geometry.width < screen_width || geometry.height < screen_height {
            // The overlay is titled "Capture Button area" and covers the
            // screen; this fallback keeps a renamed window reachable.
            fallback.push(window);
        }
    }
    let windows = if main_windows.is_empty() {
        fallback
    } else {
        main_windows
    };
    for window in windows {
        let event = ClientMessageEvent {
            response_type: x11rb::protocol::xproto::CLIENT_MESSAGE_EVENT,
            format: 32,
            sequence: 0,
            window,
            type_: active,
            data: ClientMessageData::from([1u32, time, 0, 0, 0]),
        };
        let _ = conn.send_event(false, root, mask, event);
        let _ = conn.flush();
    }
}

/// The X server's current time.
///
/// Window managers use the timestamp of `_NET_ACTIVE_WINDOW` for focus-stealing
/// prevention and can drop a request stamped 0 as older than the user's last
/// action. The time is read from the `PropertyNotify` of a property change on a
/// throw-away input-only window, which is the portable way to ask the server
/// for its clock. A failure returns 0, which sends the request without a
/// timestamp, the same as before.
fn x11_server_time(conn: &impl x11rb::connection::Connection, root: u32) -> u32 {
    use x11rb::protocol::xproto::{
        AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, PropMode, WindowClass,
    };
    use x11rb::protocol::Event;

    let Ok(window) = conn.generate_id() else {
        return 0;
    };
    let Ok(atom) = conn.intern_atom(false, b"_CAPTURE_BUTTON_TIME") else {
        return 0;
    };
    let Ok(atom) = atom.reply() else {
        return 0;
    };
    if conn
        .create_window(
            0,
            window,
            root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_ONLY,
            0,
            &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )
        .is_err()
    {
        return 0;
    }
    let _ = conn.change_property(
        PropMode::REPLACE,
        window,
        atom.atom,
        AtomEnum::CARDINAL,
        32,
        1,
        &0u32.to_ne_bytes(),
    );
    let _ = conn.flush();
    let mut time = 0;
    for _ in 0..20 {
        match conn.poll_for_event() {
            Ok(Some(Event::PropertyNotify(event))) if event.window == window => {
                time = event.time;
                break;
            }
            Ok(_) => {}
            Err(_) => break,
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let _ = conn.destroy_window(window);
    let _ = conn.flush();
    time
}

pub fn watch_foreground() {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::Arc;
    use std::time::Instant;

    /// A directory unique to one test, so the tests can run in parallel.
    fn temp_dir(name: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "capture-button-lock-{}-{}-{name}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn release() {
        *SINGLE_INSTANCE
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
    }

    /// Sends one line to a running test listener and returns its answer.
    fn send_line(path: &Path, line: &str) -> String {
        let mut stream = UnixStream::connect(path).expect("the test socket must accept");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("a read timeout");
        stream
            .write_all(line.as_bytes())
            .expect("the request must be written");
        let mut reply = String::new();
        stream
            .read_to_string(&mut reply)
            .expect("the listener must answer");
        reply
    }

    /// Starts a listener on a fresh temp socket; the returned flag is set when
    /// the listener asked for the window.
    fn show_socket() -> (PathBuf, PathBuf, Arc<AtomicBool>) {
        let dir = temp_dir("show-socket");
        std::fs::create_dir_all(&dir).expect("the test directory must be created");
        let path = dir.join("capture-button.sock");
        let listener = bind_show_socket(&path).expect("the test socket must bind");
        let asked = Arc::new(AtomicBool::new(false));
        let flag = asked.clone();
        std::thread::Builder::new()
            .name("capture-button-show-socket-test".into())
            .spawn(move || {
                serve_show_requests(listener, move || flag.store(true, Ordering::SeqCst))
            })
            .expect("the test listener must start");
        (dir, path, asked)
    }

    #[test]
    fn icon_payload_is_size_then_argb_pixels() {
        let mut pixels = image::RgbaImage::new(2, 1);
        pixels.put_pixel(0, 0, image::Rgba([10, 20, 30, 40]));
        pixels.put_pixel(1, 0, image::Rgba([1, 2, 3, 255]));
        assert_eq!(argb_pixels(&pixels), vec![2, 1, 0x280A_141E, 0xFF01_0203]);
    }

    #[test]
    fn the_embedded_icon_decodes_to_256_square() {
        let icon = window_icon_data().expect("the embedded icon must decode");
        assert_eq!(&icon[..2], &[256, 256]);
        assert_eq!(icon.len(), 2 + 256 * 256);
    }

    #[test]
    fn quitting_ends_the_loop() {
        destroy_window();
        // `run` returns on the queued Quit without handing it to the loop body.
        run(|_| panic!("quit must end the loop, not reach the handler"));
    }

    #[test]
    fn a_second_start_in_the_same_process_is_refused() {
        let dir = temp_dir("second-start");
        let path = dir.join("capture-button.lock");
        assert!(acquire(std::slice::from_ref(&path)));
        // `init` returns false here, and the application exits.
        assert!(!acquire(std::slice::from_ref(&path)));
        release();
        // Once the first owner is gone the same path can be locked again.
        assert!(acquire(std::slice::from_ref(&path)));
        release();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_held_lock_refuses_a_second_descriptor_and_frees_on_drop() {
        let dir = temp_dir("flock");
        let path = dir.join("capture-button.lock");
        let held = lock(&path)
            .expect("the lock file must be created")
            .expect("the first lock must succeed");
        assert!(lock(&path).expect("the lock file must open").is_none());
        drop(held);
        assert!(lock(&path).expect("the lock file must open").is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_mode_uses_its_own_lock_files() {
        let runtime = PathBuf::from("/run/user/1000");
        let config = PathBuf::from("/home/e2e/.config/capture-button");
        let production = lock_paths(Some(runtime.clone()), config.clone(), false);
        let testing = lock_paths(Some(runtime.clone()), config.clone(), true);
        assert_eq!(
            production,
            vec![
                runtime.join("capture-button.lock"),
                config.join("single-instance.lock"),
            ]
        );
        assert_eq!(
            testing,
            vec![
                runtime.join("capture-button.test.lock"),
                config.join("single-instance.test.lock"),
            ]
        );
        // Without a runtime directory the configuration file is the fallback.
        assert_eq!(
            lock_paths(None, config.clone(), false),
            vec![config.join("single-instance.lock")]
        );
        assert_eq!(
            lock_paths(None, config, true),
            vec![PathBuf::from(
                "/home/e2e/.config/capture-button/single-instance.test.lock"
            )]
        );
    }

    #[test]
    fn only_a_show_line_is_a_request() {
        assert!(is_show_request(SHOW_REQUEST));
        assert!(is_show_request("show"));
        assert!(is_show_request("  show  \n"));
        assert!(!is_show_request(""));
        assert!(!is_show_request("quit\n"));
        assert!(!is_show_request("shows\n"));
        assert!(!is_show_request("show now\n"));
    }

    #[test]
    fn test_mode_uses_its_own_show_socket() {
        let runtime = PathBuf::from("/run/user/1000");
        let config = PathBuf::from("/home/e2e/.config/capture-button");
        assert_eq!(
            socket_paths(Some(runtime.clone()), config.clone(), false),
            vec![
                runtime.join("capture-button.sock"),
                config.join("capture-button.sock"),
            ]
        );
        assert_eq!(
            socket_paths(Some(runtime), config.clone(), true),
            vec![
                PathBuf::from("/run/user/1000/capture-button.test.sock"),
                PathBuf::from("/home/e2e/.config/capture-button/capture-button.test.sock"),
            ]
        );
        // The second instance of a test run never reaches the production window.
        assert_ne!(
            socket_paths(None, config.clone(), false),
            socket_paths(None, config, true)
        );
    }

    #[test]
    fn a_hotkey_key_maps_to_its_x11_keysym() {
        assert_eq!(keysym(HotkeyKey::Char('A')), u32::from(b'a'));
        assert_eq!(keysym(HotkeyKey::Char('7')), u32::from(b'7'));
        assert_eq!(keysym(HotkeyKey::Function(1)), 0xffbe);
        assert_eq!(keysym(HotkeyKey::Function(9)), 0xffc6);
        assert_eq!(keysym(HotkeyKey::Space), 0x20);
    }

    #[test]
    fn the_show_socket_answers_show_and_ignores_anything_else() {
        let (dir, path, asked) = show_socket();

        request_show(std::slice::from_ref(&path));
        assert!(
            asked.swap(false, Ordering::SeqCst),
            "a second start must ask for the window"
        );

        assert_eq!(send_line(&path, "quit\n"), "");
        assert!(
            !asked.load(Ordering::SeqCst),
            "an unknown request must not touch the window"
        );

        let long = format!("{}\n", "a".repeat(200));
        assert_eq!(send_line(&path, &long), "");
        assert!(!asked.load(Ordering::SeqCst));

        assert_eq!(send_line(&path, SHOW_REQUEST), SHOW_REPLY);
        assert!(asked.load(Ordering::SeqCst));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stale_socket_does_not_hang_the_second_instance() {
        let dir = temp_dir("stale-socket");
        std::fs::create_dir_all(&dir).expect("the test directory must be created");
        let path = dir.join("capture-button.sock");
        drop(bind_show_socket(&path).expect("the test socket must bind"));
        assert!(path.exists(), "a killed instance leaves the socket file");

        let start = Instant::now();
        request_show(std::slice::from_ref(&path));
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "a socket without a listener must fail at once, not wait for the timeout"
        );

        // The next start replaces the stale file instead of failing on it.
        assert!(bind_show_socket(&path).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_tray_hint_follows_the_desktop_click_gesture() {
        // GNOME sends Activate on a double click, so its hint says so.
        assert_eq!(
            tray_click_hint_key(Some("ubuntu:GNOME")),
            "A single click opens the menu; double-click starts area selection, right click opens the menu."
        );
        // KDE and other StatusNotifier hosts send it on a single click.
        assert_eq!(
            tray_click_hint_key(Some("KDE")),
            "Clicking the icon starts area selection, right click opens the menu."
        );
        assert_eq!(
            tray_click_hint_key(Some("X-Cinnamon")),
            "Clicking the icon starts area selection, right click opens the menu."
        );
        // Without an answer nothing is promised about the click count.
        assert_eq!(
            tray_click_hint_key(None),
            "Clicking the icon starts area selection (in GNOME, double-click)."
        );
        assert_eq!(
            tray_click_hint_key(Some("")),
            "Clicking the icon starts area selection (in GNOME, double-click)."
        );
        assert_eq!(
            tray_click_hint_key(Some("   ")),
            "Clicking the icon starts area selection (in GNOME, double-click)."
        );
    }
}
