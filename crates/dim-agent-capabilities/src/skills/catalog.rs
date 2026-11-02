use super::{
    CatalogEntry, CatalogVersion, CatalogView, InstallOutcome, PreviewConflict, PreviewShadow,
    PreviewSkill, RefreshTarget, Replace, SelectedSkill, SkillIssue, SkillPreview, SkillSource,
    SkillStatus, SkillsError,
    discovery::{RootKind, RootSnapshot, scan_root},
    install,
};
use crate::{
    host_platform::HostPlatform,
    settings::{SettingsChanged, settings_changes},
    skill_settings::{
        SOURCE, SettingsVersion, SkillInstallSource, SkillInstallation, SkillSettings,
        SkillSettingsError, SkillSettingsHost, SkillSettingsSnapshot,
    },
};
use dim_agent_runtime::{Names, PartRegistry};
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis::{Ctx, Error, Event};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio::sync::watch;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

const SKILLS_DIR: &str = ".agents/skills";

/// The catalog advanced to `version`: a root was rescanned or the disabled set
/// changed. It never carries the view; a listener queries the catalog.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CatalogChanged {
    pub version: CatalogVersion,
}
impl Event for CatalogChanged {
    const NAME: &'static str = "skills.catalog_changed";
    type Value = ();
    fn name(&self) -> &str {
        catalog_changes()
    }
}
/// The rutis channel of catalog changes, in version order.
pub fn catalog_changes() -> &'static str {
    "skills/catalog"
}

struct CatalogState {
    user: Option<Arc<RootSnapshot>>,
    /// Scanned plugin roots, by plugin id, with the root they were scanned at.
    plugins: BTreeMap<String, (PathBuf, Arc<RootSnapshot>)>,
    projects: HashMap<PathBuf, Arc<RootSnapshot>>,
    settings: Result<SkillSettingsSnapshot, SkillsError>,
    version: CatalogVersion,
}

/// Process-wide Skill discovery and global enablement.
pub struct SkillsCatalog {
    user_root: PathBuf,
    settings: Arc<dyn SkillSettings>,
    state: tokio::sync::Mutex<CatalogState>,
    /// Serializes settings reloads so a later read is never overwritten by
    /// an earlier one.
    reloading: tokio::sync::Mutex<()>,
    /// Serializes the scans of the roots with an installation placing or
    /// removing files, so no view is built from a half-swapped directory.
    operating: tokio::sync::Mutex<()>,
    /// The owner's context, which announces catalog changes.
    events: Ctx,
    closed: CancellationToken,
}

async fn scan(root: PathBuf, kind: RootKind) -> Result<RootSnapshot, SkillIssue> {
    let path = root.clone();
    tokio::task::spawn_blocking(move || scan_root(&path, kind))
        .await
        .unwrap_or_else(|_| {
            Err(SkillIssue::RootUnreadable {
                root,
                reason: "scan task failed".into(),
            })
        })
}

impl SkillsCatalog {
    fn check_open(&self) -> Result<(), SkillsError> {
        if self.closed.is_cancelled() {
            Err(SkillsError::Closed)
        } else {
            Ok(())
        }
    }

    /// The project root of `cwd`, unless it is the user root.
    fn project_root(&self, cwd: &Path) -> Option<PathBuf> {
        let root = cwd.join(SKILLS_DIR);
        let same = matches!(
            (root.canonicalize(), self.user_root.canonicalize()),
            (Ok(a), Ok(b)) if a == b
        );
        (!same).then_some(root)
    }

    /// A root that cannot be read on first use is recorded as an issue.
    async fn first_scan(root: PathBuf, kind: RootKind) -> Arc<RootSnapshot> {
        Arc::new(scan(root, kind).await.unwrap_or_else(|issue| RootSnapshot {
            skills: Vec::new(),
            issues: vec![issue],
        }))
    }

    pub async fn query(&self, cwd: Option<&Path>) -> Result<CatalogView, SkillsError> {
        self.check_open()?;
        let _operating = self.operating.lock().await;
        self.view(cwd).await
    }

    /// The assembled view; the caller holds the operating lock (or is the
    /// query that took it).
    async fn view(&self, cwd: Option<&Path>) -> Result<CatalogView, SkillsError> {
        if self.state.lock().await.settings.is_err() {
            self.reload_settings().await?;
        }
        let mut state = self.state.lock().await;
        if state.user.is_none() {
            state.user = Some(Self::first_scan(self.user_root.clone(), RootKind::User).await);
        }
        let registered = state
            .settings
            .as_ref()
            .map(|settings| settings.plugins.clone())
            .map_err(Clone::clone)?;
        state
            .plugins
            .retain(|plugin, (root, _)| registered.get(plugin) == Some(root));
        for (plugin, root) in registered {
            if let std::collections::btree_map::Entry::Vacant(entry) = state.plugins.entry(plugin) {
                let kind = RootKind::Plugin(entry.key().clone());
                let snapshot = Self::first_scan(root.clone(), kind).await;
                entry.insert((root, snapshot));
            }
        }
        let project_root = cwd.and_then(|cwd| self.project_root(cwd));
        if let Some(root) = &project_root
            && !state.projects.contains_key(root)
        {
            let snapshot = Self::first_scan(root.clone(), RootKind::Project).await;
            state.projects.insert(root.clone(), snapshot);
        }
        let roots =
            std::iter::once((
                SkillSource::User,
                state.user.clone().expect("user root scanned"),
            ))
            .chain(state.plugins.iter().map(|(plugin, (_, snapshot))| {
                (SkillSource::Plugin(plugin.clone()), snapshot.clone())
            }))
            .chain(
                project_root
                    .and_then(|root| state.projects.get(&root).cloned())
                    .map(|snapshot| (SkillSource::Project, snapshot)),
            );
        let mut winners: BTreeMap<String, String> = BTreeMap::new();
        let mut entries = Vec::new();
        let mut issues = Vec::new();
        let settings = state.settings.as_ref().map_err(Clone::clone)?;
        for (source, snapshot) in roots {
            issues.extend(snapshot.issues.iter().cloned());
            for skill in &snapshot.skills {
                let status = match winners.get(&skill.name) {
                    Some(by) => SkillStatus::Shadowed { by: by.clone() },
                    None => {
                        winners.insert(skill.name.clone(), skill.id.clone());
                        SkillStatus::Active
                    }
                };
                entries.push(CatalogEntry {
                    definition: skill.clone(),
                    source: source.clone(),
                    enabled: !settings.disabled.contains(&skill.id),
                    status,
                });
            }
        }
        Ok(CatalogView {
            version: state.version,
            entries,
            issues,
        })
    }

    /// Advances the version and announces it. Callers hold the state lock, so
    /// announcements follow version order.
    fn advance(&self, state: &mut CatalogState) -> CatalogVersion {
        state.version += 1;
        let changed = CatalogChanged {
            version: state.version,
        };
        if let Err(error) = self.events.emit(changed) {
            tracing::warn!(%error, "Skill catalog change was not announced");
        }
        state.version
    }

    /// Reads the settings service and stores its snapshot; a changed
    /// disabled set is a new version. Reloads run one at a time, in order.
    async fn reload_settings(&self) -> Result<(), SkillsError> {
        let _order = self.reloading.lock().await;
        let snapshot = self
            .settings
            .skills(self.closed.child_token())
            .await
            .map_err(|error| SkillsError::SettingsRead(error.to_string()));
        let mut state = self.state.lock().await;
        let changed = state.settings.as_ref().map(|settings| {
            (
                &settings.disabled,
                &settings.plugins,
                &settings.installations,
            )
        }) != snapshot.as_ref().map(|settings| {
            (
                &settings.disabled,
                &settings.plugins,
                &settings.installations,
            )
        });
        let result = snapshot.as_ref().map(|_| ()).map_err(Clone::clone);
        state.settings = snapshot;
        if changed {
            self.advance(&mut state);
        }
        result
    }

    pub async fn set_enabled(
        &self,
        id: &str,
        enabled: bool,
        cwd: Option<&Path>,
    ) -> Result<(CatalogEntry, CatalogVersion), SkillsError> {
        let view = self.query(cwd).await?;
        if !view.entries.iter().any(|entry| entry.definition.id == id) {
            return Err(SkillsError::UnknownSkill(id.into()));
        }
        let settings_error = |error: SkillSettingsError| match error {
            SkillSettingsError::Conflict => SkillsError::SettingsConflict,
            SkillSettingsError::Closed | SkillSettingsError::Cancelled => SkillsError::Closed,
            other => SkillsError::SettingsWrite(other.to_string()),
        };
        let cancel = self.closed.child_token();
        self.reload_settings().await?;
        let current = self
            .state
            .lock()
            .await
            .settings
            .as_ref()
            .map_err(Clone::clone)?
            .version
            .clone();
        self.settings
            .clone()
            .update_skills(current, id.into(), enabled, cancel)
            .await
            .map_err(settings_error)?;
        self.reload_settings()
            .await
            .map_err(|error| SkillsError::SettingsSync(error.to_string()))?;
        let view = self.query(cwd).await?;
        let entry = view
            .entries
            .into_iter()
            .find(|entry| entry.definition.id == id)
            .ok_or_else(|| SkillsError::UnknownSkill(id.into()))?;
        Ok((entry, view.version))
    }

    /// Registers `root` as the skills root of `plugin`, replacing what the
    /// plugin registered before, and scans it now. The registration is kept
    /// in the settings and restored after the catalog reloads. Returns the
    /// issues of the scan: Skills that do not parse, or an unreadable root.
    pub async fn register_plugin(
        &self,
        plugin: &str,
        root: PathBuf,
    ) -> Result<Vec<SkillIssue>, SkillsError> {
        self.check_open()?;
        self.settings
            .clone()
            .set_plugin(plugin.into(), Some(root.clone()), self.closed.child_token())
            .await
            .map_err(|error| SkillsError::SettingsWrite(error.to_string()))?;
        self.reload_settings().await?;
        let snapshot = Self::first_scan(root.clone(), RootKind::Plugin(plugin.into())).await;
        let issues = snapshot.issues.clone();
        let mut state = self.state.lock().await;
        state.plugins.insert(plugin.into(), (root, snapshot));
        self.advance(&mut state);
        Ok(issues)
    }

    /// Withdraws what `plugin` registered; nothing when it registered nothing.
    pub async fn withdraw_plugin(&self, plugin: &str) -> Result<(), SkillsError> {
        self.check_open()?;
        self.settings
            .clone()
            .set_plugin(plugin.into(), None, self.closed.child_token())
            .await
            .map_err(|error| SkillsError::SettingsWrite(error.to_string()))?;
        self.reload_settings().await
    }

    /// The plugin ids whose skills roots the catalog holds.
    pub async fn plugins(&self) -> Result<Vec<String>, SkillsError> {
        self.query(None).await?;
        Ok(self.state.lock().await.plugins.keys().cloned().collect())
    }

    /// The issues of the skills root `plugin` registered, as the catalog
    /// holds them now.
    pub async fn plugin_issues(&self, plugin: &str) -> Result<Vec<SkillIssue>, SkillsError> {
        self.query(None).await?;
        Ok(self
            .state
            .lock()
            .await
            .plugins
            .get(plugin)
            .map(|(_, snapshot)| snapshot.issues.clone())
            .unwrap_or_default())
    }

    /// Rescans whole roots; any unreadable root keeps every old snapshot.
    pub async fn refresh(
        &self,
        target: RefreshTarget,
    ) -> Result<(CatalogVersion, Vec<SkillIssue>), SkillsError> {
        self.check_open()?;
        let _operating = self.operating.lock().await;
        self.reload_settings().await?;
        let mut state = self.state.lock().await;
        let root_error = |issue| match issue {
            SkillIssue::RootUnreadable { root, reason } => {
                SkillsError::RootUnreadable { root, reason }
            }
            _ => unreachable!("scan_root fails only for an unreadable root"),
        };
        let user = match target.user {
            true => Some(
                scan(self.user_root.clone(), RootKind::User)
                    .await
                    .map_err(root_error)?,
            ),
            false => None,
        };
        let project = match target.project.and_then(|cwd| self.project_root(&cwd)) {
            Some(root) => Some((
                root.clone(),
                scan(root, RootKind::Project).await.map_err(root_error)?,
            )),
            None => None,
        };
        let mut issues = Vec::new();
        if let Some(user) = user {
            issues.extend(user.issues.iter().cloned());
            state.user = Some(Arc::new(user));
        }
        if let Some((root, project)) = project {
            issues.extend(project.issues.iter().cloned());
            state.projects.insert(root, Arc::new(project));
        }
        Ok((self.advance(&mut state), issues))
    }

    /// The Skills a source tree holds, what installing them as `id` would
    /// collide with, and what it would hide. Nothing is written. `cwd` adds
    /// that project root to what it would hide.
    pub async fn preview(
        &self,
        root: &Path,
        id: &str,
        cwd: Option<&Path>,
    ) -> Result<SkillPreview, SkillsError> {
        self.check_open()?;
        let view = {
            let _operating = self.operating.lock().await;
            self.view(cwd).await?
        };
        let snapshot = scan(root.to_owned(), RootKind::Preview)
            .await
            .map_err(root_error)?;
        let owned = format!("user:{id}/");
        let mut skills = Vec::new();
        let mut conflicts = Vec::new();
        let mut shadows = Vec::new();
        let mut issues = snapshot.issues.clone();
        for skill in &snapshot.skills {
            let Some(dir) = skill.file_path.parent() else {
                continue;
            };
            let path = dir
                .strip_prefix(root)
                .map(|path| path.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|_| ".".into());
            let revision = match install::revision(dir) {
                Ok(revision) => revision,
                Err(error) => {
                    issues.push(SkillIssue::Unreadable {
                        path: skill.file_path.clone(),
                        reason: error.to_string(),
                    });
                    continue;
                }
            };
            let conflict = view.entries.iter().find(|entry| {
                entry.definition.name == skill.name
                    && entry.definition.id.starts_with("user:")
                    && !entry.definition.id.starts_with(&owned)
            });
            if let Some(entry) = conflict {
                conflicts.push(PreviewConflict {
                    path: path.clone(),
                    name: skill.name.clone(),
                    by: entry.definition.id.clone(),
                });
            }
            let shadow = view.entries.iter().find(|entry| {
                entry.definition.name == skill.name
                    && !entry.definition.id.starts_with("user:")
                    && entry.status == SkillStatus::Active
            });
            if let Some(entry) = shadow {
                shadows.push(PreviewShadow {
                    path: path.clone(),
                    name: skill.name.clone(),
                    by: entry.definition.id.clone(),
                });
            }
            skills.push(PreviewSkill {
                path,
                name: skill.name.clone(),
                description: skill.description.clone(),
                revision,
            });
        }
        Ok(SkillPreview {
            skills,
            conflicts,
            shadows,
            issues,
        })
    }

    /// Installs `selection` from the acquired tree `root` as `id`, replacing
    /// only what `replace` names. The files, then the record, then the catalog
    /// view: a listener never sees an installation the record does not hold.
    pub async fn install(
        &self,
        id: &str,
        source: SkillInstallSource,
        root: &Path,
        operation_dir: &Path,
        selection: &[SelectedSkill],
        replace: Replace,
    ) -> Result<InstallOutcome, SkillsError> {
        self.check_open()?;
        if !install::valid_id(id) {
            return Err(SkillsError::SelectionInvalid(format!(
                "{id:?} is not an installation id"
            )));
        }
        let selected = validate_selection(selection)?;
        let _operating = self.operating.lock().await;
        self.reload_settings().await?;
        let recorded = self.installations_locked().await?;
        let previous = recorded.get(id).cloned();
        let view = self.view(None).await?;
        match (&replace, &previous) {
            (Replace::Create, Some(_)) => {
                return Err(SkillsError::InstallationExists(id.into()));
            }
            (Replace::Expected(expected), Some(record)) if record != expected => {
                return Err(SkillsError::UnknownInstallation(id.into()));
            }
            (Replace::Expected(_), None) => {
                return Err(SkillsError::UnknownInstallation(id.into()));
            }
            _ => {}
        }
        if previous.is_none() && install::occupied(&self.user_root, id)? {
            return Err(unmanaged(id, &self.user_root, &view));
        }
        let snapshot = scan(root.to_owned(), RootKind::Preview)
            .await
            .map_err(root_error)?;
        let mut found: BTreeMap<String, Arc<super::SkillDefinition>> = BTreeMap::new();
        for skill in &snapshot.skills {
            let Some(dir) = skill.file_path.parent() else {
                continue;
            };
            let path = dir
                .strip_prefix(root)
                .map(|path| path.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|_| ".".into());
            found.insert(path, skill.clone());
        }
        let owned = format!("user:{id}/");
        let mut names = BTreeSet::new();
        let mut revisions = Vec::new();
        let mut placements = Vec::new();
        for skill in &selected {
            let definition = found.get(&skill.path).ok_or_else(|| {
                SkillsError::SelectionInvalid(format!(
                    "{:?} is not a Skill in the source",
                    skill.path
                ))
            })?;
            if !names.insert(definition.name.clone()) {
                return Err(SkillsError::SelectionInvalid(format!(
                    "{:?} is selected twice",
                    definition.name
                )));
            }
            if let Some(entry) = view.entries.iter().find(|entry| {
                entry.definition.id.starts_with("user:")
                    && !entry.definition.id.starts_with(&owned)
                    && entry.definition.name == definition.name
            }) {
                return Err(SkillsError::InstallConflict(format!(
                    "{} ({})",
                    definition.name, entry.definition.id
                )));
            }
            let dir = definition
                .file_path
                .parent()
                .expect("a Skill has a directory");
            let revision = install::revision(dir)?;
            if let Some(expected) = &skill.revision
                && expected != &revision
            {
                return Err(SkillsError::StalePreview {
                    path: skill.path.clone(),
                    expected: expected.clone(),
                    found: revision,
                });
            }
            revisions.push((skill.path.clone(), revision));
            placements.push((dir.to_owned(), skill.path.clone()));
        }
        let placed = install::place(
            &self.user_root,
            id,
            operation_dir,
            &placements,
            previous.is_some(),
        )?;
        let installation = SkillInstallation {
            source,
            revision: install::selection_revision(&revisions),
            skills: selected.iter().map(|skill| skill.path.clone()).collect(),
            extra: BTreeMap::new(),
        };
        let written = self
            .settings
            .clone()
            .set_installation(
                id.into(),
                Some(installation.clone()),
                self.closed.child_token(),
            )
            .await;
        if let Err(error) = written {
            placed.revert();
            return Err(write_error(error));
        }
        self.reload_settings()
            .await
            .map_err(|error| SkillsError::SettingsSync(error.to_string()))?;
        let (version, issues) = self.rescan_user().await?;
        Ok(InstallOutcome {
            installation,
            previous,
            version,
            issues,
        })
    }

    /// Removes the files of installation `id`, then its record. A failed
    /// deletion keeps the record: it is the only proof of what was installed.
    pub async fn uninstall(
        &self,
        id: &str,
    ) -> Result<(SkillInstallation, CatalogVersion), SkillsError> {
        self.check_open()?;
        if !install::valid_id(id) {
            return Err(SkillsError::UnknownInstallation(id.into()));
        }
        let _operating = self.operating.lock().await;
        self.reload_settings().await?;
        let recorded = self.installations_locked().await?;
        let previous = recorded
            .get(id)
            .cloned()
            .ok_or_else(|| SkillsError::UnknownInstallation(id.into()))?;
        install::remove(&self.user_root, id)?;
        self.settings
            .clone()
            .set_installation(id.into(), None, self.closed.child_token())
            .await
            .map_err(write_error)?;
        self.reload_settings()
            .await
            .map_err(|error| SkillsError::SettingsSync(error.to_string()))?;
        let (version, _) = self.rescan_user().await?;
        Ok((previous, version))
    }

    /// The installations the user made, by id.
    pub async fn installations(&self) -> Result<BTreeMap<String, SkillInstallation>, SkillsError> {
        self.query(None).await?;
        self.installations_locked().await
    }

    /// One recorded installation.
    pub async fn installation(&self, id: &str) -> Result<Option<SkillInstallation>, SkillsError> {
        self.query(None).await?;
        Ok(self.installations_locked().await?.get(id).cloned())
    }

    async fn installations_locked(
        &self,
    ) -> Result<BTreeMap<String, SkillInstallation>, SkillsError> {
        Ok(self
            .state
            .lock()
            .await
            .settings
            .as_ref()
            .map_err(Clone::clone)?
            .installations
            .clone())
    }

    /// Rescans the user root and advances the version; the caller holds the
    /// operating lock.
    async fn rescan_user(&self) -> Result<(CatalogVersion, Vec<SkillIssue>), SkillsError> {
        let snapshot = scan(self.user_root.clone(), RootKind::User)
            .await
            .map_err(root_error)?;
        let issues = snapshot.issues.clone();
        let mut state = self.state.lock().await;
        state.user = Some(Arc::new(snapshot));
        Ok((self.advance(&mut state), issues))
    }
}

/// `<user root>/<id>` without a record: what is there is reported, never
/// replaced.
fn unmanaged(id: &str, user_root: &Path, view: &CatalogView) -> SkillsError {
    let owned = format!("user:{id}/");
    let found: Vec<&str> = view
        .entries
        .iter()
        .filter(|entry| entry.definition.id.starts_with(&owned))
        .map(|entry| entry.definition.name.as_str())
        .collect();
    let holds = match found.as_slice() {
        [] => "no Skill".to_owned(),
        names => names.join(", "),
    };
    SkillsError::UnmanagedDirectory(format!(
        "{id:?} is not a recorded installation; {} holds {holds}, and only a recorded installation is replaced: choose another id or move that directory",
        user_root.join(id).display()
    ))
}

/// A scan failure of a root the caller named.
fn root_error(issue: SkillIssue) -> SkillsError {
    match issue {
        SkillIssue::RootUnreadable { root, reason } => SkillsError::RootUnreadable { root, reason },
        _ => unreachable!("scan_root fails only for an unreadable root"),
    }
}

fn write_error(error: SkillSettingsError) -> SkillsError {
    match error {
        SkillSettingsError::Conflict => SkillsError::SettingsConflict,
        SkillSettingsError::Closed | SkillSettingsError::Cancelled => SkillsError::Closed,
        other => SkillsError::SettingsWrite(other.to_string()),
    }
}

/// A non-empty selection of distinct relative paths.
fn validate_selection(selection: &[SelectedSkill]) -> Result<Vec<SelectedSkill>, SkillsError> {
    if selection.is_empty() {
        return Err(SkillsError::SelectionInvalid("no Skills selected".into()));
    }
    let mut seen = BTreeSet::new();
    for skill in selection {
        if install::selection_path(&skill.path).is_none() {
            return Err(SkillsError::SelectionInvalid(format!(
                "{:?} is not a relative path",
                skill.path
            )));
        }
        if !seen.insert(skill.path.clone()) {
            return Err(SkillsError::SelectionInvalid(format!(
                "{:?} is selected twice",
                skill.path
            )));
        }
    }
    Ok(selection.to_vec())
}

/// The Skills plugin: `SkillsCatalog` at the process, and the Session and
/// branch parts it registers ([`super::register_parts`]).
pub struct SkillsCatalogPlugin {
    platform: Key<HostPlatform>,
    settings: Key<SkillSettingsHost>,
    parts: Key<PartRegistry>,
}
impl SkillsCatalogPlugin {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            platform: names.key()?,
            settings: names.key()?,
            parts: names.key()?,
        })
    }
}
impl DynPlugin for SkillsCatalogPlugin {
    fn name(&self) -> &str {
        "SkillsCatalog"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.platform.erase(),
            self.settings.erase(),
            self.parts.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let platform = ctx.require_at(&self.platform)?;
            let settings = ctx.require_at(&self.settings)?;
            let parts = ctx.require_at(&self.parts)?;
            let catalog = Self::mount(ctx, (platform, settings)).await?;
            ctx.provide_as::<SkillsCatalog>(catalog)?;
            super::register_parts(ctx, &parts)?;
            Ok(Effect::Done)
        })
    }
}
impl SkillsCatalogPlugin {
    /// Initializes the catalog in its owner's context. An embedding host may
    /// publish additional interfaces in the same plugin without an adapter row,
    /// and registers the parts there too ([`super::register_parts`]).
    pub async fn mount(
        ctx: &Ctx,
        (platform, settings): (Arc<HostPlatform>, Arc<SkillSettingsHost>),
    ) -> Result<Arc<SkillsCatalog>, Error> {
        let settings = settings.0.clone();
        let lifecycle = ctx.cancellation_token();
        let (wake, mut changes) = watch::channel(());
        ctx.listen::<SettingsChanged>()
            .name(settings_changes(SOURCE))
            .on(move |_, _| {
                wake.send_replace(());
                async { Ok(None) }
            })?;
        let initial = settings
            .skills(lifecycle.child_token())
            .await
            .map_err(|error| SkillsError::SettingsRead(error.to_string()));
        let catalog = Arc::new(SkillsCatalog {
            user_root: platform.home.join(SKILLS_DIR),
            settings: settings.clone(),
            state: tokio::sync::Mutex::new(CatalogState {
                user: None,
                plugins: BTreeMap::new(),
                projects: HashMap::new(),
                settings: initial,
                version: 0,
            }),
            reloading: tokio::sync::Mutex::new(()),
            operating: tokio::sync::Mutex::new(()),
            events: ctx.clone(),
            closed: lifecycle.clone(),
        });
        let tasks = TaskTracker::new();
        let draining = tasks.clone();
        ctx.effect(move || async move {
            draining.close();
            draining.wait().await;
            Ok(())
        })?;
        let owner = catalog.clone();
        tasks.spawn(async move {
            loop {
                tokio::select! {
                    _ = owner.closed.cancelled() => return,
                    changed = changes.changed() => if changed.is_err() { return },
                }
                if let Err(error) = owner.reload_settings().await {
                    tracing::warn!(%error, "Skill settings read failed");
                }
            }
        });
        Ok(catalog)
    }
}

/// In-memory `SkillSettings` for tests and embedded hosts without storage. The
/// catalog is its only writer and reads back its own writes, so it announces
/// nothing.
pub struct MemorySkillSettings {
    state: Mutex<SkillSettingsSnapshot>,
}
impl Default for MemorySkillSettings {
    fn default() -> Self {
        Self {
            state: Mutex::new(SkillSettingsSnapshot {
                version: "0".into(),
                disabled: BTreeSet::new(),
                plugins: BTreeMap::new(),
                installations: BTreeMap::new(),
            }),
        }
    }
}
impl SkillSettings for MemorySkillSettings {
    fn skills(
        &self,
        _: CancellationToken,
    ) -> BoxFuture<'_, Result<SkillSettingsSnapshot, SkillSettingsError>> {
        let snapshot = self.state.lock().unwrap().clone();
        Box::pin(async move { Ok(snapshot) })
    }
    fn update_skills(
        self: Arc<Self>,
        expected: SettingsVersion,
        id: String,
        enabled: bool,
        _: CancellationToken,
    ) -> BoxFuture<'static, Result<SettingsVersion, SkillSettingsError>> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            if state.version != expected {
                return Err(SkillSettingsError::Conflict);
            }
            if enabled {
                state.disabled.remove(&id);
            } else {
                state.disabled.insert(id);
            }
            let next = state.version.parse::<u64>().unwrap_or_default() + 1;
            state.version = next.to_string();
            Ok(state.version.clone())
        })
    }
    fn set_plugin(
        self: Arc<Self>,
        plugin: String,
        root: Option<PathBuf>,
        _: CancellationToken,
    ) -> BoxFuture<'static, Result<SettingsVersion, SkillSettingsError>> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            match root {
                Some(root) => state.plugins.insert(plugin, root),
                None => state.plugins.remove(&plugin),
            };
            let next = state.version.parse::<u64>().unwrap_or_default() + 1;
            state.version = next.to_string();
            Ok(state.version.clone())
        })
    }
    fn set_installation(
        self: Arc<Self>,
        id: String,
        installation: Option<SkillInstallation>,
        _: CancellationToken,
    ) -> BoxFuture<'static, Result<SettingsVersion, SkillSettingsError>> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            match installation {
                Some(installation) => state.installations.insert(id, installation),
                None => state.installations.remove(&id),
            };
            let next = state.version.parse::<u64>().unwrap_or_default() + 1;
            state.version = next.to_string();
            Ok(state.version.clone())
        })
    }
}
