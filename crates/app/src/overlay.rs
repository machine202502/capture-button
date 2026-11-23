//! The Slint area-selection overlay.
//!
//! This is a renderer only. The state, the layout, the hit-testing, the button
//! policy and the keys are `platform_core::region::Model`; this module turns the
//! model into the values `ui/overlay.slint` draws ([`scene`] does that as a pure
//! function, so the drawing can be tested without a window), feeds pointer, key
//! and text events back, and performs the [`Action`]s the model returns.
//!
//! The native GDI overlay in `platform-windows/src/region.rs` is the reference
//! for every behaviour here. Nothing in this file calls Win32.
//!
//! The window is hosted by the application's Slint UI thread, which is in one of
//! two states: running the main window's event loop ([`spawn`]), or parked
//! waiting for a command ([`run`], which drives its own event loop until the
//! overlay closes).

use platform::i18n::tr;
use platform::palette::Palette;
use platform::region::{
    caption, handles, icon, role_enabled, Action, Button, FontSeed, Key, Metrics, Model, Outcome,
    Point, Role, Shape, ShapeKind, Tool,
};
use slint::{ComponentHandle, ModelRc};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

include!(concat!(env!("OUT_DIR"), "/overlay.rs"));

/// Who runs the window's event loop.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    /// The main window's loop is running and the overlay lives inside it.
    Shared,
    /// The UI thread was idle; this overlay runs its own loop.
    Nested,
}

/// How often the overlay notices an outcome the application published from
/// another thread (the way [`platform::region::cancel`] closes it).
const WATCH: Duration = Duration::from_millis(150);

/// How close two presses have to be to count as a double click.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
const DOUBLE_CLICK_SLOP: i32 = 4;

/// What the overlay remembers between events, next to the neutral model.
struct State {
    model: Model,
    mode: Mode,
    /// The window's DPI factor. The model counts the frozen screen's pixels,
    /// Slint counts logical ones.
    dpi: f32,
    /// Set once the overlay has published its one outcome.
    closed: bool,
    last_press: Option<(Instant, Point)>,
    /// The text being typed. The model only remembers where the text box is.
    edit_text: String,
    watch: Rc<slint::Timer>,
    /// One-shot rebuild once Windows has told Slint the window's real scale.
    settle: Rc<slint::Timer>,
}

thread_local! {
    /// The overlay shown inside the main window's event loop. It is kept here so
    /// its window is only dropped on a later open, never from inside one of its
    /// own callbacks.
    static VISIBLE: RefCell<Option<OverlayWindow>> = const { RefCell::new(None) };
}

/// The Slint color for a `0x00BBGGRR` palette value.
fn color(value: u32) -> slint::Color {
    slint::Color::from_rgb_u8(
        (value & 0xff) as u8,
        ((value >> 8) & 0xff) as u8,
        ((value >> 16) & 0xff) as u8,
    )
}

/// The body font size of the overlay chrome, in screen pixels.
fn label_size(scale: u32) -> i32 {
    (15 * scale as i32 / 100).max(13)
}

/// The glyph font size of the overlay chrome, in screen pixels.
fn glyph_size(scale: u32) -> i32 {
    (17 * scale as i32 / 100).max(13)
}

/// The padding around the text box, in screen pixels.
fn text_pad(scale: u32) -> i32 {
    (8 * scale as i32 / 100).max(6)
}

/// The model's screen pixels in the window's logical pixels.
fn to_window(value: i32, dpi: f32) -> f32 {
    value as f32 / dpi
}

fn rect(x: i32, y: i32, w: i32, h: i32, dpi: f32) -> OverlayRect {
    OverlayRect {
        x: to_window(x, dpi),
        y: to_window(y, dpi),
        width: to_window(w.max(0), dpi),
        height: to_window(h.max(0), dpi),
    }
}

/// Everything the window draws for one state of the model.
///
/// The coordinates are logical pixels, like every `length` in Slint; the model
/// works in the frozen screen's pixels, so [`scene`] divides by the window's DPI
/// factor.
pub struct Scene {
    pub dims: Vec<OverlayRect>,
    pub borders: Vec<OverlayRect>,
    pub handles: Vec<OverlayPoint>,
    pub buttons: Vec<OverlayButton>,
    pub shapes: Vec<OverlayShape>,
    pub badge: Option<OverlayRect>,
    pub badge_text: String,
    pub hint: Option<String>,
    pub editor: Option<OverlayRect>,
    pub editor_font_size: f32,
}

/// Slint cannot measure text for the model's layout.
///
/// The only button with a caption is Record, and the model's own widths are
/// generous, so an average glyph width keeps the buttons close to the native
/// ones. The size badge uses the same estimate.
struct Estimate;

impl Metrics for Estimate {
    fn text_width(&self, text: &str, font: FontSeed, scale: u32) -> i32 {
        let per_char = match font {
            FontSeed::Body => 7,
            FontSeed::Icon => 10,
        };
        (text.chars().count() as i32 * per_char * scale as i32 / 100).max(1)
    }
}

/// Builds what the window draws. Pure: no window, no OS.
pub fn build_scene(model: &Model, dpi: f32) -> Scene {
    let dpi = if dpi.is_finite() && dpi > 0.0 { dpi } else { 1.0 };
    let width = model.frame.width as i32;
    let height = model.frame.height as i32;
    let selection = model.selected();

    // The dim cover: everything but the selection. With no selection the whole
    // frozen screen is covered, exactly like the native overlay.
    let mut dims = Vec::new();
    match selection {
        Some(selection) => {
            for (x, y, w, h) in [
                (0, 0, width, selection.y),
                (0, selection.bottom(), width, height - selection.bottom()),
                (0, selection.y, selection.x, selection.h),
                (
                    selection.right(),
                    selection.y,
                    width - selection.right(),
                    selection.h,
                ),
            ] {
                if w > 0 && h > 0 {
                    dims.push(rect(x, y, w, h, dpi));
                }
            }
        }
        None => dims.push(rect(0, 0, width, height, dpi)),
    }

    let borders = selection
        .map(|selection| {
            vec![rect(
                selection.x,
                selection.y,
                selection.w,
                selection.h,
                dpi,
            )]
        })
        .unwrap_or_default();

    let mut points = Vec::new();
    if let Some(selection) = selection {
        if model.tool == Tool::Select {
            for (x, y) in handles(selection) {
                points.push(OverlayPoint {
                    x: to_window(x, dpi),
                    y: to_window(y, dpi),
                });
            }
        }
    }

    let mut buttons = Vec::new();
    for button in &model.buttons {
        if button.shown {
            buttons.push(button_scene(model, button, dpi));
        }
    }

    let mut shapes = Vec::new();
    for shape in model.shapes.iter().chain(model.current.iter()) {
        push_shape(&mut shapes, shape, model, dpi);
    }

    let mut badge = None;
    let mut badge_text = String::new();
    if let Some(selection) = selection {
        badge_text = format!("{} × {}", selection.w, selection.h);
        let size = (
            Estimate.text_width(&badge_text, FontSeed::Body, model.scale) + 16,
            label_size(model.scale) + 8,
        );
        let mut blocked: Vec<platform::geom::Panel> = model
            .buttons
            .iter()
            .filter(|button| button.shown)
            .map(|button| platform::geom::Panel {
                x: button.x,
                y: button.y,
                w: button.w,
                h: button.h,
            })
            .collect();
        if model.editing {
            blocked.push(model.text_outer().panel());
        }
        let screen = platform::geom::Panel {
            x: 0,
            y: 0,
            w: width,
            h: height,
        };
        let spot = platform::geom::place_badge(screen, selection.panel(), size, &blocked);
        badge = Some(rect(spot.x, spot.y, spot.w, spot.h, dpi));
    }

    // The native overlay shows the hint only while nothing is selected.
    let hint = selection.is_none().then(|| {
        tr("Drag to select an area    ·    Double click: full screen    ·    Tab: next display    ·    Esc: cancel")
    });

    let editor = model.editing.then(|| {
        rect(
            model.edit_pos.x,
            model.edit_pos.y,
            model.edit_w,
            model.edit_h,
            dpi,
        )
    });

    Scene {
        dims,
        borders,
        handles: points,
        buttons,
        shapes,
        badge,
        badge_text,
        hint,
        editor,
        editor_font_size: to_window(model.font_size.max(18), dpi),
    }
}

/// The fill of a button, as the native overlay chooses it.
fn button_fill(palette: &Palette, role: Role, enabled: bool, primary: bool, swatch: Option<usize>) -> u32 {
    if let Some(swatch) = swatch {
        palette.SWATCHES[swatch]
    } else if !enabled {
        palette.OVERLAY_DISABLED
    } else if role == Role::Copy {
        palette.COPY
    } else if role == Role::Cancel {
        palette.CANCEL
    } else if primary {
        palette.ACCENT
    } else {
        palette.OVERLAY
    }
}

/// The ink of a button, as the native overlay chooses it.
fn button_ink(palette: &Palette, role: Role, enabled: bool, primary: bool) -> u32 {
    if !enabled {
        palette.OVERLAY_DISABLED_INK
    } else if primary || matches!(role, Role::Copy | Role::Cancel) {
        palette.ON_ACCENT
    } else {
        palette.OVERLAY_INK
    }
}

fn button_scene(model: &Model, button: &Button, dpi: f32) -> OverlayButton {
    let enabled = role_enabled(&model.shapes, &model.redo, model.allow_record, button.role);
    let selected = matches!(
        (button.role, model.tool),
        (Role::Select, Tool::Select)
            | (Role::Pen, Tool::Pen)
            | (Role::Line, Tool::Line)
            | (Role::Arrow, Tool::Arrow)
            | (Role::Rectangle, Tool::Rectangle)
            | (Role::Marker, Tool::Marker)
            | (Role::Text, Tool::Text)
    );
    let primary = selected
        || matches!(
            button.role,
            Role::Record | Role::RecordMkv | Role::RecordGif | Role::RecordWebp
        );
    let palette = &model.palette;
    let fill = button_fill(palette, button.role, enabled, primary, button.swatch);
    let ink = button_ink(palette, button.role, enabled, primary);
    let edge = if selected {
        palette.ACCENT_DEEP
    } else {
        palette.OVERLAY_EDGE
    };
    let (special, dot, stroke) = match button.role {
        Role::Color => (1, model.color, 0),
        Role::Width => (2, 0, model.width),
        _ => match button.swatch {
            Some(index) => (3, palette.SWATCHES[index], 0),
            None => (0, 0, 0),
        },
    };
    OverlayButton {
        x: to_window(button.x, dpi),
        y: to_window(button.y, dpi),
        width: to_window(button.w, dpi),
        height: to_window(button.h, dpi),
        fill: color(fill),
        ink: color(ink),
        edge: color(edge),
        glyph: icon(button.role).unwrap_or_default().into(),
        caption: caption(button.role).unwrap_or_default().into(),
        special,
        dot: color(dot),
        stroke_width: to_window(stroke, dpi),
        swatch_on: button
            .swatch
            .is_some_and(|index| palette.SWATCHES[index] == model.color),
    }
}

/// Adds one window element for a path annotation.
fn push_path(out: &mut Vec<OverlayShape>, commands: String, kind: i32, ink: u32, width: i32, dpi: f32) {
    out.push(OverlayShape {
        kind,
        commands: commands.into(),
        color: color(ink),
        stroke_width: to_window(width.max(1), dpi),
        ..Default::default()
    });
}

/// Appends the window elements for one annotation, following the native
/// overlay's drawing rules: markers are thicker and use the marker color, an
/// arrow head is a filled triangle, and a one-point pen is a dot.
fn push_shape(out: &mut Vec<OverlayShape>, shape: &Shape, model: &Model, dpi: f32) {
    let marker = matches!(shape.kind, ShapeKind::Marker(_));
    let ink = if marker {
        model.palette.MARKER
    } else {
        shape.color
    };
    let width = if marker { shape.width * 4 } else { shape.width }.max(1);
    let px = |value: i32| value as f32 / dpi;
    match &shape.kind {
        ShapeKind::Pen(points) | ShapeKind::Marker(points) => {
            let commands: Vec<String> = points
                .iter()
                .enumerate()
                .map(|(index, point)| {
                    format!(
                        "{}{} {}",
                        if index == 0 { "M " } else { "L " },
                        px(point.x),
                        px(point.y)
                    )
                })
                .collect();
            push_path(out, commands.join(" "), 0, ink, width, dpi);
            if points.len() == 1 {
                // A single click leaves a dot, like the native `Ellipse`.
                let point = points[0];
                out.push(OverlayShape {
                    kind: 3,
                    x: px(point.x),
                    y: px(point.y),
                    radius: px(width) / 2.0,
                    color: color(ink),
                    ..Default::default()
                });
            }
        }
        ShapeKind::Line(from, to) => {
            push_path(
                out,
                format!(
                    "M {} {} L {} {}",
                    px(from.x),
                    px(from.y),
                    px(to.x),
                    px(to.y)
                ),
                0,
                ink,
                width,
                dpi,
            );
        }
        ShapeKind::Rectangle(from, to) => {
            let (left, right) = (from.x.min(to.x), from.x.max(to.x));
            let (top, bottom) = (from.y.min(to.y), from.y.max(to.y));
            push_path(
                out,
                format!(
                    "M {} {} L {} {} L {} {} L {} {} Z",
                    px(left),
                    px(top),
                    px(right),
                    px(top),
                    px(right),
                    px(bottom),
                    px(left),
                    px(bottom)
                ),
                0,
                ink,
                width,
                dpi,
            );
        }
        ShapeKind::Arrow(from, to) => {
            push_path(
                out,
                format!(
                    "M {} {} L {} {}",
                    px(from.x),
                    px(from.y),
                    px(to.x),
                    px(to.y)
                ),
                0,
                ink,
                width,
                dpi,
            );
            let dx = (to.x - from.x) as f32;
            let dy = (to.y - from.y) as f32;
            let length = dx.hypot(dy).max(1.0);
            let (ux, uy) = (dx / length, dy / length);
            let size = (shape.width * 4).max(12) as f32;
            let left = (
                to.x as f32 - (ux * size - uy * size * 0.55),
                to.y as f32 - (uy * size + ux * size * 0.55),
            );
            let right = (
                to.x as f32 - (ux * size + uy * size * 0.55),
                to.y as f32 - (uy * size - ux * size * 0.55),
            );
            push_path(
                out,
                format!(
                    "M {} {} L {} {} L {} {} Z",
                    px(left.0.round() as i32),
                    px(left.1.round() as i32),
                    px(to.x),
                    px(to.y),
                    px(right.0.round() as i32),
                    px(right.1.round() as i32)
                ),
                1,
                ink,
                0,
                dpi,
            );
        }
        ShapeKind::Text(at, wrap, text) => {
            out.push(OverlayShape {
                kind: 2,
                x: px(at.x),
                y: px(at.y),
                width: px(*wrap),
                text: text.clone().into(),
                font_size: px(shape.font_size.max(18)),
                color: color(shape.color),
                ..Default::default()
            });
        }
    }
}

/// Paints the palette and the chrome sizes into the window once.
fn install_chrome(ui: &OverlayWindow, palette: &Palette, scale: u32, dpi: f32) {
    ui.set_accent(color(palette.ACCENT));
    ui.set_white(color(palette.WHITE));
    ui.set_badge_fill(color(palette.BADGE));
    ui.set_badge_ink(color(palette.BADGE_TEXT));
    ui.set_hint_ink(color(palette.WHITE));
    ui.set_hint_shadow(color(palette.HINT_SHADOW));
    ui.set_glyph_size(to_window(glyph_size(scale), dpi));
    ui.set_label_size(to_window(label_size(scale), dpi));
    ui.set_text_pad(to_window(text_pad(scale), dpi));
    ui.set_handle_size(to_window(10, dpi));
    ui.set_unit(1.0 / dpi);
}

/// Puts the frozen screen into the window. The frame is BGRA, Slint wants RGBA,
/// so this converts it once per grabbed display.
fn install_frame(ui: &OverlayWindow, model: &Model) {
    let frame = &model.frame;
    if frame.width == 0 || frame.height == 0 {
        ui.set_frame_image(slint::Image::default());
        return;
    }
    let mut buffer =
        slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(frame.width, frame.height);
    let pixels = buffer.make_mut_bytes();
    let source = &frame.data;
    for index in 0..(frame.width as usize * frame.height as usize) {
        let offset = index * 4;
        if offset + 4 > source.len() {
            break;
        }
        pixels[offset] = source[offset + 2];
        pixels[offset + 1] = source[offset + 1];
        pixels[offset + 2] = source[offset];
        pixels[offset + 3] = 255;
    }
    ui.set_frame_image(slint::Image::from_rgba8(buffer));
}

/// Moves and sizes the window to the model's monitor.
fn place(ui: &OverlayWindow, model: &Model) {
    let monitor = model.monitor;
    ui.window()
        .set_position(slint::PhysicalPosition::new(monitor.x, monitor.y));
    ui.window()
        .set_size(slint::PhysicalSize::new(monitor.w.max(1), monitor.h.max(1)));
}

/// Rebuilds the scene and pushes it into the window.
fn apply(ui: &OverlayWindow, state: &Rc<RefCell<State>>) {
    let (scene, edit_text, edit_ink, had_editor) = {
        let current = state.borrow();
        (
            build_scene(&current.model, current.dpi),
            current.edit_text.clone(),
            color(current.model.color),
            ui.get_has_editor(),
        )
    };
    ui.set_dims(ModelRc::new(slint::VecModel::from(scene.dims)));
    ui.set_borders(ModelRc::new(slint::VecModel::from(scene.borders)));
    ui.set_handles(ModelRc::new(slint::VecModel::from(scene.handles)));
    ui.set_buttons(ModelRc::new(slint::VecModel::from(scene.buttons)));
    ui.set_shapes(ModelRc::new(slint::VecModel::from(scene.shapes)));
    ui.set_has_badge(scene.badge.is_some());
    if let Some(badge) = scene.badge {
        ui.set_badge(badge);
        ui.set_badge_text(scene.badge_text.into());
    }
    ui.set_hint_text(scene.hint.unwrap_or_default().into());
    if let Some(editor) = scene.editor {
        ui.set_editor(editor);
        ui.set_editor_font_size(scene.editor_font_size);
        ui.set_editor_ink(edit_ink);
        ui.set_editor_text(edit_text.into());
        ui.set_has_editor(true);
    } else {
        ui.set_has_editor(false);
    }
    if had_editor && !ui.get_has_editor() {
        // The text box took the keyboard with it; give the keys back.
        ui.invoke_focus_keys();
    }
}

/// The annotated still for the model's selection.
fn still(state: &Rc<RefCell<State>>) -> Option<platform::Frame> {
    platform::region::render_still(&state.borrow().model)
}

/// Converts a pointer position from the window's logical pixels into the
/// model's frozen-screen pixels.
///
/// Slint reports pointers in logical pixels and the model counts the pixels of
/// the grabbed monitor, so a click on a 200% display arrives at half the
/// distance from the origin unless it is multiplied back.
fn pointer_to_model(x: i32, y: i32, dpi: f32, width: i32, height: i32) -> Point {
    let dpi = if dpi.is_finite() && dpi > 0.0 { dpi } else { 1.0 };
    Point {
        x: ((x as f32 * dpi).round() as i32).clamp(0, width),
        y: ((y as f32 * dpi).round() as i32).clamp(0, height),
    }
}

/// The model's pixel for a pointer position.
///
/// The window covers exactly one monitor and its logical size is the monitor
/// size divided by the DPI factor, so the pointer has to be scaled back up.
/// Clamping keeps a press that lands on the very edge inside the frame.
fn model_point(state: &State, x: i32, y: i32) -> Point {
    pointer_to_model(
        x,
        y,
        state.dpi,
        state.model.frame.width as i32,
        state.model.frame.height as i32,
    )
}

fn pointer_down(ui: &OverlayWindow, state: &Rc<RefCell<State>>, x: i32, y: i32) {
    let (point, was_editing) = {
        let current = state.borrow();
        (model_point(&current, x, y), current.model.editing)
    };
    if was_editing {
        if state.borrow().model.inside_edit(point) {
            // The text box itself takes the press.
            return;
        }
        commit_editor(ui, state);
    }
    let double = {
        let mut current = state.borrow_mut();
        let now = Instant::now();
        let double = current.last_press.is_some_and(|(when, last)| {
            now.duration_since(when) <= DOUBLE_CLICK
                && (last.x - point.x).abs() <= DOUBLE_CLICK_SLOP
                && (last.y - point.y).abs() <= DOUBLE_CLICK_SLOP
        });
        current.last_press = Some((now, point));
        double
    };
    let action = {
        let mut current = state.borrow_mut();
        match current.model.hit_button(point) {
            Some(index) => current.model.press(index, point, &Estimate),
            None => current
                .model
                .mouse_down(point, double, was_editing, &Estimate),
        }
    };
    act(ui, state, action);
}

fn pointer_move(ui: &OverlayWindow, state: &Rc<RefCell<State>>, x: i32, y: i32) {
    let point = {
        let current = state.borrow();
        model_point(&current, x, y)
    };
    let action = state.borrow_mut().model.mouse_move(point);
    act(ui, state, action);
}

fn pointer_up(ui: &OverlayWindow, state: &Rc<RefCell<State>>, x: i32, y: i32) {
    let point = {
        let current = state.borrow();
        model_point(&current, x, y)
    };
    let action = state.borrow_mut().model.mouse_up(point, &Estimate);

    act(ui, state, action);
}

/// The neutral key for a Slint key event's text.
///
/// Slint encodes the special keys as control characters and private-use code
/// points. Ctrl+letter can arrive as the control character Windows produces, so
/// both forms map to the letter the model expects.
fn key_from_text(text: &str) -> Option<Key> {
    let mut chars = text.chars();
    let ch = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    Some(match ch {
        '\u{0008}' => Key::Backspace,
        '\u{0009}' => Key::Tab,
        '\u{000a}' => Key::Enter,
        '\u{001b}' => Key::Escape,
        '\u{007f}' => Key::Delete,
        '\u{F700}' => Key::ArrowUp,
        '\u{F701}' => Key::ArrowDown,
        '\u{F702}' => Key::ArrowLeft,
        '\u{F703}' => Key::ArrowRight,
        // Windows delivers Ctrl+letter as the matching control character.
        '\u{0001}'..='\u{001a}' => Key::Character((b'A' + (ch as u8 - 1)) as char),
        other if other.is_control() => return None,
        other => Key::Character(other.to_ascii_uppercase()),
    })
}

fn key_event(
    ui: &OverlayWindow,
    state: &Rc<RefCell<State>>,
    text: &str,
    control: bool,
    shift: bool,
) {
    let Some(key) = key_from_text(text) else {
        return;
    };
    if state.borrow().model.editing {
        // The text box owns the keyboard; leaving it is the overlay's.
        if key == Key::Escape {
            cancel_editor(ui, state);
        }
        return;
    }
    let action = state
        .borrow_mut()
        .model
        .key_down(key, control, shift, &Estimate);
    act(ui, state, action);
}

fn commit_editor(ui: &OverlayWindow, state: &Rc<RefCell<State>>) {
    let action = {
        let mut current = state.borrow_mut();
        if !current.model.editing {
            return;
        }
        let text = current.edit_text.clone();
        let origin = current.model.edit_pos;
        let wrap = current.model.edit_w;
        current.model.commit_text(text, origin, wrap, &Estimate)
    };
    act(ui, state, action);
}

fn cancel_editor(ui: &OverlayWindow, state: &Rc<RefCell<State>>) {
    let action = state.borrow_mut().model.cancel_text(&Estimate);
    act(ui, state, action);
}

/// Performs what the model decided it cannot do itself.
fn act(ui: &OverlayWindow, state: &Rc<RefCell<State>>, action: Action) {
    match action {
        Action::None => {}
        Action::Redraw => apply(ui, state),
        Action::OpenTextEditor { text, .. } => {
            state.borrow_mut().edit_text = text;
            apply(ui, state);
        }
        Action::CloseTextEditor => apply(ui, state),
        Action::SaveDialog => {
            let Some(frame) = still(state) else {
                return;
            };
            let Some(path) = platform::region::ask_save_path() else {
                return;
            };
            finish(ui, state, Outcome::Saved(frame, Some(path)));
        }
        Action::Save | Action::Copy => {
            let Some(frame) = still(state) else {
                return;
            };
            finish(ui, state, Outcome::Saved(frame, None));
        }
        Action::NextMonitor => next_monitor(ui, state),
        Action::Finish(outcome) => finish(ui, state, outcome),
    }
}

fn next_monitor(ui: &OverlayWindow, state: &Rc<RefCell<State>>) {
    let monitor = {
        let mut current = state.borrow_mut();
        if !matches!(current.model.next_monitor(), Action::NextMonitor) {
            return;
        }
        current.model.monitor
    };
    match platform::screen::grab_still(monitor) {
        Ok(frame) => {
            {
                let mut current = state.borrow_mut();
                current.model.frame = frame;
                current.model.selection = None;
                current.model.shapes.clear();
                current.model.redo.clear();
            }
            {
                let current = state.borrow();
                install_frame(ui, &current.model);
            }
            refit(ui, state);
        }
        Err(error) => {
            platform::logging::warn(format!(
                "slint overlay: could not capture the next display: {error}"
            ));
            apply(ui, state);
        }
    }
}

/// Publishes the one outcome and closes the window.
fn finish(ui: &OverlayWindow, state: &Rc<RefCell<State>>, outcome: Outcome) {
    let mode = {
        let mut current = state.borrow_mut();
        if current.closed {
            return;
        }
        current.closed = true;
        current.mode
    };
    platform::region::publish(outcome);
    close(ui, mode);
}

fn close(ui: &OverlayWindow, mode: Mode) {
    ui.hide().ok();
    if mode == Mode::Nested {
        let _ = slint::quit_event_loop();
    }
}

/// Logs a failure and returns the application to idle through the outcome
/// channel, so a renderer that cannot start never leaves it selecting.
fn report(error: impl std::fmt::Display) {
    platform::logging::error(format!("slint overlay: {error}"));
    platform::region::publish(Outcome::Cancel);
}

fn bind_callbacks(ui: &OverlayWindow, state: &Rc<RefCell<State>>) {
    let window = ui.as_weak();
    let shared = state.clone();
    ui.on_pointer_down(move |x, y, _middle| {
        // The middle button only moves the native text box, which the Slint
        // overlay does not offer.
        if let Some(ui) = window.upgrade() {
            pointer_down(&ui, &shared, x, y);
        }
    });

    let window = ui.as_weak();
    let shared = state.clone();
    ui.on_pointer_move(move |x, y| {
        if let Some(ui) = window.upgrade() {
            pointer_move(&ui, &shared, x, y);
        }
    });

    let window = ui.as_weak();
    let shared = state.clone();
    ui.on_pointer_up(move |x, y| {
        if let Some(ui) = window.upgrade() {
            pointer_up(&ui, &shared, x, y);
        }
    });

    let window = ui.as_weak();
    let shared = state.clone();
    ui.on_key_event(move |text, control, shift| {
        if let Some(ui) = window.upgrade() {
            key_event(&ui, &shared, &text, control, shift);
        }
    });

    let window = ui.as_weak();
    let shared = state.clone();
    ui.on_commit_text(move |text| {
        let Some(ui) = window.upgrade() else {
            return;
        };
        shared.borrow_mut().edit_text = text.to_string();
        commit_editor(&ui, &shared);
    });

    let window = ui.as_weak();
    let shared = state.clone();
    ui.on_cancel_text(move || {
        if let Some(ui) = window.upgrade() {
            cancel_editor(&ui, &shared);
        }
    });
}

/// Watches for an outcome another thread published — the way the application
/// cancels an overlay — and closes the window when one is waiting.
fn start_watch(ui: &OverlayWindow, state: &Rc<RefCell<State>>) {
    let window = ui.as_weak();
    let shared = Rc::downgrade(state);
    let timer = Rc::downgrade(&state.borrow().watch);
    state
        .borrow()
        .watch
        .start(slint::TimerMode::Repeated, WATCH, move || {
            let Some(shared) = shared.upgrade() else {
                return;
            };
            let closed = shared.borrow().closed;
            if closed {
                if let Some(timer) = timer.upgrade() {
                    timer.stop();
                }
                return;
            }
            let Some(ui) = window.upgrade() else {
                return;
            };
            // The window's scale can arrive late or change with the display.
            let scale = ui.window().scale_factor();
            let changed = {
                let mut current = shared.borrow_mut();
                if (current.dpi - scale).abs() > f32::EPSILON {
                    current.dpi = scale;
                    true
                } else {
                    false
                }
            };
            if changed {
                refit(&ui, &shared);
                return;
            }
            if !platform::region::pending() {
                return;
            }
            // The application cancelled the overlay and is waiting for the
            // window to go before it takes the outcome.
            {
                let mut current = shared.borrow_mut();
                if current.closed {
                    return;
                }
                current.closed = true;
            }
            let mode = shared.borrow().mode;
            close(&ui, mode);
            if let Some(timer) = timer.upgrade() {
                timer.stop();
            }
        });
}

/// Re-reads the window's DPI factor and rebuilds the scene with it.
///
/// Slint only knows the monitor's scale once the window is shown, and the model
/// counts the frozen screen's pixels while Slint counts logical ones, so the
/// scene has to be built again after the first show (and after a monitor switch,
/// which can land on a display with another scale).
fn refit(ui: &OverlayWindow, state: &Rc<RefCell<State>>) {
    let dpi = ui.window().scale_factor();
    if dpi.is_finite() && dpi > 0.0 {
        state.borrow_mut().dpi = dpi;
    }
    {
        let current = state.borrow();
        install_chrome(ui, &current.model.palette, current.model.scale, current.dpi);
        place(ui, &current.model);
    }
    apply(ui, state);
}

/// Rebuilds the scene once the window has been mapped.
///
/// Slint only learns the monitor's scale factor after Windows has mapped the
/// window, so the value read at creation is a placeholder. A one-shot timer
/// rebuilds the scene with the real factor, and the watch below keeps checking
/// it in case it changes again (another display, a DPI change).
fn settle(ui: &OverlayWindow, state: &Rc<RefCell<State>>) {
    let window = ui.as_weak();
    let shared = Rc::downgrade(state);
    let timer = state.borrow().settle.clone();
    timer.start(
        slint::TimerMode::SingleShot,
        Duration::from_millis(20),
        move || {
            if let (Some(ui), Some(shared)) = (window.upgrade(), shared.upgrade()) {
                refit(&ui, &shared);
            }
            // The window is mapped by now: put it above the shell's task bar even
            // when the toolkit applied always-on-top only after the first show.
            platform::shell::raise_overlay_window();
        },
    );
}

/// Creates and fills the window in the given mode, before it is shown.
fn create(
    model: Model,
    mode: Mode,
) -> Result<(OverlayWindow, Rc<RefCell<State>>), slint::PlatformError> {
    let ui = OverlayWindow::new()?;
    let dpi = ui.window().scale_factor();
    let mut model = model;
    model.layout(&Estimate);
    let state = Rc::new(RefCell::new(State {
        model,
        mode,
        dpi,
        closed: false,
        last_press: None,
        edit_text: String::new(),
        watch: Rc::new(slint::Timer::default()),
        settle: Rc::new(slint::Timer::default()),
    }));
    {
        let current = state.borrow();
        install_chrome(&ui, &current.model.palette, current.model.scale, dpi);
        place(&ui, &current.model);
        install_frame(&ui, &current.model);
    }
    apply(&ui, &state);
    bind_callbacks(&ui, &state);
    // A close request (Alt+F4) cancels the selection, like the native overlay's
    // `WM_CLOSE`; the window must not disappear while the app still waits.
    let window = ui.as_weak();
    let shared = state.clone();
    ui.window().on_close_requested(move || {
        if let Some(ui) = window.upgrade() {
            finish(&ui, &shared, Outcome::Cancel);
        }
        slint::CloseRequestResponse::KeepWindowShown
    });
    start_watch(&ui, &state);
    platform::region::report_open(true);
    Ok((ui, state))
}

/// Shows the overlay inside the main window's event loop. Called on the UI
/// thread through `slint::invoke_from_event_loop`.
pub fn spawn(model: Model) {
    match create(model, Mode::Shared) {
        Ok((ui, state)) => {
            if let Err(error) = ui.show() {
                report(error);
                return;
            }
            platform::shell::raise_overlay_window();
            settle(&ui, &state);
            let previous = VISIBLE.with(|slot| {
                let mut slot = slot.borrow_mut();
                let previous = slot.take();
                *slot = Some(ui);
                previous
            });
            drop(previous);
        }
        Err(error) => report(error),
    }
}

/// Runs the overlay on the idle UI thread until it closes.
pub fn run(model: Model) {
    match create(model, Mode::Nested) {
        Ok((ui, state)) => {
            if let Err(error) = ui.show() {
                report(error);
                return;
            }
            settle(&ui, &state);
            if let Err(error) = ui.run() {
                report(error);
            }
        }
        Err(error) => report(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform::palette;
    use platform::region::{Overlay, Selection};
    use platform::{Frame, Rect};

    fn fixture(overlay: Overlay) -> Model {
        Model::new(
            overlay,
            true,
            Frame {
                width: 800,
                height: 600,
                data: vec![0; 800 * 600 * 4],
            },
            Rect {
                x: 0,
                y: 0,
                w: 800,
                h: 600,
            },
            palette::DEFAULT,
        )
    }

    fn selected(overlay: Overlay) -> Model {
        let mut model = fixture(overlay);
        model.selection = Some(Selection {
            x: 100,
            y: 150,
            w: 200,
            h: 100,
        });
        model.layout(&Estimate);
        model
    }

    #[test]
    fn a_logical_pointer_is_scaled_back_to_screen_pixels() {
        // 100%: the window's logical pixels are the screen's pixels.
        assert_eq!(pointer_to_model(400, 300, 1.0, 3200, 2000), Point { x: 400, y: 300 });
        // 200%: a click at logical (400, 300) is physical (800, 600).
        assert_eq!(pointer_to_model(400, 300, 2.0, 3200, 2000), Point { x: 800, y: 600 });
        // 150%: rounding keeps the pixel the user aimed at.
        assert_eq!(pointer_to_model(401, 301, 1.5, 3200, 2000), Point { x: 602, y: 452 });
    }

    #[test]
    fn a_pointer_past_the_edge_stays_inside_the_frame() {
        assert_eq!(pointer_to_model(-10, -10, 2.0, 3200, 2000), Point { x: 0, y: 0 });
        assert_eq!(
            pointer_to_model(4000, 3000, 2.0, 3200, 2000),
            Point {
                x: 3200,
                y: 2000
            }
        );
        // A broken scale factor must not turn every click into (0, 0).
        assert_eq!(pointer_to_model(100, 200, f32::NAN, 3200, 2000), Point { x: 100, y: 200 });
    }
    fn button(model: &Model, scene: &Scene, role: Role) -> OverlayButton {
        let shown: Vec<&Button> = model.buttons.iter().filter(|button| button.shown).collect();
        let index = shown
            .iter()
            .position(|button| button.role == role)
            .unwrap_or_else(|| panic!("{role:?} is not shown"));
        scene.buttons[index].clone()
    }

    #[test]
    fn buttons_carry_their_rect_label_and_colour() {
        let model = selected(Overlay::Screenshot);
        let scene = build_scene(&model, 1.0);
        let copy = button(&model, &scene, Role::Copy);
        let expected = model
            .buttons
            .iter()
            .find(|button| button.role == Role::Copy && button.shown)
            .expect("copy");
        assert_eq!(copy.x, expected.x as f32);
        assert_eq!(copy.y, expected.y as f32);
        assert_eq!(copy.width, expected.w as f32);
        assert_eq!(copy.height, expected.h as f32);
        assert_eq!(copy.fill, color(model.palette.COPY));
        assert_eq!(copy.ink, color(model.palette.ON_ACCENT));
        assert_eq!(copy.glyph, icon(Role::Copy).expect("a glyph"));
        assert_eq!(copy.caption, "");
        assert_eq!(copy.special, 0);
        assert!(!copy.swatch_on);
    }

    #[test]
    fn disabled_and_selected_buttons_are_drawn_differently() {
        let model = selected(Overlay::Screenshot);
        let scene = build_scene(&model, 1.0);
        // Nothing has been drawn yet, so Undo is disabled.
        let undo = button(&model, &scene, Role::Undo);
        assert_eq!(undo.fill, color(model.palette.OVERLAY_DISABLED));
        assert_eq!(undo.ink, color(model.palette.OVERLAY_DISABLED_INK));
        // The select tool is the current one, so it is the primary button.
        let select = button(&model, &scene, Role::Select);
        assert_eq!(select.fill, color(model.palette.ACCENT));
        assert_eq!(select.edge, color(model.palette.ACCENT_DEEP));
    }

    #[test]
    fn the_selection_gets_a_border_and_eight_handles() {
        let model = selected(Overlay::Screenshot);
        let scene = build_scene(&model, 1.0);
        assert_eq!(scene.borders.len(), 1);
        assert_eq!(
            scene.borders[0],
            OverlayRect {
                x: 100.0,
                y: 150.0,
                width: 200.0,
                height: 100.0
            }
        );
        assert_eq!(scene.handles.len(), 8);
        assert_eq!(
            scene.handles[0],
            OverlayPoint {
                x: 100.0,
                y: 150.0
            }
        );
        assert_eq!(
            scene.handles[3],
            OverlayPoint {
                x: 300.0,
                y: 250.0
            }
        );
        // The pen tool keeps the area but hides the resize handles.
        let mut pen = model;
        pen.tool = Tool::Pen;
        pen.layout(&Estimate);
        assert!(build_scene(&pen, 1.0).handles.is_empty());
    }

    #[test]
    fn the_dim_cover_leaves_exactly_the_selection_out() {
        let scene = build_scene(&selected(Overlay::Screenshot), 1.0);
        assert_eq!(scene.dims.len(), 4);
        let area: f32 = scene
            .dims
            .iter()
            .map(|rect| rect.width * rect.height)
            .sum();
        assert_eq!(area, 800.0 * 600.0 - 200.0 * 100.0);
        let empty = build_scene(&fixture(Overlay::Screenshot), 1.0);
        assert_eq!(empty.dims.len(), 1);
        assert_eq!(empty.dims[0].width, 800.0);
        assert!(empty.hint.is_some());
        assert!(empty.badge.is_none());
        assert!(scene.hint.is_none());
        assert!(scene.badge.is_some());
    }

    #[test]
    fn annotations_become_path_commands_and_marker_strokes() {
        let mut model = selected(Overlay::Screenshot);
        model.shapes.push(Shape {
            kind: ShapeKind::Line(Point { x: 10, y: 20 }, Point { x: 30, y: 40 }),
            color: 0x000000ff,
            width: 4,
            font_size: 24,
            font_name: "Segoe UI".into(),
        });
        model.shapes.push(Shape {
            kind: ShapeKind::Marker(vec![Point { x: 5, y: 5 }, Point { x: 15, y: 5 }]),
            color: 0x000000ff,
            width: 3,
            font_size: 24,
            font_name: "Segoe UI".into(),
        });
        model.shapes.push(Shape {
            kind: ShapeKind::Arrow(Point { x: 0, y: 0 }, Point { x: 100, y: 0 }),
            color: 0x000000ff,
            width: 2,
            font_size: 24,
            font_name: "Segoe UI".into(),
        });
        model.layout(&Estimate);
        let scene = build_scene(&model, 1.0);
        assert_eq!(scene.shapes.len(), 4, "the arrow also has a head");
        assert_eq!(scene.shapes[0].kind, 0);
        assert_eq!(scene.shapes[0].commands, "M 10 20 L 30 40");
        assert_eq!(scene.shapes[0].stroke_width, 4.0);
        // A marker is drawn thicker and with the marker colour.
        assert_eq!(scene.shapes[1].commands, "M 5 5 L 15 5");
        assert_eq!(scene.shapes[1].stroke_width, 12.0);
        assert_eq!(scene.shapes[1].color, color(model.palette.MARKER));
        assert_eq!(scene.shapes[3].kind, 1, "the arrow head is filled");
    }

    #[test]
    fn the_same_scene_halves_every_length_on_a_hidpi_window() {
        let model = selected(Overlay::Screenshot);
        let scene = build_scene(&model, 2.0);
        assert_eq!(
            scene.borders[0],
            OverlayRect {
                x: 50.0,
                y: 75.0,
                width: 100.0,
                height: 50.0
            }
        );
        assert_eq!(
            scene.handles[0],
            OverlayPoint {
                x: 50.0,
                y: 75.0
            }
        );
        assert_eq!(
            scene.dims[0],
            OverlayRect {
                x: 0.0,
                y: 0.0,
                width: 400.0,
                height: 75.0
            }
        );
    }

    #[test]
    fn the_badge_names_the_size_of_the_selection() {
        let scene = build_scene(&selected(Overlay::Screenshot), 1.0);
        assert_eq!(scene.badge_text, "200 × 100");
        let badge = scene.badge.expect("a badge");
        // It sits next to the selection, not on it.
        let overlaps = badge.x < 300.0
            && badge.x + badge.width > 100.0
            && badge.y < 250.0
            && badge.y + badge.height > 150.0;
        assert!(!overlaps, "the badge covers the selection");
    }

    #[test]
    fn keys_map_from_the_slint_encoding() {
        assert_eq!(key_from_text("\u{001b}"), Some(Key::Escape));
        assert_eq!(key_from_text("\u{000a}"), Some(Key::Enter));
        assert_eq!(key_from_text("\u{0009}"), Some(Key::Tab));
        assert_eq!(key_from_text("\u{0008}"), Some(Key::Backspace));
        assert_eq!(key_from_text("\u{007f}"), Some(Key::Delete));
        assert_eq!(key_from_text("\u{F700}"), Some(Key::ArrowUp));
        assert_eq!(key_from_text("z"), Some(Key::Character('Z')));
        // Ctrl+Z on Windows arrives as the control character.
        assert_eq!(key_from_text("\u{001a}"), Some(Key::Character('Z')));
        assert_eq!(key_from_text("ab"), None);
    }
}
