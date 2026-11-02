//! Public RPC v3 transport shared by native Dim clients.

pub mod callback;
pub mod question;
pub mod wire;

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{collections::VecDeque, io, path::PathBuf, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

const PROTOCOL_VERSION: u32 = 3;
const FRAME_LIMIT: usize = 16 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Hello {
    instance_id: String,
    server_version: String,
    protocol_version: u32,
    features: Vec<String>,
    daemon_profile: DaemonProfile,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum DaemonProfile {
    Product,
    Configured {
        #[serde(rename = "modelConfigSha256")]
        model_config_sha256: String,
    },
    Mock,
}

#[derive(Debug, Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    pub data: Option<Value>,
}

#[derive(Debug)]
pub struct RequestTooLarge;
impl std::fmt::Display for RequestTooLarge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("App Server request exceeds 16 MiB after JSON encoding")
    }
}
impl std::error::Error for RequestTooLarge {}
impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (RPC {}", self.message, self.code)?;
        if let Some(code) = self
            .data
            .as_ref()
            .and_then(|data| data.get("appError"))
            .and_then(Value::as_str)
        {
            write!(f, ", {code}")?;
        }
        write!(f, ")")
    }
}
impl std::error::Error for RpcError {}
/// The App Server error code of a failed call, if the error is one.
pub fn app_error(error: &crate::Error) -> Option<&str> {
    error
        .downcast_ref::<RpcError>()?
        .data
        .as_ref()?
        .get("appError")?
        .as_str()
}

#[derive(Debug)]
pub enum Message {
    Response {
        id: u64,
        result: std::result::Result<Value, RpcError>,
    },
    Notice {
        method: String,
        params: Value,
    },
}

/// Endpoint, identity and profile of one independently owned connection.
#[derive(Clone, Debug)]
pub struct ConnectionConfig {
    pub endpoint: PathBuf,
    pub server: Server,
    pub profile: Option<DaemonProfile>,
    pub client_kind: &'static str,
    pub client_name: &'static str,
}

pub struct Client {
    config: ConnectionConfig,
    read: BufReader<Box<dyn AsyncRead + Unpin + Send>>,
    write: Box<dyn AsyncWrite + Unpin + Send>,
    next_id: u64,
    frame: Vec<u8>,
    pub notices: VecDeque<Message>,
    /// The methods and capabilities the server's hello listed.
    features: Vec<String>,
    #[cfg(windows)]
    _bridge: tokio::process::Child,
    instance_id: String,
    server_version: String,
}
impl Client {
    /// Opens the protected transport. Only a host that just started its daemon
    /// passes `starting` while that owner finishes securing its endpoint.
    pub async fn open(config: &ConnectionConfig, _starting: bool) -> io::Result<Self> {
        let endpoint = &config.endpoint;
        #[cfg(windows)]
        let _server = &config.server;
        #[cfg(unix)]
        let stream = {
            use std::os::unix::fs::FileTypeExt;
            use std::os::unix::fs::MetadataExt;
            let metadata = std::fs::symlink_metadata(endpoint)?;
            let owned_socket =
                metadata.file_type().is_socket() && metadata.uid() == unsafe { libc::geteuid() };
            if !owned_socket || metadata.mode() & 0o077 != 0 {
                return Err(io::Error::new(
                    if _starting && owned_socket {
                        io::ErrorKind::WouldBlock
                    } else {
                        io::ErrorKind::PermissionDenied
                    },
                    "Daemon endpoint must be a private socket owned by the current user",
                ));
            }
            let stream = tokio::net::UnixStream::connect(endpoint).await?;
            if stream.peer_cred()?.uid() != unsafe { libc::geteuid() } {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Daemon belongs to a different user",
                ));
            }
            stream
        };
        #[cfg(windows)]
        let (read, write, bridge) = {
            let mut bridge = tokio::process::Command::new(&_server.program)
                .args(_server.subcommand())
                .arg("--connect-pipe")
                .arg(endpoint)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true)
                .spawn()?;
            let mut readiness = BufReader::new(bridge.stderr.take().expect("bridge stderr"));
            let mut line = String::new();
            tokio::time::timeout(Duration::from_secs(5), readiness.read_line(&mut line))
                .await
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::TimedOut, "Pipe authentication timed out")
                })??;
            #[derive(Deserialize)]
            struct Ready {
                status: String,
                code: Option<String>,
            }
            let result: Ready = serde_json::from_str(&line).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Invalid authenticated pipe bridge response",
                )
            })?;
            if result.status != "ready" {
                return Err(io::Error::new(
                    if result.code.as_deref() == Some("ENOENT") {
                        io::ErrorKind::NotFound
                    } else {
                        io::ErrorKind::PermissionDenied
                    },
                    "Authenticated pipe connection failed",
                ));
            }
            (
                bridge.stdout.take().expect("bridge stdout"),
                bridge.stdin.take().expect("bridge stdin"),
                bridge,
            )
        };
        #[cfg(unix)]
        let (read, write) = tokio::io::split(stream);
        Ok(Self {
            config: config.clone(),
            read: BufReader::new(Box::new(read)),
            write: Box::new(write),
            next_id: 0,
            frame: Vec::new(),
            notices: VecDeque::new(),
            features: Vec::new(),
            instance_id: String::new(),
            server_version: String::new(),
            #[cfg(windows)]
            _bridge: bridge,
        })
    }

    pub async fn send(&mut self, method: &str, params: Value) -> Result<u64> {
        self.next_id += 1;
        let id = self.next_id;
        let mut bytes = serde_json::to_vec(
            &json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}),
        )?;
        if bytes.len() > FRAME_LIMIT {
            return Err(RequestTooLarge.into());
        }
        bytes.push(b'\n');
        self.write.write_all(&bytes).await?;
        self.write.flush().await?;
        Ok(id)
    }

    async fn receive(&mut self) -> Result<Message> {
        loop {
            let buffer = self.read.fill_buf().await?;
            if buffer.is_empty() {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof,"App Server connection closed; the submitted input may have been accepted. Resume the session instead of resending automatically").into());
            }
            let end = buffer.iter().position(|byte| *byte == b'\n');
            let size = end.map_or(buffer.len(), |index| index + 1);
            if self.frame.len() + size > FRAME_LIMIT {
                return Err("App Server frame exceeds 16 MiB".into());
            }
            self.frame.extend_from_slice(&buffer[..size]);
            self.read.consume(size);
            if end.is_some() {
                break;
            }
        }
        #[derive(Deserialize)]
        struct Envelope {
            jsonrpc: String,
            id: Option<u64>,
            result: Option<Value>,
            error: Option<RpcError>,
            method: Option<String>,
            params: Option<Value>,
        }
        let message: Envelope = serde_json::from_slice(&std::mem::take(&mut self.frame))
            .map_err(|_| "Invalid App Server JSON-RPC frame")?;
        if message.jsonrpc != "2.0" {
            return Err("Unsupported App Server envelope".into());
        }
        match (
            message.id,
            message.result,
            message.error,
            message.method,
            message.params,
        ) {
            (Some(id), Some(result), None, None, None) => Ok(Message::Response {
                id,
                result: Ok(result),
            }),
            (Some(id), None, Some(error), None, None) => Ok(Message::Response {
                id,
                result: Err(error),
            }),
            (None, None, None, Some(method), Some(params)) => {
                Ok(Message::Notice { method, params })
            }
            _ => Err("Invalid App Server response identity".into()),
        }
    }

    /// Whether the daemon advertised every named method.
    pub fn supports(&self, methods: &[&str]) -> bool {
        methods
            .iter()
            .all(|method| self.features.iter().any(|feature| feature == method))
    }

    pub async fn next(&mut self) -> Result<Message> {
        if let Some(message) = self.notices.pop_front() {
            return Ok(message);
        }
        self.receive().await
    }

    pub async fn rpc(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.send(method, params).await?;
        self.response(id).await
    }

    /// Waits for the reply to `id`. Replies to other requests still in
    /// flight (sent with [`Client::send`]) and notices are kept, in order,
    /// for [`Client::next`].
    pub async fn response(&mut self, id: u64) -> Result<Value> {
        if let Some(index) = self.notices.iter().position(
            |message| matches!(message, Message::Response { id: queued, .. } if *queued == id),
        ) {
            let Some(Message::Response { result, .. }) = self.notices.remove(index) else {
                unreachable!("matched a response");
            };
            return result.map_err(Into::into);
        }
        loop {
            match self.receive().await? {
                Message::Response {
                    id: received,
                    result,
                } if received == id => return result.map_err(Into::into),
                message @ Message::Notice { .. } => self.notices.push_back(message),
                message @ Message::Response { id: received, .. } if received <= self.next_id => {
                    self.notices.push_back(message)
                }
                _ => return Err("Unexpected App Server response identity".into()),
            }
        }
    }

    pub async fn typed<T: DeserializeOwned>(&mut self, method: &str, params: Value) -> Result<T> {
        let value = self.rpc(method, params).await?;
        serde_json::from_value(value).map_err(|_| format!("Invalid {method} response").into())
    }

    /// Negotiates RPC v3, checks the host's explicit model profile and returns the server version.
    pub async fn hello(&mut self) -> Result<String> {
        let hello: Hello = tokio::time::timeout(Duration::from_secs(5), self.typed("hello", json!({"protocolVersion":PROTOCOL_VERSION,"clientKind":self.config.client_kind,"clientName":self.config.client_name}))).await??;
        if hello.protocol_version != PROTOCOL_VERSION
            || hello.instance_id.is_empty()
            || hello.server_version.is_empty()
            || !hello
                .features
                .iter()
                .any(|feature| feature == "session.create")
            || !hello
                .features
                .iter()
                .any(|feature| feature == "session.history")
        {
            return Err("Incompatible daemon; use an isolated --home or explicitly restart the daemon you own".into());
        }
        if self
            .config
            .profile
            .as_ref()
            .is_some_and(|profile| &hello.daemon_profile != profile)
        {
            return Err("Existing daemon uses a different model profile; use a separate --home for --model-config".into());
        }
        self.features = hello.features;
        self.instance_id = hello.instance_id;
        self.server_version = hello.server_version;
        Ok(self.server_version.clone())
    }

    /// Whether the server's hello offered `feature` (a method name, or a
    /// capability such as `session.fork`).
    pub fn offers(&self, feature: &str) -> bool {
        self.features.iter().any(|offered| offered == feature)
    }

    pub fn features(&self) -> &[String] {
        &self.features
    }
    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }
    /// The version the daemon's hello reported, empty before the hello.
    pub fn server_version(&self) -> &str {
        &self.server_version
    }
    /// Reconnects without starting or replacing a daemon.
    pub async fn connect(config: &ConnectionConfig) -> Result<Self> {
        let mut client = Self::open(config, false).await?;
        client.hello().await?;
        Ok(client)
    }

    pub fn connection_config(&self) -> &ConnectionConfig {
        &self.config
    }

    /// Waits for transport EOF and the authenticated Windows bridge to exit.
    pub async fn wait_closed(mut self) -> Result<()> {
        loop {
            let buffer = self.read.fill_buf().await?;
            if buffer.is_empty() {
                break;
            }
            let size = buffer.len();
            self.read.consume(size);
        }
        #[cfg(windows)]
        if !self._bridge.wait().await?.success() {
            return Err("Daemon transport bridge did not close successfully".into());
        }
        Ok(())
    }
}

/// Executable used for the authenticated Windows pipe bridge.
#[derive(Clone, Debug)]
pub struct Server {
    pub program: std::path::PathBuf,
    pub embedded: bool,
}

impl Server {
    pub fn subcommand(&self) -> &'static [&'static str] {
        if self.embedded { &["app-server"] } else { &[] }
    }
}
