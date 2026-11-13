mod local_support;

use dim_agent_capabilities::environment_clients::{
    ClientChannel, ClientPresence, ClientsChanged, SessionPresences, clients_changes,
};
use dim_agent_runtime::Session;
use local_support::*;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A mutable presence source: the test changes the connections, as another
/// client would, and announces the change on the process bus.
struct Presence(Mutex<Vec<ClientPresence>>);
impl Presence {
    fn set(&self, presences: Vec<ClientPresence>) {
        *self.0.lock().unwrap() = presences;
    }
    fn presences(&self) -> Vec<ClientPresence> {
        self.0.lock().unwrap().clone()
    }
    fn host(self: &Arc<Self>) -> SessionPresences {
        let presence = self.clone();
        Arc::new(move |_| presence.presences())
    }
}

fn connection(channel: ClientChannel, label: Option<&str>) -> ClientPresence {
    ClientPresence {
        channel,
        label: label.map(str::to_owned),
    }
}

fn changed(env: &Env, session: &Session) {
    env.root
        .emit(ClientsChanged {
            name: clients_changes(session.id()),
            session_id: session.id().to_owned(),
        })
        .unwrap();
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
    .expect("presence was appended");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn presence_is_appended_and_unchanged_counts_do_not_duplicate() {
    let dirs = Dirs::new();
    let cwd = dirs.dir("work");
    let presence = Arc::new(Presence(Mutex::new(Vec::new())));
    presence.set(vec![connection(ClientChannel::Tui, Some("eric@laptop"))]);
    let env = env(
        &dirs,
        Options {
            clients: Some(presence.host()),
            ..Options::default()
        },
    )
    .await;
    let session = env.session(&cwd, "").await.unwrap();

    run(&session, "hello").await;
    until_block(&session, "1 client connection attached:").await;
    run(&session, "hello again").await;
    let text = request_text(requests(&session).await.last().unwrap());
    assert!(text.contains("1 client connection attached:"), "{text}");
    assert!(text.contains("- tui \"eric@laptop\" (claimed)"), "{text}");

    presence.set(vec![
        connection(ClientChannel::Tui, Some("eric@laptop")),
        connection(ClientChannel::Tui, None),
        connection(ClientChannel::Web, Some("alice")),
    ]);
    changed(&env, &session);
    run(&session, "again").await;
    until_block(&session, "3 client connections attached:").await;
    run(&session, "third").await;
    let text = request_text(requests(&session).await.last().unwrap());
    assert!(text.contains("- tui \"eric@laptop\" (claimed)"), "{text}");
    assert!(text.contains("- tui\n"), "{text}");
    assert!(text.contains("- web \"alice\" (claimed)"), "{text}");
    assert_eq!(
        text.matches("3 client connections attached:").count(),
        1,
        "an unchanged presence must not write another block:\n{text}"
    );

    presence.set(Vec::new());
    changed(&env, &session);
    run(&session, "done").await;
    until_block(&session, "No client connection is attached.").await;
    run(&session, "later").await;
    let text = request_text(requests(&session).await.last().unwrap());
    assert!(
        text.contains(
            "<dim-state source=\"dim/environment/clients\">No client connection is attached.</dim-state>"
        ),
        "{text}"
    );

    env.agent.close().await.unwrap();
}
