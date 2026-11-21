//! `session.model.get`: one line's adopted configuration at that line's own
//! cutoff.
use super::*;
use dim_agent_runtime::{ConfigureUpdate, Timeline, TimelineIdentity};
use dim_agent_types::{ContextConfiguredData, ModelConfiguration};
use rutis::Ctx;
use rutis::host::{BoxFuture, DynPlugin, Effect, Key, TypeKey};

fn create() -> Value {
    json!({
        "cwd": std::env::temp_dir(),
        "tools": ["branch"],
        "permissionPolicy": {"allowedTools": ["branch"]}
    })
}

fn configured(model: &str) -> ContextConfiguredData {
    ContextConfiguredData {
        timeline: "main".into(),
        revision: model.into(),
        previous: None,
        prompt: None,
        settings: Box::new(ModelConfiguration {
            provider_id: "mock".into(),
            model_id: model.into(),
            capability_snapshot: None,
            tools: None,
            tool_sources: vec![],
            config: None,
            metadata: None,
        }),
        system: None,
        system_source: None,
    }
}

/// The line's last published rest point, as `session.history` reads it.
async fn head(client: &mut Client, session: &str, branch: &str) -> u64 {
    let page = client
        .rpc(
            "session.history",
            json!({"sessionId":session,"branchId":branch,"after":0,"limit":256}),
        )
        .await;
    assert_ok(&page);
    page["result"]["through"].as_u64().unwrap()
}

/// Opens a child of `parent` at its last rest point.
async fn open_line(client: &mut Client, session: &str, parent: &str, prompt: &str) -> String {
    let at = head(client, session, parent).await;
    let created = client
        .rpc(
            "session.branch_create",
            json!({"sessionId":session,"parentBranchId":parent,"at":at,"prompt":prompt}),
        )
        .await;
    assert_ok(&created);
    created["result"]["branch"]["branchId"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn directory(client: &mut Client, session: &str) -> Vec<Value> {
    let reply = client
        .rpc("session.branches", json!({"sessionId":session}))
        .await;
    assert_ok(&reply);
    reply["result"]["branches"].as_array().unwrap().clone()
}

fn row<'a>(rows: &'a [Value], id: &str) -> &'a Value {
    rows.iter()
        .find(|row| row["branchId"] == id)
        .unwrap_or_else(|| panic!("{id} is missing: {rows:?}"))
}

async fn adopt(session: &dim_agent_runtime::Session, branch: &str, model: &str) {
    session
        .configure(
            ConfigureUpdate {
                model: Some(dim_agent_types::ModelSelection {
                    provider_id: "mock".into(),
                    model_id: model.into(),
                    capability_snapshot: None,
                    config: None,
                    metadata: None,
                }),
                ..Default::default()
            },
            branch,
        )
        .await
        .unwrap();
}

/// `session.model.get` at an open line's current head, or at a cutoff.
async fn read(client: &mut Client, session: &str, branch: &str, through: Option<u64>) -> Value {
    let mut params = json!({"sessionId":session,"branchId":branch});
    if let Some(through) = through {
        params["through"] = json!(through);
    }
    client.rpc("session.model.get", params).await
}

fn model_of(reply: &Value) -> &str {
    assert_ok(reply);
    reply["result"]["configuration"]["model"]["modelId"]
        .as_str()
        .unwrap_or_else(|| panic!("a configuration: {reply}"))
}

fn through_of(reply: &Value) -> u64 {
    assert_ok(reply);
    reply["result"]["through"].as_u64().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_line_answers_its_own_configuration_and_its_own_cutoff() {
    let (server, agent) = launch_with_agent(Arc::new(MockTools), ServerConfig::default()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert!(
        client
            .features
            .as_array()
            .unwrap()
            .contains(&json!("session.model.get")),
        "the feature is advertised: {:?}",
        client.features
    );
    let created = client.rpc("session.create", create()).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = agent.get_session(&id).unwrap();
    session.idle("main").await.unwrap();
    // Grow the root well past the child's own sequence range, so a cutoff
    // borrowed from the parent would visibly swallow the child's facts.
    for turn in 0..4 {
        assert_ok(
            &client
                .rpc(
                    "session.submit",
                    json!({"sessionId":id,"branchId":"main","text":format!("turn {turn}")}),
                )
                .await,
        );
        assert_ok(
            &client
                .rpc("session.idle", json!({"sessionId":id,"branchId":"main"}))
                .await,
        );
    }
    adopt(&session, "main", "alpha").await;

    let root = read(&mut client, &id, "main", None).await;
    assert_eq!(model_of(&root), "alpha");
    assert_eq!(root["result"]["branchId"], "main");
    let root_through = through_of(&root);

    let child = open_line(&mut client, &id, "main", "worker work").await;
    session.idle(&child).await.unwrap();
    adopt(&session, &child, "beta").await;
    let rows = directory(&mut client, &id).await;
    let cut = row(&rows, &child)["branchPoint"].as_u64().unwrap();

    let line = read(&mut client, &id, &child, None).await;
    assert_eq!(model_of(&line), "beta");
    assert_eq!(line["result"]["branchId"], child);
    let line_through = through_of(&line);
    assert!(
        line_through < cut,
        "the line's cutoff {line_through} stays below the parent's cut {cut}"
    );
    assert!(
        line_through < root_through,
        "the line's cutoff {line_through} is not the root's {root_through}"
    );

    let root = read(&mut client, &id, "main", None).await;
    assert_eq!(model_of(&root), "alpha");
    assert!(
        through_of(&root) >= root_through,
        "the root's head only moves forward"
    );

    // A client that applies only this line's `Configuration` events above the
    // returned cutoff still takes the next change.
    adopt(&session, &child, "gamma").await;
    let changed = read(&mut client, &id, &child, None).await;
    assert_eq!(model_of(&changed), "gamma");
    assert!(
        through_of(&changed) > line_through,
        "the later adoption is above the earlier cutoff"
    );

    drop(client);
    drop(agent);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_ended_line_reads_at_its_fixed_head_and_an_unknown_line_is_an_error() {
    let (server, agent) = launch_with_agent(Arc::new(MockTools), ServerConfig::default()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", create()).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = agent.get_session(&id).unwrap();
    session.idle("main").await.unwrap();
    adopt(&session, "main", "alpha").await;
    let child = open_line(&mut client, &id, "main", "worker work").await;
    session.idle(&child).await.unwrap();
    adopt(&session, &child, "beta").await;

    assert_ok(
        &client
            .rpc(
                "session.branch_end",
                json!({"sessionId":id,"branchId":child}),
            )
            .await,
    );
    let rows = directory(&mut client, &id).await;
    let ended = row(&rows, &child);
    assert_eq!(ended["state"], "ended", "{rows:?}");
    let fixed = ended["head"].as_u64().unwrap();

    let line = read(&mut client, &id, &child, None).await;
    assert_eq!(model_of(&line), "beta");
    assert_eq!(through_of(&line), fixed);

    let earlier = read(&mut client, &id, &child, Some(1)).await;
    assert_eq!(model_of(&earlier), "alpha");
    assert_eq!(through_of(&earlier), 1);

    let after = directory(&mut client, &id).await;
    assert_eq!(
        row(&after, &child)["state"],
        "ended",
        "the read reopens nothing"
    );
    assert_eq!(row(&after, &child)["head"].as_u64().unwrap(), fixed);

    for through in [None, Some(1)] {
        let unknown = read(&mut client, &id, "no-such-line", through).await;
        assert_eq!(
            unknown["error"]["data"]["appError"], "session_invalid_input",
            "{unknown}"
        );
        assert!(
            unknown["error"]["message"]
                .as_str()
                .unwrap()
                .contains("unknown branch"),
            "{unknown}"
        );
    }

    drop(client);
    drop(agent);
    server.shutdown();
    server.join().await.unwrap();
}

/// A test branch part that hands the root's Timeline to the test, so it can
/// commit facts without waiting for a producer.
struct TimelineProbePart {
    timeline: Key<Timeline>,
    identity: Key<TimelineIdentity>,
    probe: Arc<std::sync::Mutex<Option<Arc<Timeline>>>>,
}
impl DynPlugin for TimelineProbePart {
    fn name(&self) -> &str {
        "TimelineProbe"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.timeline.erase(), self.identity.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            if ctx.require_at(&self.identity)?.branch_id == "main" {
                *self.probe.lock().unwrap() = Some(timeline);
            }
            Ok(Effect::Done)
        })
    }
}

fn timeline_probe(
    probe: Arc<std::sync::Mutex<Option<Arc<Timeline>>>>,
) -> impl FnOnce(&mut AgentBuilder) + Send + Sync + 'static {
    move |builder| {
        builder.require_feature(Feature::new("TimelineProbe").branch(
            "TimelineProbe",
            move |names| {
                Ok(Box::new(TimelineProbePart {
                    timeline: names.key()?,
                    identity: names.key()?,
                    probe: probe.clone(),
                }))
            },
        ));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_configuration_committed_just_before_the_read_is_included() {
    let probe = Arc::new(std::sync::Mutex::new(None));
    let (server, agent) = launch_with(
        Arc::new(MockTools),
        ServerConfig::default(),
        timeline_probe(probe.clone()),
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", create()).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = agent.get_session(&id).unwrap();
    session.idle("main").await.unwrap();
    adopt(&session, "main", "alpha").await;

    let timeline = probe.lock().unwrap().clone().expect("the root's Timeline");
    let written = timeline
        .append_batch_registered(
            vec![
                EventKind::ContextConfigured(configured("hot-1")).into(),
                EventKind::ContextConfigured(configured("hot-2")).into(),
            ],
            |_| Ok(()),
        )
        .unwrap();
    let last = written.last().unwrap().sequence;

    let reply = read(&mut client, &id, "main", None).await;
    assert_eq!(model_of(&reply), "hot-2");
    assert_eq!(through_of(&reply), last);

    drop(client);
    drop(agent);
    server.shutdown();
    server.join().await.unwrap();
}
