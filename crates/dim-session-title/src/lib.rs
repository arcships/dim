use dim_agent_capabilities::{
    host::{AuxiliaryModelHost, AuxiliaryRequest},
    methods::{Interruption, MethodCall, MethodDecl, RpcError, SessionMethods, reply},
    session_broadcast::{SessionBroadcast, SessionBroadcastEvent, session_channel},
};
use dim_agent_runtime::{
    AgentBuilder, CollaborationRole, Names, PartRegistry, PartScope, SessionConfig,
    SessionIdentity, SessionLifecycle,
};
use dim_agent_types::{AgentEvent, ContentBlock, EventKind, Message, ModelId, Role, UserInput};
use dim_app_server::{
    settings::{SettingsScope, SettingsService, SourceDeclaration},
    storage::{MetadataError, SessionStore, Title, TitleSource},
};
use regex::Regex;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis_bridge::session::{HostDispatch, Value as ServiceValue, arg, host_key, settle};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
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
    sessions: Key<dyn HostDispatch>,
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
            sessions: host_key("sessions"),
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
            self.sessions.erase(),
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
                sessions: ctx.require_at(&self.sessions)?,
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
    sessions: Arc<dyn HostDispatch>,
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
    async fn attempt(&self, sequence: u64, input_id: String, epoch: u64) {
        let started = Instant::now();
        let mut entered = false;
        let result: Result<&str, ()> = async {
            let document = self.document().await;
            if !self.current(epoch) || document.as_ref().is_ok_and(|d| !d.enabled) { return Ok("skipped"); }
            let stored = self.store.metadata(&self.id).await;
            if !self.current(epoch) || stored.as_ref().is_ok_and(|m| m.title.is_some()) { return Ok("skipped"); }
            let mut first = None;
            let mut model = None;
            let mut operator = false;
            let mut inputs = HashMap::<String, UserInput>::new();
            let mut after = None;
            let mut through = None;
            loop {
                let call = self.sessions.invoke("history", json!([{
                    "sessionId": self.id, "branchId":"main", "after":after, "through":through,
                    "atLeast":through.is_none().then_some(sequence),
                }]).into()).map_err(|_| ())?;
                let mut page = settle(call).await.map_err(|_| ())?.json().map_err(|_| ())?;
                if page["ok"] != true { return Err(()); }
                let page = page.get_mut("value").ok_or(())?;
                let head = page["through"].as_u64().ok_or(())?;
                let events: Vec<AgentEvent> = serde_json::from_value(page["events"].take()).map_err(|_| ())?;
                through = Some(head);
                for event in &events {
                    match &event.kind {
                        EventKind::InputMessage(message) => {
                            if let Some(user) = &message.user {
                                operator |= event.id == input_id;
                                if first.is_none() { inputs.insert(event.id.clone(), user.clone()); }
                            }
                        },
                        EventKind::InputApplied(applied) if first.is_none() => first = inputs.remove(&applied.input_id),
                        EventKind::ContextConfigured(configured) => model = Some(ModelId {
                            provider_id: configured.settings.provider_id.clone(),
                            model_id: configured.settings.model_id.clone(),
                        }),
                        _ => {},
                    }
                }
                let Some(last) = events.last() else { break; };
                after = Some(last.sequence);
                if last.sequence >= head { break; }
            }
            if !self.current(epoch) || !operator { return Ok("skipped"); }
            let Some(first) = first else { return Ok("skipped"); };
            {
                let mut state = self.state.lock().unwrap();
                if state.epoch != epoch || self.stop.is_cancelled() { return Ok("skipped"); }
                state.epoch += 1;
                state.generating = true;
            }
            entered = true;
            let document = document?;
            let stored = stored.map_err(|_| ())?;
            let text = first.content.iter().filter_map(|block| match block {
                ContentBlock::Text { text, .. } => Some(text.as_str()), _ => None,
            }).collect::<Vec<_>>().join("\n").chars().take(2000).collect::<String>();
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
fn fallback_title(text: &str, max_length: usize) -> String {
    let text = SPACE.replace_all(text, " ");
    let text: String = text.trim_matches(space).chars().take(max_length).collect();
    text.trim_matches(space).to_owned()
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
