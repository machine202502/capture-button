//! The host message loop, timers and the desktop integration of Linux.
//!
//! There is no tray icon on plain GNOME, so the tray calls are accepted and do
//! nothing: the application keeps working from its window. Timers and the two
//! wake channels emulate the Win32 message loop the application was built on.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Duration;

pub use platform_core::tray::{PaintEvent, ShellEvent, TrayBadge, TrayEntry, TrayGlyph};

pub const TIMER_ID: usize = 1;
pub const HISTORY_ACK_TIMER: usize = 2;
pub const TRAY_RETRY_TIMER: usize = 3;

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

fn enqueue(event: ShellEvent) {
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

/// Prepares the host. Always succeeds: an X11 display is checked lazily.
pub fn init() -> bool {
    true
}

/// The host window has no Linux equivalent; the Slint window owns the event loop.
pub fn create_window() {}

pub fn destroy_window() {}

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

/// The tray is not available on plain GNOME; the entries are accepted and
/// ignored, and the window remains the only surface.
pub fn build_tray(_items: &[TrayEntry]) {}

pub fn set_tray_item_enabled(_id: i32, _enabled: bool) {}

pub fn set_tray_text(_id: i32, _text: &str) {}

pub fn set_tray_status(_text: &str, _badge: TrayBadge) {}

pub fn show_tray_menu(_x: i32, _y: i32) {}

pub fn remove_tray() {}

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

pub fn notify_region_closed() {}

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

pub fn app_in_foreground() -> bool {
    false
}

pub fn main_window_open() -> bool {
    false
}

/// Asks the X11 window manager to put our overlay fullscreen above everything.
///
/// The application's renderer draws a borderless window the size of the screen,
/// and on X11 a normal window still sits under the main window and the GNOME
/// panel; `_NET_WM_STATE_FULLSCREEN` plus `_NET_WM_STATE_ABOVE` is what actually
/// hides them.
pub fn raise_overlay_window() {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{AtomEnum, ClientMessageData, ClientMessageEvent, ConnectionExt as _, EventMask};

    let Ok((conn, screen_num)) = x11rb::connect(None) else {
        return;
    };
    let screen = &conn.setup().roots[screen_num];
    let root = screen.root;
    let width = screen.width_in_pixels as u32;
    let height = screen.height_in_pixels as u32;
    let Ok(clients) = conn.get_property(
        false,
        root,
        x11rb::protocol::xproto::AtomEnum::from(AtomEnum::WINDOW),
        0,
        x11rb::protocol::xproto::AtomEnum::from(AtomEnum::WINDOW),
        0,
        1024,
    ) else {
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
        let Ok(pid_reply) = conn.get_property(false, window, x11rb::protocol::xproto::AtomEnum::from(AtomEnum::CARDINAL), 0, 4) else {
            continue;
        };
        let Ok(pid_reply) = pid_reply.reply() else {
            continue;
        };
        let owner = pid_reply.value32().and_then(|mut values| values.next()).unwrap_or(0);
        if owner != pid {
            continue;
        }
        let Ok(geometry) = conn.get_geometry(window) else {
            continue;
        };
        let Ok(geometry) = geometry.reply() else {
            continue;
        };
        if geometry.width < width as u16 || geometry.height < height as u16 {
            continue;
        }
        let atom = |name: &str| -> u32 {
            conn.intern_atom(false, name.as_bytes())
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .map(|reply| reply.atom)
                .unwrap_or(0)
        };
        let state = atom("_NET_WM_STATE");
        let fullscreen = atom("_NET_WM_STATE_FULLSCREEN");
        let above = atom("_NET_WM_STATE_ABOVE");
        if state != 0 {
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
                    data: ClientMessageData::from([1, property, 0, 1, 0]),
                };
                let _ = conn.send_event(
                    false,
                    root,
                    EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                    event,
                );
            }
        }
        let _ = conn.configure_window(
            window,
            &x11rb::protocol::xproto::ConfigureWindowAux::new().stack_mode(x11rb::protocol::xproto::StackMode::ABOVE),
        );
        let _ = conn.flush();
    }
}

pub fn restore_app_window() {}

pub fn activate_app_window() {}

pub fn watch_foreground() {}
