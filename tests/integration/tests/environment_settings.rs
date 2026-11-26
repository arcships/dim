use crate::local_support;

use dim_agent_capabilities::execution_settings::ExecutionSettingsError;
use dim_agent_capabilities::{
    execution_settings::{ExecutionSettings, ExecutionSettingsHost},
    settings::{SettingsChanged, SettingsScope, settings_changes},
};
use dim_agent_runtime::BranchOptions;
use dim_agent_runtime::Session;
use dim_agent_types::{BranchSource, EventKind, InputBlock};
use local_support::*;
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn changed(env: &Env, revision: &str) {
    env.root
        .emit(SettingsChanged {
            name: settings_changes("execution"),
            source: "execution".into(),
            scope: SettingsScope::User,
            revision: revision.into(),
        })
        .unwrap();
}

/// A mutable execution-settings source: the test changes it as another client
/// would.
struct Execution(Arc<Mutex<ExecutionSettings>>);
impl Execution {
    /// The switches as the face's `read` answers them.
    fn host(&self) -> ExecutionSettingsHost {
        let settings = self.0.clone();
        ExecutionSettingsHost::new(Arc::new(move |_| {
            let settings = *settings.lock().unwrap();
            Box::pin(async move { Ok(settings) })
        }))
    }
}
impl Execution {
    fn set(&self, settings: ExecutionSettings) {
        *self.0.lock().unwrap() = settings;
    }
}

async fn until_block(session: &Session, needle: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let found = inputs(session)
                .await
                .iter()
                .flat_map(|input| &input.blocks)
                .any(|block| {
                    let dim_agent_types::InputBlock::Raw { text, .. } = block;
                    text.contains(needle)
                });
            if found {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("execution settings were adopted");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execution_state_is_adopted_and_unchanged_values_do_not_duplicate() {
    let dirs = Dirs::new();
    let cwd = dirs.dir("work");
    let execution = Arc::new(Execution(Arc::new(
        Mutex::new(ExecutionSettings::default()),
    )));
    let env = env(
        &dirs,
        Options {
            execution: Some(Arc::new(execution.host())),
            ..Options::default()
        },
    )
    .await;
    let session = env.session(&cwd, "").await.unwrap();

    // The branch may take its first turn before the part's detached append
    // lands (`execution_settings/prompt.rs`), so wait for the snapshot's
    // Timeline append before the turn that must carry it.
    until_block(
        &session,
        "<dim-state source=\"dim/settings/execution\">Execution sandbox: off.",
    )
    .await;
    run(&session, "hello").await;
    let text = request_text(requests(&session).await.last().unwrap());
    assert!(
        text.contains(
            "<dim-state source=\"dim/settings/execution\">Execution sandbox: off. New processes run with the host's normal access.</dim-state>"
        ),
        "{text}"
    );

    execution.set(ExecutionSettings {
        sandbox: true,
        network: false,
    });
    changed(&env, "1");
    run(&session, "sandbox").await;
    until_block(&session, "Execution sandbox: on; network: off.").await;
    run(&session, "after").await;
    let text = request_text(requests(&session).await.last().unwrap());
    assert!(
        text.contains(
            "<dim-state source=\"dim/settings/execution\">Execution sandbox: on; network: off. New processes only; a running shell keeps its original boundary.</dim-state>"
        ),
        "{text}"
    );
    assert_eq!(
        text.matches("Execution sandbox: on; network: off.").count(),
        1,
        "an unchanged value must not write another block:\n{text}"
    );

    execution.set(ExecutionSettings {
        sandbox: true,
        network: true,
    });
    changed(&env, "2");
    run(&session, "network").await;
    until_block(&session, "Execution sandbox: on; network: on.").await;
    run(&session, "after-network").await;
    let text = request_text(requests(&session).await.last().unwrap());
    assert!(
        text.contains("Execution sandbox: on; network: on."),
        "{text}"
    );

    execution.set(ExecutionSettings::default());
    changed(&env, "3");
    run(&session, "off").await;
    until_block(&session, "Execution sandbox: off.").await;
    run(&session, "after-off").await;
    let text = request_text(requests(&session).await.last().unwrap());
    assert!(
        text.contains(
            "<dim-state source=\"dim/settings/execution\">Execution sandbox: off. New processes run with the host's normal access.</dim-state>"
        ),
        "{text}"
    );

    env.agent.close().await.unwrap();
}

/// A settings source the test holds: every read waits until [`Self::release`]
/// and then answers the switches the test configured.
struct Held {
    settings: ExecutionSettings,
    open: tokio::sync::watch::Sender<bool>,
}
impl Held {
    fn new(settings: ExecutionSettings) -> Arc<Self> {
        Arc::new(Self {
            settings,
            open: tokio::sync::watch::channel(false).0,
        })
    }
    fn host(self: &Arc<Self>) -> ExecutionSettingsHost {
        let held = self.clone();
        ExecutionSettingsHost::new(Arc::new(move |cancel| {
            let held = held.clone();
            Box::pin(async move {
                let mut open = held.open.subscribe();
                tokio::select! {
                    _ = cancel.cancelled() => Err(ExecutionSettingsError::Failed {
                        message: "cancelled".into(),
                    }),
                    _ = async {
                        while !*open.borrow() {
                            if open.changed().await.is_err() {
                                return;
                            }
                        }
                    } => Ok(held.settings),
                }
            })
        }))
    }
    fn release(&self) {
        self.open.send_replace(true);
    }
}

/// The execution-settings snapshot bodies `branch` holds, oldest first.
async fn settings_snapshots(session: &Session, branch: &str) -> Vec<String> {
    dim_agent_runtime::testing::branch_events(session, branch, ..)
        .await
        .unwrap()
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::InputMessage(data) => {
                let InputBlock::Raw { text, .. } = data.blocks.first()?;
                text.starts_with("<dim-state source=\"dim/settings/execution\">")
                    .then(|| text.clone())
            }
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_held_settings_read_never_holds_a_branch_open() {
    let dirs = Dirs::new();
    let cwd = dirs.dir("work");
    let held = Held::new(ExecutionSettings {
        sandbox: true,
        network: false,
    });
    let store = Arc::new(dim_agent_runtime::MemoryTimelineStore::default());
    let options = || Options {
        store: Some(store.clone()),
        execution: Some(Arc::new(held.host())),
        ..Options::default()
    };
    let first = env(&dirs, options()).await;
    let session = first
        .session(&cwd, "")
        .await
        .expect("the session opens while the settings read is held");
    let id = session.id().to_owned();
    let at = session.sequence("main").unwrap();
    let line = session
        .branch(
            "main".into(),
            at,
            BranchOptions {
                prompt: "line work".into(),
                source: BranchSource::Host {
                    operation_id: "held-settings".into(),
                },
            },
        )
        .await
        .expect("a line opens while the settings read is held");
    for branch in ["main", line.as_str()] {
        assert_eq!(
            settings_snapshots(&session, branch).await.len(),
            0,
            "a held read appends no snapshot to {branch}"
        );
    }
    first.agent.close().await.unwrap();

    let second = env(&dirs, options()).await;
    let session = second
        .agent
        .restore_session(id)
        .await
        .expect("the session restores while the settings read is held");
    assert_eq!(
        settings_snapshots(&session, "main").await.len(),
        0,
        "the held read appends nothing during the restore"
    );
    held.release();
    until_block(
        &session,
        "<dim-state source=\"dim/settings/execution\">Execution sandbox: on; network: off.",
    )
    .await;
    assert_eq!(
        settings_snapshots(&session, "main").await.len(),
        1,
        "the released read appends one snapshot after activation"
    );
    second.agent.close().await.unwrap();
}

/// The opening read is bounded by the branch's own opening budget: with a
/// budget shorter than the part's retry interval, a stalled source still lets
/// the session open, and its snapshot is appended once the source answers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_held_read_fits_a_budget_shorter_than_the_retry_interval() {
    let dirs = Dirs::new();
    let cwd = dirs.dir("work");
    let held = Held::new(ExecutionSettings {
        sandbox: true,
        network: false,
    });
    let env = env(
        &dirs,
        Options {
            execution: Some(Arc::new(held.host())),
            startup_timeout: Some(Duration::from_millis(500)),
            ..Options::default()
        },
    )
    .await;
    let session = env
        .session(&cwd, "")
        .await
        .expect("the session opens inside its shorter opening budget");
    assert_eq!(
        settings_snapshots(&session, "main").await.len(),
        0,
        "the held read appends nothing during the opening"
    );
    held.release();
    until_block(
        &session,
        "<dim-state source=\"dim/settings/execution\">Execution sandbox: on; network: off.",
    )
    .await;
    assert_eq!(
        settings_snapshots(&session, "main").await.len(),
        1,
        "the released read appends one snapshot after activation"
    );
    env.agent.close().await.unwrap();
}
