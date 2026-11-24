//! The recording outline: a thin click-through frame around the recorded area.
//!
//! The frame is an override-redirect X11 window whose bounding shape is only the
//! perimeter of the rectangle: the interior is not part of the window, so it is
//! not drawn and pointer events go to whatever is below. The input shape is set
//! to an empty region as well, so even the drawn stroke lets clicks through. The
//! window is created by the first `sync(Some(rect))`, moved while the area is
//! recorded, and destroyed by `sync(None)`/`hide`.
//!
//! On Wayland the same window is drawn through XWayland, which composes it above
//! the Wayland windows. It needs a reachable X11 server and the Shape extension;
//! without XWayland the outline is a no-op with a single warning.

use platform_core::geom;
use platform_core::palette::Palette;
use platform_core::types::Rect;
use std::sync::OnceLock;
use x11rb::connection::RequestConnection;
use x11rb::protocol::shape;

/// Windows scales the stroke by the monitor DPI. The X11 core protocol has no
/// per-monitor DPI and toolkits already treat an X pixel as a physical pixel, so
/// the outline uses the 100% scale.
const DPI: u32 = 96;

/// The window rectangle and the hole cut out of it, in screen pixels.
///
/// The hole is the recorded area grown by the gap, so neither the stroke nor the
/// gap is part of the captured picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrameGeometry {
    outer: Bounds,
    inner: Bounds,
    stroke: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Bounds {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

impl FrameGeometry {
    fn new(rect: Rect, dpi: u32) -> Self {
        let outline = geom::recording_outline(rect.x, rect.y, rect.w, rect.h, dpi);
        let stroke = outline.stroke;
        Self {
            outer: Bounds {
                x: outline.x,
                y: outline.y,
                width: outline.width,
                height: outline.height,
            },
            inner: Bounds {
                x: outline.x + stroke,
                y: outline.y + stroke,
                width: outline.width - stroke * 2,
                height: outline.height - stroke * 2,
            },
            stroke,
        }
    }

    /// The four perimeter rectangles, in window coordinates.
    fn ring(&self) -> [Bounds; 4] {
        let (x, y) = (self.outer.x, self.outer.y);
        let top = self.inner.y - y;
        let bottom = self.inner.y + self.inner.height - y;
        let left = self.inner.x - x;
        let right = self.inner.x + self.inner.width - x;
        [
            Bounds {
                x: 0,
                y: 0,
                width: self.outer.width,
                height: top,
            },
            Bounds {
                x: 0,
                y: bottom,
                width: self.outer.width,
                height: self.outer.height - bottom,
            },
            Bounds {
                x: 0,
                y: 0,
                width: left,
                height: self.outer.height,
            },
            Bounds {
                x: right,
                y: 0,
                width: self.outer.width - right,
                height: self.outer.height,
            },
        ]
    }
}

/// Whether an X11 server with the Shape extension is reachable.
///
/// XWayland exposes an X11 display inside a Wayland session, so this is also the
/// Wayland availability check: the frame is drawn through that display. The
/// probe is cached because Settings asks for the capability on every refresh.
fn x11_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        let Ok((conn, _)) = x11rb::connect(None) else {
            return false;
        };
        conn.extension_information(shape::X11_EXTENSION_NAME)
            .is_ok_and(|info| info.is_some())
    })
}

/// Whether this platform can show the recording outline.
///
/// On Wayland this is true when XWayland is available; without it the Settings
/// row is hidden instead of promising a frame that never appears.
pub fn supported() -> bool {
    x11_available()
}

pub struct RegionOutline {
    palette: Palette,
    window: Option<x11::Outline>,
    /// Set after the first failure so the 200 ms timer does not retry a missing
    /// display or a server without Shape five times a second.
    unavailable: bool,
}

impl RegionOutline {
    pub fn new(palette: Palette) -> Self {
        Self {
            palette,
            window: None,
            unavailable: false,
        }
    }

    pub fn sync(&mut self, rect: Option<Rect>, elapsed_ms: u128) {
        let Some(rect) = rect else {
            self.hide();
            return;
        };
        if self.unavailable {
            return;
        }
        let frame = FrameGeometry::new(rect, DPI);
        let color = if geom::outline_blink(elapsed_ms) {
            self.palette.RECORDING
        } else {
            self.palette.RECORDING_DIM
        };
        let result = match self.window.take() {
            Some(mut window) => {
                let result = window.update(frame, color);
                if result.is_ok() {
                    self.window = Some(window);
                }
                result
            }
            None => x11::Outline::show(frame, color).map(|window| self.window = Some(window)),
        };
        if let Err(error) = result {
            platform_core::logging::warn(format!(
                "outline: {error}; the recording outline is not drawn"
            ));
            self.unavailable = true;
            self.window = None;
        }
    }

    pub fn hide(&mut self) {
        self.window = None;
    }
}

#[cfg(target_os = "linux")]
mod x11 {
    use super::{Bounds, FrameGeometry};
    use x11rb::connection::{Connection, RequestConnection};
    use x11rb::protocol::shape::{self, ConnectionExt as _};
    use x11rb::protocol::xproto::{
        AtomEnum, ChangeWindowAttributesAux, ClipOrdering, ConfigureWindowAux, ConnectionExt as _,
        CreateWindowAux, PropMode, Rectangle, StackMode, Window, WindowClass,
    };
    use x11rb::rust_connection::RustConnection;
    use x11rb::wrapper::ConnectionExt as _;

    const WINDOW_NAME: &str = "Capture Button recording area";

    type Result<T> = std::result::Result<T, String>;

    /// An override-redirect X11 window shaped to the outline's perimeter.
    pub(super) struct Outline {
        conn: RustConnection,
        window: Window,
        frame: FrameGeometry,
        color: u32,
    }

    impl Outline {
        pub(super) fn show(frame: FrameGeometry, color: u32) -> Result<Self> {
            let (conn, screen_num) = x11rb::connect(None)
                .map_err(|error| format!("cannot open the X11 display: {error}"))?;
            if conn
                .extension_information(shape::X11_EXTENSION_NAME)
                .map_err(|error| format!("cannot query the X11 Shape extension: {error}"))?
                .is_none()
            {
                return Err("the X11 Shape extension is unavailable".into());
            }
            let screen = &conn.setup().roots[screen_num];
            let window = conn
                .generate_id()
                .map_err(|error| format!("cannot allocate an X11 window: {error}"))?;
            let pixel = rgb(color);
            let aux = CreateWindowAux::new()
                .override_redirect(1)
                .background_pixel(pixel);
            conn.create_window(
                screen.root_depth,
                window,
                screen.root,
                coordinate(frame.outer.x),
                coordinate(frame.outer.y),
                extent(frame.outer.width),
                extent(frame.outer.height),
                0,
                WindowClass::INPUT_OUTPUT,
                0, // CopyFromParent
                &aux,
            )
            .map_err(|error| format!("cannot create the outline window: {error}"))?;
            conn.change_property8(
                PropMode::REPLACE,
                window,
                AtomEnum::WM_NAME,
                AtomEnum::STRING,
                WINDOW_NAME.as_bytes(),
            )
            .map_err(|error| format!("cannot name the outline window: {error}"))?;
            let outline = Self {
                conn,
                window,
                frame,
                color,
            };
            outline.shape()?;
            outline
                .conn
                .map_window(window)
                .map_err(|error| format!("cannot map the outline window: {error}"))?;
            outline.raise()?;
            outline.flush()?;
            Ok(outline)
        }

        pub(super) fn update(&mut self, frame: FrameGeometry, color: u32) -> Result<()> {
            if frame != self.frame {
                self.conn
                    .configure_window(
                        self.window,
                        &ConfigureWindowAux::new()
                            .x(i32::from(coordinate(frame.outer.x)))
                            .y(i32::from(coordinate(frame.outer.y)))
                            .width(u32::from(extent(frame.outer.width)))
                            .height(u32::from(extent(frame.outer.height)))
                            .stack_mode(StackMode::ABOVE),
                    )
                    .map_err(|error| format!("cannot move the outline window: {error}"))?;
                self.frame = frame;
                self.shape()?;
            }
            if color != self.color {
                self.color = color;
                self.conn
                    .change_window_attributes(
                        self.window,
                        &ChangeWindowAttributesAux::new().background_pixel(rgb(color)),
                    )
                    .map_err(|error| format!("cannot recolor the outline window: {error}"))?;
                self.conn
                    .clear_area(false, self.window, 0, 0, 0, 0)
                    .map_err(|error| format!("cannot repaint the outline window: {error}"))?;
            }
            self.raise()?;
            self.flush()
        }

        fn shape(&self) -> Result<()> {
            let ring: Vec<Rectangle> = self.frame.ring().into_iter().map(rectangle).collect();
            self.conn
                .shape_rectangles(
                    shape::SO::SET,
                    shape::SK::BOUNDING,
                    ClipOrdering::UNSORTED,
                    self.window,
                    0,
                    0,
                    &ring,
                )
                .map_err(|error| format!("cannot shape the outline window: {error}"))?;
            // An empty input region makes the drawn stroke click-through too.
            self.conn
                .shape_rectangles(
                    shape::SO::SET,
                    shape::SK::INPUT,
                    ClipOrdering::UNSORTED,
                    self.window,
                    0,
                    0,
                    &[],
                )
                .map_err(|error| format!("cannot make the outline click-through: {error}"))?;
            Ok(())
        }

        fn raise(&self) -> Result<()> {
            self.conn
                .configure_window(
                    self.window,
                    &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
                )
                .map_err(|error| format!("cannot raise the outline window: {error}"))?;
            Ok(())
        }

        fn flush(&self) -> Result<()> {
            self.conn
                .flush()
                .map_err(|error| format!("cannot flush the outline window: {error}"))
        }
    }

    impl Drop for Outline {
        fn drop(&mut self) {
            // The window is destroyed with its connection; an X error here has
            // nowhere to go, and the outline is best-effort.
            let _ = self.conn.destroy_window(self.window);
            let _ = self.conn.flush();
        }
    }

    /// X11 pixels are `0x00RRGGBB`; the palette stores `0x00BBGGRR`.
    fn rgb(color: u32) -> u32 {
        ((color & 0x0000_00ff) << 16) | (color & 0x0000_ff00) | ((color >> 16) & 0x0000_00ff)
    }

    fn coordinate(value: i32) -> i16 {
        value.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
    }

    fn extent(value: i32) -> u16 {
        value.clamp(1, i32::from(u16::MAX)) as u16
    }

    fn rectangle(bounds: Bounds) -> Rectangle {
        Rectangle {
            x: coordinate(bounds.x),
            y: coordinate(bounds.y),
            width: extent(bounds.width),
            height: extent(bounds.height),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_sits_outside_the_recorded_area() {
        let frame = FrameGeometry::new(
            Rect {
                x: 100,
                y: 50,
                w: 300,
                h: 200,
            },
            96,
        );
        assert_eq!(
            frame.outer,
            Bounds {
                x: 92,
                y: 42,
                width: 316,
                height: 216,
            }
        );
        assert_eq!(
            frame.inner,
            Bounds {
                x: 96,
                y: 46,
                width: 308,
                height: 208,
            }
        );
        assert_eq!(frame.stroke, 4);
        // The hole is still larger than the captured pixels: a gap of 4 px.
        assert!(frame.inner.x < 100 && frame.inner.y < 50);
    }

    #[test]
    fn frame_keeps_negative_coordinates() {
        let frame = FrameGeometry::new(
            Rect {
                x: -50,
                y: -20,
                w: 200,
                h: 100,
            },
            96,
        );
        assert_eq!(
            frame.outer,
            Bounds {
                x: -58,
                y: -28,
                width: 216,
                height: 116,
            }
        );
        assert_eq!(
            frame.inner,
            Bounds {
                x: -54,
                y: -24,
                width: 208,
                height: 108,
            }
        );
    }

    #[test]
    fn ring_is_only_the_perimeter() {
        let frame = FrameGeometry::new(
            Rect {
                x: 0,
                y: 0,
                w: 100,
                h: 50,
            },
            96,
        );
        assert_eq!(
            frame.ring(),
            [
                Bounds {
                    x: 0,
                    y: 0,
                    width: 116,
                    height: 4,
                },
                Bounds {
                    x: 0,
                    y: 62,
                    width: 116,
                    height: 4,
                },
                Bounds {
                    x: 0,
                    y: 0,
                    width: 4,
                    height: 66,
                },
                Bounds {
                    x: 112,
                    y: 0,
                    width: 4,
                    height: 66,
                },
            ]
        );
    }
}
