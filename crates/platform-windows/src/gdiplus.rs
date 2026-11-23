//! Anti-aliased drawing for the area overlay's chrome, through GDI+.
//!
//! Plain GDI has no anti-aliasing for shapes: circles come out jagged and
//! rounded corners show a staircase. The overlay keeps GDI for the frozen frame
//! (one fast blit of the whole screen) and uses this module for the small
//! chrome — selection border, buttons, badge, swatch dots and annotation
//! strokes — so it looks like the Slint overlay without a software-rendered
//! full screen.
//!
//! The functions never panic: when GDI+ cannot start or a call fails, they draw
//! nothing and the caller keeps whatever it drew before.

use windows::Win32::Foundation::COLORREF;
use windows::Win32::Graphics::Gdi::HDC;
use windows::Win32::Graphics::GdiPlus::*;

/// `Status::Ok` is zero.
const OK: i32 = 0;

/// Starts GDI+ once per process and returns its token.
fn token() -> Option<usize> {
    static TOKEN: std::sync::OnceLock<Option<usize>> = std::sync::OnceLock::new();
    *TOKEN.get_or_init(|| {
        let input = GdiplusStartupInput {
            GdiplusVersion: 1,
            ..Default::default()
        };
        let mut token = 0usize;
        // SAFETY: The input struct lives for the call, the output is optional and
        // null, and the returned token is only reused by GdiplusShutdown.
        let status = unsafe { GdiplusStartup(&mut token, &input, std::ptr::null_mut()) };
        (status.0 == OK && token != 0).then_some(token)
    })
}

/// GDI+ colors are `0xAARRGGBB`; `COLORREF` is `0x00BBGGRR`, so the red byte
/// is the lowest one and the blue byte the highest.
fn argb(color: COLORREF) -> u32 {
    let value = color.0;
    let (r, g, b) = (value & 0xff, (value >> 8) & 0xff, (value >> 16) & 0xff);
    0xff00_0000 | (r << 16) | (g << 8) | b
}

/// An anti-aliased drawing surface on a GDI device context.
///
/// Dropping it releases the GDI+ graphics; the device context is owned by the
/// caller and stays usable by GDI afterwards.
pub struct Graphics {
    native: *mut GpGraphics,
}

impl Graphics {
    pub fn new(dc: HDC) -> Option<Self> {
        token()?;
        let mut native: *mut GpGraphics = std::ptr::null_mut();
        // SAFETY: The HDC is valid for the duration of the call and the graphics
        // handle is written into a local that is checked below.
        let status = unsafe { GdipCreateFromHDC(dc, &mut native) };
        if status.0 != OK || native.is_null() {
            return None;
        }
        // SAFETY: `native` is a live graphics handle from the call above.
        unsafe { GdipSetSmoothingMode(native, SmoothingModeAntiAlias) }.ok();
        Some(Self { native })
    }

    fn brush(&self, color: COLORREF) -> Option<*mut GpSolidFill> {
        let mut brush: *mut GpSolidFill = std::ptr::null_mut();
        // SAFETY: The graphics is live and the brush handle is a local.
        let status = unsafe { GdipCreateSolidFill(argb(color), &mut brush) };
        (status.0 == OK && !brush.is_null()).then_some(brush)
    }

    fn pen(&self, color: COLORREF, width: f32) -> Option<*mut GpPen> {
        let mut pen: *mut GpPen = std::ptr::null_mut();
        // SAFETY: The width is a plain float and the pen handle is a local.
        let status = unsafe { GdipCreatePen1(argb(color), width.max(1.0), UnitPixel, &mut pen) };
        if status.0 != OK || pen.is_null() {
            return None;
        }
        // SAFETY: `pen` is a live pen from the call above.
        unsafe {
            GdipSetPenLineCap197819(pen, LineCapRound, LineCapRound, DashCapFlat).ok();
            GdipSetPenLineJoin(pen, LineJoinRound).ok();
        }
        Some(pen)
    }

    /// Fills an ellipse inscribed in the given rectangle.
    pub fn fill_ellipse(&self, color: COLORREF, x: f32, y: f32, w: f32, h: f32) {
        let Some(brush) = self.brush(color) else {
            return;
        };
        // SAFETY: The graphics and brush are live GDI+ handles.
        unsafe {
            GdipFillEllipse(self.native, brush.cast(), x, y, w, h).ok();
            GdipDeleteBrush(brush.cast()).ok();
        }
    }

    /// Draws the outline of the same ellipse.
    pub fn stroke_ellipse(&self, color: COLORREF, width: f32, x: f32, y: f32, w: f32, h: f32) {
        let Some(pen) = self.pen(color, width) else {
            return;
        };
        // SAFETY: The graphics and pen are live GDI+ handles.
        unsafe {
            GdipDrawEllipse(self.native, pen, x, y, w, h).ok();
            GdipDeletePen(pen).ok();
        }
    }

    /// Fills a rectangle with rounded corners; `radius` is the corner radius.
    pub fn fill_round_rect(
        &self,
        color: COLORREF,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
    ) {
        let path = round_rect_path(x, y, w, h, radius);
        if path.is_null() {
            return;
        }
        if let Some(brush) = self.brush(color) {
            // SAFETY: The graphics, brush and path are live GDI+ handles.
            unsafe {
                GdipFillPath(self.native, brush.cast(), path).ok();
                GdipDeleteBrush(brush.cast()).ok();
            }
        }
        // SAFETY: The path was created by round_rect_path and is not used again.
        unsafe { GdipDeletePath(path).ok() };
    }

    /// Draws the outline of a rounded rectangle; `radius` is the corner radius.
    #[allow(clippy::too_many_arguments)]
    pub fn stroke_round_rect(
        &self,
        color: COLORREF,
        width: f32,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
    ) {
        let path = round_rect_path(x, y, w, h, radius);
        if path.is_null() {
            return;
        }
        if let Some(pen) = self.pen(color, width) {
            // SAFETY: The graphics, pen and path are live GDI+ handles.
            unsafe {
                GdipDrawPath(self.native, pen, path).ok();
                GdipDeletePen(pen).ok();
            }
        }
        // SAFETY: The path was created by round_rect_path and is not used again.
        unsafe { GdipDeletePath(path).ok() };
    }

    /// Fills an axis-aligned rectangle.
    pub fn fill_rect(&self, color: COLORREF, x: f32, y: f32, w: f32, h: f32) {
        let Some(brush) = self.brush(color) else {
            return;
        };
        // SAFETY: The graphics and brush are live GDI+ handles.
        unsafe {
            GdipFillRectangle(self.native, brush.cast(), x, y, w, h).ok();
            GdipDeleteBrush(brush.cast()).ok();
        }
    }

    /// Draws a polyline with round caps and joins.
    pub fn line(&self, color: COLORREF, width: f32, points: &[(f32, f32)]) {
        if points.len() < 2 {
            return;
        }
        let Some(pen) = self.pen(color, width) else {
            return;
        };
        let points: Vec<PointF> = points
            .iter()
            .map(|(x, y)| PointF { X: *x, Y: *y })
            .collect();
        // SAFETY: The point array lives for the call, and the graphics and pen are live.
        unsafe {
            GdipDrawLines(
                self.native,
                pen,
                points.as_ptr(),
                points.len() as i32,
            )
            .ok();
            GdipDeletePen(pen).ok();
        }
    }

    /// Fills a closed polygon, for the arrow head.
    pub fn fill_polygon(&self, color: COLORREF, points: &[(f32, f32)]) {
        if points.len() < 3 {
            return;
        }
        let path = polygon_path(points);
        if path.is_null() {
            return;
        }
        if let Some(brush) = self.brush(color) {
            // SAFETY: The graphics, brush and path are live GDI+ handles.
            unsafe {
                GdipFillPath(self.native, brush.cast(), path).ok();
                GdipDeleteBrush(brush.cast()).ok();
            }
        }
        // SAFETY: The path was created by polygon_path and is not used again.
        unsafe { GdipDeletePath(path).ok() };
    }

    /// Draws a closed polygon outline.
    pub fn stroke_polygon(&self, color: COLORREF, width: f32, points: &[(f32, f32)]) {
        if points.len() < 3 {
            return;
        }
        let path = polygon_path(points);
        if path.is_null() {
            return;
        }
        if let Some(pen) = self.pen(color, width) {
            // SAFETY: The graphics, pen and path are live GDI+ handles.
            unsafe {
                GdipDrawPath(self.native, pen, path).ok();
                GdipDeletePen(pen).ok();
            }
        }
        // SAFETY: The path was created by polygon_path and is not used again.
        unsafe { GdipDeletePath(path).ok() };
    }
}

impl Drop for Graphics {
    fn drop(&mut self) {
        // SAFETY: The graphics was created in `new` and is deleted exactly once.
        unsafe { GdipDeleteGraphics(self.native).ok() };
    }
}

/// A rounded-rectangle path, or null when GDI+ cannot create it.
fn round_rect_path(x: f32, y: f32, w: f32, h: f32, radius: f32) -> *mut GpPath {
    let mut path: *mut GpPath = std::ptr::null_mut();
    // SAFETY: The fill mode is a plain value and the path handle is a local.
    let status = unsafe { GdipCreatePath(FillModeWinding, &mut path) };
    if status.0 != OK || path.is_null() {
        return std::ptr::null_mut();
    }
    let r = radius.max(0.0).min(w / 2.0).min(h / 2.0);
    // SAFETY: `path` is a live path; every call below only adds to it.
    unsafe {
        if r > 0.5 {
            let d = 2.0 * r;
            GdipAddPathArc(path, x, y, d, d, 180.0, 90.0).ok();
            GdipAddPathArc(path, x + w - d, y, d, d, 270.0, 90.0).ok();
            GdipAddPathArc(path, x + w - d, y + h - d, d, d, 0.0, 90.0).ok();
            GdipAddPathArc(path, x, y + h - d, d, d, 90.0, 90.0).ok();
        } else {
            let corners = [
                PointF { X: x, Y: y },
                PointF {
                    X: x + w,
                    Y: y,
                },
                PointF {
                    X: x + w,
                    Y: y + h,
                },
                PointF { X: x, Y: y + h },
            ];
            GdipAddPathLine2(path, corners.as_ptr(), corners.len() as i32).ok();
        }
        GdipClosePathFigure(path).ok();
    }
    path
}

/// A closed polygon path, or null when GDI+ cannot create it.
fn polygon_path(points: &[(f32, f32)]) -> *mut GpPath {
    let mut path: *mut GpPath = std::ptr::null_mut();
    // SAFETY: The fill mode is a plain value and the path handle is a local.
    let status = unsafe { GdipCreatePath(FillModeWinding, &mut path) };
    if status.0 != OK || path.is_null() {
        return std::ptr::null_mut();
    }
    let points: Vec<PointF> = points
        .iter()
        .map(|(x, y)| PointF { X: *x, Y: *y })
        .collect();
    // SAFETY: The point array lives for the call and the path is live.
    unsafe {
        GdipAddPathLine2(path, points.as_ptr(), points.len() as i32).ok();
        GdipClosePathFigure(path).ok();
    }
    path
}

/// Small helper so a failed GDI+ call reads like "ignore the status".
trait IgnoreStatus {
    fn ok(self);
}

impl IgnoreStatus for Status {
    fn ok(self) {
        let _ = self;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colorref_channels_map_to_argb() {
        // COLORREF is 0x00BBGGRR: red sits in the low byte, blue in the high one.
        assert_eq!(argb(COLORREF(0x0000_00ff)), 0xffff_0000, "red");
        assert_eq!(argb(COLORREF(0x0000_ff00)), 0xff00_ff00, "green");
        assert_eq!(argb(COLORREF(0x00ff_0000)), 0xff00_00ff, "blue");
        assert_eq!(argb(COLORREF(0x0001_e2f0)), 0xfff0_e201, "mixed");
    }
}
