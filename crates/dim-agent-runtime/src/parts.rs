//! The part registry and the instances' mounters
//! ([ADR 006 D3](../../../docs/adr/006-plugin-contract.md#d3-a-plugin-is-one-row-each-instance-mounts-the-parts-registered-for-it)).
//!
//! A plugin registers parts for the Session or branch scope while it runs:
//! a Rust plugin a generator, whose registration is an effect of the caller
//! and is withdrawn when the caller unloads; a plugin of another language a
//! rutis plugin definition, which ends with the row that handed it over. Every Session and branch
//! scope mounts two mounters as its children. The required
//! one mounts the parts the assembly profile requires and hands them to the
//! instance, which publishes only once they are Active. The optional one
//! mounts every other registered part; the instance lets the copies it
//! mounted when it opened settle before it publishes, as it does its optional
//! rows, and a failed or Pending one affects only its own features. Both
//! follow registrations and withdrawals while the instance is open, and a
//! mounted part closes with its instance.
use crate::{Names, SessionConfig};
use rutis::host::{
    BoxFuture, CtxHost, DynPlugin, Effect, ErrorKind, FiberState, FiberView, Key, TypeKey,
};
use rutis::{Ctx, Definition, Error, Plugin, service};
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};
use tokio::sync::watch;

/// The scope a part is registered for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PartScope {
    Session,
    Branch,
}

/// What a generator sees of the instance it builds a part for.
pub struct Mount<'a> {
    /// The names its dependencies resolve to in this instance.
    pub names: &'a Names<'a>,
    /// The Session's configuration (the enclosing Session's for a branch).
    pub session: &'a SessionConfig,
    pub session_id: &'a str,
    /// Whether the Session was restored rather than created.
    pub restored: bool,
    /// The branch, for a branch part.
    pub branch_id: Option<&'a str>,
}

/// Builds a part for one instance, or nothing when that instance does not
/// need one, such as a Session whose configuration turns the feature off.
pub type Generator =
    Arc<dyn Fn(&Mount<'_>) -> Result<Option<Box<dyn DynPlugin>>, Error> + Send + Sync>;

/// What an instance mounts a registered part from.
enum Source {
    /// A Rust plugin's generator.
    Generator(Generator),
    /// A plugin definition another language handed over: each instance
    /// mounts one copy, in that language's runtime.
    Definition(Definition),
}

struct Entry {
    id: u64,
    scope: PartScope,
    name: String,
    source: Source,
}

impl Entry {
    /// The part for the instance of `site`, mounted on `ctx`, or nothing
    /// when the generator builds none there.
    fn build(&self, ctx: &Ctx, site: &MountSite) -> Result<Option<Box<dyn DynPlugin>>, Error> {
        match &self.source {
            Source::Generator(generator) => site.with_mount(|mount| generator(mount)),
            Source::Definition(definition) => {
                Ok(Some(Box::new(definition.at(ctx, serde_json::json!({}))?)))
            }
        }
    }
}

#[derive(Default)]
struct State {
    next_entry: u64,
    next_mounter: u64,
    entries: Vec<Arc<Entry>>,
    /// The copies mounted from each entry, with the mounter that owns each.
    copies: HashMap<u64, Vec<(u64, FiberView)>>,
}

/// The parts plugins register for Sessions and branches. One registry serves
/// every Agent built from an [`AgentBuilder`](crate::AgentBuilder); the host
/// hands it to its process plugins ([`AgentBuilder::parts`](crate::AgentBuilder::parts)).
/// Plugins of other languages reach it by the process name `partRegistry`.
pub struct PartRegistry {
    shared: Arc<Shared>,
}

struct Shared {
    state: Mutex<State>,
    version: watch::Sender<u64>,
}

impl Default for PartRegistry {
    fn default() -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::default(),
                version: watch::channel(0).0,
            }),
        }
    }
}

impl std::fmt::Debug for PartRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.shared.state.lock().unwrap();
        let entries: Vec<_> = state
            .entries
            .iter()
            .map(|entry| (&entry.name, entry.scope))
            .collect();
        f.debug_struct("PartRegistry")
            .field("entries", &entries)
            .finish()
    }
}

impl PartRegistry {
    /// Registers a part generator for `scope` on behalf of the plugin that
    /// owns `ctx`. Every open instance of the scope mounts it, and so does
    /// every later one. `name` is the part's name, by which the assembly
    /// profile requires it ([`AgentBuilder::require_part`](crate::AgentBuilder::require_part)).
    ///
    /// The registration is withdrawn when that plugin unloads: every instance
    /// unmounts its copy, and the plugin's unload waits for those copies to
    /// unload.
    pub fn register(
        self: &Arc<Self>,
        ctx: &Ctx,
        scope: PartScope,
        name: impl Into<String>,
        generator: impl Fn(&Mount<'_>) -> Result<Option<Box<dyn DynPlugin>>, Error>
        + Send
        + Sync
        + 'static,
    ) -> Result<(), Error> {
        let id = self
            .shared
            .add(scope, name.into(), Source::Generator(Arc::new(generator)));
        let shared = self.shared.clone();
        let registered = ctx.effect(move || async move {
            shared.withdraw(id).await;
            Ok(())
        });
        if registered.is_err() {
            let shared = self.shared.clone();
            tokio::spawn(async move { shared.withdraw(id).await });
        }
        registered.map(|_| ())
    }
}

/// The part registry as other languages call it.
#[service]
impl PartRegistry {
    /// Registers a plugin definition for `scope`: every open instance of the
    /// scope mounts one copy of it, and so does every later one, each in the
    /// runtime that defined it with its `inject` resolved in that instance.
    /// `name` is the part's name, as for a generator.
    ///
    /// The definition belongs to the row that handed it over. When that row
    /// unloads, rutis ends every copy first; the registration then goes.
    pub fn register_definition(&self, scope: PartScope, name: String, definition: Definition) {
        let ended = definition.ended();
        let id = self.shared.add(scope, name, Source::Definition(definition));
        let shared = self.shared.clone();
        tokio::spawn(async move {
            ended.await;
            shared.withdraw(id).await;
        });
    }
}

impl Shared {
    fn add(&self, scope: PartScope, name: String, source: Source) -> u64 {
        let id = {
            let mut state = self.state.lock().unwrap();
            state.next_entry += 1;
            let id = state.next_entry;
            state.entries.push(Arc::new(Entry {
                id,
                scope,
                name,
                source,
            }));
            id
        };
        self.version.send_modify(|version| *version += 1);
        id
    }

    /// Removes the entry and unloads every copy mounted from it.
    async fn withdraw(&self, id: u64) {
        let copies = {
            let mut state = self.state.lock().unwrap();
            state.entries.retain(|entry| entry.id != id);
            state.copies.remove(&id).unwrap_or_default()
        };
        self.version.send_modify(|version| *version += 1);
        futures_util::future::join_all(copies.into_iter().map(|(_, view)| async move {
            let _ = view.dispose().await;
        }))
        .await;
    }

    /// Every registration of every scope at one moment, by name: a mounter
    /// decides and mounts from the same snapshot, in the same order each time.
    fn snapshot(&self) -> Vec<Arc<Entry>> {
        let mut entries = self.state.lock().unwrap().entries.clone();
        entries.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        entries
    }

    /// Records a mounted copy. False when its entry was withdrawn meanwhile;
    /// the caller then unloads the copy itself.
    fn attach(&self, entry: u64, mounter: u64, view: FiberView) -> bool {
        let mut state = self.state.lock().unwrap();
        if !state.entries.iter().any(|e| e.id == entry) {
            return false;
        }
        state.copies.entry(entry).or_default().push((mounter, view));
        true
    }

    /// Forgets a mounter's copies: its instance unloads them with itself.
    fn detach(&self, mounter: u64) {
        let mut state = self.state.lock().unwrap();
        for copies in state.copies.values_mut() {
            copies.retain(|(owner, _)| *owner != mounter);
        }
        state.copies.retain(|_, copies| !copies.is_empty());
    }

    fn mounter(&self) -> u64 {
        let mut state = self.state.lock().unwrap();
        state.next_mounter += 1;
        state.next_mounter
    }
}

/// Every copy an instance's mounters mounted: its close counts a copy that
/// already failed as it counts a failed row ([`crate::assembly`]).
pub(crate) type Copies = Arc<Mutex<Vec<FiberView>>>;

/// The copies an instance's mounters mounted when it opened. Its creation
/// waits for the required ones to be Active, as for its required rows, and
/// lets the others settle, as its optional rows; a part registered later
/// never holds publication.
#[derive(Clone, Default)]
pub(crate) struct Opened {
    pub required: Vec<FiberView>,
    pub optional: Vec<FiberView>,
}

/// What a mounter knows of its instance.
pub(crate) struct MountSite {
    pub scope: PartScope,
    pub registry: Arc<PartRegistry>,
    /// Registered parts the profile requires, by name.
    pub required: Arc<HashSet<String>>,
    pub catalog: Arc<rutis_loader::ServiceCatalog>,
    pub build: rutis_loader::resolve::Build,
    pub session: SessionConfig,
    pub session_id: String,
    pub restored: bool,
    pub branch_id: Option<String>,
    /// Where the mounters hand the copies they mounted when the instance
    /// opened, which its creation waits for before it publishes.
    pub mounted: Arc<Mutex<Opened>>,
    pub copies: Copies,
}

impl MountSite {
    /// Calls `f` with this instance's [`Mount`].
    fn with_mount<R>(&self, f: impl FnOnce(&Mount<'_>) -> R) -> R {
        let names = Names::copy(&self.catalog, &self.build);
        f(&Mount {
            names: &names,
            session: &self.session,
            session_id: &self.session_id,
            restored: self.restored,
            branch_id: self.branch_id.as_deref(),
        })
    }
}

/// One of an instance's two mounters ([module docs](self)).
pub(crate) struct PartMounter {
    pub site: Arc<MountSite>,
    pub required: bool,
}

impl PartMounter {
    /// The module and plugin name of a mounter.
    pub(crate) fn module(scope: PartScope, required: bool) -> &'static str {
        match (scope, required) {
            (PartScope::Session, true) => "SessionParts",
            (PartScope::Session, false) => "SessionOptionalParts",
            (PartScope::Branch, true) => "BranchParts",
            (PartScope::Branch, false) => "BranchOptionalParts",
        }
    }

    /// Whether `name` is one of the mounters' names.
    pub(crate) fn is_module(name: &str) -> bool {
        [PartScope::Session, PartScope::Branch]
            .into_iter()
            .flat_map(|scope| [Self::module(scope, true), Self::module(scope, false)])
            .any(|module| module == name)
    }

    fn owns(&self, entry: &Entry) -> bool {
        self.site.required.contains(&entry.name) == self.required
    }

    /// The required names `snapshot` has no registration of, in any scope.
    fn missing(&self, snapshot: &[Arc<Entry>]) -> Vec<String> {
        let mut missing: Vec<String> = self
            .site
            .required
            .iter()
            .filter(|name| !snapshot.iter().any(|entry| &entry.name == *name))
            .cloned()
            .collect();
        missing.sort();
        missing
    }

    /// Mounts the entries of `snapshot` this mounter owns that it has not
    /// seen yet, and forgets withdrawn ones, whose copies the registry
    /// unloads. Returns the new copies. A registered part that fails to build
    /// fails a required mounter; for the optional one it affects only that
    /// part.
    fn reconcile(
        &self,
        ctx: &Ctx,
        id: u64,
        seen: &mut HashMap<u64, Option<FiberView>>,
        snapshot: &[Arc<Entry>],
    ) -> Result<Vec<FiberView>, Error> {
        let site = &self.site;
        let entries: Vec<_> = snapshot
            .iter()
            .filter(|entry| entry.scope == site.scope)
            .cloned()
            .collect();
        let live: HashSet<_> = entries.iter().map(|entry| entry.id).collect();
        let withdrawn: HashSet<_> = seen
            .extract_if(|entry, _| !live.contains(entry))
            .filter_map(|(_, view)| view.map(|view| view.id))
            .collect();
        site.copies
            .lock()
            .unwrap()
            .retain(|copy| !withdrawn.contains(&copy.id));
        let mut new = vec![];
        for entry in entries {
            if !self.owns(&entry) || seen.contains_key(&entry.id) {
                continue;
            }
            let part = match entry.build(ctx, site) {
                Ok(part) => part,
                Err(error) if self.required => return Err(error),
                Err(error) => {
                    tracing::warn!(part = %entry.name, %error, "a registered part failed to build");
                    seen.insert(entry.id, None);
                    continue;
                }
            };
            let Some(part) = part else {
                seen.insert(entry.id, None);
                continue;
            };
            let view = mount(ctx, part, &site.copies);
            if !site.registry.shared.attach(entry.id, id, view.clone()) {
                tokio::spawn(async move {
                    let _ = view.dispose().await;
                });
                continue;
            }
            seen.insert(entry.id, Some(view.clone()));
            new.push(view);
        }
        Ok(new)
    }
}

/// Mounts a copy under the mounter.
fn mount(ctx: &Ctx, part: Box<dyn DynPlugin>, copies: &Copies) -> FiberView {
    let view = ctx.plugin(part);
    let mut copies = copies.lock().unwrap();
    copies.retain(|copy| copy.state().state != FiberState::Disposed);
    copies.push(view.clone());
    view
}

/// Unloads a mounter's copies before rutis's own cascade does. A copy that
/// already failed showed its failure when it happened and is not reported
/// again; a cleanup error of a running copy is the mounter's, as a row's
/// would be.
async fn unmount(copies: impl Iterator<Item = FiberView>) -> Result<(), Error> {
    let errors: Vec<_> = futures_util::future::join_all(copies.map(|copy| async move {
        match copy.state().state {
            FiberState::Disposed => None,
            FiberState::Failed => {
                let _ = copy.dispose().await;
                None
            }
            _ => copy.dispose().await.err(),
        }
    }))
    .await
    .into_iter()
    .flatten()
    .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ErrorKind::Aggregate { errors }.into())
    }
}

impl DynPlugin for PartMounter {
    fn name(&self) -> &str {
        Self::module(self.site.scope, self.required)
    }

    fn injects(&self) -> Vec<TypeKey> {
        Vec::new()
    }

    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, Error>> {
        Box::pin(async move {
            let site = self.site.clone();
            let id = site.registry.shared.mounter();
            let mut waiting = vec![];
            let mut changes = site.registry.shared.version.subscribe();
            changes.mark_unchanged();
            let mut seen = HashMap::new();
            let cancel = ctx.cancellation_token();
            let snapshot = loop {
                let snapshot = site.registry.shared.snapshot();
                let missing = if self.required {
                    self.missing(&snapshot)
                } else {
                    vec![]
                };
                if missing.is_empty() {
                    break snapshot;
                }
                tokio::select! {
                    changed = changes.changed() => if changed.is_err() { break snapshot },
                    () = cancel.cancelled() => {
                        return Err(Error::from(ErrorKind::PluginFailed(
                            format!("required parts not registered: {}", missing.join(", "))
                                .into(),
                        )));
                    }
                }
            };
            waiting.extend(self.reconcile(ctx, id, &mut seen, &snapshot)?);
            let follow = {
                let this = PartMounter {
                    site: site.clone(),
                    required: self.required,
                };
                let (ctx, cancel) = (ctx.clone(), cancel.clone());
                tokio::spawn(async move {
                    loop {
                        tokio::select! {
                            changed = changes.changed() => if changed.is_err() { break },
                            _ = cancel.cancelled() => break,
                        }
                        let snapshot = this.site.registry.shared.snapshot();
                        if let Err(error) = this.reconcile(&ctx, id, &mut seen, &snapshot) {
                            tracing::warn!(%error, "a registered part failed to build");
                        }
                    }
                    seen
                })
            };
            let registry = site.registry.clone();
            ctx.effect(move || async move {
                cancel.cancel();
                let seen = match follow.await {
                    Ok(seen) => seen,
                    Err(error) => {
                        tracing::warn!(%error, "a part mounter's follow task ended abnormally");
                        HashMap::new()
                    }
                };
                registry.shared.detach(id);
                unmount(seen.into_values().flatten()).await
            })?;
            let mut opened = site.mounted.lock().unwrap();
            if self.required {
                opened.required.extend(waiting);
            } else {
                opened.optional.extend(waiting);
            }
            Ok(Effect::Done)
        })
    }
}

/// One part a [`Feature`] registers.
#[derive(Clone)]
struct FeaturePart {
    scope: PartScope,
    name: String,
    generator: Generator,
}

/// A plugin that owns nothing for the process: while it runs it registers
/// its Session and branch parts ([ADR 006 D3](../../../docs/adr/006-plugin-contract.md#d3-a-plugin-is-one-row-each-instance-mounts-the-parts-registered-for-it)).
/// A feature that holds process state writes its own plugin and registers
/// with [`PartRegistry::register`]. Add it with
/// [`AgentBuilder::feature`](crate::AgentBuilder::feature); the host's
/// profile requires parts by name
/// ([`AgentBuilder::require_part`](crate::AgentBuilder::require_part)).
#[derive(Clone)]
pub struct Feature {
    name: String,
    parts: Vec<FeaturePart>,
}

impl Feature {
    /// A feature whose plugin is named `name`.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            parts: vec![],
        }
    }

    /// A part of every Session named `name`: `make` builds a Session's copy
    /// from the names its dependencies resolve to there, usually
    /// `|names| Ok(Box::new(MyPart::new(names)?))`.
    pub fn session(
        self,
        name: impl Into<String>,
        make: impl Fn(&Names<'_>) -> Result<Box<dyn DynPlugin>, Error> + Send + Sync + 'static,
    ) -> Self {
        self.when(PartScope::Session, name, move |mount| {
            make(mount.names).map(Some)
        })
    }

    /// A part of every branch named `name`, as [`Self::session`].
    pub fn branch(
        self,
        name: impl Into<String>,
        make: impl Fn(&Names<'_>) -> Result<Box<dyn DynPlugin>, Error> + Send + Sync + 'static,
    ) -> Self {
        self.when(PartScope::Branch, name, move |mount| {
            make(mount.names).map(Some)
        })
    }

    /// A part of `scope` that `generator` builds or not for each instance,
    /// from its identity and configuration ([`Mount`]).
    pub fn when(
        mut self,
        scope: PartScope,
        name: impl Into<String>,
        generator: impl Fn(&Mount<'_>) -> Result<Option<Box<dyn DynPlugin>>, Error>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.parts.push(FeaturePart {
            scope,
            name: name.into(),
            generator: Arc::new(generator),
        });
        self
    }

    /// The name of its plugin.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The names of the parts it registers.
    pub fn part_names(&self) -> impl Iterator<Item = &str> {
        self.parts.iter().map(|part| part.name.as_str())
    }

    /// Its plugin, keyed from `names`.
    pub fn plugin(&self, names: &Names<'_>) -> Result<Box<dyn DynPlugin>, Error> {
        Ok(Box::new(FeaturePlugin {
            feature: self.clone(),
            registry: names.key()?,
        }))
    }
}

struct FeaturePlugin {
    feature: Feature,
    registry: Key<PartRegistry>,
}

impl DynPlugin for FeaturePlugin {
    fn name(&self) -> &str {
        &self.feature.name
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.registry.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, Error>> {
        Box::pin(async move {
            let registry = ctx.require_at(&self.registry)?;
            for part in &self.feature.parts {
                let generator = part.generator.clone();
                registry.register(ctx, part.scope, part.name.clone(), move |mount| {
                    generator(mount)
                })?;
            }
            Ok(Effect::Done)
        })
    }
}

/// Provides a placement's registry to its plugins, at the process name
/// `partRegistry`.
pub(crate) struct PartRegistryPlugin {
    pub registry: Arc<PartRegistry>,
    pub key: Key<PartRegistry>,
}

impl Plugin for PartRegistryPlugin {
    type Deps = ();
    fn name(&self) -> &str {
        "PartRegistry"
    }
    async fn apply(&self, ctx: &Ctx, (): ()) -> rutis::Result {
        ctx.provide_at(self.key.clone(), self.registry.clone())?;
        Ok(())
    }
}
