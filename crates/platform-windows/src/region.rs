//! The Win32 area overlay.
//!
//! It owns the window, the GDI painting, the text-editor child window and the
//! screen grabs. The overlay's state, layout, hit-testing, text-box geometry and
//! button/key policy live in [`platform_core::region::Model`]; this module
//! performs the [`platform_core::region::Action`]s that model returns.

use crate::palette::Palette;
use platform_core::geom::{self, Panel};
use platform_core::i18n::tr;
use platform_core::types::{Frame, Rect};

pub use platform_core::region::{Outcome, Overlay};

use platform_core::region::{
    caption, handles, icon, role_enabled, text_commit_box, Action, Button, Drag,
    FontSeed, Handle, Key, Metrics, Model, Point, Role, Selection, Shape, ShapeKind, Tool,
};
use std::path::PathBuf;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::Dialogs::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const CLASS: PCWSTR = w!("CaptureButton.region");
static mut REGION: *mut Region = std::ptr::null_mut();
static mut EDIT_PROC: isize = 0;
static PALETTE: std::sync::OnceLock<Palette> = std::sync::OnceLock::new();
static CORE_PALETTE: std::sync::OnceLock<platform_core::palette::Palette> =
    std::sync::OnceLock::new();

fn palette() -> &'static Palette {
    PALETTE.get().unwrap_or_else(|| crate::palette::default())
}

/// The neutral palette the model works in, until the application installs its own.
fn core_palette() -> platform_core::palette::Palette {
    *CORE_PALETTE.get_or_init(|| platform_core::palette::DEFAULT)
}

/// The two overlay fonts, in the form the neutral layout measures with.
struct Fonts {
    body: HFONT,
    icon: HFONT,
}

impl Metrics for Fonts {
    fn text_width(&self, text: &str, font: FontSeed, _scale: u32) -> i32 {
        let font = match font {
            FontSeed::Body => self.body,
            FontSeed::Icon => self.icon,
        };
        unsafe {
            // SAFETY: The DC is obtained here and released in the same call. measure restores the font.

            let dc = GetDC(None);
            let width = measure(dc, font, text);
            ReleaseDC(None, dc);
            width
        }
    }
}

pub struct Region {
    model: Model,
    hwnd: HWND,
    edit: HWND,
    edit_font: HFONT,
    font: HFONT,
    icon_font: HFONT,
    font_resource: HANDLE,
    font_scale: u32,
    back_dc: HDC,
    back_bmp: HBITMAP,
    back_w: u32,
    back_h: u32,
}

impl Region {
    /// The overlay freezes the screen without the pointer. A pointer baked into the
    /// backdrop never moves and reads as a second cursor next to the live one. A
    /// screenshot is also always taken without it; no profile can add one.
    fn new(hwnd: HWND, overlay: Overlay, allow_record: bool) -> Result<Box<Self>, String> {
        let current = crate::os::cursor_monitor_rect();
        let frame = if allow_record {
            crate::screen::grab_still(current)?
        } else {
            crate::screen::grab_display(current)?
        };
        let mut region = Self::with_frame(hwnd, overlay, allow_record, frame, current);
        let monitors = monitor_rects();
        region.model.current_monitor = monitors
            .iter()
            .position(|rect| same_rect(*rect, current))
            .unwrap_or(0);
        region.model.monitors = monitors;
        Ok(region)
    }

    fn with_frame(
        hwnd: HWND,
        overlay: Overlay,
        allow_record: bool,
        frame: Frame,
        monitor: Rect,
    ) -> Box<Self> {
        Box::new(Self::with_model(
            hwnd,
            Model::new(overlay, allow_record, frame, monitor, core_palette()),
        ))
    }

    fn with_model(hwnd: HWND, model: Model) -> Self {
        Self {
            model,
            hwnd,
            edit: HWND::default(),
            edit_font: HFONT::default(),
            font: HFONT::default(),
            icon_font: HFONT::default(),
            font_resource: HANDLE::default(),
            font_scale: 0,
            back_dc: HDC::default(),
            back_bmp: HBITMAP::default(),
            back_w: 0,
            back_h: 0,
        }
    }

    fn show(&mut self) {
        self.layout();
        // Paint before the window is visible: its first composition then shows
        // the scene, not a black surface waiting for the first WM_PAINT.
        render(self);
        unsafe {
            // SAFETY: The HWND is a window or child control this program created and still owns.

            let disabled = 1i32;
            DwmSetWindowAttribute(
                self.hwnd,
                DWMWA_TRANSITIONS_FORCEDISABLED,
                (&disabled as *const i32).cast(),
                4,
            )
            .ok();
            SetWindowDisplayAffinity(self.hwnd, WDA_EXCLUDEFROMCAPTURE).ok();
            ShowWindow(self.hwnd, SW_SHOW);
            // The task bar is topmost too: until the overlay is at the top of the
            // topmost band, its bottom edge can stay visible over ours.
            SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )
            .ok();
            SetForegroundWindow(self.hwnd);
            SetFocus(self.hwnd);
            RedrawWindow(
                self.hwnd,
                None,
                None,
                RDW_INVALIDATE | RDW_ERASE | RDW_UPDATENOW | RDW_ALLCHILDREN,
            )
            .ok();
        }
    }

    /// The fonts are current before anything measures with them.
    fn metrics(&mut self) -> Fonts {
        self.ensure_fonts();
        Fonts {
            body: self.font,
            icon: self.icon_font,
        }
    }

    fn layout(&mut self) {
        let metrics = self.metrics();
        self.model.layout(&metrics);
    }

    /// Performs what the model decided it cannot do itself.
    fn perform(&mut self, action: Action) {
        match action {
            Action::None => {}
            Action::Redraw => self.redraw(),
            Action::OpenTextEditor {
                at,
                width,
                height,
                text,
            } => self.open_text_editor(at, width, height, text),
            Action::CloseTextEditor => self.close_text_editor(),
            Action::SaveDialog => self.save(true),
            Action::NextMonitor => self.next_monitor(),
            Action::Finish(outcome) => self.finish(outcome),
            Action::Copy => self.save(false),
            Action::Save => self.save(false),
        }
    }

    fn finish(&mut self, outcome: Outcome) {
        if self.model.outcome.is_some() {
            // A second click, Alt+F4 or an auto-repeated key must not post a
            // second notification after the overlay has been destroyed.
            return;
        }
        self.commit_text();
        self.model.outcome = Some(outcome);
        unsafe {
            // SAFETY: The HWND is a window or child control this program created and still owns.

            ShowWindow(self.hwnd, SW_HIDE);
            SetWindowDisplayAffinity(self.hwnd, WDA_NONE).ok();
            crate::shell::notify_region_closed();
        }
    }

    fn crop(&self) -> Option<Frame> {
        let selection = self.model.selected()?;
        let mut frame = platform_core::region::crop_pixels(&self.model)?;
        draw_annotations(
            &mut frame,
            &self.model.shapes,
            Point {
                x: -selection.x,
                y: -selection.y,
            },
        );
        Some(frame)
    }

    fn save(&mut self, choose_path: bool) {
        if self.model.outcome.is_some() {
            return;
        }
        self.commit_text();
        let Some(frame) = self.crop() else { return };
        let path = if choose_path { save_dialog() } else { None };
        if choose_path && path.is_none() {
            return;
        }
        self.finish(Outcome::Saved(frame, path));
    }

    fn begin_text_move(&mut self, point: Point) {
        self.model.begin_text_move(point);
        unsafe {
            // SAFETY: The HWND is the visible overlay owned by this thread.

            SetCapture(self.hwnd);
        }
    }

    fn begin_text_size(&mut self, handle: Handle, point: Point) {
        self.model.begin_text_size(handle, point);
        unsafe {
            // SAFETY: The HWND is the visible overlay owned by this thread.

            SetCapture(self.hwnd);
        }
    }

    fn forward_edit_click(&mut self, point: Point, double: bool) {
        let x = point.x - self.model.edit_pos.x;
        let y = point.y - self.model.edit_pos.y;
        let packed = (x & 0xffff) as isize | (((y & 0xffff) as isize) << 16);
        let message = if double {
            WM_LBUTTONDBLCLK
        } else {
            WM_LBUTTONDOWN
        };
        unsafe {
            // SAFETY: The HWND is the editor or main window this program created.

            SetFocus(self.edit);
            SendMessageW(self.edit, message, WPARAM(1), LPARAM(packed));
        }
    }

    fn place_edit(&mut self) {
        self.model.clamp_edit();
        let origin = self.edit_screen_origin(self.model.edit_pos);
        unsafe {
            // SAFETY: The HWND is a window this program created. The position is plain integers.

            SetWindowPos(
                self.edit,
                HWND_TOPMOST,
                origin.x,
                origin.y,
                self.model.edit_w,
                self.model.edit_h,
                SWP_NOACTIVATE | SWP_NOCOPYBITS,
            )
            .ok();
            InvalidateRect(self.edit, None, true);
        }
        self.redraw();
    }

    fn edit_screen_origin(&self, at: Point) -> POINT {
        let mut origin = POINT { x: at.x, y: at.y };
        unsafe {
            // SAFETY: The POINT is in client coordinates of a window this program created.

            ClientToScreen(self.hwnd, &mut origin).ok();
        }
        origin
    }

    /// Glyph origin and wrap width inside the edit.
    fn edit_text_box(&self) -> (Point, i32) {
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: self.model.edit_w,
            bottom: self.model.edit_h,
        };
        let mut margins = 0i32;
        if !self.edit.is_invalid() {
            unsafe {
                // SAFETY: The target is a window this program created. Any pointer in the message addresses memory that outlives the call.

                let mut actual = RECT::default();
                SendMessageW(
                    self.edit,
                    0x00B2,
                    WPARAM(0),
                    LPARAM(&mut actual as *mut RECT as isize),
                );
                if actual.right > actual.left && actual.bottom > actual.top {
                    rect = actual;
                }
                margins = SendMessageW(self.edit, 0x00D4, WPARAM(0), LPARAM(0)).0 as i32;
            }
        }
        text_commit_box(
            self.model.edit_pos,
            self.model.edit_w,
            rect.left,
            rect.top,
            rect.right,
            margins,
        )
    }

    fn mouse_down(&mut self, point: Point, double: bool) {
        let was_editing = self.model.editing;
        if was_editing {
            if let Some(handle) = self.model.hit_text_handle(point) {
                self.begin_text_size(handle, point);
                return;
            }
            if self.model.in_text_border(point) {
                self.begin_text_move(point);
                return;
            }
            if self.model.inside_edit(point) {
                self.forward_edit_click(point, double);
                return;
            }
            self.commit_text();
        }
        let metrics = self.metrics();
        let action = self.model.mouse_down(point, double, was_editing, &metrics);
        self.perform(action);
        // A press keeps the pointer while a drag, a shape or the text box is in
        // progress. The editor took its own press above, and a click that only
        // ended a text session takes nothing.
        if self.model.drag.is_some()
            || self.model.current.is_some()
            || (!was_editing && self.model.editing)
        {
            unsafe {
                // SAFETY: The HWND is the visible overlay owned by this thread.

                SetCapture(self.hwnd);
            }
        }
    }

    fn mouse_move(&mut self, point: Point) {
        let action = self.model.mouse_move(point);
        if matches!(
            self.model.drag,
            Some(Drag::TextMove(_, _)) | Some(Drag::TextSize(_, _, _, _, _))
        ) {
            self.place_edit();
            return;
        }
        self.perform(action);
    }

    fn mouse_up(&mut self, point: Point) {
        let metrics = self.metrics();
        let action = self.model.mouse_up(point, &metrics);
        self.perform(action);
        unsafe {
            // SAFETY: This thread captured the mouse on the overlay window.

            ReleaseCapture().ok();
        }
    }

    fn open_text_editor(&mut self, at: Point, width: i32, height: i32, text: String) {
        if !self.edit_font.is_invalid() {
            unsafe {
                // SAFETY: The face name is NUL-terminated and outlives the call. The font is stored and later deleted.

                DeleteObject(self.edit_font);
            }
        }
        self.edit_font = unsafe {
            // SAFETY: The face name is NUL-terminated and outlives the call. The font is stored and later deleted.

            CreateFontW(
                -self.model.font_size.max(18),
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
                NONANTIALIASED_QUALITY.0 as u32,
                0,
                w!("Segoe UI"),
            )
        };
        let edit_alive = unsafe {
            // SAFETY: The handle is either null or the editor this overlay created. IsWindow only reads its state.
            IsWindow(self.edit).as_bool()
        };
        if !edit_alive {
            self.edit = HWND::default();
            let origin = self.edit_screen_origin(at);
            let style = WS_POPUP
                | WINDOW_STYLE(ES_MULTILINE as u32 | ES_AUTOVSCROLL as u32 | ES_NOHIDESEL as u32);
            let Ok(edit) = (unsafe {
                // SAFETY: The class name lives for the process. The new HWND is stored and destroyed with its owner.

                CreateWindowExW(
                    WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                    w!("EDIT"),
                    None,
                    style,
                    origin.x,
                    origin.y,
                    width,
                    height,
                    self.hwnd,
                    None,
                    GetModuleHandleW(None).unwrap(),
                    None,
                )
            }) else {
                self.model.editing = false;
                self.layout();
                self.redraw();
                return;
            };
            self.edit = edit;
            unsafe {
                // SAFETY: The HWND is a control this program created. The theme names are literals.

                windows::Win32::UI::Controls::SetWindowTheme(self.edit, w!(""), w!("")).ok();
                SetLayeredWindowAttributes(self.edit, palette().CHROMA, 0, LWA_COLORKEY).ok();
                let previous = SetWindowLongPtrW(
                    self.edit,
                    GWLP_WNDPROC,
                    edit_proc as *const () as usize as isize,
                );
                EDIT_PROC = previous;
            }
        }
        self.place_edit();
        unsafe {
            // SAFETY: The HWND is a window or child control this program created and still owns.

            SendMessageW(
                self.edit,
                WM_SETFONT,
                WPARAM(self.edit_font.0 as usize),
                LPARAM(1),
            );
            let hint = platform_core::i18n::wide(&tr("Type, Enter to finish"));
            SendMessageW(self.edit, 0x1501, WPARAM(1), LPARAM(hint.as_ptr() as isize));
            let wide = platform_core::i18n::wide(&text);
            SetWindowTextW(self.edit, PCWSTR(wide.as_ptr())).ok();
            ShowWindow(self.edit, SW_SHOW);
            SetFocus(self.edit);
        }
        self.layout();
        self.redraw();
    }

    fn close_text_editor(&mut self) {
        if !self.edit.is_invalid() {
            unsafe {
                // SAFETY: The HWND is a window or child control this program created and still owns.

                ShowWindow(self.edit, SW_HIDE);
            }
        }
        self.redraw();
    }

    fn commit_text(&mut self) {
        if !self.model.editing || self.edit.is_invalid() {
            return;
        }
        let len = unsafe {
            // SAFETY: The HWND is a window or child control this program created and still owns.
            GetWindowTextLengthW(self.edit)
        } as usize;
        let mut buffer = vec![0u16; len + 1];
        unsafe {
            // SAFETY: The HWND is a window or child control this program created and still owns.

            GetWindowTextW(self.edit, &mut buffer);
            ShowWindow(self.edit, SW_HIDE);
        }
        let text = String::from_utf16_lossy(&buffer[..len]);
        let (origin, wrap) = self.edit_text_box();
        let metrics = self.metrics();
        let action = self.model.commit_text(text, origin, wrap, &metrics);
        self.perform(action);
    }

    fn cancel_text(&mut self) {
        let metrics = self.metrics();
        let action = self.model.cancel_text(&metrics);
        self.perform(action);
    }

    fn key_down(&mut self, key: Key) {
        let control = unsafe {
            // SAFETY: The call only reads keyboard state and takes no pointer.
            GetKeyState(VK_CONTROL.0 as i32)
        } < 0;
        let shift = unsafe {
            // SAFETY: The call only reads keyboard state and takes no pointer.
            GetKeyState(VK_SHIFT.0 as i32)
        } < 0;
        let metrics = self.metrics();
        let action = self.model.key_down(key, control, shift, &metrics);
        self.perform(action);
    }

    fn next_monitor(&mut self) {
        let monitor = self.model.monitor;
        if let Ok(frame) = crate::screen::grab_still(monitor) {
            self.model.frame = frame;
            self.model.selection = None;
            self.model.shapes.clear();
            self.model.redo.clear();
            unsafe {
                // SAFETY: The HWND is a window this program created. The position is plain integers.

                SetWindowPos(
                    self.hwnd,
                    HWND_TOPMOST,
                    monitor.x,
                    monitor.y,
                    monitor.w as i32,
                    monitor.h as i32,
                    SWP_NOACTIVATE,
                )
                .ok();
            }
        }
        self.layout();
        self.redraw();
    }

    fn ensure_fonts(&mut self) {
        if self.font_scale == self.model.scale && !self.icon_font.is_invalid() {
            return;
        }
        unsafe {
            // SAFETY: The face name is NUL-terminated and outlives the call. The font is stored and later deleted.

            if !self.font.is_invalid() {
                DeleteObject(self.font);
            }
            if !self.icon_font.is_invalid() {
                DeleteObject(self.icon_font);
            }
            if self.font_resource.is_invalid() {
                static FONT: &[u8] = include_bytes!("../../../assets/Phosphor.ttf");
                let count = 0u32;
                self.font_resource =
                    AddFontMemResourceEx(FONT.as_ptr().cast(), FONT.len() as u32, None, &count);
            }
        }
        let px = |size: i32| -(size * self.model.scale as i32 / 100).max(13);
        unsafe {
            // SAFETY: The face name is NUL-terminated and outlives the call. The font is stored and later deleted.

            self.font = CreateFontW(
                px(15),
                0,
                0,
                0,
                FW_SEMIBOLD.0 as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET.0 as u32,
                OUT_DEFAULT_PRECIS.0 as u32,
                CLIP_DEFAULT_PRECIS.0 as u32,
                CLEARTYPE_QUALITY.0 as u32,
                0,
                w!("Segoe UI"),
            );
            self.icon_font = CreateFontW(
                px(17),
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
                CLEARTYPE_QUALITY.0 as u32,
                0,
                w!("Phosphor"),
            );
        }
        self.font_scale = self.model.scale;
    }

    fn redraw(&self) {
        unsafe {
            // SAFETY: The HWND is a window this program created.

            InvalidateRect(self.hwnd, None, false).ok();
        }
    }

    fn ensure_back(&mut self, dc: HDC) {
        if self.back_w == self.model.frame.width
            && self.back_h == self.model.frame.height
            && !self.back_dc.is_invalid()
        {
            return;
        }
        unsafe {
            // SAFETY: The GDI object was created by this window and is not selected into a DC.

            if !self.back_bmp.is_invalid() {
                DeleteObject(self.back_bmp);
            }
            if !self.back_dc.is_invalid() {
                DeleteDC(self.back_dc);
            }
            self.back_dc = CreateCompatibleDC(dc);
            self.back_bmp = CreateCompatibleBitmap(
                dc,
                self.model.frame.width as i32,
                self.model.frame.height as i32,
            );
            SelectObject(self.back_dc, self.back_bmp);
            self.back_w = self.model.frame.width;
            self.back_h = self.model.frame.height;
        }
    }
}

impl Drop for Region {
    fn drop(&mut self) {
        unsafe {
            // SAFETY: These GDI objects were created for this overlay and are no longer selected into a DC.
            if !self.font.is_invalid() {
                DeleteObject(self.font);
            }
            if !self.icon_font.is_invalid() {
                DeleteObject(self.icon_font);
            }
            if !self.edit_font.is_invalid() {
                DeleteObject(self.edit_font);
            }
            if !self.font_resource.is_invalid() {
                RemoveFontMemResourceEx(self.font_resource);
            }
            if !self.back_bmp.is_invalid() {
                DeleteObject(self.back_bmp);
            }
            if !self.back_dc.is_invalid() {
                DeleteDC(self.back_dc);
            }
        }
    }
}

fn region<'a>() -> &'a mut Region {
    unsafe {
        // SAFETY: The POINT is a writable local, and any cursor position is a valid monitor query.
        &mut *REGION
    }
}

fn same_rect(a: Rect, b: Rect) -> bool {
    a.x == b.x && a.y == b.y && a.w == b.w && a.h == b.h
}

fn point(lparam: LPARAM) -> Point {
    Point {
        x: (lparam.0 as i16) as i32,
        y: ((lparam.0 >> 16) as i16) as i32,
    }
}

fn cursor_screen() -> (i32, i32) {
    unsafe {
        // SAFETY: The POINT is a writable local, and any cursor position is a valid monitor query.

        let mut cursor = POINT::default();
        GetCursorPos(&mut cursor).ok();
        (cursor.x, cursor.y)
    }
}

/// The rectangle of every display, in the order Windows enumerates them.
pub fn monitor_rects() -> Vec<Rect> {
    // SAFETY: The enumerator calls this once per monitor. The Vec behind data stays alive until EnumDisplayMonitors returns.
    unsafe extern "system" fn collect(
        monitor: HMONITOR,
        _: HDC,
        _: *mut RECT,
        data: LPARAM,
    ) -> BOOL {
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if unsafe {
            // SAFETY: The callback's Vec stays alive until the enumeration returns.
            GetMonitorInfoW(monitor, &mut info)
        }
        .as_bool()
        {
            let rect = info.rcMonitor;
            unsafe {
                // SAFETY: The callback's Vec stays alive until the enumeration returns.
                &mut *(data.0 as *mut Vec<Rect>)
            }
            .push(Rect {
                x: rect.left,
                y: rect.top,
                w: (rect.right - rect.left) as u32,
                h: (rect.bottom - rect.top) as u32,
            });
        }
        true.into()
    }
    let mut result = Vec::new();
    unsafe {
        // SAFETY: The callback's Vec stays alive until the enumeration returns.

        EnumDisplayMonitors(
            None,
            None,
            Some(collect),
            LPARAM(&mut result as *mut _ as isize),
        )
    }
    .ok();
    result
}

pub fn suggested_image_path() -> String {
    crate::os::output_path(platform_core::types::OutputCategory::Screenshots, "Image", "png")
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "Image.png".into())
}

pub fn save_dialog() -> Option<PathBuf> {
    let mut path = vec![0u16; 32768];
    let suggested = platform_core::i18n::wide(&suggested_image_path());
    let count = suggested.len().min(path.len());
    path[..count].copy_from_slice(&suggested[..count]);
    let filter = "PNG image\0*.png\0WebP image\0*.webp\0\0"
        .encode_utf16()
        .collect::<Vec<_>>();
    // The dialog is also used by a renderer that has no native overlay window,
    // so the owner is optional instead of dereferencing a null `REGION`.
    let owner = if unsafe {
        // SAFETY: This only reads the pointer; it is dereferenced only when non-null.
        REGION.is_null()
    } {
        HWND::default()
    } else {
        // REGION was just checked, so this is the overlay window this module created.
        region().hwnd
    };
    let mut dialog = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: windows::core::PWSTR(path.as_mut_ptr()),
        nMaxFile: path.len() as u32,
        lpstrDefExt: w!("png"),
        Flags: OFN_OVERWRITEPROMPT | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    if unsafe {
        // SAFETY: The path buffer and filter outlive the call, and lStructSize matches the struct.
        GetSaveFileNameW(&mut dialog)
    }
    .as_bool()
    {
        let len = path
            .iter()
            .position(|value| *value == 0)
            .unwrap_or(path.len());
        Some(PathBuf::from(String::from_utf16_lossy(&path[..len])))
    } else {
        None
    }
}

fn chroma_brush() -> HBRUSH {
    static BRUSH: std::sync::OnceLock<isize> = std::sync::OnceLock::new();
    HBRUSH(*BRUSH.get_or_init(|| unsafe {
        // SAFETY: The caller passes the editor paint DC. The fill uses the process-owned chroma brush.
        CreateSolidBrush(palette().CHROMA).0 as isize
    }) as *mut std::ffi::c_void)
}

// SAFETY: The caller passes the editor paint DC. The fill uses the process-owned chroma brush.
unsafe fn erase_edit_background(dc: HDC) -> LRESULT {
    let region = region();
    let rect = RECT {
        left: 0,
        top: 0,
        right: region.model.edit_w.max(1),
        bottom: region.model.edit_h.max(1),
    };
    FillRect(dc, &rect, chroma_brush());
    LRESULT(1)
}

fn edit_edge_handle(point: Point, width: i32, height: i32) -> Option<Handle> {
    let near = |value: i32, edge: i32| (value - edge).abs() <= 8;
    let left = near(point.x, 0);
    let right = near(point.x, width);
    let top = near(point.y, 0);
    let bottom = near(point.y, height);
    Some(match (left, right, top, bottom) {
        (true, _, true, _) => Handle::NW,
        (_, true, true, _) => Handle::NE,
        (true, _, _, true) => Handle::SW,
        (_, true, _, true) => Handle::SE,
        (true, _, _, _) => Handle::W,
        (_, true, _, _) => Handle::E,
        (_, _, true, _) => Handle::N,
        (_, _, _, true) => Handle::S,
        _ => return None,
    })
}

fn move_edit(current: Point) {
    let Some(drag) = region().model.edit_drag else {
        return;
    };
    let delta = geom::drag_delta(
        drag.start_screen,
        cursor_screen(),
        (drag.start_client.x, drag.start_client.y),
        (current.x, current.y),
    );
    let region = region();
    region.model.edit_pos = Point {
        x: drag.origin.x + delta.0,
        y: drag.origin.y + delta.1,
    };
    region.place_edit();
}

fn draw_edit_chrome(dc: HDC, width: i32, height: i32) {
    unsafe {
        // SAFETY: The color is a plain COLORREF. The brush is stored for the process or deleted in this block.

        let pen = CreatePen(PS_SOLID, 3, palette().ACCENT);
        let old_pen = SelectObject(dc, pen);
        let old_brush = SelectObject(dc, GetStockObject(NULL_BRUSH));
        RoundRect(dc, 1, 1, width - 1, height - 1, 10, 10);
        let fill = CreateSolidBrush(palette().WHITE);
        let edge = CreatePen(PS_SOLID, 1, palette().ACCENT);
        SelectObject(dc, fill);
        SelectObject(dc, edge);
        for (x, y) in [
            (0, 0),
            (width, 0),
            (0, height),
            (width, height),
            (width / 2, 0),
            (width / 2, height),
            (0, height / 2),
            (width, height / 2),
        ] {
            Ellipse(dc, x - 5, y - 5, x + 5, y + 5);
        }
        SelectObject(dc, old_pen);
        SelectObject(dc, old_brush);
        DeleteObject(pen);
        DeleteObject(fill);
        DeleteObject(edge);
    }
}

// SAFETY: Windows calls this for the text editor this program created. The saved procedure is called only while it is still installed.
unsafe extern "system" fn edit_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if REGION.is_null() || EDIT_PROC == 0 {
        return DefWindowProcW(hwnd, message, wparam, lparam);
    }
    match message {
        WM_ERASEBKGND => erase_edit_background(HDC(wparam.0 as *mut std::ffi::c_void)),
        WM_PAINT | 0x0317 | 0x0318 => {
            let result = CallWindowProcW(
                std::mem::transmute::<isize, WNDPROC>(EDIT_PROC),
                hwnd,
                message,
                wparam,
                lparam,
            );
            let dc = if message == WM_PAINT {
                GetDC(hwnd)
            } else {
                HDC(wparam.0 as *mut std::ffi::c_void)
            };
            let mut rect = RECT::default();
            GetClientRect(hwnd, &mut rect).ok();
            draw_edit_chrome(dc, rect.right, rect.bottom);
            if message == WM_PAINT {
                ReleaseDC(hwnd, dc);
            }
            result
        }
        WM_MBUTTONDOWN => {
            let local = point(lparam);
            let mut parent_point = POINT {
                x: local.x,
                y: local.y,
            };
            ClientToScreen(hwnd, &mut parent_point).ok();
            ScreenToClient(region().hwnd, &mut parent_point).ok();
            region().begin_text_move(Point {
                x: parent_point.x,
                y: parent_point.y,
            });
            LRESULT(0)
        }
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            let local = point(lparam);
            let mut rect = RECT::default();
            GetClientRect(hwnd, &mut rect).ok();
            if message == WM_LBUTTONDOWN {
                if let Some(handle) = edit_edge_handle(local, rect.right, rect.bottom) {
                    let mut parent_point = POINT {
                        x: local.x,
                        y: local.y,
                    };
                    ClientToScreen(hwnd, &mut parent_point).ok();
                    ScreenToClient(region().hwnd, &mut parent_point).ok();
                    region().begin_text_size(
                        handle,
                        Point {
                            x: parent_point.x,
                            y: parent_point.y,
                        },
                    );
                    return LRESULT(0);
                }
            }
            CallWindowProcW(
                std::mem::transmute::<isize, WNDPROC>(EDIT_PROC),
                hwnd,
                message,
                wparam,
                lparam,
            )
        }
        WM_MOUSEMOVE => {
            if region().model.edit_drag.is_some() {
                move_edit(point(lparam));
                LRESULT(0)
            } else {
                CallWindowProcW(
                    std::mem::transmute::<isize, WNDPROC>(EDIT_PROC),
                    hwnd,
                    message,
                    wparam,
                    lparam,
                )
            }
        }
        WM_LBUTTONUP => {
            if region().model.edit_drag.is_some() {
                move_edit(point(lparam));
                region().model.edit_drag = None;
                ReleaseCapture().ok();
                LRESULT(0)
            } else {
                CallWindowProcW(
                    std::mem::transmute::<isize, WNDPROC>(EDIT_PROC),
                    hwnd,
                    message,
                    wparam,
                    lparam,
                )
            }
        }
        WM_KEYDOWN if wparam.0 == VK_RETURN.0 as usize && GetKeyState(VK_SHIFT.0 as i32) >= 0 => {
            region().commit_text();
            SetFocus(region().hwnd);
            LRESULT(0)
        }
        WM_KEYDOWN if wparam.0 == VK_ESCAPE.0 as usize => {
            region().cancel_text();
            SetFocus(region().hwnd);
            LRESULT(0)
        }
        // Alt+F4 (or any other close request) belongs to the text box, not to the whole
        // overlay. Destroying only the child editor left the Text tool holding a dead
        // window handle, so it could never be opened again in this selection.
        WM_CLOSE => {
            region().cancel_text();
            SetFocus(region().hwnd);
            LRESULT(0)
        }
        _ => CallWindowProcW(
            std::mem::transmute::<isize, WNDPROC>(EDIT_PROC),
            hwnd,
            message,
            wparam,
            lparam,
        ),
    }
}

/// The neutral key for a virtual-key code.
fn key_from_vk(key: u32) -> Option<Key> {
    Some(match key {
        value if value == VK_ESCAPE.0 as u32 => Key::Escape,
        value if value == VK_RETURN.0 as u32 => Key::Enter,
        value if value == VK_TAB.0 as u32 => Key::Tab,
        value if value == VK_LEFT.0 as u32 => Key::ArrowLeft,
        value if value == VK_RIGHT.0 as u32 => Key::ArrowRight,
        value if value == VK_UP.0 as u32 => Key::ArrowUp,
        value if value == VK_DOWN.0 as u32 => Key::ArrowDown,
        value if value == VK_BACK.0 as u32 => Key::Backspace,
        value if value == VK_DELETE.0 as u32 => Key::Delete,
        value if (VK_A.0 as u32..=VK_Z.0 as u32).contains(&value) => {
            Key::Character(char::from(b'A' + (value - VK_A.0 as u32) as u8))
        }
        _ => return None,
    })
}

// SAFETY: Windows calls this with a window of this class. Paint uses the DC from BeginPaint, and every other message is forwarded to DefWindowProc.
unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if REGION.is_null() {
        return DefWindowProcW(hwnd, message, wparam, lparam);
    }
    match message {
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_ERASEBKGND => {
            let region = region();
            if !region.back_dc.is_invalid() {
                let dc = HDC(wparam.0 as *mut std::ffi::c_void);
                unsafe {
                    // SAFETY: The DC belongs to this window and the back buffer to this module.
                    BitBlt(
                        dc,
                        0,
                        0,
                        region.back_w as i32,
                        region.back_h as i32,
                        region.back_dc,
                        0,
                        0,
                        SRCCOPY,
                    )
                    .ok();
                }
            }
            LRESULT(1)
        }
        WM_CTLCOLOREDIT => {
            let dc = HDC(wparam.0 as *mut std::ffi::c_void);
            SetBkColor(dc, palette().CHROMA);
            SetTextColor(dc, COLORREF(region().model.color));
            LRESULT(chroma_brush().0 as isize)
        }
        WM_KEYDOWN => {
            if let Some(key) = key_from_vk(wparam.0 as u32) {
                region().key_down(key);
            }
            LRESULT(0)
        }
        WM_CHAR => {
            let key = Key::Character(char::from_u32(wparam.0 as u32).unwrap_or('\0'));
            region().key_down(key);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            region().mouse_down(point(lparam), false);
            LRESULT(0)
        }
        WM_LBUTTONDBLCLK => {
            region().mouse_down(point(lparam), true);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            region().mouse_move(point(lparam));
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            region().mouse_up(point(lparam));
            LRESULT(0)
        }
        WM_MBUTTONDOWN => {
            let cursor = point(lparam);
            if region().model.over_text_box(cursor) {
                region().begin_text_move(cursor);
            }
            LRESULT(0)
        }
        WM_MBUTTONUP => {
            if matches!(region().model.drag, Some(Drag::TextMove(_, _))) {
                region().mouse_up(point(lparam));
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            region().finish(Outcome::Cancel);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

/// Draws the whole overlay into the back buffer.
///
/// It runs before the window is shown, so the first composition shows the
/// finished scene instead of a black window while the first paint runs (GDI+
/// starts lazily inside that paint and can take a while).
fn render(region: &mut Region) {
    let dc = unsafe {
        // SAFETY: The window belongs to this module and the DC is released below.
        GetDC(region.hwnd)
    };
    region.ensure_back(dc);
    unsafe {
        // SAFETY: The DC was obtained from this window in the same function.
        ReleaseDC(region.hwnd, dc);
    }
    let memory = region.back_dc;
    blit_frame(memory, &region.model.frame);
    draw_dim(memory, region);
    for shape in region.model.shapes.iter().chain(region.model.current.iter()) {
        draw_shape(memory, shape, Point { x: 0, y: 0 });
    }
    if let Some(selection) = region.model.selected() {
        unsafe {
            // SAFETY: The back buffer belongs to this module and the pen and brush are deleted below.
            let pen = CreatePen(PS_SOLID, 2, palette().ACCENT);
            let old_pen = SelectObject(memory, pen);
            let old_brush = SelectObject(memory, GetStockObject(NULL_BRUSH));
            Rectangle(
                memory,
                selection.x,
                selection.y,
                selection.right(),
                selection.bottom(),
            );
            SelectObject(memory, old_pen);
            SelectObject(memory, old_brush);
            DeleteObject(pen);
            if region.model.tool == Tool::Select {
                let brush = CreateSolidBrush(palette().ACCENT);
                for (x, y) in handles(selection) {
                    let rect = RECT {
                        left: x - 5,
                        top: y - 5,
                        right: x + 5,
                        bottom: y + 5,
                    };
                    FillRect(memory, &rect, brush);
                }
                DeleteObject(brush);
            }
        }
        draw_badge(memory, region, selection);
    } else if region.model.buttons.iter().all(|button| !button.shown)
        || region.model.drag.is_none()
    {
        draw_hint(memory, region);
    }
    for button in &region.model.buttons {
        if button.shown {
            draw_button(memory, region, button);
        }
    }
    if region.model.editing {
        draw_text_frame(memory, region);
    }
}

fn paint(hwnd: HWND) {
    let region = region();
    let mut ps = PAINTSTRUCT::default();
    unsafe {
        // SAFETY: Windows is painting this window; EndPaint receives the same struct.
        let dc = BeginPaint(hwnd, &mut ps);
        render(region);
        BitBlt(
            dc,
            0,
            0,
            region.model.frame.width as i32,
            region.model.frame.height as i32,
            region.back_dc,
            0,
            0,
            SRCCOPY,
        )
        .ok();
        EndPaint(hwnd, &ps);
    }
}

fn draw_text_frame(dc: HDC, region: &Region) {
    let frame = region.model.text_outer();
    let gp = crate::gdiplus::Graphics::new(dc);
    if let Some(gp) = &gp {
        gp.stroke_round_rect(
            palette().ACCENT,
            2.0,
            frame.x as f32,
            frame.y as f32,
            frame.w as f32,
            frame.h as f32,
            6.0,
        );
    } else {
        unsafe {
            // SAFETY: The pen and brush are plain GDI objects deleted before return.
            let pen = CreatePen(PS_SOLID, 2, palette().ACCENT);
            let old_pen = SelectObject(dc, pen);
            let old_brush = SelectObject(dc, GetStockObject(NULL_BRUSH));
            RoundRect(dc, frame.x, frame.y, frame.right(), frame.bottom(), 12, 12);
            SelectObject(dc, old_pen);
            SelectObject(dc, old_brush);
            DeleteObject(pen);
        }
    }
    drop(gp);
    unsafe {
        // SAFETY: The fill brush and edge pen are deleted before return.
        let fill = CreateSolidBrush(palette().WHITE);
        let edge = CreatePen(PS_SOLID, 1, palette().ACCENT);
        let old_brush = SelectObject(dc, fill);
        let old_pen = SelectObject(dc, edge);
        for (x, y) in [
            (frame.x, frame.y),
            (frame.right(), frame.y),
            (frame.x, frame.bottom()),
            (frame.right(), frame.bottom()),
            (frame.x + frame.w / 2, frame.y),
            (frame.x + frame.w / 2, frame.bottom()),
            (frame.x, frame.y + frame.h / 2),
            (frame.right(), frame.y + frame.h / 2),
        ] {
            Rectangle(dc, x - 4, y - 4, x + 4, y + 4);
        }
        SelectObject(dc, old_brush);
        SelectObject(dc, old_pen);
        DeleteObject(fill);
        DeleteObject(edge);
    }
}

fn blit_frame(dc: HDC, frame: &Frame) {
    unsafe {
        // SAFETY: Every field the API reads, including the size, is written before the struct is used.

        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = frame.width as i32;
        info.bmiHeader.biHeight = -(frame.height as i32);
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB.0;
        SetDIBitsToDevice(
            dc,
            0,
            0,
            frame.width,
            frame.height,
            0,
            0,
            0,
            frame.height,
            frame.data.as_ptr().cast(),
            &info,
            DIB_RGB_COLORS,
        );
    }
}

fn draw_dim(dc: HDC, region: &Region) {
    let width = region.model.frame.width as i32;
    let height = region.model.frame.height as i32;
    let selection = region.model.selection.unwrap_or(Selection {
        x: 0,
        y: 0,
        w: 0,
        h: 0,
    });
    let rects = [
        (0, 0, width, selection.y),
        (0, selection.bottom(), width, height - selection.bottom()),
        (0, selection.y, selection.x, selection.h),
        (
            selection.right(),
            selection.y,
            width - selection.right(),
            selection.h,
        ),
    ];
    unsafe {
        // SAFETY: dc is the overlay paint DC. The temporary DC and bitmap are deleted before return.
        let dim = CreateCompatibleDC(dc);
        let bitmap = CreateCompatibleBitmap(dc, 1, 1);
        let old = SelectObject(dim, bitmap);
        SetPixelV(dim, 0, 0, palette().BLACK);
        for (x, y, w, h) in rects {
            if w > 0 && h > 0 {
                GdiAlphaBlend(
                    dc,
                    x,
                    y,
                    w,
                    h,
                    dim,
                    0,
                    0,
                    1,
                    1,
                    BLENDFUNCTION {
                        BlendOp: AC_SRC_OVER as u8,
                        BlendFlags: 0,
                        SourceConstantAlpha: 120,
                        AlphaFormat: 0,
                    },
                );
            }
        }
        SelectObject(dim, old);
        DeleteObject(bitmap);
        DeleteDC(dim);
    }
}

fn draw_badge(dc: HDC, region: &Region, selection: Selection) {
    let text = format!("{} × {}", selection.w, selection.h);
    let wide = platform_core::i18n::wide(&text);
    unsafe {
        // SAFETY: The DC belongs to a window this program is painting, and created GDI objects are deleted before return.

        let old = SelectObject(dc, region.font);
        let mut size = SIZE::default();
        GetTextExtentPoint32W(dc, &wide, &mut size).ok();
        let mut blocked: Vec<Panel> = region
            .model
            .buttons
            .iter()
            .filter(|button| button.shown)
            .map(|button| Panel {
                x: button.x,
                y: button.y,
                w: button.w,
                h: button.h,
            })
            .collect();
        if region.model.editing {
            blocked.push(region.model.text_outer().panel());
        }
        let screen = Panel {
            x: 0,
            y: 0,
            w: region.model.frame.width as i32,
            h: region.model.frame.height as i32,
        };
        let badge = geom::place_badge(
            screen,
            selection.panel(),
            (size.cx + 16, size.cy + 8),
            &blocked,
        );
        let x = badge.x;
        let y = badge.y;
        let rect = RECT {
            left: x,
            top: y,
            right: x + size.cx + 16,
            bottom: y + size.cy + 8,
        };
        let gp = crate::gdiplus::Graphics::new(dc);
        if let Some(gp) = &gp {
            gp.fill_round_rect(
                palette().BADGE,
                x as f32,
                y as f32,
                (size.cx + 16) as f32,
                (size.cy + 8) as f32,
                6.0,
            );
        } else {
            let brush = CreateSolidBrush(palette().BADGE);
            let pen = CreatePen(PS_SOLID, 1, palette().BADGE);
            SelectObject(dc, brush);
            SelectObject(dc, pen);
            RoundRect(dc, rect.left, rect.top, rect.right, rect.bottom, 12, 12);
            DeleteObject(brush);
            DeleteObject(pen);
        }
        drop(gp);
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, palette().BADGE_TEXT);
        TextOutW(dc, x + 8, y + 4, &wide);
        SelectObject(dc, old);
    }
}

fn draw_hint(dc: HDC, region: &Region) {
    let mut text = platform_core::i18n::wide(&tr(
        "Drag to select an area    ·    Double click: full screen    ·    Tab: next display    ·    Esc: cancel",
    ));
    unsafe {
        // SAFETY: The DC belongs to a window this program is painting, and created GDI objects are deleted before return.

        let old = SelectObject(dc, region.font);
        SetBkMode(dc, TRANSPARENT);
        let mut rect = RECT {
            left: 40,
            top: 0,
            right: region.model.frame.width as i32 - 40,
            bottom: region.model.frame.height as i32,
        };
        SetTextColor(dc, palette().HINT_SHADOW);
        let mut shadow = rect;
        shadow.left += 1;
        shadow.top += 1;
        DrawTextW(
            dc,
            &mut text,
            &mut shadow,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
        SetTextColor(dc, palette().WHITE);
        DrawTextW(
            dc,
            &mut text,
            &mut rect,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
        SelectObject(dc, old);
    }
}

/// The ink box of a single glyph: its offset and size inside the text cell,
/// with the cell's top-left as the origin.
///
/// GDI centres the text *cell* on the button, but the visible ink of a symbol
/// font sits off the cell centre, so the icon looks shifted. Measuring the ink
/// lets the renderer centre what the eye sees.
fn glyph_ink(dc: HDC, font: HFONT, glyph: &str) -> Option<(i32, i32, i32, i32)> {
    if glyph.chars().count() != 1 {
        return None;
    }
    let character = glyph.chars().next()? as u32;
    unsafe {
        // SAFETY: The DC is the overlay DC, the font is owned by the overlay and
        // restored before return, and the metrics land in locals.
        let old = SelectObject(dc, font);
        let mut metrics = GLYPHMETRICS::default();
        let matrix = MAT2 {
            eM11: FIXED { fract: 0, value: 1 },
            eM12: FIXED::default(),
            eM21: FIXED::default(),
            eM22: FIXED { fract: 0, value: 1 },
        };
        GetGlyphOutlineW(dc, character, GGO_METRICS, &mut metrics, 0, None, &matrix);
        let mut text = TEXTMETRICW::default();
        let measured = GetTextMetricsW(dc, &mut text).as_bool();
        SelectObject(dc, old);
        if !measured || metrics.gmBlackBoxX == 0 || metrics.gmBlackBoxY == 0 {
            return None;
        }
        Some((
            metrics.gmptGlyphOrigin.x,
            text.tmAscent - metrics.gmptGlyphOrigin.y,
            metrics.gmBlackBoxX as i32,
            metrics.gmBlackBoxY as i32,
        ))
    }
}

/// Draws one glyph with its ink box centred on `(cx, cy)`.
///
/// Returns false when the font has no measurable ink, so the caller can fall
/// back to centring the text cell instead.
fn draw_glyph_centered(dc: HDC, font: HFONT, glyph: &str, cx: i32, cy: i32) -> bool {
    let Some((left, top, width, height)) = glyph_ink(dc, font, glyph) else {
        return false;
    };
    unsafe {
        // SAFETY: The DC is the overlay DC and the font is owned by the overlay.
        let old = SelectObject(dc, font);
        let wide = platform_core::i18n::wide(glyph);
        TextOutW(dc, cx - left - width / 2, cy - top - height / 2, &wide).ok();
        SelectObject(dc, old);
    }
    true
}

fn measure(dc: HDC, font: HFONT, text: &str) -> i32 {
    unsafe {
        // SAFETY: dc is the overlay paint DC passed in, and the previous font is restored before return.
        let old = SelectObject(dc, font);
        let wide = platform_core::i18n::wide(text);
        let mut size = SIZE::default();
        GetTextExtentPoint32W(dc, &wide, &mut size).ok();
        SelectObject(dc, old);
        size.cx
    }
}

fn button_fill(role: Role, enabled: bool, primary: bool, swatch: Option<usize>) -> COLORREF {
    if let Some(swatch) = swatch {
        palette().SWATCHES[swatch]
    } else if !enabled {
        palette().OVERLAY_DISABLED
    } else if role == Role::Copy {
        palette().COPY
    } else if role == Role::Cancel {
        palette().CANCEL
    } else if primary {
        palette().ACCENT
    } else {
        palette().OVERLAY
    }
}

fn button_ink(role: Role, enabled: bool, primary: bool) -> COLORREF {
    if !enabled {
        palette().OVERLAY_DISABLED_INK
    } else if primary || matches!(role, Role::Copy | Role::Cancel) {
        palette().ON_ACCENT
    } else {
        palette().OVERLAY_INK
    }
}

fn draw_button(dc: HDC, region: &Region, button: &Button) {
    let selected = matches!(
        (button.role, region.model.tool),
        (Role::Select, Tool::Select)
            | (Role::Pen, Tool::Pen)
            | (Role::Line, Tool::Line)
            | (Role::Arrow, Tool::Arrow)
            | (Role::Rectangle, Tool::Rectangle)
            | (Role::Marker, Tool::Marker)
            | (Role::Text, Tool::Text)
    );
    let enabled = role_enabled(
        &region.model.shapes,
        &region.model.redo,
        region.model.allow_record,
        button.role,
    );
    let primary = selected
        || matches!(
            button.role,
            Role::Record | Role::RecordMkv | Role::RecordGif | Role::RecordWebp
        );
    let fill = button_fill(button.role, enabled, primary, button.swatch);
    let ink = button_ink(button.role, enabled, primary);
    let edge = if selected {
        palette().ACCENT_DEEP
    } else {
        palette().OVERLAY_EDGE
    };
    let gp = crate::gdiplus::Graphics::new(dc);
    let radius = button.h as f32 / 2.0;
    if let Some(gp) = &gp {
        gp.fill_round_rect(
            fill,
            button.x as f32,
            button.y as f32,
            button.w as f32,
            button.h as f32,
            radius,
        );
        gp.stroke_round_rect(
            edge,
            1.0,
            button.x as f32,
            button.y as f32,
            button.w as f32,
            button.h as f32,
            radius,
        );
    } else {
        unsafe {
            // SAFETY: The colors are plain COLORREFs. The brush and pen are deleted before return.
            let brush = CreateSolidBrush(fill);
            let pen = CreatePen(PS_SOLID, 1, edge);
            let old_brush = SelectObject(dc, brush);
            let old_pen = SelectObject(dc, pen);
            let radius = button.h;
            RoundRect(
                dc,
                button.x,
                button.y,
                button.x + button.w,
                button.y + button.h,
                radius,
                radius,
            );
            SelectObject(dc, old_brush);
            SelectObject(dc, old_pen);
            DeleteObject(brush);
            DeleteObject(pen);
        }
    }
    if button.role == Role::Color {
        let cx = button.x + button.w / 2;
        let cy = button.y + button.h / 2;
        if let Some(gp) = &gp {
            gp.fill_ellipse(
                COLORREF(region.model.color),
                (cx - 8) as f32,
                (cy - 8) as f32,
                16.0,
                16.0,
            );
            gp.stroke_ellipse(palette().WHITE, 2.0, (cx - 8) as f32, (cy - 8) as f32, 16.0, 16.0);
        } else {
            unsafe {
                // SAFETY: The dot brush and ring pen are deleted before return.
                let dot = CreateSolidBrush(COLORREF(region.model.color));
                let ring = CreatePen(PS_SOLID, 2, palette().WHITE);
                let old_dot = SelectObject(dc, dot);
                let old_ring = SelectObject(dc, ring);
                Ellipse(dc, cx - 8, cy - 8, cx + 8, cy + 8);
                SelectObject(dc, old_dot);
                SelectObject(dc, old_ring);
                DeleteObject(dot);
                DeleteObject(ring);
            }
        }
    } else if button.role == Role::Width {
        let cy = button.y + button.h / 2;
        if let Some(gp) = &gp {
            gp.line(
                ink,
                region.model.width.max(1) as f32,
                &[
                    ((button.x + 8) as f32, cy as f32),
                    ((button.x + button.w - 8) as f32, cy as f32),
                ],
            );
        } else {
            unsafe {
                // SAFETY: The stroke pen is deleted before return.
                let stroke = CreatePen(PS_SOLID, region.model.width.max(1), ink);
                let old_stroke = SelectObject(dc, stroke);
                MoveToEx(dc, button.x + 8, cy, None).ok();
                LineTo(dc, button.x + button.w - 8, cy).ok();
                SelectObject(dc, old_stroke);
                DeleteObject(stroke);
            }
        }
    } else if let Some(swatch) = button.swatch {
        if palette().SWATCHES[swatch].0 == region.model.color {
            if let Some(gp) = &gp {
                gp.stroke_round_rect(
                    palette().WHITE,
                    2.0,
                    (button.x + 3) as f32,
                    (button.y + 2) as f32,
                    (button.w - 6) as f32,
                    (button.h - 4) as f32,
                    4.0,
                );
            } else {
                unsafe {
                    // SAFETY: The ring pen is deleted before return.
                    let ring = CreatePen(PS_SOLID, 2, palette().WHITE);
                    let old_ring = SelectObject(dc, ring);
                    let old_brush = SelectObject(dc, GetStockObject(NULL_BRUSH));
                    RoundRect(
                        dc,
                        button.x + 3,
                        button.y + 2,
                        button.x + button.w - 3,
                        button.y + button.h - 2,
                        8,
                        8,
                    );
                    SelectObject(dc, old_brush);
                    SelectObject(dc, old_ring);
                    DeleteObject(ring);
                }
            }
        }
    } else if let Some(glyph) = icon(button.role) {
        drop(gp);
        let center_y = button.y + button.h / 2;
        match caption(button.role) {
            None => {
                unsafe {
                    // SAFETY: The DC is the overlay DC; the font is owned by the overlay.
                    SetBkMode(dc, TRANSPARENT);
                    SetTextColor(dc, ink);
                }
                if !draw_glyph_centered(dc, region.icon_font, glyph, button.x + button.w / 2, center_y)
                {
                    unsafe {
                        // SAFETY: The DC is the overlay DC and the font is restored before return.
                        let old = SelectObject(dc, region.icon_font);
                        let mut wide = platform_core::i18n::wide(glyph);
                        let mut rect = RECT {
                            left: button.x,
                            top: button.y,
                            right: button.x + button.w,
                            bottom: button.y + button.h,
                        };
                        DrawTextW(dc, &mut wide, &mut rect, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
                        SelectObject(dc, old);
                    }
                }
            }
            Some(caption) => {
                // Centre the glyph and its caption as one group, measured with the
                // fonts that actually draw them, the way the Slint overlay does.
                let icon_w = measure(dc, region.icon_font, glyph);
                let text_w = measure(dc, region.font, &caption);
                let gap = (8 * region.model.scale as i32 / 100).max(6);
                let content = icon_w + gap + text_w;
                let left = button.x + (button.w - content).max(0) / 2;
                unsafe {
                    // SAFETY: The DC is the overlay DC and the font is restored before return.
                    SetBkMode(dc, TRANSPARENT);
                    SetTextColor(dc, ink);
                    let old = SelectObject(dc, region.font);
                    let mut text = platform_core::i18n::wide(&caption);
                    let mut rect = RECT {
                        left: left + icon_w + gap,
                        top: button.y,
                        right: button.x + button.w,
                        bottom: button.y + button.h,
                    };
                    DrawTextW(dc, &mut text, &mut rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
                    SelectObject(dc, old);
                }
                if !draw_glyph_centered(
                    dc,
                    region.icon_font,
                    glyph,
                    left + icon_w / 2,
                    center_y,
                ) {
                    unsafe {
                        // SAFETY: The DC is the overlay DC and the font is restored before return.
                        let old = SelectObject(dc, region.icon_font);
                        let mut wide = platform_core::i18n::wide(glyph);
                        let mut rect = RECT {
                            left,
                            top: button.y,
                            right: left + icon_w,
                            bottom: button.y + button.h,
                        };
                        DrawTextW(dc, &mut wide, &mut rect, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
                        SelectObject(dc, old);
                    }
                }
            }
        }
    }
}

pub fn draw_annotations(frame: &mut Frame, shapes: &[Shape], offset: Point) -> bool {
    unsafe {
        // SAFETY: Every field the API reads, including the size, is written before the struct is used.
        if frame.width == 0 || frame.height == 0 {
            return false;
        }
        let bytes = frame.width as usize * frame.height as usize * 4;
        if frame.data.len() < bytes {
            return false;
        }
        let screen = GetDC(None);
        let dc = CreateCompatibleDC(screen);
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = frame.width as i32;
        info.bmiHeader.biHeight = -(frame.height as i32);
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB.0;
        let mut bits = std::ptr::null_mut();
        let bitmap = match CreateDIBSection(screen, &info, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(bitmap) if !bits.is_null() => bitmap,
            _ => {
                // A very large selection or a GDI crunch must not abort the app.
                DeleteDC(dc);
                ReleaseDC(None, screen);
                return false;
            }
        };
        let old = SelectObject(dc, bitmap);
        std::ptr::copy_nonoverlapping(frame.data.as_ptr(), bits.cast::<u8>(), bytes);
        for shape in shapes {
            draw_shape(dc, shape, offset);
        }
        GdiFlush();
        std::ptr::copy_nonoverlapping(bits.cast::<u8>(), frame.data.as_mut_ptr(), bytes);
        // GDI writes the stroke color and clears the alpha byte, so a red line becomes a transparent hole in PNG.
        for pixel in frame.data.as_chunks_mut::<4>().0 {
            pixel[3] = 255;
        }
        SelectObject(dc, old);
        DeleteObject(bitmap);
        DeleteDC(dc);
        ReleaseDC(None, screen);
        true
    }
}

/// Paints one annotation: GDI+ when it started, plain GDI as the fallback.
fn draw_shape(dc: HDC, shape: &Shape, offset: Point) {
    if matches!(shape.kind, ShapeKind::Text(..)) {
        draw_shape_gdi(dc, shape, offset);
        return;
    }
    match crate::gdiplus::Graphics::new(dc) {
        Some(gp) => draw_shape_aa(&gp, shape, offset),
        None => draw_shape_gdi(dc, shape, offset),
    }
}

/// The anti-aliased geometry of one annotation. Text is drawn by GDI, which
/// already anti-aliases it with ClearType.
fn draw_shape_aa(gp: &crate::gdiplus::Graphics, shape: &Shape, offset: Point) {
    let color = if matches!(shape.kind, ShapeKind::Marker(_)) {
        palette().MARKER
    } else {
        COLORREF(shape.color)
    };
    let width = if matches!(shape.kind, ShapeKind::Marker(_)) {
        shape.width * 4
    } else {
        shape.width
    };
    let p = |point: Point| {
        (
            (point.x + offset.x) as f32,
            (point.y + offset.y) as f32,
        )
    };
    match &shape.kind {
        ShapeKind::Pen(points) | ShapeKind::Marker(points) => {
            let points: Vec<(f32, f32)> = points.iter().copied().map(p).collect();
            if points.len() == 1 {
                let (x, y) = points[0];
                let d = width as f32;
                gp.fill_ellipse(color, x - d / 2.0, y - d / 2.0, d, d);
            }
            gp.line(color, width as f32, &points);
        }
        ShapeKind::Line(from, to) => gp.line(color, width as f32, &[p(*from), p(*to)]),
        ShapeKind::Arrow(from, to) => {
            let (ax, ay) = p(*from);
            let (bx, by) = p(*to);
            gp.line(color, width as f32, &[(ax, ay), (bx, by)]);
            let dx = bx - ax;
            let dy = by - ay;
            let length = dx.hypot(dy).max(1.0);
            let (ux, uy) = (dx / length, dy / length);
            let size = (shape.width * 4).max(12) as f32;
            let left = (
                bx - (ux * size - uy * size * 0.55),
                by - (uy * size + ux * size * 0.55),
            );
            let right = (
                bx - (ux * size + uy * size * 0.55),
                by - (uy * size - ux * size * 0.55),
            );
            gp.fill_polygon(color, &[left, (bx, by), right]);
        }
        ShapeKind::Rectangle(from, to) => {
            let (ax, ay) = p(*from);
            let (bx, by) = p(*to);
            gp.stroke_polygon(
                color,
                width as f32,
                &[(ax, ay), (bx, ay), (bx, by), (ax, by)],
            );
        }
        ShapeKind::Text(..) => {}
    }
}

fn draw_shape_gdi(dc: HDC, shape: &Shape, offset: Point) {
    unsafe {
        // SAFETY: The color is a plain COLORREF. The brush is stored for the process or deleted in this block.

        let color = if matches!(shape.kind, ShapeKind::Marker(_)) {
            palette().MARKER
        } else {
            COLORREF(shape.color)
        };
        let width = if matches!(shape.kind, ShapeKind::Marker(_)) {
            shape.width * 4
        } else {
            shape.width
        };
        SetBkMode(dc, TRANSPARENT);
        let pen = CreatePen(PS_SOLID, width, color);
        let old_pen = SelectObject(dc, pen);
        let brush = CreateSolidBrush(color);
        let old_brush = SelectObject(dc, brush);
        let p = |point: Point| POINT {
            x: point.x + offset.x,
            y: point.y + offset.y,
        };
        match &shape.kind {
            ShapeKind::Pen(points) | ShapeKind::Marker(points) => {
                if points.len() == 1 {
                    let point = p(points[0]);
                    Ellipse(
                        dc,
                        point.x - width / 2,
                        point.y - width / 2,
                        point.x + width / 2,
                        point.y + width / 2,
                    );
                }
                let points: Vec<POINT> = points.iter().copied().map(p).collect();
                Polyline(dc, &points).ok();
            }
            ShapeKind::Line(from, to) => {
                let a = p(*from);
                let b = p(*to);
                MoveToEx(dc, a.x, a.y, None).ok();
                LineTo(dc, b.x, b.y).ok();
            }
            ShapeKind::Arrow(from, to) => {
                let a = p(*from);
                let b = p(*to);
                MoveToEx(dc, a.x, a.y, None).ok();
                LineTo(dc, b.x, b.y).ok();
                let dx = (b.x - a.x) as f32;
                let dy = (b.y - a.y) as f32;
                let length = dx.hypot(dy).max(1.0);
                let ux = dx / length;
                let uy = dy / length;
                let size = (shape.width * 4).max(12) as f32;
                let left = POINT {
                    x: b.x - (ux * size - uy * size * 0.55) as i32,
                    y: b.y - (uy * size + ux * size * 0.55) as i32,
                };
                let right = POINT {
                    x: b.x - (ux * size + uy * size * 0.55) as i32,
                    y: b.y - (uy * size - ux * size * 0.55) as i32,
                };
                Polygon(dc, &[left, b, right]);
            }
            ShapeKind::Rectangle(from, to) => {
                SelectObject(dc, GetStockObject(NULL_BRUSH));
                let a = p(*from);
                let b = p(*to);
                Rectangle(dc, a.x, a.y, b.x, b.y);
            }
            ShapeKind::Text(pos, wrap, text) => {
                let pos = p(*pos);
                let font = CreateFontW(
                    -shape.font_size.max(18),
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
                    CLEARTYPE_QUALITY.0 as u32,
                    0,
                    PCWSTR(platform_core::i18n::wide(&shape.font_name).as_ptr()),
                );
                let old_font = SelectObject(dc, font);
                SetBkMode(dc, TRANSPARENT);
                SetTextColor(dc, COLORREF(shape.color));
                let mut wide = platform_core::i18n::wide(text);
                let mut rect = RECT {
                    left: pos.x,
                    top: pos.y,
                    right: pos.x + (*wrap).max(1),
                    bottom: pos.y + 4000,
                };
                DrawTextW(dc, &mut wide, &mut rect, DT_LEFT | DT_TOP | DT_WORDBREAK);
                SelectObject(dc, old_font);
                DeleteObject(font);
            }
        }
        SelectObject(dc, old_pen);
        SelectObject(dc, old_brush);
        DeleteObject(pen);
        DeleteObject(brush);
    }
}

pub fn register(instance: HINSTANCE) {
    unsafe {
        // SAFETY: The class name lives for the process. The new HWND is stored and destroyed with its owner.

        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
            lpfnWndProc: Some(wnd_proc),
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_CROSS).unwrap(),
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassW(&class);
    }
}

pub fn open(
    overlay: Overlay,
    allow_record: bool,
    palette: platform_core::palette::Palette,
) -> Result<(), String> {
    let _ = CORE_PALETTE.set(palette);
    let _ = PALETTE.set(Palette::from(palette));
    unsafe {
        // SAFETY: The class name lives for the process. The new HWND is stored and destroyed with its owner.

        if !REGION.is_null() {
            return Err(tr("Area selection is already active"));
        }
        let monitor = crate::os::cursor_monitor_rect();
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            CLASS,
            w!("Capture Button area"),
            WS_POPUP,
            monitor.x,
            monitor.y,
            monitor.w as i32,
            monitor.h as i32,
            None,
            None,
            GetModuleHandleW(None).unwrap(),
            None,
        )
        .map_err(|error| error.to_string())?;
        REGION = Box::into_raw(Region::new(hwnd, overlay, allow_record).inspect_err(|_| {
            // The window is not tracked yet; destroy it instead of
            // leaking a topmost layered window on every failed retry.
            DestroyWindow(hwnd).ok();
        })?);
        region().show();
        Ok(())
    }
}

#[cfg(test)]
pub fn open_with_frame(frame: Frame, scale: u32) -> Result<(), String> {
    unsafe {
        // SAFETY: The class name lives for the process. The new HWND is stored and destroyed with its owner.

        if !REGION.is_null() {
            shutdown();
        }
        let monitor = Rect {
            x: 0,
            y: 0,
            w: frame.width,
            h: frame.height,
        };
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            CLASS,
            w!("Capture Button area"),
            WS_POPUP,
            0,
            0,
            frame.width as i32,
            frame.height as i32,
            None,
            None,
            GetModuleHandleW(None).unwrap(),
            None,
        )
        .map_err(|error| error.to_string())?;
        REGION = Box::into_raw(Region::with_frame(
            hwnd,
            Overlay::Screenshot,
            true,
            frame,
            monitor,
        ));
        region().model.scale = scale;
        region().model.selection = Some(Selection {
            x: 96,
            y: 72,
            w: 360,
            h: 260,
        });
        region().model.shapes.push(Shape {
            kind: ShapeKind::Arrow(Point { x: 120, y: 100 }, Point { x: 300, y: 220 }),
            color: palette().SWATCHES[0].0,
            width: 6,
            font_size: 24,
            font_name: "Segoe UI".into(),
        });
        region().layout();
        region().show();
        Ok(())
    }
}

#[cfg(test)]
pub fn test_hwnd() -> HWND {
    region().hwnd
}

#[cfg(test)]
pub fn set_overlay(overlay: Overlay) {
    region().model.overlay = overlay;
    region().show();
}

#[cfg(test)]
pub fn hide_selection() {
    region().model.selection = None;
    region().show();
}

#[cfg(test)]
pub fn restore_sample_selection() {
    region().model.selection = Some(Selection {
        x: 96,
        y: 72,
        w: 360,
        h: 260,
    });
    region().show();
}

#[cfg(test)]
pub fn text_edit() -> HWND {
    region().edit
}

#[cfg(test)]
pub fn selection_screen_rect() -> RECT {
    let region = region();
    let selection = region.model.selected().expect("selection");
    let mut origin = POINT {
        x: selection.x,
        y: selection.y,
    };
    unsafe {
        // SAFETY: The POINT is in client coordinates of a window this program created.

        ClientToScreen(region.hwnd, &mut origin).ok();
    }
    RECT {
        left: origin.x,
        top: origin.y,
        right: origin.x + selection.w,
        bottom: origin.y + selection.h,
    }
}

#[cfg(test)]
pub fn composited() -> Option<Frame> {
    region().crop()
}

#[cfg(test)]
pub fn demonstrate_tools() {
    let region = region();
    let ink = |kind| Shape {
        kind,
        color: palette().SWATCHES[0].0,
        width: 5,
        font_size: 32,
        font_name: "Segoe UI".into(),
    };
    region.model.shapes.push(ink(ShapeKind::Pen(vec![
        Point { x: 130, y: 110 },
        Point { x: 190, y: 160 },
        Point { x: 170, y: 210 },
    ])));
    region.model.shapes.push(ink(ShapeKind::Line(
        Point { x: 150, y: 280 },
        Point { x: 420, y: 110 },
    )));
    region.model.shapes.push(ink(ShapeKind::Rectangle(
        Point { x: 220, y: 100 },
        Point { x: 360, y: 200 },
    )));
    region.model.shapes.push(Shape {
        kind: ShapeKind::Marker(vec![Point { x: 160, y: 250 }, Point { x: 400, y: 250 }]),
        color: palette().MARKER.0,
        width: 6,
        font_size: 32,
        font_name: "Segoe UI".into(),
    });
}

#[cfg(test)]
pub fn chrome_screen_rect(role: Role) -> Option<RECT> {
    let region = region();
    let button = region
        .model
        .buttons
        .iter()
        .find(|button| button.role == role && button.shown)?;
    let mut point = POINT {
        x: button.x,
        y: button.y,
    };
    unsafe {
        // SAFETY: The POINT is in client coordinates of a window this program created.

        ClientToScreen(region.hwnd, &mut point).ok();
    }
    Some(RECT {
        left: point.x,
        top: point.y,
        right: point.x + button.w,
        bottom: point.y + button.h,
    })
}

#[cfg(test)]
pub fn select_tool(role: Role) {
    let region = region();
    let metrics = region.metrics();
    let action = region.model.select_role(role, &metrics);
    region.perform(action);
}

pub fn active() -> bool {
    // SAFETY: This only reads the pointer. The region is used only when it is non-null.
    unsafe { !REGION.is_null() }
}

#[cfg(test)]
pub fn select_sample() {
    region().model.selection = Some(Selection {
        x: 40,
        y: 40,
        w: 220,
        h: 140,
    });
    region().layout();
}

#[cfg(test)]
pub fn set_record_allowed(allowed: bool) {
    region().model.allow_record = allowed;
    region().redraw();
}

#[cfg(test)]
pub fn press_role(role: Role) {
    let region = region();
    let Some(index) = region
        .model
        .buttons
        .iter()
        .position(|button| button.role == role && button.shown)
    else {
        return;
    };
    let metrics = region.metrics();
    let action = region.model.press(index, Point { x: 0, y: 0 }, &metrics);
    region.perform(action);
}

#[cfg(test)]
pub fn has_outcome() -> bool {
    region().model.outcome.is_some()
}

pub fn take_outcome() -> Option<Outcome> {
    // The overlay can already be gone when a second posted notification
    // arrives. Dereferencing REGION here used to kill the whole process.
    unsafe {
        // SAFETY: This only checks the pointer; it is dereferenced below only when non-null.
        if REGION.is_null() {
            return None;
        }
    }
    let outcome = region().model.outcome.take();
    if outcome.is_some() {
        shutdown();
    }
    outcome
}

pub fn cancel() {
    unsafe {
        // SAFETY: Windows calls this only for the short-lived probe edit. Every message is forwarded to DefWindowProc.

        if !REGION.is_null() {
            region().finish(Outcome::Cancel);
        }
    }
}

pub fn shutdown() {
    unsafe {
        // SAFETY: Windows calls this only for the short-lived probe edit. Every message is forwarded to DefWindowProc.

        if !REGION.is_null() {
            let region = Box::from_raw(REGION);
            if !region.edit.is_invalid() {
                DestroyWindow(region.edit).ok();
            }
            DestroyWindow(region.hwnd).ok();
            REGION = std::ptr::null_mut();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // SAFETY: Windows calls this only for the short-lived probe edit. Every message is forwarded to DefWindowProc.
    unsafe extern "system" fn probe_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        DefWindowProcW(hwnd, message, wparam, lparam)
    }

    #[test]
    fn edit_text_inset_is_stable() {
        unsafe {
            // SAFETY: The class name lives for the process. The new HWND is stored and destroyed with its owner.

            let instance = GetModuleHandleW(None).unwrap();
            let class = w!("CaptureButton.edit.probe");
            let window = WNDCLASSW {
                lpfnWndProc: Some(probe_proc),
                hInstance: instance.into(),
                lpszClassName: class,
                ..Default::default()
            };
            RegisterClassW(&window);
            let parent = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class,
                w!("probe"),
                WS_POPUP,
                0,
                0,
                400,
                300,
                None,
                None,
                instance,
                None,
            )
            .unwrap();
            let style = WS_CHILD | WINDOW_STYLE(ES_MULTILINE as u32 | ES_AUTOVSCROLL as u32);
            let edit = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("EDIT"),
                w!("Hello"),
                style,
                10,
                20,
                240,
                80,
                parent,
                None,
                instance,
                None,
            )
            .unwrap();
            let mut before = RECT::default();
            SendMessageW(
                edit,
                0x00B2,
                WPARAM(0),
                LPARAM(&mut before as *mut RECT as isize),
            );
            let font = CreateFontW(
                -28,
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
                CLEARTYPE_QUALITY.0 as u32,
                0,
                w!("Segoe UI"),
            );
            SendMessageW(edit, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
            ShowWindow(parent, SW_SHOWNA);
            ShowWindow(edit, SW_SHOW);
            UpdateWindow(edit);
            let dc = GetDC(edit);
            let mut ink = None;
            for y in 0..40 {
                for x in 0..80 {
                    let pixel = GetPixel(dc, x, y);
                    let r = pixel.0 & 255;
                    if r < 40 {
                        ink = Some((x, y));
                        break;
                    }
                }
                if ink.is_some() {
                    break;
                }
            }
            let mem = CreateCompatibleDC(dc);
            let bmp = CreateCompatibleBitmap(dc, 240, 80);
            SelectObject(mem, bmp);
            let white = CreateSolidBrush(palette().WHITE);
            let fill = RECT {
                left: 0,
                top: 0,
                right: 240,
                bottom: 80,
            };
            FillRect(mem, &fill, white);
            DeleteObject(white);
            let old = SelectObject(mem, font);
            SetBkMode(mem, TRANSPARENT);
            SetTextColor(mem, palette().BLACK);
            let mut drawn = RECT {
                left: 0,
                top: 0,
                right: 240,
                bottom: 80,
            };
            let mut wide = platform_core::i18n::wide("Hello");
            DrawTextW(mem, &mut wide, &mut drawn, DT_LEFT | DT_TOP);
            let mut drawn_ink = None;
            for y in 0..40 {
                for x in 0..80 {
                    let pixel = GetPixel(mem, x, y);
                    if pixel.0 & 255 < 40 {
                        drawn_ink = Some((x, y));
                        break;
                    }
                }
                if drawn_ink.is_some() {
                    break;
                }
            }
            let margins = SendMessageW(edit, 0x00D4, WPARAM(0), LPARAM(0)).0 as i32;
            let (origin, wrap) = text_commit_box(
                Point { x: 10, y: 20 },
                240,
                before.left,
                before.top,
                before.right,
                margins,
            );
            SelectObject(mem, old);
            DeleteObject(bmp);
            DeleteDC(mem);
            ReleaseDC(edit, dc);
            DeleteObject(font);
            DestroyWindow(edit).ok();
            DestroyWindow(parent).ok();
            let (edit_at, drawn_at) = (ink.expect("edit glyph"), drawn_ink.expect("drawn glyph"));
            assert_eq!(
                edit_at.0 - drawn_at.0,
                margins & 0xffff,
                "the edit's left margin is the horizontal jump"
            );
            assert_eq!(edit_at.1, drawn_at.1, "the edit and DrawText share a top");
            assert_eq!(
                origin,
                Point {
                    x: 10 + (margins & 0xffff),
                    y: 20
                }
            );
            assert_eq!(wrap, 240 - (margins & 0xffff) - (margins >> 16));
        }
    }

    #[test]
    fn a_marker_dot_is_anti_aliased() {
        let mut frame = Frame {
            width: 64,
            height: 64,
            data: vec![0; 64 * 64 * 4],
        };
        let dot = Shape {
            kind: ShapeKind::Marker(vec![Point { x: 32, y: 32 }]),
            color: 0x00ffffff,
            width: 5,
            font_size: 16,
            font_name: "Segoe UI".into(),
        };
        assert!(draw_annotations(
            &mut frame,
            &[dot],
            Point { x: 0, y: 0 }
        ));
        let colors: std::collections::HashSet<[u8; 4]> = frame
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]])
            .collect();
        assert!(
            colors.len() > 10,
            "a hard-edged dot has two colors, an anti-aliased one blends its rim; got {}",
            colors.len()
        );
    }

    #[test]
    fn annotations_are_composited_into_the_frame() {
        let mut frame = Frame {
            width: 8,
            height: 8,
            data: vec![255; 8 * 8 * 4],
        };
        let shape = Shape {
            kind: ShapeKind::Line(Point { x: 0, y: 0 }, Point { x: 7, y: 7 }),
            color: palette().SWATCHES[0].0,
            width: 2,
            font_size: 12,
            font_name: "Segoe UI".into(),
        };
        draw_annotations(&mut frame, &[shape], Point { x: 0, y: 0 });

        let pixels = frame.data.as_chunks::<4>().0;

        assert!(

            pixels.iter().any(|pixel| pixel[..3] != [255, 255, 255]),

            "the stroke never reached the frame"

        );

        assert!(

            pixels.iter().all(|pixel| pixel[3] == 255),

            "a stroke pixel was stored with zero alpha and would punch a hole"

        );

    }
}


#[test]
fn a_glyph_is_centred_on_its_ink() {
    unsafe {
        // SAFETY: The DCs, the bitmap and the font are created here and released before return.
        let screen = GetDC(None);
        let dc = CreateCompatibleDC(screen);
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = 64;
        info.bmiHeader.biHeight = -64;
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB.0;
        let mut bits = std::ptr::null_mut();
        let bitmap = CreateDIBSection(screen, &info, DIB_RGB_COLORS, &mut bits, None, 0).unwrap();
        let old = SelectObject(dc, bitmap);
        let black = CreateSolidBrush(COLORREF(0));
        let area = RECT {
            left: 0,
            top: 0,
            right: 64,
            bottom: 64,
        };
        FillRect(dc, &area, black);
        DeleteObject(black);
        static FONT: &[u8] = include_bytes!("../../../assets/Phosphor.ttf");
        let count = 0u32;
        AddFontMemResourceEx(FONT.as_ptr().cast(), FONT.len() as u32, None, &count);
        let font = CreateFontW(
            -25,
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
            CLEARTYPE_QUALITY.0 as u32,
            0,
            w!("Phosphor"),
        );
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, palette().WHITE);
        let glyph = icon(Role::Cancel).expect("a glyph for cancel");
        assert!(
            glyph_ink(dc, font, glyph).is_some(),
            "the icon font gives glyph metrics"
        );
        assert!(draw_glyph_centered(dc, font, glyph, 32, 32));
        SelectObject(dc, old);
        let pixels = std::slice::from_raw_parts(bits.cast::<u8>(), 64 * 64 * 4);
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (64, 64, 0, 0);
        let mut seen = false;
        for y in 0..64 {
            for x in 0..64 {
                let index = (y * 64 + x) * 4;
                if pixels[index] > 160 && pixels[index + 1] > 160 && pixels[index + 2] > 160 {
                    seen = true;
                    min_x = min_x.min(x);
                    min_y = min_y.min(y);
                    max_x = max_x.max(x);
                    max_y = max_y.max(y);
                }
            }
        }
        assert!(seen, "the glyph drew no ink");
        let dx = ((min_x + max_x) as f32 / 2.0 - 32.0).abs();
        let dy = ((min_y + max_y) as f32 / 2.0 - 32.0).abs();
        assert!(dx <= 1.0 && dy <= 1.0, "ink centre is off by ({dx}, {dy})");
        DeleteObject(font);
        DeleteObject(bitmap);
        DeleteDC(dc);
        ReleaseDC(None, screen);
    }
}

#[cfg(test)]
mod region_actions {
    use super::{palette, suggested_image_path, Role};

    #[test]
    fn copy_is_green_and_cancel_is_red() {
        assert_eq!(
            super::button_fill(Role::Copy, true, false, None),
            palette().COPY
        );
        assert_eq!(
            super::button_fill(Role::Cancel, true, false, None),
            palette().CANCEL
        );
        assert_eq!(
            super::button_fill(Role::Record, true, true, None),
            palette().ACCENT
        );
    }

    #[test]
    fn the_save_dialog_starts_with_a_generated_name() {
        let path = suggested_image_path();
        let name = std::path::Path::new(&path)
            .file_name()
            .unwrap()
            .to_string_lossy();
        assert!(name.starts_with("Image_"), "{name}");
        assert!(name.ends_with(".png"), "{name}");
    }
}
