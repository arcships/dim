//! The data PluginManager reads at its boundary: `plugins.json` (its own
//! record), the default configuration file and plugin manifests
//! (PluginManager §2).
use super::PluginError;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
};

/// The manifest file at a plugin directory's root.
pub const MANIFEST: &str = "dim-plugin.json";
/// The record version that holds an `{optional}` source or an installed
/// version (§2.2): what this binary writes for such content, and the newest
/// version it reads.
const RECORD_VERSION: u32 = 3;
/// The version written for content without an optional source: what a
/// binary from before Phase 2 reads.
const PLAIN_VERSION: u32 = 2;

/// `<home>/plugins.json` (§2.2): configuration only.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub version: u32,
    #[serde(default)]
    pub installed: Vec<Installed>,
    #[serde(default)]
    pub names: BTreeMap<String, Choice>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Installed {
    pub id: String,
    pub name: String,
    pub source: Source,
    pub dir: PathBuf,
    pub kinds: Vec<Kind>,
    /// The installed version of an `{optional}` source, from its manifest;
    /// absent for other sources (§2.2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(untagged, deny_unknown_fields)]
pub enum Source {
    Path {
        path: PathBuf,
    },
    Git {
        git: String,
        #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
        reference: Option<String>,
        commit: String,
    },
    /// A plugin the default configuration file ships optionally (§2.3);
    /// `optional` is its name there.
    Optional {
        optional: String,
    },
}

/// The user's decision about one name; absent fields are the shipped state.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Choice {
    #[serde(rename = "use", default, skip_serializing_if = "Option::is_none")]
    pub use_: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub off: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Code,
    Skills,
    Mcp,
    Hooks,
    Ui,
}

impl Record {
    /// Reads `path`; a missing file is the empty record. A file that does not
    /// parse, or has another version, is `record_unreadable` with its path.
    /// Reading accepts versions 1, 2 and 3 (§2.2).
    pub fn read(path: &Path) -> Result<Self, PluginError> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    version: PLAIN_VERSION,
                    ..Self::default()
                });
            }
            Err(error) => return Err(unreadable(path, error)),
        };
        let mut record: Self =
            serde_json::from_slice(&bytes).map_err(|error| unreadable(path, error))?;
        if !(1..=RECORD_VERSION).contains(&record.version) {
            return Err(unreadable(
                path,
                format!("unknown version {}", record.version),
            ));
        }
        if record.version == 1 {
            if record
                .installed
                .iter()
                .any(|entry| entry.kinds.contains(&Kind::Ui))
            {
                return Err(unreadable(path, "UI content requires record version 2"));
            }
            record.version = PLAIN_VERSION;
        }
        if record.version < RECORD_VERSION && record.needs_version_3() {
            return Err(unreadable(
                path,
                "optional sources require record version 3",
            ));
        }
        Ok(record)
    }

    /// Whether the record holds content only a version 3 record may carry:
    /// an `{optional}` source or an installed version (§2.2).
    fn needs_version_3(&self) -> bool {
        self.installed
            .iter()
            .any(|entry| matches!(entry.source, Source::Optional { .. }) || entry.version.is_some())
    }

    /// Writes atomically: a temporary file in the same directory, synced
    /// through its own writable handle, renamed over `path` (§2.2). The
    /// version written follows the content: 3 exactly when the record holds
    /// an `{optional}` entry, and 2 otherwise, so removing the last one
    /// writes a record older binaries read.
    pub fn write(&mut self, path: &Path) -> std::io::Result<()> {
        self.version = if self.needs_version_3() {
            RECORD_VERSION
        } else {
            PLAIN_VERSION
        };
        let existing = match std::fs::read(path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        if let Some(bytes) = existing {
            let current: Self = serde_json::from_slice(&bytes).map_err(std::io::Error::other)?;
            if !(1..=RECORD_VERSION).contains(&current.version) {
                return Err(std::io::Error::other("unknown plugin record version"));
            }
            // §2.2: the version 1 backup is written once, because a record
            // never returns to version 1. The version 2 backup is replaced
            // before every version 3 write over a version 2 file, because
            // the record does return to version 2 when the last optional
            // entry is removed: the backup then always holds the last
            // version 2 record, the one a rollback wants.
            match current.version {
                1 => {
                    let backup = path.with_extension("json.v1.backup");
                    match std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&backup)
                    {
                        Ok(mut file) => {
                            if let Err(error) = file.write_all(&bytes).and_then(|_| file.sync_all())
                            {
                                drop(file);
                                let _ = std::fs::remove_file(&backup);
                                return Err(error);
                            }
                        }
                        Err(error)
                            if error.kind() == std::io::ErrorKind::AlreadyExists
                                && std::fs::read(&backup)? == bytes => {}
                        Err(error) => return Err(error),
                    }
                }
                PLAIN_VERSION if self.version == RECORD_VERSION => {
                    write_atomic(&path.with_extension("json.v2.backup"), &bytes)?;
                }
                _ => {}
            }
        }
        let mut text = serde_json::to_vec_pretty(self).expect("record serializes");
        text.push(b'\n');
        write_atomic(path, &text)
    }

    pub fn installed(&self, id: &str) -> Option<&Installed> {
        self.installed.iter().find(|entry| entry.id == id)
    }
}

/// Writes `bytes` to `path` through a temporary file in the same directory,
/// synced on its own handle and renamed over `path`, so a failure leaves the
/// file as it was (§2.2).
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let directory = path.parent().expect("a record file has a directory");
    let name = path
        .file_name()
        .expect("a record file has a name")
        .to_string_lossy();
    let temporary = directory.join(format!(".{name}.{}", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn unreadable(path: &Path, error: impl std::fmt::Display) -> PluginError {
    PluginError::new("record_unreadable", format!("{}: {error}", path.display()))
}

/// The default configuration file (§2.3), shipped read-only.
#[derive(Clone, Debug, Default)]
pub struct Defaults {
    /// Each default plugin's name and directory, resolved against the file.
    pub plugins: BTreeMap<String, PathBuf>,
    /// Each optional plugin's name and directory, resolved against the file
    /// and not canonicalized at startup: a missing one must never fail
    /// startup, so it is resolved when used (§2.3, §3.2).
    pub optional: BTreeMap<String, PathBuf>,
    pub locked: Vec<String>,
    /// RuntimeManager's preset (RuntimeManager §2.2).
    pub runtimes: crate::runtime_manager::Choices,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DefaultsFile {
    #[serde(default)]
    plugins: BTreeMap<String, PathBuf>,
    #[serde(default)]
    optional: BTreeMap<String, PathBuf>,
    #[serde(default)]
    locked: Vec<String>,
    #[serde(default)]
    runtimes: crate::runtime_manager::Choices,
}

impl Defaults {
    pub fn read(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
        let file: DefaultsFile = serde_json::from_slice(&bytes)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        // §2.3: a name cannot be in both maps. An optional plugin's item
        // has no candidates (§3.6), so it would shadow the default plugin
        // of the same name.
        for name in file.optional.keys() {
            if file.plugins.contains_key(name) {
                return Err(format!(
                    "{name} is in both \"plugins\" and \"optional\" in {}",
                    path.display()
                ));
            }
        }
        let base = path.parent().unwrap_or(Path::new("."));
        let resolve = |plugins: BTreeMap<String, PathBuf>| {
            plugins
                .into_iter()
                .map(|(name, dir)| (name, base.join(dir)))
                .collect()
        };
        Ok(Self {
            plugins: resolve(file.plugins),
            optional: resolve(file.optional),
            locked: file.locked,
            runtimes: file.runtimes,
        })
    }
}

/// A plugin manifest, read and checked at its boundary (§2.1): every path
/// it names is resolved inside the plugin directory.
#[derive(Clone, Debug, PartialEq)]
pub struct Manifest {
    pub name: String,
    /// The plugin's own version (§2.1); required for an optional plugin,
    /// where it names the install directory.
    pub version: Option<String>,
    /// The package manager whose locked dependencies the plugin declares
    /// (§2.1). The only value today is `"npm"`.
    pub dependencies: Option<String>,
    pub code: Option<Code>,
    pub skills: Option<PathBuf>,
    pub mcp: Option<PathBuf>,
    pub hooks: Option<PathBuf>,
    pub ui: Option<Value>,
}

/// The rutis row fields of a plugin's code, with `entry` resolved.
#[derive(Clone, Debug, PartialEq)]
pub struct Code {
    pub entry: PathBuf,
    pub inject: Vec<String>,
    pub provides: Map<String, Value>,
    pub instance_names: Map<String, Value>,
    pub config: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestFile {
    name: String,
    version: Option<String>,
    #[serde(default)]
    #[expect(
        dead_code,
        reason = "the manifest's `description` is accepted and not read"
    )]
    description: Option<String>,
    dependencies: Option<String>,
    code: Option<CodeFile>,
    skills: Option<String>,
    mcp: Option<String>,
    hooks: Option<String>,
    ui: Option<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CodeFile {
    entry: String,
    #[serde(default)]
    inject: Vec<String>,
    #[serde(default)]
    provides: Map<String, Value>,
    #[serde(default)]
    instance_names: Map<String, Value>,
    #[serde(default)]
    config: Option<Value>,
}

fn invalid(field: &str, reason: impl std::fmt::Display) -> PluginError {
    PluginError::new("invalid_manifest", format!("{field}: {reason}"))
}

impl Manifest {
    /// Reads `<dir>/dim-plugin.json`. `dir` is canonical.
    pub fn read(dir: &Path) -> Result<Self, PluginError> {
        let path = inside(dir, MANIFEST, MANIFEST)?;
        let bytes = std::fs::read(&path).map_err(|error| invalid(MANIFEST, error))?;
        let file: ManifestFile =
            serde_json::from_slice(&bytes).map_err(|error| invalid(MANIFEST, error))?;
        if file.name.is_empty() || file.name.chars().any(char::is_control) {
            return Err(invalid("name", "must be a non-empty name"));
        }
        let version = match file.version {
            Some(version) => {
                if !is_version(&version) {
                    return Err(invalid("version", "must be three dot-separated numbers"));
                }
                Some(version)
            }
            None => None,
        };
        if let Some(dependencies) = &file.dependencies {
            if dependencies != "npm" {
                return Err(invalid("dependencies", "must be \"npm\""));
            }
            // §2.1: a plugin declaring its dependencies ships its lock, and
            // both files stay inside the plugin directory (§7).
            for name in ["package.json", "package-lock.json"] {
                if !dir.join(name).is_file() {
                    return Err(invalid("dependencies", format!("the plugin has no {name}")));
                }
                inside(dir, name, "dependencies")?;
            }
        }
        let code = file
            .code
            .map(|code| {
                for (service, group) in &code.instance_names {
                    if !matches!(group.as_str(), Some("session" | "branch")) {
                        return Err(invalid(
                            &format!("code.instanceNames.{service}"),
                            "must be \"session\" or \"branch\"",
                        ));
                    }
                }
                let entry = inside(dir, &code.entry, "code.entry")?;
                if !entry.is_file() {
                    return Err(invalid("code.entry", "is not a file"));
                }
                Ok(Code {
                    entry,
                    inject: code.inject,
                    provides: code.provides,
                    instance_names: code.instance_names,
                    config: code.config.unwrap_or_else(|| Value::Object(Map::new())),
                })
            })
            .transpose()?;
        let path = |value: Option<String>, field| {
            value.map(|value| inside(dir, &value, field)).transpose()
        };
        Ok(Self {
            name: file.name,
            version,
            dependencies: file.dependencies,
            code,
            skills: path(file.skills, "skills")?,
            mcp: path(file.mcp, "mcp")?,
            hooks: path(file.hooks, "hooks")?,
            ui: file.ui,
        })
    }

    /// An optional plugin's manifest must name its version (§2.1): it names
    /// the install directory.
    pub fn require_version(self) -> Result<Self, PluginError> {
        if self.version.is_none() {
            return Err(invalid(
                "version",
                "an optional plugin must declare its version",
            ));
        }
        Ok(self)
    }

    pub fn kinds(&self) -> Vec<Kind> {
        [
            (self.code.is_some(), Kind::Code),
            (self.skills.is_some(), Kind::Skills),
            (self.mcp.is_some(), Kind::Mcp),
            (self.hooks.is_some(), Kind::Hooks),
            (self.ui.is_some(), Kind::Ui),
        ]
        .into_iter()
        .filter_map(|(present, kind)| present.then_some(kind))
        .collect()
    }
}

/// Three dot-separated numbers, each one or more digits (§2.1): a
/// manifest's `version`, which names an optional plugin's install
/// directory.
pub fn is_version(version: &str) -> bool {
    let parts: Vec<&str> = version.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
}

/// `relative` resolved inside the canonical `dir` after following symbolic
/// links; anything that leaves it is `invalid_manifest`.
fn inside(dir: &Path, relative: &str, field: &str) -> Result<PathBuf, PluginError> {
    let path = Path::new(relative);
    if relative.is_empty() || path.is_absolute() || path.has_root() {
        return Err(invalid(field, "must be a relative path"));
    }
    let resolved = std::fs::canonicalize(dir.join(path)).map_err(|error| invalid(field, error))?;
    if !resolved.starts_with(dir) {
        return Err(invalid(field, "leaves the plugin directory"));
    }
    Ok(resolved)
}
