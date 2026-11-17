//! #524: a full overwrite needs a read that is still in the context. The real
//! file tools run against a real branch, and a manual compaction moves the cut
//! either side of the read's result.

use super::*;
use dim_agent_capabilities::{
    agent_loop::AgentLoop,
    defaults,
    history::summary_message,
    host_platform::{HostPlatform, ShellStatus},
    workspace_tools::{self, WorkspaceToolsConfig},
};
use dim_agent_runtime::testing::chain_events;
use serde_json::{Value, json};
use std::path::PathBuf;

/// Counts the serialized messages and never triggers an automatic step, so
/// only the manual compaction this module drives runs.
#[derive(Clone, Copy)]
struct Model;
impl ResolveContextModel for Model {
    fn resolve(
        &self,
        _: Arc<ModelRequest>,
        _: CancellationToken,
    ) -> HostFuture<'_, Arc<dyn ContextModel>> {
        Box::pin(async move { Ok(Arc::new(Model) as Arc<dyn ContextModel>) })
    }
}
impl ContextModel for Model {
    fn context_window(&self) -> f64 {
        1_000_000.
    }
    fn output_tokens(&self) -> f64 {
        64.
    }
    fn threshold_tokens(&self) -> f64 {
        1_000_000.
    }
    fn count_tokens(&self, request: &ModelRequest) -> f64 {
        serde_json::to_vec(&request.messages).unwrap().len() as f64
    }
}

/// Where the compaction cuts relative to the entry holding a marker.
#[derive(Clone, Copy)]
enum Cut {
    /// Right after the entry holding the marker: the entry is replaced.
    After(&'static str),
    /// Right before the entry holding the marker: the entry is kept.
    Before(&'static str),
}

/// Cuts at `cut`, or skips when the cut it names is not legal.
struct Cuts(Cut);
impl CompactionStrategy for Cuts {
    fn name(&self) -> &str {
        "cuts"
    }
    fn replacement_size(&self, model: &dyn ContextModel) -> f64 {
        model.count_tokens(&ModelRequest::new(
            "",
            "",
            vec![summary_message("compacted")],
        ))
    }
    fn compact(
        &self,
        input: CompactionInput,
        _: CancellationToken,
    ) -> HostFuture<'_, StrategyOutcome> {
        let marked = |entry: &HistoryEntry| {
            let marker = match self.0 {
                Cut::After(marker) | Cut::Before(marker) => marker,
            };
            serde_json::to_string(&entry.message)
                .unwrap()
                .contains(marker)
        };
        let cut = input
            .history
            .iter()
            .position(marked)
            .and_then(|entry| match self.0 {
                Cut::After(_) => input.cuts.iter().find(|cut| cut.entry > entry),
                Cut::Before(_) => input.cuts.iter().rev().find(|cut| cut.entry <= entry),
            });
        let Some(cut) = cut else {
            return Box::pin(async {
                Ok(StrategyOutcome::Skipped {
                    reason: "no_cut".into(),
                    diagnostics: test_diagnostics(),
                })
            });
        };
        let retain_from = cut.retain_from.clone();
        Box::pin(async move {
            Ok(StrategyOutcome::Completed {
                retain_from,
                replacement: vec![summary_message("compacted")],
                diagnostics: test_diagnostics(),
            })
        })
    }
}

/// One scripted model step per request.
enum Step {
    /// Answers with these tool calls.
    Calls(Vec<(&'static str, Value)>),
    /// Answers with text and ends the turn.
    Says(&'static str),
}

#[derive(Default)]
struct Script {
    steps: std::sync::Mutex<std::collections::VecDeque<Step>>,
    calls: AtomicUsize,
}
impl Script {
    fn push(&self, steps: Vec<Step>) {
        self.steps.lock().unwrap().extend(steps);
    }
}
impl ModelDriver for Script {
    fn invoke(
        &self,
        _: Arc<ModelRequest>,
        _: ModelConnection,
        _: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        let step = self.steps.lock().unwrap().pop_front().expect("a step");
        let mut chunks = vec![Ok(ModelChunk::MessageStart {
            message_id: "step".into(),
        })];
        let reason = match step {
            Step::Says(text) => {
                chunks.push(Ok(ModelChunk::Text(text.into())));
                ModelStopReason::EndTurn
            }
            Step::Calls(calls) => {
                for (name, input) in calls {
                    let id = format!("call-{}", self.calls.fetch_add(1, Ordering::SeqCst));
                    chunks.push(Ok(ModelChunk::ToolStart {
                        id: id.clone(),
                        name: name.into(),
                    }));
                    chunks.push(Ok(ModelChunk::ToolUse(ToolCall {
                        id,
                        name: name.into(),
                        input: input.as_object().unwrap().clone(),
                    })));
                }
                ModelStopReason::ToolUse
            }
        };
        chunks.push(Ok(ModelChunk::MessageStop {
            message_id: "step".into(),
            reason,
        }));
        Box::pin(async { Ok(Box::pin(futures_util::stream::iter(chunks)) as ModelStream) })
    }
}

struct Files {
    dir: PathBuf,
    model: Arc<Script>,
    agent: MinimalHost,
}

impl Files {
    async fn new(cut: Cut) -> Self {
        let dir = std::env::temp_dir().join(format!("dim-compacted-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        let mut platform = HostPlatform::current(ShellStatus::Unavailable {
            code: "COMPACTED_BINDINGS_TEST",
            reason: "The file tools run no command here.".into(),
        })
        .unwrap();
        platform.home = dir.clone();
        let model = Arc::new(Script::default());
        let mut builder = AgentBuilder::new(AgentConfig::default());
        builder
            .timeline_store(Arc::new(MemoryTimelineStore::default()))
            .process_value(Arc::new(ModelHost(model.clone())))
            .session_value(Arc::new(HistoryRenderer::default()))
            .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
            .session_value(Arc::new(ContextModelHost(Some(Arc::new(Model)))))
            .session_value(Arc::new(CompactionStrategyHost(Some(Arc::new(Cuts(cut))))))
            .require_feature(
                Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                    Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
                }),
            );
        defaults::install_with_prompt(
            &mut builder,
            Arc::new(EmptyPromptHost),
            Some(ConfigureUpdate {
                sources: Vec::new(),
                model: Some(ModelSelection {
                    provider_id: "mock".into(),
                    model_id: "echo".into(),
                    capability_snapshot: None,
                    config: None,
                    metadata: None,
                }),
            }),
        );
        workspace_tools::install(
            &mut builder,
            WorkspaceToolsConfig {
                boundary: None,
                output_root: dir.join("output"),
            },
        );
        let agent = MinimalHost::start_with(&builder, |root, _| {
            root.provide_as::<HostPlatform>(Arc::new(platform))?;
            Ok(())
        })
        .await
        .unwrap();
        Self { dir, model, agent }
    }
    async fn session(&self) -> Session {
        self.agent
            .create_session(SessionConfig {
                cwd: self.dir.to_string_lossy().into_owned(),
                tools: Some(vec!["read".into(), "write".into(), "edit".into()]),
                permission_policy: PermissionPolicy {
                    preset: PermissionPreset::FullAccess,
                    ..Default::default()
                },
                ..SessionConfig::default()
            })
            .await
            .unwrap()
    }
    /// One user turn answered by `steps`.
    async fn turn(&self, session: &Session, prompt: &str, steps: Vec<Step>) {
        self.model.push(steps);
        session
            .submit(prompt.into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
    }
    /// A manual compaction, waited for, returning its `retain_from`.
    async fn compact(&self, session: &Session) -> String {
        let command = session
            .service::<AgentLoop>("main")
            .unwrap()
            .compact()
            .unwrap();
        let events = until(session, |events| {
            events.iter().any(|event| {
                matches!(&event.kind,
                    EventKind::ContextCompaction(data) if data.command == Some(command))
            })
        })
        .await;
        let data = events
            .iter()
            .find_map(|event| match &event.kind {
                EventKind::ContextCompaction(data) if data.command == Some(command) => Some(data),
                _ => None,
            })
            .unwrap();
        let ContextCompactionResult::Completed { retain_from, .. } = &data.outcome else {
            panic!("the compaction did not complete: {:?}", data.outcome)
        };
        retain_from.clone()
    }
    async fn shutdown(self) {
        self.agent.close().await.unwrap();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn until(
    session: &Session,
    done: impl Fn(&[Arc<AgentEvent>]) -> bool,
) -> Vec<Arc<AgentEvent>> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let events = chain_events(session, "main", ..).await.unwrap();
            if done(&events) {
                return events;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the expected facts are committed")
}

/// The last tool result the model was given.
fn last_result(events: &[Arc<AgentEvent>]) -> ToolResult {
    events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ToolResult(data) => data.results.last().map(|item| item.result.clone()),
            _ => None,
        })
        .expect("a tool result")
}

fn text(result: &ToolResult) -> &str {
    match &result.content {
        ToolContent::Text(text) => text,
        ToolContent::Blocks(_) => panic!("file tools return text"),
    }
}

fn code(result: &ToolResult) -> Option<&str> {
    result.structured_content.as_ref()?.get("code")?.as_str()
}

/// A turn large enough that replacing it shrinks the request.
fn filler() -> String {
    "filler ".repeat(300)
}

/// A long turn, the turn that reads `name`, then another turn: a compaction
/// can cut either side of the read.
async fn read_then_turn(files: &Files, session: &Session, name: &str, body: &str) {
    files
        .turn(session, &filler(), vec![Step::Says("hello")])
        .await;
    std::fs::write(files.dir.join(name), body).unwrap();
    files
        .turn(
            session,
            "read the file",
            vec![
                Step::Calls(vec![("read", json!({ "path": name }))]),
                Step::Says("read"),
            ],
        )
        .await;
    files.turn(session, "next", vec![Step::Says("ok")]).await;
}

fn write(name: &str, content: &str) -> Value {
    json!({ "path": name, "content": content })
}

/// Acceptance 1 and 2: a compaction that cut the read out refuses the write,
/// and one partial read restores it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_write_after_a_compaction_that_cut_its_read_is_refused_until_the_file_is_read_again() {
    let files = Files::new(Cut::After("alpha")).await;
    let session = files.session().await;
    read_then_turn(&files, &session, "note.txt", "alpha\nbeta\n").await;
    files.compact(&session).await;
    files
        .turn(
            &session,
            "overwrite it",
            vec![
                Step::Calls(vec![("write", write("note.txt", "replaced\n"))]),
                Step::Says("done"),
            ],
        )
        .await;
    let events = chain_events(&session, "main", ..).await.unwrap();
    let refused = last_result(&events);
    assert_eq!(code(&refused), Some("text_binding_required"));
    assert!(text(&refused).contains("What you last read or wrote of this file was compacted out"));
    assert_eq!(
        std::fs::read(files.dir.join("note.txt")).unwrap(),
        b"alpha\nbeta\n"
    );

    files
        .turn(
            &session,
            "read a part",
            vec![
                Step::Calls(vec![("read", json!({ "path": "note.txt", "limit": 1 }))]),
                Step::Says("read"),
            ],
        )
        .await;
    files
        .turn(
            &session,
            "overwrite it",
            vec![
                Step::Calls(vec![("write", write("note.txt", "replaced\n"))]),
                Step::Says("done"),
            ],
        )
        .await;
    let events = chain_events(&session, "main", ..).await.unwrap();
    assert_eq!(last_result(&events).is_error, None);
    assert_eq!(
        std::fs::read(files.dir.join("note.txt")).unwrap(),
        b"replaced\n"
    );
    files.shutdown().await;
}

/// Acceptance 3: a read in the kept tail stays bound.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_compaction_that_keeps_the_read_leaves_the_write_admitted() {
    let files = Files::new(Cut::Before("alpha")).await;
    let session = files.session().await;
    read_then_turn(&files, &session, "kept.txt", "alpha\nbeta\n").await;
    let retain_from = files.compact(&session).await;
    let events = chain_events(&session, "main", ..).await.unwrap();
    let cut = events
        .iter()
        .position(|event| event.id == retain_from)
        .unwrap();
    let read = events
        .iter()
        .position(|event| {
            matches!(&event.kind, EventKind::ToolResult(data)
                if data.results.iter().any(|item| text(&item.result).contains("alpha")))
        })
        .unwrap();
    assert!(read >= cut, "the read is in the kept tail");
    files
        .turn(
            &session,
            "overwrite it",
            vec![
                Step::Calls(vec![("write", write("kept.txt", "replaced\n"))]),
                Step::Says("done"),
            ],
        )
        .await;
    let events = chain_events(&session, "main", ..).await.unwrap();
    assert_eq!(last_result(&events).is_error, None);
    assert_eq!(
        std::fs::read(files.dir.join("kept.txt")).unwrap(),
        b"replaced\n"
    );
    files.shutdown().await;
}

/// Acceptance 4: `edit` keeps its binding, because `oldText` checks content.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_edit_after_a_compaction_that_cut_its_read_succeeds() {
    let files = Files::new(Cut::After("alpha")).await;
    let session = files.session().await;
    read_then_turn(&files, &session, "edit.txt", "alpha\nbeta\n").await;
    files.compact(&session).await;
    files
        .turn(
            &session,
            "edit it",
            vec![
                Step::Calls(vec![(
                    "edit",
                    json!({ "path": "edit.txt", "oldText": "beta", "newText": "gamma" }),
                )]),
                Step::Says("done"),
            ],
        )
        .await;
    let events = chain_events(&session, "main", ..).await.unwrap();
    assert_eq!(last_result(&events).is_error, None);
    assert_eq!(
        std::fs::read(files.dir.join("edit.txt")).unwrap(),
        b"alpha\ngamma\n"
    );
    files.shutdown().await;
}

/// Acceptance 5: a binding a `write` or `edit` established is treated like a
/// read's; a compaction that cuts it refuses the next full overwrite.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_compaction_that_cuts_a_written_binding_refuses_the_next_write() {
    let files = Files::new(Cut::After("Wrote")).await;
    let session = files.session().await;
    std::fs::write(files.dir.join("new.txt"), "alpha\n").unwrap();
    // The first turn is long enough that replacing it shrinks the request with
    // the digest's planned share in the resident parts.
    files
        .turn(
            &session,
            &filler(),
            vec![
                Step::Calls(vec![("read", json!({ "path": "new.txt" }))]),
                Step::Calls(vec![("write", write("new.txt", "written\n"))]),
                Step::Says("done"),
            ],
        )
        .await;
    let events = chain_events(&session, "main", ..).await.unwrap();
    assert_eq!(last_result(&events).is_error, None);
    files.turn(&session, "next", vec![Step::Says("ok")]).await;
    files.compact(&session).await;
    files
        .turn(
            &session,
            "overwrite it again",
            vec![
                Step::Calls(vec![("write", write("new.txt", "again\n"))]),
                Step::Says("done"),
            ],
        )
        .await;
    let events = chain_events(&session, "main", ..).await.unwrap();
    let refused = last_result(&events);
    assert_eq!(code(&refused), Some("text_binding_required"));
    assert!(text(&refused).contains("What you last read or wrote of this file was compacted out"));
    assert_eq!(
        std::fs::read(files.dir.join("new.txt")).unwrap(),
        b"written\n"
    );
    files.shutdown().await;
}
