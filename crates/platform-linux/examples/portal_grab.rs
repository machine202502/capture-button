//! Live check of the portal screen-cast path.
//!
//! Run it with a Wayland-looking environment on the existing X11 session (the
//! portal itself works under X11 through mutter):
//!
//! ```text
//! XDG_SESSION_TYPE=wayland cargo run -p platform-linux --example portal_grab -- /tmp/portal.png
//! ```
//!
//! The portal picker appears; confirm a monitor and the example writes the
//! first frame and then one later frame as PNG files, printing both sizes.

use platform_core::types::{Frame, Rect};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/portal-grab.png"));
    println!(
        "Wayland portal path: {}",
        platform_linux::portal::session_is_wayland()
    );
    let whole = Rect {
        x: 0,
        y: 0,
        w: 1920,
        h: 1080,
    };
    let (monitor, x, y, width, height) = platform_linux::screen::monitor_of(whole);
    println!("X11 monitor: {width}x{height} at {x},{y}");
    let (mut capture, first) = platform_linux::screen::start_monitor(monitor, None, 10, false)?;
    println!(
        "first frame: {}x{} ({} bytes)",
        first.width,
        first.height,
        first.data.len()
    );
    save(&first, &path)?;
    let live = path.with_file_name(format!(
        "{}-live.{}",
        path.file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("portal-grab"),
        path.extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("png")
    ));
    std::thread::sleep(Duration::from_millis(700));
    let later = capture.latest().unwrap_or_else(|| first.clone());
    println!(
        "later frame: {}x{} ({} bytes)",
        later.width,
        later.height,
        later.data.len()
    );
    save(&later, &live)?;
    capture.stop();
    println!("wrote {} and {}", path.display(), live.display());
    Ok(())
}

fn save(frame: &Frame, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut rgba = Vec::with_capacity(frame.data.len());
    for pixel in frame.data.as_chunks::<4>().0 {
        rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 255]);
    }
    let image = image::RgbaImage::from_raw(frame.width, frame.height, rgba)
        .ok_or("the frame size does not match its pixels")?;
    image.save(path)?;
    Ok(())
}
