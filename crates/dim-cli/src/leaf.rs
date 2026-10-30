//! The interactive process as a UI leaf (Tui design §3.6). The client tree
//! mounts a rutis node: a private address that accepts one connection, the
//! node's identity, and a `PeerPlugin` whose only feature is `host` with
//! `Tui` in its catalog. `ui.open` makes the daemon link to it and load
//! `Tui` there; unloading it restores the terminal through `Tui`'s own
//! effect. The link is the leaf's lifetime.

use std::sync::{Arc, Mutex};

use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, ErrorKind, PluginFactory, TypeKey};
use rutis::{Ctx, Error};
use rutis_bridge::{
    Described, Dial, Features, Identity, LinkConfig, LinkState, PeerPlugin, StaticCatalog,
    StaticIdentity, Transport,
    channel::{Channel, Closer, ConnectError, PeerId},
    transport::local::framed,
    transport_key,
};
use serde::Deserialize;
use tokio::sync::{oneshot, watch};

use crate::args::{Options, UiIdentity};
use crate::plugins::{DaemonLink, Tui};
use crate::ui::{self, tui::Leave};

/// The transport and identity keys of the leaf link, as on the daemon.
const LINKS: &str = "ui-leaf";
const DAEMON_PEER: &str = "dim-daemon";

type LinkWatch = Arc<Mutex<Option<watch::Receiver<LinkState>>>>;

/// The leaf node of this process, before it is mounted: its id and the
/// address `ui.open` names.
pub(crate) struct Leaf {
    pub peer: String,
    pub address: String,
    accept: Arc<Accept>,
}

impl Leaf {
    /// Binds the private address under the native home.
    pub fn bind(options: &Options) -> crate::Result<Self> {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let peer = format!("ui-{id}");
        let (accept, address) = Accept::bind(options)?;
        Ok(Self {
            peer,
            address,
            accept: Arc::new(accept),
        })
    }

    /// The node plugin. `leave` receives how the hosted `Tui` ended; the
    /// returned watch holds the link's state once the node runs.
    pub fn node(&self, options: Options, leave: oneshot::Sender<Leave>) -> (LeafNode, LinkWatch) {
        let state = LinkWatch::default();
        (
            LeafNode {
                peer: PeerId::new(self.peer.clone()).expect("a valid leaf id"),
                accept: self.accept.clone(),
                options,
                leave: Arc::new(Mutex::new(Some(leave))),
                state: state.clone(),
            },
            state,
        )
    }
}

/// Mounted in the client tree: provides the leaf transport and identity and
/// runs the `PeerPlugin` as its child, so disposing it unloads the hosted
/// `Tui` and closes the link.
pub(crate) struct LeafNode {
    peer: PeerId,
    accept: Arc<Accept>,
    options: Options,
    leave: Arc<Mutex<Option<oneshot::Sender<Leave>>>>,
    state: LinkWatch,
}

impl DynPlugin for LeafNode {
    fn name(&self) -> &str {
        "UiLeaf"
    }

    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, Error>> {
        Box::pin(async move {
            ctx.provide_at::<dyn Transport>(transport_key(LINKS), self.accept.clone())?;
            let identity: Arc<dyn Identity> = Arc::new(StaticIdentity::new(self.peer.clone()));
            ctx.provide_at::<dyn Identity>(rutis_bridge::identity_key(LINKS), identity)?;
            let catalog = StaticCatalog::new().with(
                "Tui",
                Described {
                    schema: Some(schema()),
                    version: Some(env!("CARGO_PKG_VERSION").to_owned()),
                    integrity: None,
                },
                TuiFactory {
                    options: self.options.clone(),
                    leave: self.leave.clone(),
                },
            );
            let daemon = PeerId::new(DAEMON_PEER).expect("valid daemon peer id");
            let mut link = LinkConfig::dial(daemon, LINKS, LINKS, "accept");
            link.reconnect = false;
            let peer = PeerPlugin::new(
                link,
                Features {
                    host: Some(Arc::new(catalog)),
                    ..Features::default()
                },
            )
            .map_err(|error| rutis::Error::from(ErrorKind::PluginFailed(error.into())))?;
            let handle = peer.handle();
            let node = ctx.plugin(peer);
            (&node).await.map_err(unwrap)?;
            *self.state.lock().unwrap_or_else(|p| p.into_inner()) = handle.link_state();
            Ok(Effect::Done)
        })
    }
}

/// Waits until the link stopped.
pub(crate) async fn stopped(state: &LinkWatch) {
    let Some(mut state) = state.lock().unwrap_or_else(|p| p.into_inner()).clone() else {
        return;
    };
    loop {
        if matches!(&*state.borrow_and_update(), LinkState::Stopped { .. }) {
            return;
        }
        if state.changed().await.is_err() {
            return;
        }
    }
}

/// What the daemon decides for the hosted `Tui`; everything else is this
/// process's own invocation.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LeafConfig {
    ui_id: String,
    token: String,
    session_id: String,
}

fn schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["uiId", "token", "sessionId"],
        "properties": {
            "uiId": {"type": "string"},
            "token": {"type": "string"},
            "sessionId": {"type": "string"}
        }
    })
}

/// Builds the hosted `Tui`: this invocation's options, resumed on the
/// daemon's Session and registered as its leaf.
struct TuiFactory {
    options: Options,
    leave: Arc<Mutex<Option<oneshot::Sender<Leave>>>>,
}

impl PluginFactory<serde_json::Value> for TuiFactory {
    fn name(&self) -> &str {
        "Tui"
    }

    fn injects(&self) -> Vec<TypeKey> {
        Vec::new()
    }

    fn build(&self, config: &serde_json::Value) -> Result<Box<dyn DynPlugin>, Error> {
        let config = LeafConfig::deserialize(config).map_err(|error| {
            rutis::Error::from(ErrorKind::PluginFailed(
                format!("Tui config: {error}").into(),
            ))
        })?;
        let mut options = self.options.clone();
        options.created_session = options.resume.is_none();
        options.resume = Some(config.session_id);
        options.ui_identity = Some(UiIdentity {
            ui_id: config.ui_id,
            token: config.token,
        });
        Ok(Box::new(HostedTui {
            options,
            leave: self.leave.clone(),
        }))
    }
}

/// The ordinary client tree (`DaemonLink` + `Tui`) as the hosted row's
/// children.
struct HostedTui {
    options: Options,
    leave: Arc<Mutex<Option<oneshot::Sender<Leave>>>>,
}

impl DynPlugin for HostedTui {
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
                .ok_or_else(|| {
                    rutis::Error::from(ErrorKind::PluginFailed("Tui was loaded twice".into()))
                })?;
            let env = ui::Env {
                home: self.options.home.clone(),
                workspace: self.options.cwd.clone(),
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

fn unwrap(error: Arc<ErrorKind>) -> Error {
    rutis::Error::from(ErrorKind::PluginFailed(error.to_string().into()))
}

/// The leaf's side of its link: a private address that accepts exactly one
/// connection, the daemon's. On Unix a socket in the private native home
/// beside `daemon.sock`, with a name of the same length so any home the
/// daemon can use fits the socket path limit, removed once accepted;
/// elsewhere a loopback address whose first line must be a one-time token.
struct Accept {
    listener: Mutex<Option<Listener>>,
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
                listener: Mutex::new(Some(Listener::Unix { listener, path })),
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
                listener: Mutex::new(Some(Listener::Tcp { listener, token })),
            },
            address,
        ))
    }
}

impl Drop for Accept {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(Listener::Unix { path, .. }) = self
            .listener
            .get_mut()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            let _ = std::fs::remove_file(path);
        }
    }
}

impl Transport for Accept {
    fn kind(&self) -> &str {
        LINKS
    }

    fn dial<'a>(&'a self, _dial: &'a Dial) -> BoxFuture<'a, Result<Channel, ConnectError>> {
        Box::pin(async move {
            let listener = self
                .listener
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .take()
                .ok_or_else(|| ConnectError::Incompatible {
                    reason: "the leaf accepts one connection".into(),
                })?;
            accept(listener).await
        })
    }
}

async fn accept(listener: Listener) -> Result<Channel, ConnectError> {
    match listener {
        #[cfg(unix)]
        Listener::Unix { listener, path } => {
            listener.set_nonblocking(true).map_err(failed)?;
            let listener = tokio::net::UnixListener::from_std(listener).map_err(failed)?;
            let accepted = listener.accept().await;
            let _ = std::fs::remove_file(&path);
            let stream = accepted.map_err(failed)?.0.into_std().map_err(failed)?;
            stream.set_nonblocking(false).map_err(failed)?;
            let reader = stream.try_clone().map_err(failed)?;
            let closer = Arc::new(UnixCloser(stream.try_clone().map_err(failed)?));
            Ok(framed(reader, stream, closer))
        }
        #[cfg(not(unix))]
        Listener::Tcp { listener, token } => {
            use tokio::io::AsyncReadExt;
            listener.set_nonblocking(true).map_err(failed)?;
            let listener = tokio::net::TcpListener::from_std(listener).map_err(failed)?;
            let (mut stream, _) = listener.accept().await.map_err(failed)?;
            let mut line = Vec::new();
            while line.len() <= token.len() {
                let byte = stream.read_u8().await.map_err(failed)?;
                if byte == b'\n' {
                    break;
                }
                line.push(byte);
            }
            if line != token.as_bytes() {
                return Err(ConnectError::Incompatible {
                    reason: "the leaf token did not match".into(),
                });
            }
            let stream = stream.into_std().map_err(failed)?;
            stream.set_nonblocking(false).map_err(failed)?;
            stream.set_nodelay(true).map_err(failed)?;
            let reader = stream.try_clone().map_err(failed)?;
            let closer = Arc::new(TcpCloser(stream.try_clone().map_err(failed)?));
            Ok(framed(reader, stream, closer))
        }
    }
}

fn failed(error: std::io::Error) -> ConnectError {
    ConnectError::Incompatible {
        reason: error.to_string(),
    }
}

#[cfg(not(unix))]
struct TcpCloser(std::net::TcpStream);

#[cfg(not(unix))]
impl Closer for TcpCloser {
    fn close(&self, _reason: &str) {
        let _ = self.0.shutdown(std::net::Shutdown::Both);
    }
}

#[cfg(unix)]
struct UnixCloser(std::os::unix::net::UnixStream);

#[cfg(unix)]
impl Closer for UnixCloser {
    fn close(&self, _reason: &str) {
        let _ = self.0.shutdown(std::net::Shutdown::Both);
    }
}
