//! Runs the App Server session for the interactive interface on a worker thread.
//!
//! The terminal loop is synchronous; the client is async. The worker owns the
//! connection and the projection, receives commands, and publishes view
//! snapshots (at most one per frame interval, so a fast stream does not clone
//! the transcript per event).
//!
//! Features the protocol v3 App Server does not offer yet (model and provider
//! management, sign-in, usage, approval preferences) answer with an explicit
//! "not available" update instead of failing silently.

use std::sync::mpsc;
use std::time::Duration;

use dim_agent_types::{ApprovalDecision, ModelId};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::args::Options;
use crate::client::{self, Client, Message};
use crate::model::session::{Command, OauthEvent, SessionItem, SessionPort, SwitchKind, Update};
use crate::projection::Projection;
use crate::wire::Event;

const PUBLISH_INTERVAL: Duration = Duration::from_millis(16);
const HISTORY_PAGE: u64 = 256;
const UNAVAILABLE: &str = "not available in this build yet";

pub struct SessionHost {
    commands: UnboundedSender<Command>,
    updates: mpsc::Receiver<Update>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl SessionPort for SessionHost {
    fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }
    fn drain(&self) -> Vec<Update> {
        self.updates.try_iter().collect()
    }
    fn shutdown(self: Box<Self>) {
        SessionHost::shutdown(*self);
    }
}

impl SessionHost {
    pub fn start(options: Options) -> Self {
        let (commands, command_rx) = unbounded_channel();
        let (update_tx, updates) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("dim-session".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = update_tx.send(Update::Failed(format!("runtime: {error}")));
                        return;
                    }
                };
                runtime.block_on(run(options, command_rx, update_tx));
            })
            .expect("spawn session thread");
        Self {
            commands,
            updates,
            thread: Some(thread),
        }
    }

    /// Closes the command channel. The worker is not joined: it may still be
    /// inside the App Server start (seconds), and leaving the interface must
    /// not wait for that. The process exit reaps the thread.
    pub fn shutdown(mut self) {
        drop(std::mem::replace(&mut self.commands, unbounded_channel().0));
        if let Some(thread) = self.thread.take() {
            let deadline = std::time::Instant::now() + Duration::from_millis(150);
            while !thread.is_finished() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if thread.is_finished() {
                let _ = thread.join();
            }
        }
    }
}

/// The session the worker is attached to and what the screen shows of it.
struct Worker {
    options: Options,
    client: Client,
    updates: mpsc::Sender<Update>,
    session_id: String,
    projection: Projection,
    /// Highest event sequence applied; replayed or duplicate events are skipped.
    cursor: u64,
    dirty: bool,
}

async fn run(
    options: Options,
    mut commands: UnboundedReceiver<Command>,
    updates: mpsc::Sender<Update>,
) {
    let client = match client::connect(&options).await {
        Ok(client) => client,
        Err(error) => {
            let _ = updates.send(Update::Failed(format!(
                "Cannot start the App Server: {error}"
            )));
            return;
        }
    };
    let mut worker = Worker {
        options,
        client,
        updates: updates.clone(),
        session_id: String::new(),
        projection: Projection::default(),
        cursor: 0,
        dirty: false,
    };
    if let Err(error) = worker.open().await {
        let _ = updates.send(Update::Failed(error.to_string()));
        return;
    }
    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else { break };
                if let Err(error) = worker.command(command).await {
                    let _ = updates.send(Update::Failed(error.to_string()));
                    break;
                }
            }
            message = worker.client.next() => {
                let message = match message {
                    Ok(message) => message,
                    Err(error) => {
                        let _ = updates.send(Update::Failed(error.to_string()));
                        break;
                    }
                };
                if let Err(error) = worker.message(message) {
                    let _ = updates.send(Update::Failed(error.to_string()));
                    break;
                }
            }
            _ = tokio::time::sleep(PUBLISH_INTERVAL), if worker.dirty => worker.publish(),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Created {
    session_id: String,
    model: ModelId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct History {
    events: Vec<Event>,
    through: u64,
    next_after: u64,
    done: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EventNotice {
    session_id: String,
    event: Event,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StateNotice {
    session_id: String,
    running: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApprovalNotice {
    session_id: String,
    request_id: String,
    call: ApprovalCall,
    reason: String,
}

#[derive(Deserialize)]
struct ApprovalCall {
    name: String,
    input: dim_agent_types::JsonObject,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CancelNotice {
    request_id: String,
}

impl Worker {
    /// Creates or resumes the session the process was started for.
    async fn open(&mut self) -> crate::Result<()> {
        if let Some(id) = self.options.resume.clone() {
            return self.attach(id, SwitchKind::Resumed).await;
        }
        let mut params = json!({"cwd": self.options.cwd});
        if let (Some(provider), Some(model)) = (&self.options.provider, &self.options.model) {
            params["model"] = json!({"providerId": provider, "modelId": model});
        }
        let created: Created = self.client.typed("session.create", params).await?;
        self.enter(created.session_id, created.model, SwitchKind::New)
            .await
    }

    async fn enter(
        &mut self,
        session_id: String,
        model: ModelId,
        kind: SwitchKind,
    ) -> crate::Result<()> {
        self.client
            .rpc("session.attach", json!({"sessionId": session_id}))
            .await?;
        self.session_id = session_id.clone();
        self.projection = Projection::default();
        self.cursor = 0;
        self.load_history().await?;
        self.send(Update::Ready {
            provider: model.provider_id,
            model: model.model_id,
        });
        self.send(Update::Switched { session_id, kind });
        self.publish();
        Ok(())
    }

    async fn attach(&mut self, session_id: String, kind: SwitchKind) -> crate::Result<()> {
        let listed = self.list().await?;
        let session = listed
            .into_iter()
            .find(|session| session.session_id == session_id)
            .ok_or_else(|| format!("Session {session_id} was not found"))?;
        self.enter(session_id, session.model, kind).await
    }

    async fn list(&mut self) -> crate::Result<Vec<SessionSummary>> {
        // Only the summaries carry the preview and update time the picker
        // shows; the reply is the bare list.
        self.client.typed("session.summaries", json!({})).await
    }

    async fn load_history(&mut self) -> crate::Result<()> {
        let mut through = None;
        let mut after = self.cursor;
        loop {
            let mut params = json!({
                "sessionId": self.session_id,
                "branchId": "main",
                "after": after,
                "limit": HISTORY_PAGE,
            });
            if let Some(through) = through {
                params["through"] = json!(through);
            }
            let page: History = self.client.typed("session.history", params).await?;
            if through.is_some_and(|through| through != page.through)
                || page.next_after < after
                || page.next_after > page.through
                || !page.done && page.next_after == after
            {
                return Err("Invalid history page cursor".into());
            }
            through = Some(page.through);
            for event in &page.events {
                if event.sequence <= after || event.sequence > page.next_after {
                    return Err("History event is outside its page".into());
                }
            }
            for event in page.events {
                self.apply(event);
            }
            after = page.next_after;
            if page.done {
                if after != page.through {
                    return Err("History ended before its fixed watermark".into());
                }
                return Ok(());
            }
        }
    }

    async fn command(&mut self, command: Command) -> crate::Result<()> {
        match command {
            Command::Submit(text) => {
                let params = json!({
                    "sessionId": self.session_id,
                    "branchId": "main",
                    "submissionId": uuid::Uuid::new_v4().to_string(),
                    "text": text,
                    "mode": "queue",
                });
                let result = self.client.rpc("session.submit", params).await;
                self.report_failure(result);
            }
            Command::Stop => {
                let params = json!({
                    "sessionId": self.session_id,
                    "branchId": "main",
                    "reason": "TUI interrupted",
                });
                let result = self.client.rpc("session.stop", params).await;
                self.report_failure(result);
            }
            Command::Approval {
                request_id,
                branch_id,
                allow,
                remember: _,
            } => {
                let decision = if allow {
                    ApprovalDecision::Allow
                } else {
                    ApprovalDecision::Deny
                };
                let params = json!({
                    "sessionId": self.session_id,
                    "branchId": branch_id,
                    "requestId": request_id,
                    "decision": decision,
                });
                let result = self.client.rpc("approval.respond", params).await;
                if self.report_failure(result) {
                    self.projection.remove_approval(&request_id);
                    self.dirty = true;
                }
            }
            Command::NewSession => {
                let created: Created = self
                    .client
                    .typed("session.create", json!({"cwd": self.options.cwd}))
                    .await?;
                self.enter(created.session_id, created.model, SwitchKind::New)
                    .await?;
            }
            Command::Resume(session_id) => {
                self.attach(session_id, SwitchKind::Resumed).await?;
            }
            Command::ListSessions => {
                let sessions = self.list().await.map(|mut sessions| {
                    // The daemon lists in no particular order; the picker
                    // shows the most recent first.
                    sessions.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
                    sessions
                        .into_iter()
                        .map(|session| SessionItem {
                            id: session.session_id,
                            title: None,
                            preview: session.preview,
                            cwd: session.cwd,
                            updated_at: session.updated_at,
                            model: session.model.model_id,
                        })
                        .collect()
                });
                self.send(Update::Sessions(
                    sessions.map_err(|error| error.to_string()),
                ));
            }
            Command::Usage => self.send(Update::Report {
                text: format!("Usage reporting is {UNAVAILABLE}."),
                warning: false,
            }),
            Command::ListModels => {
                self.send(Update::Models(Err(format!("Models are {UNAVAILABLE}"))))
            }
            Command::GetApprovals | Command::SetApprovals(_) => self.send(Update::Approvals(Err(
                format!("Approval preferences are {UNAVAILABLE}"),
            ))),
            Command::ListProviders => self.send(Update::Providers(Err(format!(
                "Providers are {UNAVAILABLE}"
            )))),
            Command::Provider(_) => self.send(Update::ProviderResult(Err(format!(
                "Providers are {UNAVAILABLE}"
            )))),
            Command::OauthStart(start) => self.send(Update::Oauth {
                attempt: start.attempt,
                event: OauthEvent::Done(Err(Some(format!("Sign-in is {UNAVAILABLE}")))),
            }),
            Command::OauthCancel | Command::OauthPaste(_) => {}
            Command::SelectModel { .. } => {
                self.send(Update::Notice(format!("Model selection is {UNAVAILABLE}")))
            }
            Command::AskAnswers { .. } | Command::AskCancel { .. } => {
                self.send(Update::Notice(format!("Questions are {UNAVAILABLE}")))
            }
            Command::InputControl { .. } => {
                self.send(Update::Notice(format!("Queue control is {UNAVAILABLE}")))
            }
            Command::Reconnect => self.send(Update::Notice(format!("Reconnect is {UNAVAILABLE}"))),
        }
        Ok(())
    }

    /// Reports a failed command as a notice; the session itself keeps going.
    fn report_failure<T>(&self, result: crate::Result<T>) -> bool {
        match result {
            Ok(_) => true,
            Err(error) => {
                self.send(Update::Notice(error.to_string()));
                false
            }
        }
    }

    fn message(&mut self, message: Message) -> crate::Result<()> {
        let Message::Notice { method, params } = message else {
            return Ok(());
        };
        match method.as_str() {
            "session.event" => {
                let notice: EventNotice = serde_json::from_value(params)?;
                if notice.session_id == self.session_id {
                    self.apply(notice.event);
                }
            }
            "session.state" => {
                let notice: StateNotice = serde_json::from_value(params)?;
                if notice.session_id == self.session_id {
                    self.projection.set_running(notice.running);
                    self.dirty = true;
                }
            }
            "approval.request" => {
                let notice: ApprovalNotice = serde_json::from_value(params)?;
                if notice.session_id == self.session_id {
                    self.projection.add_approval(
                        notice.request_id,
                        notice.call.name,
                        notice.call.input,
                        notice.reason,
                    );
                    self.dirty = true;
                }
            }
            "approval.cancelled" => {
                let notice: CancelNotice = serde_json::from_value(params)?;
                self.projection.remove_approval(&notice.request_id);
                self.dirty = true;
            }
            "session.closed" => return Err("Session closed while attached".into()),
            _ => {}
        }
        Ok(())
    }

    fn apply(&mut self, event: Event) {
        if event.sequence <= self.cursor {
            return;
        }
        self.cursor = event.sequence;
        self.projection.apply(&event);
        self.dirty = true;
    }

    fn publish(&mut self) {
        self.dirty = false;
        self.send(Update::Snapshot(Box::new(self.projection.snapshot())));
    }

    fn send(&self, update: Update) {
        let _ = self.updates.send(update);
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionSummary {
    session_id: String,
    cwd: String,
    model: ModelId,
    updated_at: u64,
    #[serde(default)]
    preview: Option<String>,
}
