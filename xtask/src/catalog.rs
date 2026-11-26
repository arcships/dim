//! The service catalog table ([How Dim uses rutis](../../docs/design/rutis-usage.md)
//! §3.4 and R4).
//!
//! `graph` serves the App Server product profile, sharing the names `serve`
//! shares when it hosts foreign parts (`session_typescript::share_names`), opens
//! one Session and writes `target/architecture/catalog.md` from that assembly: every
//! catalog name one of its parts provides or injects, with its group, provider,
//! its face and the methods its dispatcher serves, and the
//! parts that are Capabilities because they inject `dispatcher` or `timeline`.
//! `check` validates the faces in the same assembly.
use crate::app_server;
use dim_agent_runtime::{FACE_REASONS, Faces, Feature, SessionConfig, names::name_of_type};
use rutis::host::diagnostics::RuntimeDiagnostics;
use rutis::host::{CtxHost, InstanceId, TypeKey};
use rutis::{Ctx, Plugin};
use rutis_bridge::session::{HostDispatch, host_key};
use rutis_loader::resolve::Build;
use rutis_loader::{LoaderError, ServiceCatalog};
use std::{
    any::TypeId,
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// The names whose injection makes a part a Capability (plugin contract §6).
const CAPABILITY_NAMES: [&str; 2] = ["dispatcher", "timeline"];
/// The plugins rutis serves an exported Rust service through: not Dim parts,
/// and not its provider, which provides the Rust service they serve.
const EXPORT_PREFIX: &str = "rutis-export:";
/// Groups in the order the table lists them.
const GROUPS: [&str; 3] = ["process", "session", "branch"];

/// A branch part that keeps its context, to read the dispatchers the
/// assembly serves names with. It names and provides nothing.
#[derive(Clone, Default)]
struct Probe(Arc<Mutex<Option<Ctx>>>);
impl Plugin for Probe {
    type Deps = ();
    fn name(&self) -> &str {
        "CatalogProbe"
    }
    async fn apply(&self, ctx: &Ctx, (): ()) -> rutis::Result {
        *self.0.lock().unwrap() = Some(ctx.clone());
        Ok(())
    }
}

/// The catalog table of the product profile with one Session.
pub(super) async fn collect() -> Result<Table> {
    let dir = app_server::scratch("catalog")?;
    let probe = Probe::default();
    let server = app_server::serve_product(&dir, |builder| {
        dim_app_server::session_typescript::share_names(builder.catalog_mut());
        dim_app_server::register_faces(builder);
        let probe = probe.clone();
        builder.require_feature(
            Feature::new("CatalogProbe")
                .branch("CatalogProbe", move |_| Ok(Box::new(probe.clone()))),
        );
    })
    .await?;
    let table = async {
        let agent = server
            .agent()
            .ok_or("the product profile has no default Agent")?;
        let session = agent
            .create_session(
                SessionConfig {
                    cwd: dir.to_string_lossy().into_owned(),
                    tools: None,
                    ..Default::default()
                },
                Default::default(),
            )
            .await?;
        let layers = [
            ("process", app_server::process_layer(server.diagnostics())),
            ("session", session.diagnostics()?),
            ("branch", session.branch_diagnostics("main")?),
        ];
        let ctx = probe.0.lock().unwrap().take();
        let ctx = ctx.ok_or("the catalog probe did not load")?;
        let table = Table::new(agent.catalog(), agent.faces(), &layers, &ctx)?;
        reach(&root_of(), agent.catalog(), agent.faces(), &table)?;
        retired(&root_of(), agent.catalog())?;
        session.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(table)
    }
    .await;
    server.shutdown();
    server.join().await?;
    let _ = std::fs::remove_dir_all(&dir);
    table
}

/// Fails when a service name the access layer records as retired is registered
/// again: the generated authoring types only cover names a route resolves, so a
/// retired name coming back would otherwise go unnoticed
/// (`xtask/access-layer.json`'s `removed` list).
fn retired(root: &std::path::Path, catalog: &ServiceCatalog) -> Result<()> {
    let layer = xtask_check::access::load(root)?;
    // A catalog name is known at the process, as a shared name, or inside an
    // instance group: `key_in` answers `OutsideInstance` for the last.
    let known = |name: &str| {
        catalog.key(name).is_some()
            || catalog.is_shared(name)
            || matches!(
                catalog.key_in(name, &rutis_loader::resolve::Build::default()),
                Err(rutis_loader::LoaderError::OutsideInstance { .. })
            )
    };
    let back: Vec<&String> = layer.removed.iter().filter(|name| known(name)).collect();
    if !back.is_empty() {
        return Err(format!(
            "{}: retired service names are registered again: {back:?}",
            xtask_check::access::FILE
        )
        .into());
    }
    Ok(())
}

/// The workspace root, from this crate's manifest directory.
fn root_of() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Checks the generated authoring types against what the runtime resolves: every
/// name `npm/branch-services.d.ts` offers for a scope is a data face the route
/// accepts there. A name the route refuses, or a face of the other scope, must
/// not be offered, so this compares the two lists with the runtime's own rule
/// ([`dim_agent_runtime::route::resolves`]).
fn reach(
    root: &std::path::Path,
    catalog: &ServiceCatalog,
    faces: &Faces,
    table: &Table,
) -> Result<()> {
    let text = std::fs::read_to_string(root.join("npm/branch-services.d.ts"))?;
    let mut offered: Vec<(&str, &str)> = Vec::new();
    for (section, group) in [
        ("export type SessionFaces = {", "session"),
        ("export type LineFaces = {", "branch"),
    ] {
        let Some(start) = text.find(section) else {
            return Err(format!("npm/branch-services.d.ts: no {section}").into());
        };
        for line in text[start + section.len()..].lines() {
            let line = line.trim();
            if line == "}" {
                break;
            }
            let Some(name) = line.split(':').next().map(str::trim) else {
                continue;
            };
            if name.is_empty() {
                continue;
            }
            offered.push((name, group));
        }
    }
    let mut refused = Vec::new();
    for (name, group) in &offered {
        if !dim_agent_runtime::resolves(catalog, faces, name, group) {
            refused.push(format!("{name} ({group})"));
        }
    }
    if !refused.is_empty() {
        return Err(format!(
            "npm/branch-services.d.ts offers names the route refuses: {}",
            refused.join(", ")
        )
        .into());
    }
    let unknown: Vec<&str> = offered
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| !table.services.contains_key(*name))
        .collect();
    if !unknown.is_empty() {
        return Err(format!(
            "npm/branch-services.d.ts offers names the assembly does not provide: {}",
            unknown.join(", ")
        )
        .into());
    }
    // The other direction: a data face the route resolves and the file declares
    // an interface for must be offered, or an author cannot write the call.
    let mut missing = Vec::new();
    for (name, service) in &table.services {
        let group: &str = match service.group {
            group @ ("session" | "branch") => group,
            _ => continue,
        };
        if service.face != "data" || !dim_agent_runtime::resolves(catalog, faces, name, group) {
            continue;
        }
        let declared = text.lines().any(|line| {
            line.strip_prefix("export interface ")
                .or_else(|| line.strip_prefix("export type "))
                .and_then(|rest| rest.split([' ', '<', '{', '=']).next())
                .is_some_and(|ty| dim_agent_runtime::names::name_of_type(ty) == *name)
        });
        if declared && !offered.iter().any(|(offered, _)| *offered == name) {
            missing.push(format!("{name} ({group})"));
        }
    }
    if !missing.is_empty() {
        return Err(format!(
            "npm/branch-services.d.ts does not offer routable data faces: {}",
            missing.join(", ")
        )
        .into());
    }
    Ok(())
}

/// The face of one catalog name (#1289).
fn face(name: &str, shared: bool, dispatcher: bool, reason: Option<&str>) -> Result<String> {
    match (shared, dispatcher, reason) {
        (true, true, None) => Ok("data".into()),
        (true, false, None) => Err(format!("{name}: shared but has no dispatcher").into()),
        (false, true, _) => Err(format!("{name}: has a dispatcher but is not shared").into()),
        (false, false, Some(reason)) if FACE_REASONS.contains(&reason) => {
            Ok(format!("internal({reason})"))
        }
        (false, false, Some(reason)) => {
            Err(format!("{name}: unknown internal reason {reason}").into())
        }
        (false, false, None) => Err(format!("{name}: no face and no internal reason").into()),
        (true, _, Some(reason)) => Err(format!("{name}: shared and internal({reason})").into()),
    }
}

#[derive(Default)]
struct Service {
    group: &'static str,
    providers: BTreeSet<String>,
    face: String,
    methods: BTreeMap<String, String>,
}

struct Capability {
    group: &'static str,
    part: String,
    injects: BTreeSet<String>,
}

pub(super) struct Table {
    services: BTreeMap<String, Service>,
    capabilities: Vec<Capability>,
}

impl Table {
    fn new(
        catalog: &ServiceCatalog,
        faces: &Faces,
        layers: &[(&'static str, RuntimeDiagnostics)],
        ctx: &Ctx,
    ) -> Result<Self> {
        let mut instances = BTreeMap::new();
        for (group, scope) in [("session", "SessionScope"), ("branch", "BranchScope")] {
            let (_, layer) = layers
                .iter()
                .find(|(layer, _)| *layer == group)
                .ok_or("missing layer")?;
            let scope = layer
                .plugins
                .iter()
                .find(|p| p.name == scope)
                .ok_or("missing scope plugin")?;
            let instance = layer
                .plugins
                .iter()
                .find(|p| Some(p.id) == scope.parent)
                .map(|p| p.instance)
                .ok_or("missing scope group")?;
            instances.insert(group, instance);
        }
        let names = Names {
            catalog,
            instances: &instances,
            ctx,
        };
        let plugins: BTreeMap<_, _> = layers
            .iter()
            .flat_map(|(_, layer)| &layer.plugins)
            .map(|plugin| (plugin.id, plugin.name.as_str()))
            .collect();
        let mut services = BTreeMap::<String, Service>::new();
        let mut capabilities = Vec::new();
        for (layer, diagnostics) in layers {
            for binding in &diagnostics.bindings {
                let Some(found) = names.of(&binding.key) else {
                    continue;
                };
                let provider = plugins.get(&binding.provider).copied().unwrap_or("?");
                let service = services.entry(found.name).or_default();
                service.group = found.group;
                if !provider.starts_with(EXPORT_PREFIX) {
                    service.providers.insert(provider.to_owned());
                }
            }
            for plugin in &diagnostics.plugins {
                if plugin.name.starts_with(EXPORT_PREFIX) {
                    continue;
                }
                let mut injects = BTreeSet::new();
                for inject in &plugin.injects {
                    if let Some(found) = names.of(&inject.key) {
                        services.entry(found.name.clone()).or_default().group = found.group;
                        if CAPABILITY_NAMES.contains(&found.name.as_str()) {
                            injects.insert(found.name);
                        }
                    }
                }
                if !injects.is_empty() {
                    capabilities.push(Capability {
                        group: layer,
                        part: plugin.name.clone(),
                        injects,
                    });
                }
            }
        }
        for (name, service) in &mut services {
            let shared = catalog.is_shared(name);
            let key = match instances.get(service.group) {
                Some(instance) => host_key(name).instance(*instance),
                None => host_key(name),
            };
            let dispatch = ctx.get_at::<dyn HostDispatch>(&key);
            let reason = faces.reason(name);
            service.face = face(name, shared, dispatch.is_some(), reason)?;
            if let Some(methods) = dispatch.and_then(|dispatch| dispatch.methods()) {
                service.methods = serde_json::from_value(methods)?;
            }
        }
        for (name, reason) in faces.iter() {
            match services.get(name) {
                None => {
                    return Err(
                        format!("{name}: internal({reason}) names no catalog service").into(),
                    );
                }
                Some(service) if service.face == "data" => {
                    return Err(format!("{name}: internal({reason}) is also a data face").into());
                }
                Some(_) => {}
            }
        }
        capabilities.sort_by(|a, b| (order(a.group), &a.part).cmp(&(order(b.group), &b.part)));
        Ok(Self {
            services,
            capabilities,
        })
    }

    pub(super) fn render(&self) -> String {
        let mut out = String::from(
            "# Service catalog\n\n<!-- Generated by `cargo run -p xtask -- graph`; do not edit. -->\n\n\
Generated from the real assembly: the App Server product profile (`serve` with a product home, sharing the names it shares when it hosts foreign parts) with one Session and its `main` branch. \
`cargo run -p xtask -- check` validates the assembled service faces. \
The rules for names are in [How Dim uses rutis §3](../../docs/design/rutis-usage.md#3-services-by-name).\n\n\
## Services\n\n\
Each catalog name a part of the assembly provides or injects. `data` means shared with a dispatcher; `internal(reason)` means the face cannot cross. Methods are `sync` or `async` (TypeScript callers receive a direct value for `sync` and a Promise for `async`). \
A non-data name is reached by Rust parts at its type's key.\n\n\
| Name | Group | Provider | Face | Methods |\n| --- | --- | --- | --- | --- |\n",
        );
        let mut rows: Vec<_> = self.services.iter().collect();
        rows.sort_by_key(|(name, service)| (order(service.group), *name));
        for (name, service) in rows {
            let provider = if service.providers.is_empty() {
                "—".to_owned()
            } else {
                service
                    .providers
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let face = service.face.as_str();
            let methods = if service.methods.is_empty() {
                "—".to_owned()
            } else {
                service
                    .methods
                    .iter()
                    .map(|(method, mode)| format!("`{method}` {mode}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            out += &format!(
                "| `{name}` | {} | {provider} | {face} | {methods} |\n",
                service.group
            );
        }
        out += "\n## Capabilities\n\n\
The parts that inject `dispatcher` or `timeline` ([plugin contract §6](../../docs/design/plugin-contract.md#6-capability)). \
Every other part is not a Capability.\n\n\
| Part | Group | Injects |\n| --- | --- | --- |\n";
        for capability in &self.capabilities {
            let injects = capability
                .injects
                .iter()
                .map(|name| format!("`{name}`"))
                .collect::<Vec<_>>()
                .join(", ");
            out += &format!(
                "| {} | {} | {injects} |\n",
                capability.part, capability.group
            );
        }
        out
    }
}

fn order(group: &str) -> usize {
    GROUPS
        .iter()
        .position(|known| *known == group)
        .unwrap_or(GROUPS.len())
}

/// Which catalog name a key of the assembly is.
struct Names<'a> {
    catalog: &'a ServiceCatalog,
    ctx: &'a Ctx,
    /// The Session's and the branch's instance.
    instances: &'a BTreeMap<&'static str, InstanceId>,
}
struct Found {
    name: String,
    group: &'static str,
}
impl Names<'_> {
    /// A dispatcher's key carries its name; a Rust key is named after its
    /// type when the catalog maps that key or a matching dispatcher exists.
    /// Shared fallback alone does not name an owner-private Rust key.
    fn of(&self, key: &TypeKey) -> Option<Found> {
        let dispatcher = key.type_id() == TypeId::of::<dyn HostDispatch>();
        let name = match key.name() {
            Some(name) if dispatcher => name.to_owned(),
            None if !dispatcher => name_of_type(key.type_name()),
            _ => return None,
        };
        let group = match self.catalog.key_in(&name, &Build::default()) {
            Ok(registered) => {
                let rust = &registered == key;
                let shared = registered == host_key(&name).erase()
                    && (dispatcher
                        || self
                            .ctx
                            .get_at::<dyn HostDispatch>(&host_key(&name))
                            .is_some());
                (key.instance_id().is_none() && (rust || shared)).then_some("process")?
            }
            Err(LoaderError::OutsideInstance { group, .. }) => {
                let (group, instance) = self.instances.get_key_value(group.as_str())?;
                let mapped = !self.catalog.is_shared(&name)
                    || dispatcher
                    || self
                        .ctx
                        .get_at::<dyn HostDispatch>(&host_key(&name).instance(*instance))
                        .is_some();
                (key.instance_id() == Some(*instance) && mapped).then_some(*group)?
            }
            Err(_) => return None,
        };
        Some(Found { name, group })
    }
}

#[cfg(test)]
mod face_tests {
    use super::face;
    struct CatalogPrivate;
    struct CatalogEcho;

    #[rutis::service]
    impl CatalogEcho {
        pub fn ping(&self) -> String {
            "echo".into()
        }
    }

    struct CatalogBindings;
    impl rutis::Plugin for CatalogBindings {
        type Deps = ();
        fn name(&self) -> &str {
            "CatalogBindings"
        }
        async fn apply(&self, ctx: &rutis::Ctx, (): ()) -> rutis::Result {
            ctx.provide(CatalogPrivate)?;
            ctx.provide(CatalogEcho)?;
            Ok(())
        }
    }

    struct CatalogMissing;
    impl rutis::host::DynPlugin for CatalogMissing {
        fn name(&self) -> &str {
            "CatalogMissing"
        }
        fn injects(&self) -> Vec<rutis::host::TypeKey> {
            vec![rutis_bridge::session::host_key("catalogMissing").erase()]
        }
        fn apply<'a>(
            &'a self,
            _: &'a rutis::Ctx,
        ) -> rutis::host::BoxFuture<'a, rutis::Result<rutis::host::Effect>> {
            Box::pin(async { Ok(rutis::host::Effect::Done) })
        }
    }

    #[tokio::test]
    async fn product_catalog_distinguishes_private_keys_from_public_bindings() {
        use super::*;
        for missing in [false, true] {
            let dir = app_server::scratch(if missing {
                "catalog-missing"
            } else {
                "catalog-bindings"
            })
            .unwrap();
            let probe = Probe::default();
            let server = app_server::serve_product(&dir, |builder| {
                builder.plugin(|_| Ok(Box::new(CatalogBindings)));
                if missing {
                    builder.optional_plugin(|_| Ok(Box::new(CatalogMissing)));
                }
                builder.catalog_mut().register_shared("catalogEcho");
                let probe = probe.clone();
                builder.require_feature(
                    Feature::new("CatalogProbe")
                        .branch("CatalogProbe", move |_| Ok(Box::new(probe.clone()))),
                );
            })
            .await
            .unwrap();
            let agent = server.agent().unwrap();
            let session = agent
                .create_session(
                    SessionConfig {
                        cwd: dir.to_string_lossy().into_owned(),
                        tools: None,
                        ..Default::default()
                    },
                    Default::default(),
                )
                .await
                .unwrap();
            let layers = [
                ("process", app_server::process_layer(server.diagnostics())),
                ("session", session.diagnostics().unwrap()),
                ("branch", session.branch_diagnostics("main").unwrap()),
            ];
            let ctx = probe.0.lock().unwrap().take().unwrap();
            if missing {
                let result = Table::new(agent.catalog(), agent.faces(), &layers, &ctx);
                assert!(
                    matches!(result, Err(error) if error.to_string().contains("catalogMissing: shared but has no dispatcher"))
                );
            } else {
                let table = Table::new(agent.catalog(), agent.faces(), &layers, &ctx).unwrap();
                let echo = &table.services["catalogEcho"];
                assert_eq!(echo.group, "process");
                assert!(echo.providers.contains("CatalogBindings"));
                assert_eq!(echo.face, "data");
                assert_eq!(echo.methods["ping"], "async");
                for private in ["catalogPrivate", "loaderService", "pluginMounter"] {
                    assert!(
                        !table.services.contains_key(private),
                        "{private} is a private key, not a public face"
                    );
                }
                assert!(table.services.contains_key("rutis/loader"));
                let reply: String =
                    rutis_bridge::session::call_service(&ctx, "catalogEcho", "ping", ())
                        .await
                        .unwrap();
                assert_eq!(reply, ctx.get::<CatalogEcho>().unwrap().ping());
                let mut declared = agent.catalog().clone();
                declared.register::<CatalogPrivate>("catalogPrivate");
                let result = Table::new(&declared, agent.faces(), &layers, &ctx);
                assert!(
                    matches!(result, Err(error) if error.to_string().contains("catalogPrivate: no face"))
                );
            }
            session.close().await.unwrap();
            server.shutdown();
            server.join().await.unwrap();
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn data_and_internal_are_derived_from_shared_dispatcher_and_reason() {
        assert_eq!(face("x", true, true, None).unwrap(), "data");
        assert_eq!(
            face("x", false, false, Some("secret")).unwrap(),
            "internal(secret)"
        );
        assert!(face("loaderService", true, false, None).is_err());
    }
    #[test]
    fn contradictions_are_errors() {
        assert!(face("x", false, true, None).is_err());
        assert!(face("x", true, true, Some("secret")).is_err());
        assert!(face("x", false, false, None).is_err());
        assert!(face("x", true, false, None).is_err());
        assert!(face("x", false, false, Some("hotpath")).is_err());
    }
}

#[cfg(test)]
mod retired_tests {
    use super::*;

    #[test]
    fn a_retired_instance_name_is_refused() {
        let directory = std::env::temp_dir().join(format!("dim-retired-{}", std::process::id()));
        std::fs::create_dir_all(directory.join("xtask")).unwrap();
        // `sessionConfig` is registered inside the Session group only.
        std::fs::write(
            directory.join("xtask/access-layer.json"),
            r#"{"allow": [], "removed": ["sessionConfig"]}"#,
        )
        .unwrap();
        let catalog = dim_agent_runtime::names::catalog();
        let error = retired(&directory, &catalog).expect_err("an instance name is covered");
        assert!(error.to_string().contains("sessionConfig"), "{error}");
        let _ = std::fs::remove_dir_all(&directory);
    }
}
