//! The client methods `settings.*` over declared sources (settings-service
//! §4.3). Settings stays persistence infrastructure: Rpc reaches it through
//! Agents and may not reach a management owner (methods contract §3.2), so
//! this owner depends on Settings, as PermissionRules does for `permission`.
use crate::{
    methods::ManagementMethods,
    protocol::Empty,
    settings::{
        Read, SettingsError, SettingsService, SourceDeclaration, Update, Writer, holds_write_only,
    },
};
use dim_agent_capabilities::{
    file_storage::FileStorageError,
    methods::{Interruption, MethodCall, MethodDecl, RpcError, params, reply},
};
use dim_agent_runtime::Names;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde::Serialize;
use serde_json::Value;
use std::{path::PathBuf, sync::Arc};

/// The JSON Schema dialect of every declared source's `schema`: the one
/// `Declared::compile` validates with (`settings/declared.rs`).
const DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";

/// The `settings.sources` reply (§4.3): the client-readable declarations.
/// It lists declarations, never documents or file names.
#[derive(Serialize)]
struct SourcesReply {
    /// [`DIALECT`], the dialect the `schema` members are written in.
    dialect: &'static str,
    /// One item per registered declared source, in source-name order.
    sources: Vec<SourceListing>,
}

/// One declared source as a client reads it. A separate wire type, so a
/// field added to [`SourceDeclaration`] cannot leak into the reply by
/// accident.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SourceListing {
    source: String,
    /// The registered schema verbatim: no member is added, removed or
    /// reordered. Display metadata is the schema's own `title` and
    /// `description` annotations, if the owner added them.
    schema: Value,
    /// `["user"]`, or `["user", "project"]` when the source declares a
    /// project file. Never a project root or a file name.
    scopes: Vec<&'static str>,
    /// The registered `userOnly` pointers, in registered order.
    user_only: Vec<String>,
    /// Whether the schema marks a member `writeOnly` through top-level
    /// `properties` recursion only; a client may scan `schema` itself for a
    /// `writeOnly` under `items`, `$defs` or `anyOf`.
    write_only: bool,
}

/// The `settings.projects` reply (§4.3): the project roots this process has
/// authorized, in cwd order. It is the same list the owner-facing `projects`
/// service answers (§4.2); only the transport differs.
#[derive(Serialize)]
struct ProjectsReply {
    /// One item per authorized root. The map only grows for the life of the
    /// process, so the list only grows too.
    projects: Vec<ProjectListing>,
}

/// One authorized project root as a client reads it. A separate wire type, so
/// a field added to the authorization event cannot leak into the reply.
#[derive(Serialize)]
struct ProjectListing {
    /// The number Settings assigned in this process, taken verbatim as
    /// `{project: root}` (§4.1). It is not a durable identity.
    root: usize,
    /// The canonical working directory the root stands for.
    cwd: PathBuf,
}

impl From<SourceDeclaration> for SourceListing {
    fn from(declaration: SourceDeclaration) -> Self {
        let write_only = holds_write_only(&declaration.schema);
        let mut scopes = vec!["user"];
        if declaration.project_file.is_some() {
            scopes.push("project");
        }
        Self {
            source: declaration.source,
            schema: declaration.schema,
            scopes,
            user_only: declaration.user_only,
            write_only,
        }
    }
}

/// The client path's wire errors (§4.3).
fn wire(error: SettingsError) -> RpcError {
    let kind = match &error {
        SettingsError::InvalidScope(_)
        | SettingsError::Document(_)
        | SettingsError::DocumentShape { .. } => {
            return RpcError::invalid(error.to_string());
        }
        SettingsError::Closed => return RpcError::reloading(),
        SettingsError::Cancelled => "operation_interrupted",
        SettingsError::UnknownSource => "settings_unknown_source",
        SettingsError::Conflict => "settings_conflict",
        SettingsError::Storage(FileStorageError::Unsynced(_)) => "settings_unsynced",
        _ => "settings_failed",
    };
    RpcError::app(-32000, kind, error.to_string())
}

/// Process owner of the `settings` management namespace. A client write may
/// change user-only fields.
pub struct SettingsMethodsPlugin {
    service: Key<SettingsService>,
    methods: Key<ManagementMethods>,
}
impl SettingsMethodsPlugin {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            service: names.key()?,
            methods: names.key()?,
        })
    }
}
impl DynPlugin for SettingsMethodsPlugin {
    fn name(&self) -> &str {
        "SettingsMethods"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.service.erase(), self.methods.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let service = ctx.require_at(&self.service)?;
            let methods = ctx.require_at(&self.methods)?;
            methods.register(
                ctx,
                "settings",
                vec![
                    MethodDecl::new("settings.read", false, Interruption::Report),
                    MethodDecl::new("settings.update", true, Interruption::Report),
                    MethodDecl::new("settings.sources", false, Interruption::Report),
                    MethodDecl::new("settings.projects", false, Interruption::Report),
                ],
                Arc::new(move |call, stopping, _| {
                    let MethodCall {
                        method,
                        params: value,
                        ..
                    } = call;
                    let service = service.clone();
                    Box::pin(async move {
                        match method.as_str() {
                            "settings.read" => {
                                let Read { source, scope } = params(value)?;
                                reply(
                                    service
                                        .read_declared(&source, scope, &stopping)
                                        .await
                                        .map_err(wire)?,
                                )
                            }
                            "settings.update" => {
                                let update: Update = params(value)?;
                                reply(
                                    service
                                        .update_declared(
                                            &update.source,
                                            update.scope,
                                            &update.expected_revision,
                                            update.document,
                                            Writer::Client,
                                            &stopping,
                                        )
                                        .await
                                        .map_err(wire)?,
                                )
                            }
                            "settings.sources" => {
                                let _: Empty = params(value)?;
                                reply(SourcesReply {
                                    dialect: DIALECT,
                                    sources: service
                                        .declarations()
                                        .into_iter()
                                        .map(SourceListing::from)
                                        .collect(),
                                })
                            }
                            "settings.projects" => {
                                let _: Empty = params(value)?;
                                reply(ProjectsReply {
                                    projects: service
                                        .project_sources()
                                        .into_iter()
                                        .map(|(root, cwd)| ProjectListing { root, cwd })
                                        .collect(),
                                })
                            }
                            _ => unreachable!("registered settings management method"),
                        }
                    })
                }),
            )?;
            Ok(Effect::Done)
        })
    }
}
