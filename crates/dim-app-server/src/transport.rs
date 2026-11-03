use crate::{
    ServerConfig, ServerError,
    connection::{ClientIdentity, Connection, ConnectionHub, Entry},
    control::ServerControl,
    outbound::MAX_FRAME_BYTES,
    protocol::*,
    storage::SqliteStorage,
};
use axum::{
    Router,
    extract::{
        State as AxumState,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::IntoResponse,
    routing::get,
};
use dim_agent_capabilities::environment_clients::{ClientChannel, DeclaredClientKind};
use dim_agent_capabilities::methods::RpcError;
use dim_agent_runtime::Names;
use futures_util::{
    SinkExt, StreamExt,
    stream::{SplitSink, SplitStream},
};
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, ErrorKind, Key, TypeKey};
use rutis::{Ctx, Error};
use std::{io, net::SocketAddr, path::PathBuf, sync::Arc};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf},
    net::TcpListener,
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

#[cfg(windows)]
use tokio::net::windows::named_pipe::{NamedPipeServer as LocalStream, ServerOptions};
#[cfg(unix)]
use {
    std::path::Path,
    tokio::net::{UnixListener, UnixStream as LocalStream},
};

#[cfg(unix)]
struct SocketFile {
    path: PathBuf,
    dev: u64,
    inode: u64,
    _lock: std::fs::File,
}
#[cfg(unix)]
impl Drop for SocketFile {
    fn drop(&mut self) {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        if std::fs::symlink_metadata(&self.path).is_ok_and(|m| {
            m.file_type().is_socket() && m.dev() == self.dev && m.ino() == self.inode
        }) {
            let _ = std::fs::remove_file(&self.path);
        }
        let _ = self._lock.unlock();
    }
}
#[cfg(unix)]
async fn unix_bind(
    path: &Path,
    probe_timeout: std::time::Duration,
) -> io::Result<(UnixListener, SocketFile)> {
    use std::{
        fs::{OpenOptions, TryLockError},
        os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    };
    if let Some(parent) = path.parent()
        && !parent.exists()
    {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(parent)?;
    }
    if let Some(parent) = path.parent() {
        let metadata = std::fs::metadata(parent)?;
        if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "socket directory must be private (0700)",
            ));
        }
    }
    let mut lock_name = path.as_os_str().to_owned();
    lock_name.push(".lock");
    let lock_path = PathBuf::from(lock_name);
    match std::fs::symlink_metadata(&lock_path) {
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "socket lock path must be a regular file",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&lock_path)?;
    let held = lock.metadata()?;
    let current = std::fs::symlink_metadata(&lock_path)?;
    if !current.file_type().is_file() || current.dev() != held.dev() || current.ino() != held.ino()
    {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "socket lock changed while opening",
        ));
    }
    lock.try_lock().map_err(|error| match error {
        TryLockError::WouldBlock => io::Error::new(
            io::ErrorKind::AddrInUse,
            "socket endpoint is owned by another server",
        ),
        TryLockError::Error(error) => error,
    })?;
    match std::fs::symlink_metadata(path) {
        Ok(previous) => {
            if !previous.file_type().is_socket() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "socket endpoint path is not a socket",
                ));
            }
            match tokio::time::timeout(probe_timeout, LocalStream::connect(path)).await {
                Ok(Err(error)) if error.kind() == io::ErrorKind::ConnectionRefused => {}
                Ok(Err(error)) => return Err(error),
                Ok(Ok(_)) => {
                    return Err(io::Error::new(
                        io::ErrorKind::AddrInUse,
                        "socket endpoint is still accepting connections",
                    ));
                }
                Err(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "socket liveness probe timed out",
                    ));
                }
            }
            let current = std::fs::symlink_metadata(path)?;
            if !current.file_type().is_socket()
                || current.dev() != previous.dev()
                || current.ino() != previous.ino()
            {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "socket endpoint changed during the liveness probe",
                ));
            }
            std::fs::remove_file(path)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let listener = UnixListener::bind(path)?;
    let metadata = std::fs::symlink_metadata(path)?;
    let owned = SocketFile {
        path: path.to_owned(),
        dev: metadata.dev(),
        inode: metadata.ino(),
        _lock: lock,
    };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok((listener, owned))
}

#[cfg(windows)]
fn pipe_bind(name: &str, first: bool) -> io::Result<LocalStream> {
    use std::{
        ffi::c_void,
        mem::size_of,
        os::windows::io::{FromRawHandle, OwnedHandle},
        ptr::null_mut,
    };
    use windows_sys::Win32::{
        Foundation::{ERROR_INSUFFICIENT_BUFFER, LocalFree},
        Security::{
            Authorization::{
                ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
                SDDL_REVISION_1,
            },
            GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
        },
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };
    if !name.starts_with(r"\\.\pipe\") || name.len() == r"\\.\pipe\".len() || name.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "named pipe requires a local \\\\.\\pipe\\ name",
        ));
    }
    struct LocalMemory(*mut c_void);
    impl Drop for LocalMemory {
        fn drop(&mut self) {
            unsafe { LocalFree(self.0) };
        }
    }
    let mut token = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let _token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut length = 0;
    if unsafe { GetTokenInformation(token, TokenUser, null_mut(), 0, &mut length) } == 0
        && io::Error::last_os_error().raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32)
    {
        return Err(io::Error::last_os_error());
    }
    let mut user = vec![0usize; (length as usize).div_ceil(size_of::<usize>())];
    if unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            user.as_mut_ptr().cast(),
            length,
            &mut length,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let sid = unsafe { (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    let mut sid_text = null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut sid_text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let _sid_text = LocalMemory(sid_text.cast());
    let mut sid_length = 0;
    while unsafe { *sid_text.add(sid_length) } != 0 {
        sid_length += 1;
    }
    let sid = String::from_utf16(unsafe { std::slice::from_raw_parts(sid_text, sid_length) })
        .map_err(io::Error::other)?;
    let descriptor: Vec<u16> = format!("O:{sid}D:P(A;;GA;;;{sid})")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut security = null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            descriptor.as_ptr(),
            SDDL_REVISION_1,
            &mut security,
            null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let _security = LocalMemory(security);
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: security,
        bInheritHandle: 0,
    };
    unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(
                name,
                (&mut attributes as *mut SECURITY_ATTRIBUTES).cast(),
            )
    }
}
/// Addresses actually bound by the transport plugin.
pub(crate) struct ListenAddresses {
    pub unix_path: Option<PathBuf>,
    pub pipe_name: Option<String>,
    pub websocket_address: Option<SocketAddr>,
}

/// Owns the listeners and accept tasks. Accepting starts once the server is
/// ready and stops at shutdown intent or unload; a listener failure becomes
/// the process fault.
pub(crate) struct TransportPlugin {
    config: Arc<ServerConfig>,
    hub: Key<ConnectionHub>,
    control: Key<ServerControl>,
    /// With native storage, endpoints bind only once the database lock is held.
    storage: Option<Key<SqliteStorage>>,
}
impl TransportPlugin {
    /// The database lock is a dependency only with native storage.
    pub fn new(names: &Names<'_>, config: Arc<ServerConfig>) -> rutis::Result<Self> {
        Ok(Self {
            hub: names.key()?,
            control: names.key()?,
            storage: config
                .database_path
                .is_some()
                .then(|| names.key::<SqliteStorage>())
                .transpose()?,
            config,
        })
    }
}
fn io_failure(error: io::Error) -> Error {
    rutis::Error::from(ErrorKind::PluginFailed(Box::new(error)))
}
impl DynPlugin for TransportPlugin {
    fn name(&self) -> &str {
        "Transport"
    }
    fn injects(&self) -> Vec<TypeKey> {
        let mut keys = vec![self.hub.erase(), self.control.erase()];
        keys.extend(self.storage.as_ref().map(Key::erase));
        keys
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let hub = ctx.require_at(&self.hub)?;
            let control = ctx.require_at(&self.control)?;
            #[cfg(unix)]
            let unix = match &self.config.unix_path {
                Some(path) => Some(
                    unix_bind(path, self.config.hello_timeout)
                        .await
                        .map_err(io_failure)?,
                ),
                None => None,
            };
            #[cfg(windows)]
            let pipe = self
                .config
                .pipe_name
                .as_ref()
                .map(|name| pipe_bind(name, true))
                .transpose()
                .map_err(io_failure)?;
            let websocket = match self.config.websocket_address {
                Some(address) => Some(TcpListener::bind(address).await.map_err(io_failure)?),
                None => None,
            };
            let addresses = ListenAddresses {
                unix_path: self.config.unix_path.clone(),
                pipe_name: self.config.pipe_name.clone(),
                websocket_address: websocket
                    .as_ref()
                    .map(TcpListener::local_addr)
                    .transpose()
                    .map_err(io_failure)?,
            };
            let stop = CancellationToken::new();
            let mut listeners = JoinSet::new();
            #[cfg(unix)]
            if let Some((listener, owned)) = unix {
                let hub = hub.clone();
                let stop = stop.clone();
                listeners.spawn(async move {
                    let _owned = owned;
                    let closing = || async {
                        tokio::select! {
                            _ = stop.cancelled() => {},
                            _ = hub.control.shutdown().cancelled() => {},
                        }
                    };
                    tokio::select! {
                        biased;
                        _ = closing() => return Ok::<_, io::Error>(()),
                        opened = hub.control.opened() => if !opened { return Ok(()) },
                    }
                    loop {
                        let accepted = tokio::select! {
                            biased;
                            _ = closing() => return Ok(()),
                            accepted = listener.accept() => accepted,
                        };
                        let (stream, _) = match accepted {
                            Ok(accepted) => accepted,
                            Err(error) => {
                                tracing::warn!(%error, "Unix accept failed; retrying");
                                tokio::select! {
                                    biased;
                                    _ = closing() => return Ok(()),
                                    _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {},
                                }
                                continue;
                            }
                        };
                        let hub = hub.clone();
                        hub.tasks.clone().spawn(async move {
                            run_local(hub, stream).await;
                        });
                    }
                });
            }
            #[cfg(windows)]
            if let Some(mut listener) = pipe {
                let hub = hub.clone();
                let stop = stop.clone();
                let name = self.config.pipe_name.clone().expect("bound named pipe");
                listeners.spawn(async move {
                    let closing = || async {
                        tokio::select! {
                            _ = stop.cancelled() => {},
                            _ = hub.control.shutdown().cancelled() => {},
                        }
                    };
                    tokio::select! {
                        biased;
                        _ = closing() => return Ok::<_, io::Error>(()),
                        opened = hub.control.opened() => if !opened { return Ok(()) },
                    }
                    loop {
                        let accepted = tokio::select! {
                            biased;
                            _ = closing() => return Ok(()),
                            accepted = listener.connect() => accepted,
                        };
                        if let Err(error) = accepted {
                            tracing::warn!(%error, "Named pipe accept failed; retrying");
                            listener = pipe_bind(&name, false)?;
                            tokio::select! {
                                biased;
                                _ = closing() => return Ok(()),
                                _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {},
                            }
                            continue;
                        }
                        let stream = listener;
                        listener = pipe_bind(&name, false)?;
                        let hub = hub.clone();
                        hub.tasks.clone().spawn(async move {
                            run_local(hub, stream).await;
                        });
                    }
                });
            }
            if let Some(listener) = websocket {
                let hub = hub.clone();
                let stop = stop.clone();
                listeners.spawn(async move {
                    let shutdown = hub.control.shutdown().clone();
                    tokio::select! {
                        biased;
                        _ = stop.cancelled() => return Ok(()),
                        opened = hub.control.opened() => if !opened { return Ok(()) },
                    }
                    let router = Router::new()
                        .route("/ws", get(upgrade))
                        .route("/health", get(health))
                        .with_state(hub);
                    axum::serve(listener, router)
                        .with_graceful_shutdown(async move {
                            tokio::select! {
                                _ = stop.cancelled() => {},
                                _ = shutdown.cancelled() => {},
                            }
                        })
                        .await
                });
            }
            let watch_stop = stop.clone();
            let watcher = tokio::spawn(async move {
                let first = listeners.join_next().await;
                if !watch_stop.is_cancelled() {
                    match first {
                        Some(Ok(Err(error))) => control.fail(ServerError::Io(error)),
                        Some(Err(error)) => control.fail(ServerError::Join(error)),
                        _ => control.shutdown().cancel(),
                    }
                }
                watch_stop.cancel();
                while listeners.join_next().await.is_some() {}
            });
            ctx.effect(move || async move {
                stop.cancel();
                Ok(watcher.await?)
            })?;
            ctx.provide_as::<ListenAddresses>(Arc::new(addresses))?;
            Ok(Effect::Done)
        })
    }
}
async fn health(AxumState(hub): AxumState<Arc<ConnectionHub>>) -> impl IntoResponse {
    if hub.control.is_ready() {
        (axum::http::StatusCode::OK, "ready")
    } else {
        (axum::http::StatusCode::SERVICE_UNAVAILABLE, "closing")
    }
}
async fn upgrade(
    AxumState(hub): AxumState<Arc<ConnectionHub>>,
    ws: WebSocketUpgrade,
) -> axum::response::Response {
    if hub.control.shutdown().is_cancelled() {
        return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    ws.max_message_size(MAX_FRAME_BYTES)
        .max_frame_size(MAX_FRAME_BYTES)
        .on_upgrade(move |socket| async move {
            let tracker = hub.tasks.clone();
            let task = tracker.spawn(async move {
                run_ws(hub, socket, Entry::Bare).await;
            });
            let _ = task.await;
        })
}
enum Reader {
    #[cfg(any(unix, windows))]
    Local(BufReader<ReadHalf<LocalStream>>, Vec<u8>),
    Ws(SplitStream<WebSocket>),
}
enum Writer {
    #[cfg(any(unix, windows))]
    Local(WriteHalf<LocalStream>),
    Ws(SplitSink<WebSocket, Message>),
}
impl Reader {
    async fn next(&mut self) -> io::Result<Option<Vec<u8>>> {
        match self {
            #[cfg(any(unix, windows))]
            Self::Local(reader, bytes) => loop {
                let part = reader.fill_buf().await?;
                if part.is_empty() {
                    return if bytes.is_empty() {
                        Ok(None)
                    } else {
                        Err(io::Error::other("unterminated NDJSON frame"))
                    };
                }
                let newline = part.iter().position(|byte| *byte == b'\n');
                let n = newline.unwrap_or(part.len());
                if bytes.len() + n > MAX_FRAME_BYTES + 1 {
                    return Err(io::Error::other("NDJSON frame exceeds 16 MiB"));
                }
                bytes.extend_from_slice(&part[..n]);
                reader.consume(n + usize::from(newline.is_some()));
                if newline.is_some() {
                    if bytes.last() == Some(&b'\r') {
                        bytes.pop();
                    }
                    if bytes.len() > MAX_FRAME_BYTES {
                        return Err(io::Error::other("NDJSON frame exceeds 16 MiB"));
                    }
                    return Ok(Some(std::mem::take(bytes)));
                }
                if bytes.len() > MAX_FRAME_BYTES && bytes.last() != Some(&b'\r') {
                    return Err(io::Error::other("NDJSON frame exceeds 16 MiB"));
                }
            },
            Self::Ws(reader) => loop {
                match reader.next().await {
                    Some(Ok(Message::Text(text))) => return Ok(Some(text.as_bytes().to_vec())),
                    Some(Ok(Message::Close(_))) | None => return Ok(None),
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                    Some(Ok(Message::Binary(_))) => {
                        return Err(io::Error::other("expected a text message"));
                    }
                    Some(Err(error)) => return Err(io::Error::other(error)),
                }
            },
        }
    }
}
impl Writer {
    async fn write(&mut self, bytes: Vec<u8>) -> io::Result<()> {
        match self {
            #[cfg(any(unix, windows))]
            Self::Local(writer) => {
                writer.write_all(&bytes).await?;
                writer.write_all(b"\n").await?;
                writer.flush().await
            }
            Self::Ws(writer) => writer
                .send(Message::Text(
                    String::from_utf8(bytes).map_err(io::Error::other)?.into(),
                ))
                .await
                .map_err(io::Error::other),
        }
    }
}
#[cfg(any(unix, windows))]
async fn run_local(hub: Arc<ConnectionHub>, stream: LocalStream) {
    let (read, write) = tokio::io::split(stream);
    run_connection(
        hub,
        Reader::Local(BufReader::new(read), Vec::new()),
        Writer::Local(write),
        Entry::Local,
    )
    .await;
}
pub(crate) async fn run_ws(hub: Arc<ConnectionHub>, socket: WebSocket, entry: Entry) {
    let (write, read) = socket.split();
    run_connection(hub, Reader::Ws(read), Writer::Ws(write), entry).await;
}
async fn run_connection(
    hub: Arc<ConnectionHub>,
    mut reader: Reader,
    mut writer: Writer,
    entry: Entry,
) {
    let local = entry == Entry::Local;
    let (connection, mut outbound) = Connection::new(hub.clone(), entry);
    let closed = connection.closed.clone();
    let timeout = hub.config().write_timeout;
    let writer_task = tokio::spawn(async move {
        loop {
            let mut frame = tokio::select! {biased;_=closed.cancelled()=>break,frame=outbound.recv()=>match frame{Some(frame)=>frame,None=>break}};
            let bytes = std::mem::take(&mut frame.bytes);
            let result = tokio::time::timeout(timeout, writer.write(bytes)).await;
            if !matches!(result, Ok(Ok(()))) {
                closed.cancel();
                return None;
            }
            if let Some(ack) = frame.written.take() {
                let _ = ack.send(());
            }
        }
        Some(writer)
    });
    let deadline = tokio::time::Instant::now() + hub.config().hello_timeout;
    let mut hello = false;
    loop {
        let frame = tokio::select! {
            biased;
            _=connection.closed.cancelled()=>break,
            _=tokio::time::sleep_until(deadline),if !hello=>break,
            result=reader.next()=>match result{Ok(Some(frame))=>frame,_=>break},
        };
        let request = match decode(&frame) {
            Ok(request) => request,
            Err((id, error)) => {
                let _ = connection.sender.send(
                    Outbound::Error {
                        jsonrpc: "2.0",
                        id,
                        error,
                    },
                    false,
                );
                continue;
            }
        };
        if !hello {
            match request.call {
                Call::Hello(params)
                    if (1..=PROTOCOL_VERSION).contains(&params.protocol_version)
                        && (entry != Entry::Web || params.protocol_version == PROTOCOL_VERSION) =>
                {
                    let declared = match &params.client_kind {
                        ClientKind::Cli => DeclaredClientKind::Cli,
                        ClientKind::Web => DeclaredClientKind::Web,
                        ClientKind::Other => DeclaredClientKind::Other,
                    };
                    connection
                        .client
                        .set(ClientIdentity {
                            channel: ClientChannel::declared(
                                declared,
                                params.client_name.as_deref(),
                            ),
                        })
                        .ok();
                    let instance = hub
                        .with_rpc(|rpc| {
                            if rpc.stopping.is_cancelled() {
                                return Err(RpcError::reloading());
                            }
                            let instance = rpc.index.server_instance_id().to_owned();
                            connection
                                .instance
                                .set(instance.clone())
                                .expect("first handshake");
                            Ok((
                                instance,
                                rpc.management
                                    .listed()
                                    .into_iter()
                                    .map(|decl| decl.name)
                                    .collect::<Vec<_>>(),
                            ))
                        })
                        .and_then(|instance| instance);
                    let (instance_id, methods) = match instance {
                        Ok(instance) => instance,
                        Err(error) => {
                            let _ = connection
                                .sender
                                .send(Outbound::result(request.id, Err(error)), false);
                            continue;
                        }
                    };
                    let _ = connection.sender.send(
                        Outbound::result(
                            request.id,
                            Ok(Reply::Hello(HelloResult {
                                instance_id,
                                server_version: SERVER_VERSION,
                                protocol_version: params.protocol_version,
                                daemon_profile: hub.config().daemon_profile.clone(),
                                features: (params.protocol_version >= 3).then(|| {
                                    let trusted = entry != Entry::Bare;
                                    let mut features = if trusted
                                        || !hub.config().protected_profile()
                                    {
                                        let mut methods =
                                            vec!["session.summaries", "daemon.shutdown.completed"];
                                        if hub.config().database_path.is_some() {
                                            methods.push("session.catalog");
                                            methods.push("session.catalog.cancel");
                                            methods.push("session.catalog.origins");
                                            methods.push("session.catalog.workspaces");
                                        }
                                        methods
                                    } else {
                                        vec![]
                                    };
                                    if hub.config().session_execution
                                        && (trusted || !hub.config().protected_profile())
                                    {
                                        features.extend([
                                            "session.history",
                                            "session.input_lookup",
                                            "session.create",
                                            "session.delete",
                                            "session.model.progress",
                                            "session.methods",
                                            "session.branch_end",
                                            "session.create.origin",
                                        ]);
                                        if hub.config().database_path.is_some() {
                                            features.push("session.fork");
                                            features.push("session.history.latestTurn");
                                        }
                                    }
                                    if trusted && hub.config().local_execution() {
                                        features.extend(LOCAL_EXECUTION_METHODS);
                                    }
                                    if local {
                                        features.extend(WEB_ENTRY_METHODS);
                                        features.extend(UI_HOST_METHODS);
                                        features.push("ui.leaf.2");
                                    }
                                    let mut features: Vec<String> =
                                        features.into_iter().map(str::to_owned).collect();
                                    features.extend(
                                        methods
                                            .into_iter()
                                            .filter(|method| {
                                                local
                                                    || WEB_MANAGEMENT_METHODS
                                                        .contains(&method.as_str())
                                            })
                                            .filter(|_| trusted),
                                    );
                                    features
                                }),
                            })),
                        ),
                        false,
                    );
                    connection
                        .version
                        .set(params.protocol_version)
                        .expect("first handshake");
                    hello = true;
                }
                Call::Hello(_) => {
                    if let Ok(Some(ack)) = connection.sender.send(
                        Outbound::result(
                            request.id,
                            Err(RpcError::app(
                                -32000,
                                "protocol_version",
                                "Unsupported protocol version",
                            )),
                        ),
                        true,
                    ) {
                        let _ = tokio::time::timeout(timeout, ack).await;
                    }
                    break;
                }
                _ => {
                    let _ = connection.sender.send(
                        Outbound::result(
                            request.id,
                            Err(RpcError::wire(-32600, "hello is required")),
                        ),
                        false,
                    );
                }
            }
        } else if hub.control.shutdown().is_cancelled() {
            let _ = connection.sender.send(
                Outbound::result(
                    request.id,
                    Err(RpcError::wire(-32603, "Server is shutting down")),
                ),
                false,
            );
        } else {
            connection.dispatch(request);
        }
    }
    connection.finish().await;
    if let Ok(Some(mut writer)) = writer_task.await
        && let Some(id) = connection
            .shutdown_reply
            .lock()
            .expect("shutdown reply")
            .take()
    {
        hub.control.defer_shutdown_reply(Box::new(move |success| {
            Box::pin(async move {
                let result = if success {
                    Ok(Reply::empty())
                } else {
                    Err(RpcError::app(
                        -32000,
                        "shutdown_failed",
                        "Daemon cleanup failed",
                    ))
                };
                if let Ok(bytes) = serde_json::to_vec(&Outbound::result(id, result)) {
                    let _ = tokio::time::timeout(timeout, writer.write(bytes)).await;
                }
            })
        }));
    }
}
