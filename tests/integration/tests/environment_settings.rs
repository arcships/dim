mod local_support;

use dim_agent_capabilities::{
    execution_settings::{ExecutionSettings, ExecutionSettingsHost},
    settings::{SettingsChanged, SettingsScope, settings_changes},
};
use dim_agent_runtime::Session;
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
