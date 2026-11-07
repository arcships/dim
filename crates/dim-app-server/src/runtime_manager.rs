//! RuntimeManager (docs/design/plugins/RuntimeManager.md): acquires the Node
//! and Python rutis runtimes, and the uv tool, into `<home>/runtimes/`,
//! verified against their publishers' checksums. A runtime's launch command is
//! provided once it is installed, and its connection row injects it, so rutis
//! starts it then; uv launches no runtime and is only installed.
use crate::methods::ManagementMethods;
use crate::session_typescript::{ForeignRuntime, RuntimeState, RuntimeStatus};
use dim_agent_capabilities::component::{ComponentSource, ComponentSourceHost, ComponentState};
use dim_agent_capabilities::methods::{
    Interruption, MethodCall, MethodDecl, RpcError, params, reply,
};
use dim_agent_capabilities::toolchain::{InstalledToolchain, ToolchainSource, ToolchainSourceHost};
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
    uv: Pin,
    ocr: Pin,
    npm_registries: Vec<String>,
    python_indexes: Vec<String>,
}
#[derive(Deserialize)]
struct Pin {
    version: String,
    /// The publisher's checksum file. A component has none: Dim publishes the
    /// bundle, so its digests sit in this file (`sha256`).
    #[serde(default)]
    checksums: Option<String>,
    /// Dim's own digest of each published archive, by platform. When it names
    /// the platform no checksum file is fetched; the source tree is the trust
    /// anchor.
    #[serde(default)]
    sha256: Option<BTreeMap<String, String>>,
    sources: Vec<String>,
    files: HashMap<String, String>,
}
#[derive(Clone, Debug)]
struct Archive {
    file: String,
    checksums: Option<String>,
    sha256: Option<String>,
}

/// Where a runtime's interpreter comes from (design §2.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Download,
    System,
}

/// What RuntimeManager supplies: the two runtimes plugin code runs in, the uv
/// tool that launches Python MCP servers, and the OCR component's bundle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Node,
    Python,
    Uv,
    Ocr,
}
impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Node, Kind::Python, Kind::Uv, Kind::Ocr];
    pub fn name(self) -> &'static str {
        match self {
            Kind::Node => "node",
            Kind::Python => "python",
            Kind::Uv => "uv",
            Kind::Ocr => "ocr",
        }
    }
    /// Whether rutis starts a runtime process for it from a launch command.
    /// Node and Python do: each has a connection row, a `RuntimeStatus` and
    /// plugin rows waiting for its session. uv is a tool and the OCR bundle a
    /// component; neither launches a runtime.
    pub fn launches_runtime(self) -> bool {
        matches!(self, Kind::Node | Kind::Python)
    }
    /// Whether the kind is a program some process runs: it may come from the
    /// user's PATH (`source: system`), its bin directory joins a started
    /// process's PATH, and a start installs it when it is on. A component is a
    /// directory of libraries and models that only the helper a task starts
    /// reads: it is only ever downloaded, by an explicit install.
    pub fn is_program(self) -> bool {
        !matches!(self, Kind::Ocr)
    }
    fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }
}

/// What a plan's installation puts on disk, and how its presence is proven.
#[derive(Clone, Debug)]
enum Installed {
    /// A runtime or a tool: the program. A runtime's presence is its archive
    /// directory, checked beside this; a tool's is the program itself.
    Program(PathBuf),
    /// A component: the bundle directory, present while `<root>/manifest.json`
    /// is a file that the owner's verifier accepted when it was installed.
    Component { root: PathBuf, manifest: PathBuf },
}
impl Installed {
    /// Whether what the plan installs is there; `launches_runtime` spares a
    /// runtime the check of its executable, whose archive directory is enough.
    fn present(&self, launches_runtime: bool) -> bool {
        match self {
            Installed::Program(_) if launches_runtime => true,
            Installed::Program(program) => program.is_file(),
            Installed::Component { manifest, .. } => manifest.is_file(),
        }
    }
    /// The program a runtime or tool runs. A component installs none.
    fn program(&self) -> Option<&Path> {
        match self {
            Installed::Program(program) => Some(program),
            Installed::Component { .. } => None,
        }
    }
    /// The path `runtime.list` reports: the program, or the bundle root.
    fn path(&self) -> &Path {
        match self {
            Installed::Program(program) => program,
            Installed::Component { root, .. } => root,
        }
    }
}

/// One runtime's configuration and the paths it is installed at.
#[derive(Clone, Debug)]
pub struct Plan {
    pub source: Source,
    pub auto: bool,
    pub version: String,
    installed: Installed,
    /// Whether this platform has an installation at all: a component's bundle
    /// is published per target, and a target without one is offered with
    /// `false` instead of failing the load. Every program has one.
    pub offered: bool,
    archive: Option<(Archive, PathBuf)>,
    sources: Vec<String>,
    /// The packages directory: Node's `npm ci` tree, Python's `pip --target`.
    /// A tool that launches no runtime has none.
    pub packages: Option<PathBuf>,
    indexes: Vec<String>,
}
impl Plan {
    /// The program this plan installs; a component installs a directory.
    pub fn program(&self) -> Option<&Path> {
        self.installed.program()
    }
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
    /// The pins and platform this value was loaded with: the embedded file and
    /// the host for the App Server, and the same pair a test served itself.
    pins: String,
    platform: String,
    node: Plan,
    python: Plan,
    uv: Plan,
    ocr: Plan,
}

/// One runtime's entry in the `runtimes` section (RuntimeManager §2.2).
#[derive(Serialize, Deserialize, Default, Clone, Copy, Debug)]
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
        Self::load_with_pins(home, preset, PINS)
    }

    /// [`Self::load`], with the pins read from `pins` instead of the embedded
    /// `runtime/runtimes.json`. The App Server always embeds them; a test that
    /// serves its own archive passes the same pins with its own source and
    /// digest, so the whole acquisition runs without the published bytes.
    pub fn load_with_pins(home: &Path, preset: &Choices, pins: &str) -> Result<Self, String> {
        Self::load_on(home, preset, pins, &platform())
    }

    fn load_on(home: &Path, preset: &Choices, pins: &str, platform: &str) -> Result<Self, String> {
        let root = home.join("runtimes");
        let user_file = root.join("config.json");
        let user = match std::fs::read(&user_file) {
            Ok(bytes) => serde_json::from_slice::<Choices>(&bytes)
                .map_err(|e| format!("{}: {e}", user_file.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Choices::new(),
            Err(error) => return Err(format!("{}: {error}", user_file.display())),
        };
        for (name, choice) in preset.iter().chain(user.iter()) {
            let kind = Kind::parse(name).ok_or_else(|| format!("unknown runtime {name}"))?;
            if !kind.is_program() && choice.source == Some(Source::System) {
                return Err(format!(
                    "{name} is a component: it is only ever downloaded, not taken from the system"
                ));
            }
        }
        let choice = |kind: Kind| {
            let preset = preset.get(kind.name()).copied().unwrap_or_default();
            let user = user.get(kind.name()).copied().unwrap_or_default();
            (
                user.source.or(preset.source).unwrap_or(Source::Download),
                // A component is installed only by `runtime.install`, so its
                // `auto` is false whatever a hand-written file says (§2.2).
                kind.is_program() && user.auto.or(preset.auto).unwrap_or(false),
            )
        };
        Self::new(
            home,
            root,
            preset.clone(),
            pins,
            platform,
            choice(Kind::Node),
            choice(Kind::Python),
            choice(Kind::Uv),
            choice(Kind::Ocr),
        )
    }

    /// The configuration this value was loaded with, read again (§2.2): the
    /// action that applies a changed `source` or `auto` reads `config.json` and
    /// the environment at that moment instead of at startup. `runtime.list`
    /// keeps reporting the runtime in effect until such an action runs.
    pub fn reload(&self) -> Result<Self, String> {
        Self::load_on(&self.home, &self.preset, &self.pins, &self.platform)
    }

    #[allow(clippy::too_many_arguments)]
    fn new(
        home: &Path,
        root: PathBuf,
        preset: Choices,
        pins_json: &str,
        platform: &str,
        node: (Source, bool),
        python: (Source, bool),
        uv: (Source, bool),
        ocr: (Source, bool),
    ) -> Result<Self, String> {
        let pins: Pins = serde_json::from_str(pins_json).expect("runtime/runtimes.json is valid");
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
            pin.files.get(platform).map(|file| {
                let archive = Archive {
                    file: file.clone(),
                    checksums: pin
                        .checksums
                        .as_ref()
                        .map(|checksums| checksums.replace("{version}", &pin.version)),
                    sha256: pin
                        .sha256
                        .as_ref()
                        .and_then(|digests| digests.get(platform))
                        .cloned(),
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
            installed: Installed::Program(node_interpreter),
            offered: true,
            archive: (node.0 == Source::Download)
                .then_some(node_archive)
                .flatten(),
            sources: sources(&pins.node, "DIMCODE_NODE_MIRROR"),
            packages: Some(root.join(format!(
                "node-packages-{}",
                digest(&[NODE_PACKAGE, NODE_LOCK])
            ))),
            indexes: registries,
        };
        let python_plan = Plan {
            source: python.0,
            auto: python.1,
            version: pins.python.version.clone(),
            installed: Installed::Program(python_interpreter),
            offered: true,
            archive: (python.0 == Source::Download)
                .then_some(python_archive)
                .flatten(),
            sources: sources(&pins.python, "DIMCODE_PYTHON_MIRROR"),
            packages: Some(root.join(format!(
                "python-packages-{}",
                digest(&[PYTHON_REQUIREMENTS])
            ))),
            indexes,
        };
        let uv_archive = archive(&pins.uv, format!("uv-{}-{platform}", pins.uv.version));
        let uv_interpreter = match uv.0 {
            Source::Download => {
                let (_, directory) = uv_archive
                    .as_ref()
                    .ok_or_else(|| format!("no uv download for {platform}"))?;
                if cfg!(windows) {
                    directory.join("uv.exe")
                } else {
                    directory.join("uv")
                }
            }
            Source::System => which("uv").unwrap_or_else(|| root.join("no-system-uv")),
        };
        let uv_plan = Plan {
            source: uv.0,
            auto: uv.1,
            version: pins.uv.version.clone(),
            installed: Installed::Program(uv_interpreter),
            offered: true,
            archive: (uv.0 == Source::Download).then_some(uv_archive).flatten(),
            sources: sources(&pins.uv, "DIMCODE_UV_MIRROR"),
            packages: None,
            indexes: vec![],
        };
        // A platform the pin publishes no bundle for still loads: the plan
        // reports its version and `runtime.install` refuses it, so one
        // unsupported target never keeps the App Server from starting.
        let ocr_name = format!("ocr-{}-{platform}", pins.ocr.version);
        let ocr_archive = archive(&pins.ocr, ocr_name.clone());
        let ocr_root = root.join(ocr_name);
        let ocr_plan = Plan {
            source: ocr.0,
            auto: ocr.1,
            version: pins.ocr.version.clone(),
            installed: Installed::Component {
                manifest: ocr_root.join("manifest.json"),
                root: ocr_root,
            },
            offered: ocr_archive.is_some(),
            archive: ocr_archive,
            sources: sources(&pins.ocr, "DIMCODE_OCR_MIRROR"),
            packages: None,
            indexes: vec![],
        };
        Ok(Self {
            root,
            home: home.to_owned(),
            preset,
            pins: pins_json.to_owned(),
            platform: platform.to_owned(),
            node: node_plan,
            python: python_plan,
            uv: uv_plan,
            ocr: ocr_plan,
        })
    }

    pub fn plan(&self, kind: Kind) -> &Plan {
        match kind {
            Kind::Node => &self.node,
            Kind::Python => &self.python,
            Kind::Uv => &self.uv,
            Kind::Ocr => &self.ocr,
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
            .as_ref()
            .expect("Node has a packages directory")
            .join("node_modules/@arcships/rutis-runtime")
    }
    /// The anchor Node resolves packages from.
    pub fn anchor(&self) -> PathBuf {
        self.node
            .packages
            .as_ref()
            .expect("Node has a packages directory")
            .join("package.json")
    }
    fn record_file(&self) -> PathBuf {
        self.root.join("runtimes.json")
    }
    /// The user's override (§2.2): the file `runtime.configure` writes and
    /// every action that applies a configuration reads.
    fn config_file(&self) -> PathBuf {
        self.root.join("config.json")
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
    /// The packages directory; absent for a tool that launches no runtime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    packages: Option<String>,
}
impl Record {
    fn of(plan: &Plan, version: &str, sha256: Option<String>) -> Self {
        Self {
            source: plan.source,
            version: version.to_owned(),
            sha256,
            packages: plan.packages.as_ref().and_then(|packages| {
                packages
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            }),
        }
    }
}

fn read_records(path: &Path) -> BTreeMap<String, Record> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Whether `record` has `plan` installed under the plan in effect (§2.4): the
/// record is this plan's with `version`, and what the plan installs is there.
/// A tool's program and a component's manifest must be present: a `system`
/// record cannot vouch for a file the user removed, and a bundle without its
/// manifest is not one its owner accepted. An acquisition skips the download
/// on it, `runtime.list` reports `path` on it and `install` acquires again
/// without it.
fn is_installed(kind: Kind, plan: &Plan, version: &str, record: Option<&Record>) -> bool {
    record.is_some_and(|record| *record == Record::of(plan, version, record.sha256.clone()))
        && plan
            .archive
            .as_ref()
            .is_none_or(|(_, directory)| directory.is_dir())
        && plan
            .packages
            .as_ref()
            .is_none_or(|packages| packages.is_dir())
        && plan.installed.present(kind.launches_runtime())
}

/// `plan` as HostPlatform sees it (§5): a downloaded runtime installed under
/// the plan in effect contributes its interpreter's directory and the archive
/// directory that contains it. A `system` plan has no archive and contributes
/// nothing; a component contributes nothing because `toolchains()` never asks
/// about one.
fn installed_toolchain(
    kind: Kind,
    plan: &Plan,
    record: Option<&Record>,
) -> Option<InstalledToolchain> {
    let (_, root) = plan.archive.as_ref()?;
    if !is_installed(kind, plan, &plan.version, record) {
        return None;
    }
    Some(InstalledToolchain {
        // The program's directory. A component never reaches here: it is not a
        // program, and `toolchains()` filters it out anyway.
        bin: plan.installed.path().parent()?.to_path_buf(),
        root: root.clone(),
    })
}

/// The text of Python's PEP 668 marker (§7): pip refuses to install into an
/// interpreter that carries it, and names a virtual environment or uv.
const EXTERNALLY_MANAGED: &str = "[externally-managed]\nError=This Python runtime is managed by Dim and is replaced when Dim updates it. Create a virtual environment (python3 -m venv .venv) or use uv for project packages.\n";

/// The interpreter's stdlib directory inside its archive directory
/// (`sysconfig.get_path("stdlib")`): `lib/python3.<minor>` on Unix, `Lib` on
/// Windows.
fn python_stdlib(plan: &Plan) -> Option<PathBuf> {
    if cfg!(windows) {
        return Some(PathBuf::from("Lib"));
    }
    let minor = plan.version.split('.').nth(1)?;
    Some(PathBuf::from("lib").join(format!("python3.{minor}")))
}

/// The PEP 668 marker of a downloaded Python (§7). `system` interpreters are
/// never touched, and every other runtime has none.
fn externally_managed(plan: &Plan) -> Option<PathBuf> {
    let (_, directory) = plan.archive.as_ref()?;
    Some(
        directory
            .join(python_stdlib(plan)?)
            .join("EXTERNALLY-MANAGED"),
    )
}

/// Writes the marker when it is absent; one already there is left as it is.
fn create_externally_managed(marker: &Path) -> Result<(), String> {
    if marker.is_file() {
        return Ok(());
    }
    if let Some(parent) = marker.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    std::fs::write(marker, EXTERNALLY_MANAGED)
        .map_err(|error| format!("{}: {error}", marker.display()))
}

/// A runtime's entry in `runtime.list` (design §5).
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct RuntimeEntry {
    pub name: &'static str,
    pub source: Source,
    /// The configuration's `auto` (§2.2): whether every start acquires it.
    pub auto: bool,
    /// Whether this platform has something to install for it: `true` for every
    /// program kind, and for a component only where a bundle is published. A
    /// client reads it instead of matching `reason`.
    pub offered: bool,
    /// The fixed version for `download`; for `system`, the version of the
    /// checked interpreter, absent until it is checked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The path the daemon resolved, present while the record marks this
    /// runtime installed under the plan in effect (§2.4): the program's
    /// interpreter, or the component's bundle root.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// `off`, `acquiring`, `failed`, or `ready` for a tool that launches no
    /// runtime, once installed; for a runtime, once its launch command is
    /// provided, its connection's status: `starting`, `ready`, `down`.
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

enum Step {
    /// An acquisition is in flight; it owns its plan, so nothing changes.
    Busy,
    /// The runtime keeps running; its plan is the fresh one.
    Keep,
    Acquire,
    /// Dispose the launch command it was given, then acquire (`true`) or
    /// provide again.
    Restart(Option<Disposer>, bool),
}

enum State {
    Off,
    Acquiring,
    Failed(String),
    /// Its launch command is provided; `None` while it is provided again.
    Provided(Option<Disposer>),
    /// A tool or component that launches no runtime: installed under the plan
    /// in effect.
    Installed,
}

/// The process service of RuntimeManager: what other process parts (and the
/// `runtime` methods) call.
pub struct RuntimeService {
    /// The configuration file's home and preset (§2.2), which an action reads
    /// again; `plans` is what each runtime is actually on.
    runtimes: Mutex<Arc<Runtimes>>,
    /// One runtime's plan in effect. Applying a runtime replaces its own entry
    /// only, so the other runtime keeps reporting what it is running.
    plans: Mutex<[Plan; Kind::ALL.len()]>,
    /// Serializes the read-modify-write of the user's configuration file.
    config: Mutex<()>,
    /// One per kind: the launch command for a runtime, `None` for a tool.
    local: Mutex<[Option<ForeignRuntime>; Kind::ALL.len()]>,
    states: Mutex<[State; Kind::ALL.len()]>,
    versions: Mutex<[Option<String>; Kind::ALL.len()]>,
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
        let records = read_records(&self.runtimes.lock().unwrap().record_file());
        let plans = self.plans.lock().unwrap().clone();
        let states = self.states.lock().unwrap();
        let versions = self.versions.lock().unwrap();
        Kind::ALL
            .into_iter()
            .map(|kind| {
                let index = Self::index(kind);
                let plan = &plans[index];
                let version = match plan.source {
                    Source::Download => Some(plan.version.clone()),
                    Source::System => versions[index].clone(),
                };
                let installed = version.as_deref().is_some_and(|version| {
                    is_installed(kind, plan, version, records.get(kind.name()))
                });
                let (status, mut reason) = match &states[index] {
                    State::Off => ("off", None),
                    State::Acquiring => ("acquiring", None),
                    State::Failed(reason) => ("failed", Some(reason.clone())),
                    // A tool and a component have no connection: they are
                    // ready exactly while their record marks them installed
                    // under the plan in effect.
                    State::Installed if installed => ("ready", None),
                    State::Installed => ("off", None),
                    State::Provided(_) => match self.status(kind) {
                        RuntimeState::Starting => ("starting", None),
                        RuntimeState::Ready => ("ready", None),
                        RuntimeState::Down(reason) => ("down", Some(reason)),
                    },
                };
                // A component this platform has no bundle for can only be
                // refused by `runtime.install`; the row says why, so a client
                // does not offer an install that cannot succeed (§2.2).
                if !kind.is_program() && !plan.offered {
                    let platform = self.runtimes.lock().unwrap().platform.clone();
                    reason = Some(format!(
                        "No {} bundle is published for {platform}",
                        kind.name()
                    ));
                }
                let mut path = None;
                if installed {
                    match plan.installed.path().to_str() {
                        Some(installed) => path = Some(installed.to_owned()),
                        None => {
                            let unrepresentable = "the installed path is not valid UTF-8";
                            reason = Some(match reason.take() {
                                Some(reason) => format!("{reason}; {unrepresentable}"),
                                None => unrepresentable.to_owned(),
                            });
                        }
                    }
                }
                RuntimeEntry {
                    name: kind.name(),
                    source: plan.source,
                    auto: plan.auto,
                    offered: plan.offered,
                    version,
                    path,
                    status,
                    reason,
                }
            })
            .collect()
    }

    /// What the component `name` is, in the vocabulary [`Self::list`] renders,
    /// so a reader cannot disagree with `runtime.list`; `None` for a name this
    /// build has no component of. Read at each use, so an install reaches the
    /// next call without a reload.
    pub fn component(&self, name: &str) -> Option<ComponentState> {
        let kind = Kind::parse(name).filter(|kind| !kind.is_program())?;
        let RuntimeEntry {
            status,
            path,
            version,
            reason,
            ..
        } = self
            .list()
            .into_iter()
            .find(|entry| entry.name == kind.name())?;
        Some(match status {
            "acquiring" => ComponentState::Acquiring,
            "failed" => ComponentState::Failed(
                reason.unwrap_or_else(|| "the acquisition failed".to_owned()),
            ),
            "ready" => match (path, version) {
                (Some(root), Some(version)) => ComponentState::Ready {
                    root: PathBuf::from(root),
                    version,
                },
                (_, _) => ComponentState::Failed(
                    reason.unwrap_or_else(|| "the installed bundle has no path".to_owned()),
                ),
            },
            _ => ComponentState::Off,
        })
    }

    /// The downloaded runtimes installed under the plan in effect (§2.4), node
    /// first: the directories HostPlatform appends to a started process's
    /// `PATH` and grants a confined one read-only. A `system` interpreter is
    /// the user's own and contributes nothing; the user's `PATH` already
    /// reaches it. The OCR bundle is a component, not a program, so no
    /// directory of it ever joins a process's `PATH`.
    pub fn toolchains(&self) -> Vec<InstalledToolchain> {
        let records = read_records(&self.runtimes.lock().unwrap().record_file());
        let plans = self.plans.lock().unwrap().clone();
        Kind::ALL
            .into_iter()
            .filter(|kind| kind.is_program())
            .filter_map(|kind| {
                installed_toolchain(kind, &plans[Self::index(kind)], records.get(kind.name()))
            })
            .collect()
    }

    /// The status of `kind`'s connection; `starting` while its row has not
    /// started. A tool or component launches no runtime, so it has no row and
    /// no status of its own.
    fn status(&self, kind: Kind) -> RuntimeState {
        let row = self.local.lock().unwrap()[Self::index(kind)]
            .as_ref()
            .map(|runtime| runtime.row);
        row.map_or(RuntimeState::Starting, |row| {
            self.ctx
                .get_at(&RuntimeStatus::key(row))
                .map_or(RuntimeState::Starting, |status| status.state())
        })
    }

    /// Provides `kind`'s launch command; a tool that launches no runtime has
    /// none, so an acquisition leaves it installed.
    fn provide(&self, kind: Kind) -> State {
        if !kind.launches_runtime() {
            return State::Installed;
        }
        let runtime = self.local.lock().unwrap()[Self::index(kind)]
            .clone()
            .expect("a launching kind has a launch command");
        match self
            .ctx
            .provide_at(runtime.command.clone(), runtime.launch.clone())
        {
            Ok(provided) => State::Provided(Some(provided)),
            Err(error) => State::Failed(error.to_string()),
        }
    }

    /// Turns `kind` on and applies the configuration in effect. It reads
    /// `<home>/runtimes/config.json` again (§2.2) and makes what it gives that
    /// runtime's plan: when the `source` differs from the one it is on, it
    /// withdraws the launch command by disposing it, then disposes the running
    /// connection and acquires under the new source. A tool or component has no
    /// launch command to withdraw; its changed source only acquires again. Only
    /// that runtime's plan changes; the other keeps reporting what it is
    /// running. While an acquisition is in flight the call changes nothing, so
    /// one acquisition never sees two plans: a change made meanwhile applies on
    /// the next call. A configuration that does not read fails here and leaves
    /// the runtime as it was, and so does a component this platform has no
    /// bundle for.
    pub fn install(self: &Arc<Self>, kind: Kind) -> Result<(), String> {
        let fresh = self.runtimes.lock().unwrap().reload()?.plan(kind).clone();
        if !fresh.offered {
            return Err(format!(
                "No {} bundle is published for {}",
                kind.name(),
                self.runtimes.lock().unwrap().platform
            ));
        }
        let index = Self::index(kind);
        // The decision, the plan it decides on and the command that plan starts
        // are one critical section: releasing the state lock in between would
        // let a second call commit its own plan and command after this one
        // reserved the transition, and the transition would be lost.
        let step = {
            let mut states = self.states.lock().unwrap();
            let plan = self.plans.lock().unwrap()[index].clone();
            let changed = plan.source != fresh.source;
            // The same predicate `runtime.list` reports `ready` and `path`
            // with, so the two cannot disagree about whether `kind` is still
            // installed.
            let records = read_records(&self.runtimes.lock().unwrap().record_file());
            let version = match plan.source {
                Source::Download => Some(plan.version.clone()),
                Source::System => self.versions.lock().unwrap()[index].clone(),
            };
            let installed = version.as_deref().is_some_and(|version| {
                is_installed(kind, &plan, version, records.get(kind.name()))
            });
            let step = match &mut states[index] {
                State::Acquiring | State::Provided(None) => Step::Busy,
                State::Provided(provided) => {
                    let down = matches!(self.status(kind), RuntimeState::Down(_));
                    if changed {
                        Step::Restart(provided.take(), true)
                    } else if down {
                        Step::Restart(provided.take(), false)
                    } else {
                        Step::Keep
                    }
                }
                State::Off | State::Failed(_) => {
                    states[index] = State::Acquiring;
                    Step::Acquire
                }
                // A tool has no launch command to withdraw: a changed source
                // or a record that no longer holds (its executable is gone,
                // say) acquires again, and otherwise the call leaves it
                // installed.
                State::Installed => {
                    if changed || !installed {
                        Step::Restart(None, true)
                    } else {
                        Step::Keep
                    }
                }
            };
            if !matches!(step, Step::Busy) {
                let mut plans = self.plans.lock().unwrap();
                plans[index] = fresh;
                let runtimes = self.runtimes.lock().unwrap().clone();
                self.local.lock().unwrap()[index] = launch(kind, &plans[index], &runtimes);
            }
            step
        };
        if matches!(step, Step::Busy) {
            return Ok(());
        }
        let service = self.clone();
        match step {
            Step::Busy | Step::Keep => Ok(()),
            Step::Acquire => {
                let task = tokio::spawn(async move {
                    let state = service
                        .acquired(kind)
                        .await
                        .unwrap_or_else(|| State::Acquiring);
                    service.states.lock().unwrap()[index] = state;
                });
                self.tasks.lock().unwrap().push(task);
                Ok(())
            }
            Step::Restart(disposed, acquire) => {
                let task = tokio::spawn(async move {
                    if let Some(disposed) = disposed
                        && let Err(error) = disposed.dispose().await
                    {
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
                    service.states.lock().unwrap()[index] = state;
                });
                self.tasks.lock().unwrap().push(task);
                Ok(())
            }
        }
    }

    /// Writes `kind`'s `source` and `auto` into the user's configuration
    /// (§2.2) and applies it, as `runtime.install` does. The file keeps the
    /// merge over the preset: only the fields given change. A file it cannot
    /// read or write is the caller's error.
    pub fn configure(
        self: &Arc<Self>,
        kind: Kind,
        source: Option<Source>,
        auto: Option<bool>,
    ) -> Result<(), String> {
        if source.is_none() && auto.is_none() {
            return Ok(());
        }
        // A component is only ever downloaded, and only by `runtime.install`
        // (§2.2). A write that says otherwise would leave a file the next
        // load refuses, so the refusal comes before the file is touched.
        if !kind.is_program() {
            if source == Some(Source::System) {
                return Err(format!(
                    "{} is a component: it is only ever downloaded, not taken from the system",
                    kind.name()
                ));
            }
            if auto == Some(true) {
                return Err(format!(
                    "{} is a component: it is installed only by `runtime.install`",
                    kind.name()
                ));
            }
        }
        let _guard = self.config.lock().unwrap();
        let index = Self::index(kind);
        if matches!(
            self.states.lock().unwrap()[index],
            State::Acquiring | State::Provided(None)
        ) {
            return Err(format!(
                "{} is being acquired; change its configuration when that settles",
                kind.name()
            ));
        }
        let path = self.runtimes.lock().unwrap().config_file();
        if let Some(directory) = path.parent() {
            std::fs::create_dir_all(directory)
                .map_err(|e| format!("{}: {e}", directory.display()))?;
        }
        let mut user: Choices = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| format!("{}: {error}", path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Choices::new(),
            Err(error) => return Err(format!("{}: {error}", path.display())),
        };
        let choice = user.entry(kind.name().to_owned()).or_default();
        if source.is_some() {
            choice.source = source;
        }
        if auto.is_some() {
            choice.auto = auto;
        }
        let bytes = serde_json::to_vec_pretty(&user).map_err(|e| e.to_string())?;
        let partial = sibling(&path, &format!(".partial-{}", std::process::id()));
        std::fs::write(&partial, bytes)
            .and_then(|()| std::fs::rename(&partial, &path))
            .map_err(|e| format!("{}: {e}", path.display()))?;
        self.install(kind)
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
        let plan = self.plans.lock().unwrap()[Self::index(kind)].clone();
        let version = match plan.source {
            Source::Download => plan.version.clone(),
            Source::System => {
                let program = plan.installed.program().ok_or_else(|| {
                    format!("{} is a component and takes no system program", kind.name())
                })?;
                check_system(kind, program).await?
            }
        };
        let marker = (kind == Kind::Python)
            .then(|| externally_managed(&plan))
            .flatten();
        let installed = is_installed(
            kind,
            &plan,
            &version,
            read_records(&runtimes.record_file()).get(kind.name()),
        );
        if installed {
            // A downloaded Python installed before the marker existed gets it
            // at the next start (§7).
            if let Some(marker) = &marker
                && let Err(error) = create_externally_managed(marker)
            {
                tracing::warn!(runtime = kind.name(), %error, "externally-managed marker not written");
            }
            return Ok(version);
        }
        std::fs::create_dir_all(&runtimes.root).map_err(|e| e.to_string())?;
        let sha256 = match &plan.archive {
            Some((archive, directory)) => {
                let client = client()?;
                // The pin's own digest names this platform for a component Dim
                // publishes, so there is no checksum file to fetch.
                let sha256 = match &archive.sha256 {
                    Some(sha256) => sha256.clone(),
                    None => checksum(&client, archive).await?,
                };
                // A tool or component is present only while what the plan
                // installs is: a directory left without it is replaced, so
                // `install` can repair a damaged download. A runtime keeps its
                // directory until the pinned version changes (§2.4).
                let present = if kind.launches_runtime() {
                    directory.is_dir()
                } else {
                    plan.installed.present(false)
                };
                if !present {
                    if directory.is_dir() {
                        std::fs::remove_dir_all(directory)
                            .map_err(|e| format!("{}: {e}", directory.display()))?;
                    }
                    let relative = marker
                        .as_deref()
                        .and_then(|marker| marker.strip_prefix(directory).ok())
                        .map(Path::to_path_buf);
                    download(
                        &client,
                        &plan.sources,
                        &archive.file,
                        &sha256,
                        directory,
                        relative,
                    )
                    .await?;
                }
                Some(sha256)
            }
            None => None,
        };
        let record = Record::of(&plan, &version, sha256);
        if let Some(packages) = &plan.packages
            && !packages.is_dir()
        {
            match kind {
                Kind::Node => npm_ci(&plan, packages).await?,
                Kind::Python => pip_install(&plan, packages).await?,
                Kind::Uv | Kind::Ocr => unreachable!("a tool and a component have no packages"),
            }
        }
        // A Python directory that was already unpacked when this started gets
        // the marker here, before the record marks it installed.
        if let Some(marker) = &marker
            && let Err(error) = create_externally_managed(marker)
        {
            tracing::warn!(runtime = kind.name(), %error, "externally-managed marker not written");
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
/// configuration. A tool that launches no runtime has none.
fn launch(kind: Kind, plan: &Plan, runtimes: &Runtimes) -> Option<ForeignRuntime> {
    if !kind.launches_runtime() {
        return None;
    }
    let program = plan.program().expect("a launching kind is a program");
    Some(match kind {
        Kind::Node => ForeignRuntime::node(program, &runtimes.js_runtime(), runtimes.anchor()),
        Kind::Python => ForeignRuntime::python(
            program,
            plan.packages.as_deref(),
            runtimes.home().join("plugins").join(".python"),
        ),
        Kind::Uv | Kind::Ocr => unreachable!("a tool and a component launch no runtime"),
    })
}

/// The launch command of a launching kind, from the two the App Server built
/// at startup. A tool or component that launches no runtime has none.
fn launch_command(
    kind: Kind,
    node: &ForeignRuntime,
    python: &ForeignRuntime,
) -> Option<ForeignRuntime> {
    if !kind.launches_runtime() {
        return None;
    }
    Some(match kind {
        Kind::Node => node.clone(),
        Kind::Python => python.clone(),
        Kind::Uv | Kind::Ocr => unreachable!("a tool and a component launch no runtime"),
    })
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
    let url = archive
        .checksums
        .as_deref()
        .ok_or_else(|| format!("{} has no checksum file to read", archive.file))?;
    let text = client
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| format!("{url}: {e}"))?
        .text()
        .await
        .map_err(|e| format!("{url}: {e}"))?;
    text.lines()
        .filter_map(|line| line.split_once(char::is_whitespace))
        .find(|(_, name)| name.trim().trim_start_matches('*') == archive.file)
        .map(|(digest, _)| digest.to_ascii_lowercase())
        .filter(|digest| digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| format!("{url} lists no SHA-256 for {}", archive.file))
}

async fn download(
    client: &reqwest::Client,
    sources: &[String],
    file: &str,
    sha256: &str,
    directory: &Path,
    // The PEP 668 marker's path inside `directory`, written into the
    // `.partial` directory before the rename so the archive directory never
    // exists without it.
    marker: Option<PathBuf>,
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
    let unpacked = tokio::task::spawn_blocking(move || -> Result<(), String> {
        unpack(&name, &from, &to)?;
        if let Some(marker) = marker {
            create_externally_managed(&to.join(marker))?;
        }
        Ok(())
    })
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
        return unzip(archive, into);
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

/// Unpacks a zip archive. A release may wrap every entry in one top-level
/// directory (Node's `node-v…-win-x64/`) or place the executables at the root
/// (uv's `uv.exe` and `uvx.exe`); a wrapper is stripped only when every entry
/// is under the same top-level directory.
fn unzip(archive: &Path, into: &Path) -> Result<(), String> {
    let file = std::fs::File::open(archive).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    let mut names = Vec::with_capacity(zip.len());
    for index in 0..zip.len() {
        let entry = zip.by_index(index).map_err(|e| e.to_string())?;
        let raw = String::from_utf8_lossy(entry.name_raw()).replace('\\', "/");
        if entry.enclosed_name().is_none() {
            return Err(format!("refusing archive path {raw}"));
        }
        names.push(raw);
    }
    let strip = wrapped(&names);
    for (index, raw) in names.into_iter().enumerate() {
        let mut entry = zip.by_index(index).map_err(|e| e.to_string())?;
        let path = PathBuf::from(&raw);
        if entry.is_symlink() {
            return Err(format!("refusing link {}", path.display()));
        }
        if !path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
        {
            return Err(format!("refusing archive path {raw}"));
        }
        let relative: PathBuf = if strip {
            path.components().skip(1).collect()
        } else {
            path
        };
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

/// Whether every zip entry sits under the same top-level directory, which is
/// the release's wrapper rather than content. A flat archive, whose entries
/// share no directory, keeps its paths.
fn wrapped(names: &[String]) -> bool {
    let mut top: Option<std::ffi::OsString> = None;
    let mut nested = false;
    for name in names {
        let mut components = Path::new(name).components();
        let Some(Component::Normal(first)) = components.next() else {
            return false;
        };
        let first = first.to_os_string();
        if top.as_ref().is_some_and(|known| known != &first) {
            return false;
        }
        if top.is_none() {
            top = Some(first);
        }
        nested |= components.next().is_some();
    }
    top.is_some() && nested
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
        Kind::Uv => {
            command.arg("--version");
            ("0.11", |major, minor| {
                major > 0 || (major == 0 && minor >= 11)
            })
        }
        Kind::Ocr => unreachable!("a component is only ever downloaded"),
    };
    if !interpreter.is_file() {
        return Err(format!("no {} interpreter on PATH", kind.name()));
    }
    let what = format!("{} {}", kind.name(), interpreter.display());
    let output = run(command, &what).await?;
    let version = match kind {
        // `uv --version` prints `uv <version> (<commit> <date>)`.
        Kind::Uv => output
            .split_whitespace()
            .nth(1)
            .unwrap_or_default()
            .trim_start_matches('v')
            .to_owned(),
        Kind::Node | Kind::Python => output.trim().trim_start_matches('v').to_owned(),
        Kind::Ocr => unreachable!("a component is only ever downloaded"),
    };
    let mut parts = version.split('.').map(|part| part.parse::<u32>().ok());
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
    if kind == Kind::Uv {
        let uvx = uvx(interpreter);
        if !uvx.is_file() {
            return Err(format!("{what} has no uvx at {}", uvx.display()));
        }
    }
    Ok(version)
}

/// `uvx` beside `uv`: the tool runs `uvx <server>` itself.
fn uvx(uv: &Path) -> PathBuf {
    uv.with_file_name(if cfg!(windows) { "uvx.exe" } else { "uvx" })
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

async fn npm_ci(plan: &Plan, packages: &Path) -> Result<(), String> {
    let partial = sibling(packages, ".partial");
    let _ = std::fs::remove_dir_all(&partial);
    std::fs::create_dir_all(&partial).map_err(|e| e.to_string())?;
    std::fs::write(partial.join("package.json"), NODE_PACKAGE)
        .and_then(|()| std::fs::write(partial.join("package-lock.json"), NODE_LOCK))
        .map_err(|e| e.to_string())?;
    let cache = sibling(packages, ".npm-cache");
    let mut errors = Vec::new();
    for registry in &plan.indexes {
        let node = plan.program().expect("Node is a program");
        let mut command = tokio::process::Command::new(node);
        command
            .arg(npm_cli(node))
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
    std::fs::rename(&partial, packages).map_err(|e| e.to_string())
}

async fn pip_install(plan: &Plan, packages: &Path) -> Result<(), String> {
    let partial = sibling(packages, ".partial");
    let requirements = sibling(packages, ".requirements.txt");
    let _ = std::fs::remove_dir_all(&partial);
    std::fs::write(&requirements, PYTHON_REQUIREMENTS).map_err(|e| e.to_string())?;
    let mut errors = Vec::new();
    for index in &plan.indexes {
        let python = plan.program().expect("Python is a program");
        let mut command = tokio::process::Command::new(python);
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
    std::fs::rename(&partial, packages).map_err(|e| e.to_string())
}

/// The port HostPlatform reads: RuntimeService is resolved at each call
/// through an optional dependency, so RuntimeManager's reload neither reloads
/// HostPlatform nor leaves it holding a withdrawn service.
struct RuntimeToolchain {
    ctx: Ctx,
    service: Key<RuntimeService>,
}
impl ToolchainSource for RuntimeToolchain {
    fn toolchains(&self) -> Vec<InstalledToolchain> {
        self.ctx
            .get_at(&self.service)
            .map(|service| service.toolchains())
            .unwrap_or_default()
    }
}

/// The process row `ToolchainSource`: provides the port that carries the
/// installed runtimes' bin directories to HostPlatform. It reads
/// RuntimeService optionally, so it exists on hosts without RuntimeManager.
pub struct ToolchainSourcePlugin {
    service: Key<RuntimeService>,
}
impl ToolchainSourcePlugin {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            service: names.key()?,
        })
    }
}
impl DynPlugin for ToolchainSourcePlugin {
    fn name(&self) -> &str {
        "ToolchainSource"
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let port = RuntimeToolchain {
                ctx: ctx.clone(),
                service: self.service.clone(),
            };
            ctx.provide_as::<ToolchainSourceHost>(Arc::new(ToolchainSourceHost(Arc::new(port))))?;
            Ok(Effect::Done)
        })
    }
}

/// The port a component's owner reads: RuntimeService is resolved at each call
/// through an optional dependency, so RuntimeManager's reload neither reloads
/// the reader nor leaves it holding a withdrawn service.
struct RuntimeComponent {
    ctx: Ctx,
    service: Key<RuntimeService>,
}
impl ComponentSource for RuntimeComponent {
    fn component(&self, name: &str) -> Option<ComponentState> {
        self.ctx
            .get_at(&self.service)
            .and_then(|service| service.component(name))
    }
}

/// The process row `ComponentSource`: provides the port that answers what
/// RuntimeManager installed, with the states `runtime.list` renders. It reads
/// RuntimeService optionally, so it exists on hosts without RuntimeManager,
/// where every component is absent.
pub struct ComponentSourcePlugin {
    service: Key<RuntimeService>,
}
impl ComponentSourcePlugin {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            service: names.key()?,
        })
    }
}
impl DynPlugin for ComponentSourcePlugin {
    fn name(&self) -> &str {
        "ComponentSource"
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let port = RuntimeComponent {
                ctx: ctx.clone(),
                service: self.service.clone(),
            };
            ctx.provide_as::<ComponentSourceHost>(Arc::new(ComponentSourceHost(Arc::new(port))))?;
            Ok(Effect::Done)
        })
    }
}

/// The process row `RuntimeManager`.
pub struct RuntimeManagerPlugin {
    methods: Key<ManagementMethods>,
    service: Key<RuntimeService>,
    runtimes: Arc<Runtimes>,
    /// One per kind in `Kind::ALL` order: a launch command for a runtime,
    /// `None` for a tool or component.
    local: [Option<ForeignRuntime>; Kind::ALL.len()],
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
            local: Kind::ALL.map(|kind| launch_command(kind, &node, &python)),
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
                plans: Mutex::new(Kind::ALL.map(|kind| self.runtimes.plan(kind).clone())),
                config: Mutex::new(()),
                local: Mutex::new(self.local.clone()),
                states: Mutex::new(Kind::ALL.map(|_| State::Off)),
                versions: Mutex::new(Kind::ALL.map(|_| None)),
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
                    MethodDecl::new("runtime.configure", true, Interruption::Report),
                ],
                Arc::new(move |call, _stopping, _| {
                    let MethodCall {
                        method,
                        params: value,
                        ..
                    } = call;
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
                            "runtime.configure" => {
                                #[derive(Deserialize)]
                                struct Configure {
                                    name: String,
                                    #[serde(default)]
                                    source: Option<Source>,
                                    #[serde(default)]
                                    auto: Option<bool>,
                                }
                                let Configure { name, source, auto } = params(value)?;
                                let kind = Kind::parse(&name).ok_or_else(|| {
                                    RpcError::invalid(format!("unknown runtime {name}"))
                                })?;
                                service.configure(kind, source, auto).map_err(|error| {
                                    RpcError::app(-32000, "runtime_configuration", error)
                                })?;
                                reply(service.list().into_iter().find(|s| s.name == kind.name()))
                            }
                            _ => unreachable!("registered runtime management method"),
                        }
                    })
                }),
            )?;
            // A start installs the programs that are on. A component is
            // installed only by explicit `runtime.install`: its bundle is large
            // and nothing can read it until its owner's verifier is there, so a
            // start neither downloads it nor repairs one the user removed.
            for kind in Kind::ALL.into_iter().filter(|kind| kind.is_program()) {
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
    use super::{
        Archive, Installed, Kind, PINS, Pins, Plan, Record, Source, checksum, client,
        create_externally_managed, download, externally_managed, hex, installed_toolchain, unpack,
    };
    use sha2::{Digest, Sha256};
    use std::path::{Path, PathBuf};

    #[test]
    fn every_release_platform_has_an_archive_and_an_official_checksum() {
        let pins: Pins = serde_json::from_str(PINS).unwrap();
        assert!(
            pins.node
                .checksums
                .as_deref()
                .is_some_and(|checksums| checksums.starts_with("https://nodejs.org/"))
        );
        assert!(pins.python.checksums.as_deref().is_some_and(|checksums| {
            checksums.starts_with("https://github.com/astral-sh/python-build-standalone/")
        }));
        assert!(pins.uv.checksums.as_deref().is_some_and(|checksums| {
            checksums.starts_with("https://github.com/astral-sh/uv/releases/")
        }));
        for platform in [
            "darwin-arm64",
            "darwin-x64",
            "linux-arm64",
            "linux-x64",
            "windows-arm64",
            "windows-x64",
        ] {
            for pin in [&pins.node, &pins.python, &pins.uv] {
                assert!(pin.files.contains_key(platform), "{platform}");
            }
        }
        // The component: Dim publishes it, so this file carries its own digest
        // per target and there is no checksum file to fetch. Intel Macs have
        // had no ONNX Runtime build since 1.23.2, so no bundle is published
        // for them.
        assert!(pins.ocr.checksums.is_none());
        let digests = pins.ocr.sha256.as_ref().expect("the bundle pin's digests");
        for platform in [
            "darwin-arm64",
            "linux-arm64",
            "linux-x64",
            "windows-arm64",
            "windows-x64",
        ] {
            assert!(pins.ocr.files.contains_key(platform), "{platform}");
            let digest = digests
                .get(platform)
                .unwrap_or_else(|| panic!("{platform}"));
            assert_eq!(digest.len(), 64, "{platform}: {digest}");
        }
        assert!(
            !pins.ocr.files.contains_key("darwin-x64"),
            "no ONNX Runtime build for Intel Macs"
        );
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
            checksums: Some(format!("http://{address}/SUMS")),
            sha256: None,
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
            None,
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

    /// The uv Windows archive holds `uv.exe` and `uvx.exe` at its root, while
    /// the Node one wraps its files in a directory; both unpack to the same
    /// layout, the executables in the archive directory.
    #[test]
    fn a_zip_without_a_wrapper_keeps_its_entries_at_the_root() {
        use std::io::Write;
        use zip::write::SimpleFileOptions;
        let root = scratch("flat");
        let file = root.join("uv.zip");
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&file).unwrap());
        for name in ["uv.exe", "uvx.exe", "uvw.exe"] {
            writer
                .start_file(name, SimpleFileOptions::default())
                .unwrap();
            writer.write_all(b"ok").unwrap();
        }
        writer.finish().unwrap();
        unpack("uv.zip", &file, &root.join("out")).unwrap();
        for name in ["uv.exe", "uvx.exe", "uvw.exe"] {
            assert_eq!(
                std::fs::read(root.join("out").join(name)).unwrap(),
                b"ok",
                "{name}"
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A fresh acquisition writes PEP 668's marker into the `.partial`
    /// directory, so the archive directory never exists without it (§7).
    #[tokio::test]
    async fn an_acquired_python_carries_the_marker_before_its_directory_is_renamed() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let root = scratch("marker-download");
        let source = root.join("python.tar.gz");
        archive(&source, |builder| {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Regular);
            header.set_path("python/lib/python3.12/os.py").unwrap();
            header.set_mode(0o644);
            header.set_size(2);
            header.set_cksum();
            builder.append(&header, &b"ok"[..]).unwrap();
        });
        let bytes = std::fs::read(&source).unwrap();
        let published = hex(&Sha256::digest(&bytes));
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0u8; 1024];
                let _ = stream.read(&mut request).await.unwrap_or(0);
                let _ = stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                            bytes.len()
                        )
                        .as_bytes(),
                    )
                    .await;
                let _ = stream.write_all(&bytes).await;
            }
        });
        let directory = root.join("python-3.12-test");
        download(
            &client().unwrap(),
            &[format!("http://{address}/{{file}}")],
            "python.tar.gz",
            &published,
            &directory,
            Some(PathBuf::from("lib/python3.12/EXTERNALLY-MANAGED")),
        )
        .await
        .unwrap();
        let marker = directory.join("lib/python3.12/EXTERNALLY-MANAGED");
        let text = std::fs::read_to_string(&marker).unwrap();
        assert!(
            text.starts_with("[externally-managed]") && text.contains("Error="),
            "{text}"
        );
        assert!(!root.join("python-3.12-test.partial").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A directory installed before the marker existed gets it at the next
    /// start, and an existing marker is left as it is.
    #[test]
    fn an_installed_directory_gets_the_marker_only_when_it_is_missing() {
        let root = scratch("marker-start");
        let directory = root.join("python-3.12-test");
        let marker = directory.join("lib/python3.12/EXTERNALLY-MANAGED");
        create_externally_managed(&marker).unwrap();
        assert!(
            std::fs::read_to_string(&marker)
                .unwrap()
                .starts_with("[externally-managed]")
        );
        std::fs::write(&marker, "mine").unwrap();
        create_externally_managed(&marker).unwrap();
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "mine");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// The marker path follows the archive's layout: the interpreter is at
    /// `bin/python3`, so `sysconfig`'s stdlib is the sibling `lib/python3.12`.
    #[cfg(unix)]
    #[test]
    fn the_marker_path_is_the_stdlib_directory_of_the_archive_layout() {
        let root = PathBuf::from("/scratch");
        let plan = Plan {
            source: Source::Download,
            auto: false,
            version: "3.12.15+20261003".into(),
            installed: Installed::Program(root.join("python-3.12.15-test/bin/python3")),
            offered: true,
            archive: Some((
                Archive {
                    file: "python.tar.gz".into(),
                    checksums: Some("https://example.invalid/SHA256SUMS".into()),
                    sha256: None,
                },
                root.join("python-3.12.15-test"),
            )),
            sources: vec![],
            packages: Some(root.join("python-packages-test")),
            indexes: vec![],
        };
        assert_eq!(
            externally_managed(&plan),
            Some(root.join("python-3.12.15-test/lib/python3.12/EXTERNALLY-MANAGED"))
        );
    }

    /// Only a downloaded, installed runtime is a toolchain: its `bin` is the
    /// interpreter's directory and its `root` is the archive directory. A
    /// `system` interpreter is the user's own and contributes nothing.
    #[test]
    fn only_a_downloaded_runtime_is_a_toolchain() {
        use dim_agent_capabilities::toolchain::InstalledToolchain;
        let root = scratch("toolchain");
        let archive = root.join("python-3.12.15-test");
        std::fs::create_dir_all(archive.join("bin")).unwrap();
        let packages = root.join("python-packages-test");
        std::fs::create_dir_all(&packages).unwrap();
        let plan = Plan {
            source: Source::Download,
            auto: false,
            version: "3.12.15+20261003".into(),
            installed: Installed::Program(archive.join("bin/python3")),
            offered: true,
            archive: Some((
                Archive {
                    file: "python.tar.gz".into(),
                    checksums: Some("https://example.invalid/SHA256SUMS".into()),
                    sha256: None,
                },
                archive.clone(),
            )),
            sources: vec![],
            packages: Some(packages),
            indexes: vec![],
        };
        let record = Record::of(&plan, &plan.version, None);
        assert_eq!(
            installed_toolchain(Kind::Python, &plan, Some(&record)),
            Some(InstalledToolchain {
                bin: archive.join("bin"),
                root: archive.clone(),
            })
        );
        assert_eq!(
            installed_toolchain(Kind::Python, &plan, None),
            None,
            "a runtime the record does not mark installed is not a toolchain"
        );
        let system = Plan {
            source: Source::System,
            archive: None,
            installed: Installed::Program(root.join("no-system-python")),
            ..plan.clone()
        };
        let record = Record::of(&system, "3.12.9", None);
        assert_eq!(
            installed_toolchain(Kind::Python, &system, Some(&record)),
            None,
            "a system interpreter contributes nothing"
        );
        // A tool's executable is part of "installed": the archive directory
        // that remains without it is not a toolchain.
        let tool = Plan {
            source: Source::Download,
            auto: false,
            version: "0.13.7".into(),
            installed: Installed::Program(archive.join("uv")),
            packages: None,
            ..plan.clone()
        };
        let record = Record::of(&tool, &tool.version, None);
        assert_eq!(
            installed_toolchain(Kind::Uv, &tool, Some(&record)),
            None,
            "a tool whose executable is gone is not installed"
        );
        std::fs::write(tool.program().expect("uv is a program"), b"ok").unwrap();
        assert_eq!(
            installed_toolchain(Kind::Uv, &tool, Some(&record)),
            Some(InstalledToolchain {
                bin: archive.clone(),
                root: archive.clone(),
            })
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
