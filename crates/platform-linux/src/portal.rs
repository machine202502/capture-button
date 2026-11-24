//! The XDG desktop portal screen-cast path, used on Wayland sessions.
//!
//! A Wayland client cannot read the screen itself: it asks
//! `org.freedesktop.portal.ScreenCast` for a PipeWire node and consumes that
//! node. The portal interaction, including the picker the user sees, must run
//! away from the capture caller (which can be the UI thread), so it lives on
//! one worker thread with a current-thread tokio runtime.
//!
//! The worker keeps the portal session alive for as long as the returned
//! `PortalStream` lease exists, because the PipeWire node disappears when the
//! session closes. `restore_token` is read from the settings store before
//! `SelectSources` and a fresh one is written after `Start`, so a later capture
//! can restore the previous source without asking again.

use ashpd::desktop::{
    screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType},
    PersistMode, ResponseError, Session,
};
use ashpd::enumflags2::BitFlags;
use std::os::fd::OwnedFd;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::OnceLock;

/// The kind of source the portal should offer in its picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    Monitor,
    Window,
    MonitorOrWindow,
}

impl SourceKind {
    /// The `types` bit flags for `SelectSources`.
    pub fn types(self) -> BitFlags<SourceType> {
        match self {
            Self::Monitor => BitFlags::from_flag(SourceType::Monitor),
            Self::Window => BitFlags::from_flag(SourceType::Window),
            Self::MonitorOrWindow => SourceType::Monitor | SourceType::Window,
        }
    }
}

/// Maps the app's "show the pointer" setting to the portal cursor mode.
pub fn cursor_mode(cursor: bool) -> CursorMode {
    if cursor {
        CursorMode::Embedded
    } else {
        CursorMode::Hidden
    }
}

/// A portal request either completed or the user closed the picker.
///
/// A cancelled request is a normal outcome, not a failure: the caller reports
/// it as such instead of treating the portal as broken.
#[derive(Debug, PartialEq, Eq)]
pub enum PortalError {
    Cancelled,
    Failed(String),
}

impl PortalError {
    pub fn message(&self) -> String {
        match self {
            Self::Cancelled => "The screen capture was cancelled".into(),
            Self::Failed(message) => message.clone(),
        }
    }
}

fn classify(error: ashpd::Error) -> PortalError {
    match error {
        ashpd::Error::Response(ResponseError::Cancelled) => PortalError::Cancelled,
        other => PortalError::Failed(format!(
            "the desktop portal could not capture the screen: {other}"
        )),
    }
}

/// One live PipeWire stream handed out by the portal.
pub struct PortalStream {
    fd: Option<OwnedFd>,
    node_id: u32,
    /// The source size the portal reported, if it reported one.
    pub size: Option<(u32, u32)>,
    lease: Lease,
}

impl PortalStream {
    /// Splits the stream into the PipeWire connection details and the lease
    /// that keeps the portal session open. Dropping the lease closes the
    /// session, so hold it for as long as frames are wanted.
    pub fn into_parts(mut self) -> (OwnedFd, u32, Lease) {
        let fd = self.fd.take().expect("the PipeWire fd is taken once");
        (fd, self.node_id, self.lease)
    }
}

/// Keeps a portal session open; dropping it asks the worker to close it.
pub struct Lease {
    id: u64,
    release: Sender<Command>,
}

impl Drop for Lease {
    fn drop(&mut self) {
        let _ = self.release.send(Command::Release(self.id));
    }
}

type Reply = Sender<Result<PortalStream, PortalError>>;

enum Command {
    Acquire {
        kind: SourceKind,
        cursor: bool,
        reply: Reply,
    },
    Release(u64),
}

struct Worker {
    commands: Sender<Command>,
}

/// Asks the portal for a source and opens its PipeWire remote.
///
/// This blocks until the user finishes with the picker. It must not be called
/// from a thread that has to keep drawing while the portal is open; the portal
/// work itself always runs on the worker thread.
pub fn pick_source(kind: SourceKind, cursor: bool) -> Result<PortalStream, PortalError> {
    let worker = worker().ok_or_else(|| {
        PortalError::Failed("the desktop portal worker could not be started".into())
    })?;
    let (reply, answer) = channel();
    worker
        .commands
        .send(Command::Acquire {
            kind,
            cursor,
            reply,
        })
        .map_err(|_| PortalError::Failed("the desktop portal worker stopped".into()))?;
    answer
        .recv()
        .map_err(|_| PortalError::Failed("the desktop portal worker stopped".into()))?
}

fn worker() -> Option<&'static Worker> {
    static WORKER: OnceLock<Option<Worker>> = OnceLock::new();
    WORKER.get_or_init(start_worker).as_ref()
}

fn start_worker() -> Option<Worker> {
    let (commands, receiver) = channel();
    let self_sender = commands.clone();
    std::thread::Builder::new()
        .name("capture-button-portal".into())
        .spawn(move || run_worker(receiver, self_sender))
        .ok()?;
    Some(Worker { commands })
}

/// The worker holds the portal session objects. It only runs while a request
/// is in flight; between requests the D-Bus connection stays open on its own,
/// which is all a screen-cast session needs once started.
fn run_worker(receiver: Receiver<Command>, self_sender: Sender<Command>) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            while let Ok(command) = receiver.recv() {
                if let Command::Acquire { reply, .. } = command {
                    let _ = reply.send(Err(PortalError::Failed(format!(
                        "cannot start the desktop portal runtime: {error}"
                    ))));
                }
            }
            return;
        }
    };
    let mut open: Option<OpenSession> = None;
    let mut next_id = 1u64;
    while let Ok(command) = receiver.recv() {
        match command {
            Command::Acquire {
                kind,
                cursor,
                reply,
            } => {
                if let Some(previous) = open.take() {
                    runtime.block_on(close(previous));
                }
                match runtime.block_on(acquire(kind, cursor)) {
                    Ok((mut session, fd, size)) => {
                        let id = next_id;
                        next_id += 1;
                        session.id = id;
                        let node_id = session.node_id;
                        open = Some(session);
                        let stream = PortalStream {
                            fd: Some(fd),
                            node_id,
                            size,
                            lease: Lease {
                                id,
                                release: self_sender.clone(),
                            },
                        };
                        let _ = reply.send(Ok(stream));
                    }
                    Err(error) => {
                        let _ = reply.send(Err(error));
                    }
                }
            }
            Command::Release(id) => {
                if open.as_ref().is_some_and(|session| session.id == id) {
                    if let Some(session) = open.take() {
                        runtime.block_on(close(session));
                    }
                }
            }
        }
    }
}

struct OpenSession {
    id: u64,
    screencast: Screencast,
    session: Session<Screencast>,
    node_id: u32,
}

async fn acquire(
    kind: SourceKind,
    cursor: bool,
) -> Result<(OpenSession, OwnedFd, Option<(u32, u32)>), PortalError> {
    let screencast = Screencast::new().await.map_err(classify)?;
    let session = screencast
        .create_session(Default::default())
        .await
        .map_err(classify)?;
    // The token is one-shot: consume it in SelectSources and write the fresh
    // one from Start. Clearing it first means a failed or cancelled Start
    // cannot leave a token behind that the portal already rejected.
    let token = crate::store::portal_restore_token();
    let options = SelectSourcesOptions::default()
        .set_sources(kind.types())
        .set_multiple(false)
        .set_cursor_mode(cursor_mode(cursor))
        .set_persist_mode(PersistMode::ExplicitlyRevoked)
        .set_restore_token(token.as_deref());
    screencast
        .select_sources(&session, options)
        .await
        .map_err(classify)?;
    if token.is_some() {
        crate::store::set_portal_restore_token(None);
    }
    let streams = screencast
        .start(&session, None, Default::default())
        .await
        .map_err(classify)?
        .response()
        .map_err(classify)?;
    if let Some(token) = streams.restore_token() {
        crate::store::set_portal_restore_token(Some(token));
    }
    let stream = streams
        .streams()
        .first()
        .ok_or_else(|| PortalError::Failed("the desktop portal returned no stream".into()))?;
    let node_id = stream.pipe_wire_node_id();
    if node_id == 0 {
        return Err(PortalError::Failed(
            "the desktop portal returned an empty PipeWire node".into(),
        ));
    }
    let size = stream.size().and_then(|(width, height)| {
        (width > 0 && height > 0).then_some((width as u32, height as u32))
    });
    let fd = screencast
        .open_pipe_wire_remote(&session, Default::default())
        .await
        .map_err(classify)?;
    Ok((
        OpenSession {
            id: 0,
            screencast,
            session,
            node_id,
        },
        fd,
        size,
    ))
}

async fn close(open: OpenSession) {
    let _ = open.session.close().await;
    drop(open.screencast);
}

/// Whether the environment says this session is Wayland and the portal is the
/// only way to the screen.
pub fn session_is_wayland() -> bool {
    wants_portal(
        std::env::var("WAYLAND_DISPLAY").ok().as_deref(),
        std::env::var("XDG_SESSION_TYPE").ok().as_deref(),
    )
}

fn wants_portal(wayland_display: Option<&str>, session_type: Option<&str>) -> bool {
    wayland_display.is_some_and(|value| !value.is_empty())
        || session_type.is_some_and(|value| value.eq_ignore_ascii_case("wayland"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wayland_environment_uses_the_portal() {
        assert!(wants_portal(Some("wayland-0"), None));
        assert!(wants_portal(None, Some("wayland")));
        assert!(wants_portal(None, Some("Wayland")));
        assert!(!wants_portal(Some(""), Some("x11")));
        assert!(!wants_portal(None, Some("x11")));
        assert!(!wants_portal(None, None));
    }

    #[test]
    fn the_three_source_requests_map_to_the_portal_type_flags() {
        assert_eq!(
            SourceKind::Monitor.types(),
            BitFlags::from_flag(SourceType::Monitor)
        );
        assert_eq!(
            SourceKind::Window.types(),
            BitFlags::from_flag(SourceType::Window)
        );
        assert_eq!(
            SourceKind::MonitorOrWindow.types(),
            SourceType::Monitor | SourceType::Window
        );
    }

    #[test]
    fn the_cursor_setting_selects_embedded_or_hidden() {
        assert_eq!(cursor_mode(true), CursorMode::Embedded);
        assert_eq!(cursor_mode(false), CursorMode::Hidden);
    }

    #[test]
    fn a_cancelled_portal_request_is_a_cancellation_not_a_failure() {
        assert_eq!(
            classify(ashpd::Error::Response(ResponseError::Cancelled)),
            PortalError::Cancelled
        );
        let other = classify(ashpd::Error::Response(ResponseError::Other));
        assert!(matches!(other, PortalError::Failed(_)));
    }
}
