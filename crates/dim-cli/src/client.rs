use crate::{Result, args::Options};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    ffi::OsString,
    io,
    path::{Path, PathBuf},
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
    async fn open(endpoint: &Path, _server: &Server, _starting: bool) -> io::Result<Self> {
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

    /// Checks the daemon can serve this CLI and returns its version.
    async fn hello(&mut self, profile: Option<&DaemonProfile>) -> Result<String> {
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
        Ok(hello.server_version)
    }

    /// Whether the server's hello offered `feature` (a method name, or a
    /// capability such as `session.fork`).
    pub fn offers(&self, feature: &str) -> bool {
        self.features.iter().any(|offered| offered == feature)
    }
}

/// How the CLI starts the App Server: this executable's `app-server`
/// subcommand (the single `dimcode` binary), or an explicit `--server` binary.
pub struct Server {
    pub program: std::path::PathBuf,
    embedded: bool,
}

impl Server {
    pub fn subcommand(&self) -> &'static [&'static str] {
        if self.embedded { &["app-server"] } else { &[] }
    }
}

/// The App Server the CLI runs: `--server`, or this executable's own.
pub fn server(options: &Options) -> Result<Server> {
    Ok(match &options.server {
        Some(path) => Server {
            program: path.clone(),
            embedded: false,
        },
        None => Server {
            program: std::env::current_exe()?,
            embedded: true,
        },
    })
}

/// Where the Web workspace's files are, when they are installed: the
/// `DIMCODE_WEB_ASSETS` directory (the npm wrapper names the installed
/// `dimcode-web` package's, `./dev` and frontend development name their
/// own), else `web/` beside the executable's `bin/` (a native archive
/// unpacked next to it), else `web/` in the product home. A directory
/// counts when it holds `index.html`.
/// What the daemon runs TypeScript plugins with.
struct PluginRuntime {
    node: PathBuf,
    js_runtime: PathBuf,
    defaults: PathBuf,
}

/// The default config file in `DIMCODE_PLUGIN_DEFAULTS` (the npm wrapper sets
/// it to the one it ships; a source checkout names `runtime/plugins.default.json`),
/// the rutis runtime package in `DIMCODE_JS_RUNTIME` or beside that file, and
/// Node in `DIMCODE_NODE` (the wrapper's own) or on `PATH`. Without a default
/// config file the daemon runs only the plugins compiled into it.
fn plugin_runtime() -> Option<PluginRuntime> {
    let defaults = PathBuf::from(std::env::var_os("DIMCODE_PLUGIN_DEFAULTS")?);
    let defaults = std::fs::canonicalize(defaults).ok()?;
    let js_runtime = match std::env::var_os("DIMCODE_JS_RUNTIME") {
        Some(path) => PathBuf::from(path),
        None => defaults
            .parent()?
            .join("node_modules/@arcships/rutis-runtime"),
    };
    let node = match std::env::var_os("DIMCODE_NODE") {
        Some(path) => PathBuf::from(path),
        None => std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|directory| directory.join(if cfg!(windows) { "node.exe" } else { "node" }))
            .find(|candidate| candidate.is_file())?,
    };
    Some(PluginRuntime {
        node: std::fs::canonicalize(node).ok()?,
        js_runtime: std::fs::canonicalize(js_runtime).ok()?,
        defaults,
    })
}

pub fn web_assets(executable: &Path, home: &Path) -> Option<PathBuf> {
    web_assets_in(std::env::var_os("DIMCODE_WEB_ASSETS"), executable, home)
}

fn web_assets_in(configured: Option<OsString>, executable: &Path, home: &Path) -> Option<PathBuf> {
    let candidates = [
        configured.map(PathBuf::from),
        executable
            .parent()
            .and_then(Path::parent)
            .map(|root| root.join("web")),
        Some(home.join("web")),
    ];
    candidates
        .into_iter()
        .flatten()
        .find(|directory| directory.join("index.html").is_file())
}

/// What to tell the user when the daemon cannot serve the Web workspace, as
/// two sentences without their full stops; `None` for any other failure. A
/// daemon started with no frontend while the CLI now finds one beside the
/// executable or in `DIMCODE_WEB_ASSETS` keeps serving without it until it
/// restarts; otherwise `dimcode web install` brings this version's.
pub fn web_hint(error: &crate::Error, executable: &Path, home: &Path) -> Option<(String, String)> {
    let install = "Install it with `dimcode web install`".to_owned();
    match app_error(error)? {
        "web_assets_missing" if web_assets(executable, home).is_some() => Some((
            "The Web workspace was installed after the daemon started".into(),
            "Restart the daemon with `dimcode daemon stop`, then try again".into(),
        )),
        "web_assets_missing" => Some(("The Web workspace is not installed".into(), install)),
        "web_assets_incompatible" => Some((
            format!(
                "The installed Web workspace does not fit dimcode {}",
                env!("CARGO_PKG_VERSION")
            ),
            install,
        )),
        _ => None,
    }
}

/// The name the daemon shows in process lists.
pub const SERVER_NAME: &str = if cfg!(windows) {
    "dimcode-server.exe"
} else {
    "dimcode-server"
};

/// `<native_home>/dimcode-server`, a link to `exe`, kept pointing at the
/// executable that starts the daemon: a hard link, whose name the kernel
/// takes as the process name, else a symbolic one, which still names the
/// command line; `exe` itself when neither can be made (the name is
/// cosmetic, the daemon runs either way).
fn server_link(native_home: &Path, exe: &Path) -> PathBuf {
    let link = native_home.join(SERVER_NAME);
    if links_to(&link, exe) {
        return link;
    }
    let _ = std::fs::remove_file(&link);
    if std::fs::hard_link(exe, &link).is_ok() {
        return link;
    }
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(exe, &link);
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_file(exe, &link);
    match made {
        Ok(()) => link,
        Err(_) => exe.to_path_buf(),
    }
}

/// Whether `link` still names `exe`: a symbolic link to its path, or a
/// hard link to the same file (a replaced executable is a new file).
fn links_to(link: &Path, exe: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(link) else {
        return false;
    };
    if meta.file_type().is_symlink() {
        return std::fs::read_link(link).is_ok_and(|target| target == exe);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(exe)
            .is_ok_and(|target| target.dev() == meta.dev() && target.ino() == meta.ino())
    }
    #[cfg(not(unix))]
    {
        // Without inode identity the hard link is remade every start.
        false
    }
}

/// Stops the daemon `client` is connected to and waits for it to close;
/// `None` means no daemon runs.
pub async fn stop(client: Option<Client>) -> Result<i32> {
    let Some(client) = client else {
        println!("Daemon is not running.");
        return Ok(0);
    };
    shutdown(client).await?;
    println!("Daemon stopped.");
    Ok(0)
}

/// Stops the daemon behind `client` and waits until it has settled.
pub async fn shutdown(mut client: Client) -> Result<()> {
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
    Ok(())
}

/// The connection to the daemon of `options.home` after `hello`. Without a
/// daemon, starts one when `start` and waits for it; else `None`.
pub async fn connect_inner(options: &Options, start: bool) -> Result<Option<Client>> {
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
    let server = server(options)?;
    let binary = &server.program;
    match Client::open(&endpoint, &server, false).await {
        Ok(mut client) => {
            let version = client.hello(profile.as_ref()).await?;
            // A daemon this executable would start but of another version
            // (left running across an upgrade) is replaced: it would serve
            // that version's tools and behavior to this CLI.
            if !(start && server.embedded && version != env!("CARGO_PKG_VERSION")) {
                return Ok(Some(client));
            }
            shutdown(client).await?;
        }
        Err(error) if missing(&error) => {}
        Err(error) => return Err(error.into()),
    }
    if !start {
        return Ok(None);
    }
    if !binary.is_file() {
        return Err(format!(
            "App Server binary is missing at {}; supply --server PATH",
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
    // The embedded server runs through a link named after it, so process
    // lists tell the daemon from the clients that share its executable.
    let program = if server.embedded {
        server_link(&native_home, binary)
    } else {
        binary.clone()
    };
    let mut command = Command::new(&program);
    command.args(server.subcommand());
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
        let product_home = std::fs::canonicalize(&options.home)?;
        command.arg("--product-home").arg(&product_home);
    }
    // The plugin runtime: Node, the rutis runtime package and the default
    // config file whose plugins load by default (PluginManager §7).
    if let Some(runtime) = plugin_runtime() {
        command
            .arg("--node")
            .arg(&runtime.node)
            .arg("--js-runtime")
            .arg(&runtime.js_runtime)
            .arg("--plugin-defaults")
            .arg(&runtime.defaults);
    }
    // The Web workspace is a separate artifact the daemon serves from a
    // directory (web-workspace.md §2.4).
    if let Some(path) = web_assets(binary, &options.home) {
        command.arg("--web-assets").arg(path);
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
        // Only the newly spawned owner's socket may be awaiting chmod.
        match Client::open(&endpoint, &server, true).await {
            Ok(mut client) => {
                client.hello(profile.as_ref()).await?;
                return Ok(Some(client));
            }
            Err(error)
                if missing(&error) || cfg!(unix) && error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error.into()),
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

#[cfg(test)]
mod web_assets_tests {
    use super::web_assets_in;

    /// The configured directory wins, then the one beside `bin/`, then the
    /// product home's; a directory without `index.html` does not count.
    #[test]
    fn the_frontend_is_found_in_order() {
        let root = std::env::temp_dir().join(format!("dim-web-lookup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (configured, beside, home) = (root.join("dev"), root.join("pkg"), root.join("home"));
        let exe = beside.join("bin").join("dimcode");
        for dir in [&configured, &beside.join("web"), &home.join("web")] {
            std::fs::create_dir_all(dir).unwrap();
        }
        assert_eq!(
            web_assets_in(Some(configured.clone().into()), &exe, &home),
            None
        );
        std::fs::write(home.join("web/index.html"), "").unwrap();
        assert_eq!(
            web_assets_in(Some(configured.clone().into()), &exe, &home),
            Some(home.join("web"))
        );
        std::fs::write(beside.join("web/index.html"), "").unwrap();
        assert_eq!(web_assets_in(None, &exe, &home), Some(beside.join("web")));
        std::fs::write(configured.join("index.html"), "").unwrap();
        assert_eq!(
            web_assets_in(Some(configured.clone().into()), &exe, &home),
            Some(configured.clone())
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
