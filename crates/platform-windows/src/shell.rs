//! The native window, message loop, tray icon and shell dialogs.
//!
//! The app drives this through [`run`], handing it a handler that receives
//! [`ShellEvent`]s. Everything here is Win32; a future backend reimplements the
//! same surface with its own event loop.

use platform_core::i18n::wide;
use crate::palette::{bgra, Palette};
use crate::region;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, IBindCtx, CLSCTX_ALL};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::Controls::{
    DRAWITEMSTRUCT, MEASUREITEMSTRUCT, ODS_DISABLED, ODS_GRAYED, ODS_SELECTED, ODT_MENU,
};
use windows::Win32::UI::HiDpi::*;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const CLASS: PCWSTR = w!("CaptureButton.window");
const TITLE: PCWSTR = w!("Capture Button");
const WM_TRAY: u32 = WM_APP + 1;
const WM_REGION: u32 = WM_APP + 2;
const WM_ACTIVATE_APP: u32 = WM_APP + 3;
const WM_UI: u32 = WM_APP + 4;
const WM_UI_WAKE: u32 = WM_APP + 5;
pub const TIMER_ID: usize = 1;
pub const TRAY_RETRY_TIMER: usize = 3;
pub const HISTORY_ACK_TIMER: usize = 2;
const TRAY_RETRY_LIMIT: u32 = 30;
pub const ID_TRAY_AREA: i32 = 200;
pub const ID_TRAY_SHOT: i32 = 201;
pub const ID_TRAY_VIDEO: i32 = 202;
pub const ID_TRAY_AUDIO: i32 = 203;
pub const ID_TRAY_STOP: i32 = 204;
pub const ID_TRAY_EXIT: i32 = 205;
pub const ID_TRAY_OPEN: i32 = 206;

static mut SHELL: *mut Shell = std::ptr::null_mut();
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
static UI_TID: AtomicU32 = AtomicU32::new(0);
type PaintHook = Box<dyn Fn(PaintEvent) + Send>;
static PAINT_GUARD: Mutex<Option<PaintHook>> = Mutex::new(None);
static PALETTE: std::sync::OnceLock<Palette> = std::sync::OnceLock::new();

fn palette() -> &'static Palette {
    PALETTE.get().unwrap_or_else(|| crate::palette::default())
}

/// Installs the app's palette before the host window is created, so the tray
/// and menu brushes are drawn with the right colors.
pub fn install_palette(palette: platform_core::palette::Palette) {
    let _ = PALETTE.set(Palette::from(palette));
}

pub use platform_core::tray::{PaintEvent, ShellEvent, TrayBadge, TrayEntry, TrayGlyph};

struct Shell {
    hwnd: HWND,
    pending: VecDeque<ShellEvent>,
    tray: NOTIFYICONDATAW,
    tray_menu: HMENU,
    base_icon: HICON,
    status_icon: HICON,
    tray_added: bool,
    tray_retry: u32,
    icon_badge: Option<TrayBadge>,
    surface: HBRUSH,
    menu_icons: Vec<HBITMAP>,
}

/// The executable's icon resource. Windows identifies it by the integer 1, not by a string.
#[allow(clippy::manual_dangling_ptr)]
fn module_icon() -> PCWSTR {
    PCWSTR(1 as *const u16)
}

fn shell<'a>() -> &'a mut Shell {
    unsafe {
        // SAFETY: SHELL was stored in WM_CREATE and is cleared only after the window is destroyed. Callers run on the UI thread.
        &mut *SHELL
    }
}

/// Queues an event for the message loop and wakes the loop.
///
/// A message sent from another thread runs this window procedure while the loop
/// waits in `GetMessageW`; the queued event alone would sit there until some
/// unrelated posted message happened to arrive. Posting `WM_NULL` makes
/// `GetMessageW` return, so the loop drains the queue in order.
fn enqueue(event: ShellEvent) {
    let shell = shell();
    shell.pending.push_back(event);
    unsafe {
        // SAFETY: The handle was created by this process; the message carries no pointer.
        PostMessageW(shell.hwnd, WM_NULL, WPARAM(0), LPARAM(0)).ok();
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

/// Initializes COM, DPI awareness, the single-instance mutex and the TaskbarCreated
/// registration. Returns `false` when another instance is already running (it has
/// been asked to show itself).
pub fn init() -> bool {
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
        let name = if crate::store::test_mode() {
            w!("Local\\CaptureButton.test.single.instance")
        } else {
            w!("Local\\CaptureButton.single.instance")
        };
        let mutex = CreateMutexW(None, false, name).unwrap_or_default();
        if !mutex.0.is_null() && GetLastError() == ERROR_ALREADY_EXISTS {
            if let Ok(hwnd) = FindWindowW(CLASS, TITLE) {
                PostMessageW(hwnd, WM_ACTIVATE_APP, WPARAM(0), LPARAM(0)).ok();
            }
            return false;
        }
        // The mutex HANDLE has no Drop, so leaving it open keeps the
        // single-instance guard alive for the process lifetime.
    }
    true
}

/// Registers the window class, creates the hidden host window and registers the
/// region overlay class. Must run after [`init`] returned `true`.
pub fn create_window() {
    unsafe {
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
                let text = wide(&platform_core::i18n::trf(
                    "Capture Button could not start: {error}",
                    &[("error", &error.to_string())],
                ));
                MessageBoxW(None, PCWSTR(text.as_ptr()), TITLE, MB_OK | MB_ICONERROR);
                std::process::exit(1);
            }
        };
        let _ = window;
    }
}

/// Runs the Win32 message loop until the window is destroyed, calling `handler`
/// for every application-level event.
pub fn run(mut handler: impl FnMut(ShellEvent)) {
    watch_foreground();
    loop {
        match next_event() {
            ShellEvent::Quit => break,
            event => handler(event),
        }
    }
    unsafe {
        if !SHELL.is_null() {
            drop(Box::from_raw(SHELL));
            SHELL = std::ptr::null_mut();
        }
        windows::Win32::System::Com::CoUninitialize();
    }
}

fn next_event() -> ShellEvent {
    loop {
        if let Some(event) = shell().pending.pop_front() {
            return event;
        }
        let mut message = MSG::default();
        let result = unsafe { GetMessageW(&mut message, None, 0, 0).0 };
        if result == 0 || result == -1 {
            return ShellEvent::Quit;
        }
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

// ---- tray ----------------------------------------------------------------

pub fn build_tray(items: &[TrayEntry]) {
    let shell = shell();
    shell.menu_icons.clear();
    shell.tray_menu = unsafe {
        // SAFETY: The menu is stored on the window and destroyed with it.
        CreatePopupMenu()
    }
    .unwrap();
    for item in items {
        match item {
            TrayEntry::Separator => unsafe {
                // SAFETY: The menu belongs to this window.
                AppendMenuW(shell.tray_menu, MF_SEPARATOR, 0, None)
            }
            .unwrap(),
            TrayEntry::Item { id, text, glyph } => {
                unsafe {
                    // SAFETY: The menu belongs to this window, and the item text is NUL-terminated for the duration of the call.
                    AppendMenuW(
                        shell.tray_menu,
                        MF_STRING,
                        *id as usize,
                        PCWSTR(wide(text).as_ptr()),
                    )
                }
                .unwrap();
                let bitmap = menu_bitmap(*glyph);
                shell.menu_icons.push(bitmap);
                let info = MENUITEMINFOW {
                    cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
                    fMask: MIIM_FTYPE | MIIM_DATA,
                    fType: MFT_OWNERDRAW,
                    dwItemData: bitmap.0 as usize,
                    ..Default::default()
                };
                unsafe {
                    // SAFETY: The menu and the bitmap in the info struct both belong to this window.
                    SetMenuItemInfoW(shell.tray_menu, *id as u32, false, &info).ok();
                }
            }
        }
    }
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: shell.hwnd,
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
    shell.base_icon = nid.hIcon;
    shell.status_icon = nid.hIcon;
    shell.tray = nid;
    let background = MENUINFO {
        cbSize: std::mem::size_of::<MENUINFO>() as u32,
        fMask: MIM_BACKGROUND | MIM_APPLYTOSUBMENUS,
        dwStyle: MNS_NOCHECK,
        hbrBack: shell.surface,
        ..Default::default()
    };
    unsafe {
        // SAFETY: The menu and the brush both belong to this window.
        SetMenuInfo(shell.tray_menu, &background).ok();
    }
    if !add_tray_icon() {
        schedule_tray_retry();
    }
}

/// Adds the balloon's icon to the current shell. Explorer owns the icon, so
/// this must be repeatable: it is called at startup, after any failure, and
/// when Explorer announces it has restarted.
fn add_tray_icon() -> bool {
    let shell = shell();
    shell.tray.hWnd = shell.hwnd;
    shell.tray.uID = 1;
    shell.tray.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    shell.tray.uCallbackMessage = WM_TRAY;
    shell.tray.hIcon = shell.status_icon;
    let added = unsafe {
        // SAFETY: The struct describes the tray icon owned by this window,
        // and every field the shell reads is filled above.
        Shell_NotifyIconW(NIM_ADD, &shell.tray)
    }
    .as_bool();
    if added {
        shell.tray_added = true;
        shell.tray_retry = 0;
        unsafe {
            // SAFETY: This timer id belongs to this window. Killing an
            // inactive timer is harmless.
            KillTimer(shell.hwnd, TRAY_RETRY_TIMER)
        }
        .ok();
        true
    } else {
        shell.tray_added = false;
        false
    }
}

fn schedule_tray_retry() {
    let shell = shell();
    if shell.tray_added || shell.tray_retry >= TRAY_RETRY_LIMIT {
        return;
    }
    shell.tray_retry += 1;
    unsafe {
        // SAFETY: The HWND is the main window, and this timer id is killed
        // as soon as the icon is accepted.
        SetTimer(shell.hwnd, TRAY_RETRY_TIMER, 1_000, None)
    };
}

fn retry_tray() {
    let shell = shell();
    if shell.tray_added {
        unsafe {
            // SAFETY: The HWND is the main window, and this timer belongs to it.
            KillTimer(shell.hwnd, TRAY_RETRY_TIMER)
        }
        .ok();
        return;
    }
    // Rebuild the status icon before the add so the shell receives the icon
    // for the mode the app is in right now.
    shell.icon_badge = None;
    shell.pending.push_back(ShellEvent::RefreshTray);
}

fn readd_tray() {
    let shell = shell();
    shell.tray_added = false;
    shell.icon_badge = None;
    shell.pending.push_back(ShellEvent::RefreshTray);
}

pub fn set_tray_item_enabled(id: i32, enabled: bool) {
    let shell = shell();
    unsafe {
        // SAFETY: The menu belongs to the main window, and the id is one of its items.
        EnableMenuItem(
            shell.tray_menu,
            id as u32,
            MF_BYCOMMAND | if enabled { MF_ENABLED } else { MF_GRAYED },
        )
    };
}

pub fn set_tray_text(id: i32, text: &str) {
    let shell = shell();
    let mut text = wide(text);
    let info = MENUITEMINFOW {
        cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
        fMask: MIIM_STRING,
        dwTypeData: PWSTR(text.as_mut_ptr()),
        cch: text.len() as u32,
        ..Default::default()
    };
    unsafe {
        // SAFETY: The text buffer lives through the call and the menu is owned by this window.
        SetMenuItemInfoW(shell.tray_menu, id as u32, false, &info).ok();
    }
}

pub fn set_tray_status(text: &str, badge: TrayBadge) {
    let shell = shell();
    shell.tray.szTip = [0; 128];
    let text = wide(text);
    // A user-editable locale can make this string longer than the fixed
    // NOTIFYICONDATAW field. Cut it instead of panicking (release aborts),
    // and keep the last unit as the NUL terminator.
    let limit = shell.tray.szTip.len().saturating_sub(1);
    let length = text.len().min(limit);
    shell.tray.szTip[..length].copy_from_slice(&text[..length]);
    shell.tray.uFlags = NIF_TIP | NIF_ICON;
    if shell.icon_badge != Some(badge) {
        let icon = tray_status_icon(shell.base_icon, badge);
        if shell.status_icon != shell.base_icon {
            unsafe {
                // SAFETY: The struct describes the tray icon owned by this window, and its size field is set.
                DestroyIcon(shell.status_icon)
            }
            .ok();
        }
        shell.status_icon = icon;
        shell.icon_badge = Some(badge);
    }
    shell.tray.hIcon = shell.status_icon;
    let mut tried_add = false;
    if !shell.tray_added && shell.tray_retry < TRAY_RETRY_LIMIT {
        tried_add = true;
        if !add_tray_icon() {
            schedule_tray_retry();
        }
    }
    let added = unsafe {
        // SAFETY: The struct describes the tray icon owned by this window, and its size field is set.
        Shell_NotifyIconW(NIM_MODIFY, &shell.tray)
    }
    .as_bool();
    if added {
        // NIM_MODIFY succeeding proves the shell owns this icon, even if we
        // had marked it missing.
        shell.tray_added = true;
        shell.tray_retry = 0;
    } else {
        // The shell forgot this icon (usually an Explorer restart that raced
        // the TaskbarCreated broadcast). Add it again, once.
        shell.tray_added = false;
        if !tried_add && shell.tray_retry < TRAY_RETRY_LIMIT && !add_tray_icon() {
            schedule_tray_retry();
        }
    }
}

pub fn show_tray_menu(x: i32, y: i32) {
    let shell = shell();
    unsafe {
        SetForegroundWindow(shell.hwnd);
        TrackPopupMenu(
            shell.tray_menu,
            TPM_RIGHTBUTTON,
            x,
            y,
            0,
            shell.hwnd,
            None,
        );
    }
}

pub fn remove_tray() {
    let shell = shell();
    unsafe {
        // SAFETY: The struct describes the tray icon owned by this window, and its size field is set.
        Shell_NotifyIconW(NIM_DELETE, &shell.tray)
    }
    .ok();
    shell.tray_added = false;
    if shell.status_icon != shell.base_icon {
        unsafe {
            // SAFETY: The icon was created for this window and is no longer the one in use.
            DestroyIcon(shell.status_icon)
        }
        .ok();
    }
}

pub fn destroy_window() {
    unsafe {
        // SAFETY: The menu belongs to the main window, and the id is one of its items.
        DestroyWindow(shell().hwnd)
    }
    .ok();
}

// ---- timers, dialogs, misc ----------------------------------------------

pub fn start_timer(id: usize, ms: u32) {
    unsafe {
        // SAFETY: The HWND is the main window, and this timer id is the one later killed.
        SetTimer(shell().hwnd, id, ms, None)
    };
}

pub fn kill_timer(id: usize) {
    unsafe {
        // SAFETY: The HWND is the main window, and this timer belongs to it.
        KillTimer(shell().hwnd, id)
    }
    .ok();
}

pub fn show_error(text: &str) {
    unsafe {
        // SAFETY: The HWND is a window or child control this program created and still owns.
        MessageBoxW(
            shell().hwnd,
            PCWSTR(wide(text).as_ptr()),
            TITLE,
            MB_OK | MB_ICONERROR,
        )
    };
}

pub fn open_url(url: &str) {
    let url = wide(url);
    unsafe {
        // SAFETY: The URL buffer is NUL-terminated and outlives the call; the
        // owner window belongs to this program.
        ShellExecuteW(
            shell().hwnd,
            w!("open"),
            PCWSTR(url.as_ptr()),
            None,
            None,
            SW_SHOWNORMAL,
        );
    }
}

/// The visible window when this process owns the foreground, so the folder
/// picker centers on it instead of on the hidden command window.
fn dialog_owner(fallback: HWND) -> HWND {
    unsafe {
        // SAFETY: Both calls only read process-wide window state, and the pid
        // pointer is a writable local.
        let foreground = GetForegroundWindow();
        if foreground.is_invalid() || !IsWindowVisible(foreground).as_bool() {
            return fallback;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(foreground, Some(&mut pid));
        if pid == GetCurrentProcessId() {
            foreground
        } else {
            fallback
        }
    }
}

/// Asks for a folder with the standard Windows picker. A cancel or a shell
/// error leaves the setting unchanged.
pub fn pick_folder(start: Option<&Path>) -> Option<PathBuf> {
    unsafe {
        // SAFETY: COM is initialized for this thread in `init`; every interface
        // pointer below is a COM object owned by this function.
        let dialog: IFileOpenDialog = CoCreateInstance(
            &FileOpenDialog,
            None::<&windows::core::IUnknown>,
            CLSCTX_ALL,
        )
        .ok()?;
        let options = dialog.GetOptions().unwrap_or_default();
        dialog
            .SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST)
            .ok()?;
        if let Some(start) = start {
            let start = wide(&start.display().to_string());
            if let Ok(item) = SHCreateItemFromParsingName::<PCWSTR, Option<&IBindCtx>, IShellItem>(
                PCWSTR(start.as_ptr()),
                None,
            ) {
                dialog.SetFolder(&item).ok();
            }
        }
        dialog.Show(dialog_owner(shell().hwnd)).ok()?;
        let item = dialog.GetResult().ok()?;
        let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = name.to_string().ok();
        CoTaskMemFree(Some(name.0.cast()));
        path.map(PathBuf::from)
    }
}

/// Returns the unused pages of a hidden, idle process to the system. This does
/// not reduce commit (the EXE is still mapped), but it keeps the working set of
/// a tray-only instance small until the window is opened again.
pub fn trim_working_set() {
    unsafe {
        // SAFETY: The handle is the current process, and both sizes are SIZE_T(-1),
        // which asks the system to trim the working set.
        SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX).ok();
    }
}

pub fn notify_region_closed() {
    unsafe {
        // SAFETY: The window handle belongs to this program. The message carries no borrowed pointer.
        if !SHELL.is_null() {
            PostMessageW((*SHELL).hwnd, WM_REGION, WPARAM(0), LPARAM(0)).ok();
        }
    }
}

pub fn screen_width() -> i32 {
    unsafe {
        // SAFETY: GetSystemMetrics only reads a process-wide integer metric.
        GetSystemMetrics(SM_CXSCREEN)
    }
}

// ---- Slint host thread hooks ----------------------------------------------

pub fn register_ui_thread() {
    unsafe {
        // SAFETY: GetCurrentThreadId has no preconditions.
        UI_TID.store(GetCurrentThreadId(), Ordering::Relaxed);
    }
}

pub fn wake_ui_thread() {
    let thread = UI_TID.load(Ordering::Relaxed);
    if thread != 0 {
        unsafe {
            // SAFETY: The id belongs to this process's UI thread, and a posted
            // message carries no pointer.
            let _ = PostThreadMessageW(thread, WM_UI_WAKE, WPARAM(0), LPARAM(0));
        }
    }
}

/// Wakes the main window so its loop emits [`ShellEvent::Ui`]. Called by the UI
/// thread when a request is queued.
pub fn wake() {
    unsafe {
        // SAFETY: The handle was created by this process and is only read here;
        // posting a message to a window of this process is safe.
        if !SHELL.is_null() {
            PostMessageW((*SHELL).hwnd, WM_UI, WPARAM(0), LPARAM(0)).ok();
        }
    }
}

/// Pumps this thread's Win32 queue once and returns. The Slint UI thread parks
/// in `WaitMessage` between commands; this keeps winit's hidden event window
/// alive while the window is closed.
pub fn wait_for_ui_message() {
    unsafe {
        // SAFETY: WaitMessage waits on this thread's own queue; the messages
        // drained below are only the ones already addressed to it.
        let _ = WaitMessage();
        let mut message = MSG::default();
        while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

/// Finds the Slint window and subclasses it so the paint guard can nudge it on
/// restore/show/size. `cb` runs on the UI thread.
pub fn install_paint_guard(cb: Box<dyn Fn(PaintEvent) + Send>) {
    *PAINT_GUARD.lock().unwrap_or_else(|e| e.into_inner()) = Some(cb);
    let mut found = 0isize;
    unsafe {
        // SAFETY: EnumWindows only calls the callback with top-level window
        // handles; `found` outlives the enumeration.
        let _ = EnumWindows(Some(find_slint_window), LPARAM(&mut found as *mut isize as isize));
    }
    if found != 0 {
        let installed = unsafe {
            // SAFETY: The handle belongs to this process and stays alive while
            // the Slint event loop runs.
            SetWindowSubclass(HWND(found as *mut _), Some(paint_subclass), 1, 0)
        };
        platform_core::logging::info(format!(
            "paint guard {found} installed {}",
            installed.as_bool()
        ));
        let hwnd = HWND(found as *mut _);
        let style = unsafe {
            // SAFETY: The handle is the Slint window found above.
            GetWindowLongPtrW(hwnd, GWL_STYLE)
        } as u32;
        let ex_style = unsafe {
            // SAFETY: The handle is the Slint window found above.
            GetWindowLongPtrW(hwnd, GWL_EXSTYLE)
        } as u32;
        let resizable =
            style | WS_THICKFRAME.0 | WS_MAXIMIZEBOX.0 | WS_MINIMIZEBOX.0 | WS_SYSMENU.0;
        if resizable != style {
            unsafe {
                // SAFETY: The style belongs to the Slint window; the replacement keeps its other bits.
                SetWindowLongPtrW(hwnd, GWL_STYLE, resizable as isize);
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED,
                );
            }
        }
        platform_core::logging::info(format!(
            "window style {style:#x} ex {ex_style:#x} style_changed {}",
            resizable != style
        ));
        unsafe {
            // SAFETY: The handle is the Slint window found above.
            let _ = RedrawWindow(
                hwnd,
                None,
                HRGN::default(),
                RDW_INVALIDATE | RDW_UPDATENOW | RDW_ALLCHILDREN,
            );
        }
    } else {
        platform_core::logging::warn("paint guard: no Slint window");
    }
}

unsafe extern "system" fn find_slint_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: hwnd was supplied by EnumWindows and is only queried.
    if !unsafe { IsWindowVisible(hwnd).as_bool() } {
        return BOOL(1);
    }
    let mut owner = 0u32;
    // SAFETY: The window handle comes from EnumWindows and the pid pointer is a writable local.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut owner)) };
    // SAFETY: GetCurrentProcessId has no preconditions.
    if owner != unsafe { GetCurrentProcessId() } {
        return BOOL(1);
    }
    let mut title = [0u16; 32];
    // SAFETY: The title buffer is valid for the duration of the call.
    let length = unsafe { GetWindowTextW(hwnd, &mut title) };
    if String::from_utf16_lossy(&title[..length as usize]) != "Capture Button" {
        return BOOL(1);
    }
    let mut rect = Default::default();
    // SAFETY: The RECT is a writable local and hwnd is a valid top-level window.
    if unsafe { GetWindowRect(hwnd, &mut rect) }.is_err() {
        return BOOL(1);
    }
    let area = (rect.right - rect.left) as i64 * (rect.bottom - rect.top) as i64;
    if area > 10_000 {
        // SAFETY: EnumWindows passes the pointer to a live isize local as lparam.
        unsafe {
            *(lparam.0 as *mut isize) = hwnd.0 as isize;
        }
        return BOOL(0);
    }
    BOOL(1)
}

unsafe extern "system" fn paint_subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    if message == WM_GETMINMAXINFO {
        unsafe {
            // SAFETY: Windows passes a writable MINMAXINFO for this message.
            let info = &mut *(lparam.0 as *mut MINMAXINFO);
            info.ptMinTrackSize = POINT { x: 560, y: 520 };
            info.ptMaxTrackSize = POINT {
                x: GetSystemMetrics(SM_CXSCREEN),
                y: GetSystemMetrics(SM_CYSCREEN),
            };
        }
    }
    if message == WM_WINDOWPOSCHANGING {
        unsafe {
            // SAFETY: Windows passes a writable WINDOWPOS for this message.
            let pos = &mut *(lparam.0 as *mut WINDOWPOS);
            if pos.flags.0 & SWP_NOSIZE.0 == 0 {
                pos.cx = pos.cx.clamp(560, GetSystemMetrics(SM_CXSCREEN));
                pos.cy = pos.cy.clamp(520, GetSystemMetrics(SM_CYSCREEN));
            }
        }
    }
    // SAFETY: The subclass chain and all message arguments come from Windows.
    let result = unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
    let restored = message == WM_SIZE && wparam.0 as u32 == SIZE_RESTORED;
    let shown = message == WM_SHOWWINDOW && wparam.0 != 0;
    if restored || shown || message == WM_ACTIVATE {
        let guard = PAINT_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(cb) = guard.as_ref() {
            if message == WM_SIZE {
                let mut rect = Default::default();
                if unsafe {
                    // SAFETY: The RECT is a writable local for this window.
                    GetClientRect(hwnd, &mut rect)
                }
                .is_ok()
                {
                    let width = (rect.right - rect.left).max(0) as u32;
                    let height = (rect.bottom - rect.top).max(0) as u32;
                    if width > 0 && height > 0 {
                        cb(PaintEvent::Size(width, height));
                    }
                }
            } else {
                cb(PaintEvent::Redraw);
            }
        }
        unsafe {
            // SAFETY: The handle is the subclassed Slint window.
            let _ = RedrawWindow(
                hwnd,
                None,
                HRGN::default(),
                RDW_INVALIDATE | RDW_UPDATENOW | RDW_ALLCHILDREN,
            );
            // SAFETY: DwmFlush only waits for the compositor; it has no pointers.
            let _ = DwmFlush();
        }
    }
    result
}

// ---- tray drawing ----------------------------------------------------------

fn measure_tray_item(measure: &mut MEASUREITEMSTRUCT) {
    let mut text = [0u16; 80];
    let len = unsafe {
        // SAFETY: The menu belongs to this window. The buffer outlives the call.
        GetMenuStringW(
            shell().tray_menu,
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
    // A normal light menu: white rows, a light grey highlight, dark text.
    let background = if selected {
        COLORREF(0x00f2f4f5)
    } else {
        COLORREF(0x00ffffff)
    };
    let ink = if grayed {
        COLORREF(0x00aeb6bc)
    } else {
        COLORREF(0x0020282e)
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
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            AlphaBlend(item.hDC, x, y, icon, icon, memory, 0, 0, icon, icon, blend).ok();
            SelectObject(memory, old);
            DeleteDC(memory);
        }
        let mut text = [0u16; 80];
        let len = GetMenuStringW(shell().tray_menu, item.itemID, Some(&mut text), MF_BYCOMMAND)
            .max(0) as usize;
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

fn menu_bitmap(glyph: TrayGlyph) -> HBITMAP {
    let logical = 16i32;
    let scale = 2i32;
    let size = logical * scale;
    let mut small = vec![0u8; (size * size * 4) as usize];
    paint_menu_glyph(&mut small, size, glyph);
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
    pixels.copy_from_slice(&small);
    bitmap
}

/// The Phosphor glyph of a tray menu item.
fn tray_glyph_char(glyph: TrayGlyph) -> &'static str {
    match glyph {
        TrayGlyph::Window => "\u{E1D2}",
        TrayGlyph::Area => "\u{E1DC}",
        TrayGlyph::Shot => "\u{E1CC}",
        TrayGlyph::Camera => "\u{E4DA}",
        TrayGlyph::Video => "\u{E792}",
        TrayGlyph::Audio => "\u{E4D8}",
        TrayGlyph::CameraVideo => "\u{E69A}",
        TrayGlyph::Stop => "\u{E1CC}",
        TrayGlyph::Exit => "\u{E4F6}",
    }
}

/// Loads the Phosphor font once and returns the face name it registered.
fn phosphor_font(size: i32) -> HFONT {
    static FONT: &[u8] = include_bytes!("../../../assets/Phosphor.ttf");
    // SAFETY: The font bytes live for the process; the handle is created per call
    // and deleted by the caller.
    unsafe {
        let count = 0u32;
        AddFontMemResourceEx(FONT.as_ptr().cast(), FONT.len() as u32, None, &count);
        CreateFontW(
            -size,
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET.0 as u32,
            OUT_DEFAULT_PRECIS.0 as u32,
            CLIP_DEFAULT_PRECIS.0 as u32,
            ANTIALIASED_QUALITY.0 as u32,
            0,
            w!("Phosphor"),
        )
    }
}

/// Paints the glyph into an RGBA buffer as the dark menu ink with a per-pixel
/// alpha taken from the antialiased white glyph.
fn paint_menu_glyph(pixels: &mut [u8], size: i32, glyph: TrayGlyph) {
    let ink = palette().TEXT;
    let (ink_b, ink_g, ink_r) = (
        (ink.0 & 0xff) as u8,
        ((ink.0 >> 8) & 0xff) as u8,
        ((ink.0 >> 16) & 0xff) as u8,
    );
    if matches!(glyph, TrayGlyph::Stop) {
        // Phosphor has no filled square, so the stop icon is drawn here: a
        // rounded square with a one-pixel antialiased rim.
        let margin = (size / 4) as f32;
        let radius = (size as f32 / 10.0).max(2.0);
        let left = margin;
        let right = size as f32 - margin;
        for y in 0..size {
            for x in 0..size {
                let px = x as f32 + 0.5;
                let py = y as f32 + 0.5;
                let inside = if (left + radius..right - radius).contains(&px) {
                    py >= margin && py <= size as f32 - margin
                } else if (left + radius..right - radius).contains(&py) {
                    px >= left && px <= right
                } else {
                    let cx = if px < left + radius { left + radius } else { right - radius };
                    let cy = if py < margin + radius { margin + radius } else { size as f32 - margin - radius };
                    ((px - cx).powi(2) + (py - cy).powi(2)).sqrt() <= radius
                };
                if inside {
                    let index = ((y * size + x) * 4) as usize;
                    pixels[index..index + 4].copy_from_slice(&[ink_b, ink_g, ink_r, 255]);
                }
            }
        }
        return;
    }
    let character = tray_glyph_char(glyph);
    unsafe {
        // SAFETY: The DCs and the bitmap are created here and released before return.
        let screen = GetDC(None);
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = size;
        info.bmiHeader.biHeight = -size;
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB.0;
        let mut bits = std::ptr::null_mut();
        let Ok(bitmap) = CreateDIBSection(screen, &info, DIB_RGB_COLORS, &mut bits, None, 0) else {
            ReleaseDC(None, screen);
            return;
        };
        if bits.is_null() {
            DeleteObject(bitmap);
            ReleaseDC(None, screen);
            return;
        }
        let dc = CreateCompatibleDC(screen);
        let old = SelectObject(dc, bitmap);
        let font = phosphor_font((size - 4).max(10));
        let old_font = SelectObject(dc, font);
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, COLORREF(0x00ffffff));
        let mut wide = platform_core::i18n::wide(character);
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: size,
            bottom: size,
        };
        DrawTextW(
            dc,
            &mut wide,
            &mut rect,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
        SelectObject(dc, old_font);
        DeleteObject(font);
        GdiFlush();
        let source = std::slice::from_raw_parts(bits.cast::<u8>(), (size * size * 4) as usize);
        for (index, pixel) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let offset = index * 4;
            let alpha = source[offset]
                .max(source[offset + 1])
                .max(source[offset + 2]);
            *pixel = [ink_b, ink_g, ink_r, alpha];
        }
        SelectObject(dc, old);
        DeleteObject(bitmap);
        DeleteDC(dc);
        ReleaseDC(None, screen);
    }
}

/// The icon the window, the task bar and the tray share. The PNG keeps the
/// antialiased edges that an icon handle loses when Windows draws it into a DIB.
const TRAY_ICON: &[u8] = include_bytes!("../../../assets/CaptureButton-64.png");

fn tray_status_icon(base: HICON, badge: TrayBadge) -> HICON {
    if badge == TrayBadge::Idle || base.is_invalid() {
        return base;
    }
    let Ok(source) = image::load_from_memory(TRAY_ICON) else {
        return base;
    };
    // The shell asks for the system small icon, so build exactly that size: an
    // icon that the shell has to downscale looks soft next to the crisp idle one.
    // The tray uses the system small icon: 16 px at 100 %, 32 px at 200 %.
    let size = (16 * unsafe { GetDpiForSystem() } as i32 / 96).clamp(16, 64);
    let mut pixels = resize_rgba(&source.to_rgba8(), size as u32);
    // The badge geometry is designed for a 32 px icon and scaled with it.
    let unit = size as f32 / 32.0;
    let (fill, edge) = if badge == TrayBadge::Recording {
        (
            bgra(palette().BADGE_RECORDING),
            bgra(palette().BADGE_RECORDING_EDGE),
        )
    } else {
        (
            bgra(palette().BADGE_SAVING),
            bgra(palette().BADGE_SAVING_EDGE),
        )
    };
    stamp_circle_aa(&mut pixels, 23.0 * unit, 23.0 * unit, 8.0 * unit, edge);
    stamp_circle_aa(&mut pixels, 23.0 * unit, 23.0 * unit, 6.0 * unit, fill);
    unsafe {
        // SAFETY: The screen DC, the DIB, the mask and the icon are created here;
        // the GDI objects are released before return.
        let screen = GetDC(None);
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = size;
        info.bmiHeader.biHeight = -size;
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB.0;
        let mut bits = std::ptr::null_mut();
        let color = match CreateDIBSection(screen, &info, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(bitmap) if !bits.is_null() => bitmap,
            _ => {
                ReleaseDC(None, screen);
                return base;
            }
        };
        let buffer = std::slice::from_raw_parts_mut(bits.cast::<u8>(), (size * size * 4) as usize);
        for (index, pixel) in pixels.as_raw().as_chunks::<4>().0.iter().enumerate() {
            let offset = index * 4;
            buffer[offset] = pixel[2];
            buffer[offset + 1] = pixel[1];
            buffer[offset + 2] = pixel[0];
            buffer[offset + 3] = pixel[3];
        }
        // A 1-bit mask where a set bit means transparent, so the icon keeps the
        // PNG's holes in shells that still consult it.
        let stride = ((size + 15) / 16) * 2;
        let mut mask_bits = vec![0u8; (stride * size) as usize];
        for (index, pixel) in pixels.as_raw().as_chunks::<4>().0.iter().enumerate() {
            if pixel[3] < 128 {
                let x = index % size as usize;
                let y = index / size as usize;
                mask_bits[y * stride as usize + x / 8] |= 0x80 >> (x % 8);
            }
        }
        let mask = CreateBitmap(size, size, 1, 1, Some(mask_bits.as_ptr().cast()));
        let icon_info = ICONINFO {
            fIcon: true.into(),
            hbmColor: color,
            hbmMask: mask,
            ..Default::default()
        };
        let icon = CreateIconIndirect(&icon_info).unwrap_or(base);
        DeleteObject(color);
        DeleteObject(mask);
        ReleaseDC(None, screen);
        icon
    }
}


/// Downscales RGBA through premultiplied alpha, so the transparent corners do
/// not bleed dark edges into the icon's border.
fn resize_rgba(source: &image::RgbaImage, size: u32) -> image::RgbaImage {
    let mut premultiplied = source.clone();
    for pixel in premultiplied.pixels_mut() {
        let alpha = pixel[3] as u16;
        for channel in 0..3 {
            pixel[channel] = (pixel[channel] as u16 * alpha / 255) as u8;
        }
    }
    let mut resized = image::imageops::resize(
        &premultiplied,
        size,
        size,
        image::imageops::FilterType::Lanczos3,
    );
    for pixel in resized.pixels_mut() {
        let alpha = pixel[3] as f32;
        if alpha > 0.0 {
            for channel in 0..3 {
                pixel[channel] = (pixel[channel] as f32 * 255.0 / alpha).min(255.0) as u8;
            }
        }
    }
    resized
}

/// Stamps a filled circle with a one-pixel antialiased rim over RGBA pixels.
/// The color is BGRA, as the rest of the GDI code stores its colours.
fn stamp_circle_aa(pixels: &mut image::RgbaImage, cx: f32, cy: f32, radius: f32, color: [u8; 4]) {
    let color = [color[2], color[1], color[0], color[3]];
    let (width, height) = pixels.dimensions();
    let left = (cx - radius - 1.0).floor().max(0.0) as u32;
    let right = (cx + radius + 1.0).ceil().min(width as f32) as u32;
    let top = (cy - radius - 1.0).floor().max(0.0) as u32;
    let bottom = (cy + radius + 1.0).ceil().min(height as f32) as u32;
    for y in top..bottom {
        for x in left..right {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            let distance = (dx * dx + dy * dy).sqrt();
            let coverage = (radius + 0.5 - distance).clamp(0.0, 1.0);
            if coverage <= 0.0 {
                continue;
            }
            let alpha = coverage * (color[3] as f32 / 255.0);
            let pixel = pixels.get_pixel_mut(x, y);
            for channel in 0..3 {
                pixel[channel] = (color[channel] as f32 * alpha
                    + pixel[channel] as f32 * (1.0 - alpha))
                    .round()
                    .clamp(0.0, 255.0) as u8;
            }
            pixel[3] = ((alpha + pixel[3] as f32 / 255.0 * (1.0 - alpha)) * 255.0)
                .round()
                .clamp(0.0, 255.0) as u8;
        }
    }
}

/// True while a window of this process owns the foreground.
pub fn app_in_foreground() -> bool {
    unsafe {
        // SAFETY: The call has no arguments and the process id lands in a local.
        let foreground = GetForegroundWindow();
        if foreground.is_invalid() {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(foreground, Some(&mut pid));
        pid == GetCurrentProcessId()
    }
}

#[link(name = "user32")]
extern "system" {
    /// Not exposed by the `windows` crate build in use; the system call is stable.
    fn AttachThreadInput(id_attach: u32, id_attach_to: u32, attach: i32) -> i32;
}

/// A topmost window can still sit under the shell's task bar until it is
/// activated, which shows as a strip of the task bar over the overlay's bottom
/// edge. Raising it to the top of the topmost band fixes that without taking
/// the foreground from the user.
pub fn raise_overlay_window() {
    // The overlay covers a whole monitor, so it is the largest window of this
    // process; looking it up by size also works when the toolkit has not marked
    // it topmost yet.
    let Some(hwnd) = largest_window() else {
        return;
    };
    unsafe {
        // SAFETY: The window belongs to this process.
        // The Windows 11 task bar is a topmost XAML island, so a single
        // HWND_TOPMOST is not enough: toggling through NOTOPMOST puts the window
        // at the very top of the topmost band, and the activation keeps it there.
        let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW;
        SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, flags).ok();
        SetWindowPos(hwnd, HWND_NOTOPMOST, 0, 0, 0, 0, flags).ok();
        SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, flags).ok();
        // A background process is not allowed to take the foreground; attaching to
        // the foreground thread makes the activation allowed, so the overlay ends
        // up above the shell's own topmost task bar.
        let foreground = GetForegroundWindow();
        if !foreground.is_invalid() && foreground != hwnd {
            let mut pid = 0u32;
            let foreground_thread = GetWindowThreadProcessId(foreground, Some(&mut pid));
            let current_thread = GetCurrentThreadId();
            if foreground_thread != 0 && foreground_thread != current_thread {
                AttachThreadInput(foreground_thread, current_thread, 1);
                SetForegroundWindow(hwnd);
                AttachThreadInput(foreground_thread, current_thread, 0);
            } else {
                SetForegroundWindow(hwnd);
            }
        } else {
            SetForegroundWindow(hwnd);
        }
    }
}

/// Restores the main window from the task bar and focuses it.
pub fn restore_app_window() {
    let Some(hwnd) = own_window(false) else {
        return;
    };
    unsafe {
        // SAFETY: The window belongs to this process.
        if IsIconic(hwnd).as_bool() {
            ShowWindow(hwnd, SW_RESTORE);
        } else {
            ShowWindow(hwnd, SW_SHOW);
        }
        SetForegroundWindow(hwnd);
        SetFocus(hwnd);
    }
}

/// Brings the main window in front after the UI thread has shown it.
pub fn activate_app_window() {
    let Some(hwnd) = own_window(false) else {
        return;
    };
    if !unsafe {
        // SAFETY: The window belongs to this process.
        IsWindowVisible(hwnd).as_bool()
    } {
        return;
    }
    unsafe {
        // SAFETY: The window belongs to this process.
        SetForegroundWindow(hwnd);
        SetFocus(hwnd);
    }
}

/// The largest visible window of this process: the area overlay.
fn largest_window() -> Option<HWND> {
    struct Search {
        pid: u32,
        area: i64,
        found: HWND,
    }
    unsafe extern "system" fn look(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: `lparam` is the Search created below and it outlives the enumeration.
        let search = &mut *(lparam.0 as *mut Search);
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid != search.pid {
            return TRUE;
        }
        let mut class = [0u16; 64];
        let length = GetClassNameW(hwnd, &mut class).max(0) as usize;
        if String::from_utf16_lossy(&class[..length]) != "Window Class" {
            return TRUE;
        }
        let mut rect = RECT::default();
        GetWindowRect(hwnd, &mut rect).ok();
        let area = (rect.right - rect.left) as i64 * (rect.bottom - rect.top) as i64;
        if area > search.area {
            search.area = area;
            search.found = hwnd;
        }
        TRUE
    }
    let mut search = Search {
        pid: std::process::id(),
        area: 0,
        found: HWND::default(),
    };
    // SAFETY: The callback only reads and writes the search struct it is given.
    unsafe {
        EnumWindows(Some(look), LPARAM(&mut search as *mut Search as isize)).ok();
    }
    (!search.found.is_invalid()).then_some(search.found)
}

/// The last two foreground changes, so the tray item can tell "the window was
/// in front until the tray click moved the foreground to the task bar".
struct ForegroundState {
    /// The main window owns the foreground.
    current: bool,
    /// The main window owned it before the latest change.
    previous: bool,
    /// The latest foreground window belongs to the shell (task bar or menu).
    shell: bool,
    changed: std::time::Instant,
}

static FOREGROUND_STATE: std::sync::OnceLock<std::sync::Mutex<ForegroundState>> =
    std::sync::OnceLock::new();
static FOREGROUND_HOOK: std::sync::atomic::AtomicIsize =
    std::sync::atomic::AtomicIsize::new(0);

fn foreground_state() -> &'static std::sync::Mutex<ForegroundState> {
    FOREGROUND_STATE.get_or_init(|| {
        std::sync::Mutex::new(ForegroundState {
            current: false,
            previous: false,
            shell: false,
            changed: std::time::Instant::now(),
        })
    })
}

fn is_main_window(hwnd: HWND) -> bool {
    let Some(main) = own_window(false) else {
        return false;
    };
    main == hwnd
}

unsafe extern "system" fn on_foreground(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    _object: i32,
    _child: i32,
    _thread: u32,
    _time: u32,
) {
    if event != EVENT_SYSTEM_FOREGROUND {
        return;
    }
    let ours = is_main_window(hwnd);
    let mut class = [0u16; 64];
    let length = GetClassNameW(hwnd, &mut class).max(0) as usize;
    let name = String::from_utf16_lossy(&class[..length]);
    let shell = matches!(
        name.as_str(),
        "Shell_TrayWnd"
            | "Shell_SecondaryTrayWnd"
            | "NotifyIconOverflowWindow"
            | "TrayNotifyWnd"
            | "Windows.UI.Core.CoreWindow"
            | "#32768"
    );
    let mut state = foreground_state()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    state.previous = state.current;
    state.current = ours;
    state.shell = shell;
    state.changed = std::time::Instant::now();
    drop(state);
    // The shell raises its task bar when the tray is clicked; while the area
    // overlay is open it must stay above that task bar.
    if shell {
        if let Some(overlay) = overlay_window() {
            // SAFETY: The window belongs to this process.
            SetWindowPos(
                overlay,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            )
            .ok();
        }
    }
}

/// Starts watching the foreground window. Called once, before the message loop,
/// so the tray item always knows whether the main window was in front.
pub fn watch_foreground() {
    if FOREGROUND_HOOK.load(std::sync::atomic::Ordering::Relaxed) != 0 {
        return;
    }
    unsafe {
        // SAFETY: The hook callback only reads the window list and updates the
        // state above; it is installed once for the lifetime of the process.
        let hook = SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(on_foreground),
            0,
            0,
            0,
        );
        FOREGROUND_HOOK.store(hook.0 as isize, std::sync::atomic::Ordering::Relaxed);
    }
}

/// The area overlay when it is open: a window clearly larger than the main one.
fn overlay_window() -> Option<HWND> {
    let overlay = largest_window()?;
    let main = own_window(false)?;
    let mut overlay_rect = RECT::default();
    let mut main_rect = RECT::default();
    // SAFETY: Both windows belong to this process.
    unsafe {
        GetWindowRect(overlay, &mut overlay_rect).ok();
        GetWindowRect(main, &mut main_rect).ok();
    }
    let overlay_area = (overlay_rect.right - overlay_rect.left) as i64
        * (overlay_rect.bottom - overlay_rect.top) as i64;
    let main_area = (main_rect.right - main_rect.left) as i64 * (main_rect.bottom - main_rect.top) as i64;
    (overlay != main && overlay_area > main_area * 3 / 2).then_some(overlay)
}

/// True when the main window is visible, restored and owns the foreground.
pub fn main_window_open() -> bool {
    let Some(hwnd) = own_window(false) else {
        return false;
    };
    unsafe {
        // SAFETY: The window belongs to this process.
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return false;
        }
        if GetForegroundWindow() == hwnd {
            return true;
        }
    }
    // Clicking the tray icon hands the foreground to the task bar, so the window
    // was in front only a moment ago. Treat that as "open": the item then says
    // "Close window" and hides it instead of raising it.
    let state = foreground_state()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    !state.current
        && state.previous
        && state.shell
        && state.changed.elapsed() < std::time::Duration::from_millis(1500)
}

/// Finds a window of this process: the overlay is topmost, the main window is not.
fn own_window(topmost: bool) -> Option<HWND> {
    struct Search {
        pid: u32,
        topmost: bool,
        found: HWND,
    }
    unsafe extern "system" fn look(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: `lparam` is the Search created below and it outlives the enumeration.
        let search = &mut *(lparam.0 as *mut Search);
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid != search.pid {
            return TRUE;
        }
        let mut class = [0u16; 64];
        let length = GetClassNameW(hwnd, &mut class).max(0) as usize;
        if String::from_utf16_lossy(&class[..length]) != "Window Class" {
            return TRUE;
        }
        let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let is_topmost = ex_style & WS_EX_TOPMOST.0 != 0;
        if is_topmost == search.topmost {
            search.found = hwnd;
            return FALSE;
        }
        TRUE
    }
    let mut search = Search {
        pid: std::process::id(),
        topmost,
        found: HWND::default(),
    };
    // SAFETY: The callback only reads and writes the search struct it is given.
    unsafe {
        EnumWindows(Some(look), LPARAM(&mut search as *mut Search as isize)).ok();
    }
    (!search.found.is_invalid()).then_some(search.found)
}

// ---- window proc ------------------------------------------------------------

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
        if !SHELL.is_null() {
            readd_tray();
        }
        return LRESULT(0);
    }
    if SHELL.is_null() && message != WM_CREATE {
        return DefWindowProcW(hwnd, message, wparam, lparam);
    }
    match message {
        WM_CREATE => {
            SHELL = Box::into_raw(Box::new(Shell {
                hwnd,
                pending: VecDeque::new(),
                tray: NOTIFYICONDATAW::default(),
                tray_menu: HMENU::default(),
                base_icon: HICON::default(),
                status_icon: HICON::default(),
                tray_added: false,
                tray_retry: 0,
                icon_badge: None,
                surface: unsafe {
                    // SAFETY: The color is a plain COLORREF, stored for the process lifetime.
                    CreateSolidBrush(palette().SURFACE)
                },
                menu_icons: Vec::new(),
            }));
            LRESULT(0)
        }
        WM_COMMAND => {
            enqueue(ShellEvent::Command {
                id: command_id(wparam.0),
                notification: notification(wparam.0),
            });
            LRESULT(0)
        }
        WM_UI => {
            enqueue(ShellEvent::Ui);
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
            let id = wparam.0;
            if id == TRAY_RETRY_TIMER {
                retry_tray();
            } else {
                enqueue(ShellEvent::Timer(id));
            }
            LRESULT(0)
        }
        WM_TRAY => {
            match low(lparam.0 as usize) as u32 {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK | 1024 | 1025 => {
                    enqueue(ShellEvent::TrayLeft);
                }
                WM_RBUTTONUP | WM_CONTEXTMENU => {
                    let mut point = POINT::default();
                    GetCursorPos(&mut point).ok();
                    enqueue(ShellEvent::TrayContext {
                        x: point.x,
                        y: point.y,
                    });
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_REGION => {
            enqueue(ShellEvent::Region);
            LRESULT(0)
        }
        WM_ACTIVATE_APP => {
            enqueue(ShellEvent::Activate);
            LRESULT(0)
        }
        WM_CLOSE => {
            enqueue(ShellEvent::Close);
            LRESULT(0)
        }
        WM_DESTROY => {
            KillTimer(hwnd, TIMER_ID).ok();
            KillTimer(hwnd, TRAY_RETRY_TIMER).ok();
            KillTimer(hwnd, HISTORY_ACK_TIMER).ok();
            region::shutdown();
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 32×32 icon whose centre is opaque and whose corners are holes.
    fn base_icon() -> HICON {
        unsafe {
            // SAFETY: The bitmaps and the DC are created here and released before return.
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
            let color = CreateDIBSection(screen, &info, DIB_RGB_COLORS, &mut bits, None, 0).unwrap();
            let pixels = std::slice::from_raw_parts_mut(bits.cast::<u8>(), 32 * 32 * 4);
            for (index, pixel) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let (x, y) = ((index % 32) as i32, (index / 32) as i32);
                let inside = (6..26).contains(&x) && (6..26).contains(&y);
                pixel.copy_from_slice(if inside {
                    &[0x40, 0x80, 0xc0, 0xff]
                } else {
                    &[0, 0, 0, 0]
                });
            }
            let mut mask_bits = [0u8; 128];
            for y in 0..32 {
                for x in 0..32 {
                    let inside = (6..26).contains(&x) && (6..26).contains(&y);
                    if !inside {
                        mask_bits[y * 4 + x / 8] |= 0x80 >> (x % 8);
                    }
                }
            }
            let mask = CreateBitmap(32, 32, 1, 1, Some(mask_bits.as_ptr().cast()));
            let icon_info = ICONINFO {
                fIcon: true.into(),
                hbmColor: color,
                hbmMask: mask,
                ..Default::default()
            };
            let icon = CreateIconIndirect(&icon_info).unwrap();
            DeleteObject(color);
            DeleteObject(mask);
            DeleteDC(dc);
            ReleaseDC(None, screen);
            icon
        }
    }



    #[test]
    fn the_recording_badge_keeps_the_icon_transparent() {
        unsafe {
            // SAFETY: Setting the process DPI awareness is a process-wide flag.
            SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2).ok();
        }
        let base = base_icon();
        let icon = tray_status_icon(base, TrayBadge::Recording);
        unsafe {
            // SAFETY: The icons are ours and the pixels are read into a local buffer.
            assert_ne!(icon, base, "the badge must produce a new icon");
            let mut info = ICONINFO::default();
            assert!(GetIconInfo(icon, &mut info).is_ok());
            let size = (16 * GetDpiForSystem() as i32 / 96).clamp(16, 64);
            let screen = GetDC(None);
            let mut dib: BITMAPINFO = std::mem::zeroed();
            dib.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            dib.bmiHeader.biWidth = size;
            dib.bmiHeader.biHeight = -size;
            dib.bmiHeader.biPlanes = 1;
            dib.bmiHeader.biBitCount = 32;
            dib.bmiHeader.biCompression = BI_RGB.0;
            let mut pixels = vec![0u8; (size * size * 4) as usize];
            let rows = GetDIBits(
                screen,
                info.hbmColor,
                0,
                size as u32,
                Some(pixels.as_mut_ptr().cast()),
                &mut dib,
                DIB_RGB_COLORS,
            );
            ReleaseDC(None, screen);
            assert_eq!(rows, size, "the icon colour bitmap was read");
            assert!(
                pixels[3] < 8,
                "the icon's transparent corner must stay transparent"
            );
            let centre = ((size / 2) * size + size / 2) * 4;
            assert_eq!(
                pixels[centre as usize + 3], 255,
                "the icon itself must stay opaque"
            );
            let unit = size as f32 / 32.0;
            let (bx, by) = ((23.0 * unit) as i32, (23.0 * unit) as i32);
            let badge = ((by * size + bx) * 4) as usize;
            assert_eq!(pixels[badge + 3], 255, "the badge must be opaque");
            assert!(
                pixels[badge + 2] > 150 && pixels[badge] < 120,
                "the recording badge must stay red (BGRA)"
            );
            DeleteObject(info.hbmColor);
            DeleteObject(info.hbmMask);
            DestroyIcon(icon);
            DestroyIcon(base);
        }
    }
}
