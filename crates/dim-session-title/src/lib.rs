use dim_agent_capabilities::{
    host::{AuxiliaryModelHost, AuxiliaryRequest},
    methods::{Interruption, MethodCall, MethodDecl, RpcError, SessionMethods, reply},
    session_broadcast::{SessionBroadcast, SessionBroadcastEvent, session_channel},
    timeline_tree::TreeCommands,
};
use dim_agent_runtime::{
    AgentBuilder, CollaborationRole, Names, PartRegistry, PartScope, QueryFilter, QueryOperand,
    QueryOutput, QueryPage, QueryPick, QuerySelect, QuerySpec, QueryValue, SessionConfig,
    SessionIdentity, SessionLifecycle, TimelineQuery, TimelineQueryScope,
};
use dim_agent_types::{
    ContentBlock, EventKind, InputBlock, Message, ModelId, Role, UserInput, markup::runtime_context,
};
use dim_app_server::{
    settings::{SettingsScope, SettingsService, SourceDeclaration},
    storage::{MetadataError, SessionStore, Title, TitleSource},
};
use regex::Regex;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis_bridge::session::{Value as ServiceValue, arg, host_key, settle};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    num::NonZeroU64,
    sync::{Arc, LazyLock, Mutex},
    time::{Duration, Instant},
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

const SOURCE: &str = "session-title";

/// Installs SessionTitle in a host profile that provides Settings and SessionStore.
pub fn install(builder: &mut AgentBuilder) {
    builder.plugin(|names| Ok(Box::new(SessionTitlePlugin::new(names)?)));
}
pub const GENERATOR: &str = "sessionTitleGenerator";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GenerateTitleRequest {
    pub provider_id: String,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum GenerateTitleReply {
    Generated { title: String },
    Unavailable { reason: TitleUnavailableReason },
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TitleUnavailableReason {
    UnsupportedProvider,
    AccountUnavailable,
    Disabled,
    NotEntitled,
    QuotaExhausted,
    ServiceReloading,
}

const SYSTEM: &str = "Write a short title for this conversation: at most 8 words, in the language of the user's message, no quotes, no trailing punctuation. Reply with the title only.";
static PUNCTUATION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\p{Quotation_Mark}+|\p{P}+$").expect("valid title punctuation expression")
});
static SPACE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[\s\p{Cc}\u{FEFF}]+").expect("valid title whitespace expression")
});

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Document {
    enabled: bool,
    max_length: usize,
    model: Option<ModelId>,
}

pub(crate) struct SessionTitlePlugin {
    settings: Key<SettingsService>,
    parts: Key<PartRegistry>,
}
impl SessionTitlePlugin {
    pub(crate) fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            settings: names.key()?,
            parts: names.key()?,
        })
    }
}
impl DynPlugin for SessionTitlePlugin {
    fn name(&self) -> &str {
        SOURCE
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.settings.erase(), self.parts.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            ctx.require_at(&self.settings)?.register_declared(
                ctx,
                SourceDeclaration {
                    source: SOURCE.into(),
                    user_file: "session-title.json".into(),
                    project_file: None,
                    user_only: vec![],
                    schema: json!({
                        "type":"object", "additionalProperties":false,
                        "properties":{
                            "enabled":{"type":"boolean","default":true},
                            "maxLength":{"type":"integer","minimum":10,"maximum":200,"default":80},
                            "model":{"default":null,"anyOf":[{"type":"null"},{
                                "type":"object","required":["providerId","modelId"],
                                "additionalProperties":false,"properties":{
                                    "providerId":{"type":"string","minLength":1},
                                    "modelId":{"type":"string","minLength":1}
                                }
                            }]}
                        }
                    }),
                },
            )?;
            ctx.require_at(&self.parts)?.register(
                ctx,
                PartScope::Session,
                "SessionTitle",
                |mount| Ok(Some(Box::new(SessionTitle::new(mount.names)?))),
            )?;
            Ok(Effect::Done)
        })
    }
}

struct SessionTitle {
    settings: Key<SettingsService>,
    tree: Key<TreeCommands>,
    store: Key<SessionStore>,
    auxiliary: Key<AuxiliaryModelHost>,
    identity: Key<SessionIdentity>,
    config: Key<SessionConfig>,
    lifecycle: Key<SessionLifecycle>,
    methods: Key<SessionMethods>,
}
impl SessionTitle {
    fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            settings: names.key()?,
            tree: names.key()?,
            store: names.key()?,
            auxiliary: names.key()?,
            identity: names.key()?,
            config: names.key()?,
            lifecycle: names.key()?,
            methods: names.key()?,
        })
    }
}
impl DynPlugin for SessionTitle {
    fn name(&self) -> &str {
        "SessionTitle"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.settings.erase(),
            self.tree.erase(),
            self.store.erase(),
            self.auxiliary.erase(),
            self.identity.erase(),
            self.config.erase(),
            self.lifecycle.erase(),
            self.methods.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let identity = ctx.require_at(&self.identity)?;
            let config = ctx.require_at(&self.config)?;
            let lifecycle = ctx.require_at(&self.lifecycle)?;
            let owner = Arc::new(Owner {
                ctx: ctx.clone(),
                id: identity.session_id.clone(),
                settings: ctx.require_at(&self.settings)?,
                tree: ctx.require_at(&self.tree)?,
                store: ctx.require_at(&self.store)?,
                auxiliary: ctx.require_at(&self.auxiliary)?,
                state: Mutex::new(State::default()),
                stop: CancellationToken::new(),
                tasks: TaskTracker::new(),
            });
            let cleanup = owner.clone();
            ctx.effect(move || async move {
                cleanup.stop.cancel();
                cleanup.tasks.close();
                cleanup.tasks.wait().await;
                Ok(())
            })?;
            let closing = owner.clone();
            let life = lifecycle.clone();
            owner.tasks.spawn(async move {
                tokio::select! {
                    _ = life.closing() => closing.stop.cancel(),
                    _ = closing.stop.cancelled() => {},
                }
            });
            if let Some(requested) = config.initial_title.clone() {
                let writing = owner.clone();
                let published = lifecycle.clone();
                owner.tasks.spawn(async move {
                    tokio::select! {
                        active = published.activated() => {
                            if active { writing.initial(requested).await; }
                        }
                        _ = writing.stop.cancelled() => {},
                    }
                });
            }
            if config.interactive && config.collaboration_role == CollaborationRole::Leader {
                let observed = owner.clone();
                ctx.listen::<SessionBroadcast>()
                    .name(session_channel(&owner.id))
                    .on(move |record, _| {
                        if let SessionBroadcastEvent::Event { branch_id, event } = &record.record
                            && branch_id == "main"
                            && let EventKind::InputApplied(applied) = &event.kind
                        {
                            let state = observed.state.lock().unwrap();
                            if !observed.stop.is_cancelled()
                                && !state.generating
                                && !state.no_text
                                && state.failures < 3
                            {
                                let owner = observed.clone();
                                let lifecycle = lifecycle.clone();
                                let (sequence, id, epoch) =
                                    (event.sequence, applied.input_id.clone(), state.epoch);
                                observed.tasks.spawn(async move {
                                    tokio::select! {
                                        active = lifecycle.activated() => {
                                            if active { owner.attempt(sequence, id, epoch).await; }
                                        },
                                        _ = owner.stop.cancelled() => {},
                                    }
                                });
                            }
                        }
                        async { Ok(None) }
                    })?;
            }
            ctx.require_at(&self.methods)?.register(
                ctx,
                "session.rename",
                vec![MethodDecl::new(
                    "session.rename",
                    true,
                    Interruption::Report,
                )],
                Arc::new(move |call, cancel, _| {
                    let MethodCall { method, params, .. } = call;
                    let owner = owner.clone();
                    Box::pin(async move {
                        if method != "session.rename" {
                            return Err(RpcError::invalid("Invalid rename method"));
                        }
                        owner.rename(params, cancel).await
                    })
                }),
            )?;
            Ok(Effect::Done)
        })
    }
}

#[derive(Default)]
struct State {
    epoch: u64,
    generating: bool,
    failures: usize,
    no_text: bool,
}
struct Owner {
    ctx: Ctx,
    id: String,
    settings: Arc<SettingsService>,
    tree: Arc<TreeCommands>,
    store: Arc<SessionStore>,
    auxiliary: Arc<AuxiliaryModelHost>,
    state: Mutex<State>,
    stop: CancellationToken,
    tasks: TaskTracker,
}
impl Owner {
    fn current(&self, epoch: u64) -> bool {
        !self.stop.is_cancelled() && self.state.lock().unwrap().epoch == epoch
    }
    async fn document(&self) -> Result<Document, ()> {
        let snapshot = self
            .settings
            .read_declared(SOURCE, SettingsScope::User, &self.stop)
            .await
            .map_err(|_| ())?;
        serde_json::from_value(snapshot.document).map_err(|_| ())
    }
    /// Writes the title the Session's creation requested (#1421): only while
    /// nothing has titled it, under rename's text rule and the operator's
    /// `maxLength`, as the first manual title. A `Conflict` means another
    /// writer titled the Session first, and that title wins.
    async fn initial(&self, requested: String) {
        let stored = self.store.metadata(&self.id).await;
        let Ok(stored) = stored else {
            tracing::info!(target: "session_title", outcome = "initial_failed", "the title record is unreadable");
            return;
        };
        if stored.revision != 0 || self.stop.is_cancelled() {
            return;
        }
        let document = self.document().await;
        let Ok(document) = document else {
            tracing::info!(target: "session_title", outcome = "initial_failed", "the title settings are unreadable");
            return;
        };
        let Some(text) = requested_title(&requested, document.max_length) else {
            tracing::info!(target: "session_title", outcome = "initial_skipped", "the requested title has no text");
            return;
        };
        let title = Title {
            text,
            source: TitleSource::Manual,
        };
        match self.store.set_title(&self.id, 0, Some(title)).await {
            Ok(_) | Err(MetadataError::Conflict) => {}
            Err(error) => {
                tracing::info!(target: "session_title", outcome = "initial_failed", %error)
            }
        }
    }
    async fn attempt(&self, sequence: u64, input_id: String, epoch: u64) {
        let started = Instant::now();
        let mut entered = false;
        let result: Result<&str, ()> = async {
            let document = self.document().await;
            if !self.current(epoch) || document.as_ref().is_ok_and(|d| !d.enabled) { return Ok("skipped"); }
            let stored = self.store.metadata(&self.id).await;
            if !self.current(epoch) || stored.as_ref().is_ok_and(|m| m.title.is_some()) { return Ok("skipped"); }
            let kind = |value: &str| QueryFilter::In {
                value: QueryOperand::Field { path: "kind".into() }, values: vec![value.into()],
            };
            let field = |path: &str, value: Value| QueryFilter::In {
                value: QueryOperand::Field { path: path.into() }, values: vec![value],
            };
            let request = |through, filter, select, pick| TimelineQuery {
                scope: TimelineQueryScope::Branch, from: None, through, include_publishing: false,
                query: QuerySpec { filter: Some(filter), select, group_by: vec![], pick, order_by: vec![] },
                page: Some(QueryPage { limit: NonZeroU64::MIN, after: None }),
            };
            let operator = self.tree.query("main".into(), request(None, QueryFilter::And {
                conditions: vec![kind("input/message"), field("id", input_id.into()), eligible_input()],
            }, QuerySelect::Fields { fields: vec!["id".into()] }, Some(QueryPick::First))).await.map_err(|_| ())?;
            let through = operator.through;
            if through < sequence { return Err(()); }
            let QueryOutput::Rows { rows } = operator.result else { return Err(()); };
            if rows.is_empty() { return Ok("skipped"); }
            let first = self.tree.query("main".into(), request(Some(through), QueryFilter::And {
                conditions: vec![kind("input/applied"), QueryFilter::Exists {
                    condition: Box::new(QueryFilter::And { conditions: vec![
                        kind("input/message"),
                        QueryFilter::Compare {
                            left: QueryOperand::Field { path: "id".into() },
                            comparison: dim_agent_runtime::QueryComparison::Eq,
                            right: QueryOperand::Outer { path: "data.input_id".into() },
                        },
                        eligible_input(),
                    ] }),
                }],
            }, QuerySelect::Fields { fields: vec!["data.input_id".into()] }, Some(QueryPick::First))).await.map_err(|_| ())?;
            let QueryOutput::Rows { rows } = first.result else { return Err(()); };
            let Some(first) = rows.first() else { return Ok("skipped"); };
            let [QueryValue::Value { value: Value::String(first_id) }] = first.values.as_slice() else { return Err(()); };
            let input = self.tree.query("main".into(), request(Some(through), QueryFilter::And {
                conditions: vec![kind("input/message"), field("id", first_id.clone().into())],
            }, QuerySelect::Fields { fields: vec!["data.user".into(), "data.blocks".into()] }, Some(QueryPick::First))).await.map_err(|_| ())?;
            let QueryOutput::Rows { rows } = input.result else { return Err(()); };
            let Some(row) = rows.first() else { return Err(()); };
            let [user, blocks] = row.values.as_slice() else { return Err(()); };
            let source = match (user, blocks) {
                (QueryValue::Value { value: user }, _) => {
                    let input: UserInput = serde_json::from_value(user.clone()).map_err(|_| ())?;
                    input.content.iter().enumerate().filter_map(|(index, block)| match block {
                        ContentBlock::Text { text } if !runtime_context(index, text) => Some(text.as_str()),
                        _ => None,
                    }).collect::<Vec<_>>().join("\n")
                }
                (_, QueryValue::Value { value: blocks }) => goal_objective(blocks).ok_or(())?,
                _ => return Ok("skipped"),
            };
            let configured = self.tree.query("main".into(), request(Some(through), kind("context/configured"),
                QuerySelect::Fields { fields: vec!["data.settings.providerId".into(), "data.settings.modelId".into()] },
                Some(QueryPick::Last))).await.map_err(|_| ())?;
            let QueryOutput::Rows { rows } = configured.result else { return Err(()); };
            let model = match rows.first() {
                None => None,
                Some(row) => {
                    let [QueryValue::Value { value: Value::String(provider_id) }, QueryValue::Value { value: Value::String(model_id) }] = row.values.as_slice() else { return Err(()); };
                    Some(ModelId { provider_id: provider_id.clone(), model_id: model_id.clone() })
                }
            };
            if !self.current(epoch) { return Ok("skipped"); }
            {
                let mut state = self.state.lock().unwrap();
                if state.epoch != epoch || self.stop.is_cancelled() { return Ok("skipped"); }
                state.epoch += 1;
                state.generating = true;
            }
            entered = true;
            let document = document?;
            let stored = stored.map_err(|_| ())?;
            let text: String = source.chars().take(2000).collect();
            if text.trim_matches(space).is_empty() {
                self.state.lock().unwrap().no_text = true;
                return Ok("skipped");
            }
            let cancel = self.stop.child_token();
            let call = self.generate(document.model, model, &text, document.max_length, cancel.clone());
            tokio::pin!(call);
            let completed = tokio::select! {
                result = &mut call => result,
                _ = tokio::time::sleep(Duration::from_secs(30)) => {
                    cancel.cancel(); let _ = call.await;
                    Ok(fallback_title(&text, document.max_length))
                },
                _ = self.stop.cancelled() => { cancel.cancel(); let _ = call.await; return Ok("skipped"); },
            };
            if self.stop.is_cancelled() { return Ok("skipped"); }
            let text = completed?;
            if text.is_empty() { return Err(()); }
            match self.store.set_title(&self.id, stored.revision, Some(Title { text, source: TitleSource::Auto })).await {
                Ok(_) => Ok("completed"),
                Err(MetadataError::Storage(_)) => Err(()),
                Err(_) => Ok("discarded"),
            }
        }.await;
        if entered {
            let mut state = self.state.lock().unwrap();
            state.generating = false;
            if result.is_err() && !self.stop.is_cancelled() {
                state.failures += 1;
            }
        }
        if !self.stop.is_cancelled() && (entered || result.is_err()) {
            tracing::info!(target: "session_title", outcome = result.unwrap_or("failed"), duration_ms = started.elapsed().as_millis() as u64);
        }
    }
    async fn generate(
        &self,
        explicit: Option<ModelId>,
        adopted: Option<ModelId>,
        text: &str,
        max_length: usize,
        cancel: CancellationToken,
    ) -> Result<String, ()> {
        if cancel.is_cancelled() {
            return Err(());
        }
        if explicit.is_none()
            && let Some(model) = &adopted
            && let Some(service) = self.ctx.get_at(&host_key(GENERATOR))
        {
            let request = GenerateTitleRequest {
                provider_id: model.provider_id.clone(),
                text: text.into(),
            };
            let cancelled = cancel.clone();
            let callback = ServiceValue::callback(move |_| {
                let cancelled = cancelled.clone();
                Ok(ServiceValue::cancellable_future(async move {
                    cancelled.cancelled().await;
                    Ok(ServiceValue::Undefined)
                }))
            });
            let reply = async {
                let reply = service.invoke(
                    "generate",
                    ServiceValue::List(vec![arg(&request)?, callback]),
                )?;
                let data = settle(reply).await?.json()?;
                serde_json::from_value::<GenerateTitleReply>(data)
                    .map_err(|error| rutis_bridge::session::Error::Value(error.to_string()))
            }
            .await;
            if cancel.is_cancelled() {
                return Err(());
            }
            match reply {
                Ok(GenerateTitleReply::Generated { title }) => {
                    let title = title_text(&title, max_length);
                    if !title.is_empty() {
                        return Ok(title);
                    }
                }
                Ok(GenerateTitleReply::Cancelled) => return Err(()),
                _ => {}
            }
        }
        if cancel.is_cancelled() {
            return Err(());
        }
        if let Some(model) = explicit.or(adopted) {
            let reply = self
                .auxiliary
                .0
                .complete(
                    model,
                    AuxiliaryRequest {
                        system: Some(SYSTEM.into()),
                        messages: vec![Message::text(Role::User, text)],
                        max_output_tokens: None,
                    },
                    cancel.clone(),
                )
                .await;
            if cancel.is_cancelled() {
                return Err(());
            }
            if let Ok(reply) = reply {
                let title = title_text(&reply.text, max_length);
                if !title.is_empty() {
                    return Ok(title);
                }
            }
        }
        Ok(fallback_title(text, max_length))
    }
    async fn rename(&self, params: Value, cancel: CancellationToken) -> Result<Value, RpcError> {
        if self.stop.is_cancelled() {
            return Err(reloading());
        }
        let text = match params.get("title") {
            Some(Value::Null) => None,
            Some(Value::String(value)) => {
                let text = value.trim_matches(space);
                if text.is_empty()
                    || value
                        .chars()
                        .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
                {
                    return Err(RpcError::invalid("Invalid title or control character"));
                }
                Some(text.to_owned())
            }
            _ => return Err(RpcError::invalid("title must be a string or null")),
        };
        let document = self.document().await;
        if self.stop.is_cancelled() || cancel.is_cancelled() {
            return Err(reloading());
        }
        let document = document
            .map_err(|_| RpcError::app(-32000, "storage_failed", "Cannot read title settings"))?;
        if text
            .as_ref()
            .is_some_and(|text| text.chars().count() > document.max_length)
        {
            return Err(RpcError::invalid("Invalid title length"));
        }
        for _ in 0..=3 {
            let stored = self.store.metadata(&self.id).await;
            if self.stop.is_cancelled() || cancel.is_cancelled() {
                return Err(reloading());
            }
            let stored = stored.map_err(metadata_error)?;
            let title = text.as_ref().map(|text| Title {
                text: text.clone(),
                source: TitleSource::Manual,
            });
            match self.store.set_title(&self.id, stored.revision, title).await {
                Ok(revision) => {
                    return reply(
                        json!({"title":text, "titleSource":text.as_ref().map(|_| "manual"), "revision":revision}),
                    );
                }
                Err(MetadataError::Conflict) => {
                    if self.stop.is_cancelled() || cancel.is_cancelled() {
                        return Err(reloading());
                    }
                }
                Err(error) => return Err(metadata_error(error)),
            }
        }
        Err(RpcError::app(
            -32000,
            "service_reloading",
            "Title changed during rename",
        ))
    }
}

/// The opening of the objective element (Goal.md A.5). Goal writes the
/// objective as an element of its own lines, and the decoder requires the line
/// break; eligibility requires it too, so the two checks cannot disagree.
const OBJECTIVE_OPENING: &str = "<objective>\n";

/// The applied inputs that may title a Session (SessionTitle.md §0.1, A10): an
/// operator `input/message` with `data.user`, or Goal's applied `goal:state`
/// record, which states the objective.
///
/// Goal's key identifies the record, and eligibility requires the same opening
/// `goal_objective` decodes, so a record cannot be selected and then fail to
/// decode. The condition stays narrow on purpose: Goal also writes a retired
/// `goal:state` record with no objective when a goal is cleared, and such a
/// record must not become the first eligible input and block a later title.
fn eligible_input() -> QueryFilter {
    QueryFilter::Or {
        conditions: vec![
            QueryFilter::Present {
                value: QueryOperand::Field {
                    path: "data.user".into(),
                },
            },
            QueryFilter::Any {
                field: "data.blocks".into(),
                condition: Box::new(QueryFilter::And {
                    conditions: vec![
                        QueryFilter::In {
                            value: QueryOperand::Element { path: "key".into() },
                            values: vec!["goal:state".into()],
                        },
                        QueryFilter::Contains {
                            value: QueryOperand::Element {
                                path: "text".into(),
                            },
                            text: OBJECTIVE_OPENING.into(),
                        },
                    ],
                }),
            },
        ],
    }
}

/// The objective an applied Goal state record states (Goal.md A.5): the block
/// keyed `goal:state` whose marker is Goal's and whose body carries the
/// `<objective>` element. Goal owns the format; SessionTitle follows it and
/// owns no second contract (SessionTitle.md A10), and takes no dependency on
/// Goal's implementation.
fn goal_objective(blocks: &Value) -> Option<String> {
    let blocks: Vec<InputBlock> = serde_json::from_value(blocks.clone()).ok()?;
    blocks.iter().find_map(|block| {
        let InputBlock::Raw { text, key, retire } = block;
        if *retire || key.as_deref() != Some("goal:state") {
            return None;
        }
        let rest = text.strip_prefix("<dim-goal ")?;
        let end = rest.find('>')?;
        if !rest[..end]
            .split_whitespace()
            .any(|attribute| attribute == "kind=\"state\"")
        {
            return None;
        }
        let body = &rest[end + 1..];
        let start = body.find(OBJECTIVE_OPENING)? + OBJECTIVE_OPENING.len();
        let end = start + body[start..].find("\n</objective>")?;
        let objective = unescape(&body[start..end]);
        (!objective.trim().is_empty()).then_some(objective)
    })
}

/// The inverse of Goal's `escape` (Goal.md A.5): Goal owns the encoding, and
/// SessionTitle mirrors the inverse here rather than calling Goal.
fn unescape(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&gt;", ">")
        .replace("&lt;", "<")
        .replace("&amp;", "&")
}

fn fallback_title(text: &str, max_length: usize) -> String {
    let text = SPACE.replace_all(text, " ");
    let text: String = text.trim_matches(space).chars().take(max_length).collect();
    text.trim_matches(space).to_owned()
}
/// The title a Session's creation requested (#1421), under `session.rename`'s
/// text rule: no text for an empty one, or one with a control character or a
/// line separator. The length is bounded the way [`fallback_title`] bounds a
/// generated one, and punctuation stays: the text is the caller's own, not a
/// model's reply.
fn requested_title(text: &str, max_length: usize) -> Option<String> {
    if text
        .chars()
        .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
    {
        return None;
    }
    let text: String = text.trim_matches(space).chars().take(max_length).collect();
    let text = text.trim_matches(space);
    (!text.is_empty()).then(|| text.to_owned())
}
fn title_text(text: &str, max_length: usize) -> String {
    let line = text
        .trim_matches(space)
        .split(['\r', '\n', '\u{2028}', '\u{2029}'])
        .next()
        .expect("split has a first line")
        .trim_matches(space);
    let line = PUNCTUATION.replace_all(line, "");
    let line = SPACE.replace_all(&line, " ");
    let text: String = line.trim_matches(space).chars().take(max_length).collect();
    text.trim_matches(space).to_owned()
}
fn space(c: char) -> bool {
    (c.is_whitespace() && c != '\u{0085}') || c == '\u{FEFF}'
}
fn reloading() -> RpcError {
    RpcError::app(
        -32000,
        "service_reloading",
        "SessionTitle is closing or reloading",
    )
}
fn metadata_error(error: MetadataError) -> RpcError {
    RpcError::app(
        -32000,
        if error == MetadataError::NotFound {
            "session_not_found"
        } else {
            "storage_failed"
        },
        error.to_string(),
    )
}
