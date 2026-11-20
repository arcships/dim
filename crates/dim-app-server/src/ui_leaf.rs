//! The daemon side of UI leaves (Tui design §3.6). A leaf is the interactive
//! `dimcode` process: a rutis kernel waiting on a private address, which runs
//! its own `Tui`. `UiLeaf` dials that address, greets from a context where
//! the daemon's plugin catalog is isolated, and mounts the session; the
//! session's end is the leaf's. The daemon starts no process, writes no
//! terminal and runs no UI-specific control protocol.

use dim_agent_capabilities::methods::RpcError;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, ErrorKind, FiberState, FiberView};
use rutis::{Ctx, Error, Plugin};
use rutis_bridge::{Channel, Close, PeerId, Session, catalog_key, channel::Closer, pipe::framed};
use std::sync::{Arc, Mutex};

/// What runs once when a mounted leaf session ends.
pub(crate) type Ended = Box<dyn FnOnce() + Send>;

/// Mounts leaves under `UiHostPlugin`.
pub(crate) struct UiLeaves {
    ctx: Ctx,
}

/// One leaf: its id (the `uiId`) and where it waits.
pub(crate) struct LeafSpec {
    pub ui_id: String,
    pub address: String,
}

impl UiLeaves {
    /// Mounts one `UiLeaf` and resolves once its session is mounted; a dial
    /// or handshake failure is `ui_leaf_failed`. `ended` runs when that
    /// session ends.
    pub async fn start(&self, spec: LeafSpec, ended: Ended) -> Result<FiberView, RpcError> {
        let peer = PeerId::new(spec.ui_id.clone()).map_err(leaf_failed)?;
        let view = self.ctx.plugin(UiLeaf {
            name: format!("UiLeaf#{}", spec.ui_id),
            peer,
            address: spec.address,
            ended: Mutex::new(Some(ended)),
        });
        match (&view).await {
            Ok(()) => Ok(view),
            Err(error) => {
                let _ = view.dispose().await;
                Err(leaf_failed(error))
            }
        }
    }
}

/// One leaf: the session with it, as a child, so disposing the leaf closes
/// the session and its channel.
struct UiLeaf {
    name: String,
    peer: PeerId,
    address: String,
    ended: Mutex<Option<Ended>>,
}

impl DynPlugin for UiLeaf {
    fn name(&self) -> &str {
        &self.name
    }

    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, Error>> {
        Box::pin(async move {
            let channel = dial(&self.address)
                .await
                .map_err(|error| failed(format!("cannot reach the leaf: {error}")))?;
            let isolated = ctx.isolate(catalog_key(), &self.name);
            let greeted = Session::handshake(&isolated, channel)
                .await
                .map_err(|error| failed(format!("the leaf did not greet: {error}")))?;
            if greeted.id() != self.peer.as_str() {
                return Err(failed(format!(
                    "the leaf greeted as {}, not {}",
                    greeted.id(),
                    self.peer
                )));
            }
            let session = isolated.plugin(greeted);
            (&session)
                .await
                .map_err(|error| failed(format!("the leaf session did not start: {error}")))?;
            let ended = self.ended.lock().expect("ui leaf").take();
            let mut state = session.watch();
            let watcher = tokio::spawn(async move {
                let _ = state
                    .wait_for(|s| matches!(s.state, FiberState::Disposed | FiberState::Failed))
                    .await;
                if let Some(ended) = ended {
                    ended();
                }
            });
            Ok(Effect::Disposer(Box::new(move || {
                watcher.abort();
                Ok(())
            })))
        })
    }
}

/// Dials the address a leaf waits on: `unix:<path>` on Unix, or
/// `tcp:<host>:<port>#<token>` (loopback, the token sent as the first line,
/// before the handshake) elsewhere. The leaf accepts one connection.
async fn dial(address: &str) -> std::io::Result<Channel> {
    if let Some(rest) = address.strip_prefix("tcp:") {
        let (host, token) = rest.split_once('#').ok_or_else(|| incompatible(address))?;
        let stream = tokio::net::TcpStream::connect(host).await?.into_std()?;
        stream.set_nonblocking(false)?;
        stream.set_nodelay(true)?;
        let mut writer = stream.try_clone()?;
        use std::io::Write;
        writeln!(writer, "{token}")?;
        let reader = stream.try_clone()?;
        let closer = Arc::new(TcpCloser(stream.try_clone()?));
        return Ok(framed(reader, stream, closer));
    }
    #[cfg(unix)]
    if let Some(path) = address.strip_prefix("unix:") {
        let stream = tokio::net::UnixStream::connect(path).await?.into_std()?;
        stream.set_nonblocking(false)?;
        let reader = stream.try_clone()?;
        let closer = Arc::new(UnixCloser(stream.try_clone()?));
        return Ok(framed(reader, stream, closer));
    }
    Err(incompatible(address))
}

fn incompatible(address: &str) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("not a UI leaf address: {address}"),
    )
}

struct TcpCloser(std::net::TcpStream);

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

/// The process capability owning UI leaves: `UiHost`, the registry. Its
/// disposal waits for every leaf to close.
#[derive(Default)]
pub(crate) struct UiHostPlugin;

impl Plugin for UiHostPlugin {
    type Deps = ();

    fn name(&self) -> &str {
        "UiHost"
    }

    async fn apply(&self, ctx: &Ctx, (): ()) -> rutis::Result {
        let host = crate::ui_host::UiHost::new(UiLeaves { ctx: ctx.clone() });
        ctx.provide_as::<crate::ui_host::UiHost>(host.clone())?;
        ctx.provide_at::<dyn rutis_bridge::session::HostDispatch>(
            rutis_bridge::session::host_key("uiHost"),
            host.clone(),
        )?;
        ctx.effect(move || async move {
            host.close_all().await;
            Ok(())
        })?;
        Ok(())
    }
}

fn failed(message: String) -> Error {
    Error::from(ErrorKind::PluginFailed(message.into()))
}

pub(crate) fn leaf_failed(message: impl std::fmt::Display) -> RpcError {
    RpcError::app(-32000, "ui_leaf_failed", message.to_string())
}
