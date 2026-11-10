//! #1361: a snapshot read over a long, fully delivered prefix. The read
//! collects the inputs its inherited changes name and asks for their
//! consumptions in one query, whose membership list grows with the history.
//! It runs on the native store, because the SQLite compiler is where a long
//! list used to reach an expression-depth limit.

use super::*;
use dim_agent_capabilities::host::{HostFuture, LoadPrompt};
use dim_agent_runtime::{BranchOptions, ConfigureUpdate, PromptSourceTarget};
use std::{
    collections::BTreeSet,
    sync::atomic::{AtomicUsize, Ordering},
};

/// The number of adopted sources: each one is a State change whose delivery
/// input the child's read names.
const SOURCES: usize = 1_100;

/// Sources adopted per round. The flow's cost is its turns, not its changes:
/// one round adopts every source that changed.
const PER_ROUND: usize = 20;

/// The State text source `index` renders as.
fn value(index: usize) -> String {
    format!("<dim-state source=\"dim/test/{index}\">value {index}</dim-state>")
}

/// Prompt sources released one round at a time, each with an unchanging State
/// entry: a round adopts every source it newly sees.
struct Rounds(Arc<AtomicUsize>);
impl LoadPrompt for Rounds {
    fn load(
        &self,
        target: PromptSourceTarget,
        _: tokio_util::sync::CancellationToken,
    ) -> HostFuture<'_, Option<PromptEntry>> {
        let released = self.0.load(Ordering::SeqCst);
        Box::pin(async move {
            let index: usize = target
                .source
                .strip_prefix("dim/test/")
                .expect("a registered source")
                .parse()
                .expect("a source index");
            Ok((index < released).then(|| PromptEntry {
                source: target.source,
                scope: target.scope,
                version: "1".into(),
                body: value(index),
                delivery: PromptDelivery::State,
                order: 0,
            }))
        })
    }
}

fn model() -> ModelSelection {
    ModelSelection {
        provider_id: "provider".into(),
        model_id: "model".into(),
        capability_snapshot: None,
        config: None,
        metadata: None,
    }
}

fn sources(count: usize) -> Vec<PromptSourceTarget> {
    (0..count)
        .map(|index| PromptSourceTarget {
            source: format!("dim/test/{index}"),
            scope: PromptScope::Branch {
                timeline: "main".into(),
            },
        })
        .collect()
}

/// A fully delivered prefix of 1,100 adopted State sources, then a child cut:
/// the child's read names every input those changes were delivered with, so
/// the store compiles a membership list of 1,100 candidates.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_snapshot_read_over_a_long_delivered_prefix_has_no_list_length_limit() {
    tokio::time::timeout(Duration::from_secs(300), async {
        let database = Database::new();
        let db = SqliteStorage::open(database.0.clone()).unwrap();
        let store = SqliteTimelineStore::new(db.clone());
        let rounds = Arc::new(AtomicUsize::new(0));
        let mut builder = AgentBuilder::new(AgentConfig::default());
        dim_agent_capabilities::defaults::mock_hosts(&mut builder);
        dim_agent_capabilities::defaults::install_with_prompt(&mut builder, Arc::new(Rounds(rounds.clone())), None);
        builder.timeline_store(store.clone());
        let agent = MinimalHost::start(&builder).await.unwrap();
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        session
            .configure(
                ConfigureUpdate {
                    sources: Vec::new(),
                    model: Some(model()),
                },
                "main",
            )
            .await
            .unwrap();
        // Each round adopts the sources it newly sees as one change each, and
        // the turn that follows consumes every delivery input of the round.
        for round in 1..=SOURCES / PER_ROUND {
            rounds.store(round * PER_ROUND, Ordering::SeqCst);
            session
                .configure(
                    ConfigureUpdate {
                        sources: sources(round * PER_ROUND),
                        model: None,
                    },
                    "main",
                )
                .await
                .unwrap();
            session
                .submit(format!("turn {round}"), InputMode::Queue, "main")
                .await
                .unwrap();
            session.idle("main").await.unwrap();
        }
        let at = session.sequence("main").unwrap();
        let child = session
            .branch(
                "main".into(),
                at,
                BranchOptions {
                    prompt: "child work".into(),
                    source: BranchSource::Host {
                        operation_id: "child".into(),
                    },
                },
            )
            .await
            .unwrap();
        session.idle(&child).await.unwrap();
        let snapshot = session
            .get::<dim_agent_capabilities::prompt_state::PromptSnapshot>(&child)
            .unwrap()
            .expect("the line holds its prompt state");
        assert_eq!(
            snapshot
                .entries
                .values()
                .map(|entry| entry.body.clone())
                .collect::<BTreeSet<_>>(),
            (0..SOURCES).map(value).collect::<BTreeSet<_>>(),
            "every adopted value is in force on the child"
        );
        let events = chain_events(&session, &child, ..).await.unwrap();
        let named = events
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::PromptChanged(data) => Some(data.inputs.len()),
                _ => None,
            })
            .sum::<usize>();
        assert_eq!(
            named, SOURCES,
            "the read names every delivery input of the inherited changes"
        );
        let _ = agent.close().await;
        store.close().await.unwrap();
        db.close().await.unwrap();
    })
    .await
    .expect("a long delivered prefix is read without a list-length limit");
}
