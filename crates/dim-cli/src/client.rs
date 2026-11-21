use crate::{
    Result,
    args::{Options, SERVER_REQUIREMENT},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

pub(crate) const STARTUP_RETRY: &str = "DIMCODE_STARTUP_RETRY";

#[derive(Clone, Debug)]
pub(crate) struct NewerStore {
    pub(crate) format: u32,
    pub(crate) diagnostic: String,
}

impl std::fmt::Display for NewerStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.diagnostic)
    }
}

impl std::error::Error for NewerStore {}

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
    let install = if dim_agent_capabilities::build::release() {
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
                dim_agent_capabilities::build::current()
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
    /// The stage the upgrade reported, when it reported one; a daemon from
    /// before stages reports none.
    pub stage: Option<UpgradeStage>,
    /// How long the upgrade has been running, from the first note read.
    pub elapsed: Duration,
}

/// The stage of a store upgrade a progress note names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpgradeStage {
    Images,
    Index,
    Verify,
}
impl UpgradeStage {
    /// The stage a note names, `None` for a name this client does not know:
    /// a stage is worth naming only when it can be shown.
    fn parse(name: &str) -> Option<Self> {
        match name {
            "images" => Some(Self::Images),
            "index" => Some(Self::Index),
            "verify" => Some(Self::Verify),
            _ => None,
        }
    }
}

/// What a startup progress note reports.
struct ProgressNote {
    /// The daemon that wrote it.
    pid: u32,
    /// Rises with each rewrite; a reader counts only increases.
    counter: u64,
    kind: ProgressKind,
}

/// The kind of startup a progress note reports, and the fields only one kind
/// carries.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ProgressKind {
    /// A store format upgrade.
    StoreUpgrade {
        from: u32,
        to: u32,
        stage: Option<UpgradeStage>,
    },
    /// The daemon's ordinary assembly after its store opened.
    Assembly,
}
impl ProgressKind {
    /// Whether both notes count one series: the kind, never the fields a
    /// stage or a format moves.
    fn same_series(self, other: Self) -> bool {
        std::mem::discriminant(&self) == std::mem::discriminant(&other)
    }
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

/// The startup progress a daemon's note announces, when the note is one.
fn read_progress_note(path: &Path) -> Option<ProgressNote> {
    let text = read_note(path)?;
    let (header, body) = text.split_once('\n')?;
    let pid = header
        .strip_prefix("dimcode-startup-progress v1 pid=")?
        .parse()
        .ok()?;
    let mut fields = body.lines().next()?.split_whitespace();
    let kind = fields.next()?;
    let (mut from, mut to, mut counter, mut stage) = (None, None, None, None);
    for field in fields {
        let (name, value) = field.split_once('=')?;
        match name {
            "from" => from = Some(u32::try_from(value.parse::<u64>().ok()?).ok()?),
            "to" => to = Some(u32::try_from(value.parse::<u64>().ok()?).ok()?),
            "progress" => counter = Some(value.parse().ok()?),
            "stage" => stage = UpgradeStage::parse(value),
            // A later kind may add fields; an older client ignores them,
            // whatever their values, as the daemon's own reader does.
            _ => {}
        }
    }
    let kind = match kind {
        "store-upgrade" => ProgressKind::StoreUpgrade {
            from: from?,
            to: to?,
            stage,
        },
        "assembly" => ProgressKind::Assembly,
        _ => return None,
    };
    Some(ProgressNote {
        pid,
        counter: counter?,
        kind,
    })
}

/// Extends a startup wait while its daemon reports real progress. A note for
/// another pid is ignored when `pid` names the child being waited on; a lock
/// waiter passes `None`, because it does not know the holder's child. A note
/// from another pid than the series being counted starts a new series, whose
/// first note counts even when its counter is lower: a new daemon begins at
/// one, while a daemon killed mid-upgrade may have left a higher counter. A
/// note of another kind starts a new series too: an assembly's counter begins
/// at one after the store's upgrade reached a higher count. A note of the same
/// kind continues the series, even when an upgrade moves to another stage.
#[derive(Default)]
struct ProgressWatch {
    /// The pid whose note series is being counted.
    pid: Option<u32>,
    /// The kind of note the series counts.
    kind: Option<ProgressKind>,
    counter: u64,
    /// The upgrade line has been printed.
    reported: bool,
    /// When the first note of the series was read, so later reports can say
    /// how long the upgrade has been running.
    started: Option<std::time::Instant>,
    /// Where the upgrade line goes; a redrawn line replaces the last one.
    line: crate::UpgradeLine,
}

/// What one new progress counter reports to a wait.
enum ProgressReport {
    /// A store upgrade, which is shown.
    StoreUpgrade(StoreUpgrade),
    /// An ordinary assembly, which is not shown.
    Assembly,
}

impl ProgressWatch {
    /// `None` when the note is absent, for another pid, or not a new report;
    /// otherwise what the note reports and a deadline one `timeout` away.
    fn observe(
        &mut self,
        path: &Path,
        pid: Option<u32>,
        deadline: &mut tokio::time::Instant,
        timeout: Duration,
    ) -> Option<ProgressReport> {
        let note = read_progress_note(path)?;
        if pid.is_some_and(|pid| note.pid != pid) {
            return None;
        }
        if self.pid != Some(note.pid) {
            self.pid = Some(note.pid);
            self.counter = 0;
            self.started = None;
            self.line = crate::UpgradeLine::default();
        } else if self.kind.is_some_and(|kind| !kind.same_series(note.kind)) {
            // Keep the upgrade line: it is still the one on screen, and a
            // change of kind does not print another.
            self.counter = 0;
            self.started = None;
        }
        self.kind = Some(note.kind);
        if note.counter <= self.counter {
            return None;
        }
        self.counter = note.counter;
        let started = *self.started.get_or_insert_with(std::time::Instant::now);
        *deadline = tokio::time::Instant::now() + timeout;
        Some(match note.kind {
            ProgressKind::StoreUpgrade { from, to, stage } => {
                ProgressReport::StoreUpgrade(StoreUpgrade {
                    from,
                    to,
                    stage,
                    elapsed: started.elapsed(),
                })
            }
            ProgressKind::Assembly => ProgressReport::Assembly,
        })
    }
    /// Starts counting again for a child whose notes begin at one.
    fn reset(&mut self) {
        self.pid = None;
        self.kind = None;
        self.counter = 0;
        self.started = None;
        self.line = crate::UpgradeLine::default();
    }
    /// Prints the upgrade's first line and then the progress of it, in
    /// `locale`: one line on a terminal that shows the elapsed time and the
    /// stage again each time a note reports, and one line per note otherwise.
    fn announce(&mut self, locale: crate::locale::Locale, upgrade: StoreUpgrade) {
        let text = match (self.reported, upgrade.stage) {
            (false, _) => crate::store_upgrade_line(locale, &upgrade),
            (true, Some(stage)) => {
                crate::store_upgrade_progress_line(locale, stage, upgrade.elapsed)
            }
            // A daemon that reports no stage leaves the first line as the
            // whole report, as it always did.
            (true, None) => return,
        };
        self.reported = true;
        self.line.write(&text);
    }
    /// Ends the redrawn line, if one is on screen, so the lines after it do
    /// not share it.
    fn finish(&mut self) {
        self.line.finish();
    }
}

impl Drop for StartupWait<'_> {
    /// The redrawn upgrade line belongs to this wait's screen area: leaving it
    /// drawn would let the next output share its line.
    fn drop(&mut self) {
        self.watch.finish();
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
/// or assembly finishes (#1335). A note that names this client is stale,
/// because a client never starts a daemon itself, so its pid has been reused.
/// Whether the pid is still alive is the wait's question, which asks again on
/// every poll.
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

    /// Reads the note, moves the bound and prints what it reports: a store
    /// upgrade says which upgrade is running, each later report redraws it, and
    /// an assembly note only moves the bound.
    fn observe(&mut self, pid: Option<u32>) {
        match self
            .watch
            .observe(self.note, pid, &mut self.deadline, self.timeout)
        {
            Some(ProgressReport::StoreUpgrade(upgrade)) => {
                self.watch.announce(self.locale, upgrade);
            }
            Some(ProgressReport::Assembly) | None => {}
        }
    }

    /// Starts another phase: its own bound and a new counter series.
    fn rebind(&mut self, timeout: Duration) {
        self.timeout = timeout;
        self.deadline = tokio::time::Instant::now() + timeout;
        self.watch.finish();
        self.watch.reset();
    }

    fn expired(&self) -> bool {
        tokio::time::Instant::now() >= self.deadline
    }
    /// Takes the reply to a `hello` this client sent under this wait's own
    /// inactivity bound. A server that is bound but not yet open answers
    /// nothing yet, and its startup progress moves the bound forward like any
    /// other phase's. The note's pid is not compared: the server that accepted
    /// this connection is the one being waited for.
    async fn reply(&mut self, client: &mut Client, id: u64) -> Result<Value> {
        let reply = client.response(id);
        tokio::pin!(reply);
        loop {
            tokio::select! {
                biased;
                reply = &mut reply => return reply,
                () = tokio::time::sleep(STARTUP_POLL) => {}
            }
            self.observe(None);
            if self.expired() {
                return Err(format!(
                    "App Server did not answer hello during startup; see {}",
                    self.note.with_file_name("daemon.log").display()
                )
                .into());
            }
        }
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
pub enum DaemonNotice {
    /// A daemon of another version, or of a lower protocol, that stayed in use
    /// because its clients are not done with it.
    Kept {
        /// The daemon's version, from `hello`.
        server: String,
        /// The counts a `daemon_busy` refusal carried, when it sent them.
        counts: Option<BusyCounts>,
    },
    /// A daemon of this same version and protocol whose artifact stamp says
    /// another build, or none.
    Build {
        /// The version both report: the same on both sides, from `hello`.
        server: String,
        kind: BuildKind,
        /// The command that stops this home's daemon, for the user's shell.
        command: String,
    },
}

/// How the daemon's stamp compares with this client's.
#[derive(Clone, Copy)]
pub enum BuildKind {
    /// Both are known and they differ.
    Different,
    /// The daemon reported no stamp, so nothing proves it is this build.
    Unverified,
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

/// Stops this home's daemon for an update, only while it is idle: no client,
/// attached Session, running turn or accepted work. A client that has just
/// disconnected may still be counted for a moment, so a refusal is asked
/// again for a short while. `Err` names what keeps the daemon and the next
/// step; nothing is stopped then.
pub async fn stop_idle_for_update(options: &Options, locale: crate::locale::Locale) -> Result<()> {
    let command = invocation(options);
    let mut options = options.clone();
    options.action = crate::args::Action::DaemonStop;
    let Some(Link { mut client, .. }) = connect_inner(&options, false, locale).await? else {
        return Ok(());
    };
    if !client.offers("daemon.shutdown.ifIdle") {
        return Err(format!(
            "The running server is too old to stop safely.\nClose all DimCode windows, run `{command} daemon stop`, then run: {command} update"
        )
        .into());
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        match shutdown_if_idle(&mut client).await? {
            IdleShutdown::Stopped => return Ok(()),
            IdleShutdown::Busy(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            IdleShutdown::Busy(counts) => {
                let detail = counts.map_or_else(String::new, |counts| {
                    format!(
                        " (clients: {}, attached sessions: {}, running turns: {})",
                        counts.connections, counts.attached_sessions, counts.running
                    )
                });
                return Err(format!(
                    "Other DimCode windows or tasks are using the server{detail}.\nClose them or let them finish, then run: {command} update"
                )
                .into());
            }
        }
    }
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
/// stops it when it is another version or a lower protocol and idle, and keeps
/// it with a notice when it is in use. A daemon of this version and protocol
/// whose stamp names another build is kept too, whatever it is doing: the
/// client reports the difference and the user decides when to switch builds.
/// `None` means the daemon stopped and the caller may start its own. Every
/// client that reaches a serving daemon goes through here, whether the first
/// attempt found it or a startup wait connected to it, and it has already
/// greeted the daemon.
async fn adopt(
    mut client: Client,
    start: bool,
    server: &Server,
    startup_note: &Path,
    desired: u32,
    locale: crate::locale::Locale,
    home: &Path,
) -> Result<Option<Link>> {
    let version = client.server_version().to_owned();
    let protocol = client.connection_config().protocol_version;
    if !(start && server.embedded) {
        let _ = std::fs::remove_file(startup_note);
        return Ok(Some(Link {
            client,
            notice: None,
        }));
    }
    if version == env!("CARGO_PKG_VERSION") && protocol >= desired {
        let _ = std::fs::remove_file(startup_note);
        // One version and one protocol: only the artifact stamp tells two
        // builds apart. A client without one of its own has nothing to compare,
        // and a source build names its tree in the version it reports already.
        let kind = match (dim_agent_capabilities::build::build_id(), client.build_id()) {
            (Some(ours), Some(daemon)) if ours == daemon => {
                return Ok(Some(Link {
                    client,
                    notice: None,
                }));
            }
            (Some(_), Some(_)) => BuildKind::Different,
            (Some(_), None) => BuildKind::Unverified,
            (None, _) => {
                return Ok(Some(Link {
                    client,
                    notice: None,
                }));
            }
        };
        return Ok(Some(Link {
            client,
            notice: Some(DaemonNotice::Build {
                server: version,
                kind,
                command: daemon_stop_command(&server.program, home),
            }),
        }));
    }
    // A daemon of another version or of a lower protocol is replaced only while
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
    // The daemon is still there: it refused the idle replacement, or it cannot
    // be asked as well as this client needs. A Session created over it would
    // be stored at its protocol and hidden from builds of this one, so a start
    // that agreed on a lower protocol ends here instead.
    if protocol < desired {
        return Err(crate::daemon_too_old(locale, &version, counts, protocol).into());
    }
    Ok(Some(Link {
        client,
        notice: Some(DaemonNotice::Kept {
            server: version,
            counts,
        }),
    }))
}

/// The command word that runs this CLI on the selected home, for a POSIX
/// shell: `--home` follows the entry point only when the invocation named
/// one, since `DIMCODE_HOME` reaches a command run from the same shell.
pub fn invocation(options: &Options) -> String {
    let program = shell_word(&crate::update::entry_command());
    if options.home_explicit {
        format!(
            "{program} --home {}",
            shell_word(&options.home.display().to_string())
        )
    } else {
        program
    }
}

/// The command that stops the selected home's daemon in a POSIX shell.
fn daemon_stop_command(executable: &Path, home: &Path) -> String {
    let program = shell_word(&crate::update::entry(executable));
    let home = shell_word(&home.display().to_string());
    format!("{program} --home {home} daemon stop")
}

/// `word` as a POSIX shell takes it whole.
pub(crate) fn shell_word(word: &str) -> String {
    if !word.is_empty()
        && word
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_./-".contains(&byte))
    {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

/// Opens the endpoint and greets the daemon; `None` when no daemon answers at
/// the endpoint, which `WouldBlock` counts as only during the startup wait.
/// With `negotiate`, a typed `protocol_version` refusal steps one version
/// down, no lower than [`CLIENT_PROTOCOL_FLOOR`], and every attempt opens a
/// new connection; any other failure is final, as is a refusal at the floor.
/// The greeting's reply is taken under `wait`, so a daemon that accepted the
/// connection before it finished opening is a startup phase like the rest.
async fn connect_negotiating(
    config: &ConnectionConfig,
    starting: bool,
    negotiate: bool,
    wait: &mut StartupWait<'_>,
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
        // A server that is bound but not open has accepted this connection and
        // answers nothing yet. Its reply is a startup phase, never a request
        // with a bound of its own (#1620). A refusal arrives as an error, so the
        // step-down keeps the greeting as one result beside the reply check.
        let id = client.greet().await?;
        let greeting = wait.reply(&mut client, id).await;
        match greeting.and_then(|reply| client.complete_hello(reply)) {
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
    let home = std::path::absolute(options.home.join("."))?;
    let native_home = home.join("rust");
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
    // never started or replaced, so they take no lock. The connect below meets a
    // daemon that may still be starting, so it is bounded like the spawn wait;
    // only the lock has the wider bound, because its holder may shut one down
    // first.
    let mut wait = StartupWait::new(&startup_note, daemon_start_timeout(), locale);
    let _start_lock = if start && server.embedded {
        wait.rebind(start_lock_timeout());
        let lock = start_lock(&native_home, &mut wait).await?;
        wait.rebind(daemon_start_timeout());
        Some(lock)
    } else {
        None
    };
    let found = connect_negotiating(&connection, false, negotiate, &mut wait).await?;
    if let Some(client) = found
        && let Some(link) = adopt(
            client,
            start,
            &server,
            &startup_note,
            desired,
            locale,
            &home,
        )
        .await?
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
        .env_remove(STARTUP_RETRY)
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
    // A daemon another client started may already own the store: its upgrade
    // or assembly runs before it serves, so it has no socket yet. Its progress
    // note, from a pid that is still alive, says so, and this client waits
    // for that daemon instead of spawning a second one that would fail
    // admission (#1335). The note belongs to that daemon, so this client
    // removes it only when it starts its own.
    let mut waiting = another_daemon_starting(&startup_note);
    wait.rebind(daemon_start_timeout());
    let mut child: Option<std::process::Child> = None;
    let mut poll = Duration::from_millis(5);
    loop {
        if let Some(client) = connect_negotiating(&connection, true, negotiate, &mut wait).await? {
            match adopt(
                client,
                start,
                &server,
                &startup_note,
                desired,
                locale,
                &home,
            )
            .await?
            {
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
            poll = Duration::from_millis(5);
            tokio::time::sleep(poll).await;
            continue;
        }
        let child = child.as_mut().expect("the loop spawned a child");
        if let Some(status) = child.try_wait()? {
            let cause = startup_failure(&startup_note, child.id());
            let _ = std::fs::remove_file(&startup_note);
            return Err(match cause {
                Some((cause, newer)) => {
                    let diagnostic = format!("{cause}\n  Details: {}", log_path.display());
                    match newer {
                        Some(format) if server.embedded => {
                            Box::new(NewerStore { format, diagnostic })
                        }
                        _ => diagnostic.into(),
                    }
                }
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
        poll = (poll * 2).min(STARTUP_POLL);
        tokio::time::sleep(poll).await;
    }
}

/// The storage startup note this child left, if it is a private file owned by
/// the current user and the header names this child.
fn startup_failure(path: &Path, pid: u32) -> Option<(String, Option<u32>)> {
    let text = read_note(path)?;
    let (header, body) = text.split_once('\n')?;
    if header != format!("dimcode-startup-failure v1 pid={pid}") {
        return None;
    }
    let mut body = body.trim_end();
    let mut newer = None;
    if let Some((diagnostic, metadata)) = body.rsplit_once('\n')
        && let Some(formats) = metadata.strip_prefix("store-newer found=")
    {
        body = diagnostic;
        newer = formats
            .split_once(" supported=")
            .and_then(|(found, supported)| {
                let found = found.parse::<u32>().ok()?;
                let supported = supported.parse::<u32>().ok()?;
                (found > supported).then_some(found)
            });
    }
    let body: String = body
        .chars()
        .filter(|character| *character == '\n' || *character == '\t' || !character.is_control())
        .collect();
    let body = body.trim_end();
    (!body.is_empty()).then(|| (body.to_owned(), newer))
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
    /// client; a missing note names nothing. A store upgrade and an assembly
    /// are both progress a starting daemon reports. The failed-note and
    /// permission cases live in the startup flow tests, where the client runs.
    #[test]
    fn a_progress_note_names_another_process() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dimcode.sqlite.startup");
        let write = |pid: u32, body: &str| {
            fs::write(
                &path,
                format!("dimcode-startup-progress v1 pid={pid}\n{body}\n"),
            )
            .unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        };
        write(4242, "store-upgrade from=34 to=36 progress=1");
        assert_eq!(another_daemon_starting(&path), Some(4242));
        write(4242, "assembly progress=1");
        assert_eq!(another_daemon_starting(&path), Some(4242));
        write(std::process::id(), "assembly progress=2");
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

#[cfg(test)]
mod progress_note_tests {
    use super::{
        ProgressKind, ProgressNote, ProgressReport, ProgressWatch, UpgradeStage, read_progress_note,
    };
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    fn note_path(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "dim-progress-{}-{name}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root.join("dimcode.sqlite.startup")
    }

    /// Writes a note as the daemon writes it: the header binds the note to a
    /// pid.
    fn write(path: &Path, body: &str) {
        std::fs::write(
            path,
            format!(
                "dimcode-startup-progress v1 pid={}\n{body}\n",
                std::process::id()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    /// Writes a note as the daemon writes it and reads it back: a client that
    /// knows fewer fields than the note carries reads the ones it knows.
    fn note(name: &str, body: &str) -> Option<ProgressNote> {
        let path = note_path(name);
        write(&path, body);
        let note = read_progress_note(&path);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
        note
    }

    fn upgrade(note: ProgressNote) -> (u32, u32, Option<UpgradeStage>) {
        match note.kind {
            ProgressKind::StoreUpgrade { from, to, stage } => (from, to, stage),
            ProgressKind::Assembly => panic!("not a store upgrade"),
        }
    }

    /// A note that names a known stage is read with it, and one that names a
    /// stage this client does not know keeps every field it does know.
    #[test]
    fn a_progress_note_reads_the_stage_it_knows_and_ignores_the_rest() {
        let known = note(
            "known",
            "store-upgrade from=49 to=50 progress=7 stage=index",
        )
        .unwrap();
        assert_eq!(known.counter, 7);
        assert_eq!(upgrade(known), (49, 50, Some(UpgradeStage::Index)));

        let unknown = note(
            "unknown",
            "store-upgrade from=49 to=50 progress=8 stage=compacting future=value",
        )
        .unwrap();
        assert_eq!(unknown.counter, 8);
        assert_eq!(
            upgrade(unknown),
            (49, 50, None),
            "a stage this client cannot show is not a stage it shows as something else"
        );

        let older = note("older", "store-upgrade from=49 to=50 progress=9").unwrap();
        assert_eq!(older.counter, 9);
        assert_eq!(
            upgrade(older),
            (49, 50, None),
            "a daemon from before stages names none"
        );
        for stage in ["images", "verify"] {
            let read = note(
                stage,
                &format!("store-upgrade from=49 to=50 progress=1 stage={stage}"),
            )
            .unwrap();
            assert!(upgrade(read).2.is_some(), "{stage}");
        }
        assert!(note("broken", "store-upgrade from=49 to=50").is_none());
    }

    /// An assembly note needs only its counter: the formats and the stage
    /// belong to a store upgrade.
    #[test]
    fn a_progress_note_reads_assembly_progress_without_upgrade_fields() {
        let assembly = note("assembly", "assembly progress=7 future=value").unwrap();
        assert_eq!(assembly.counter, 7);
        assert!(matches!(assembly.kind, ProgressKind::Assembly));
        assert!(note("assembly-without-counter", "assembly").is_none());
    }

    /// A daemon that switches from an upgrade to its assembly begins a new
    /// counter series at one, so the assembly's lower counter still counts.
    #[test]
    fn a_new_note_kind_starts_a_new_counter_series() {
        let path = note_path("kind-change");
        write(&path, "store-upgrade from=49 to=50 progress=9");
        let mut watch = ProgressWatch::default();
        let mut deadline = tokio::time::Instant::now();
        assert!(matches!(
            watch.observe(
                &path,
                Some(std::process::id()),
                &mut deadline,
                Duration::from_secs(10)
            ),
            Some(ProgressReport::StoreUpgrade(_))
        ));
        write(&path, "assembly progress=1");
        assert!(matches!(
            watch.observe(
                &path,
                Some(std::process::id()),
                &mut deadline,
                Duration::from_secs(10)
            ),
            Some(ProgressReport::Assembly)
        ));
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    /// A stage change inside one upgrade continues its counter series, so the
    /// elapsed time keeps running from the first note.
    #[test]
    fn a_stage_change_keeps_the_upgrade_series() {
        let path = note_path("stage-change-series");
        let pid = Some(std::process::id());
        let timeout = Duration::from_secs(10);
        let mut deadline = tokio::time::Instant::now();
        let mut watch = ProgressWatch::default();
        write(&path, "store-upgrade from=49 to=50 progress=1 stage=images");
        let first = match watch.observe(&path, pid, &mut deadline, timeout) {
            Some(ProgressReport::StoreUpgrade(upgrade)) => upgrade,
            _ => panic!("the first note is a store upgrade"),
        };
        std::thread::sleep(Duration::from_millis(1100));
        write(&path, "store-upgrade from=49 to=50 progress=2 stage=index");
        let second = match watch.observe(&path, pid, &mut deadline, timeout) {
            Some(ProgressReport::StoreUpgrade(upgrade)) => upgrade,
            _ => panic!("the next note is the same upgrade"),
        };
        assert_eq!(second.stage, Some(UpgradeStage::Index));
        assert!(
            second.elapsed >= first.elapsed + Duration::from_secs(1),
            "the elapsed time continues across the stage change: {:?}",
            second.elapsed
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    /// A note the daemon writes without a counter, or with an unknown header,
    /// is not a progress note at all.
    #[test]
    fn a_note_that_is_not_a_progress_note_is_no_report() {
        assert!(note("no-counter", "store-upgrade from=49 to=50").is_none());
        assert!(note("no-kind", "something-else from=49 to=50 progress=1").is_none());
    }
}
