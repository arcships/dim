//! The connection-bound change stream of `schedule.watch` (design §11.2)
//! through a real App Server: the call stays open, a change reaches the
//! watching connection alone as one numbered progress item, run states in
//! the order the record took them, nothing after the connection closes, and
//! a reload answers the open call `operation_interrupted`.
mod support;

use dim_agent_capabilities::{defaults, history::HistoryRenderer};
use dim_agent_runtime::{AgentBuilder, AgentConfig};
use dim_app_server::{RunningServer, ServerConfig, serve};
use dim_client::{Client, Message};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, TypeKey};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::{Arc, OnceLock},
    time::Duration,
};

/// A `management.progress` item of a `schedule.watch` call: its call's id
/// and the item's data.
fn watch_item(method: &str, params: &Value) -> Option<(u64, Value)> {
    (method == "management.progress" && params["method"] == json!("schedule.watch")).then(|| {
        (
            params["id"].as_u64().expect("a request id"),
            params["data"].clone(),
        )
    })
}

/// The next `schedule.watch` progress item on this connection, with its call.
async fn next_item(client: &mut Client) -> (u64, Value) {
    loop {
        let Message::Notice { method, params } = client.next().await.unwrap() else {
            continue;
        };
        if let Some(item) = watch_item(&method, &params) {
            return item;
        }
    }
}

/// The next item of the watch call `id`.
async fn wait_item(client: &mut Client, id: u64, what: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let (received, item) = next_item(client).await;
            if received == id {
                return item;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

/// The first item of a watch call: `{seq: 0, kind: "watching"}`.
async fn wait_watching(client: &mut Client, id: u64) -> Value {
    let item = wait_item(client, id, "the watching item").await;
    assert_eq!(item, json!({"kind": "watching", "seq": 0}), "{item}");
    item
}

/// Asserts that no `schedule.watch` item arrives within `window`.
async fn no_item(client: &mut Client, window: Duration, what: &str) {
    let seen = tokio::time::timeout(window, next_item(client)).await;
    assert!(seen.is_err(), "{what}: {seen:?}");
}

/// Saves one job through Settings and returns its id; the write is the
/// `schedule` source's change the jobs item announces.
async fn save_job(client: &mut Client, name: &str, cwd: &Path) -> String {
    let saved: Value = client
        .typed(
            "schedule.save",
            json!({"job": {"name": name, "schedule": {"at": "2030-01-01T00:00:00Z"},
                "prompt": "Report the day.",
                "target": {"kind": "launch", "cwd": cwd}}}),
        )
        .await
        .unwrap();
    saved["job"]["id"].as_str().unwrap().to_owned()
}

/// Only the watching connection receives items: `seq` 0 `watching`, then
/// `{seq: 1, kind: "jobs"}` after a job is added through Settings.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_jobs_change_reaches_only_the_watching_connection() {
    let home = support::home();
    let model = Arc::new(support::Script::default());
    let server = support::start(home.path(), model).await;
    let mut a = support::connect(&server).await;
    let mut b = support::connect(&server).await;
    let watch = a.send("schedule.watch", json!({})).await.unwrap();
    wait_watching(&mut a, watch).await;

    save_job(&mut a, "nightly", home.path()).await;
    let item = wait_item(&mut a, watch, "the jobs item").await;
    assert_eq!(item, json!({"kind": "jobs", "seq": 1}), "{item}");

    // B never watched: its next reply is preceded by no schedule.watch item.
    let id = b.send("schedule.list", json!({})).await.unwrap();
    b.response(id).await.unwrap();
    let leaked = b.notices.iter().any(|message| {
        matches!(message, Message::Notice { method, params }
            if watch_item(method, params).is_some())
    });
    assert!(!leaked, "{:?}", b.notices);

    drop((a, b));
    server.shutdown();
    server.join().await.unwrap();
}

/// A later watch takes over: the first call answers `{}`, and one change
/// sends one item, to the second call.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn watching_twice_answers_the_first_and_sends_one_item() {
    let home = support::home();
    let model = Arc::new(support::Script::default());
    let server = support::start(home.path(), model).await;
    let mut client = support::connect(&server).await;
    let first = client.send("schedule.watch", json!({})).await.unwrap();
    wait_watching(&mut client, first).await;
    let second = client.send("schedule.watch", json!({})).await.unwrap();
    assert_eq!(client.response(first).await.unwrap(), json!({}));
    wait_watching(&mut client, second).await;

    save_job(&mut client, "nightly", home.path()).await;
    let (id, item) = next_item(&mut client).await;
    assert_eq!(id, second, "the item goes to the watch that took over");
    assert_eq!(item, json!({"kind": "jobs", "seq": 1}), "{item}");
    no_item(
        &mut client,
        Duration::from_millis(750),
        "a second item for one change",
    )
    .await;

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// The run's record states reach the watcher in order, with consecutive
/// `seq`, ending terminal.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn run_states_reach_the_watcher_in_order() {
    let home = support::home();
    let model = Arc::new(support::Script::default());
    let server = support::start(home.path(), model).await;
    let mut client = support::connect(&server).await;
    let watch = client.send("schedule.watch", json!({})).await.unwrap();
    wait_watching(&mut client, watch).await;
    let job_id = save_job(&mut client, "nightly", home.path()).await;
    let accepted: Value = client
        .typed("schedule.run_now", json!({"job_id": job_id}))
        .await
        .unwrap();
    let run_id = accepted["run_id"].as_str().unwrap().to_owned();

    let mut expected = 1;
    let mut states = Vec::new();
    loop {
        let item = wait_item(&mut client, watch, "the next item").await;
        assert_eq!(item["seq"], json!(expected), "{item}");
        expected += 1;
        if item["kind"] != json!("run") || item["runId"] != json!(run_id) {
            continue;
        }
        assert_eq!(item["jobId"], json!(job_id), "{item}");
        let state = item["state"].as_str().unwrap().to_owned();
        let terminal = item["terminal"].as_bool().unwrap();
        assert_eq!(
            terminal,
            matches!(state.as_str(), "delivered" | "failed" | "lost" | "missed"),
            "{item}"
        );
        states.push(state);
        if terminal {
            break;
        }
    }
    assert_eq!(
        states,
        ["delivering", "delivered"],
        "one item per state the record took, in order"
    );
    let settled = support::settled(&mut client, &job_id, &run_id).await;
    assert_eq!(settled["state"], json!("delivered"), "{settled}");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// Concurrent jobs and run sources keep one watch's items in `seq` order:
/// several jobs are saved through Settings while runs that started together
/// move through their states, and every item arrives with the next number.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_sources_deliver_each_watch_item_in_sequence() {
    const RUNS: usize = 6;
    const SAVES: usize = 6;

    let home = support::home();
    let model = Arc::new(support::Script::default());
    let server = support::start(home.path(), model).await;
    let mut watcher = support::connect(&server).await;
    let watch = watcher.send("schedule.watch", json!({})).await.unwrap();
    wait_watching(&mut watcher, watch).await;

    // The jobs the concurrent run tasks use; their items are read first.
    let mut driver = support::connect(&server).await;
    let mut jobs = Vec::new();
    for name in ["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"] {
        jobs.push(save_job(&mut driver, name, home.path()).await);
    }
    for expected in 1..=jobs.len() as u64 {
        let item = wait_item(&mut watcher, watch, "the setup jobs item").await;
        assert_eq!(item, json!({"kind": "jobs", "seq": expected}), "{item}");
    }

    // The run tasks and the save driver start together, so the jobs source
    // and the run sources emit concurrently on the same watch.
    let mut run_clients = Vec::new();
    for _ in 0..RUNS {
        run_clients.push(support::connect(&server).await);
    }
    let save_client = support::connect(&server).await;
    let barrier = Arc::new(tokio::sync::Barrier::new(RUNS + 1));
    let mut writers = Vec::new();
    for (client, job_id) in run_clients.into_iter().zip(jobs.iter().cloned()) {
        let barrier = barrier.clone();
        writers.push(tokio::spawn(async move {
            let mut client = client;
            barrier.wait().await;
            let accepted: Value = client
                .typed("schedule.run_now", json!({"job_id": job_id}))
                .await
                .unwrap();
            Some(accepted["run_id"].as_str().unwrap().to_owned())
        }));
    }
    {
        let barrier = barrier.clone();
        let cwd = home.path().to_owned();
        writers.push(tokio::spawn(async move {
            let mut client = save_client;
            barrier.wait().await;
            for index in 0..SAVES {
                save_job(&mut client, &format!("extra-{index}"), &cwd).await;
            }
            None
        }));
    }

    // Every item after the setup arrives in arrival order; its number must be
    // exactly the next one, with no gap and no inversion.
    let mut expected = jobs.len() as u64 + 1;
    let mut jobs_items = 0;
    let mut run_items = 0;
    let mut runs: HashMap<String, Vec<(String, bool)>> = HashMap::new();
    while jobs_items < SAVES || run_items < RUNS * 2 {
        let item = wait_item(&mut watcher, watch, "the next concurrent item").await;
        assert_eq!(item["seq"], json!(expected), "{item}");
        expected += 1;
        if item["kind"] == json!("jobs") {
            jobs_items += 1;
            continue;
        }
        assert_eq!(item["kind"], json!("run"), "{item}");
        let run_id = item["runId"].as_str().unwrap().to_owned();
        let state = item["state"].as_str().unwrap().to_owned();
        let terminal = item["terminal"].as_bool().unwrap();
        runs.entry(run_id).or_default().push((state, terminal));
        run_items += 1;
    }
    assert_eq!(jobs_items, SAVES, "one item per saved job");
    assert_eq!(runs.len(), RUNS, "one run per accepted run_now");
    for (run_id, states) in &runs {
        let names: Vec<&str> = states.iter().map(|(state, _)| state.as_str()).collect();
        assert_eq!(
            names,
            ["delivering", "delivered"],
            "run {run_id}: {states:?}"
        );
        let terminals: Vec<bool> = states.iter().map(|(_, terminal)| *terminal).collect();
        assert_eq!(terminals, [false, true], "run {run_id}");
    }

    let mut replied = HashSet::new();
    for writer in writers {
        if let Some(run_id) = writer.await.unwrap() {
            replied.insert(run_id);
        }
    }
    assert_eq!(runs.keys().cloned().collect::<HashSet<_>>(), replied);

    drop((watcher, driver));
    server.shutdown();
    server.join().await.unwrap();
}

/// A closed connection fails nothing, and another watcher still gets its
/// item: the gone connection's entry is dropped instead of erroring on every
/// later change, and the survivor's changes keep arriving.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_closed_connection_fails_nothing_and_another_watcher_gets_its_item() {
    let home = support::home();
    let model = Arc::new(support::Script::default());
    let server = support::start(home.path(), model).await;
    let mut gone = support::connect(&server).await;
    let mut survivor = support::connect(&server).await;
    let gone_watch = gone.send("schedule.watch", json!({})).await.unwrap();
    let survivor_watch = survivor.send("schedule.watch", json!({})).await.unwrap();
    wait_watching(&mut gone, gone_watch).await;
    wait_watching(&mut survivor, survivor_watch).await;
    drop(gone);
    // The close is observed and the entry removed by the watch's own call;
    // the change below is delivered to the survivor whether or not that
    // removal has run yet, which is the contract.
    tokio::time::sleep(Duration::from_millis(100)).await;

    save_job(&mut survivor, "nightly", home.path()).await;
    let item = wait_item(&mut survivor, survivor_watch, "the survivor's jobs item").await;
    assert_eq!(item, json!({"kind": "jobs", "seq": 1}), "{item}");
    // The change after the close failed nothing: the next one is served too.
    save_job(&mut survivor, "morning", home.path()).await;
    let item = wait_item(
        &mut survivor,
        survivor_watch,
        "the survivor's next jobs item",
    )
    .await;
    assert_eq!(item, json!({"kind": "jobs", "seq": 2}), "{item}");

    drop(survivor);
    server.shutdown();
    server.join().await.unwrap();
}

/// A Scheduler reload answers the open watch call `operation_interrupted`,
/// and a watch made after the reload starts again at `seq` 0.
///
/// A watch belongs to the generation that admitted it, and the stopping
/// generation cannot announce the end itself: rutis cancels a consumer's
/// generation token before any listener or task of the stopping generation
/// can run (the status event is dispatched asynchronously), and its callers
/// answer `gone` from then on (methods contract A.2). The end a client can
/// observe is the call's `operation_interrupted` reply. After the client
/// watches again, the namespace answers `service_reloading` (retryable)
/// until the new generation serves it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reload_answers_the_open_call_and_a_new_watch_starts_at_zero() {
    let home = support::home();
    let model = Arc::new(support::Script::default());
    let probe = Arc::new(OnceLock::new());
    let server = start_with_probe(home.path(), model, probe.clone()).await;
    let mut client = support::connect(&server).await;
    let first = client.send("schedule.watch", json!({})).await.unwrap();
    wait_watching(&mut client, first).await;

    // The Scheduler reloads when a row it injects restarts: PluginFiles
    // provides its PluginFileTransactions. The restart is the row's own, so
    // the test drives it through the probe's runtime context.
    let generation = scheduler_generation(&server);
    let files = server
        .diagnostics()
        .plugins
        .iter()
        .find(|plugin| plugin.name == "PluginFiles")
        .map(|plugin| plugin.id)
        .expect("the PluginFiles row");
    probe
        .get()
        .expect("the probe applied")
        .view(files)
        .expect("the PluginFiles fiber")
        .restart()
        .await
        .unwrap();
    support::wait_for("the Scheduler reloads", || {
        scheduler_generation(&server) > generation
    })
    .await;

    let error = client.response(first).await.unwrap_err();
    let text = format!("{error}");
    assert!(
        text.contains("operation_interrupted"),
        "the open call ends with the generation: {text}"
    );

    wait_served(&mut client).await;
    let second = client.send("schedule.watch", json!({})).await.unwrap();
    wait_watching(&mut client, second).await;
    save_job(&mut client, "after the reload", home.path()).await;
    let item = wait_item(&mut client, second, "the jobs item after re-watching").await;
    assert_eq!(item, json!({"kind": "jobs", "seq": 1}), "{item}");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// A process plugin that keeps the runtime context, so the test can restart a
/// row by name the way a reload does.
struct Probe(Arc<OnceLock<Ctx>>);

impl DynPlugin for Probe {
    fn name(&self) -> &str {
        "watch-reload-probe"
    }
    fn injects(&self) -> Vec<TypeKey> {
        Vec::new()
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let _ = self.0.set(ctx.clone());
            Ok(Effect::Done)
        })
    }
}

/// [`support::start`] with the probe that exposes the runtime context.
async fn start_with_probe(
    home: &Path,
    model: Arc<support::Script>,
    probe: Arc<OnceLock<Ctx>>,
) -> RunningServer {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    defaults::mock_hosts_with_model(&mut builder, HistoryRenderer::default(), model);
    dim_agent_scheduler::install(&mut builder);
    builder.plugin(move |_| Ok(Box::new(Probe(probe.clone())) as Box<dyn DynPlugin>));
    serve(
        builder,
        ServerConfig {
            startup_timeout: Duration::from_secs(60),
            database_path: Some(home.join("dimcode.sqlite")),
            product_home: Some(home.to_owned()),
            unix_path: Some(home.join("native.sock")),
            ..Default::default()
        },
    )
    .await
    .unwrap()
}

fn scheduler_generation(server: &RunningServer) -> u64 {
    server
        .diagnostics()
        .plugins
        .iter()
        .find(|plugin| plugin.name == "Scheduler")
        .map(|plugin| plugin.generation)
        .unwrap_or(0)
}

/// Waits until the `schedule` namespace answers again, since an owner reloads
/// between generations.
async fn wait_served(client: &mut Client) {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match client.rpc("schedule.list", json!({})).await {
                Ok(_) => return,
                Err(error) => {
                    let text = format!("{error}");
                    if !text.contains("service_reloading") && !text.contains("unknown_method") {
                        return;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the schedule namespace is served");
}

/// A `session` delivery streams the new vocabulary: `delivering`, then
/// `delivered` as the terminal item, one item per state.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_session_delivery_reaches_the_watcher_with_the_new_vocabulary() {
    let home = support::home();
    let model = Arc::new(support::Script::default());
    let server = support::start(home.path(), model).await;
    let mut client = support::connect(&server).await;
    let session = support::create_session(&mut client, home.path(), true).await;
    let watch = client.send("schedule.watch", json!({})).await.unwrap();
    wait_watching(&mut client, watch).await;

    let saved: Value = client
        .typed(
            "schedule.save",
            json!({"job": {"name": "inbox", "schedule": {"at": "2030-01-01T00:00:00Z"},
                "prompt": "Report the day.",
                "target": {"kind": "session", "session_id": session, "branch_id": "main"}}}),
        )
        .await
        .unwrap();
    let job_id = saved["job"]["id"].as_str().unwrap().to_owned();
    let accepted: Value = client
        .typed("schedule.run_now", json!({"job_id": job_id}))
        .await
        .unwrap();
    let run_id = accepted["run_id"].as_str().unwrap().to_owned();

    let mut states = Vec::new();
    loop {
        let item = wait_item(&mut client, watch, "the next item").await;
        if item["kind"] != json!("run") || item["runId"] != json!(run_id) {
            continue;
        }
        let state = item["state"].as_str().unwrap().to_owned();
        let terminal = item["terminal"].as_bool().unwrap();
        assert_eq!(terminal, state == "delivered", "{item}");
        states.push(state);
        if terminal {
            break;
        }
    }
    assert_eq!(
        states,
        ["delivering", "delivered"],
        "one item per state the record took, in order"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}
