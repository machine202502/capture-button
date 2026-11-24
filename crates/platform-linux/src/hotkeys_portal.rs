//! Global hotkeys through the XDG desktop portal's `GlobalShortcuts` API.
//!
//! Wayland has no global key grabs, so on such a session the desktop owns the
//! combinations: the portal asks the user in its own dialog, stores the answer
//! per application id, and reports every press back. This module is the portal
//! side of that contract:
//!
//! - [`available`] probes once whether this session has a usable portal, which
//!   is what selects `HotkeyChoice::SystemDialog` over the X11 fallback;
//! - [`bind`] applies the whole action table in one `BindShortcuts` call. A
//!   portal session accepts exactly one bind, and the call *replaces* the
//!   application's set instead of merging into it, so every change — a clear,
//!   a change, a start — rebuilds the complete set;
//! - a worker thread owns the live session, listens for `Activated` and hands
//!   each press to the application as `ShellEvent::Hotkey`.
//!
//! The application id is the installed menu entry's name without `.desktop`
//! (`capture-button.desktop` -> `capture-button`): the portal resolves the app
//! info through it, so [`crate::desktop::install`] has to have written the
//! entry before the id is registered. [`probe`] does that itself, which is why
//! a start from the source tree works without a prior installation.

use ashpd::desktop::global_shortcuts::{
    Activated, BindShortcutsOptions, GlobalShortcuts, NewShortcut,
};
use ashpd::desktop::{CreateSessionOptions, Session};
use ashpd::zbus;
use futures_util::StreamExt;
use platform_core::i18n::{tr, trf};
use platform_core::logging;
use platform_core::tray::{
    hotkey_by_id, hotkey_defs, HotkeyAction, HotkeyBinding, HotkeyEvent, ShellEvent,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crate::shell::enqueue;

/// How long the one-time probe may take before the session is treated as
/// having no portal. A hung portal must not keep the window from opening.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// The probe runs once per process; `UNKNOWN` is the state before that.
const UNKNOWN: u8 = 0;
const USABLE: u8 = 1;
const DEAD: u8 = 2;

static STATE: AtomicU8 = AtomicU8::new(UNKNOWN);

/// Set while a bind is in flight, so a second request cannot stack a second
/// system dialog on the first one.
static BUSY: AtomicBool = AtomicBool::new(false);

/// One command for the worker thread.
enum Command {
    /// Answer whether this session has a portal that can bind shortcuts.
    Probe(Sender<bool>),
    /// Rebuild the set. `ask_again` names the action the user wants to change,
    /// which the portal only asks about when the id is not already bound.
    Bind {
        bindings: Vec<HotkeyBinding>,
        ask_again: Option<HotkeyAction>,
    },
}

/// The live portal session: the object that keeps the bindings active.
struct Live {
    session: Session<GlobalShortcuts>,
    events: tokio::task::JoinHandle<()>,
}

impl Live {
    /// Ends the session, which also stops its `Activated` stream.
    async fn stop(self) {
        self.events.abort();
        let _ = self.session.close().await;
    }
}

/// Why a bind could not be applied.
enum BindFailure {
    /// The user closed the desktop's dialog without accepting.
    Cancelled,
    /// The portal cannot provide global shortcuts on this session after all.
    Unavailable(String),
    /// The request failed for another reason.
    Failed(String),
}

/// Whether this session has a usable `GlobalShortcuts` portal.
///
/// The answer is cached for the process: a portal that was absent at start-up
/// does not appear later, and one that failed is the fallback's job from then
/// on.
pub fn available() -> bool {
    match STATE.load(Ordering::SeqCst) {
        USABLE => return true,
        DEAD => return false,
        _ => {}
    }
    // The probe is a D-Bus round trip; the mutex keeps two callers from making
    // it twice and from overwriting the state each other wrote.
    static PROBING: Mutex<()> = Mutex::new(());
    let _guard = PROBING.lock().unwrap_or_else(|error| error.into_inner());
    match STATE.load(Ordering::SeqCst) {
        USABLE => return true,
        DEAD => return false,
        _ => {}
    }
    let usable = probe_portal();
    STATE.store(if usable { USABLE } else { DEAD }, Ordering::SeqCst);
    if !usable {
        logging::info("hotkeys: no GlobalShortcuts portal; using the X11 fallback");
    }
    usable
}

/// Marks the portal unusable, so the next registration takes the fallback.
fn disable() {
    STATE.store(DEAD, Ordering::SeqCst);
}

fn probe_portal() -> bool {
    let (reply, answer) = channel();
    let Some(worker) = worker() else {
        return false;
    };
    if worker.send(Command::Probe(reply)).is_err() {
        return false;
    }
    answer.recv_timeout(PROBE_TIMEOUT).unwrap_or(false)
}

/// Applies `bindings` through the portal. A system dialog may follow; the
/// answer arrives as a `ShellEvent::Hotkeys`.
pub fn bind(bindings: &[HotkeyBinding], ask_again: Option<HotkeyAction>) -> Result<(), String> {
    let Some(worker) = worker() else {
        disable();
        return Err(tr("The desktop portal is not running"));
    };
    if BUSY.swap(true, Ordering::SeqCst) {
        return Err(tr("The system dialog is already open"));
    }
    let command = Command::Bind {
        bindings: bindings.to_vec(),
        ask_again,
    };
    if worker.send(command).is_err() {
        BUSY.store(false, Ordering::SeqCst);
        disable();
        return Err(tr("The desktop portal is not running"));
    }
    Ok(())
}

/// Hands an event to the application. A terminal event releases the busy flag
/// set by [`bind`].
fn notify(event: HotkeyEvent) {
    if !matches!(event, HotkeyEvent::Pending) {
        BUSY.store(false, Ordering::SeqCst);
    }
    enqueue(ShellEvent::Hotkeys(event));
}

fn worker() -> Option<&'static tokio::sync::mpsc::UnboundedSender<Command>> {
    static WORKER: OnceLock<Option<tokio::sync::mpsc::UnboundedSender<Command>>> = OnceLock::new();
    WORKER.get_or_init(start_worker).as_ref()
}

fn start_worker() -> Option<tokio::sync::mpsc::UnboundedSender<Command>> {
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    std::thread::Builder::new()
        .name("capture-button-hotkeys-portal".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    logging::warn(format!("hotkeys: cannot start the portal runtime: {error}"));
                    return;
                }
            };
            runtime.block_on(serve(receiver));
        })
        .ok()?;
    Some(commands)
}

/// The worker loop. It owns the D-Bus connection and the live session, so a
/// bind never runs on the thread that draws the window or pumps events.
async fn serve(mut commands: tokio::sync::mpsc::UnboundedReceiver<Command>) {
    let mut connection: Option<zbus::Connection> = None;
    let mut live: Option<Live> = None;
    let mut last_applied: Vec<HotkeyBinding> = Vec::new();
    while let Some(command) = commands.recv().await {
        match command {
            Command::Probe(reply) => {
                let usable = probe(&mut connection).await.is_ok();
                let _ = reply.send(usable);
            }
            Command::Bind {
                bindings,
                ask_again,
            } => {
                handle_bind(
                    &mut connection,
                    &mut live,
                    &mut last_applied,
                    &bindings,
                    ask_again,
                )
                .await;
            }
        }
    }
}

/// The session connection, opened on first use and kept for the process.
async fn session_bus(slot: &mut Option<zbus::Connection>) -> Result<zbus::Connection, String> {
    if let Some(connection) = slot {
        return Ok(connection.clone());
    }
    let connection = zbus::Connection::session()
        .await
        .map_err(|error| format!("no session bus: {error}"))?;
    *slot = Some(connection.clone());
    Ok(connection)
}

/// Checks that this session can bind shortcuts at all: the app id has to be
/// registered (which needs the installed menu entry) and a session has to be
/// creatable. Nothing is bound, so the user sees no dialog.
async fn probe(connection: &mut Option<zbus::Connection>) -> Result<(), String> {
    // The portal resolves the app id through the menu entry, which the desktop
    // integration writes; make sure it is there before asking.
    crate::desktop::install();
    let connection = session_bus(connection).await?;
    register_app(&connection).await?;
    let proxy = GlobalShortcuts::with_connection(connection)
        .await
        .map_err(|error| error.to_string())?;
    if proxy.version() < 1 {
        return Err("the portal has no GlobalShortcuts interface".into());
    }
    let session = proxy
        .create_session(CreateSessionOptions::default())
        .await
        .map_err(|error| error.to_string())?;
    let _ = session.close().await;
    Ok(())
}

/// Registers this process as the owner of our application id.
///
/// `ashpd::register_host_app_with_connection` would be the natural call, but its
/// `AppID` type rejects an id without a dot, and our id is the menu entry's
/// name (`capture-button`), which is what the portal resolves app info
/// through. The portal accepts it — the registry call is the same one.
async fn register_app(connection: &zbus::Connection) -> Result<(), String> {
    let options: HashMap<String, zbus::zvariant::OwnedValue> = HashMap::new();
    connection
        .call_method(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            Some("org.freedesktop.host.portal.Registry"),
            "Register",
            &(crate::desktop::APP_ID, &options),
        )
        .await
        .map_err(|error| format!("the portal rejected the application id: {error}"))?;
    Ok(())
}

/// One bind request and what the desktop did with it.
///
/// The current session is closed before the new bind, because the desktop ties
/// the application's registration to its session: closing a session *after* a
/// newer bind has been accepted drops the new bindings again. A cancelled
/// dialog therefore leaves no session behind, and the caller restores the
/// previous set with [`apply_set`], which is silent for ids the desktop already
/// knows.
async fn apply_set(
    connection: &mut Option<zbus::Connection>,
    live: &mut Option<Live>,
    bindings: &[HotkeyBinding],
) -> Result<Vec<HotkeyBinding>, BindFailure> {
    let connection = session_bus(connection)
        .await
        .map_err(BindFailure::Unavailable)?;
    let proxy = GlobalShortcuts::with_connection(connection)
        .await
        .map_err(classify)?;
    let desired: Vec<&HotkeyBinding> = bindings.iter().filter(|binding| binding.is_set()).collect();
    if let Some(previous) = live.take() {
        previous.stop().await;
    }
    let session = proxy
        .create_session(CreateSessionOptions::default())
        .await
        .map_err(classify)?;
    if desired.is_empty() {
        // Nothing to bind: an empty set is a valid bind and clears whatever the
        // portal still had for this app id.
        let empty = proxy
            .bind_shortcuts(&session, &[], None, BindShortcutsOptions::default())
            .await;
        let empty = match empty {
            Ok(request) => request.response(),
            Err(error) => Err(error),
        };
        let _ = session.close().await;
        if let Err(error) = empty {
            return Err(classify(error));
        }
        return Ok(unbound());
    }
    let activated = proxy.receive_activated().await.map_err(classify)?;
    let mut shortcuts = Vec::with_capacity(desired.len());
    for binding in &desired {
        let def = binding.action.def();
        let mut shortcut = NewShortcut::new(def.id, tr(def.label_key));
        if let Some(trigger) = preferred_trigger(&binding.combination) {
            shortcut = shortcut.preferred_trigger(trigger.as_str());
        }
        shortcuts.push(shortcut);
    }
    let request = proxy
        .bind_shortcuts(&session, &shortcuts, None, BindShortcutsOptions::default())
        .await
        .map_err(classify)?;
    let answer = match request.response() {
        Ok(answer) => answer,
        Err(error) => {
            let _ = session.close().await;
            return Err(classify(error));
        }
    };
    let mut descriptions: HashMap<&str, String> = HashMap::new();
    for shortcut in answer.shortcuts() {
        descriptions.insert(shortcut.id(), shortcut.trigger_description().to_string());
    }
    let applied: Vec<HotkeyBinding> = hotkey_defs()
        .iter()
        .map(|def| HotkeyBinding {
            action: def.action,
            combination: descriptions.get(def.id).cloned().unwrap_or_default(),
        })
        .collect();
    *live = Some(Live {
        session,
        events: spawn_events(activated),
    });
    Ok(applied)
}

/// Reads `Activated` for the live session and forwards the presses.
///
/// `Deactivated` (sent when the key is released) is ignored: the actions here
/// are triggers, not push-to-talk.
fn spawn_events<S>(activated: S) -> tokio::task::JoinHandle<()>
where
    S: futures_util::Stream<Item = Activated> + Send + 'static,
{
    tokio::spawn(async move {
        let mut activated = std::pin::pin!(activated);
        while let Some(event) = activated.next().await {
            if let Some(action) = hotkey_by_id(event.shortcut_id()) {
                logging::info(format!("hotkey portal: activated {}", event.shortcut_id()));
                enqueue(ShellEvent::Hotkey(action));
            }
        }
    })
}

/// Applies the requested set, then reports what the desktop did.
async fn handle_bind(
    connection: &mut Option<zbus::Connection>,
    live: &mut Option<Live>,
    last_applied: &mut Vec<HotkeyBinding>,
    bindings: &[HotkeyBinding],
    ask_again: Option<HotkeyAction>,
) {
    notify(HotkeyEvent::Pending);
    if let Some(action) = ask_again {
        // The portal ignores a `preferred_trigger` for an id it already knows,
        // and a second bind on one session is refused, so the only way to make
        // it ask again is to drop that id first and add it back with the full
        // set. Dropping only that action means a cancelled dialog costs that one
        // row, not the whole set.
        let reduced: Vec<HotkeyBinding> = bindings
            .iter()
            .map(|binding| {
                if binding.action == action {
                    HotkeyBinding::unset(action)
                } else {
                    binding.clone()
                }
            })
            .collect();
        match apply_set(connection, live, &reduced).await {
            Ok(applied) => *last_applied = applied,
            Err(failure) => {
                restore(connection, live, last_applied).await;
                report(failure, last_applied);
                return;
            }
        }
    }
    match apply_set(connection, live, bindings).await {
        Ok(applied) => {
            *last_applied = applied.clone();
            notify(HotkeyEvent::Applied(applied));
        }
        Err(failure) => {
            // The cancelled bind already closed the previous session, so the
            // set that was in effect is bound again here. Its ids are in the
            // desktop's store, which makes this silent.
            restore(connection, live, last_applied).await;
            report(failure, last_applied);
        }
    }
}

/// Puts the last applied set back in effect after a failed bind.
///
/// A failed bind has closed its session, so without this the user would be left
/// with no working combination until the next start. The restore is silent: the
/// desktop already knows these ids.
async fn restore(
    connection: &mut Option<zbus::Connection>,
    live: &mut Option<Live>,
    last_applied: &mut Vec<HotkeyBinding>,
) {
    if live.is_some() || last_applied.iter().all(|binding| !binding.is_set()) {
        return;
    }
    match apply_set(connection, live, last_applied).await {
        Ok(applied) => *last_applied = applied,
        Err(_) => logging::info("hotkeys: the previous set could not be restored"),
    }
}

/// Turns a failure into the event the application sees.
fn report(failure: BindFailure, last_applied: &[HotkeyBinding]) {
    match failure {
        BindFailure::Cancelled => notify(HotkeyEvent::Failed {
            message: tr("The system dialog was cancelled"),
            bindings: last_applied.to_vec(),
        }),
        BindFailure::Unavailable(reason) => {
            logging::info(format!(
                "hotkeys: the GlobalShortcuts portal is not usable: {reason}"
            ));
            disable();
            notify(HotkeyEvent::Unavailable);
        }
        BindFailure::Failed(reason) => notify(HotkeyEvent::Failed {
            message: trf(
                "The desktop portal could not set the hotkeys: {error}",
                &[("error", &reason)],
            ),
            bindings: last_applied.to_vec(),
        }),
    }
}

/// The event-free failure class of a portal call.
fn classify(error: ashpd::Error) -> BindFailure {
    match error {
        // The desktop sends `Other`, not `Cancelled`, when the user closes its
        // dialog without accepting, so both mean "cancelled".
        ashpd::Error::Response(_) => BindFailure::Cancelled,
        ashpd::Error::Portal(ashpd::PortalError::Cancelled(_)) => BindFailure::Cancelled,
        ashpd::Error::Portal(ashpd::PortalError::NotAllowed(message)) => {
            BindFailure::Unavailable(message)
        }
        ashpd::Error::RequiresVersion(required, current) => BindFailure::Unavailable(format!(
            "the interface version {required} is required, {current} is available"
        )),
        ashpd::Error::PortalNotFound(_) => {
            BindFailure::Unavailable("the GlobalShortcuts interface is not exported".into())
        }
        // A dropped connection means the portal went away; the fallback takes
        // over instead of leaving the user without hotkeys.
        ashpd::Error::Zbus(error) => BindFailure::Unavailable(error.to_string()),
        other => BindFailure::Failed(other.to_string()),
    }
}

/// The whole table with every action unbound.
fn unbound() -> Vec<HotkeyBinding> {
    hotkey_defs()
        .iter()
        .map(|def| HotkeyBinding::unset(def.action))
        .collect()
}

/// The trigger the system dialog starts with for this action.
///
/// `preferred_trigger` is only a proposal: the user can change it in the
/// dialog, and for an id the desktop already knows it is ignored. When the
/// action has neither a saved text nor a description to carry a trigger, the
/// table's proposal keeps the dialog's row usable; an empty row would be
/// offered as "Disabled" and could not be accepted.
fn preferred_trigger(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    // A description the desktop reported ("Press <Shift><Control>F12") carries
    // the trigger after its leading word; our own label uses `+`.
    let trigger = text.strip_prefix("Press ").unwrap_or(text).trim();
    if trigger.starts_with('<') {
        return Some(trigger.to_string());
    }
    let hotkey = platform_core::tray::parse_hotkey(trigger)?;
    let mut trigger = String::new();
    if hotkey.ctrl {
        trigger.push_str("<Control>");
    }
    if hotkey.alt {
        trigger.push_str("<Alt>");
    }
    if hotkey.shift {
        trigger.push_str("<Shift>");
    }
    if hotkey.meta {
        trigger.push_str("<Super>");
    }
    trigger.push_str(&hotkey.key.label());
    Some(trigger)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_label_becomes_a_gtk_trigger() {
        assert_eq!(
            preferred_trigger("Ctrl+Shift+F12").as_deref(),
            Some("<Control><Shift>F12")
        );
        assert_eq!(
            preferred_trigger("Alt+Space").as_deref(),
            Some("<Alt>Space")
        );
        assert_eq!(preferred_trigger(""), None);
        assert_eq!(preferred_trigger("Nonsense"), None);
    }

    #[test]
    fn a_reported_description_keeps_its_trigger() {
        assert_eq!(
            preferred_trigger("Press <Shift><Control>F12").as_deref(),
            Some("<Shift><Control>F12")
        );
        assert_eq!(
            preferred_trigger("<Control><Alt>Q").as_deref(),
            Some("<Control><Alt>Q")
        );
    }

    #[test]
    fn every_action_starts_unbound() {
        let bindings = unbound();
        assert_eq!(bindings.len(), hotkey_defs().len());
        assert!(bindings.iter().all(|binding| !binding.is_set()));
        for (binding, def) in bindings.iter().zip(hotkey_defs()) {
            assert_eq!(binding.action, def.action);
        }
    }
}
