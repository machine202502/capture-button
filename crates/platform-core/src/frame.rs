//! Shared frame utilities used by both the screen backend and the app's
//! capture engine.

use crate::types::Frame;

pub fn frame_consistent(frame: &Frame) -> bool {
    frame.width >= 2
        && frame.height >= 2
        && frame.data.len() == frame.width as usize * frame.height as usize * 4
}

pub fn blank(width: u32, height: u32) -> Frame {
    Frame {
        width,
        height,
        data: vec![0; width as usize * height as usize * 4],
        age_ns: 0,
    }
}

/// Fits a frame into an even `width`×`height` by cropping or padding the right
/// and bottom edge, never scaling. A torn or undersized frame becomes a blank.
pub fn fit(frame: Frame, width: u32, height: u32) -> Frame {
    let width = width & !1;
    let height = height & !1;
    if !frame_consistent(&frame) {
        return blank(width, height);
    }
    if frame.width == width && frame.height == height {
        return frame;
    }
    let mut data = vec![0u8; width as usize * height as usize * 4];
    let copy_w = (frame.width.min(width)) as usize;
    let copy_h = (frame.height.min(height)) as usize;
    for y in 0..copy_h {
        let source = y * frame.width as usize * 4;
        let target = y * width as usize * 4;
        let row = copy_w * 4;
        if source + row > frame.data.len() || target + row > data.len() {
            return blank(width, height);
        }
        data[target..target + row].copy_from_slice(&frame.data[source..source + row]);
    }
    Frame {
        width,
        height,
        data,
        age_ns: frame.age_ns,
    }
}

#[cfg(test)]
mod tests {
    use super::{fit, Frame};

    #[test]
    fn an_odd_window_is_locked_to_an_even_frame_before_encoding() {
        let frame = Frame {
            width: 51,
            height: 40,
            data: [9u8, 8, 7, 255].repeat(51 * 40),
            age_ns: 0,
        };
        let fitted = fit(frame, 50, 40);
        assert_eq!((fitted.width, fitted.height), (50, 40));
        assert_eq!(fitted.data.len(), 50 * 40 * 4);
    }
}
