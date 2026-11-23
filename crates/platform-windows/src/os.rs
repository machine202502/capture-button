//! Windows OS services: screen grabs, clipboard, shell actions, sounds, output
//! folders and microphone enumeration.

use platform_core::types::{AudioDevice, Frame, OutputCategory, OutputFolders, Rect};
use std::path::{Path, PathBuf};
use std::time::Duration;
use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::Memory::*;
use windows::Win32::System::Ole::CF_HDROP;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

/// The user's local wall-clock time.
pub fn local_time() -> platform_core::time::LocalTime {
    let time = unsafe {
        // SAFETY: GetLocalTime fills a plain SYSTEMTIME value.
        windows::Win32::System::SystemInformation::GetLocalTime()
    };
    platform_core::time::LocalTime {
        year: time.wYear,
        month: time.wMonth as u8,
        day: time.wDay as u8,
        hour: time.wHour as u8,
        minute: time.wMinute as u8,
        second: time.wSecond as u8,
        millis: time.wMilliseconds,
    }
}

/// `%LOCALAPPDATA%\CaptureButton\logs`, where the application log lives.
pub fn log_dir() -> PathBuf {
    let root = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    root.join("CaptureButton").join("logs")
}

/// Opens the log folder in Explorer.
pub fn open_log_dir() {
    let _ = std::fs::create_dir_all(log_dir());
    let _ = std::process::Command::new("explorer.exe")
        .arg(log_dir())
        .spawn();
}

pub fn cursor_monitor_rect() -> Rect {    // SAFETY: The POINT and MONITORINFO are writable locals. cbSize is set before GetMonitorInfoW.
    unsafe {
        let mut point = POINT::default();
        GetCursorPos(&mut point).ok();
        let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        GetMonitorInfoW(monitor, &mut info).ok();
        let r = info.rcMonitor;
        Rect {
            x: r.left,
            y: r.top,
            w: (r.right - r.left) as u32,
            h: (r.bottom - r.top) as u32,
        }
    }
}

pub fn grab(rect: Rect) -> Result<Frame, String> {
    grab_screen(rect, true)
}

/// Screen pixels for a still. The pointer is not drawn into the frame.
pub fn grab_without_cursor(rect: Rect) -> Result<Frame, String> {
    grab_screen(rect, false)
}

fn grab_screen(rect: Rect, draw_cursor: bool) -> Result<Frame, String> {
    // SAFETY: The screen DC is released before return. The bitmap header size is set, and the copy uses only the requested byte count.
    unsafe {
        let screen = GetDC(None);
        if screen.is_invalid() {
            return Err("GetDC failed".into());
        }
        let memory = CreateCompatibleDC(screen);
        let mut bitmap_info: BITMAPINFO = std::mem::zeroed();
        bitmap_info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bitmap_info.bmiHeader.biWidth = rect.w as i32;
        bitmap_info.bmiHeader.biHeight = -(rect.h as i32);
        bitmap_info.bmiHeader.biPlanes = 1;
        bitmap_info.bmiHeader.biBitCount = 32;
        bitmap_info.bmiHeader.biCompression = BI_RGB.0;
        let mut bits = std::ptr::null_mut();
        let bitmap = CreateDIBSection(screen, &bitmap_info, DIB_RGB_COLORS, &mut bits, None, 0)
            .map_err(|e| e.to_string())?;
        let old = SelectObject(memory, bitmap);
        // CAPTUREBLT is left out on purpose: while the recording fallback calls
        // BitBlt every frame, it makes the live pointer flicker on some systems.
        // The pointer is drawn below. A layered window can be missing from this
        // fallback frame; the normal Windows Graphics Capture frames still have it.
        BitBlt(
            memory,
            0,
            0,
            rect.w as i32,
            rect.h as i32,
            screen,
            rect.x,
            rect.y,
            SRCCOPY,
        )
        .map_err(|e| e.to_string())?;
        let len = rect.w as usize * rect.h as usize * 4;
        let mut data = vec![0u8; len];
        std::ptr::copy_nonoverlapping(bits.cast::<u8>(), data.as_mut_ptr(), len);
        let mut cursor = CURSORINFO {
            cbSize: std::mem::size_of::<CURSORINFO>() as u32,
            ..Default::default()
        };
        if draw_cursor && GetCursorInfo(&mut cursor).is_ok() && cursor.flags == CURSOR_SHOWING {
            let mut icon_info = ICONINFO::default();
            if GetIconInfo(cursor.hCursor, &mut icon_info).is_ok() {
                let x = cursor.ptScreenPos.x - rect.x - icon_info.xHotspot as i32;
                let y = cursor.ptScreenPos.y - rect.y - icon_info.yHotspot as i32;
                DrawIconEx(memory, x, y, cursor.hCursor, 0, 0, 0, None, DI_NORMAL).ok();
                if !icon_info.hbmColor.is_invalid() {
                    DeleteObject(icon_info.hbmColor);
                }
                if !icon_info.hbmMask.is_invalid() {
                    DeleteObject(icon_info.hbmMask);
                }
                std::ptr::copy_nonoverlapping(bits.cast::<u8>(), data.as_mut_ptr(), len);
            }
        }
        SelectObject(memory, old);
        DeleteObject(bitmap);
        DeleteDC(memory);
        ReleaseDC(None, screen);
        Ok(Frame {
            width: rect.w,
            height: rect.h,
            data,
        })
    }
}

/// Applies the folder settings loaded at startup or changed in the window.
pub fn set_output_folders(folders: &OutputFolders) {
    platform_core::paths::set_folders(folders);
}

/// The folder where screenshots and recordings are written by default.
pub fn output_dir() -> Result<PathBuf, String> {
    // SAFETY: The folder path from the shell is freed with CoTaskMemFree before this function returns. Only the characters up to the NUL are read.
    unsafe {
        let raw = SHGetKnownFolderPath(&FOLDERID_Documents, KNOWN_FOLDER_FLAG(0), None)
            .map_err(|e| e.to_string())?;
        let mut len = 0;
        while *raw.0.add(len) != 0 {
            len += 1;
        }
        let root = String::from_utf16_lossy(std::slice::from_raw_parts(raw.0, len));
        CoTaskMemFree(Some(raw.0.cast()));
        let dir = PathBuf::from(root).join("CaptureButton");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        Ok(dir)
    }
}

/// The folder a file of `category` belongs in: its own folder when chosen, the
/// default folder when only that one was moved, or the built-in folder.
pub fn output_dir_for(category: OutputCategory) -> Result<PathBuf, String> {
    match platform_core::paths::configured(category) {
        Some(dir) => platform_core::paths::ensure_dir(&dir),
        None => output_dir(),
    }
}

pub fn output_path(
    category: OutputCategory,
    kind: &str,
    extension: &str,
) -> Result<PathBuf, String> {
    let dir = output_dir_for(category)?;
    platform_core::paths::unique_path(&dir, kind, extension, local_time())
}
/// Closes the clipboard on every exit path, including an early `?`.
struct Clipboard;

/// `OpenClipboard` does not wait. While another process (an RDP client or a
/// clipboard manager) holds the clipboard open, it fails with
/// `ERROR_ACCESS_DENIED`; retrying is the Win32 way to wait for that lock.
const CLIPBOARD_ATTEMPTS: usize = 10;
const CLIPBOARD_RETRY_DELAY: Duration = Duration::from_millis(20);

impl Clipboard {
    fn open() -> Result<Self, String> {
        unsafe {
            // SAFETY: The clipboard is opened on this thread and closed by Drop.
            OpenClipboard(None).map_err(|error| error.to_string())?;
        }
        Ok(Self)
    }

    fn open_with_retry() -> Result<Self, String> {
        let mut last = String::new();
        for attempt in 0..CLIPBOARD_ATTEMPTS {
            match Self::open() {
                Ok(clipboard) => return Ok(clipboard),
                Err(error) => {
                    last = error;
                    if attempt + 1 < CLIPBOARD_ATTEMPTS {
                        std::thread::sleep(CLIPBOARD_RETRY_DELAY);
                    }
                }
            }
        }
        Err(last)
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        unsafe {
            // SAFETY: This guard exists only after OpenClipboard succeeded.
            CloseClipboard().ok();
        }
    }
}

pub fn copy_text_to_clipboard(text: &str) -> Result<(), String> {
    // SAFETY: The guard owns the clipboard. The allocated block matches the
    // UTF-16 text and is handed to the clipboard; on failure it is freed here.
    unsafe {
        let _clipboard = Clipboard::open()?;
        EmptyClipboard().map_err(|e| e.to_string())?;
        let wide = text
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let bytes = wide.len() * 2;
        let memory = GlobalAlloc(GMEM_MOVEABLE, bytes).map_err(|e| e.to_string())?;
        let pointer = GlobalLock(memory) as *mut u16;
        if pointer.is_null() {
            GlobalFree(memory);
            return Err("Cannot lock the clipboard memory".into());
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), pointer, wide.len());
        GlobalUnlock(memory).ok();
        if let Err(error) = SetClipboardData(13, HANDLE(memory.0)) {
            GlobalFree(memory);
            return Err(error.to_string());
        }
        Ok(())
    }
}

pub fn open_file(path: &Path) -> Result<(), String> {
    let verb = "open"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let file = path
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // SAFETY: The verb and path are NUL-terminated and live until ShellExecuteW returns. The verb open uses the file's associated program.
    let code = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(file.as_ptr()),
            None,
            None,
            SW_SHOWNORMAL,
        )
        .0 as isize
    };
    if code > 32 {
        Ok(())
    } else {
        Err("Cannot open the file".into())
    }
}

pub fn reveal_in_folder(path: &Path) -> Result<(), String> {
    let verb = "open"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let program = "explorer.exe"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let params = format!("/select,\"{}\"", path.display())
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // SAFETY: The verb, program, and parameters are NUL-terminated and live until ShellExecuteW returns.
    let code = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(program.as_ptr()),
            PCWSTR(params.as_ptr()),
            None,
            SW_SHOWNORMAL,
        )
        .0 as isize
    };
    if code > 32 {
        Ok(())
    } else {
        Err("Cannot open the file's folder".into())
    }
}

/// Writes a DROPFILES block for one file into `bytes` bytes of writable memory.
/// `path` includes its UTF-16 NUL. The block ends with a full UTF-16 NUL so a
/// parser stops after exactly one file name.
fn write_file_drop(pointer: *mut u8, bytes: usize, path: &[u16]) {
    unsafe {
        // SAFETY: The caller passes a writable block of at least `bytes` bytes, and
        // `path` is a live UTF-16 buffer. Every write stays inside that block.
        let drop = DROPFILES {
            pFiles: std::mem::size_of::<DROPFILES>() as u32,
            fWide: true.into(),
            ..Default::default()
        };
        std::ptr::write(pointer.cast::<DROPFILES>(), drop);
        std::ptr::copy_nonoverlapping(
            path.as_ptr(),
            pointer.add(std::mem::size_of::<DROPFILES>()).cast::<u16>(),
            path.len(),
        );
        std::ptr::write(pointer.add(bytes - 2).cast::<u16>(), 0u16);
    }
}

pub fn copy_file_to_clipboard(path: &Path) -> Result<(), String> {
    // SAFETY: The guard owns the clipboard. The drop list is sized to the path
    // and handed to the clipboard; on failure its block is freed here.
    unsafe {
        let _clipboard = Clipboard::open_with_retry()?;
        EmptyClipboard().map_err(|e| e.to_string())?;
        let path = path
            .to_string_lossy()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let bytes = std::mem::size_of::<DROPFILES>() + (path.len() + 1) * 2;
        let memory = GlobalAlloc(GMEM_MOVEABLE, bytes).map_err(|e| e.to_string())?;
        let pointer = GlobalLock(memory) as *mut u8;
        if pointer.is_null() {
            GlobalFree(memory);
            return Err("Cannot lock the clipboard memory".into());
        }
        write_file_drop(pointer, bytes, &path);
        GlobalUnlock(memory).ok();
        if let Err(error) = SetClipboardData(CF_HDROP.0 as u32, HANDLE(memory.0)) {
            GlobalFree(memory);
            return Err(error.to_string());
        }
        Ok(())
    }
}

/// The single shutter sound the app plays for screenshots and camera photos.
pub fn screenshot_sound(volume: u32) {
    static SOUND: &[u8] = include_bytes!("../../../assets/shutter.wav");
    play_sound(SOUND, volume);
}

pub fn completion_sound(volume: u32) {
    static SOUND: &[u8] = include_bytes!("../../../assets/recording-saved.wav");
    play_sound(SOUND, volume);
}

/// Play an embedded WAV at `volume` percent. `PlaySoundW` has no volume
/// parameter, so the PCM body is copied and scaled before playback. The copy
/// lives on the playing thread for the whole synchronous call.
fn play_sound(wav: &[u8], volume: u32) {
    let data = wav_at_volume(wav, volume.min(100));
    std::thread::Builder::new()
        .name("CaptureButton-sound".into())
        .spawn(move || {
            // SAFETY: `data` holds the complete WAV image and outlives the
            // synchronous call; SND_MEMORY reads only that buffer.
            unsafe {
                windows::Win32::Media::Audio::PlaySoundW(
                    PCWSTR(data.as_ptr().cast()),
                    None,
                    windows::Win32::Media::Audio::SND_MEMORY
                        | windows::Win32::Media::Audio::SND_SYNC
                        | windows::Win32::Media::Audio::SND_NODEFAULT,
                )
                .ok();
            }
        })
        .ok();
}

/// A copy of `wav` with its 16-bit PCM body scaled to `volume` percent. Any
/// other encoding, or a malformed header, is returned unchanged.
fn wav_at_volume(wav: &[u8], volume: u32) -> Vec<u8> {
    let mut data = wav.to_vec();
    if volume >= 100 {
        return data;
    }
    let Some((start, length)) = wav_pcm16(wav) else {
        return data;
    };
    let end = start + length;
    for sample in data[start..end].as_chunks_mut::<2>().0 {
        let value = i16::from_le_bytes([sample[0], sample[1]]) as i32;
        sample.copy_from_slice(&((value * volume as i32 / 100) as i16).to_le_bytes());
    }
    data
}

/// Locate the 16-bit PCM samples in a RIFF/WAVE image.
fn wav_pcm16(wav: &[u8]) -> Option<(usize, usize)> {
    if wav.len() < 12 || &wav[..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
        return None;
    }
    let mut offset = 12;
    let mut bits = 0u16;
    let mut pcm = false;
    while offset + 8 <= wav.len() {
        let id = &wav[offset..offset + 4];
        let length =
            u32::from_le_bytes([wav[offset + 4], wav[offset + 5], wav[offset + 6], wav[offset + 7]])
                as usize;
        let body = offset + 8;
        if body + length > wav.len() {
            return None;
        }
        match id {
            b"fmt " if length >= 16 => {
                let tag = u16::from_le_bytes([wav[body], wav[body + 1]]);
                bits = u16::from_le_bytes([wav[body + 14], wav[body + 15]]);
                pcm = tag == 1;
            }
            b"data" if pcm && bits == 16 => return Some((body, length)),
            _ => {}
        }
        offset = body + length + (length & 1);
    }
    None
}

fn device_enumerator() -> Result<windows::Win32::Media::Audio::IMMDeviceEnumerator, String> {
    // SAFETY: The COM object is released when this Result is dropped.
    unsafe {
        windows::Win32::System::Com::CoCreateInstance(
            &windows::Win32::Media::Audio::MMDeviceEnumerator,
            None,
            windows::Win32::System::Com::CLSCTX_ALL,
        )
        .map_err(|e| e.to_string())
    }
}

fn pwstr(value: windows::core::PWSTR) -> String {
    // SAFETY: A null pointer returns before any read. Otherwise only the characters up to the NUL are copied.
    unsafe {
        if value.is_null() {
            return String::new();
        }
        let mut len = 0;
        while *value.0.add(len) != 0 {
            len += 1;
        }
        let text = String::from_utf16_lossy(std::slice::from_raw_parts(value.0, len));
        CoTaskMemFree(Some(value.0.cast()));
        text
    }
}

fn endpoint_name(device: &windows::Win32::Media::Audio::IMMDevice) -> String {
    // SAFETY: The device is alive. The allocated friendly name is freed with CoTaskMemFree before return.
    unsafe {
        use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
        use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
        let Ok(store) = device.OpenPropertyStore(windows::Win32::System::Com::STGM_READ) else {
            return "Microphone".into();
        };
        let Ok(value) = store.GetValue(&PKEY_Device_FriendlyName) else {
            return "Microphone".into();
        };
        let Ok(text) = PropVariantToStringAlloc(&value) else {
            return "Microphone".into();
        };
        pwstr(text)
    }
}

pub fn microphones() -> Vec<AudioDevice> {
    (|| {
        let enumerator = device_enumerator().ok()?;
        // SAFETY: The enumerator is alive. The returned collection is released when dropped.
        let collection = unsafe {
            enumerator.EnumAudioEndpoints(
                windows::Win32::Media::Audio::eCapture,
                windows::Win32::Media::Audio::DEVICE_STATE_ACTIVE,
            )
        }
        .ok()?;
        // SAFETY: The endpoint collection is alive for this call.
        let count = unsafe { collection.GetCount() }.ok()?;
        let mut result = Vec::new();
        for index in 0..count {
            // SAFETY: index is below the count this collection just returned.
            let Ok(device) = (unsafe { collection.Item(index) }) else {
                continue;
            };
            // SAFETY: The device came from this live collection.
            let Ok(id) = (unsafe { device.GetId() }) else {
                continue;
            };
            result.push(AudioDevice {
                id: pwstr(id),
                name: endpoint_name(&device),
            });
        }
        Some(result)
    })()
    .unwrap_or_default()
}

pub fn device_name(id: &str) -> Option<String> {
    microphones()
        .into_iter()
        .find(|d| d.id == id)
        .map(|d| d.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav_with_samples(samples: &[i16]) -> Vec<u8> {
        let data_length = (samples.len() * 2) as u32;
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_length).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&44100u32.to_le_bytes());
        wav.extend_from_slice(&88200u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_length.to_le_bytes());
        for sample in samples {
            wav.extend_from_slice(&sample.to_le_bytes());
        }
        wav
    }

    #[test]
    fn playback_volume_scales_only_the_pcm_samples() {
        let wav = wav_with_samples(&[1000, -2000, 32767, -32768]);
        let half = wav_at_volume(&wav, 50);
        assert_eq!(&half[..44], &wav[..44], "the WAV header stays untouched");
        let scaled: Vec<i16> = half[44..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|sample| i16::from_le_bytes(*sample))
            .collect();
        assert_eq!(scaled, [500, -1000, 16383, -16384]);
        assert_eq!(wav_at_volume(&wav, 100), wav);
        assert!(wav_at_volume(&wav, 0)[44..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn every_embedded_sound_is_a_16_bit_pcm_wav() {
        let (start, length) =
            wav_pcm16(include_bytes!("../../../assets/shutter.wav")).expect("shutter is PCM");
        assert!(start >= 44 && length > 2000, "{length} bytes of samples");
        let (_, length) = wav_pcm16(include_bytes!("../../../assets/recording-saved.wav"))
            .expect("completion asset is 16-bit PCM");
        assert!(length > 40_000);
    }

    #[test]
    fn a_copied_file_list_ends_with_a_real_utf16_nul() {
        use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
        let name = r"C:\Pictures\CaptureButton\Shot.png";
        let path = name
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let bytes = std::mem::size_of::<DROPFILES>() + (path.len() + 1) * 2;
        // A non-zero fill stands in for the bytes GlobalAlloc hands back. The last
        // UTF-16 unit must be written completely; a one-byte write leaves a stray
        // high byte that becomes a second, garbage file name.
        let mut block = vec![0xFFu8; bytes];
        write_file_drop(block.as_mut_ptr(), bytes, &path);
        assert_eq!(
            &block[bytes - 2..],
            &[0, 0],
            "the last UTF-16 unit of the file list was only half written"
        );
        let (count, parsed) = unsafe {
            // SAFETY: The block is copied byte for byte into a global block of the same size, which DragQueryFileW reads and this test frees.
            let memory = GlobalAlloc(GMEM_MOVEABLE, bytes).unwrap();
            let pointer = GlobalLock(memory) as *mut u8;
            std::ptr::copy_nonoverlapping(block.as_ptr(), pointer, bytes);
            GlobalUnlock(memory).ok();
            let count = DragQueryFileW(HDROP(memory.0), u32::MAX, None);
            let mut buffer = vec![0u16; path.len() + 8];
            let len = DragQueryFileW(HDROP(memory.0), 0, Some(&mut buffer));
            let parsed = String::from_utf16_lossy(&buffer[..len as usize]);
            GlobalFree(memory).ok();
            (count, parsed)
        };
        assert_eq!(count, 1, "the shell sees more than one file");
        assert_eq!(parsed, name);
    }
}
