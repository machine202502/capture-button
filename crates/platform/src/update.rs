//! App updates through Velopack, reported to the app as [`UpdateState`].
//!
//! The checker and downloader run off the UI thread. Their only contact with
//! the window is the `on_state` callback.

use crate::logging;
use crate::store::test_mode;
use crate::types::UpdateState;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};
use velopack::sources::HttpSource;
use velopack::{UpdateCheck, UpdateInfo, UpdateManager, VelopackApp};

/// The feed is served by the same GitHub Pages deployment as the site.
const UPDATE_FEED: &str = "https://machine202502.github.io/capture-button/updates";

type StateCallback = Box<dyn Fn(UpdateState) + Send + Sync>;

fn available() -> &'static Mutex<Option<UpdateInfo>> {
    static AVAILABLE: OnceLock<Mutex<Option<UpdateInfo>>> = OnceLock::new();
    AVAILABLE.get_or_init(|| Mutex::new(None))
}

fn busy() -> &'static AtomicBool {
    static BUSY: AtomicBool = AtomicBool::new(false);
    &BUSY
}

fn feed() -> String {
    if test_mode() {
        if let Ok(url) = std::env::var("CAPTURE_BUTTON_UPDATE_FEED") {
            if !url.trim().is_empty() {
                return url;
            }
        }
    }
    UPDATE_FEED.to_string()
}

fn manager() -> Result<UpdateManager, velopack::Error> {
    UpdateManager::new(HttpSource::new(feed()), None, None)
}

fn take_available() -> Option<UpdateInfo> {
    available()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}

fn set_available(info: Option<UpdateInfo>) {
    *available()
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = info;
}

// A check returns this once and the value is matched away immediately, so
// boxing the update info would add an allocation without saving anything.
#[allow(clippy::large_enum_variant)]
enum Checked {
    Update(UpdateInfo),
    Current,
    Unsupported,
}

/// Called when the About window opens. A check already running or a found
/// update is left alone; otherwise the feed is queried in the background.
pub fn check(on_state: StateCallback) {
    if test_mode() && std::env::var_os("CAPTURE_BUTTON_UPDATE_FEED").is_none() {
        on_state(UpdateState::Idle);
        return;
    }
    if busy().swap(true, Ordering::SeqCst) {
        return;
    }
    if take_available().is_some() {
        busy().store(false, Ordering::SeqCst);
        return;
    }

    on_state(UpdateState::Checking);
    std::thread::spawn(move || {
        let result = (|| -> Result<Checked, String> {
            let manager = match manager() {
                Ok(manager) => manager,
                Err(velopack::Error::NotInstalled(_)) | Err(velopack::Error::NotSupported(_)) => {
                    return Ok(Checked::Unsupported);
                }
                Err(error) => return Err(error.to_string()),
            };
            if manager.get_is_portable() {
                return Ok(Checked::Unsupported);
            }
            match manager
                .check_for_updates()
                .map_err(|error| error.to_string())?
            {
                UpdateCheck::UpdateAvailable(info) => Ok(Checked::Update(*info)),
                UpdateCheck::NoUpdateAvailable | UpdateCheck::RemoteIsEmpty => Ok(Checked::Current),
            }
        })();

        busy().store(false, Ordering::SeqCst);
        match result {
            Ok(Checked::Update(info)) => {
                let version = info.TargetFullRelease.Version.clone();
                logging::info(format!("update available: {version}"));
                set_available(Some(info));
                on_state(UpdateState::Available);
            }
            Ok(Checked::Current) => {
                logging::info("no update available");
                set_available(None);
                on_state(UpdateState::UpToDate);
            }
            Ok(Checked::Unsupported) => {
                logging::info("updates are not supported for this installation");
                set_available(None);
                on_state(UpdateState::Unsupported);
            }
            Err(error) => {
                set_available(None);
                logging::warn(format!("update check failed: {error}"));
                on_state(UpdateState::Error);
            }
        }
    });
}

/// Called when the green update button is pressed. Downloads the update found
/// by `check`, then hands control to Velopack, which exits and restarts the app.
pub fn install(on_state: StateCallback) {
    if busy().swap(true, Ordering::SeqCst) {
        return;
    }
    let Some(info) = take_available() else {
        busy().store(false, Ordering::SeqCst);
        check(on_state);
        return;
    };

    let on_state = std::sync::Arc::new(on_state);
    let version = info.TargetFullRelease.Version.clone();
    on_state(UpdateState::Downloading(0));
    std::thread::spawn(move || {
        let result = (|| -> Result<UpdateManager, String> {
            let manager = manager().map_err(|error| error.to_string())?;
            let (progress_tx, progress_rx) = mpsc::channel::<i16>();
            let progress_controls = std::sync::Arc::clone(&on_state);
            let progress_thread = std::thread::spawn(move || {
                while let Ok(percent) = progress_rx.recv() {
                    let percent = percent.clamp(0, 100) as u8;
                    progress_controls(UpdateState::Downloading(percent));
                }
            });

            let download = manager
                .download_updates(&info, Some(progress_tx))
                .map_err(|error| error.to_string());
            let _ = progress_thread.join();
            download?;
            Ok(manager)
        })();

        match result {
            Ok(manager) => {
                logging::info(format!("update {version} downloaded; restarting"));
                on_state(UpdateState::Installing);
                if let Err(error) = manager.apply_updates_and_restart(&info) {
                    logging::warn(format!("update install failed: {error}"));
                    busy().store(false, Ordering::SeqCst);
                    on_state(UpdateState::Error);
                }
            }
            Err(error) => {
                logging::warn(format!("update download failed: {error}"));
                busy().store(false, Ordering::SeqCst);
                on_state(UpdateState::Error);
            }
        }
    });
}

/// Velopack must see its own installation arguments before the window exists.
pub fn init() {
    if !test_mode() {
        VelopackApp::build().run();
    }
}
