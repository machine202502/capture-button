//! Persistent settings in the Windows registry (HKCU).

use platform_core::i18n::wide;
use windows::core::{w, PCWSTR};
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::Registry::*;

pub use platform_core::runtime::test_mode;

pub struct Registry {
    key: HKEY,
}

impl Registry {
    pub fn open() -> Option<Self> {
        let mut key = HKEY::default();
        let path = if test_mode() {
            w!("Software\\CaptureButton-test")
        } else {
            w!("Software\\CaptureButton")
        };
        unsafe {
            // SAFETY: The key is HKCU and the subkey is NUL-terminated. The returned HKEY is closed by its owner.

            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                path,
                0,
                None,
                REG_OPTION_NON_VOLATILE,
                KEY_READ | KEY_WRITE,
                None,
                &mut key,
                None,
            )
        }
        .ok()
        .ok()?;
        Some(Self { key })
    }

    pub fn dword(&self, name: &str, default: u32) -> u32 {
        let name = wide(name);
        let mut value = default;
        let mut size = 4u32;
        unsafe {
            // SAFETY: The key is open, the name is NUL-terminated, and the buffer length is the size passed in.

            RegQueryValueExW(
                self.key,
                PCWSTR(name.as_ptr()),
                None,
                None,
                Some((&mut value as *mut u32).cast()),
                Some(&mut size),
            )
        }
        .ok();
        value
    }

    pub fn set_dword(&self, name: &str, value: u32) {
        let name = wide(name);
        let bytes = value.to_le_bytes();
        unsafe {
            // SAFETY: The key is open, the name is NUL-terminated, and the buffer length is the size passed in.
            RegSetValueExW(self.key, PCWSTR(name.as_ptr()), 0, REG_DWORD, Some(&bytes))
        }
        .ok();
    }

    pub fn string(&self, name: &str, default: &str) -> String {
        let name = wide(name);
        let mut size = 0u32;
        if unsafe {
            // SAFETY: The key is open, the name is NUL-terminated, and the buffer length is the size passed in.

            RegQueryValueExW(
                self.key,
                PCWSTR(name.as_ptr()),
                None,
                None,
                None,
                Some(&mut size),
            )
        }
        .is_err()
            || size < 2
        {
            return default.into();
        }
        // A REG_SZ is UTF-16. Read it in a bounded loop instead of trusting one
        // fixed clamp: the history value can legitimately exceed 4 KiB, and a
        // value written between the size query and the read reports MORE_DATA.
        const MAX_BYTES: u32 = 1_048_576;
        let mut size = size.min(MAX_BYTES);
        for _ in 0..3 {
            let capacity = size.clamp(2, MAX_BYTES) & !1;
            if capacity < 2 {
                return default.into();
            }
            let mut data = vec![0u16; capacity as usize / 2];
            let mut actual = capacity;
            let status = unsafe {
                // SAFETY: The buffer is exactly `capacity` bytes and `actual`
                // carries that same length into the call.
                RegQueryValueExW(
                    self.key,
                    PCWSTR(name.as_ptr()),
                    None,
                    None,
                    Some(data.as_mut_ptr().cast()),
                    Some(&mut actual),
                )
            };
            if status.is_ok() {
                let used = (actual.min(capacity) as usize / 2).min(data.len());
                return String::from_utf16_lossy(&data[..used])
                    .trim_end_matches('\0')
                    .into();
            }
            if status == windows::Win32::Foundation::ERROR_MORE_DATA && actual > capacity {
                size = actual.min(MAX_BYTES);
                continue;
            }
            return default.into();
        }
        default.into()
    }

    pub fn set_string(&self, name: &str, value: &str) {
        let name = wide(name);
        let data = wide(value);
        let bytes = unsafe {
            // SAFETY: The pointer addresses a live buffer, and the length is exactly that buffer's size.
            std::slice::from_raw_parts(data.as_ptr().cast::<u8>(), data.len() * 2)
        };
        unsafe {
            // SAFETY: The key is open. The name and bytes outlive the call and match the declared size.
            RegSetValueExW(self.key, PCWSTR(name.as_ptr()), 0, REG_SZ, Some(bytes))
        }
        .ok();
    }
}

impl Drop for Registry {
    fn drop(&mut self) {
        unsafe {
            // SAFETY: This HKEY was opened above and is closed once.
            RegCloseKey(self.key)
        }
        .ok();
    }
}

pub fn update_autostart(enabled: bool, minimized: bool) {
    if test_mode() {
        return;
    }
    let mut key = HKEY::default();
    if unsafe {
        // SAFETY: The key is HKCU and the subkey is NUL-terminated. The returned HKEY is closed before this function returns.
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
            0,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_READ | KEY_WRITE,
            None,
            &mut key,
            None,
        )
    }
    .is_err()
    {
        return;
    }
    if enabled {
        let mut path = vec![0u16; 32768];
        let len = unsafe {
            // SAFETY: The pointer addresses a live buffer, and the length is exactly that buffer's size.
            GetModuleFileNameW(None, &mut path)
        } as usize;
        let suffix = if minimized { " --minimized" } else { "" };
        let value = format!("\"{}\"{suffix}", String::from_utf16_lossy(&path[..len]));
        let data = wide(&value);
        let bytes = unsafe {
            // SAFETY: The pointer addresses a live buffer, and the length is exactly that buffer's size.
            std::slice::from_raw_parts(data.as_ptr().cast::<u8>(), data.len() * 2)
        };
        unsafe {
            // SAFETY: The key is open. The name and bytes outlive the call and match the declared size.
            RegSetValueExW(key, w!("CaptureButton"), 0, REG_SZ, Some(bytes))
        }
        .ok();
    } else {
        unsafe {
            // SAFETY: The key is open and the value name is a NUL-terminated literal.
            RegDeleteValueW(key, w!("CaptureButton"))
        }
        .ok();
    }
    unsafe {
        // SAFETY: This HKEY was opened above and is closed once.
        RegCloseKey(key)
    }
    .ok();
}

#[cfg(test)]
mod tests {
    use super::Registry;

    #[test]
    fn a_long_registry_string_is_read_past_the_old_four_kib_clamp() {
        let Some(registry) = Registry::open() else {
            return;
        };
        let value = "x".repeat(5_000);
        registry.set_string("long_value", &value);
        assert_eq!(registry.string("long_value", ""), value);
        // Leave no test payload behind in HKCU.
        registry.set_string("long_value", "");
    }
}
