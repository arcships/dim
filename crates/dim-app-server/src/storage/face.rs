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
    AddGrant(Grant),
    RemoveGrant(Path),
    Grants(Session),
    SetSessionLanguage(Language),
    CreationRecord(Session),
    Owner(Session),
    HasRecords(Session),
    Delete(Session),
    Fork(Fork),
    SessionConfigText(Session),
    CreateOwnedSession(Box<Create>),
    WorktreeDirectories(Empty),
    SessionsUnderWorkspace(Root),
    RemoveWorktreeDirectory(Directory),
    WorktreeUsages(Directory),
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Session {
    session_id: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Path {
    session_id: String,
    path: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Grant {
    session_id: String,
    path: String,
    kind: GrantKind,
    mode: GrantMode,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Language {
    session_id: String,
    #[serde(deserialize_with = "Option::<ReplyLanguage>::deserialize")]
    language: Option<ReplyLanguage>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Fork {
    source_session_id: String,
    session_id: String,
    at: u64,
    config_text: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Create {
    session_id: String,
    config: SessionConfig,
    agent_id: String,
    protocol_version: u32,
    leader_session_id: Option<String>,
    origin: SessionOrigin,
    worktree: Option<Worktree>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Worktree {
    repository: String,
    root: String,
    subtree: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Directory {
    repository: String,
    root: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Root {
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
struct Catalog {
    request: crate::protocol::Catalog,
    agent: Option<String>,
    version: u32,
    hosts: Vec<String>,
    closed: Vec<String>,
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
            "worktreeDirectories":"async", "sessionsUnderWorkspace":"async",
            "removeWorktreeDirectory":"async", "worktreeUsages":"async", "removeWorktree":"async"
        }))
    }
    fn invoke(&self, method: &str, args: Value) -> Reply {
        if method == "removeWorktree" {
            let mut args = args.list()?.into_iter();
            let directory = args
                .next()
                .ok_or_else(|| Error::value("missing directory"))?
                .json()?;
            if !directory.is_object() {
                return Err(Error::value("store request must be an object"));
            }
            let directory: Directory = serde_json::from_value(directory)
                .map_err(|error| Error::value(error.to_string()))?;
            let remove = args
                .next()
                .ok_or_else(|| Error::value("missing removal callback"))?
                .reference()?;
            if args.next().is_some() {
                return Err(Error::value("unexpected removeWorktree argument"));
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
            .map_err(|error| Error::value(error.to_string()))?;
        if !request.is_object() {
            return Err(Error::value("store request must be an object"));
        }
        if method == "catalog" {
            let p: Catalog =
                serde_json::from_value(request).map_err(|error| Error::value(error.to_string()))?;
            let owner = self.clone();
            return Ok(Value::cancellable_future(async move {
                let cancel = tokio_util::sync::CancellationToken::new();
                let _guard = cancel.clone().drop_guard();
                let page = owner
                    .catalog(p.request, p.agent, p.version, p.hosts, p.closed, cancel)
                    .await
                    .map_err(failure)?;
                Ok(serde_json::to_value(page)
                    .map_err(|error| Error::value(error.to_string()))?
                    .into())
            }));
        }
        let request: Request = serde_json::from_value(json!({"method":method,"request":request}))
            .map_err(|error| Error::value(error.to_string()))?;
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
                    Request::AddGrant(p) => {
                        owner
                            .add_grant(&p.session_id, p.path.into(), p.kind, p.mode)
                            .await?;
                        json!({})
                    }
                    Request::RemoveGrant(p) => {
                        owner.remove_grant(&p.session_id, p.path.into()).await?;
                        json!({})
                    }
                    Request::Grants(p) => json!({"grants":owner.grants(&p.session_id).await?}),
                    Request::SetSessionLanguage(p) => {
                        owner
                            .set_session_language(&p.session_id, p.language)
                            .await?;
                        json!({})
                    }
                    Request::CreationRecord(p) => {
                        let (origin, leader) = owner.creation_record(&p.session_id).await?;
                        json!({"origin":origin,"leaderSessionId":leader})
                    }
                    Request::Owner(p) => json!({"agentId":owner.owner(&p.session_id).await?}),
                    Request::HasRecords(p) => {
                        json!({"exists":owner.has_records(&p.session_id).await?})
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
                    Request::SessionConfigText(p) => {
                        json!({"configText":owner.session_config_text(&p.session_id).await?})
                    }
                    Request::CreateOwnedSession(p) => {
                        let worktree = p.worktree.map(|worktree| PreparedWorktree {
                            directory: WorktreeDirectory {
                                repository: worktree.repository.into(),
                                root: worktree.root.into(),
                            },
                            subtree: worktree.subtree.into(),
                        });
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
                    Request::WorktreeDirectories(_) => {
                        let directories: Vec<_> = owner
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
                    Request::SessionsUnderWorkspace(p) => {
                        let root = std::path::Path::new(&p.root);
                        let ids = owner.sessions_under_workspace(root).await?;
                        json!({"sessionIds":ids})
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
                    Request::WorktreeUsages(p) => {
                        let directory = WorktreeDirectory {
                            repository: p.repository.into(),
                            root: p.root.into(),
                        };
                        let usages: Vec<_> = owner
                            .worktree_usages(directory)
                            .await?
                            .into_iter()
                            .map(|entry| {
                                json!({"sessionId":entry.session_id,"subtree":entry.subtree})
                            })
                            .collect();
                        json!({"usages":usages})
                    }
                })
            }
            .await;
            result.map(Value::from).map_err(failure)
        }))
    }
}
fn failure(error: AgentError) -> Error {
    let code = match &error {
        AgentError::InvalidInput(_) => "invalid",
        AgentError::Closed(_) => "closed",
        AgentError::Cancelled(_) => "cancelled",
        _ => "storage",
    };
    rutis_bridge::exports::failure("StoreError", code, error.to_string(), None)
}
