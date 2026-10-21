use crate::{Result, args::Options};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    io,
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};
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

#[derive(Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
enum DaemonProfile {
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

pub struct Client {
    read: BufReader<Box<dyn AsyncRead + Unpin + Send>>,
    write: Box<dyn AsyncWrite + Unpin + Send>,
    next_id: u64,
    frame: Vec<u8>,
    pub notices: VecDeque<Message>,
    shutdown_completion: bool,
    /// The methods and capabilities the server's hello listed.
    features: Vec<String>,
    #[cfg(windows)]
    _bridge: tokio::process::Child,
}
impl Client {
    async fn open(endpoint: &Path, _binary: &Path) -> io::Result<Self> {
        #[cfg(unix)]
        let stream = {
            use std::os::unix::fs::FileTypeExt;
            use std::os::unix::fs::MetadataExt;
            let metadata = std::fs::symlink_metadata(endpoint)?;
            if !metadata.file_type().is_socket()
                || metadata.mode() & 0o077 != 0
                || metadata.uid() != unsafe { libc::geteuid() }
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
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
            let mut bridge = tokio::process::Command::new(_binary)
                .arg("--connect-pipe")
                .arg(endpoint)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
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
            read: BufReader::new(Box::new(read)),
            write: Box::new(write),
            next_id: 0,
            frame: Vec::new(),
            notices: VecDeque::new(),
            shutdown_completion: false,
            features: Vec::new(),
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
                return Err("App Server connection closed; the submitted input may have been accepted. Resume the session instead of resending automatically".into());
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

    pub async fn response(&mut self, id: u64) -> Result<Value> {
        loop {
            match self.receive().await? {
                Message::Response {
                    id: received,
                    result,
                } if received == id => return result.map_err(Into::into),
                notice @ Message::Notice { .. } => self.notices.push_back(notice),
                _ => return Err("Unexpected App Server response identity".into()),
            }
        }
    }

    pub async fn typed<T: DeserializeOwned>(&mut self, method: &str, params: Value) -> Result<T> {
        let value = self.rpc(method, params).await?;
        serde_json::from_value(value).map_err(|_| format!("Invalid {method} response").into())
    }

    async fn hello(&mut self, profile: Option<&DaemonProfile>) -> Result<()> {
        let hello: Hello = tokio::time::timeout(Duration::from_secs(5), self.typed("hello", json!({"protocolVersion":PROTOCOL_VERSION,"clientKind":"cli","clientName":"dimcode"}))).await??;
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
        if profile.is_some_and(|profile| &hello.daemon_profile != profile) {
            return Err("Existing daemon uses a different model profile; use a separate --home for --model-config".into());
        }
        self.shutdown_completion = hello
            .features
            .iter()
            .any(|feature| feature == "daemon.shutdown.completed");
        self.features = hello.features;
        Ok(())
    }
}

pub async fn connect(options: &Options) -> Result<Client> {
    connect_inner(options, true)
        .await?
        .ok_or_else(|| "App Server did not start".into())
}

pub async fn stop(options: &Options) -> Result<i32> {
    let Some(mut client) = connect_inner(options, false).await? else {
        println!("Daemon is not running.");
        return Ok(0);
    };
    if !client.shutdown_completion {
        return Err("Daemon does not support verified shutdown completion; stop the older daemon with its matching CLI before restarting".into());
    }
    tokio::time::timeout(Duration::from_secs(30), async {
        client.rpc("daemon.shutdown", json!({})).await?;
        loop {
            let buffer = client.read.fill_buf().await?;
            if buffer.is_empty() {
                break;
            }
            let size = buffer.len();
            client.read.consume(size);
        }
        #[cfg(windows)]
        if !client._bridge.wait().await?.success() {
            return Err("Daemon transport bridge did not close successfully".into());
        }
        Ok::<_, crate::Error>(())
    })
    .await
    .map_err(|_| "Daemon shutdown timed out; it is still settling and was not force-killed")??;
    println!("Daemon stopped.");
    Ok(0)
}

async fn connect_inner(options: &Options, start: bool) -> Result<Option<Client>> {
    let profile = if !start {
        None
    } else {
        Some(match &options.model_config {
            Some(path) => DaemonProfile::Configured {
                model_config_sha256: format!("{:x}", Sha256::digest(std::fs::read(path)?)),
            },
            None => DaemonProfile::Product,
        })
    };
    let native_home = options.home.join("rust");
    if start {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&native_home)?;
    }
    let native_home = match std::fs::canonicalize(native_home) {
        Ok(path) => path,
        Err(error) if !start && error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::metadata(&native_home)?;
        if metadata.mode() & 0o077 != 0 || metadata.uid() != unsafe { libc::geteuid() } {
            return Err("Rust home must be private (0700) and owned by the current user".into());
        }
    }
    #[cfg(unix)]
    let endpoint = native_home.join("daemon.sock");
    #[cfg(windows)]
    let endpoint = {
        let key = format!(
            "{:x}",
            Sha256::digest(format!("dimcode:{}", native_home.display()))
        );
        std::path::PathBuf::from(format!(r"\\.\pipe\dim-rust-{}", &key[..32]))
    };
    let binary = match &options.server {
        Some(path) => path.clone(),
        None => std::env::current_exe()?
            .parent()
            .ok_or("CLI executable has no parent directory")?
            .join(if cfg!(windows) {
                "dim-app-server.exe"
            } else {
                "dim-app-server"
            }),
    };
    match Client::open(&endpoint, &binary).await {
        Ok(mut client) => {
            client.hello(profile.as_ref()).await?;
            return Ok(Some(client));
        }
        Err(error) if missing(&error) => {}
        Err(error) => return Err(error.into()),
    }
    if !start {
        return Ok(None);
    }
    if !binary.is_file() {
        return Err(format!(
            "App Server binary is missing at {}; build dim-app-server or supply --server PATH",
            binary.display()
        )
        .into());
    }
    let log_path = native_home.join("daemon.log");
    let mut log = std::fs::OpenOptions::new();
    log.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        log.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let log = log.open(&log_path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = log.metadata()?;
        if !metadata.is_file()
            || metadata.mode() & 0o077 != 0
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            return Err("Daemon log must be a private file owned by the current user".into());
        }
    }
    let mut command = Command::new(&binary);
    command
        .current_dir(&options.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    command
        .arg(if cfg!(windows) { "--pipe" } else { "--unix" })
        .arg(&endpoint)
        .args(["--no-ws", "--home"])
        .arg(&native_home);
    if let Some(path) = &options.model_config {
        command.arg("--model-config").arg(path);
    } else {
        command
            .arg("--product-home")
            .arg(std::fs::canonicalize(&options.home)?);
        if let (Some(plugin), Some(node), Some(runtime)) =
            (&options.plugin_dir, &options.node, &options.js_runtime)
        {
            command
                .arg("--plugin-dir")
                .arg(plugin)
                .arg("--node")
                .arg(node)
                .arg("--js-runtime")
                .arg(runtime);
        } else if let Some(root) = binary.parent().and_then(Path::parent) {
            let plugin = root.join("plugins/dim-oauth");
            if plugin.join("dim-plugin.json").is_file() {
                command
                    .arg("--plugin-dir")
                    .arg(plugin)
                    .arg("--node")
                    .arg(root.join(if cfg!(windows) {
                        "runtime/node/node.exe"
                    } else {
                        "runtime/node/bin/node"
                    }))
                    .arg("--js-runtime")
                    .arg(root.join("runtime/rutis/node_modules/@arcships/rutis-runtime"));
            }
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000 | 0x00000200);
    }
    let mut child = command.spawn()?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        #[cfg(unix)]
        let permissions_pending = {
            use std::os::unix::fs::{FileTypeExt, MetadataExt};
            std::fs::symlink_metadata(&endpoint).is_ok_and(|metadata| {
                metadata.file_type().is_socket()
                    && metadata.uid() == unsafe { libc::geteuid() }
                    && metadata.mode() & 0o077 != 0
            })
        };
        #[cfg(not(unix))]
        let permissions_pending = false;
        // Only the newly spawned owner's socket may be awaiting chmod.
        if !permissions_pending {
            match Client::open(&endpoint, &binary).await {
                Ok(mut client) => {
                    client.hello(profile.as_ref()).await?;
                    return Ok(Some(client));
                }
                Err(error) if missing(&error) => {}
                Err(error) => return Err(error.into()),
            }
        }

        if let Some(status) = child.try_wait()? {
            return Err(format!("App Server exited ({status}); see {}", log_path.display()).into());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("App Server startup timed out; see {}", log_path.display()).into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn missing(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    ) || cfg!(windows) && error.raw_os_error() == Some(231)
}
