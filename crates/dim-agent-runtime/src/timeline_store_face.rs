use crate::{AgentError, QueryPage, QuerySpec, TimelineKey, TimelineReadScope, TimelineStore};
use crate::{BoxFuture, PublishCallback, SessionConfig, StoredBranch, StoredEvent, TimelineTrace};
use dim_agent_types::RecoverySnapshot;
use rutis::Disposer;
use rutis::{Ctx, Event};
use rutis_bridge::session::{Error, HostDispatch, Reply, Value};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::sync::{Arc, Mutex, OnceLock};

/// Shared native and data binding for one Timeline backend.
/// Clone this binding across Agents using the same provider runtime.
/// Other runtimes may use read operations. Completion listeners must belong to
/// the installed provider runtime; sharing this binding does not bridge runtime buses.
#[derive(Clone)]
pub struct TimelineStoreBinding(Arc<Binding>);
struct Binding {
    backend: Arc<dyn TimelineStore>,
    ctx: OnceLock<Ctx>,
    channel: String,
    subscriptions: Arc<Mutex<HashMap<String, Disposer>>>,
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Completion {
    channel: String,
    key: TimelineKey,
    batch_id: String,
    through: u64,
    result: Result<(), CommitFailure>,
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct CommitFailure {
    code: String,
    message: String,
}
impl From<AgentError> for CommitFailure {
    fn from(error: AgentError) -> Self {
        Self {
            code: error_code(&error).into(),
            message: error.to_string(),
        }
    }
}
impl Event for Completion {
    const NAME: &'static str = "";
    type Value = ();
    fn name(&self) -> &str {
        &self.channel
    }
}
impl TimelineStoreBinding {
    /// Creates the one binding shared by all consumers of this backend.
    /// Providers must call [`Self::install`] before exposing or invoking its
    /// `HostDispatch`; `AgentBuilder` installs it before providing Session services.
    pub fn new(backend: Arc<dyn TimelineStore>) -> Arc<Self> {
        Arc::new(Self(Arc::new(Binding {
            backend,
            ctx: OnceLock::new(),
            channel: format!("timelineStore/commits/{}", uuid::Uuid::new_v4()),
            subscriptions: Arc::default(),
        })))
    }
    /// Installs the storage owner context once; Agent assembly only supplies a root
    /// context when the owner has not already installed one.
    pub fn install(&self, ctx: &Ctx) {
        self.0.ctx.get_or_init(|| ctx.clone());
    }
    fn append_with_id(
        &self,
        key: TimelineKey,
        batch_id: String,
        events: Vec<StoredEvent>,
        publish: PublishCallback,
    ) -> crate::Result<()> {
        let Some(through) = events.last().map(|record| record.event.sequence) else {
            return self.0.backend.append(key, events, publish);
        };
        let owner = self.clone();
        self.0.backend.append(
            key.clone(),
            events,
            Box::new(move |result| {
                struct Emit<'a>(&'a Ctx, Option<Completion>);
                impl Drop for Emit<'_> {
                    fn drop(&mut self) {
                        if let Some(completion) = self.1.take()
                            && let Err(error) = self.0.emit(completion)
                        {
                            tracing::warn!(%error, "failed to emit Timeline storage completion");
                        }
                    }
                }
                let _emit = Emit(
                    owner
                        .0
                        .ctx
                        .get()
                        .expect("Timeline binding is installed before subscription"),
                    Some(Completion {
                        channel: owner.0.channel.clone(),
                        key,
                        batch_id,
                        through,
                        result: result.clone().map_err(CommitFailure::from),
                    }),
                );
                publish(result);
            }),
        )
    }
}

impl TimelineStore for TimelineStoreBinding {
    fn query<'a>(
        &'a self,
        scope: &'a TimelineReadScope,
        query: &'a QuerySpec,
        page: Option<&'a QueryPage>,
    ) -> BoxFuture<'a, crate::Result<crate::QueryResult>> {
        self.0.backend.query(scope, query, page)
    }
    fn create_session<'a>(
        &'a self,
        session_id: &'a str,
        config: &'a SessionConfig,
    ) -> BoxFuture<'a, crate::Result<()>> {
        self.0.backend.create_session(session_id, config)
    }
    fn rollback_session<'a>(&'a self, session_id: &'a str) -> BoxFuture<'a, crate::Result<()>> {
        self.0.backend.rollback_session(session_id)
    }
    fn release_session<'a>(&'a self, session_id: &'a str) -> BoxFuture<'a, crate::Result<()>> {
        self.0.backend.release_session(session_id)
    }
    fn session_config<'a>(
        &'a self,
        session_id: &'a str,
    ) -> BoxFuture<'a, crate::Result<Option<SessionConfig>>> {
        self.0.backend.session_config(session_id)
    }
    fn branches<'a>(
        &'a self,
        session_id: &'a str,
    ) -> BoxFuture<'a, crate::Result<Vec<StoredBranch>>> {
        self.0.backend.branches(session_id)
    }
    fn head<'a>(&'a self, key: &'a TimelineKey) -> BoxFuture<'a, crate::Result<u64>> {
        self.0.backend.head(key)
    }
    fn append(
        &self,
        key: TimelineKey,
        events: Vec<StoredEvent>,
        publish: PublishCallback,
    ) -> crate::Result<()> {
        if self.0.subscriptions.lock().unwrap().is_empty() {
            return self.0.backend.append(key, events, publish);
        }
        let Some(through) = events.last().map(|record| record.event.sequence) else {
            return self.0.backend.append(key, events, publish);
        };
        let batch_id = json!([key.branch_id, through]).to_string();
        self.append_with_id(key, batch_id, events, publish)
    }
    fn batch_end<'a>(
        &'a self,
        key: &'a TimelineKey,
        sequence: u64,
    ) -> BoxFuture<'a, crate::Result<()>> {
        self.0.backend.batch_end(key, sequence)
    }
    fn batch_end_at<'a>(
        &'a self,
        key: &'a TimelineKey,
        sequence: u64,
    ) -> BoxFuture<'a, crate::Result<u64>> {
        self.0.backend.batch_end_at(key, sequence)
    }
    fn read<'a>(
        &'a self,
        key: &'a TimelineKey,
        range: RangeInclusive<u64>,
    ) -> BoxFuture<'a, crate::Result<Vec<StoredEvent>>> {
        self.0.backend.read(key, range)
    }
    fn read_page<'a>(
        &'a self,
        key: &'a TimelineKey,
        after: u64,
        limit: usize,
        through: u64,
    ) -> BoxFuture<'a, crate::Result<Vec<StoredEvent>>> {
        self.0.backend.read_page(key, after, limit, through)
    }
    fn read_segments<'a>(
        &'a self,
        session_id: &'a str,
        segments: Vec<(String, RangeInclusive<u64>)>,
    ) -> BoxFuture<'a, crate::Result<Vec<Vec<StoredEvent>>>> {
        self.0.backend.read_segments(session_id, segments)
    }
    fn trace<'a>(
        &'a self,
        key: &'a TimelineKey,
        sequence: u64,
    ) -> BoxFuture<'a, crate::Result<Option<TimelineTrace>>> {
        self.0.backend.trace(key, sequence)
    }
    fn save_snapshot<'a>(
        &'a self,
        key: &'a TimelineKey,
        snapshot: RecoverySnapshot,
    ) -> BoxFuture<'a, crate::Result<()>> {
        self.0.backend.save_snapshot(key, snapshot)
    }
    fn latest_snapshot<'a>(
        &'a self,
        key: &'a TimelineKey,
        through: u64,
        interpreter: &'a str,
    ) -> BoxFuture<'a, crate::Result<Option<RecoverySnapshot>>> {
        self.0.backend.latest_snapshot(key, through, interpreter)
    }
}

#[derive(Deserialize)]
#[serde(tag = "method", content = "request", rename_all = "camelCase")]
enum Request {
    SessionConfig(Session),
    CreateSession(Box<Create>),
    ReleaseSession(Session),
    RollbackSession(Session),
    SaveSnapshot(SaveSnapshot),
    Branches(Session),
    Head(TimelineKey),
    ReadPage(Page),
    ReadSegments(Segments),
    Query(Box<Query>),
    Trace(Position),
    BatchEnd(Position),
    BatchEndAt(Position),
    LatestSnapshot(Snapshot),
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Session {
    session_id: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Create {
    session_id: String,
    config: SessionConfig,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SaveSnapshot {
    session_id: String,
    branch_id: String,
    snapshot: RecoverySnapshot,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Append {
    session_id: String,
    branch_id: String,
    batch_id: String,
    events: Vec<StoredEvent>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Close {
    subscription_id: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Page {
    session_id: String,
    branch_id: String,
    after: u64,
    limit: usize,
    through: u64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Segments {
    session_id: String,
    segments: Vec<Segment>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Segment {
    branch_id: String,
    from: u64,
    through: u64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Query {
    scope: TimelineReadScope,
    query: QuerySpec,
    page: Option<QueryPage>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Position {
    session_id: String,
    branch_id: String,
    sequence: u64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Snapshot {
    session_id: String,
    branch_id: String,
    through: u64,
    interpreter: String,
}

impl HostDispatch for TimelineStoreBinding {
    fn methods(&self) -> Option<serde_json::Value> {
        Some(json!({
            "append":"sync", "commits":"async", "closeCommits":"async",
            "createSession":"async", "releaseSession":"async", "rollbackSession":"async", "saveSnapshot":"async",
            "sessionConfig":"async", "branches":"async", "head":"async",
            "readPage":"async", "readSegments":"async", "query":"async",
            "trace":"async", "batchEnd":"async", "batchEndAt":"async",
            "latestSnapshot":"async"
        }))
    }
    fn invoke(&self, method: &str, args: Value) -> Reply {
        if method == "commits" {
            let caller = rutis_bridge::session::caller_row()
                .map(|row| row.ctx().clone())
                .ok_or_else(|| Error::Value("commits needs the calling row's context".into()))?;
            let [request, listener]: [Value; 2] = args
                .list()?
                .try_into()
                .map_err(|_| Error::Value("commits expects a request and listener".into()))?;
            let request = request.json()?;
            if !request.is_object() {
                return Err(Error::Value(
                    "timelineStore request must be an object".into(),
                ));
            }
            let request: Session =
                serde_json::from_value(request).map_err(|error| Error::Value(error.to_string()))?;
            let listener = listener.reference()?;
            let owner = self.clone();
            return Ok(Value::future(async move {
                let id = uuid::Uuid::new_v4().to_string();
                let subscription_id = id.clone();
                let disposer = caller.listen::<Completion>().name(owner.0.channel.clone()).on(move |completion, _| {
                    let listener = listener.clone();
                    let subscription_id = subscription_id.clone();
                    let matches = completion.key.session_id == request.session_id;
                    async move {
                        if matches {
                            let result = match &completion.result {
                                Ok(()) => json!("committed"),
                                Err(error) => json!(error),
                            };
                            let payload = json!({"subscriptionId":subscription_id,"batchId":completion.batch_id,
                                "branchId":completion.key.branch_id,"through":completion.through,"result":result});
                            if let Err(error) = listener.call_async(Value::List(vec![payload.into()])).await {
                                tracing::warn!(%error, "Timeline completion listener failed");
                            }
                        }
                        Ok(None)
                    }
                }).map_err(|error| Error::Value(error.to_string()))?;
                owner
                    .0
                    .subscriptions
                    .lock()
                    .unwrap()
                    .insert(id.clone(), disposer);
                let subscriptions = Arc::downgrade(&owner.0.subscriptions);
                let registered = id.clone();
                if let Err(error) = caller.effect(move || async move {
                    if let Some(subscriptions) = subscriptions.upgrade() {
                        subscriptions.lock().unwrap().remove(&registered);
                    }
                    Ok(())
                }) {
                    let disposer = owner.0.subscriptions.lock().unwrap().remove(&id);
                    if let Some(disposer) = disposer {
                        let _ = disposer.dispose().await;
                    }
                    return Err(Error::Value(error.to_string()));
                }
                Ok(json!({"subscriptionId":id}).into())
            }));
        }
        let [request]: [serde_json::Value; 1] = serde_json::from_value(args.json()?)
            .map_err(|error| Error::Value(error.to_string()))?;
        if !request.is_object() {
            return Err(Error::Value(
                "timelineStore request must be an object".into(),
            ));
        }
        if method == "append" {
            if rutis_bridge::session::caller().is_some() {
                return Err(failure(AgentError::InvalidInput(
                    "Timeline append must run beside its state owner".into(),
                )));
            }
            let p: Append =
                serde_json::from_value(request).map_err(|error| Error::Value(error.to_string()))?;
            if p.events.is_empty() {
                return Err(failure(AgentError::InvalidInput(
                    "timeline batch must not be empty".into(),
                )));
            }
            self.append_with_id(
                TimelineKey {
                    session_id: p.session_id,
                    branch_id: p.branch_id,
                },
                p.batch_id,
                p.events,
                Box::new(|_| {}),
            )
            .map_err(failure)?;
            return Ok(json!({"accepted":true}).into());
        }
        if method == "closeCommits" {
            let p: Close =
                serde_json::from_value(request).map_err(|error| Error::Value(error.to_string()))?;
            let disposer = self
                .0
                .subscriptions
                .lock()
                .unwrap()
                .remove(&p.subscription_id);
            return Ok(Value::future(async move {
                if let Some(disposer) = disposer {
                    disposer
                        .dispose()
                        .await
                        .map_err(|error| Error::Value(error.to_string()))?;
                }
                Ok(json!({}).into())
            }));
        }
        let request: Request = serde_json::from_value(json!({"method":method,"request":request}))
            .map_err(|error| Error::Value(error.to_string()))?;
        let store = self.clone();
        Ok(Value::future(async move {
            let result: crate::Result<serde_json::Value> = async {
                Ok(match request {
                    Request::CreateSession(p) => { store.create_session(&p.session_id, &p.config).await?; json!({}) },
                    Request::ReleaseSession(p) => { store.release_session(&p.session_id).await?; json!({}) },
                    Request::RollbackSession(p) => { store.rollback_session(&p.session_id).await?; json!({}) },
                    Request::SaveSnapshot(p) => { store.save_snapshot(&TimelineKey {session_id:p.session_id,branch_id:p.branch_id}, p.snapshot).await?; json!({}) },
                    Request::SessionConfig(p) => json!({"config":store.session_config(&p.session_id).await?}),
                    Request::Branches(p) => json!({"branches":store.branches(&p.session_id).await?}),
                    Request::Head(key) => json!({"sequence":store.head(&key).await?}),
                    Request::ReadPage(p) => json!({"events":store.read_page(&TimelineKey {
                        session_id:p.session_id, branch_id:p.branch_id
                    }, p.after, p.limit, p.through).await?}),
                    Request::ReadSegments(p) => json!({"segments":store.read_segments(
                        &p.session_id,
                        p.segments.into_iter().map(|segment| (segment.branch_id, segment.from..=segment.through)).collect()
                    ).await?}),
                    Request::Query(p) => serde_json::to_value(store.query(&p.scope, &p.query, p.page.as_ref()).await?)
                        .map_err(|error| AgentError::Storage(error.to_string()))?,
                    Request::Trace(p) => json!({"trace":store.trace(&TimelineKey {
                        session_id:p.session_id, branch_id:p.branch_id
                    }, p.sequence).await?}),
                    Request::BatchEnd(p) => {
                        store.batch_end(&TimelineKey {session_id:p.session_id, branch_id:p.branch_id}, p.sequence).await?;
                        json!({})
                    }
                    Request::BatchEndAt(p) => json!({"sequence":store.batch_end_at(&TimelineKey {
                        session_id:p.session_id, branch_id:p.branch_id
                    }, p.sequence).await?}),
                    Request::LatestSnapshot(p) => json!({"snapshot":store.latest_snapshot(&TimelineKey {
                        session_id:p.session_id, branch_id:p.branch_id
                    }, p.through, &p.interpreter).await?}),
                })
            }.await;
            result.map(Value::from).map_err(failure)
        }))
    }
}
fn error_code(error: &AgentError) -> &'static str {
    match error {
        AgentError::InvalidInput(_) => "invalid",
        AgentError::Closed(_) => "closed",
        AgentError::Cancelled(_) => "cancelled",
        _ => "storage",
    }
}
fn failure(error: AgentError) -> Error {
    rutis_bridge::exports::failure("StoreError", error_code(&error), error.to_string(), None)
}
