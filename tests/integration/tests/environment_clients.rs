mod local_support;

use dim_agent_capabilities::environment_clients::{
    ClientChannel, ClientCounts, SessionClientsSource,
};
use dim_agent_runtime::Session;
use local_support::*;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A mutable presence source: the test changes the counts while the branch
/// is idle, as another client would.
struct Presence(Mutex<ClientCounts>);
impl Presence {
    fn set(&self, channels: &[ClientChannel]) {
        let mut counts = ClientCounts::default();
        for channel in channels {
            counts.add(*channel);
        }
        *self.0.lock().unwrap() = counts;
    }
}
impl SessionClientsSource for Presence {
    fn counts(&self, _: &str) -> ClientCounts {
        self.0.lock().unwrap().clone()
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
    .expect("presence was adopted");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn presence_is_adopted_and_unchanged_counts_do_not_duplicate() {
    let dirs = Dirs::new();
    let cwd = dirs.dir("work");
    let presence = Arc::new(Presence(Mutex::new(ClientCounts::default())));
    presence.set(&[ClientChannel::Tui]);
    let env = env(
        &dirs,
        Options {
            clients: Some(presence.clone()),
            ..Options::default()
        },
    )
    .await;
    let session = env.session(&cwd, "").await.unwrap();

    run(&session, "hello").await;
    let text = request_text(requests(&session).await.last().unwrap());
    assert!(
        text.contains(
            "<dim-state source=\"dim/environment/clients\">1 client attached: 1 tui.</dim-state>"
        ),
        "{text}"
    );

    presence.set(&[ClientChannel::Tui, ClientChannel::Tui, ClientChannel::Web]);
    run(&session, "again").await;
    until_block(&session, "3 clients attached: 2 tui, 1 web.").await;
    run(&session, "third").await;
    let text = request_text(requests(&session).await.last().unwrap());
    assert!(
        text.contains(
            "<dim-state source=\"dim/environment/clients\">3 clients attached: 2 tui, 1 web.</dim-state>"
        ),
        "{text}"
    );
    assert_eq!(
        text.matches("3 clients attached: 2 tui, 1 web.").count(),
        1,
        "an unchanged count must not write another block:\n{text}"
    );

    presence.set(&[]);
    run(&session, "done").await;
    until_block(&session, "No client is attached.").await;
    run(&session, "later").await;
    let text = request_text(requests(&session).await.last().unwrap());
    assert!(
        text.contains(
            "<dim-state source=\"dim/environment/clients\">No client is attached.</dim-state>"
        ),
        "{text}"
    );

    env.agent.close().await.unwrap();
}
