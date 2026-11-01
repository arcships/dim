//! RuntimeManager (docs/design/plugins/RuntimeManager.md): acquires the Node
//! and Python rutis runtimes into `<home>/runtimes/`, verified against their
//! publishers' checksums, and provides each one's launch command once it is
//! installed. The runtime's connection row injects the command, so rutis
//! starts it then.
use crate::methods::ManagementMethods;
use crate::session_typescript::{ForeignRuntime, RuntimeState, RuntimeStatus};
use dim_agent_capabilities::methods::{Interruption, MethodDecl, RpcError, params, reply};
use dim_agent_runtime::Names;
use rutis::Ctx;
use rutis::Disposer;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    path::{Component, Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

const PINS: &str = include_str!("../../../runtime/runtimes.json");
const NODE_PACKAGE: &str = include_str!("../../../runtime/package.json");
const NODE_LOCK: &str = include_str!("../../../runtime/package-lock.json");
const PYTHON_REQUIREMENTS: &str = include_str!("../../../runtime/python-requirements.txt");

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pins {
    node: Pin,
    python: Pin,
    npm_registries: Vec<String>,
    python_indexes: Vec<String>,
}
#[derive(Deserialize)]
struct Pin {
    version: String,
    checksums: String,
    sources: Vec<String>,
    files: HashMap<String, String>,
}
#[derive(Clone, Debug)]
struct Archive {
    file: String,
    checksums: String,
}

/// Where a runtime's interpreter comes from (design §2.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Download,
    System,
}

/// The two runtimes RuntimeManager supplies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Node,
    Python,
}
impl Kind {
    pub const ALL: [Kind; 2] = [Kind::Node, Kind::Python];
    pub fn name(self) -> &'static str {
        match self {
            Kind::Node => "node",
            Kind::Python => "python",
        }
    }
    fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }
}

/// One runtime's configuration and the paths it is installed at.
#[derive(Clone, Debug)]
pub struct Plan {
    pub source: Source,
    pub auto: bool,
    pub version: String,
    /// The interpreter the runtime runs with.
    pub interpreter: PathBuf,
    archive: Option<(Archive, PathBuf)>,
    sources: Vec<String>,
    /// The packages directory: Node's `npm ci` tree, Python's `pip --target`.
    pub packages: PathBuf,
    indexes: Vec<String>,
}

/// The runtimes of a home (design §2.3): every path follows from the fixed
/// versions, the configuration and the home.
#[derive(Clone, Debug)]
pub struct Runtimes {
    root: PathBuf,
    /// The home the configuration was read from, kept so an action can read it
    /// again (§2.2).
    home: PathBuf,
    preset: Choices,
    node: Plan,
    python: Plan,
}

/// One runtime's entry in the `runtimes` section (RuntimeManager §2.2).
#[derive(Deserialize, Default, Clone, Copy, Debug)]
#[serde(deny_unknown_fields)]
pub struct Choice {
    pub source: Option<Source>,
    pub auto: Option<bool>,
}

/// The `runtimes` section of the default configuration file, by runtime name.
pub type Choices = BTreeMap<String, Choice>;

impl Runtimes {
    /// The runtimes of `home`: the default configuration file's `preset`,
    /// overridden by `<home>/runtimes/config.json`.
    pub fn load(home: &Path, preset: &Choices) -> Result<Self, String> {
        let root = home.join("runtimes");
        let user_file = root.join("config.json");
        let user = match std::fs::read(&user_file) {
            Ok(bytes) => serde_json::from_slice::<Choices>(&bytes)
                .map_err(|e| format!("{}: {e}", user_file.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Choices::new(),
            Err(error) => return Err(format!("{}: {error}", user_file.display())),
        };
        for name in preset.keys().chain(user.keys()) {
            if Kind::parse(name).is_none() {
                return Err(format!("unknown runtime {name}"));
            }
        }
        let choice = |kind: Kind| {
            let preset = preset.get(kind.name()).copied().unwrap_or_default();
            let user = user.get(kind.name()).copied().unwrap_or_default();
            (
                user.source.or(preset.source).unwrap_or(Source::Download),
                user.auto.or(preset.auto).unwrap_or(false),
            )
        };
        Self::new(
            home,
            root,
            preset.clone(),
            choice(Kind::Node),
            choice(Kind::Python),
        )
    }

    /// The configuration this value was loaded with, read again (§2.2): the
    /// action that applies a changed `source` or `auto` reads `config.json` and
    /// the environment at that moment instead of at startup. `runtime.list`
    /// keeps reporting the runtime in effect until such an action runs.
    pub fn reload(&self) -> Result<Self, String> {
        Self::load(&self.home, &self.preset)
    }

    fn new(
        home: &Path,
        root: PathBuf,
        preset: Choices,
        node: (Source, bool),
        python: (Source, bool),
    ) -> Result<Self, String> {
        let pins: Pins = serde_json::from_str(PINS).expect("runtime/runtimes.json is valid");
        let platform = platform();
        let mirror = |variable: &str| std::env::var(variable).ok().filter(|v| !v.is_empty());
        let sources = |pin: &Pin, variable: &str| {
            mirror(variable)
                .map(|base| format!("{}/{{file}}", base.trim_end_matches('/')))
                .into_iter()
                .chain(pin.sources.iter().cloned())
                .map(|source| source.replace("{version}", &pin.version))
                .collect::<Vec<_>>()
        };
        let registries: Vec<String> = mirror("DIMCODE_NPM_REGISTRY")
            .into_iter()
            .chain(pins.npm_registries.iter().cloned())
            .collect();
        let indexes: Vec<String> = mirror("DIMCODE_PYTHON_INDEX")
            .into_iter()
            .chain(pins.python_indexes.iter().cloned())
            .collect();
        let digest = |parts: &[&str]| {
            let mut hash = Sha256::new();
            parts.iter().for_each(|part| hash.update(part.as_bytes()));
            hex(&hash.finalize())[..16].to_owned()
        };
        let archive = |pin: &Pin, directory: String| {
            pin.files.get(&platform).map(|file| {
                let archive = Archive {
                    file: file.clone(),
                    checksums: pin.checksums.replace("{version}", &pin.version),
                };
                (archive, root.join(directory))
            })
        };
        let node_archive = archive(
            &pins.node,
            format!("node-v{}-{platform}", pins.node.version),
        );
        let node_interpreter = match node.0 {
            Source::Download => {
                let (_, directory) = node_archive
                    .as_ref()
                    .ok_or_else(|| format!("no Node download for {platform}"))?;
                if cfg!(windows) {
                    directory.join("node.exe")
                } else {
                    directory.join("bin/node")
                }
            }
            Source::System => which("node").unwrap_or_else(|| root.join("no-system-node")),
        };
        let python_archive = archive(
            &pins.python,
            format!("python-{}-{platform}", pins.python.version),
        );
        let python_interpreter = match python.0 {
            Source::Download => {
                let (_, directory) = python_archive
                    .as_ref()
                    .ok_or_else(|| format!("no Python download for {platform}"))?;
                if cfg!(windows) {
                    directory.join("python.exe")
                } else {
                    directory.join("bin/python3")
                }
            }
            Source::System => {
                let name = if cfg!(windows) { "python" } else { "python3" };
                which(name).unwrap_or_else(|| root.join("no-system-python"))
            }
        };
        let node_plan = Plan {
            source: node.0,
            auto: node.1,
            version: pins.node.version.clone(),
            interpreter: node_interpreter,
            archive: (node.0 == Source::Download)
                .then_some(node_archive)
                .flatten(),
            sources: sources(&pins.node, "DIMCODE_NODE_MIRROR"),
            packages: root.join(format!(
                "node-packages-{}",
                digest(&[NODE_PACKAGE, NODE_LOCK])
            )),
            indexes: registries,
        };
        let python_plan = Plan {
            source: python.0,
            auto: python.1,
            version: pins.python.version.clone(),
            interpreter: python_interpreter,
            archive: (python.0 == Source::Download)
                .then_some(python_archive)
                .flatten(),
            sources: sources(&pins.python, "DIMCODE_PYTHON_MIRROR"),
            packages: root.join(format!(
                "python-packages-{}",
                digest(&[PYTHON_REQUIREMENTS])
            )),
            indexes,
        };
        Ok(Self {
            root,
            home: home.to_owned(),
            preset,
            node: node_plan,
            python: python_plan,
        })
    }

    pub fn plan(&self, kind: Kind) -> &Plan {
        match kind {
            Kind::Node => &self.node,
            Kind::Python => &self.python,
        }
    }
    /// The home this configuration was read from.
    pub fn home(&self) -> &Path {
        &self.home
    }
    /// `@arcships/rutis-runtime` in the Node packages directory.
    pub fn js_runtime(&self) -> PathBuf {
        self.node
            .packages
            .join("node_modules/@arcships/rutis-runtime")
    }
    /// The anchor Node resolves packages from.
    pub fn anchor(&self) -> PathBuf {
        self.node.packages.join("package.json")
    }
    fn record_file(&self) -> PathBuf {
        self.root.join("runtimes.json")
    }
}

fn platform() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        other => other,
    };
    format!("{os}-{arch}")
}

fn which(program: &str) -> Option<PathBuf> {
    let file = if cfg!(windows) {
        format!("{program}.exe")
    } else {
        program.to_owned()
    };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|directory| directory.join(&file))
        .find(|path| path.is_file())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
struct Record {
    source: Source,
    version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sha256: Option<String>,
    packages: String,
}
impl Record {
    fn of(plan: &Plan, version: &str, sha256: Option<String>) -> Self {
        Self {
            source: plan.source,
            version: version.to_owned(),
            sha256,
            packages: plan
                .packages
                .file_name()
                .expect("a packages directory")
                .to_string_lossy()
                .into_owned(),
        }
    }
}

fn read_records(path: &Path) -> BTreeMap<String, Record> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// A runtime's entry in `runtime.list` (design §5).
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct RuntimeEntry {
    pub name: &'static str,
    pub source: Source,
    /// The fixed version for `download`; for `system`, the version of the
    /// checked interpreter, absent until it is checked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// `off`, `acquiring`, `failed`, or, once its launch command is
    /// provided, its connection's status: `starting`, `ready`, `down`.
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

enum State {
    Off,
    Acquiring,
    Failed(String),
    /// Its launch command is provided; `None` while it is provided again.
    Provided(Option<Disposer>),
}

/// The process service of RuntimeManager: what other process parts (and the
/// `runtime` methods) call.
pub struct RuntimeService {
    /// The configuration in effect: replaced when `runtime.install` applies a
    /// changed `source` (§3.1).
    runtimes: Mutex<Arc<Runtimes>>,
    local: Mutex<[ForeignRuntime; 2]>,
    states: Mutex<[State; 2]>,
    versions: Mutex<[Option<String>; 2]>,
    record: tokio::sync::Mutex<()>,
    ctx: Ctx,
    stop: CancellationToken,
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}
impl RuntimeService {
    fn index(kind: Kind) -> usize {
        kind as usize
    }
    /// Every runtime's status.
    pub fn list(&self) -> Vec<RuntimeEntry> {
        let runtimes = self.runtimes.lock().unwrap().clone();
        let states = self.states.lock().unwrap();
        let versions = self.versions.lock().unwrap();
        Kind::ALL
            .into_iter()
            .map(|kind| {
                let plan = runtimes.plan(kind);
                let (status, reason) = match &states[Self::index(kind)] {
                    State::Off => ("off", None),
                    State::Acquiring => ("acquiring", None),
                    State::Failed(reason) => ("failed", Some(reason.clone())),
                    State::Provided(_) => match self.status(kind) {
                        RuntimeState::Starting => ("starting", None),
                        RuntimeState::Ready => ("ready", None),
                        RuntimeState::Down(reason) => ("down", Some(reason)),
                    },
                };
                RuntimeEntry {
                    name: kind.name(),
                    source: plan.source,
                    version: match plan.source {
                        Source::Download => Some(plan.version.clone()),
                        Source::System => versions[Self::index(kind)].clone(),
                    },
                    status,
                    reason,
                }
            })
            .collect()
    }

    /// The status of `kind`'s connection; `starting` while its row has not
    /// started.
    fn status(&self, kind: Kind) -> RuntimeState {
        let row = self.local.lock().unwrap()[Self::index(kind)].row;
        self.ctx
            .get_at(&RuntimeStatus::key(row))
            .map_or(RuntimeState::Starting, |status| status.state())
    }

    /// Provides `kind`'s launch command.
    fn provide(&self, kind: Kind) -> State {
        let runtime = self.local.lock().unwrap()[Self::index(kind)].clone();
        match self
            .ctx
            .provide_at(runtime.command.clone(), runtime.launch.clone())
        {
            Ok(provided) => State::Provided(Some(provided)),
            Err(error) => State::Failed(error.to_string()),
        }
    }

    /// Turns `kind` on and applies the configuration in effect. It reads
    /// `<home>/runtimes/config.json` again (§2.2), and when the `source` it
    /// gives differs from the one in effect, withdraws the launch command,
    /// disposes the running connection and acquires under the new source. A
    /// configuration that does not read fails here and leaves the runtime as it
    /// was.
    pub fn install(self: &Arc<Self>, kind: Kind) -> Result<(), String> {
        let reloaded = Arc::new(self.runtimes.lock().unwrap().reload()?);
        let changed = {
            let current = self.runtimes.lock().unwrap();
            reloaded.plan(kind).source != current.plan(kind).source
        };
        if changed {
            self.local.lock().unwrap()[Self::index(kind)] = launch(kind, &reloaded);
            *self.runtimes.lock().unwrap() = reloaded;
        }
        let (disposed, acquire) = {
            let mut states = self.states.lock().unwrap();
            let slot = &mut states[Self::index(kind)];
            match slot {
                State::Acquiring | State::Provided(None) => (None, false),
                State::Provided(provided @ Some(_)) => {
                    let down = matches!(self.status(kind), RuntimeState::Down(_));
                    if changed || down {
                        (provided.take(), changed)
                    } else {
                        (None, false)
                    }
                }
                State::Off | State::Failed(_) => (None, true),
            }
        };
        match disposed {
            Some(disposed) => {
                let service = self.clone();
                let task = tokio::spawn(async move {
                    if let Err(error) = disposed.dispose().await {
                        tracing::warn!(runtime = kind.name(), %error, "runtime command withdrawal failed");
                    }
                    let state = if acquire {
                        service
                            .acquired(kind)
                            .await
                            .unwrap_or_else(|| State::Acquiring)
                    } else {
                        service.provide(kind)
                    };
                    service.states.lock().unwrap()[Self::index(kind)] = state;
                });
                self.tasks.lock().unwrap().push(task);
            }
            None if acquire => {
                self.states.lock().unwrap()[Self::index(kind)] = State::Acquiring;
                let service = self.clone();
                let task = tokio::spawn(async move {
                    let state = service
                        .acquired(kind)
                        .await
                        .unwrap_or_else(|| State::Acquiring);
                    service.states.lock().unwrap()[Self::index(kind)] = state;
                });
                self.tasks.lock().unwrap().push(task);
            }
            None => {}
        }
        Ok(())
    }

    /// Acquires `kind` and provides its launch command; `None` when the
    /// service is stopping, which leaves the runtime as it is.
    async fn acquired(self: &Arc<Self>, kind: Kind) -> Option<State> {
        let result = tokio::select! {
            result = self.acquire(kind) => result,
            _ = self.stop.cancelled() => return None,
        };
        Some(match result {
            Ok(version) => {
                self.versions.lock().unwrap()[Self::index(kind)] = Some(version);
                self.provide(kind)
            }
            Err(reason) => {
                tracing::warn!(runtime = kind.name(), %reason, "runtime acquisition failed");
                State::Failed(reason)
            }
        })
    }

    async fn acquire(&self, kind: Kind) -> Result<String, String> {
        let runtimes = self.runtimes.lock().unwrap().clone();
        let plan = runtimes.plan(kind);
        let version = match plan.source {
            Source::Download => plan.version.clone(),
            Source::System => check_system(kind, &plan.interpreter).await?,
        };
        let installed = read_records(&runtimes.record_file())
            .get(kind.name())
            .is_some_and(|record| *record == Record::of(plan, &version, record.sha256.clone()))
            && plan
                .archive
                .as_ref()
                .is_none_or(|(_, directory)| directory.is_dir())
            && plan.packages.is_dir();
        if installed {
            return Ok(version);
        }
        std::fs::create_dir_all(&runtimes.root).map_err(|e| e.to_string())?;
        let sha256 = match &plan.archive {
            Some((archive, directory)) => {
                let client = client()?;
                let sha256 = checksum(&client, archive).await?;
                if !directory.is_dir() {
                    download(&client, &plan.sources, &archive.file, &sha256, directory).await?;
                }
                Some(sha256)
            }
            None => None,
        };
        let record = Record::of(plan, &version, sha256);
        if !plan.packages.is_dir() {
            match kind {
                Kind::Node => npm_ci(plan).await?,
                Kind::Python => pip_install(plan).await?,
            }
        }
        let _guard = self.record.lock().await;
        let path = runtimes.record_file();
        let mut records = read_records(&path);
        records.insert(kind.name().to_owned(), record);
        let bytes = serde_json::to_vec_pretty(&records).expect("records serialize");
        let partial = sibling(&path, ".partial");
        std::fs::write(&partial, bytes)
            .and_then(|()| std::fs::rename(&partial, &path))
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(version)
    }
}

/// `kind`'s launch command for `runtimes` (RuntimeManager §2.3): what its
/// connection row starts, rebuilt when `runtime.install` applies a changed
/// configuration.
fn launch(kind: Kind, runtimes: &Runtimes) -> ForeignRuntime {
    let plan = runtimes.plan(kind);
    match kind {
        Kind::Node => {
            ForeignRuntime::node(&plan.interpreter, &runtimes.js_runtime(), runtimes.anchor())
        }
        Kind::Python => ForeignRuntime::python(
            &plan.interpreter,
            Some(&plan.packages),
            runtimes.home().join("plugins").join(".python"),
        ),
    }
}

/// `path` with `suffix` appended to its whole file name.
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(suffix);
    path.with_file_name(name)
}

const TIMEOUT: Duration = Duration::from_secs(60);

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(TIMEOUT)
        .read_timeout(TIMEOUT)
        .build()
        .map_err(|e| e.to_string())
}

async fn checksum(client: &reqwest::Client, archive: &Archive) -> Result<String, String> {
    let text = client
        .get(&archive.checksums)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| format!("{}: {e}", archive.checksums))?
        .text()
        .await
        .map_err(|e| format!("{}: {e}", archive.checksums))?;
    text.lines()
        .filter_map(|line| line.split_once(char::is_whitespace))
        .find(|(_, name)| name.trim().trim_start_matches('*') == archive.file)
        .map(|(digest, _)| digest.to_ascii_lowercase())
        .filter(|digest| digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| {
            format!(
                "{} lists no SHA-256 for {}",
                archive.checksums, archive.file
            )
        })
}

async fn download(
    client: &reqwest::Client,
    sources: &[String],
    file: &str,
    sha256: &str,
    directory: &Path,
) -> Result<(), String> {
    let partial = sibling(directory, ".partial");
    let archive = sibling(directory, ".download");
    let mut errors = Vec::new();
    for source in sources {
        let url = source.replace("{file}", &file.replace('+', "%2B"));
        match fetch(client, &url, &archive, sha256).await {
            Ok(()) => {
                errors.clear();
                break;
            }
            Err(error) => errors.push(format!("{url}: {error}")),
        }
    }
    if !errors.is_empty() || sources.is_empty() {
        let _ = std::fs::remove_file(&archive);
        return Err(format!("{file} not acquired: {}", errors.join("; ")));
    }
    let _ = std::fs::remove_dir_all(&partial);
    let name = file.to_owned();
    let (from, to) = (archive.clone(), partial.clone());
    let unpacked = tokio::task::spawn_blocking(move || unpack(&name, &from, &to))
        .await
        .map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&archive);
    if let Err(error) = unpacked {
        let _ = std::fs::remove_dir_all(&partial);
        return Err(format!("{file}: {error}"));
    }
    std::fs::rename(&partial, directory).map_err(|e| format!("{}: {e}", directory.display()))
}

async fn fetch(
    client: &reqwest::Client,
    url: &str,
    path: &Path,
    expected: &str,
) -> Result<(), String> {
    let mut response = client
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| e.to_string())?;
    let mut out = tokio::fs::File::create(path)
        .await
        .map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        hash.update(&chunk);
        out.write_all(&chunk).await.map_err(|e| e.to_string())?;
    }
    out.flush().await.map_err(|e| e.to_string())?;
    drop(out);
    let actual = hex(&hash.finalize());
    if actual != expected {
        let _ = std::fs::remove_file(path);
        return Err(format!(
            "SHA-256 {actual} does not match the published {expected}"
        ));
    }
    Ok(())
}

fn unpack(name: &str, archive: &Path, into: &Path) -> Result<(), String> {
    std::fs::create_dir_all(into).map_err(|e| e.to_string())?;
    let inside = |path: &Path| -> Result<PathBuf, String> {
        if !path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
        {
            return Err(format!("refusing archive path {}", path.display()));
        }
        Ok(path.components().skip(1).collect())
    };
    if name.ends_with(".zip") {
        return unzip(archive, into, &inside);
    }
    let file = std::fs::File::open(archive).map_err(|e| e.to_string())?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    for entry in tar.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path().map_err(|e| e.to_string())?.into_owned();
        let relative = inside(&path)?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        let destination = into.join(&relative);
        let kind = entry.header().entry_type();
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        match kind {
            tar::EntryType::Directory => {
                std::fs::create_dir_all(&destination).map_err(|e| e.to_string())?
            }
            tar::EntryType::Regular | tar::EntryType::Continuous => {
                let mut out = std::fs::File::create(&destination).map_err(|e| e.to_string())?;
                std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = entry.header().mode().map_err(|e| e.to_string())? & 0o755;
                    std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(mode))
                        .map_err(|e| e.to_string())?;
                }
            }
            tar::EntryType::Symlink | tar::EntryType::Link => {
                let target = entry
                    .link_name()
                    .map_err(|e| e.to_string())?
                    .ok_or("a link without a target")?
                    .into_owned();
                let resolved = if kind == tar::EntryType::Symlink {
                    relative.parent().unwrap_or(Path::new("")).join(&target)
                } else {
                    inside(&target)?
                };
                let mut depth = 0i32;
                for component in resolved.components() {
                    depth += match component {
                        Component::Normal(_) => 1,
                        Component::ParentDir => -1,
                        Component::CurDir => 0,
                        _ => return Err(format!("refusing link {}", path.display())),
                    };
                    if depth < 0 {
                        return Err(format!("refusing link {}", path.display()));
                    }
                }
                if kind == tar::EntryType::Link {
                    std::fs::copy(into.join(&resolved), &destination).map_err(|e| e.to_string())?;
                } else {
                    #[cfg(unix)]
                    std::os::unix::fs::symlink(&target, &destination).map_err(|e| e.to_string())?;
                    #[cfg(windows)]
                    std::fs::copy(into.join(&resolved), &destination).map_err(|e| e.to_string())?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn unzip(
    archive: &Path,
    into: &Path,
    inside: &dyn Fn(&Path) -> Result<PathBuf, String>,
) -> Result<(), String> {
    let file = std::fs::File::open(archive).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).map_err(|e| e.to_string())?;
        let raw = String::from_utf8_lossy(entry.name_raw()).replace('\\', "/");
        let path = PathBuf::from(&raw);
        if entry.enclosed_name().is_none() {
            return Err(format!("refusing archive path {raw}"));
        }
        if entry.is_symlink() {
            return Err(format!("refusing link {}", path.display()));
        }
        let relative = inside(&path)?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        let destination = into.join(relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&destination).map_err(|e| e.to_string())?;
        } else if entry.is_file() {
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let mut out = std::fs::File::create(&destination).map_err(|e| e.to_string())?;
            std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
            #[cfg(unix)]
            if let Some(mode) = entry.unix_mode() {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(
                    &destination,
                    std::fs::Permissions::from_mode(mode & 0o755),
                )
                .map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(())
}

async fn run(mut command: tokio::process::Command, what: &str) -> Result<String, String> {
    let output = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| format!("{what}: {e}"))?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    let text = String::from_utf8_lossy(&output.stderr);
    let tail: Vec<&str> = text.lines().rev().take(5).collect();
    Err(format!(
        "{what}: {}: {}",
        output.status,
        tail.into_iter().rev().collect::<Vec<_>>().join(" | ")
    ))
}

/// Checks a `system` interpreter (design §2.2) and returns its version.
async fn check_system(kind: Kind, interpreter: &Path) -> Result<String, String> {
    let mut command = tokio::process::Command::new(interpreter);
    let (minimum, accepts): (&str, fn(u32, u32) -> bool) = match kind {
        Kind::Node => {
            command.arg("--version");
            ("22.19", |major, minor| {
                major > 23 || (major == 23 && minor >= 11) || (major == 22 && minor >= 19)
            })
        }
        Kind::Python => {
            command.args(["-c", "import sys; print('%d.%d.%d' % sys.version_info[:3])"]);
            ("3.12", |major, minor| {
                major > 3 || (major == 3 && minor >= 12)
            })
        }
    };
    if !interpreter.is_file() {
        return Err(format!("no {} interpreter on PATH", kind.name()));
    }
    let what = format!("{} {}", kind.name(), interpreter.display());
    let version = run(command, &what).await?;
    let mut parts = version
        .trim()
        .trim_start_matches('v')
        .split('.')
        .map(|part| part.parse::<u32>().ok());
    let version = version.trim().trim_start_matches('v').to_owned();
    match (parts.next().flatten(), parts.next().flatten()) {
        (Some(major), Some(minor)) if accepts(major, minor) => {}
        _ => {
            return Err(format!(
                "{what} is {version}, {minimum} or later is required"
            ));
        }
    }
    if kind == Kind::Node && !npm_cli(interpreter).is_file() {
        return Err(format!(
            "{what} has no bundled npm at {}",
            npm_cli(interpreter).display()
        ));
    }
    Ok(version)
}

fn npm_cli(node: &Path) -> PathBuf {
    let node = node.canonicalize().unwrap_or_else(|_| node.to_owned());
    let directory = node.parent().unwrap_or(Path::new(""));
    if cfg!(windows) {
        directory.join("node_modules/npm/bin/npm-cli.js")
    } else {
        directory.join("../lib/node_modules/npm/bin/npm-cli.js")
    }
}

async fn npm_ci(plan: &Plan) -> Result<(), String> {
    let partial = sibling(&plan.packages, ".partial");
    let _ = std::fs::remove_dir_all(&partial);
    std::fs::create_dir_all(&partial).map_err(|e| e.to_string())?;
    std::fs::write(partial.join("package.json"), NODE_PACKAGE)
        .and_then(|()| std::fs::write(partial.join("package-lock.json"), NODE_LOCK))
        .map_err(|e| e.to_string())?;
    let cache = sibling(&plan.packages, ".npm-cache");
    let mut errors = Vec::new();
    for registry in &plan.indexes {
        let mut command = tokio::process::Command::new(&plan.interpreter);
        command
            .arg(npm_cli(&plan.interpreter))
            .args([
                "ci",
                "--ignore-scripts",
                "--omit=dev",
                "--no-audit",
                "--no-fund",
                "--registry",
                registry,
            ])
            .env("npm_config_cache", &cache)
            .current_dir(&partial);
        match run(command, "npm ci").await {
            Ok(_) => {
                errors.clear();
                break;
            }
            Err(error) => errors.push(format!("{registry}: {error}")),
        }
    }
    let _ = std::fs::remove_dir_all(&cache);
    if !errors.is_empty() {
        let _ = std::fs::remove_dir_all(&partial);
        return Err(format!(
            "Node packages not installed: {}",
            errors.join("; ")
        ));
    }
    std::fs::rename(&partial, &plan.packages).map_err(|e| e.to_string())
}

async fn pip_install(plan: &Plan) -> Result<(), String> {
    let partial = sibling(&plan.packages, ".partial");
    let requirements = sibling(&plan.packages, ".requirements.txt");
    let _ = std::fs::remove_dir_all(&partial);
    std::fs::write(&requirements, PYTHON_REQUIREMENTS).map_err(|e| e.to_string())?;
    let mut errors = Vec::new();
    for index in &plan.indexes {
        let mut command = tokio::process::Command::new(&plan.interpreter);
        command
            .args([
                "-I",
                "-m",
                "pip",
                "install",
                "--require-hashes",
                "--no-deps",
                "--only-binary",
                ":all:",
                "--no-input",
                "--no-cache-dir",
                "--disable-pip-version-check",
                "--index-url",
                index,
                "--target",
            ])
            .arg(&partial)
            .arg("-r")
            .arg(&requirements)
            .env(
                "PIP_CONFIG_FILE",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            );
        match run(command, "pip install").await {
            Ok(_) => {
                errors.clear();
                break;
            }
            Err(error) => {
                let _ = std::fs::remove_dir_all(&partial);
                errors.push(format!("{index}: {error}"));
            }
        }
    }
    let _ = std::fs::remove_file(&requirements);
    if !errors.is_empty() {
        return Err(format!(
            "Python packages not installed: {}",
            errors.join("; ")
        ));
    }
    std::fs::rename(&partial, &plan.packages).map_err(|e| e.to_string())
}

/// The process row `RuntimeManager`.
pub struct RuntimeManagerPlugin {
    methods: Key<ManagementMethods>,
    service: Key<RuntimeService>,
    runtimes: Arc<Runtimes>,
    local: [ForeignRuntime; 2],
}
impl RuntimeManagerPlugin {
    pub(crate) fn new(
        names: &Names<'_>,
        runtimes: Arc<Runtimes>,
        node: ForeignRuntime,
        python: ForeignRuntime,
    ) -> rutis::Result<Self> {
        Ok(Self {
            methods: names.key()?,
            service: names.key()?,
            runtimes,
            local: [node, python],
        })
    }
}
impl DynPlugin for RuntimeManagerPlugin {
    fn name(&self) -> &str {
        "RuntimeManager"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.methods.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let methods = ctx.require_at(&self.methods)?;
            let records = read_records(&self.runtimes.record_file());
            let service = Arc::new(RuntimeService {
                runtimes: Mutex::new(self.runtimes.clone()),
                local: Mutex::new(self.local.clone()),
                states: Mutex::new([State::Off, State::Off]),
                versions: Mutex::new([None, None]),
                record: tokio::sync::Mutex::new(()),
                ctx: ctx.clone(),
                stop: CancellationToken::new(),
                tasks: Mutex::new(Vec::new()),
            });
            let owned = service.clone();
            ctx.effect(move || async move {
                owned.stop.cancel();
                let tasks = std::mem::take(&mut *owned.tasks.lock().unwrap());
                for task in tasks {
                    let _ = task.await;
                }
                Ok(())
            })?;
            ctx.provide_at(self.service.clone(), service.clone())?;
            let handler = service.clone();
            methods.register(
                ctx,
                "runtime",
                vec![
                    MethodDecl::new("runtime.list", false, Interruption::Report),
                    MethodDecl::new("runtime.install", true, Interruption::Report),
                ],
                Arc::new(move |method, value, _stopping, _| {
                    let service = handler.clone();
                    Box::pin(async move {
                        match method.as_str() {
                            "runtime.list" => reply(service.list()),
                            "runtime.install" => {
                                #[derive(Deserialize)]
                                struct Install {
                                    name: String,
                                }
                                let Install { name } = params(value)?;
                                let kind = Kind::parse(&name).ok_or_else(|| {
                                    RpcError::invalid(format!("unknown runtime {name}"))
                                })?;
                                service.install(kind).map_err(|error| {
                                    RpcError::app(-32000, "runtime_configuration", error)
                                })?;
                                reply(service.list().into_iter().find(|s| s.name == kind.name()))
                            }
                            _ => unreachable!("registered runtime management method"),
                        }
                    })
                }),
            )?;
            for kind in Kind::ALL {
                if (self.runtimes.plan(kind).auto || records.contains_key(kind.name()))
                    && let Err(error) = service.install(kind)
                {
                    tracing::warn!(runtime = kind.name(), %error, "runtime configuration did not read");
                }
            }
            Ok(Effect::Done)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Archive, PINS, Pins, checksum, client, download, unpack};
    use std::path::Path;

    #[test]
    fn every_release_platform_has_an_archive_and_an_official_checksum() {
        let pins: Pins = serde_json::from_str(PINS).unwrap();
        assert!(pins.node.checksums.starts_with("https://nodejs.org/"));
        assert!(
            pins.python
                .checksums
                .starts_with("https://github.com/astral-sh/python-build-standalone/")
        );
        for platform in [
            "darwin-arm64",
            "darwin-x64",
            "linux-arm64",
            "linux-x64",
            "windows-arm64",
            "windows-x64",
        ] {
            for pin in [&pins.node, &pins.python] {
                assert!(pin.files.contains_key(platform), "{platform}");
            }
        }
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("dim-rm-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn a_download_that_does_not_match_its_published_checksum_is_refused() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let published = "ab".repeat(32);
        let sums = format!(
            "{published}  node.tar.gz\n{}  other.tar.gz\n",
            "cd".repeat(32)
        );
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0u8; 1024];
                let read = stream.read(&mut request).await.unwrap_or(0);
                let body = if String::from_utf8_lossy(&request[..read]).contains("SUMS") {
                    sums.clone()
                } else {
                    "tampered".to_owned()
                };
                let _ = stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await;
            }
        });
        let client = client().unwrap();
        let archive = |file: &str| Archive {
            file: file.into(),
            checksums: format!("http://{address}/SUMS"),
        };
        assert_eq!(
            checksum(&client, &archive("node.tar.gz")).await.unwrap(),
            published
        );
        let unlisted = checksum(&client, &archive("node.zip")).await.unwrap_err();
        assert!(
            unlisted.contains("lists no SHA-256 for node.zip"),
            "{unlisted}"
        );
        let root = scratch("pin");
        let directory = root.join("node-v0-test");
        let error = download(
            &client,
            &[format!("http://{address}/{{file}}")],
            "node.tar.gz",
            &published,
            &directory,
        )
        .await
        .unwrap_err();
        assert!(error.contains("does not match the published"), "{error}");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn archive(path: &Path, build: impl FnOnce(&mut tar::Builder<Vec<u8>>)) {
        let mut builder = tar::Builder::new(Vec::new());
        build(&mut builder);
        let tar = builder.into_inner().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, &tar).unwrap();
        std::fs::write(path, gz.finish().unwrap()).unwrap();
    }

    #[test]
    fn unpacking_stays_inside_its_directory() {
        let root = scratch("unpack");
        let entry = |builder: &mut tar::Builder<Vec<u8>>, kind, path: &str, link: Option<&str>| {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(kind);
            header.set_mode(0o755);
            header.set_size(if link.is_some() { 0 } else { 2 });
            if let Some(link) = link {
                header.set_link_name(link).unwrap();
            }
            let name = &mut header.as_gnu_mut().unwrap().name;
            name[..path.len()].copy_from_slice(path.as_bytes());
            header.set_cksum();
            let data: &[u8] = if link.is_some() { b"" } else { b"ok" };
            builder.append(&header, data).unwrap();
        };
        let good = root.join("good.tar.gz");
        archive(&good, |b| {
            entry(b, tar::EntryType::Regular, "top/bin/node", None);
            entry(
                b,
                tar::EntryType::Symlink,
                "top/bin/npm",
                Some("../lib/npm.js"),
            );
        });
        unpack("good.tar.gz", &good, &root.join("good")).unwrap();
        assert_eq!(std::fs::read(root.join("good/bin/node")).unwrap(), b"ok");
        assert!(root.join("good/bin/npm").is_symlink());
        for (name, kind, path, link) in [
            ("parent", tar::EntryType::Regular, "top/../../escape", None),
            ("rooted", tar::EntryType::Regular, "/top/escape", None),
            (
                "link",
                tar::EntryType::Symlink,
                "top/bin/x",
                Some("../../../etc/passwd"),
            ),
            (
                "absolute",
                tar::EntryType::Symlink,
                "top/bin/y",
                Some("/etc/passwd"),
            ),
        ] {
            let file = root.join(format!("{name}.tar.gz"));
            archive(&file, |b| entry(b, kind, path, link));
            let error = unpack("x.tar.gz", &file, &root.join(name)).unwrap_err();
            assert!(error.contains("refusing"), "{name}: {error}");
        }
        assert!(!root.parent().unwrap().join("escape").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unpacking_drops_privileged_permission_bits() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch("mode");
        let file = root.join("mode.tar.gz");
        archive(&file, |builder| {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Regular);
            header.set_path("top/bin/node").unwrap();
            header.set_mode(0o7777);
            header.set_size(2);
            header.set_cksum();
            builder.append(&header, &b"ok"[..]).unwrap();
        });
        unpack("mode.tar.gz", &file, &root.join("out")).unwrap();
        let mode = std::fs::metadata(root.join("out/bin/node"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o7777, 0o755);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unzipping_stays_inside_and_refuses_links() {
        use std::io::Write;
        use zip::write::SimpleFileOptions;
        let root = scratch("unzip");
        let build = |name: &str, add: &dyn Fn(&mut zip::ZipWriter<std::fs::File>)| {
            let path = root.join(name);
            let mut writer = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
            add(&mut writer);
            writer.finish().unwrap();
            path
        };
        let good = build("good.zip", &|w| {
            w.start_file(
                "top/node.exe",
                SimpleFileOptions::default().unix_permissions(0o4755),
            )
            .unwrap();
            w.write_all(b"ok").unwrap();
        });
        unpack("good.zip", &good, &root.join("good")).unwrap();
        assert_eq!(std::fs::read(root.join("good/node.exe")).unwrap(), b"ok");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(root.join("good/node.exe"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o7000, 0);
        }
        type Add<'a> = &'a dyn Fn(&mut zip::ZipWriter<std::fs::File>);
        let cases: [(&str, Add); 3] = [
            ("parent", &|w| {
                w.start_file("top/../../escape", SimpleFileOptions::default())
                    .unwrap();
                w.write_all(b"x").unwrap();
            }),
            ("absolute", &|w| {
                w.start_file("/tmp/escape", SimpleFileOptions::default())
                    .unwrap();
                w.write_all(b"x").unwrap();
            }),
            ("link", &|w| {
                w.add_symlink("top/link", "../../../etc", SimpleFileOptions::default())
                    .unwrap();
            }),
        ];
        for (name, add) in cases {
            let file = build(&format!("{name}.zip"), add);
            let error = unpack("x.zip", &file, &root.join(name)).unwrap_err();
            assert!(error.contains("refusing"), "{name}: {error}");
        }
        assert!(!root.parent().unwrap().join("escape").exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
