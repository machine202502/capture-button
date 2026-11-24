//! The area-selection overlay: its model and its rules.
//!
//! This module does not draw and does not call the OS. It owns the vocabulary
//! (tools, shapes, buttons, outcomes), the selection geometry, which buttons a
//! given overlay offers and when they are enabled, and the glyph/caption of each
//! button. A renderer asks for a scene and feeds pointer/key events back; the
//! native overlay and the Slint overlay therefore share every rule and look the
//! same by construction.
//!
//! [`Model`] holds the overlay's state and decides layout, hit-testing, text-box
//! geometry, button presses and keys. Its only OS dependency is measuring text
//! ([`Metrics`]); everything else it cannot do itself is returned as an
//! [`Action`] for the host to perform.

use crate::geom::{self, Panel};
use crate::i18n::tr;
use crate::palette::Palette;
use crate::types::{Format, Frame, Rect};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// How the area overlay is drawn.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RendererKind {
    /// The backend draws the window; the application only opens it and takes
    /// the outcome.
    Native,
    /// The application draws the window from `overlay.slint`.
    Slint,
}

/// An area-overlay renderer this build offers.
#[derive(Clone, Copy, Debug)]
pub struct Renderer {
    /// Stable key stored in the settings: 0, 1, 2…
    pub id: u8,
    /// The label the settings picker shows, supplied by the backend.
    pub name: &'static str,
    pub kind: RendererKind,
}

/// The renderers a platform offers, in menu order, and the one a fresh profile
/// starts with.
#[derive(Clone, Copy, Debug)]
pub struct Renderers {
    pub available: &'static [Renderer],
    /// The [`Renderer::id`] of the default renderer.
    pub default: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Select,
    Pen,
    Line,
    Arrow,
    Rectangle,
    Marker,
    Text,
    Color,
    Width,
    Undo,
    Redo,
    Delete,
    Copy,
    Save,
    Record,
    RecordMkv,
    RecordGif,
    RecordWebp,
    Video,
    Cancel,
    Swatch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Selection {
    pub fn new(a: Point, b: Point) -> Self {
        Self {
            x: a.x.min(b.x),
            y: a.y.min(b.y),
            w: (a.x - b.x).abs(),
            h: (a.y - b.y).abs(),
        }
    }

    pub fn right(self) -> i32 {
        self.x + self.w
    }

    pub fn bottom(self) -> i32 {
        self.y + self.h
    }

    pub fn usable(self) -> bool {
        self.w >= 2 && self.h >= 2
    }

    pub fn clamp(self, width: i32, height: i32) -> Self {
        let x0 = self.x.max(0).min(width);
        let y0 = self.y.max(0).min(height);
        let x1 = self.right().max(0).min(width);
        let y1 = self.bottom().max(0).min(height);
        Self {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        }
    }

    pub fn output(self, monitor: Rect) -> Rect {
        Rect {
            x: monitor.x + self.x,
            y: monitor.y + self.y,
            w: self.w as u32,
            h: self.h as u32,
        }
    }

    /// Moves an existing area. Reaching an edge stops it there; it must not be
    /// squeezed against that edge the way a resize would be.
    pub fn nudge(self, dx: i32, dy: i32, width: i32, height: i32) -> Self {
        let max_x = (width - self.w).max(0);
        let max_y = (height - self.h).max(0);
        Self {
            x: (self.x + dx).clamp(0, max_x),
            y: (self.y + dy).clamp(0, max_y),
            ..self
        }
    }

    pub fn panel(self) -> Panel {
        Panel {
            x: self.x,
            y: self.y,
            w: self.w,
            h: self.h,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    Select,
    Pen,
    Line,
    Arrow,
    Rectangle,
    Marker,
    Text,
}

#[derive(Clone)]
pub enum ShapeKind {
    Pen(Vec<Point>),
    Line(Point, Point),
    Arrow(Point, Point),
    Rectangle(Point, Point),
    Marker(Vec<Point>),
    Text(Point, i32, String),
}

#[derive(Clone)]
pub struct Shape {
    pub kind: ShapeKind,
    /// `0x00BBGGRR`, like the palette.
    pub color: u32,
    pub width: i32,
    pub font_size: i32,
    pub font_name: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Handle {
    Move,
    N,
    S,
    E,
    W,
    NW,
    NE,
    SW,
    SE,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Drag {
    Create(Point),
    Edit(Handle, Point, Selection),
    Width(i32, i32),
    TextMove(Point, Point),
    TextSize(Handle, Point, Point, i32, i32),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct EditDrag {
    pub origin: Point,
    pub start_client: Point,
    pub start_screen: (i32, i32),
}

pub struct Button {
    pub role: Role,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub shown: bool,
    pub swatch: Option<usize>,
}

pub enum Outcome {
    Cancel,
    /// A recording of the selection, optionally forcing the picture format.
    Record(Rect, Option<Format>),
    Screen,
    /// An annotated still for the app to save. `None` asks the app to use its
    /// default screenshots path; `Some` is a path chosen in the save dialog.
    Saved(Frame, Option<PathBuf>),
}

/// Which area overlay is open. Video is only offered from the tray.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Overlay {
    /// Capture tab, record the selection. Uses the capture profile.
    Capture,
    /// Capture tab, screenshot the selection. Record uses the capture profile.
    Screenshot,
    /// Tray area. Record uses the tray area profile. Video uses tray video recording.
    Tray,
}

impl Overlay {
    pub fn annotates(self) -> bool {
        matches!(self, Self::Screenshot | Self::Tray)
    }

    /// The full-screen Video button. Screenshot and capture selection do not show it.
    pub fn shows_video(self) -> bool {
        matches!(self, Self::Tray)
    }
}

/// The two overlay fonts a measurement can use: the body face and the glyph face.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontSeed {
    Body,
    Icon,
}

/// The only OS dependency of the layout: how wide a string is in an overlay font.
pub trait Metrics {
    fn text_width(&self, text: &str, font: FontSeed, scale: u32) -> i32;
}

/// Something the model decided but cannot do itself.
pub enum Action {
    None,
    Redraw,
    OpenTextEditor {
        at: Point,
        width: i32,
        height: i32,
        text: String,
    },
    CloseTextEditor,
    /// The host crops, asks for a path and finishes.
    SaveDialog,
    /// The host grabs the next monitor and repositions the window.
    NextMonitor,
    /// Cancel / record / screen / saved.
    Finish(Outcome),
    /// Crop + clipboard.
    Copy,
    /// Crop + save to the default folder.
    Save,
}

/// A key the overlay reacts to. Backends translate their native codes here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Escape,
    Enter,
    Tab,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    ArrowDown,
    Backspace,
    Delete,
    Character(char),
}

/// Everything the area overlay remembers between messages.
pub struct Model {
    pub frame: Frame,
    pub monitor: Rect,
    pub monitors: Vec<Rect>,
    pub current_monitor: usize,
    pub overlay: Overlay,
    pub allow_record: bool,
    pub selection: Option<Selection>,
    pub drag: Option<Drag>,
    pub tool: Tool,
    pub shapes: Vec<Shape>,
    pub redo: Vec<Shape>,
    pub current: Option<Shape>,
    pub color: u32,
    pub width: i32,
    pub font_size: i32,
    pub font_name: String,
    pub buttons: Vec<Button>,
    pub scale: u32,
    pub edit_pos: Point,
    pub edit_w: i32,
    pub edit_h: i32,
    pub editing: bool,
    pub edit_drag: Option<EditDrag>,
    pub palette: Palette,
    pub swatches_open: bool,
    pub outcome: Option<Outcome>,
}

impl Model {
    pub fn new(
        overlay: Overlay,
        allow_record: bool,
        frame: Frame,
        monitor: Rect,
        palette: Palette,
    ) -> Self {
        // Automatic interface scale: the overlay follows the monitor height.
        let scale = crate::geom::interface_scale(0, monitor.h);
        Self {
            frame,
            monitor,
            monitors: vec![monitor],
            current_monitor: 0,
            overlay,
            allow_record,
            selection: None,
            drag: None,
            tool: Tool::Select,
            shapes: Vec::new(),
            redo: Vec::new(),
            current: None,
            color: palette.SWATCHES[0],
            width: 4,
            font_size: 28,
            font_name: "Segoe UI".into(),
            buttons: Vec::new(),
            scale,
            edit_pos: Point { x: 0, y: 0 },
            edit_w: 240,
            edit_h: 80,
            editing: false,
            edit_drag: None,
            palette,
            swatches_open: false,
            outcome: None,
        }
    }

    pub fn selected(&self) -> Option<Selection> {
        self.selection.filter(|selection| selection.usable())
    }

    fn push_shape(&mut self, shape: Shape) {
        self.shapes.push(shape);
        self.redo.clear();
    }

    fn undo(&mut self) {
        if let Some(shape) = self.shapes.pop() {
            self.redo.push(shape);
        }
    }

    fn redo_shape(&mut self) {
        if let Some(shape) = self.redo.pop() {
            self.shapes.push(shape);
        }
    }

    pub fn press(&mut self, index: usize, point: Point, metrics: &dyn Metrics) -> Action {
        let Some(button) = self
            .buttons
            .get(index)
            .map(|button| (button.role, button.swatch))
        else {
            return Action::None;
        };
        if !role_enabled(&self.shapes, &self.redo, self.allow_record, button.0) {
            return Action::None;
        }
        let action = match button.0 {
            Role::Select => {
                self.tool = Tool::Select;
                Action::Redraw
            }
            Role::Pen => {
                self.tool = Tool::Pen;
                Action::Redraw
            }
            Role::Line => {
                self.tool = Tool::Line;
                Action::Redraw
            }
            Role::Arrow => {
                self.tool = Tool::Arrow;
                Action::Redraw
            }
            Role::Rectangle => {
                self.tool = Tool::Rectangle;
                Action::Redraw
            }
            Role::Marker => {
                self.tool = Tool::Marker;
                Action::Redraw
            }
            Role::Text => {
                self.tool = Tool::Text;
                Action::Redraw
            }
            Role::Color => {
                self.swatches_open = !self.swatches_open;
                Action::Redraw
            }
            Role::Swatch => {
                if let Some(swatch) = button.1 {
                    self.color = self.palette.SWATCHES[swatch];
                }
                self.swatches_open = false;
                Action::Redraw
            }
            Role::Width => {
                self.drag = Some(Drag::Width(point.x, self.width));
                Action::Redraw
            }
            Role::Undo => {
                self.undo();
                Action::Redraw
            }
            Role::Redo => {
                self.redo_shape();
                Action::Redraw
            }
            Role::Delete => {
                self.undo();
                Action::Redraw
            }
            Role::Copy => Action::Copy,
            Role::Save => Action::SaveDialog,
            Role::Record => match self.selected() {
                Some(selection) => {
                    Action::Finish(Outcome::Record(selection.output(self.monitor), None))
                }
                None => Action::Redraw,
            },
            Role::RecordMkv => match self.selected() {
                Some(selection) => Action::Finish(Outcome::Record(
                    selection.output(self.monitor),
                    Some(Format::Mkv),
                )),
                None => Action::Redraw,
            },
            Role::RecordGif => match self.selected() {
                Some(selection) => Action::Finish(Outcome::Record(
                    selection.output(self.monitor),
                    Some(Format::Gif),
                )),
                None => Action::Redraw,
            },
            Role::RecordWebp => match self.selected() {
                Some(selection) => Action::Finish(Outcome::Record(
                    selection.output(self.monitor),
                    Some(Format::Webp),
                )),
                None => Action::Redraw,
            },
            Role::Video => Action::Finish(Outcome::Screen),
            Role::Cancel => Action::Finish(Outcome::Cancel),
        };
        self.layout(metrics);
        action
    }

    pub fn select_role(&mut self, role: Role, metrics: &dyn Metrics) -> Action {
        self.tool = match role {
            Role::Pen => Tool::Pen,
            Role::Line => Tool::Line,
            Role::Arrow => Tool::Arrow,
            Role::Rectangle => Tool::Rectangle,
            Role::Marker => Tool::Marker,
            Role::Text => Tool::Text,
            _ => Tool::Select,
        };
        self.layout(metrics);
        Action::Redraw
    }

    pub fn key_down(
        &mut self,
        key: Key,
        control: bool,
        shift: bool,
        metrics: &dyn Metrics,
    ) -> Action {
        if self.editing {
            return Action::None;
        }
        if key == Key::Escape {
            if self.tool != Tool::Select {
                self.tool = Tool::Select;
            } else {
                self.layout(metrics);
                return Action::Finish(Outcome::Cancel);
            }
            self.layout(metrics);
            return Action::None;
        }
        let mut action = Action::Redraw;
        if control && shift && key == Key::Character('Z') {
            self.redo_shape();
        } else if control && key == Key::Character('Z') {
            self.undo();
        } else if control && key == Key::Character('A') {
            self.selection = Some(Selection {
                x: 0,
                y: 0,
                w: self.frame.width as i32,
                h: self.frame.height as i32,
            });
        } else if control && key == Key::Character('C') {
            action = Action::Copy;
        } else if control && key == Key::Character('S') {
            action = Action::SaveDialog;
        } else if key == Key::Delete {
            self.undo();
        } else if key == Key::Tab {
            let next = self.next_monitor();
            if matches!(next, Action::NextMonitor) {
                return next;
            }
        } else if key == Key::Enter {
            if self.overlay.annotates() {
                action = Action::Save;
            } else if self.allow_record {
                if let Some(selection) = self.selected() {
                    action = Action::Finish(Outcome::Record(selection.output(self.monitor), None));
                }
            }
        } else if let Some(mut selection) = self.selected() {
            let (dx, dy) = match key {
                Key::ArrowLeft => (-1, 0),
                Key::ArrowRight => (1, 0),
                Key::ArrowUp => (0, -1),
                Key::ArrowDown => (0, 1),
                _ => (0, 0),
            };
            if dx != 0 || dy != 0 {
                if shift {
                    selection.w = (selection.w + dx).max(2);
                    selection.h = (selection.h + dy).max(2);
                    selection = selection.clamp(self.frame.width as i32, self.frame.height as i32);
                } else {
                    selection =
                        selection.nudge(dx, dy, self.frame.width as i32, self.frame.height as i32);
                }
                self.selection = Some(selection);
            }
        }
        self.layout(metrics);
        action
    }

    /// Advances to the next monitor. The host grabs it and moves the window.
    pub fn next_monitor(&mut self) -> Action {
        if self.monitors.len() < 2 {
            return Action::None;
        }
        self.current_monitor = (self.current_monitor + 1) % self.monitors.len();
        self.monitor = self.monitors[self.current_monitor];
        Action::NextMonitor
    }

    pub fn hit_button(&self, point: Point) -> Option<usize> {
        self.buttons
            .iter()
            .enumerate()
            .rev()
            .find(|(_, button)| {
                button.shown
                    && point.x >= button.x
                    && point.x < button.x + button.w
                    && point.y >= button.y
                    && point.y < button.y + button.h
            })
            .map(|(index, _)| index)
    }

    fn text_pad(&self) -> i32 {
        (8 * self.scale as i32 / 100).max(6)
    }

    pub fn text_outer(&self) -> Selection {
        let pad = self.text_pad();
        Selection {
            x: self.edit_pos.x - pad,
            y: self.edit_pos.y - pad,
            w: self.edit_w + pad * 2,
            h: self.edit_h + pad * 2,
        }
    }

    pub fn hit_text_handle(&self, point: Point) -> Option<Handle> {
        if !self.editing {
            return None;
        }
        text_handles(self.text_outer())
            .into_iter()
            .find(|(_, x, y)| (point.x - x).abs() <= 8 && (point.y - y).abs() <= 8)
            .map(|(handle, _, _)| handle)
    }

    /// The handle the open edit box's own edges grab.
    ///
    /// The native overlay's edit control checks this before it moves the caret
    /// (`edit_edge_handle` in the GDI overlay), so any press in the eight-pixel
    /// band inside the box resizes it, not only a press on one of the painted
    /// dots. The padded frame outside the box is [`Self::hit_text_handle`] and
    /// [`Self::in_text_border`] instead.
    pub fn hit_edit_edge_handle(&self, point: Point) -> Option<Handle> {
        if !self.editing || !self.inside_edit(point) {
            return None;
        }
        let near = |value: i32, edge: i32| (value - edge).abs() <= 8;
        let left = near(point.x, self.edit_pos.x);
        let right = near(point.x, self.edit_pos.x + self.edit_w);
        let top = near(point.y, self.edit_pos.y);
        let bottom = near(point.y, self.edit_pos.y + self.edit_h);
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

    pub fn over_text_box(&self, point: Point) -> bool {
        if !self.editing {
            return false;
        }
        let outer = self.text_outer();
        point.x >= outer.x
            && point.x < outer.right()
            && point.y >= outer.y
            && point.y < outer.bottom()
    }

    pub fn begin_text_move(&mut self, point: Point) {
        self.drag = Some(Drag::TextMove(self.edit_pos, point));
    }

    pub fn begin_text_size(&mut self, handle: Handle, point: Point) {
        self.drag = Some(Drag::TextSize(
            handle,
            point,
            self.edit_pos,
            self.edit_w,
            self.edit_h,
        ));
    }

    pub fn inside_edit(&self, point: Point) -> bool {
        point.x >= self.edit_pos.x
            && point.x < self.edit_pos.x + self.edit_w
            && point.y >= self.edit_pos.y
            && point.y < self.edit_pos.y + self.edit_h
    }

    pub fn in_text_border(&self, point: Point) -> bool {
        let outer = self.text_outer();
        let inside_outer = point.x >= outer.x
            && point.x < outer.right()
            && point.y >= outer.y
            && point.y < outer.bottom();
        let inside_edit = point.x >= self.edit_pos.x
            && point.x < self.edit_pos.x + self.edit_w
            && point.y >= self.edit_pos.y
            && point.y < self.edit_pos.y + self.edit_h;
        inside_outer && !inside_edit
    }

    /// Keeps the text box on screen. The host moves the editor window after this.
    pub fn clamp_edit(&mut self) {
        self.edit_pos.x = self
            .edit_pos
            .x
            .clamp(0, (self.frame.width as i32 - self.edit_w).max(0));
        self.edit_pos.y = self
            .edit_pos
            .y
            .clamp(0, (self.frame.height as i32 - self.edit_h).max(0));
    }

    fn resize_text(
        &mut self,
        handle: Handle,
        origin: Point,
        start: Point,
        width: i32,
        height: i32,
        point: Point,
    ) {
        let dx = point.x - origin.x;
        let dy = point.y - origin.y;
        let mut left = start.x;
        let mut top = start.y;
        let mut right = start.x + width;
        let mut bottom = start.y + height;
        if matches!(handle, Handle::W | Handle::NW | Handle::SW) {
            left += dx;
        }
        if matches!(handle, Handle::E | Handle::NE | Handle::SE) {
            right += dx;
        }
        if matches!(handle, Handle::N | Handle::NW | Handle::NE) {
            top += dy;
        }
        if matches!(handle, Handle::S | Handle::SW | Handle::SE) {
            bottom += dy;
        }
        if right - left < 96 {
            if matches!(handle, Handle::W | Handle::NW | Handle::SW) {
                left = right - 96;
            } else {
                right = left + 96;
            }
        }
        if bottom - top < 40 {
            if matches!(handle, Handle::N | Handle::NW | Handle::NE) {
                top = bottom - 40;
            } else {
                bottom = top + 40;
            }
        }
        self.edit_pos = Point { x: left, y: top };
        self.edit_w = right - left;
        self.edit_h = bottom - top;
    }

    /// The pointer pressed on the overlay. The host has already handed a press
    /// that belongs to the text editor to the editor itself, and has committed
    /// the text when the press landed outside the box.
    pub fn mouse_down(
        &mut self,
        point: Point,
        double: bool,
        was_editing: bool,
        metrics: &dyn Metrics,
    ) -> Action {
        if let Some(index) = self.hit_button(point) {
            return self.press(index, point, metrics);
        }
        if was_editing {
            return Action::None;
        }
        if double && self.tool == Tool::Select {
            self.selection = Some(Selection {
                x: 0,
                y: 0,
                w: self.frame.width as i32,
                h: self.frame.height as i32,
            });
            self.layout(metrics);
            return Action::Redraw;
        }
        match self.tool {
            Tool::Select => {
                self.drag = Some(
                    match self
                        .selection
                        .and_then(|selection| hit_test(selection, point))
                    {
                        Some(handle) => Drag::Edit(handle, point, self.selection.unwrap()),
                        None => {
                            self.selection = Some(Selection {
                                x: point.x,
                                y: point.y,
                                w: 0,
                                h: 0,
                            });
                            Drag::Create(point)
                        }
                    },
                );
            }
            Tool::Text => return self.start_text(point),
            tool => {
                let kind = match tool {
                    Tool::Pen => ShapeKind::Pen(vec![point]),
                    Tool::Line => ShapeKind::Line(point, point),
                    Tool::Arrow => ShapeKind::Arrow(point, point),
                    Tool::Rectangle => ShapeKind::Rectangle(point, point),
                    Tool::Marker => ShapeKind::Marker(vec![point]),
                    _ => return Action::None,
                };
                self.current = Some(Shape {
                    kind,
                    color: self.color,
                    width: self.width,
                    font_size: self.font_size,
                    font_name: self.font_name.clone(),
                });
            }
        }
        self.layout(metrics);
        Action::Redraw
    }

    pub fn mouse_move(&mut self, point: Point) -> Action {
        let point = Point {
            x: point.x.clamp(0, self.frame.width as i32),
            y: point.y.clamp(0, self.frame.height as i32),
        };
        if let Some(Drag::Width(origin, start)) = self.drag {
            self.width = (start + (point.x - origin) / 8).clamp(1, 16);
            return Action::Redraw;
        }
        if let Some(Drag::TextMove(origin, start)) = self.drag {
            self.edit_pos = Point {
                x: origin.x + point.x - start.x,
                y: origin.y + point.y - start.y,
            };
            return Action::Redraw;
        }
        if let Some(Drag::TextSize(handle, origin, start, width, height)) = self.drag {
            self.resize_text(handle, origin, start, width, height, point);
            return Action::Redraw;
        }
        if let Some(drag) = self.drag {
            match drag {
                Drag::Create(anchor) => self.selection = Some(Selection::new(anchor, point)),
                Drag::Edit(handle, origin, start) => {
                    self.selection = Some(
                        edit_selection(start, handle, origin, point)
                            .clamp(self.frame.width as i32, self.frame.height as i32),
                    )
                }
                Drag::Width(_, _) | Drag::TextMove(_, _) | Drag::TextSize(_, _, _, _, _) => {}
            }
        } else if let Some(shape) = &mut self.current {
            match &mut shape.kind {
                ShapeKind::Pen(points) | ShapeKind::Marker(points) => points.push(point),
                ShapeKind::Line(_, end)
                | ShapeKind::Arrow(_, end)
                | ShapeKind::Rectangle(_, end) => *end = point,
                ShapeKind::Text(_, _, _) => {}
            }
        } else {
            return Action::None;
        }
        Action::Redraw
    }

    pub fn mouse_up(&mut self, point: Point, metrics: &dyn Metrics) -> Action {
        if matches!(
            self.drag,
            Some(Drag::TextMove(_, _)) | Some(Drag::TextSize(_, _, _, _, _))
        ) {
            self.drag = None;
            return Action::Redraw;
        }
        if let Some(Drag::Width(origin, start)) = self.drag.take() {
            if (point.x - origin).abs() < 4 {
                self.width = match start {
                    0..=2 => 4,
                    3..=4 => 8,
                    5..=8 => 12,
                    _ => 2,
                };
            }
            self.layout(metrics);
            return Action::Redraw;
        }
        self.drag = None;
        if self.selection.is_some_and(|selection| !selection.usable()) {
            self.selection = None;
        }
        if let Some(shape) = self.current.take() {
            self.push_shape(shape);
        }
        self.layout(metrics);
        Action::Redraw
    }

    pub fn start_text(&mut self, point: Point) -> Action {
        self.edit_pos = point;
        self.edit_w = 240;
        self.edit_h = 80;
        self.editing = true;
        Action::OpenTextEditor {
            at: point,
            width: 240,
            height: 80,
            text: String::new(),
        }
    }

    /// Takes the text the host read from the editor and records it as a shape.
    pub fn commit_text(
        &mut self,
        text: String,
        origin: Point,
        wrap: i32,
        metrics: &dyn Metrics,
    ) -> Action {
        self.editing = false;
        if !text.trim().is_empty() {
            self.push_shape(Shape {
                kind: ShapeKind::Text(origin, wrap, text),
                color: self.color,
                width: self.width,
                font_size: self.font_size.max(18),
                font_name: self.font_name.clone(),
            });
        }
        self.layout(metrics);
        Action::CloseTextEditor
    }

    pub fn cancel_text(&mut self, metrics: &dyn Metrics) -> Action {
        self.editing = false;
        self.layout(metrics);
        Action::CloseTextEditor
    }

    pub fn layout(&mut self, metrics: &dyn Metrics) {
        self.buttons.clear();
        let scale = |value: i32| value * self.scale as i32 / 100;
        let screen = Panel {
            x: 0,
            y: 0,
            w: self.frame.width as i32,
            h: self.frame.height as i32,
        };
        let shown_cancel = !self.editing && self.drag.is_none() && self.current.is_none();
        let Some(selection) = self.selected() else {
            let roles = opening_roles(self.overlay);
            let height = scale(32);
            let gap = scale(6);
            let widths: Vec<i32> = roles
                .iter()
                .map(|role| action_width(metrics, self.scale, *role))
                .collect();
            let total = widths.iter().sum::<i32>() + gap * (widths.len() as i32 - 1).max(0);
            let mut x = screen.w / 2 - total / 2;
            let y = screen.h / 2 + scale(48);
            for (role, width) in roles.into_iter().zip(widths) {
                self.buttons.push(Button {
                    role,
                    x,
                    y,
                    w: width,
                    h: height,
                    shown: shown_cancel,
                    swatch: None,
                });
                x += width + gap;
            }
            return;
        };
        let shown = shown_cancel;
        let tools = if self.overlay.annotates() {
            vec![
                Role::Select,
                Role::Pen,
                Role::Line,
                Role::Arrow,
                Role::Rectangle,
                Role::Marker,
                Role::Text,
                Role::Color,
                Role::Width,
                Role::Undo,
                Role::Redo,
                Role::Delete,
            ]
        } else {
            Vec::new()
        };
        let actions = action_roles(self.overlay);
        let gap = scale(2);
        let side = scale(34);
        let action_gap = scale(6);
        let action_h = scale(32);
        let margin = scale(8);
        let extra = if self.swatches_open { 8 } else { 0 };
        let stride = side + gap;
        let rows_fit = ((screen.h - margin * 2 + gap) / stride).max(1);
        let count = tools.len() as i32 + extra;
        let columns = ((count + rows_fit - 1) / rows_fit).max(1);
        let rows = ((count + columns - 1) / columns).max(1);
        let tools_w = columns * side + (columns - 1) * gap;
        let tools_h = rows * side + (rows - 1) * gap;
        let action_widths: Vec<i32> = actions
            .iter()
            .copied()
            .map(|role| action_width(metrics, self.scale, role))
            .collect();
        let max_row =
            (screen.w - margin * 2).max(action_widths.iter().copied().max().unwrap_or(side));
        let mut action_rows: Vec<Vec<(Role, i32)>> = Vec::new();
        let mut row: Vec<(Role, i32)> = Vec::new();
        let mut row_w = 0;
        for (role, width) in actions.into_iter().zip(action_widths) {
            let next = if row.is_empty() {
                width
            } else {
                row_w + action_gap + width
            };
            if next > max_row && !row.is_empty() {
                action_rows.push(std::mem::take(&mut row));
                row_w = 0;
            }
            row_w = if row.is_empty() {
                width
            } else {
                row_w + action_gap + width
            };
            row.push((role, width));
        }
        if !row.is_empty() {
            action_rows.push(row);
        }
        let actions_w = action_rows
            .iter()
            .map(|row| {
                row.iter().map(|(_, width)| *width).sum::<i32>()
                    + action_gap * (row.len() as i32 - 1).max(0)
            })
            .max()
            .unwrap_or(0);
        let actions_h = action_rows.len() as i32 * action_h
            + action_gap * (action_rows.len() as i32 - 1).max(0);
        let (tools_rect, actions_rect) = geom::place_panels(
            screen,
            selection.panel(),
            self.overlay
                .annotates()
                .then_some((tools_w, tools_h.max(side))),
            (actions_w, actions_h),
            margin,
        );
        if let Some(rect) = tools_rect {
            let mut index = 0i32;
            for role in tools {
                let column = index / rows;
                let row_index = index % rows;
                self.buttons.push(Button {
                    role,
                    x: rect.x + column * stride,
                    y: rect.y + row_index * stride,
                    w: side,
                    h: side,
                    shown,
                    swatch: None,
                });
                index += 1;
            }
            if self.swatches_open {
                for swatch in 0..8 {
                    let column = index / rows;
                    let row_index = index % rows;
                    self.buttons.push(Button {
                        role: Role::Swatch,
                        x: rect.x + column * stride,
                        y: rect.y + row_index * stride,
                        w: side,
                        h: side,
                        shown,
                        swatch: Some(swatch as usize),
                    });
                    index += 1;
                }
            }
        }
        for (row_index, row) in action_rows.into_iter().enumerate() {
            let mut x = actions_rect.x;
            let y = actions_rect.y + row_index as i32 * (action_h + action_gap);
            for (role, width) in row {
                self.buttons.push(Button {
                    role,
                    x,
                    y,
                    w: width,
                    h: action_h,
                    shown,
                    swatch: None,
                });
                x += width + action_gap;
            }
        }
    }
}

/// Which resize handle (or the middle) a point on a selection hits.
pub fn hit_test(selection: Selection, point: Point) -> Option<Handle> {
    if !selection.usable() {
        return None;
    }
    let near = |a: i32, b: i32| (a - b).abs() <= 8;
    if point.x < selection.x - 8
        || point.x > selection.right() + 8
        || point.y < selection.y - 8
        || point.y > selection.bottom() + 8
    {
        return None;
    }
    let left = near(point.x, selection.x);
    let right = near(point.x, selection.right());
    let top = near(point.y, selection.y);
    let bottom = near(point.y, selection.bottom());
    Some(match (left, right, top, bottom) {
        (true, _, true, _) => Handle::NW,
        (_, true, true, _) => Handle::NE,
        (true, _, _, true) => Handle::SW,
        (_, true, _, true) => Handle::SE,
        (true, _, _, _) => Handle::W,
        (_, true, _, _) => Handle::E,
        (_, _, true, _) => Handle::N,
        (_, _, _, true) => Handle::S,
        _ if point.x >= selection.x
            && point.x < selection.right()
            && point.y >= selection.y
            && point.y < selection.bottom() =>
        {
            Handle::Move
        }
        _ => return None,
    })
}

/// A selection resized by one of its handles.
pub fn edit_selection(start: Selection, handle: Handle, origin: Point, point: Point) -> Selection {
    let dx = point.x - origin.x;
    let dy = point.y - origin.y;
    if matches!(handle, Handle::Move) {
        return Selection {
            x: start.x + dx,
            y: start.y + dy,
            ..start
        };
    }
    let mut left = start.x;
    let mut right = start.right();
    let mut top = start.y;
    let mut bottom = start.bottom();
    if matches!(handle, Handle::W | Handle::NW | Handle::SW) {
        left += dx;
    }
    if matches!(handle, Handle::E | Handle::NE | Handle::SE) {
        right += dx;
    }
    if matches!(handle, Handle::N | Handle::NW | Handle::NE) {
        top += dy;
    }
    if matches!(handle, Handle::S | Handle::SW | Handle::SE) {
        bottom += dy;
    }
    Selection::new(
        Point { x: left, y: top },
        Point {
            x: right,
            y: bottom,
        },
    )
}

/// Where the text of a committed text box starts, and how wide it wraps.
///
/// The formatting rectangle does not include the control's left and right margins, so committing
/// at the window corner draws the text a margin-width to the left of where it was typed.
pub fn text_commit_box(
    edit_pos: Point,
    edit_w: i32,
    format_left: i32,
    format_top: i32,
    format_right: i32,
    margins: i32,
) -> (Point, i32) {
    let left_margin = margins & 0xffff;
    let right_margin = (margins as u32 >> 16) as i32;
    let left = format_left.max(left_margin);
    let right_limit = if right_margin > 0 {
        edit_w - right_margin
    } else {
        format_right
    };
    let right = format_right.min(right_limit).max(left + 1);
    (
        Point {
            x: edit_pos.x + left,
            y: edit_pos.y + format_top,
        },
        right - left,
    )
}

/// The padding, glyph width, gap and caption width of an action button.
pub fn action_metrics(metrics: &dyn Metrics, scale: u32, role: Role) -> (i32, i32, i32, i32) {
    let pad = (8 * scale as i32 / 100).max(6);
    let gap = (8 * scale as i32 / 100).max(6);
    let Some(glyph) = icon(role) else {
        return (pad, 0, 0, 0);
    };
    let icon_w = metrics
        .text_width(glyph, FontSeed::Icon, scale)
        .max((16 * scale as i32 / 100).max(14));
    let (gap, text_w) = match caption(role) {
        Some(caption) => (gap, metrics.text_width(&caption, FontSeed::Body, scale)),
        None => (0, 0),
    };
    (pad, icon_w, gap, text_w)
}

pub fn action_width(metrics: &dyn Metrics, scale: u32, role: Role) -> i32 {
    let (pad, icon_w, gap, text_w) = action_metrics(metrics, scale, role);
    pad + icon_w + gap + text_w + pad
}

/// The eight resize handles of a selection, in the order the renderers draw them.
pub fn handles(selection: Selection) -> [(i32, i32); 8] {
    [
        (selection.x, selection.y),
        (selection.right(), selection.y),
        (selection.x, selection.bottom()),
        (selection.right(), selection.bottom()),
        (selection.x + selection.w / 2, selection.y),
        (selection.right(), selection.y + selection.h / 2),
        (selection.x + selection.w / 2, selection.bottom()),
        (selection.x, selection.y + selection.h / 2),
    ]
}

/// The eight resize handles of an open text box, with the edge each one drags.
///
/// The frame is the text box grown by the padding ([`Model::text_outer`]), so
/// the handles sit on the visible border: the renderer draws them there and
/// [`Model::hit_text_handle`] presses them there.
pub fn text_handles(frame: Selection) -> [(Handle, i32, i32); 8] {
    [
        (Handle::NW, frame.x, frame.y),
        (Handle::NE, frame.right(), frame.y),
        (Handle::SW, frame.x, frame.bottom()),
        (Handle::SE, frame.right(), frame.bottom()),
        (Handle::N, frame.x + frame.w / 2, frame.y),
        (Handle::S, frame.x + frame.w / 2, frame.bottom()),
        (Handle::W, frame.x, frame.y + frame.h / 2),
        (Handle::E, frame.right(), frame.y + frame.h / 2),
    ]
}

/// Whether a button does something right now: undo needs a shape, redo needs an
/// undone shape, and recording needs a source that allows it.
pub fn role_enabled(shapes: &[Shape], redo: &[Shape], allow_record: bool, role: Role) -> bool {
    match role {
        Role::Undo | Role::Delete => !shapes.is_empty(),
        Role::Redo => !redo.is_empty(),
        Role::Record | Role::RecordMkv | Role::RecordGif | Role::RecordWebp | Role::Video => {
            allow_record
        }
        _ => true,
    }
}

/// The buttons shown before any selection exists. The tray area offers the
/// full-screen Video recording; the capture-tab overlays only offer Cancel until
/// an area is selected.
pub fn opening_roles(overlay: Overlay) -> Vec<Role> {
    let mut roles = Vec::new();
    if overlay.shows_video() {
        roles.push(Role::Video);
    }
    roles.push(Role::Cancel);
    roles
}

/// The actions offered once a selection exists, in menu order.
pub fn action_roles(overlay: Overlay) -> Vec<Role> {
    if overlay.shows_video() {
        vec![
            Role::RecordMkv,
            Role::RecordGif,
            Role::RecordWebp,
            Role::Video,
            Role::Cancel,
            Role::Copy,
            Role::Save,
        ]
    } else {
        let mut roles = Vec::new();
        if overlay.annotates() {
            roles.extend([Role::Copy, Role::Save]);
        }
        if overlay == Overlay::Capture {
            roles.push(Role::Record);
        }
        roles.push(Role::Cancel);
        roles
    }
}
/// Phosphor Regular, the same glyphs the overlay and the application use.
pub fn icon(role: Role) -> Option<&'static str> {
    Some(match role {
        Role::Select => "\u{E1DC}",
        Role::Pen => "\u{E3B4}",
        Role::Line => "\u{E6D2}",
        Role::Arrow => "\u{E092}",
        Role::Rectangle => "\u{E3F0}",
        Role::Marker => "\u{EC76}",
        Role::Text => "\u{E48A}",
        Role::Undo => "\u{E038}",
        Role::Redo => "\u{E036}",
        Role::Delete => "\u{E4A6}",
        Role::Copy => "\u{E1CA}",
        Role::Save => "\u{E248}",
        Role::Record => "\u{E69A}",
        Role::RecordMkv => "\u{E792}",
        Role::RecordGif => "\u{E274}",
        Role::RecordWebp => "\u{E2CA}",
        Role::Video => "\u{E4DA}",
        Role::Cancel => "\u{E4F6}",
        _ => return None,
    })
}

pub fn caption(role: Role) -> Option<String> {
    match role {
        Role::Record => Some(tr("Record")),
        _ => None,
    }
}

/// The selected pixels, before any annotation is drawn on them.
///
/// Both renderers copy the area the same way, so the picture a screenshot
/// produces does not depend on which overlay the user saw.
pub fn crop_pixels(model: &Model) -> Option<Frame> {
    let selection = model.selected()?;
    let frame = &model.frame;
    if frame.width == 0 || frame.height == 0 || selection.w <= 0 || selection.h <= 0 {
        return None;
    }
    let left = selection.x.max(0) as usize;
    let top = selection.y.max(0) as usize;
    let width = (selection.w as usize).min(frame.width as usize - left.min(frame.width as usize));
    let height = (selection.h as usize).min(frame.height as usize - top.min(frame.height as usize));
    if width == 0 || height == 0 {
        return None;
    }
    let mut data = vec![0u8; width * height * 4];
    for y in 0..height {
        let source = ((top + y) * frame.width as usize + left) * 4;
        let target = y * width * 4;
        let end = source + width * 4;
        if end <= frame.data.len() {
            data[target..target + width * 4].copy_from_slice(&frame.data[source..end]);
        }
    }
    Some(Frame {
        width: width as u32,
        height: height as u32,
        data,
        age_ns: 0,
    })
}

/// One overlay outcome waiting for the application.
///
/// The native overlay keeps its outcome in its own window state; a renderer
/// without such state (the Slint overlay) publishes here instead, and the app
/// takes it through the same path. Only one outcome is kept: a second click,
/// Alt+F4 or an auto-repeated key must not queue a second save.
static OUTCOME: Mutex<Option<Outcome>> = Mutex::new(None);

/// Whether a renderer that reports through this channel is still open.
///
/// The app keeps the native overlay alive after its outcome is ready, because
/// its `active` flag is the pointer itself. A renderer with no window state
/// reports its own liveness here so the app's `active` check keeps working.
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Stores an outcome for the app. False when one is already waiting.
pub fn publish(outcome: Outcome) -> bool {
    let mut slot = OUTCOME.lock().unwrap_or_else(|error| error.into_inner());
    if slot.is_some() {
        return false;
    }
    *slot = Some(outcome);
    true
}

/// Takes the waiting outcome, if any.
pub fn take_outcome() -> Option<Outcome> {
    OUTCOME
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take()
}

/// Whether an outcome is waiting for the app.
pub fn pending() -> bool {
    OUTCOME
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .is_some()
}

/// Records whether a reporter-backed renderer is open.
pub fn set_active(active: bool) {
    ACTIVE.store(active, Ordering::SeqCst);
}

/// Whether a reporter-backed renderer is open.
pub fn reported_active() -> bool {
    ACTIVE.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape() -> Shape {
        Shape {
            kind: ShapeKind::Line(Point { x: 0, y: 0 }, Point { x: 10, y: 10 }),
            color: 0x00ffffff,
            width: 2,
            font_size: 16,
            font_name: "Segoe UI".into(),
        }
    }

    /// A measurer that needs no OS: seven pixels per character.
    struct Fixed;

    impl Metrics for Fixed {
        fn text_width(&self, text: &str, _font: FontSeed, _scale: u32) -> i32 {
            text.chars().count() as i32 * 7
        }
    }

    fn model(overlay: Overlay) -> Model {
        Model::new(
            overlay,
            false,
            Frame {
                width: 800,
                height: 600,
                data: Vec::new(),
                age_ns: 0,
            },
            Rect {
                x: 0,
                y: 0,
                w: 800,
                h: 600,
            },
            crate::palette::DEFAULT,
        )
    }

    #[test]
    fn a_selection_never_goes_negative_or_past_the_screen() {
        let dragged = Selection::new(Point { x: 50, y: 70 }, Point { x: 10, y: 30 });
        assert_eq!(
            (dragged.x, dragged.y, dragged.w, dragged.h),
            (10, 30, 40, 40)
        );
        let clamped = dragged.clamp(30, 40);
        assert_eq!(
            (clamped.x, clamped.y, clamped.w, clamped.h),
            (10, 30, 20, 10)
        );
        assert!(!Selection {
            x: 0,
            y: 0,
            w: 1,
            h: 1
        }
        .usable());
    }

    #[test]
    fn nudging_stops_at_the_edge_instead_of_shrinking() {
        let selection = Selection {
            x: 10,
            y: 10,
            w: 40,
            h: 40,
        };
        let moved = selection.nudge(-100, 0, 100, 100);
        assert_eq!((moved.x, moved.w), (0, 40));
        let moved = selection.nudge(100, 100, 100, 100);
        assert_eq!((moved.x, moved.y, moved.w, moved.h), (60, 60, 40, 40));
    }

    #[test]
    fn handles_sit_on_the_corners_and_edges() {
        let selection = Selection {
            x: 0,
            y: 0,
            w: 100,
            h: 50,
        };
        let list = handles(selection);
        assert_eq!(list[0], (0, 0));
        assert_eq!(list[1], (100, 0));
        assert_eq!(list[2], (0, 50));
        assert_eq!(list[3], (100, 50));
        assert_eq!(list[4], (50, 0));
        assert_eq!(list[6], (50, 50));
    }

    #[test]
    fn undo_redo_and_recording_follow_the_state() {
        let none: Vec<Shape> = Vec::new();
        let one = vec![shape()];
        assert!(!role_enabled(&none, &none, false, Role::Undo));
        assert!(role_enabled(&one, &none, false, Role::Undo));
        assert!(!role_enabled(&none, &none, false, Role::Redo));
        assert!(role_enabled(&none, &one, false, Role::Redo));
        assert!(!role_enabled(&none, &none, false, Role::Record));
        assert!(role_enabled(&none, &none, true, Role::RecordMkv));
        assert!(role_enabled(&none, &none, false, Role::Copy));
    }

    #[test]
    fn a_screenshot_offers_annotations_and_no_recording() {
        let roles = action_roles(Overlay::Screenshot);
        assert!(roles.contains(&Role::Copy));
        assert!(roles.contains(&Role::Save));
        assert!(!roles.contains(&Role::Record));
        assert_eq!(roles.last(), Some(&Role::Cancel));
        assert!(action_roles(Overlay::Capture).contains(&Role::Record));
        assert!(!action_roles(Overlay::Capture).contains(&Role::Copy));
        assert!(action_roles(Overlay::Tray).contains(&Role::Video));
        assert!(action_roles(Overlay::Tray).contains(&Role::RecordGif));
        assert_eq!(action_roles(Overlay::Tray).last(), Some(&Role::Save));
    }

    #[test]
    fn opening_roles_are_video_and_cancel_on_the_tray() {
        assert_eq!(
            opening_roles(Overlay::Tray),
            vec![Role::Video, Role::Cancel]
        );
        assert_eq!(opening_roles(Overlay::Capture), vec![Role::Cancel]);
        assert_eq!(opening_roles(Overlay::Screenshot), vec![Role::Cancel]);
    }
    #[test]
    fn every_action_has_a_glyph() {
        for role in action_roles(Overlay::Tray) {
            assert!(icon(role).is_some(), "{role:?} has no glyph");
        }
        assert!(caption(Role::Record).is_some());
        assert_eq!(caption(Role::Save), None);
    }

    #[test]
    fn record_is_plain_in_the_ui_and_split_by_format_in_the_tray() {
        assert_eq!(
            action_roles(Overlay::Capture),
            vec![Role::Record, Role::Cancel]
        );
        assert_eq!(
            action_roles(Overlay::Screenshot),
            vec![Role::Copy, Role::Save, Role::Cancel]
        );
        assert!(!Overlay::Capture.shows_video());
        assert!(!Overlay::Screenshot.shows_video());
        assert_eq!(
            action_roles(Overlay::Tray),
            vec![
                Role::RecordMkv,
                Role::RecordGif,
                Role::RecordWebp,
                Role::Video,
                Role::Cancel,
                Role::Copy,
                Role::Save
            ]
        );
        assert!(Overlay::Tray.shows_video());
        assert_eq!(
            opening_roles(Overlay::Tray),
            vec![Role::Video, Role::Cancel]
        );
        assert_eq!(opening_roles(Overlay::Capture), vec![Role::Cancel]);
        assert_eq!(opening_roles(Overlay::Screenshot), vec![Role::Cancel]);
    }

    #[test]
    fn utility_buttons_are_icon_only() {
        for role in [
            Role::Copy,
            Role::Save,
            Role::Video,
            Role::Cancel,
            Role::RecordMkv,
            Role::RecordGif,
            Role::RecordWebp,
        ] {
            assert!(caption(role).is_none(), "{role:?} still has a caption");
            assert!(icon(role).is_some(), "{role:?} lost its icon");
        }
        assert!(caption(Role::Record).is_some());
        assert_ne!(icon(Role::RecordMkv), icon(Role::RecordGif));
        assert_ne!(icon(Role::RecordGif), icon(Role::RecordWebp));
    }

    #[test]
    fn the_opening_button_sits_in_the_middle_under_the_hint() {
        let mut model = model(Overlay::Screenshot);
        model.layout(&Fixed);
        assert_eq!(model.buttons.len(), 1);
        let button = &model.buttons[0];
        assert_eq!(button.role, Role::Cancel);
        assert!(button.shown);
        let width = action_width(&Fixed, 100, Role::Cancel);
        assert_eq!(button.w, width);
        assert_eq!(button.h, 32);
        assert_eq!(button.x, 800 / 2 - width / 2);
        assert_eq!(button.y, 600 / 2 + 48);
    }

    #[test]
    fn a_selection_lays_tools_and_actions_out_off_the_area() {
        let mut model = model(Overlay::Screenshot);
        model.selection = Some(Selection {
            x: 200,
            y: 150,
            w: 300,
            h: 200,
        });
        model.layout(&Fixed);
        let pen = model
            .buttons
            .iter()
            .find(|button| button.role == Role::Pen)
            .expect("pen tool");
        assert!(pen.x + pen.w <= 200 || pen.x >= 500, "pen at {}", pen.x);
        let copy = model
            .buttons
            .iter()
            .find(|button| button.role == Role::Copy)
            .expect("copy action");
        assert!(
            copy.y + copy.h <= 150 || copy.y >= 350,
            "copy at {}",
            copy.y
        );
        assert!(model.buttons.iter().all(|button| button.shown));
    }

    #[test]
    fn hit_test_finds_the_handles_the_middle_and_nothing_far_away() {
        let selection = Selection {
            x: 100,
            y: 100,
            w: 200,
            h: 100,
        };
        assert_eq!(
            hit_test(selection, Point { x: 100, y: 100 }),
            Some(Handle::NW)
        );
        assert_eq!(
            hit_test(selection, Point { x: 300, y: 200 }),
            Some(Handle::SE)
        );
        assert_eq!(
            hit_test(selection, Point { x: 100, y: 150 }),
            Some(Handle::W)
        );
        assert_eq!(
            hit_test(selection, Point { x: 200, y: 150 }),
            Some(Handle::Move)
        );
        assert_eq!(hit_test(selection, Point { x: 400, y: 150 }), None);
        assert_eq!(
            hit_test(
                Selection {
                    x: 0,
                    y: 0,
                    w: 1,
                    h: 1
                },
                Point { x: 0, y: 0 }
            ),
            None
        );
    }

    #[test]
    fn committed_text_starts_inside_the_edit_margins() {
        let (origin, wrap) = text_commit_box(Point { x: 10, y: 20 }, 240, 0, 4, 240, (3 << 16) | 2);
        assert_eq!(origin, Point { x: 12, y: 24 });
        assert_eq!(wrap, 235);
        let (origin, wrap) = text_commit_box(Point { x: 0, y: 0 }, 100, 10, 6, 60, 0);
        assert_eq!(origin, Point { x: 10, y: 6 });
        assert_eq!(wrap, 50);
    }

    #[test]
    fn an_open_text_box_presses_and_draws_the_same_handles() {
        let mut model = model(Overlay::Screenshot);
        model.edit_pos = Point { x: 100, y: 100 };
        model.edit_w = 200;
        model.edit_h = 100;
        model.editing = true;
        let outer = model.text_outer();
        // The hit test and the renderer share the spots: corner handles, edge middles.
        for (handle, x, y) in text_handles(outer) {
            assert_eq!(model.hit_text_handle(Point { x, y }), Some(handle));
        }
        assert_eq!(
            model.hit_text_handle(Point {
                x: outer.x + 20,
                y: outer.y
            }),
            None
        );
        // The border between the handles moves the box; the inside belongs to the editor.
        assert!(model.in_text_border(Point {
            x: outer.x + 20,
            y: outer.y
        }));
        assert!(!model.in_text_border(Point {
            x: model.edit_pos.x + 20,
            y: model.edit_pos.y + 20
        }));
    }

    #[test]
    fn the_edit_box_edges_grab_a_resize_like_the_native_edit_control() {
        let mut model = model(Overlay::Screenshot);
        model.edit_pos = Point { x: 100, y: 100 };
        model.edit_w = 200;
        model.edit_h = 100;
        model.editing = true;
        // The whole band inside each edge resizes, not only the painted dots.
        assert_eq!(
            model.hit_edit_edge_handle(Point { x: 200, y: 102 }),
            Some(Handle::N)
        );
        assert_eq!(
            model.hit_edit_edge_handle(Point { x: 102, y: 150 }),
            Some(Handle::W)
        );
        assert_eq!(
            model.hit_edit_edge_handle(Point { x: 298, y: 198 }),
            Some(Handle::SE)
        );
        // The middle of the field belongs to the editor and its caret.
        assert_eq!(model.hit_edit_edge_handle(Point { x: 200, y: 150 }), None);
        // The padded frame outside the field is not an edit-control edge.
        assert_eq!(model.hit_edit_edge_handle(Point { x: 106, y: 94 }), None);
        // Without an open editor there is no edge to grab.
        model.editing = false;
        assert_eq!(model.hit_edit_edge_handle(Point { x: 200, y: 102 }), None);
    }

    #[test]
    fn copy_and_save_hand_the_picture_to_the_host() {
        let mut model = model(Overlay::Screenshot);
        model.selection = Some(Selection {
            x: 10,
            y: 10,
            w: 100,
            h: 80,
        });
        model.layout(&Fixed);
        let index = model
            .buttons
            .iter()
            .position(|button| button.role == Role::Copy)
            .expect("copy");
        assert!(matches!(
            model.press(index, Point { x: 0, y: 0 }, &Fixed),
            Action::Copy
        ));
        let index = model
            .buttons
            .iter()
            .position(|button| button.role == Role::Save)
            .expect("save");
        assert!(matches!(
            model.press(index, Point { x: 0, y: 0 }, &Fixed),
            Action::SaveDialog
        ));
    }

    #[test]
    fn escape_returns_to_select_before_it_cancels() {
        let mut model = model(Overlay::Screenshot);
        model.selection = Some(Selection {
            x: 10,
            y: 10,
            w: 100,
            h: 80,
        });
        model.tool = Tool::Pen;
        assert!(matches!(
            model.key_down(Key::Escape, false, false, &Fixed),
            Action::None
        ));
        assert_eq!(model.tool, Tool::Select);
        assert!(matches!(
            model.key_down(Key::Escape, false, false, &Fixed),
            Action::Finish(Outcome::Cancel)
        ));
    }

    /// A model whose frame is `width`×`height`, with the pixel index in the red
    /// byte so a crop can be followed by eye.
    fn picture(width: u32, height: u32) -> Model {
        let data = (0..width * height)
            .flat_map(|index| [index as u8, 0, 0, 255])
            .collect();
        Model::new(
            Overlay::Screenshot,
            true,
            Frame {
                width,
                height,
                data,
                age_ns: 0,
            },
            Rect {
                x: 0,
                y: 0,
                w: width,
                h: height,
            },
            crate::palette::DEFAULT,
        )
    }

    #[test]
    fn crop_pixels_takes_the_selected_rows_and_leaves_the_rest() {
        let mut model = picture(4, 4);
        model.selection = Some(Selection {
            x: 1,
            y: 1,
            w: 2,
            h: 2,
        });
        let cropped = crop_pixels(&model).expect("a selected area");
        assert_eq!((cropped.width, cropped.height), (2, 2));
        assert_eq!(cropped.data.len(), 2 * 2 * 4);
        // The first pixel of the crop is (1, 1) of the frame: index 5.
        assert_eq!(&cropped.data[0..4], &[5, 0, 0, 255]);
        // The last one is (2, 2): index 10.
        assert_eq!(&cropped.data[12..16], &[10, 0, 0, 255]);
    }

    #[test]
    fn crop_pixels_needs_a_usable_selection_and_pixels() {
        let model = picture(4, 4);
        assert!(crop_pixels(&model).is_none(), "nothing is selected");
        let mut model = picture(4, 4);
        model.selection = Some(Selection {
            x: 0,
            y: 0,
            w: 1,
            h: 1,
        });
        assert!(crop_pixels(&model).is_none(), "one pixel is not usable");
        let mut model = picture(0, 0);
        model.selection = Some(Selection {
            x: 0,
            y: 0,
            w: 10,
            h: 10,
        });
        assert!(crop_pixels(&model).is_none(), "there is no frame");
    }

    #[test]
    fn one_outcome_waits_in_the_channel_until_the_app_takes_it() {
        // The channel is process-wide; start from a known empty state.
        let _ = take_outcome();
        assert!(!pending());
        assert!(publish(Outcome::Cancel));
        assert!(pending());
        assert!(
            !publish(Outcome::Screen),
            "a second outcome must be refused"
        );
        assert!(matches!(take_outcome(), Some(Outcome::Cancel)));
        assert!(!pending());
        assert!(take_outcome().is_none());
    }

    #[test]
    fn a_reporting_renderer_remembers_whether_it_is_open() {
        set_active(true);
        assert!(reported_active());
        set_active(false);
        assert!(!reported_active());
    }
}
