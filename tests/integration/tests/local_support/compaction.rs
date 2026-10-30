//! Window facts and the strategy that takes the retention rule's cut, so an
//! execution flow test can compact a real command's history and read the
//! digest (ContextCompaction §5.10, acceptance 18).
#![allow(dead_code)]
use dim_agent_capabilities::{
    agent_loop::AgentLoop,
    history::summary_message,
    host::{
        CompactionInput, CompactionStrategy, ContextModel, HostFuture, ResolveContextModel,
        StrategyDiagnostics, StrategyOutcome,
    },
};
use dim_agent_runtime::*;
use dim_agent_types::*;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

/// `40,960` window tokens with `64` reserved for output, so `B` is `40,896`
/// and the digest's bound is `0.05 × B`. The threshold is out of reach: only a
/// test that asks compacts.
#[derive(Default)]
pub struct Window {
    /// The request each strategy call was handed, when the test asks for
    /// them.
    pub requests: Option<Arc<Mutex<Vec<Arc<ModelRequest>>>>>,
}
impl Window {
    /// The window facts, and the requests its strategy is given.
    pub fn recording() -> (Self, Arc<Mutex<Vec<Arc<ModelRequest>>>>) {
        let requests = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                requests: Some(requests.clone()),
            },
            requests,
        )
    }
}

impl ResolveContextModel for Window {
    fn resolve(
        &self,
        _: Arc<ModelRequest>,
        _: CancellationToken,
    ) -> HostFuture<'_, Arc<dyn ContextModel>> {
        Box::pin(async { Ok(Arc::new(Window::default()) as Arc<dyn ContextModel>) })
    }
}

impl ContextModel for Window {
    fn context_window(&self) -> f64 {
        40_960.
    }
    fn output_tokens(&self) -> f64 {
        64.
    }
    fn threshold_tokens(&self) -> f64 {
        f64::MAX
    }
    fn count_tokens(&self, request: &ModelRequest) -> f64 {
        serde_json::to_vec(&request.messages).unwrap().len() as f64
    }
}

impl CompactionStrategy for Window {
    fn name(&self) -> &str {
        "window"
    }
    fn replacement_size(&self, model: &dyn ContextModel) -> f64 {
        model.count_tokens(&ModelRequest::new("", "", vec![summary_message("short")]))
    }
    fn compact(
        &self,
        input: CompactionInput,
        _: CancellationToken,
    ) -> HostFuture<'_, StrategyOutcome> {
        Box::pin(async move {
            if let Some(requests) = &self.requests {
                requests.lock().unwrap().push(input.request.clone());
            }
            let Some(cut) = input.cut() else {
                return Ok(StrategyOutcome::Skipped {
                    reason: "nothing_to_compact".into(),
                    diagnostics: diagnostics(),
                });
            };
            Ok(StrategyOutcome::Completed {
                retain_from: cut.retain_from.clone(),
                replacement: vec![summary_message("short")],
                diagnostics: diagnostics(),
            })
        })
    }
}

fn diagnostics() -> StrategyDiagnostics {
    StrategyDiagnostics {
        model_call_count: 1,
        model_usage: None,
        record: CompactionStrategyRecord {
            kind: "window".into(),
            details: JsonObject::new(),
        },
    }
}

/// The Loop's manual compaction, waited for, with the branch's events.
pub async fn compact_now(session: &Session) -> (u64, Vec<Arc<AgentEvent>>) {
    let command = session
        .service::<AgentLoop>("main")
        .unwrap()
        .compact()
        .unwrap();
    let events = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let events = session.local("main", ..).await.unwrap();
            if events.iter().any(|event| {
                matches!(
                    &event.kind,
                    EventKind::ContextCompaction(data) if data.command == Some(command)
                )
            }) {
                return events;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the manual compaction");
    (command, events)
}

/// The text of one replacement message.
pub fn message_text(message: &Message) -> &str {
    match &message.content {
        MessageContent::Text(text) => text,
        MessageContent::Parts(_) => panic!("the digest is one text message"),
    }
}

/// The digest entries of the completed compaction `command`, and its
/// replacement messages.
pub fn digest(events: &[Arc<AgentEvent>], command: u64) -> (Option<DigestEntries>, Vec<Message>) {
    let data = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ContextCompaction(data) if data.command == Some(command) => Some(data),
            _ => None,
        })
        .expect("the compaction result");
    let ContextCompactionResult::Completed {
        replacement,
        digest_entries,
        ..
    } = &data.outcome
    else {
        panic!("the compaction did not complete: {:?}", data.outcome)
    };
    (digest_entries.clone(), replacement.clone())
}
