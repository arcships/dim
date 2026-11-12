use crate::{
    Result,
    args::{Options, SERVER_REQUIREMENT},
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

pub use dim_client::{
    Client, ConnectionConfig, DaemonProfile, Message, RequestTooLarge, RpcError, Server, app_error,
};

/// The App Server the CLI runs: `--server`, or this executable's own.
pub fn server(options: &Options) -> Result<Server> {
    let Some(path) = &options.server else {
        return Ok(Server {
            program: std::env::current_exe()?,
            embedded: true,
        });
    };
    if is_this_executable(path) {
        return Err(format!(
            "{SERVER_REQUIREMENT}, not this executable ({}); without --server, this \
             executable's app-server subcommand runs",
            path.display()
        )
        .into());
    }
    Ok(Server {
        program: path.clone(),
        embedded: false,
    })
}

fn is_this_executable(path: &Path) -> bool {
    std::env::current_exe().is_ok_and(|executable| same_file(path, &executable))
}

/// Whether `a` and `b` name the same file: links and symlinks included on
/// Unix, symlinks included elsewhere.
fn same_file(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let (Ok(a), Ok(b)) = (std::fs::metadata(a), std::fs::metadata(b)) else {
            return false;
        };
        a.dev() == b.dev() && a.ino() == b.ino()
    }
    #[cfg(not(unix))]
    {
        let (Ok(a), Ok(b)) = (std::fs::canonicalize(a), std::fs::canonicalize(b)) else {
            return false;
        };
        a == b
    }
}

/// Where the Web workspace's files are, when they are installed: the
/// `DIMCODE_WEB_ASSETS` directory (the npm wrapper names the installed
/// `dimcode-web` package's, `./dev` and frontend development name their
/// own), else `web/` beside the executable's `bin/` (a native archive
/// unpacked next to it), else `web/` in the product home. A directory
/// counts when it holds `index.html`.
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

/// The default plugin configuration file (PluginManager §2.3):
/// `DIMCODE_DEFAULT_PLUGINS`, else `default-plugins.json` beside the
/// executable's `bin/`, next to the `plugins/` directory it lists.
fn default_plugins(executable: &Path) -> Option<PathBuf> {
    [
        std::env::var_os("DIMCODE_DEFAULT_PLUGINS").map(PathBuf::from),
        executable
            .parent()
            .and_then(Path::parent)
            .map(|root| root.join("default-plugins.json")),
    ]
    .into_iter()
    .flatten()
    .find(|file| file.is_file())
}

/// What to tell the user when the daemon cannot serve the Web workspace, as
/// two sentences without their full stops; `None` for any other failure. A
/// daemon started with no frontend while the CLI now finds one beside the
/// executable or in `DIMCODE_WEB_ASSETS` keeps serving without it until it
/// restarts; otherwise a released build installs this version's with
/// `dimcode web install`, while a build from source, which has none
/// published, is pointed at `DIMCODE_WEB_ASSETS` or the embedded-web build.
pub fn web_hint(error: &crate::Error, executable: &Path, home: &Path) -> Option<(String, String)> {
    let install = if crate::version::RELEASE {
        "Install it with `dimcode web install`".to_owned()
    } else {
        "Set DIMCODE_WEB_ASSETS to a frontend built from this source, or build the App Server with the embedded-web feature".to_owned()
    };
    match app_error(error)? {
        "web_assets_missing" if web_assets(executable, home).is_some() => Some((
            "The Web workspace was installed after the daemon started".into(),
            "Restart the daemon with `dimcode daemon stop`, then try again".into(),
        )),
        "web_assets_missing" => Some(("The Web workspace is not installed".into(), install)),
        "web_assets_incompatible" => Some((
            format!(
                "The installed Web workspace does not fit dimcode {}",
                crate::version::current()
            ),
            install,
        )),
        _ => None,
    }
}

/// Why a startup step of a resumed Session failed. A Session the daemon
/// refuses for its protocol version is returned unchanged: its text names the
/// recorded and the connection's versions when both are known, and no startup
/// step is what refused it. Every other failure keeps `context` before the
/// daemon's text.
pub fn resume_failure(context: &str, error: crate::Error) -> crate::Error {
    match app_error(&error) {
        Some("session_protocol_version") => error,
        _ => format!("{context}: {error}").into(),
    }
}

/// Whether running the same command again could open the resumed Session:
/// false when the daemon refuses it for its protocol version, which fails the
/// same way for as long as the Session exists. The refusal arrives both
/// through [`resume_failure`] and directly from `session.attach`.
pub fn resume_can_succeed(error: &crate::Error) -> bool {
    app_error(error) != Some("session_protocol_version")
}

/// The name the daemon shows in process lists.
pub const SERVER_NAME: &str = if cfg!(windows) {
    "dimcode-server.exe"
} else {
    "dimcode-server"
};

/// How long a spawned daemon may take to answer `hello` without reporting
/// progress.
const DAEMON_START_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a verified `daemon.shutdown` may take to settle.
const DAEMON_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a client waits on another client's start decision without a
/// progress report. The holder may spend a verified shutdown and a startup
/// before it releases the lock, plus a margin for the handshakes and the
/// Session check between them; it is derived from the two budgets so the
/// three cannot drift apart. Like the spawn wait, this is a bound on
/// inactivity: a holder whose daemon reports startup progress moves it
/// forward.
const START_LOCK_TIMEOUT: Duration =
    Duration::from_secs(DAEMON_SHUTDOWN_TIMEOUT.as_secs() + DAEMON_START_TIMEOUT.as_secs() + 10);
/// The bypass note a starting daemon writes beside its store.
const STARTUP_NOTE: &str = "dimcode.sqlite.startup";
/// How long every startup wait sleeps between polls.
const STARTUP_POLL: Duration = Duration::from_millis(50);
/// The oldest protocol this client can hand-shake with. A daemon's `hello`
/// reply below v3 carries no `features` at all, and this client requires
/// `session.create` and `session.history` from it.
const CLIENT_PROTOCOL_FLOOR: u32 = 3;

/// The startup bound a spawned daemon is given.
fn daemon_start_timeout() -> Duration {
    test_timeout("DIM_TEST_DAEMON_START_TIMEOUT_MS").unwrap_or(DAEMON_START_TIMEOUT)
}

/// The bound a client waiting on the start lock is given.
fn start_lock_timeout() -> Duration {
    test_timeout("DIM_TEST_START_LOCK_TIMEOUT_MS").unwrap_or(START_LOCK_TIMEOUT)
}

/// A flow test's shortened bound; a release build ignores the variable.
fn test_timeout(name: &str) -> Option<Duration> {
    #[cfg(debug_assertions)]
    if let Some(millis) = std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
    {
        return Some(Duration::from_millis(millis));
    }
    #[cfg(not(debug_assertions))]
    let _ = name;
    None
}

/// The file one client at a time holds to decide reuse, replacement or a
/// start: `<native_home>/daemon.start.lock`, a private file owned by the
/// current user like the daemon log and never a link. The OS releases the
/// lock when its holder exits.
fn open_start_lock(native_home: &Path) -> io::Result<std::fs::File> {
    let path = native_home.join("daemon.start.lock");
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(&path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.mode() & 0o077 != 0
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Daemon start lock must be a private file owned by the current user",
            ));
        }
    }
    Ok(file)
}

/// A store upgrade a daemon reports while starting, as its precise kind
/// calls it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoreUpgrade {
    pub from: u32,
    pub to: u32,
}

/// What a startup progress note reports.
struct ProgressNote {
    /// The daemon that wrote it.
    pid: u32,
    from: u32,
    to: u32,
    /// Rises with each rewrite; a reader counts only increases.
    counter: u64,
}

/// The note's text when it is a regular file of a bounded size, owned by the
/// current user and private on Unix, as the daemon writes it.
fn read_note(path: &Path) -> Option<String> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > 16 * 1024 {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o077 != 0 || metadata.uid() != unsafe { libc::geteuid() } {
            return None;
        }
    }
    String::from_utf8(std::fs::read(path).ok()?).ok()
}

/// The store upgrade a daemon's progress note announces, when the note is one.
fn read_progress_note(path: &Path) -> Option<ProgressNote> {
    let text = read_note(path)?;
    let (header, body) = text.split_once('\n')?;
    let pid = header
        .strip_prefix("dimcode-startup-progress v1 pid=")?
        .parse()
        .ok()?;
    let mut fields = body.lines().next()?.split_whitespace();
    if fields.next()? != "store-upgrade" {
        return None;
    }
    let (mut from, mut to, mut counter) = (None, None, None);
    for field in fields {
        let (name, value) = field.split_once('=')?;
        match name {
            "from" => from = Some(u32::try_from(value.parse::<u64>().ok()?).ok()?),
            "to" => to = Some(u32::try_from(value.parse::<u64>().ok()?).ok()?),
            "progress" => counter = Some(value.parse().ok()?),
            // A later kind may add fields; an older client ignores them,
            // whatever their values, as the daemon's own reader does.
            _ => {}
        }
    }
    Some(ProgressNote {
        pid,
        from: from?,
        to: to?,
        counter: counter?,
    })
}

/// Extends a startup wait while its daemon reports real progress. A note for
/// another pid is ignored when `pid` names the child being waited on; a lock
/// waiter passes `None`, because it does not know the holder's child. A note
/// from another pid than the series being counted starts a new series, whose
/// first note counts even when its counter is lower: a new daemon begins at
/// one, while a daemon killed mid-upgrade may have left a higher counter.
#[derive(Default)]
struct ProgressWatch {
    /// The pid whose note series is being counted.
    pid: Option<u32>,
    counter: u64,
    /// The upgrade line has been printed.
    reported: bool,
}

impl ProgressWatch {
    /// `None` when the note is absent, for another pid, or not a new report;
    /// otherwise the reported upgrade and a deadline one `timeout` away.
    fn observe(
        &mut self,
        path: &Path,
        pid: Option<u32>,
        deadline: &mut tokio::time::Instant,
        timeout: Duration,
    ) -> Option<StoreUpgrade> {
        let note = read_progress_note(path)?;
        if pid.is_some_and(|pid| note.pid != pid) {
            return None;
        }
        if self.pid != Some(note.pid) {
            self.pid = Some(note.pid);
            self.counter = 0;
        }
        if note.counter <= self.counter {
            return None;
        }
        self.counter = note.counter;
        *deadline = tokio::time::Instant::now() + timeout;
        Some(StoreUpgrade {
            from: note.from,
            to: note.to,
        })
    }
    /// Starts counting again for a child whose notes begin at one.
    fn reset(&mut self) {
        self.pid = None;
        self.counter = 0;
    }
    /// Prints the upgrade line once per watch, in `locale`.
    fn announce(&mut self, locale: crate::locale::Locale, upgrade: StoreUpgrade) {
        if !self.reported {
            self.reported = true;
            eprintln!("{}", crate::store_upgrade_line(locale, &upgrade));
        }
    }
}

/// Whether a process with this pid exists and this user may signal it. The
/// note lives in this user's private home, so the daemon that wrote it runs
/// as this user: a pid this user cannot signal is not that daemon.
fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        // A pid that does not fit `pid_t` is not one to ask about; `kill`
        // would read a wrapped value as a process group.
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return false;
        };
        // Signal 0 only checks that the process exists. `EPERM` means it
        // belongs to another user, and the daemon that wrote the note cannot
        // as the note is in this user's home, so it counts as gone.
        unsafe { libc::kill(pid, 0) == 0 }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            return false;
        }
        unsafe { CloseHandle(handle) };
        true
    }
}

/// The pid a progress note names, when it names another process: a daemon
/// another client started owns the store and has no socket until its upgrade
/// finishes (#1335). A note that names this client is stale, because a client
/// never upgrades a store itself, so its pid has been reused. Whether the pid
/// is still alive is the wait's question, which asks again on every poll.
fn another_daemon_starting(note: &Path) -> Option<u32> {
    let note = read_progress_note(note)?;
    (note.pid != std::process::id()).then_some(note.pid)
}

/// A startup wait bounded by inactivity: each new progress counter the daemon
/// reports moves the bound one `timeout` forward. The start-lock wait, the
/// wait for a daemon another client started, and the spawn wait all use this
/// one path.
struct StartupWait<'a> {
    note: &'a Path,
    watch: ProgressWatch,
    timeout: Duration,
    deadline: tokio::time::Instant,
    locale: crate::locale::Locale,
}

impl<'a> StartupWait<'a> {
    fn new(note: &'a Path, timeout: Duration, locale: crate::locale::Locale) -> Self {
        Self {
            note,
            watch: ProgressWatch::default(),
            timeout,
            deadline: tokio::time::Instant::now() + timeout,
            locale,
        }
    }

    /// Reads the note, moves the bound and prints the upgrade line once.
    fn observe(&mut self, pid: Option<u32>) {
        if let Some(upgrade) = self
            .watch
            .observe(self.note, pid, &mut self.deadline, self.timeout)
        {
            self.watch.announce(self.locale, upgrade);
        }
    }

    /// Starts another phase: its own bound and a new counter series.
    fn rebind(&mut self, timeout: Duration) {
        self.timeout = timeout;
        self.deadline = tokio::time::Instant::now() + timeout;
        self.watch.reset();
    }

    fn expired(&self) -> bool {
        tokio::time::Instant::now() >= self.deadline
    }
}

/// Waits for the start lock, polling every `STARTUP_POLL` as the spawn wait
/// does. The bound is inactivity: the holder's fixed budget covers a verified
/// shutdown, a startup and a margin, and a progress note in the home moves it
/// forward, whatever pid it names, because the holder's child may be
/// upgrading the store. A waiter that outlasts the bound fails and names the
/// daemon log; it never spawns without the lock.
async fn start_lock(native_home: &Path, wait: &mut StartupWait<'_>) -> Result<std::fs::File> {
    let file = open_start_lock(native_home)?;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) => {}
            Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
        }
        wait.observe(None);
        if wait.expired() {
            return Err(format!(
                "Another client is starting or replacing the App Server and has not finished; see {}",
                native_home.join("daemon.log").display()
            )
            .into());
        }
        tokio::time::sleep(STARTUP_POLL).await;
    }
}

/// `<native_home>/dimcode-server`, a link to `exe`, kept pointing at the
/// executable that starts the daemon: a hard link, whose name the kernel
/// takes as the process name, else a symbolic one, which still names the
/// command line; `exe` itself when neither can be made (the name is
/// cosmetic, the daemon runs either way).
fn server_link(native_home: &Path, exe: &Path) -> PathBuf {
    let link = native_home.join(SERVER_NAME);
    if same_file(&link, exe) {
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
    if !client.offers("daemon.shutdown.completed") {
        return Err("Daemon does not support verified shutdown completion; stop the older daemon with its matching CLI before restarting".into());
    }
    tokio::time::timeout(DAEMON_SHUTDOWN_TIMEOUT, async {
        client.rpc("daemon.shutdown", json!({})).await?;
        client.wait_closed().await?;
        Ok::<_, crate::Error>(())
    })
    .await
    .map_err(|_| "Daemon shutdown timed out; it is still settling and was not force-killed")??;
    Ok(())
}

/// A connection the daemon decision produced, with the notice a reused
/// daemon of another build produces.
pub struct Link {
    pub client: Client,
    pub notice: Option<DaemonNotice>,
}

/// A kept daemon of another build, as its client reports it.
pub struct DaemonNotice {
    /// The daemon's build, from `hello`.
    pub server: String,
    /// The counts a `daemon_busy` refusal carried, when it sent them.
    pub counts: Option<BusyCounts>,
}

/// What a refused `daemon.shutdown {ifIdle}` reported in use.
#[derive(Clone, Copy, Default)]
pub struct BusyCounts {
    pub connections: u64,
    pub attached_sessions: u64,
    pub running: u64,
}

/// What `daemon.shutdown {ifIdle: true}` decided.
enum IdleShutdown {
    /// The daemon was idle and is closing; its successor can be spawned.
    Stopped,
    /// The daemon is in use and stays; the counts it reported, when sent.
    Busy(Option<BusyCounts>),
}

/// The counts a `daemon_busy` refusal carries, when the daemon sent them.
fn busy_counts(error: &crate::Error) -> Option<BusyCounts> {
    let error = error.downcast_ref::<RpcError>()?;
    let data = error.data.as_ref()?;
    let count = |name: &str| data.get(name).and_then(serde_json::Value::as_u64);
    Some(BusyCounts {
        connections: count("connections")?,
        attached_sessions: count("attachedSessions")?,
        running: count("running")?,
    })
}

/// Asks the daemon to stop only if it is idle and waits for it to settle. A
/// `daemon_busy` refusal keeps the connection for reuse.
async fn shutdown_if_idle(client: &mut Client) -> Result<IdleShutdown> {
    tokio::time::timeout(DAEMON_SHUTDOWN_TIMEOUT, async {
        match client.rpc("daemon.shutdown", json!({"ifIdle": true})).await {
            Ok(_) => {
                client.wait_closed().await?;
                Ok(IdleShutdown::Stopped)
            }
            Err(error) if app_error(&error) == Some("daemon_busy") => {
                Ok(IdleShutdown::Busy(busy_counts(&error)))
            }
            Err(error) => Err(error),
        }
    })
    .await
    .map_err(|_| "Daemon shutdown timed out; it is still settling and was not force-killed")?
}

/// Whether an older daemon's `session.summaries` reports a client attached to
/// a Session or a running turn. The call is advertised together with
/// `daemon.shutdown.completed`; a failed call or an unexpected answer counts
/// as busy, so an unknown state never replaces a daemon that may be in use.
/// The read and the shutdown that follows are separate requests, so an attach
/// landing between them is not seen: that window is the cost of supporting a
/// daemon that predates `daemon.shutdown.ifIdle`.
async fn daemon_in_use(client: &mut Client) -> bool {
    let Ok(summaries) = client.rpc("session.summaries", json!({})).await else {
        return true;
    };
    let Some(sessions) = summaries.as_array() else {
        return true;
    };
    sessions.iter().any(|session| {
        let attached = session
            .get("attachedClients")
            .and_then(serde_json::Value::as_u64);
        let running = session.get("running").and_then(serde_json::Value::as_bool);
        match (attached, running) {
            (Some(attached), Some(running)) => attached > 0 || running,
            _ => true,
        }
    })
}

/// Uses the daemon behind `client` when it is this build at this protocol,
/// stops it when it is another build or a lower protocol and idle, and keeps
/// it with a notice when it is in use. `None` means the daemon stopped and the
/// caller may start its own. Every client that reaches a serving daemon goes
/// through here, whether the first attempt found it or a startup wait
/// connected to it, and it has already greeted the daemon.
async fn adopt(
    mut client: Client,
    start: bool,
    server: &Server,
    startup_note: &Path,
    desired: u32,
    locale: crate::locale::Locale,
) -> Result<Option<Link>> {
    let version = client.server_version().to_owned();
    let protocol = client.connection_config().protocol_version;
    if !(start && server.embedded && (version != env!("CARGO_PKG_VERSION") || protocol < desired)) {
        let _ = std::fs::remove_file(startup_note);
        return Ok(Some(Link {
            client,
            notice: None,
        }));
    }
    // A daemon of another build or of a lower protocol is replaced only while
    // it is idle: one that serves other clients is kept, and the client says
    // so. `hello` proved that this negotiated version is usable and offered
    // the required methods, which is not the same as the daemon being ours;
    // the restart happens on a later start that finds it idle.
    let counts = if client.offers("daemon.shutdown.ifIdle") {
        match shutdown_if_idle(&mut client).await? {
            IdleShutdown::Busy(counts) => counts,
            IdleShutdown::Stopped => return Ok(None),
        }
    } else if client.offers("daemon.shutdown.completed") && daemon_in_use(&mut client).await {
        None
    } else {
        // A daemon without verified shutdown is reported by `shutdown` as
        // before.
        shutdown(client).await?;
        return Ok(None);
    };
    let _ = std::fs::remove_file(startup_note);
    let notice = DaemonNotice {
        server: version,
        counts,
    };
    // The daemon is still there: it refused the idle replacement, or it cannot
    // be asked as well as this client needs. A Session created over it would
    // be stored at its protocol and hidden from builds of this one, so a start
    // that agreed on a lower protocol ends here instead.
    if protocol < desired {
        return Err(crate::daemon_too_old(locale, &notice, protocol).into());
    }
    Ok(Some(Link {
        client,
        notice: Some(notice),
    }))
}

/// Opens the endpoint and greets the daemon; `None` when no daemon answers at
/// the endpoint, which `WouldBlock` counts as only during the startup wait.
/// With `negotiate`, a typed `protocol_version` refusal steps one version
/// down, no lower than [`CLIENT_PROTOCOL_FLOOR`], and every attempt opens a
/// new connection; any other failure is final, as is a refusal at the floor.
async fn connect_negotiating(
    config: &ConnectionConfig,
    starting: bool,
    negotiate: bool,
) -> Result<Option<Client>> {
    let mut config = config.clone();
    // One discarded connection per version from this client's protocol down to
    // CLIENT_PROTOCOL_FLOOR, each only after the daemon's typed refusal, so the
    // floor sets the bound.
    loop {
        let mut client = match Client::open(&config, starting).await {
            Ok(client) => client,
            Err(error)
                if missing(&error)
                    || starting && cfg!(unix) && error.kind() == io::ErrorKind::WouldBlock =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        match client.hello().await {
            Ok(_) => return Ok(Some(client)),
            Err(error)
                if negotiate
                    && protocol_refused(&error)
                    && config.protocol_version > CLIENT_PROTOCOL_FLOOR =>
            {
                config.protocol_version -= 1;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Whether a `hello` failure is the daemon's typed refusal of this client's
/// protocol version: the only failure another version may answer.
fn protocol_refused(error: &crate::Error) -> bool {
    error
        .downcast_ref::<RpcError>()
        .is_some_and(|refusal| refusal.code == -32000)
        && app_error(error) == Some("protocol_version")
}

/// The connection to the daemon of `options.home` after `hello`. Without a
/// daemon, starts one when `start` and waits for it; else `None`. With
/// `start`, one client at a time decides under `<native_home>/daemon.start.lock`
/// whether to reuse the daemon, replace it, or start one. A daemon that
/// reports a store upgrade while it starts is waited for, and the line is
/// printed once in `locale`; every wait is bounded by inactivity, not by
/// total time.
pub async fn connect_inner(
    options: &Options,
    start: bool,
    locale: crate::locale::Locale,
) -> Result<Option<Link>> {
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
    let startup_note = native_home.join(STARTUP_NOTE);
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
    let connection = ConnectionConfig {
        endpoint: endpoint.clone(),
        server: server.clone(),
        profile,
        client_kind: if matches!(options.action, crate::args::Action::Acp { .. }) {
            "other"
        } else {
            "cli"
        },
        client_name: if matches!(options.action, crate::args::Action::Acp { .. }) {
            "AcpGateway"
        } else {
            "dimcode"
        },
        protocol_version: if matches!(options.action, crate::args::Action::Acp { .. }) {
            3
        } else {
            4
        },
    };
    // `daemon stop` addresses the home's daemon whatever binary runs it, and a
    // start reuses a `--server` daemon as it is and never replaces it, so it
    // greets that daemon once.
    let desired = connection.protocol_version;
    let negotiate = match options.action {
        crate::args::Action::DaemonStop => true,
        _ => start && server.embedded,
    };
    // One client at a time decides whether to reuse, replace or start the
    // daemon; everyone else waits here and then runs the same decision, which
    // normally finds the successor at its own version. `--server` daemons are
    // never started or replaced, so they take no lock.
    let mut wait = StartupWait::new(&startup_note, start_lock_timeout(), locale);
    let _start_lock = if start && server.embedded {
        Some(start_lock(&native_home, &mut wait).await?)
    } else {
        None
    };
    if let Some(client) = connect_negotiating(&connection, false, negotiate).await?
        && let Some(link) = adopt(client, start, &server, &startup_note, desired, locale).await?
    {
        return Ok(Some(link));
    }
    if !start {
        return Ok(None);
    }
    if !binary.is_file() {
        return Err(if server.embedded {
            format!(
                "App Server binary is missing at {}; supply --server PATH",
                binary.display()
            )
        } else {
            format!("{SERVER_REQUIREMENT}; {} is missing", binary.display())
        }
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
    let program = if server.embedded {
        server_link(&native_home, binary)
    } else {
        binary.clone()
    };
    let mut command = Command::new(&program);
    command.args(server.subcommand());
    for name in [
        "DIM_MODEL",
        "DIM_EFFORT_LEVEL",
        "DIM_BASE_URL",
        "DIM_API_FORMAT",
        "DIM_API_KEY",
        "DIM_AUTH_TOKEN",
        "DIM_CUSTOM_HEADERS",
        "DIM_MAX_CONTEXT_TOKENS",
        "DIM_MAX_OUTPUT_TOKENS",
        "DIM_AUTO_COMPACT_WINDOW",
    ] {
        command.env_remove(name);
    }
    command
        .current_dir(options.launch_dir())
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
    // The Web workspace is a separate artifact the daemon serves from a
    // directory (web-workspace.md §2.4).
    if let Some(path) = web_assets(binary, &options.home) {
        command.arg("--web-assets").arg(path);
    }
    if let Some(path) = default_plugins(binary) {
        command.arg("--defaults").arg(path);
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
    // A daemon another client started may already own the store: a store
    // upgrade runs before it serves, so it has no socket yet. Its progress
    // note, from a pid that is still alive, says so, and this client waits
    // for that daemon instead of spawning a second one that would fail
    // admission (#1335). The note belongs to that daemon, so this client
    // removes it only when it starts its own.
    let mut waiting = another_daemon_starting(&startup_note);
    wait.rebind(daemon_start_timeout());
    let mut child: Option<std::process::Child> = None;
    loop {
        if let Some(client) = connect_negotiating(&connection, true, negotiate).await? {
            match adopt(client, start, &server, &startup_note, desired, locale).await? {
                Some(link) => return Ok(Some(link)),
                // The daemon behind the endpoint was another build's; it is
                // stopped now. A child this client already spawned may have
                // lost the store or the endpoint to it, so it is reaped, and
                // the loop starts this client's own.
                None => {
                    if let Some(mut child) = child.take() {
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                    waiting = None;
                }
            }
        }
        if let Some(pid) = waiting {
            if pid_alive(pid) {
                wait.observe(None);
                if !wait.expired() {
                    tokio::time::sleep(STARTUP_POLL).await;
                    continue;
                }
                // The daemon went quiet; whatever store ownership it still
                // holds is a spawned child's to report.
            }
            waiting = None;
        }
        if child.is_none() {
            let _ = std::fs::remove_file(&startup_note);
            wait.rebind(daemon_start_timeout());
            child = Some(command.spawn()?);
            tokio::time::sleep(STARTUP_POLL).await;
            continue;
        }
        let child = child.as_mut().expect("the loop spawned a child");
        if let Some(status) = child.try_wait()? {
            let cause = startup_failure(&startup_note, child.id());
            let _ = std::fs::remove_file(&startup_note);
            return Err(match cause {
                Some(cause) => format!("{cause}\n  Details: {}", log_path.display()).into(),
                None if server.embedded => {
                    format!("App Server exited ({status}); see {}", log_path.display()).into()
                }
                None => format!(
                    "{SERVER_REQUIREMENT}; it exited before serving ({status}); see {}",
                    log_path.display()
                )
                .into(),
            });
        }
        wait.observe(Some(child.id()));
        if wait.expired() {
            return Err(if server.embedded {
                format!("App Server startup timed out; see {}", log_path.display())
            } else {
                format!(
                    "{SERVER_REQUIREMENT}; it did not start in time; see {}",
                    log_path.display()
                )
            }
            .into());
        }
        tokio::time::sleep(STARTUP_POLL).await;
    }
}

/// The storage startup note this child left, if it is a private file owned by
/// the current user and the header names this child.
fn startup_failure(path: &std::path::Path, pid: u32) -> Option<String> {
    let text = read_note(path)?;
    let (header, body) = text.split_once('\n')?;
    if header != format!("dimcode-startup-failure v1 pid={pid}") {
        return None;
    }
    let body: String = body
        .chars()
        .filter(|character| *character == '\n' || *character == '\t' || !character.is_control())
        .collect();
    let body = body.trim_end();
    (!body.is_empty()).then(|| body.to_owned())
}

fn missing(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    ) || cfg!(windows) && error.raw_os_error() == Some(231)
}

#[cfg(all(test, unix))]
mod starting_note_tests {
    use super::another_daemon_starting;
    use std::{fs, os::unix::fs::PermissionsExt};

    /// A progress note names the daemon to wait for, unless it names this
    /// client; a missing note names nothing. The failed-note and permission
    /// cases live in the startup flow tests, where the client runs.
    #[test]
    fn a_progress_note_names_another_process() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dimcode.sqlite.startup");
        let write = |pid: u32| {
            fs::write(
                &path,
                format!("dimcode-startup-progress v1 pid={pid}\nstore-upgrade from=34 to=36 progress=1\n"),
            )
            .unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        };
        write(4242);
        assert_eq!(another_daemon_starting(&path), Some(4242));
        write(std::process::id());
        assert_eq!(another_daemon_starting(&path), None);
        assert_eq!(another_daemon_starting(&dir.path().join("missing")), None);
    }
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

#[cfg(all(test, unix))]
mod same_file_tests {
    use super::same_file;
    use std::fs;

    /// A file, a hard link and a symlink to it are the same file; a copy
    /// and a missing path are not.
    #[test]
    fn links_name_the_same_file() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("dimcode");
        fs::write(&file, "binary").unwrap();
        let hard = root.path().join("dimcode-server");
        fs::hard_link(&file, &hard).unwrap();
        let symlink = root.path().join("dimcode-link");
        std::os::unix::fs::symlink(&file, &symlink).unwrap();
        let copy = root.path().join("copy");
        fs::copy(&file, &copy).unwrap();

        assert!(same_file(&file, &hard), "hard link");
        assert!(same_file(&file, &symlink), "symlink");
        assert!(same_file(&hard, &symlink), "link and symlink");
        assert!(!same_file(&file, &copy), "copy");
        assert!(
            !same_file(&file, &root.path().join("missing")),
            "missing path"
        );
    }
}
