use super::{PreparedWorktree, SessionStore, WorktreeDirectory};
use crate::protocol::SessionOrigin;
use dim_agent_runtime::{AgentError, ReplyLanguage, SessionConfig};
use dim_agent_types::grant::{GrantKind, GrantMode};
use rutis_bridge::session::{Error, HostDispatch, Reply, Value};
use serde::Deserialize;
use serde_json::json;

use super::{WorktreeDirectoryRecord, WorktreeUsage};
use dim_agent_runtime::{QueryPage, QueryResult, QuerySpec, Result, TimelineReadScope};
use dim_agent_types::grant::SessionGrant;
use rutis::host::BoxFuture;
use std::{path::PathBuf, sync::Arc};
use tokio_util::sync::CancellationToken;

/// The Session repository operations of the public `store` face.
#[derive(Clone)]
pub struct Store(pub(super) Arc<SessionStore>);

impl Store {
    pub(crate) fn bind_timeline(&self, builder: &mut dim_agent_runtime::AgentBuilder) {
        builder.timeline_store(self.0.timeline_store());
    }

    pub fn session_config(
        &self,
        session_id: &str,
    ) -> impl Future<Output = Result<Option<SessionConfig>>> {
        self.0.session_config(session_id)
    }
    pub fn query(
        &self,
        scope: &TimelineReadScope,
        query: &QuerySpec,
        page: Option<&QueryPage>,
    ) -> impl Future<Output = Result<QueryResult>> {
        self.0.query(scope, query, page)
    }
    pub fn catalog(
        &self,
        request: crate::protocol::Catalog,
        agent: Option<String>,
        version: u32,
        hosts: Vec<String>,
        closed: Vec<String>,
        cancel: CancellationToken,
    ) -> impl Future<Output = Result<crate::protocol::CatalogResult>> {
        self.0
            .catalog(request, agent, version, hosts, closed, cancel)
    }
    pub fn add_grant(
        &self,
        session_id: &str,
        path: PathBuf,
        kind: GrantKind,
        mode: GrantMode,
    ) -> impl Future<Output = Result<()>> {
        self.0.add_grant(session_id, path, kind, mode)
    }
    pub fn remove_grant(
        &self,
        session_id: &str,
        path: PathBuf,
    ) -> impl Future<Output = Result<()>> {
        self.0.remove_grant(session_id, path)
    }
    pub fn grants(&self, session_id: &str) -> impl Future<Output = Result<Vec<SessionGrant>>> {
        self.0.grants(session_id)
    }
    pub fn set_session_language(
        &self,
        session_id: &str,
        language: Option<ReplyLanguage>,
    ) -> impl Future<Output = Result<()>> {
        self.0.set_session_language(session_id, language)
    }
    pub fn creation_record(
        &self,
        session_id: &str,
    ) -> impl Future<Output = Result<(SessionOrigin, Option<String>)>> {
        self.0.creation_record(session_id)
    }
    pub fn owner(&self, id: &str) -> impl Future<Output = Result<Option<String>>> {
        self.0.owner(id)
    }
    pub fn has_records(&self, id: &str) -> impl Future<Output = Result<bool>> {
        self.0.has_records(id)
    }
    pub fn delete_session(&self, id: &str) -> impl Future<Output = Result<bool>> {
        self.0.delete_session(id)
    }
    pub fn fork_session(
        &self,
        source: &str,
        fork: &str,
        at: u64,
        config: &str,
    ) -> impl Future<Output = Result<()>> {
        self.0.fork_session(source, fork, at, config)
    }
    pub fn session_config_text(
        &self,
        session_id: &str,
    ) -> impl Future<Output = Result<Option<String>>> {
        self.0.session_config_text(session_id)
    }
    #[expect(
        clippy::too_many_arguments,
        reason = "the face mirrors the owner's signature, one parameter per field the Session is created with"
    )]
    pub fn create_owned_session(
        &self,
        session_id: &str,
        config: &SessionConfig,
        agent_id: &str,
        protocol_version: u32,
        leader: Option<&str>,
        origin: SessionOrigin,
        worktree: Option<PreparedWorktree>,
    ) -> impl Future<Output = Result<()>> {
        self.0.create_owned_session(
            session_id,
            config,
            agent_id,
            protocol_version,
            leader,
            origin,
            worktree,
        )
    }
    pub fn worktree_directories(
        &self,
    ) -> impl Future<Output = Result<Vec<WorktreeDirectoryRecord>>> {
        self.0.worktree_directories()
    }
    pub fn sessions_under_workspace(
        &self,
        root: &std::path::Path,
    ) -> impl Future<Output = Result<Vec<String>>> {
        self.0.sessions_under_workspace(root)
    }
    pub fn remove_worktree_directory(
        &self,
        directory: WorktreeDirectory,
    ) -> impl Future<Output = Result<bool>> {
        self.0.remove_worktree_directory(directory)
    }
    pub fn worktree_usages(
        &self,
        directory: WorktreeDirectory,
    ) -> impl Future<Output = Result<Vec<WorktreeUsage>>> {
        self.0.worktree_usages(directory)
    }
    pub fn remove_worktree(
        &self,
        directory: WorktreeDirectory,
        remove: impl FnOnce(PathBuf) -> BoxFuture<'static, Result<()>>,
    ) -> impl Future<Output = Result<bool>> {
        self.0.remove_worktree(directory, remove)
    }
}

#[derive(Deserialize)]
#[serde(tag = "method", content = "request", rename_all = "camelCase")]
enum Request {
    SessionConfig(Session),
    Query(Box<Query>),
    Delete(Session),
    Fork(Fork),
    CreateOwnedSession(Box<Create>),
    AssociateWorktree(dim_agent_capabilities::file_access::WorktreeAssociation),
    RemoveWorktreeDirectory(Directory),
    #[serde(untagged)]
    Rows(SessionRows),
}
#[derive(Deserialize)]
#[serde(tag = "method", content = "request", rename_all = "camelCase")]
pub(super) enum SessionRows {
    AddGrant(Grant),
    RemoveGrant(Path),
    Grants(Session),
    SetSessionLanguage(Language),
    CreationRecord(Session),
    Owner(Session),
    HasRecords(Session),
    SessionConfigText(Session),
    WorktreeDirectories(Empty),
    SessionsUnderWorkspace(Root),
    WorktreeUsages(Directory),
}

impl SessionRows {
    pub(super) async fn execute(self, db: &Arc<super::SqliteStorage>) -> Result<serde_json::Value> {
        Ok(match self {
            Self::AddGrant(p) => {
                db.add_grant(&p.session_id, p.path.into(), p.kind, p.mode)
                    .await?;
                json!({})
            }
            Self::RemoveGrant(p) => {
                db.remove_grant(&p.session_id, p.path.into()).await?;
                json!({})
            }
            Self::Grants(p) => json!({"grants":db.grants(&p.session_id).await?}),
            Self::SetSessionLanguage(p) => {
                db.set_session_language(&p.session_id, p.language).await?;
                json!({})
            }
            Self::CreationRecord(p) => {
                let (origin, leader) = db.creation_record(&p.session_id).await?;
                json!({"origin":origin,"leaderSessionId":leader})
            }
            Self::Owner(p) => json!({"agentId":db.owner(&p.session_id).await?}),
            Self::HasRecords(p) => {
                json!({"exists":db.has_records(&p.session_id).await?})
            }
            Self::SessionConfigText(p) => {
                json!({"configText":db.session_config_text(&p.session_id).await?})
            }
            Self::WorktreeDirectories(_) => {
                let directories: Vec<_> = db
                    .worktree_directories()
                    .await?
                    .into_iter()
                    .map(|entry| {
                        json!({
                            "repository":entry.directory.repository,
                            "root":entry.directory.root,
                            "lastUsedAtMs":entry.last_used_at_ms,
                        })
                    })
                    .collect();
                json!({"directories":directories})
            }
            Self::SessionsUnderWorkspace(p) => {
                let root = std::path::Path::new(&p.root);
                let ids = db.sessions_under_workspace(root).await?;
                json!({"sessionIds":ids})
            }
            Self::WorktreeUsages(p) => {
                let directory = WorktreeDirectory {
                    repository: p.repository.into(),
                    root: p.root.into(),
                };
                let usages: Vec<_> = db
                    .worktree_usages(directory)
                    .await?
                    .into_iter()
                    .map(|entry| json!({"sessionId":entry.session_id,"subtree":entry.subtree}))
                    .collect();
                json!({"usages":usages})
            }
        })
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Empty {}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Session {
    pub(super) session_id: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Path {
    session_id: String,
    path: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Grant {
    session_id: String,
    path: String,
    kind: GrantKind,
    mode: GrantMode,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Language {
    session_id: String,
    #[serde(deserialize_with = "Option::<ReplyLanguage>::deserialize")]
    language: Option<ReplyLanguage>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Fork {
    pub(super) source_session_id: String,
    pub(super) session_id: String,
    pub(super) at: u64,
    pub(super) config_text: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Create {
    pub(super) session_id: String,
    pub(super) config: SessionConfig,
    pub(super) agent_id: String,
    pub(super) protocol_version: u32,
    pub(super) leader_session_id: Option<String>,
    pub(super) origin: SessionOrigin,
    pub(super) worktree: Option<Worktree>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Worktree {
    pub(super) repository: String,
    pub(super) root: String,
    pub(super) subtree: String,
}
impl From<Worktree> for PreparedWorktree {
    fn from(worktree: Worktree) -> Self {
        Self {
            directory: WorktreeDirectory {
                repository: worktree.repository.into(),
                root: worktree.root.into(),
            },
            subtree: worktree.subtree.into(),
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Directory {
    pub(super) repository: String,
    pub(super) root: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Root {
    root: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Query {
    scope: dim_agent_runtime::TimelineReadScope,
    query: dim_agent_runtime::QuerySpec,
    page: Option<dim_agent_runtime::QueryPage>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Catalog {
    pub(super) request: crate::protocol::Catalog,
    pub(super) agent: Option<String>,
    pub(super) version: u32,
    pub(super) hosts: Vec<String>,
    pub(super) closed: Vec<String>,
}

impl SessionStore {
    pub async fn session_config(
        &self,
        session_id: &str,
    ) -> dim_agent_runtime::Result<Option<SessionConfig>> {
        use dim_agent_runtime::TimelineStore;
        self.timeline.session_config(session_id).await
    }
    pub async fn query(
        &self,
        scope: &dim_agent_runtime::TimelineReadScope,
        query: &dim_agent_runtime::QuerySpec,
        page: Option<&dim_agent_runtime::QueryPage>,
    ) -> dim_agent_runtime::Result<dim_agent_runtime::QueryResult> {
        use dim_agent_runtime::TimelineStore;
        self.timeline.query(scope, query, page).await
    }
}

impl HostDispatch for Store {
    fn methods(&self) -> Option<serde_json::Value> {
        Some(json!({
            "sessionConfig":"async", "query":"async", "catalog":"async",
            "addGrant":"async", "removeGrant":"async", "grants":"async",
            "setSessionLanguage":"async", "creationRecord":"async", "owner":"async",
            "hasRecords":"async", "delete":"async", "fork":"async",
            "sessionConfigText":"async", "createOwnedSession":"async",
            "readMetadata":"async", "setTitle":"async","associateWorktree":"async","worktreeDirectories":"async", "sessionsUnderWorkspace":"async",
            "removeWorktreeDirectory":"async", "worktreeUsages":"async", "removeWorktree":"async"
        }))
    }
    fn invoke(&self, method: &str, args: Value) -> Reply {
        if matches!(method, "readMetadata" | "setTitle") {
            return self.title_operation(method, args);
        }
        if method == "removeWorktree" {
            let mut args = args.list()?.into_iter();
            let directory = args
                .next()
                .ok_or_else(|| Error::Value("missing directory".into()))?
                .json()?;
            if !directory.is_object() {
                return Err(Error::Value("store request must be an object".into()));
            }
            let directory: Directory = serde_json::from_value(directory)
                .map_err(|error| Error::Value(error.to_string()))?;
            let remove = args
                .next()
                .ok_or_else(|| Error::Value("missing removal callback".into()))?
                .reference()?;
            if args.next().is_some() {
                return Err(Error::Value("unexpected removeWorktree argument".into()));
            }
            let owner = self.clone();
            return Ok(Value::future(async move {
                let removed = owner
                    .remove_worktree(
                        WorktreeDirectory {
                            repository: directory.repository.into(),
                            root: directory.root.into(),
                        },
                        move |path| {
                            Box::pin(async move {
                                let result = async {
                                    let value = remove
                                        .call_async(Value::List(vec![json!(path).into()]))
                                        .await?;
                                    if let Value::Reference(reference) = value
                                        && reference.is_future()
                                    {
                                        reference.wait_async().await?;
                                    }
                                    Ok::<_, Error>(())
                                }
                                .await;
                                result.map_err(|error| {
                                    let message = match &error {
                                        Error::Remote { message, .. } => message.clone(),
                                        _ => error.to_string(),
                                    };
                                    match error.code() {
                                        Some("invalid") => AgentError::InvalidInput(message),
                                        Some("cancelled") => AgentError::Cancelled(message),
                                        _ => AgentError::Storage(message),
                                    }
                                })
                            })
                        },
                    )
                    .await
                    .map_err(failure)?;
                Ok(json!({"removed":removed}).into())
            }));
        }
        let [request]: [serde_json::Value; 1] = serde_json::from_value(args.json()?)
            .map_err(|error| Error::Value(error.to_string()))?;
        if !request.is_object() {
            return Err(Error::Value("store request must be an object".into()));
        }
        if method == "catalog" {
            let p: Catalog =
                serde_json::from_value(request).map_err(|error| Error::Value(error.to_string()))?;
            let owner = self.clone();
            return Ok(Value::cancellable_future(async move {
                let cancel = tokio_util::sync::CancellationToken::new();
                let _guard = cancel.clone().drop_guard();
                let page = owner
                    .catalog(p.request, p.agent, p.version, p.hosts, p.closed, cancel)
                    .await
                    .map_err(failure)?;
                Ok(serde_json::to_value(page)
                    .map_err(|error| Error::Value(error.to_string()))?
                    .into())
            }));
        }
        let request: Request = serde_json::from_value(json!({"method":method,"request":request}))
            .map_err(|error| Error::Value(error.to_string()))?;
        let association = matches!(&request, Request::AssociateWorktree(_));
        let owner = self.clone();
        Ok(Value::future(async move {
            let result: dim_agent_runtime::Result<serde_json::Value> = async {
                Ok(match request {
                    Request::SessionConfig(p) => {
                        json!({"config": owner.session_config(&p.session_id).await?})
                    }
                    Request::Query(p) => {
                        let page = p.page.as_ref();
                        let result = owner.query(&p.scope, &p.query, page).await?;
                        serde_json::to_value(result)
                            .map_err(|error| AgentError::Storage(error.to_string()))?
                    }
                    Request::Delete(p) => {
                        json!({"removed":owner.delete_session(&p.session_id).await?})
                    }
                    Request::Fork(p) => {
                        owner
                            .fork_session(&p.source_session_id, &p.session_id, p.at, &p.config_text)
                            .await?;
                        json!({})
                    }
                    Request::CreateOwnedSession(p) => {
                        let worktree = p.worktree.map(Into::into);
                        owner
                            .create_owned_session(
                                &p.session_id,
                                &p.config,
                                &p.agent_id,
                                p.protocol_version,
                                p.leader_session_id.as_deref(),
                                p.origin,
                                worktree,
                            )
                            .await?;
                        json!({})
                    }
                    Request::AssociateWorktree(p) => {
                        owner
                            .0
                            .associate_worktree(&p.session_id, p.directory(), p.subtree.into())
                            .await?;
                        json!({})
                    }
                    Request::RemoveWorktreeDirectory(p) => {
                        let removed = owner
                            .remove_worktree_directory(WorktreeDirectory {
                                repository: p.repository.into(),
                                root: p.root.into(),
                            })
                            .await?;
                        json!({"removed":removed})
                    }
                    Request::Rows(rows) => rows.execute(&owner.0.timeline.db).await?,
                })
            }
            .await;
            result.map(Value::from).map_err(|error| {
                if association {
                    association_failure(error)
                } else {
                    failure(error)
                }
            })
        }))
    }
}
pub(super) fn association_failure(error: AgentError) -> Error {
    rutis_bridge::exports::failure("StoreError", "failed", error.to_string(), None)
}

pub(super) fn failure(error: AgentError) -> Error {
    let code = match &error {
        AgentError::InvalidInput(_) => "invalid",
        AgentError::Closed(_) => "closed",
        AgentError::Cancelled(_) => "cancelled",
        _ => "storage",
    };
    rutis_bridge::exports::failure("StoreError", code, error.to_string(), None)
}
