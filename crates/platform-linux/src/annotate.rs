//! Bakes the area overlay's annotations into a saved still.
//!
//! The Linux overlay is the Slint one: it draws the strokes and the text into
//! its window, while a saved picture is cropped from the frozen screen and has
//! none of them. This module is the Linux counterpart of the native overlay's
//! `draw_annotations`: it paints the same geometry — the same pen and marker
//! widths, the same arrow head, the same word-wrapped text — into the cropped
//! frame, so the file the user saves is the picture the user saw.

use ab_glyph::{Font, FontVec, GlyphId, OutlineCurve, PxScaleFont, ScaleFont};
use platform_core::palette;
use platform_core::region::{Point, Shape, ShapeKind};
use platform_core::types::Frame;
use std::path::PathBuf;
use std::process::Command;
use tiny_skia::{FillRule, LineCap, LineJoin, Paint, Path, PathBuilder, Pixmap, Stroke, Transform};

/// Fonts to try when fontconfig cannot name a file for the family.
const FALLBACK_FONTS: [&str; 5] = [
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/freefont/FreeSans.ttf",
];

/// Paints `shapes` into `frame`, moving every point by `offset`.
///
/// The offset is the selection's origin, so an annotation is drawn where the
/// overlay showed it inside the cropped picture. False when the frame is not a
/// usable picture; the shapes are then left out rather than half drawn.
pub fn bake(frame: &mut Frame, shapes: &[Shape], offset: Point) -> bool {
    if frame.width == 0 || frame.height == 0 {
        return false;
    }
    let expected = frame.width as usize * frame.height as usize * 4;
    if frame.data.len() < expected {
        return false;
    }
    let Some(mut pixmap) = Pixmap::new(frame.width, frame.height) else {
        return false;
    };
    unpack(frame, pixmap.data_mut());
    for shape in shapes {
        draw_shape(&mut pixmap, shape, offset);
    }
    pack(pixmap.data(), frame);
    true
}

/// Copies the frame's BGRA pixels into the pixmap's premultiplied RGBA ones.
///
/// A screenshot is opaque, so the conversion is a channel swap plus a full
/// alpha, which is also what the native overlay's GDI buffer assumes.
fn unpack(frame: &Frame, pixels: &mut [u8]) {
    for (target, source) in pixels
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(frame.data.as_chunks::<4>().0)
    {
        target[0] = source[2];
        target[1] = source[1];
        target[2] = source[0];
        target[3] = 255;
    }
}

/// Writes the pixmap back as the opaque BGRA the rest of the app saves.
fn pack(pixels: &[u8], frame: &mut Frame) {
    for (target, source) in frame
        .data
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(pixels.as_chunks::<4>().0)
    {
        target[0] = source[2];
        target[1] = source[1];
        target[2] = source[0];
        target[3] = 255;
    }
}

/// `0x00BBGGRR` to the three bytes a `Paint` takes.
fn rgb(color: u32) -> (u8, u8, u8) {
    (
        (color & 0xff) as u8,
        ((color >> 8) & 0xff) as u8,
        ((color >> 16) & 0xff) as u8,
    )
}

/// The paint of one annotation: the marker's own colour and thickness are the
/// native overlay's doubling, not the shape's.
fn annotation_paint(shape: &Shape) -> (Paint<'_>, i32) {
    let marker = matches!(shape.kind, ShapeKind::Marker(_));
    let color = if marker {
        palette::DEFAULT.MARKER
    } else {
        shape.color
    };
    let width = if marker { shape.width * 4 } else { shape.width }.max(1);
    let (red, green, blue) = rgb(color);
    let mut paint = Paint::default();
    paint.set_color_rgba8(red, green, blue, 255);
    paint.anti_alias = true;
    (paint, width)
}

fn draw_shape(pixmap: &mut Pixmap, shape: &Shape, offset: Point) {
    let (paint, width) = annotation_paint(shape);
    let p = |point: Point| ((point.x + offset.x) as f32, (point.y + offset.y) as f32);
    match &shape.kind {
        ShapeKind::Pen(points) | ShapeKind::Marker(points) => {
            if points.len() == 1 {
                // A single click leaves a dot, like the native overlay's ellipse.
                let (x, y) = p(points[0]);
                fill_circle(pixmap, &paint, x, y, width as f32 / 2.0);
            } else {
                let points: Vec<(f32, f32)> = points.iter().copied().map(p).collect();
                stroke(pixmap, &paint, width as f32, &points, false);
            }
        }
        ShapeKind::Line(from, to) => {
            stroke(pixmap, &paint, width as f32, &[p(*from), p(*to)], false);
        }
        ShapeKind::Rectangle(from, to) => {
            let (left, top) = p(*from);
            let (right, bottom) = p(*to);
            stroke(
                pixmap,
                &paint,
                width as f32,
                &[(left, top), (right, top), (right, bottom), (left, bottom)],
                true,
            );
        }
        ShapeKind::Arrow(from, to) => {
            let (ax, ay) = p(*from);
            let (bx, by) = p(*to);
            stroke(pixmap, &paint, width as f32, &[(ax, ay), (bx, by)], false);
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
            fill_polygon(pixmap, &paint, &[left, (bx, by), right]);
        }
        ShapeKind::Text(at, wrap, text) => {
            draw_text(pixmap, shape, *at, *wrap, text, offset);
        }
    }
}

fn stroke(pixmap: &mut Pixmap, paint: &Paint, width: f32, points: &[(f32, f32)], close: bool) {
    let Some(first) = points.first() else {
        return;
    };
    let mut builder = PathBuilder::new();
    builder.move_to(first.0, first.1);
    for point in &points[1..] {
        builder.line_to(point.0, point.1);
    }
    if close {
        builder.close();
    }
    let Some(path) = builder.finish() else {
        return;
    };
    let stroke = Stroke {
        width,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Default::default()
    };
    pixmap.stroke_path(&path, paint, &stroke, Transform::identity(), None);
}

fn fill_circle(pixmap: &mut Pixmap, paint: &Paint, x: f32, y: f32, radius: f32) {
    let mut builder = PathBuilder::new();
    builder.push_circle(x, y, radius.max(0.5));
    if let Some(path) = builder.finish() {
        pixmap.fill_path(&path, paint, FillRule::Winding, Transform::identity(), None);
    }
}

fn fill_polygon(pixmap: &mut Pixmap, paint: &Paint, points: &[(f32, f32)]) {
    let Some(first) = points.first() else {
        return;
    };
    let mut builder = PathBuilder::new();
    builder.move_to(first.0, first.1);
    for point in &points[1..] {
        builder.line_to(point.0, point.1);
    }
    builder.close();
    if let Some(path) = builder.finish() {
        pixmap.fill_path(&path, paint, FillRule::Winding, Transform::identity(), None);
    }
}

/// Draws a committed text shape: the same origin, wrap width and size the Slint
/// scene gives its `Text` element.
fn draw_text(pixmap: &mut Pixmap, shape: &Shape, at: Point, wrap: i32, text: &str, offset: Point) {
    if text.trim().is_empty() {
        return;
    }
    let Some(font) = load_font(&shape.font_name) else {
        return;
    };
    let size = shape.font_size.max(18) as f32;
    let scaled = font.as_scaled(size);
    let line_height = (scaled.ascent() - scaled.descent() + scaled.line_gap()).max(size);
    let (red, green, blue) = rgb(shape.color);
    let mut paint = Paint::default();
    paint.set_color_rgba8(red, green, blue, 255);
    paint.anti_alias = true;
    let left = (at.x + offset.x) as f32;
    let mut baseline = (at.y + offset.y) as f32 + scaled.ascent();
    for line in wrap_lines(text, wrap.max(1) as f32, |ch| {
        scaled.h_advance(scaled.glyph_id(ch))
    }) {
        let mut pen = left;
        let mut previous: Option<GlyphId> = None;
        for ch in line.chars() {
            let id = scaled.glyph_id(ch);
            if let Some(previous) = previous {
                pen += scaled.kern(previous, id);
            }
            if ch != ' ' {
                if let Some(path) = glyph_path(&font, &scaled, id, pen, baseline) {
                    pixmap.fill_path(
                        &path,
                        &paint,
                        FillRule::Winding,
                        Transform::identity(),
                        None,
                    );
                }
            }
            pen += scaled.h_advance(id);
            previous = Some(id);
        }
        baseline += line_height;
    }
}

/// Greedy word wrap at `width`, the way the scene's `word-wrap: word-wrap` text
/// breaks. A word that does not fit on a line of its own breaks between glyphs.
fn wrap_lines(text: &str, width: f32, advance: impl Fn(char) -> f32) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        let mut used = 0.0f32;
        for word in paragraph.split(' ') {
            let word_width: f32 = word.chars().map(&advance).sum();
            if !line.is_empty() && used + advance(' ') + word_width > width {
                lines.push(std::mem::take(&mut line));
                used = 0.0;
            }
            if !line.is_empty() {
                line.push(' ');
                used += advance(' ');
            }
            if word_width <= width {
                line.push_str(word);
                used += word_width;
                continue;
            }
            for ch in word.chars() {
                let next = advance(ch);
                if !line.is_empty() && used + next > width {
                    lines.push(std::mem::take(&mut line));
                    used = 0.0;
                }
                line.push(ch);
                used += next;
            }
        }
        lines.push(line);
    }
    lines
}

/// The outline of one glyph, positioned at the pen and scaled like the font.
fn glyph_path(
    font: &FontVec,
    scaled: &PxScaleFont<&FontVec>,
    id: GlyphId,
    x: f32,
    baseline: f32,
) -> Option<Path> {
    let outline = font.outline(id)?;
    let factor = scaled.scale_factor();
    let place = |point: ab_glyph::Point| {
        (
            x + point.x * factor.horizontal,
            baseline - point.y * factor.vertical,
        )
    };
    let mut builder = PathBuilder::new();
    let mut cursor: Option<(f32, f32)> = None;
    for curve in &outline.curves {
        let start = match curve {
            OutlineCurve::Line(from, _) => *from,
            OutlineCurve::Quad(from, _, _) => *from,
            OutlineCurve::Cubic(from, _, _, _) => *from,
        };
        let (sx, sy) = place(start);
        let connected =
            cursor.is_some_and(|(cx, cy)| (cx - sx).abs() < 0.01 && (cy - sy).abs() < 0.01);
        if !connected {
            if cursor.is_some() {
                builder.close();
            }
            builder.move_to(sx, sy);
        }
        match curve {
            OutlineCurve::Line(_, to) => {
                let (tx, ty) = place(*to);
                builder.line_to(tx, ty);
            }
            OutlineCurve::Quad(_, control, to) => {
                let (cx, cy) = place(*control);
                let (tx, ty) = place(*to);
                builder.quad_to(cx, cy, tx, ty);
            }
            OutlineCurve::Cubic(_, first, second, to) => {
                let (ax, ay) = place(*first);
                let (bx, by) = place(*second);
                let (tx, ty) = place(*to);
                builder.cubic_to(ax, ay, bx, by, tx, ty);
            }
        }
        let end = match curve {
            OutlineCurve::Line(_, to) => *to,
            OutlineCurve::Quad(_, _, to) => *to,
            OutlineCurve::Cubic(_, _, _, to) => *to,
        };
        cursor = Some(place(end));
    }
    builder.close();
    builder.finish()
}

/// The file fontconfig names for a family, the way the Slint window asks for it.
fn font_file(family: &str) -> Option<PathBuf> {
    let output = Command::new("fc-match")
        .args(["--format", "%{file}", family])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!path.is_empty()).then(|| PathBuf::from(path))
}

/// The face to draw a text shape with: the requested family, the system's
/// default sans, then a short list of usual paths.
fn load_font(family: &str) -> Option<FontVec> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    for name in [family, "sans-serif"] {
        if let Some(path) = font_file(name) {
            candidates.push(path);
        }
    }
    candidates.extend(FALLBACK_FONTS.iter().map(PathBuf::from));
    candidates
        .into_iter()
        .find_map(|path| FontVec::try_from_vec(std::fs::read(path).ok()?).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blank(width: u32, height: u32) -> Frame {
        Frame {
            width,
            height,
            data: vec![0; width as usize * height as usize * 4],
            age_ns: 0,
        }
    }

    fn shape(kind: ShapeKind, color: u32) -> Shape {
        Shape {
            kind,
            color,
            width: 4,
            font_size: 28,
            font_name: "Segoe UI".into(),
        }
    }

    fn pixel(frame: &Frame, x: u32, y: u32) -> (u8, u8, u8) {
        let offset = (y as usize * frame.width as usize + x as usize) * 4;
        let data = &frame.data[offset..offset + 4];
        (data[2], data[1], data[0])
    }

    fn painted(frame: &Frame) -> usize {
        frame
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|pixel| pixel[0] | pixel[1] | pixel[2] != 0)
            .count()
    }

    /// Left, top, right, bottom of everything that is not background.
    fn ink_bounds(frame: &Frame) -> Option<(u32, u32, u32, u32)> {
        let mut bounds: Option<(u32, u32, u32, u32)> = None;
        for y in 0..frame.height {
            for x in 0..frame.width {
                if pixel(frame, x, y) == (0, 0, 0) {
                    continue;
                }
                bounds = Some(match bounds {
                    Some((left, top, right, bottom)) => {
                        (left.min(x), top.min(y), right.max(x), bottom.max(y))
                    }
                    None => (x, y, x, y),
                });
            }
        }
        bounds
    }

    #[test]
    fn an_empty_still_is_still_a_valid_picture() {
        let mut frame = blank(10, 10);
        assert!(bake(&mut frame, &[], Point { x: 0, y: 0 }));
        assert_eq!(painted(&frame), 0);
        assert!(frame
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| pixel[3] == 255));
    }

    #[test]
    fn an_unusable_frame_is_refused() {
        let mut empty = blank(0, 0);
        assert!(!bake(&mut empty, &[], Point { x: 0, y: 0 }));
        let mut short = Frame {
            width: 4,
            height: 4,
            data: vec![0; 4],
            age_ns: 0,
        };
        assert!(!bake(&mut short, &[], Point { x: 0, y: 0 }));
    }

    #[test]
    fn an_arrow_lands_where_the_selection_offset_puts_it() {
        // The arrow is drawn at (20,30)-(100,30), and the offset moves it by the
        // selection's origin, which is the same 20/30, so it points along y=60.
        let mut frame = blank(160, 120);
        let arrow = shape(
            ShapeKind::Arrow(Point { x: 20, y: 30 }, Point { x: 100, y: 30 }),
            0x0000_00ff,
        );
        assert!(bake(&mut frame, &[arrow], Point { x: 20, y: 30 }));
        // The shaft is on the row the arrow points along.
        assert_eq!(pixel(&frame, 60, 60), (255, 0, 0));
        assert_eq!(pixel(&frame, 80, 60), (255, 0, 0));
        // Nothing above it.
        assert_eq!(pixel(&frame, 60, 20), (0, 0, 0));
        // The head fills a triangle near the tip, off the shaft's row.
        assert_eq!(pixel(&frame, 110, 56), (255, 0, 0));
        assert!(painted(&frame) > 200);
    }

    #[test]
    fn a_marker_is_thicker_and_uses_the_marker_colour() {
        let mut frame = blank(80, 40);
        let pen = shape(
            ShapeKind::Pen(vec![Point { x: 0, y: 0 }, Point { x: 60, y: 0 }]),
            0x0000_00ff,
        );
        let marker = shape(
            ShapeKind::Marker(vec![Point { x: 0, y: 20 }, Point { x: 60, y: 20 }]),
            0x0000_00ff,
        );
        assert!(bake(&mut frame, &[pen, marker], Point { x: 10, y: 10 }));
        let (red, green, blue) = rgb(palette::DEFAULT.MARKER);
        assert_eq!(pixel(&frame, 30, 10), (255, 0, 0));
        assert_eq!(pixel(&frame, 30, 30), (red, green, blue));
        // The marker's stroke is four times as wide: it covers rows the pen does not.
        assert_ne!(pixel(&frame, 30, 36), (0, 0, 0));
        assert_eq!(pixel(&frame, 30, 16), (0, 0, 0));
    }

    #[test]
    fn a_single_clicked_pen_leaves_a_dot() {
        let mut frame = blank(40, 40);
        let dot = shape(ShapeKind::Pen(vec![Point { x: 20, y: 20 }]), 0x0000_00ff);
        assert!(bake(&mut frame, &[dot], Point { x: 0, y: 0 }));
        assert_eq!(pixel(&frame, 20, 20), (255, 0, 0));
        assert_eq!(pixel(&frame, 0, 0), (0, 0, 0));
    }

    #[test]
    fn a_rectangle_is_closed() {
        let mut frame = blank(80, 80);
        let rect = shape(
            ShapeKind::Rectangle(Point { x: 10, y: 10 }, Point { x: 60, y: 50 }),
            0x0000_00ff,
        );
        assert!(bake(&mut frame, &[rect], Point { x: 0, y: 0 }));
        // All four edges are drawn, including the one closing the path.
        assert_eq!(pixel(&frame, 10, 30), (255, 0, 0));
        assert_eq!(pixel(&frame, 60, 30), (255, 0, 0));
        assert_eq!(pixel(&frame, 35, 10), (255, 0, 0));
        assert_eq!(pixel(&frame, 35, 50), (255, 0, 0));
        assert_eq!(pixel(&frame, 35, 30), (0, 0, 0));
    }

    #[test]
    fn the_font_chain_finds_a_face() {
        assert!(
            load_font("Segoe UI").is_some(),
            "no system font to draw text with"
        );
    }

    #[test]
    fn text_is_drawn_inside_its_origin_and_wrap() {
        let mut frame = blank(240, 80);
        let text = shape(
            ShapeKind::Text(Point { x: 10, y: 10 }, 120, "Hello".into()),
            0x00ff_ffff,
        );
        assert!(bake(&mut frame, &[text], Point { x: 0, y: 0 }));
        let ink = painted(&frame);
        assert!(ink > 40, "only {ink} text pixels were drawn");
        let (left, top, right, bottom) = ink_bounds(&frame).expect("text pixels");
        // The glyphs start at the text's origin, not before it.
        assert!(left >= 10, "ink starts at x={left}");
        assert!(top >= 10, "ink starts at y={top}");
        // And the box width still wraps the line.
        assert!(right < 10 + 120, "ink reaches x={right}");
        assert!(bottom < 10 + 60, "ink reaches y={bottom}");
    }

    #[test]
    fn long_text_wraps_at_the_box_width() {
        let mut frame = blank(240, 120);
        let text = shape(
            ShapeKind::Text(Point { x: 10, y: 10 }, 80, "one two three".into()),
            0x00ff_ffff,
        );
        assert!(bake(&mut frame, &[text], Point { x: 0, y: 0 }));
        let rows: Vec<u32> = (0..120)
            .filter(|y| (0..100).any(|x| pixel(&frame, x, *y) != (0, 0, 0)))
            .collect();
        assert!(
            rows.last().copied().unwrap_or(0) > 30,
            "the text did not wrap: rows {rows:?}"
        );
    }
}
