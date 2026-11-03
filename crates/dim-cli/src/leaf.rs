//! The interactive process as a UI leaf (Tui design §3.6). `LeafNode`
//! accepts the daemon's one connection on a private address and mounts the
//! session with it; the process runs its own `Tui` once `ui.open` replies.
//! The session is the leaf's lifetime: its end disposes the client tree.

use std::sync::{Arc, Mutex};

use rutis::host::{BoxFuture, DynPlugin, Effect, ErrorKind, FiberState};
use rutis::{Ctx, Error};
use rutis_bridge::{Channel, Close, LocalId, PeerId, Session, channel::Closer, pipe::framed};
use serde::Deserialize;
use tokio::sync::oneshot;

use crate::args::{Options, UiIdentity};
use crate::plugins::{DaemonLink, Tui};
use crate::ui::{self, tui::Leave};

/// The id the daemon greets with.
const DAEMON_PEER: &str = "dim-daemon";

/// The leaf of this process, before it is mounted: its id and the address
/// `ui.open` names.
pub(crate) struct Leaf {
    pub peer: String,
    pub address: String,
    accept: Accept,
}

impl Leaf {
    /// Binds the private address under the native home.
    pub fn bind(options: &Options) -> crate::Result<Self> {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let (accept, address) = Accept::bind(options)?;
        Ok(Self {
            peer: format!("ui-{id}"),
            address,
            accept,
        })
    }

    /// The node plugin, and what resolves once its session ended, or once
    /// the node ended without one.
    pub fn node(self) -> (LeafNode, oneshot::Receiver<()>) {
        let (ended, end) = oneshot::channel();
        (
            LeafNode {
                peer: self.peer,
                accept: Mutex::new(Some((self.accept, ended))),
            },
            end,
        )
    }
}

/// Mounted in the client tree: this process's `LocalId`, and the session
/// with the daemon as its child once the daemon dials. It offers no plugin
/// catalog, so the daemon runs nothing here.
pub(crate) struct LeafNode {
    peer: String,
    accept: Mutex<Option<(Accept, oneshot::Sender<()>)>>,
}

impl DynPlugin for LeafNode {
    fn name(&self) -> &str {
        "UiLeaf"
    }

    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, Error>> {
        Box::pin(async move {
            let peer = PeerId::new(self.peer.clone()).map_err(failed)?;
            ctx.provide(LocalId(peer))?;
            let (accept, ended) = self
                .accept
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .take()
                .ok_or_else(|| failed("the leaf accepts one connection"))?;
            let ctx = ctx.clone();
            let session = tokio::spawn(async move {
                let _ended = ended;
                let Ok(channel) = accept.accept().await else {
                    return;
                };
                let Ok(greeted) = Session::handshake(&ctx, channel).await else {
                    return;
                };
                if greeted.id() != DAEMON_PEER {
                    return;
                }
                let mut state = ctx.plugin(greeted).watch();
                let _ = state
                    .wait_for(|s| matches!(s.state, FiberState::Disposed | FiberState::Failed))
                    .await;
            });
            Ok(Effect::Disposer(Box::new(move || {
                session.abort();
                Ok(())
            })))
        })
    }
}

/// What `ui.open` replies.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Opened {
    ui_id: String,
    token: String,
}

/// The leaf's own `Tui`: the ordinary client tree (`DaemonLink` + `Tui`) as
/// its children, on the daemon's Session and registered as its leaf.
pub(crate) struct LeafTui {
    options: Options,
    leave: Mutex<Option<oneshot::Sender<Leave>>>,
}

impl LeafTui {
    /// This invocation's options, on `session_id`, attaching as `opened`.
    pub fn new(
        options: &Options,
        session_id: String,
        opened: Opened,
        leave: oneshot::Sender<Leave>,
    ) -> Self {
        let mut options = options.clone();
        options.created_session = options.resume.is_none();
        options.resume = Some(session_id);
        options.ui_identity = Some(UiIdentity {
            ui_id: opened.ui_id,
            token: opened.token,
        });
        Self {
            options,
            leave: Mutex::new(Some(leave)),
        }
    }
}

impl DynPlugin for LeafTui {
    fn name(&self) -> &str {
        "Tui"
    }

    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, Error>> {
        Box::pin(async move {
            let link = ctx.plugin(DaemonLink {
                options: self.options.clone(),
                start: false,
            });
            (&link).await.map_err(unwrap)?;
            let leave = self
                .leave
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .take()
                .ok_or_else(|| failed("Tui was loaded twice"))?;
            let env = ui::Env {
                home: self.options.home.clone(),
                workspace: self.options.workspace().as_str().to_owned(),
                fullscreen: self
                    .options
                    .fullscreen
                    .unwrap_or_else(|| ui::config::saved_fullscreen(&self.options.home)),
            };
            let tui = ctx.plugin(Tui {
                env,
                options: self.options.clone(),
                leave: Mutex::new(Some(leave)),
            });
            (&tui).await.map_err(unwrap)?;
            Ok(Effect::Done)
        })
    }
}

fn failed(error: impl std::fmt::Display) -> Error {
    Error::from(ErrorKind::PluginFailed(error.to_string().into()))
}

fn unwrap(error: Arc<ErrorKind>) -> Error {
    failed(error)
}

/// The leaf's side of its session: a private address that accepts exactly
/// one connection, the daemon's. On Unix a socket in the private native
/// home beside `daemon.sock`, with a name of the same length so any home the
/// daemon can use fits the socket path limit, removed once accepted;
/// elsewhere a loopback address whose first line must be a one-time token.
struct Accept {
    listener: Option<Listener>,
}

enum Listener {
    #[cfg(unix)]
    Unix {
        listener: std::os::unix::net::UnixListener,
        path: std::path::PathBuf,
    },
    #[cfg(not(unix))]
    Tcp {
        listener: std::net::TcpListener,
        token: String,
    },
}

impl Accept {
    #[cfg(unix)]
    fn bind(options: &Options) -> crate::Result<(Self, String)> {
        let home = std::fs::canonicalize(options.home.join("rust"))?;
        let mut attempts = 0;
        let (listener, path) = loop {
            let id = &uuid::Uuid::new_v4().simple().to_string()[..8];
            let path = home.join(format!("ui-{id}"));
            match std::os::unix::net::UnixListener::bind(&path) {
                Ok(listener) => break (listener, path),
                Err(error) if error.kind() == std::io::ErrorKind::AddrInUse && attempts < 3 => {
                    attempts += 1;
                }
                Err(error) => return Err(error.into()),
            }
        };
        let address = format!("unix:{}", path.display());
        Ok((
            Self {
                listener: Some(Listener::Unix { listener, path }),
            },
            address,
        ))
    }

    #[cfg(not(unix))]
    fn bind(_options: &Options) -> crate::Result<(Self, String)> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let token = uuid::Uuid::new_v4().simple().to_string();
        let address = format!("tcp:{}#{token}", listener.local_addr()?);
        Ok((
            Self {
                listener: Some(Listener::Tcp { listener, token }),
            },
            address,
        ))
    }

    /// Accepts the one connection.
    async fn accept(mut self) -> std::io::Result<Channel> {
        let listener = self.listener.take().expect("an unaccepted leaf address");
        match listener {
            #[cfg(unix)]
            Listener::Unix { listener, path } => {
                listener.set_nonblocking(true)?;
                let listener = tokio::net::UnixListener::from_std(listener)?;
                let accepted = listener.accept().await;
                let _ = std::fs::remove_file(&path);
                let stream = accepted?.0.into_std()?;
                stream.set_nonblocking(false)?;
                let reader = stream.try_clone()?;
                let closer = Arc::new(UnixCloser(stream.try_clone()?));
                Ok(framed(reader, stream, closer))
            }
            #[cfg(not(unix))]
            Listener::Tcp { listener, token } => {
                use tokio::io::AsyncReadExt;
                listener.set_nonblocking(true)?;
                let listener = tokio::net::TcpListener::from_std(listener)?;
                let (mut stream, _) = listener.accept().await?;
                let mut line = Vec::new();
                while line.len() <= token.len() {
                    let byte = stream.read_u8().await?;
                    if byte == b'\n' {
                        break;
                    }
                    line.push(byte);
                }
                if line != token.as_bytes() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "the leaf token did not match",
                    ));
                }
                let stream = stream.into_std()?;
                stream.set_nonblocking(false)?;
                stream.set_nodelay(true)?;
                let reader = stream.try_clone()?;
                let closer = Arc::new(TcpCloser(stream.try_clone()?));
                Ok(framed(reader, stream, closer))
            }
        }
    }
}

impl Drop for Accept {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(Listener::Unix { path, .. }) = self.listener.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(not(unix))]
struct TcpCloser(std::net::TcpStream);

#[cfg(not(unix))]
impl Closer for TcpCloser {
    fn close(&self, _why: Close) {
        let _ = self.0.shutdown(std::net::Shutdown::Both);
    }
}

#[cfg(unix)]
struct UnixCloser(std::os::unix::net::UnixStream);

#[cfg(unix)]
impl Closer for UnixCloser {
    fn close(&self, _why: Close) {
        let _ = self.0.shutdown(std::net::Shutdown::Both);
    }
}
