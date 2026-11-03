//! An operator's `$skill:` reference loads that Skill with the same input,
//! before the model is asked (`docs/design/plugins/Skills.md`).
use super::*;
use dim_agent_capabilities::adapters::EchoModel;
use dim_agent_capabilities::{
    history::HistoryRenderer,
    host::{HostFuture, ModelConnection, ModelDriver, ModelStream},
    skill_settings::{SkillSettings, SkillSettingsHost},
    skills::{self, MemorySkillSettings, SkillsCatalog},
};
use dim_agent_runtime::PartRegistry;
use dim_agent_types::{ContentBlock, InputMessageData, InputMode, ModelRequest};
use rutis::Ctx;
use rutis::host::{BoxFuture, DynPlugin, Effect, Key, TypeKey};
use std::sync::Mutex;

/// Mounts the Skills catalog in the test host, which has no home plugins,
/// and keeps it so a test can change enablement.
struct CatalogHost {
    platform: Arc<dim_agent_capabilities::host_platform::HostPlatform>,
    settings: Arc<dyn SkillSettings>,
    parts: Key<PartRegistry>,
    catalog: Arc<Mutex<Option<Arc<SkillsCatalog>>>>,
}
impl DynPlugin for CatalogHost {
    fn name(&self) -> &str {
        "SkillReferencesCatalog"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.parts.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let parts = ctx.require_at(&self.parts)?;
            ctx.provide_as::<dim_agent_capabilities::host_platform::HostPlatform>(
                self.platform.clone(),
            )?;
            let catalog = skills::SkillsCatalogPlugin::mount(
                ctx,
                (
                    self.platform.clone(),
                    Arc::new(SkillSettingsHost(self.settings.clone())),
                ),
            )
            .await?;
            ctx.provide_as::<SkillsCatalog>(catalog.clone())?;
            skills::register_parts(ctx, &parts)?;
            *self.catalog.lock().unwrap() = Some(catalog);
            Ok(Effect::Done)
        })
    }
}

/// Answers like the mock model and keeps each request for inspection.
#[derive(Default)]
struct Recording(Mutex<Vec<Arc<ModelRequest>>>);
impl ModelDriver for Recording {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        _: ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        self.0.lock().unwrap().push(request.clone());
        EchoModel.invoke(request, ModelConnection::Local, cancel)
    }
}
impl Recording {
    /// The texts of the last user message of the last request.
    fn last_user_texts(&self) -> Vec<String> {
        let requests = self.0.lock().unwrap();
        let request = requests.last().expect("a model request");
        let messages = serde_json::to_value(&request.messages).unwrap();
        let user = messages.as_array().unwrap().last().unwrap();
        user["content"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|part| part["text"].as_str().map(str::to_owned))
            .collect()
    }
    fn requests(&self) -> usize {
        self.0.lock().unwrap().len()
    }
}

/// A home the product server can own: a canonical native home and its rust
/// root, so a Skill's file path is the one the test wrote.
fn home(name: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let home = std::path::PathBuf::from("/tmp")
        .join(format!("dim-skill-refs-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(home.join("rust")).unwrap();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    home.canonicalize().unwrap()
}

/// Writes the project Skill `name` and returns the project directory.
fn project_skill(
    home: &std::path::Path,
    name: &str,
    description: &str,
    body: &str,
) -> std::path::PathBuf {
    let project = home.join("project");
    let dir = project.join(".agents/skills").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\n\n{body}\n"),
    )
    .unwrap();
    project.canonicalize().unwrap()
}

fn build(
    home: &std::path::Path,
    model: Arc<Recording>,
    catalog: Arc<Mutex<Option<Arc<SkillsCatalog>>>>,
) -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(Arc::new(dim_agent_runtime::MemoryTimelineStore::default()));
    defaults::install_with_prompt_capability(
        &mut builder,
        dim_agent_capabilities::model_provider::ModelProviderPlugin::new,
    );
    builder.session_value(Arc::new(
        dim_agent_capabilities::permission::CommandRulesHost(Arc::new(
            dim_agent_capabilities::permission::NoCommandRules,
        )),
    ));
    defaults::mock_model_hosts_with_model(&mut builder, HistoryRenderer::default(), model);
    dim_agent_capabilities::defaults::tool_host(&mut builder, Arc::new(MockTools));
    dim_app_server::collaboration::install(&mut builder);
    dim_agent_capabilities::prompt_sources::install(&mut builder);
    skills::install(&mut builder);
    let mut platform = dim_agent_capabilities::host_platform::HostPlatform::current(
        dim_agent_capabilities::host_platform::ShellStatus::Unavailable {
            code: "SKILL_REFERENCES",
            reason: "The flow runs no command".into(),
        },
    )
    .unwrap();
    platform.home = home.to_owned();
    let platform = Arc::new(platform);
    let settings: Arc<dyn SkillSettings> = Arc::new(MemorySkillSettings::default());
    builder.plugin({
        let catalog = catalog.clone();
        move |names| {
            Ok(Box::new(CatalogHost {
                platform: platform.clone(),
                settings: settings.clone(),
                parts: names.key::<PartRegistry>()?,
                catalog: catalog.clone(),
            }))
        }
    });
    builder
}

async fn serve_with(builder: AgentBuilder, database: Option<std::path::PathBuf>) -> RunningServer {
    serve(
        builder,
        ServerConfig {
            unix_path: Some(std::path::PathBuf::from("/tmp").join(format!(
                "dim-skill-refs-{}/server.sock",
                uuid::Uuid::new_v4()
            ))),
            database_path: database,
            ..Default::default()
        },
    )
    .await
    .unwrap()
}

async fn launch(
    home: &std::path::Path,
    model: Arc<Recording>,
) -> (RunningServer, Arc<Mutex<Option<Arc<SkillsCatalog>>>>) {
    let catalog: Arc<Mutex<Option<Arc<SkillsCatalog>>>> = Default::default();
    let server = serve_with(build(home, model, catalog.clone()), None).await;
    (server, catalog)
}

/// A server that keeps its Sessions in `database`, for a restart.
async fn launch_in_database(
    home: &std::path::Path,
    model: Arc<Recording>,
    database: &std::path::Path,
) -> (RunningServer, Arc<Mutex<Option<Arc<SkillsCatalog>>>>) {
    let catalog: Arc<Mutex<Option<Arc<SkillsCatalog>>>> = Default::default();
    let server = serve_with(
        build(home, model, catalog.clone()),
        Some(database.to_owned()),
    )
    .await;
    (server, catalog)
}

/// Creates a Session in `cwd`, returning the client, its id and the runtime
/// Session.
async fn session(
    server: &RunningServer,
    cwd: &std::path::Path,
    tools: Value,
) -> (Client, String, dim_agent_runtime::Session) {
    let mut client = Client::new_version(server, false, 3).await;
    let created = client
        .rpc(
            "session.create",
            json!({"cwd": cwd.to_string_lossy(), "tools": tools}),
        )
        .await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = server
        .agent()
        .unwrap()
        .get_session(&id)
        .expect("the created Session");
    (client, id, session)
}

/// The operator inputs of `main`, oldest first.
async fn operator_inputs(session: &dim_agent_runtime::Session) -> Vec<InputMessageData> {
    session
        .local("main", ..)
        .await
        .unwrap()
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::InputMessage(data) if data.user.is_some() => Some(data.clone()),
            _ => None,
        })
        .collect()
}

fn texts(input: &InputMessageData) -> Vec<String> {
    input
        .user
        .as_ref()
        .expect("operator input")
        .content
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text } => text.clone(),
            ContentBlock::Image { .. } => "<image>".into(),
        })
        .collect()
}

/// The text of a Skill block the test wrote.
fn block(name: &str, description: &str, body: &str, file: &std::path::Path) -> String {
    format!(
        "<dim-skill name=\"{name}\">\nSkill: {name}\nDescription: {description}\nFile: {}\n\nInstructions:\n{body}\n</dim-skill>",
        file.display()
    )
}

fn view_version(session: &dim_agent_runtime::Session) -> String {
    session
        .service::<skills::SkillsBranch>("main")
        .unwrap()
        .session()
        .current()
        .unwrap()
        .version
        .clone()
}

async fn wait_view_change(session: &dim_agent_runtime::Session, before: &str) {
    for _ in 0..500 {
        if view_version(session) != before {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the Session's Skill view did not change");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reference_loads_the_skill_with_the_submitted_input() {
    let home = home("submit");
    let project = project_skill(
        &home,
        "checkin",
        "Answers check-in questions",
        "End every reply with the line SKILL-OK-7Q.",
    );
    let model = Arc::new(Recording::default());
    let (server, _) = launch(&home, model.clone()).await;
    let (mut client, id, session) = session(&server, &project, json!([])).await;

    let submitted = client
        .rpc(
            "session.submit",
            json!({"sessionId": id, "branchId": "main", "text": "$skill: checkin what is new?"}),
        )
        .await;
    assert_ok(&submitted);

    let input = operator_inputs(&session).await.pop().expect("the input");
    let sent = texts(&input);
    assert_eq!(
        sent[0], "$skill: checkin what is new?",
        "the operator's own text stays first"
    );
    assert_eq!(sent.len(), 2, "one Skill block follows it: {sent:?}");
    assert_eq!(
        sent[1],
        block(
            "checkin",
            "Answers check-in questions",
            "End every reply with the line SKILL-OK-7Q.",
            &project.join(".agents/skills/checkin/SKILL.md"),
        ),
        "the block carries the skill tool's own text"
    );
    assert!(
        input.blocks.is_empty(),
        "the input carries no raw block, so it stays operator input"
    );

    let idle = client
        .rpc("session.idle", json!({"sessionId": id, "branchId": "main"}))
        .await;
    assert_ok(&idle);
    assert_eq!(model.requests(), 1, "one model request for the input");
    assert_eq!(
        model.last_user_texts(),
        sent,
        "the first request carries the operator's text and the block"
    );
    let results = session
        .local("main", ..)
        .await
        .unwrap()
        .into_iter()
        .filter(|event| matches!(event.kind, EventKind::ToolResult(_)))
        .count();
    assert_eq!(results, 0, "no skill tool call happened before the request");
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(&home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_steer_reference_gets_the_block() {
    let home = home("steer");
    let project = project_skill(&home, "checkin", "Answers check-in questions", "STEER-BODY");
    let model = Arc::new(Recording::default());
    let (server, _) = launch(&home, model.clone()).await;
    let (mut client, id, session) = session(&server, &project, json!([])).await;
    let submitted = client
        .rpc(
            "session.submit",
            json!({"sessionId": id, "branchId": "main", "text": "hello"}),
        )
        .await;
    assert_ok(&submitted);
    let idle = client
        .rpc("session.idle", json!({"sessionId": id, "branchId": "main"}))
        .await;
    assert_ok(&idle);

    let withdrawn = client
        .rpc(
            "session.input.withdraw",
            json!({"sessionId": id, "branchId": "main", "inputId": "unknown",
                "steer": {"content": [{"type": "text", "text": "$skill: checkin more"}]}}),
        )
        .await;
    assert_ok(&withdrawn);
    let idle = client
        .rpc("session.idle", json!({"sessionId": id, "branchId": "main"}))
        .await;
    assert_ok(&idle);

    let inputs = operator_inputs(&session).await;
    let steer = inputs.last().expect("the steer input");
    assert_eq!(steer.mode, Some(InputMode::Steer));
    let sent = texts(steer);
    assert_eq!(sent[0], "$skill: checkin more");
    assert_eq!(sent.len(), 2, "the steer carries the block: {sent:?}");
    assert!(sent[1].starts_with("<dim-skill name=\"checkin\">"));
    assert!(sent[1].contains("STEER-BODY"));
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(&home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_branch_prompt_gets_no_block() {
    let home = home("branch");
    let project = project_skill(
        &home,
        "checkin",
        "Answers check-in questions",
        "BRANCH-BODY",
    );
    let model = Arc::new(Recording::default());
    let (server, _) = launch(&home, model.clone()).await;
    let (mut client, id, session) = session(&server, &project, json!(["branch"])).await;

    let submitted = client
        .rpc(
            "session.submit",
            json!({"sessionId": id, "branchId": "main", "text": "$skill: checkin on main"}),
        )
        .await;
    assert_ok(&submitted);
    // The branch position is read from the settled branch, so the turn's
    // model output cannot still be streaming over it.
    let idle = client
        .rpc("session.idle", json!({"sessionId": id, "branchId": "main"}))
        .await;
    assert_ok(&idle);
    let main_input = operator_inputs(&session).await.pop().expect("the input");
    assert_eq!(
        texts(&main_input).len(),
        2,
        "a submit still carries the block"
    );

    let created = client
        .rpc(
            "session.branch_create",
            json!({"sessionId": id, "parentBranchId": "main",
                "at": session.sequence("main").unwrap(), "prompt": "$skill: checkin child"}),
        )
        .await;
    assert_ok(&created);
    let child = created["result"]["branch"]["branchId"]
        .as_str()
        .unwrap()
        .to_owned();
    let first = session
        .local(&child, ..)
        .await
        .unwrap()
        .into_iter()
        .find_map(|event| match &event.kind {
            EventKind::InputMessage(data) => data.user.clone(),
            _ => None,
        })
        .expect("the branch's first input");
    assert_eq!(
        first.content,
        vec![ContentBlock::Text {
            text: "$skill: checkin child".into()
        }],
        "a branch's first input is its prompt alone"
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(&home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_disabled_or_unknown_name_adds_nothing() {
    let home = home("absent");
    let project = project_skill(&home, "checkin", "Answers check-in questions", "BODY");
    let alpha = project.join(".agents/skills/alpha");
    std::fs::create_dir_all(&alpha).unwrap();
    std::fs::write(
        alpha.join("SKILL.md"),
        "---\nname: alpha\ndescription: Alpha\n---\n\nALPHA-BODY\n",
    )
    .unwrap();
    let model = Arc::new(Recording::default());
    let (server, catalog) = launch(&home, model.clone()).await;
    let (mut client, id, session) = session(&server, &project, json!([])).await;

    let catalog = catalog.lock().unwrap().clone().expect("the catalog");
    let view = catalog.query(Some(&project)).await.unwrap();
    let entry = view
        .entries
        .iter()
        .find(|entry| entry.definition.name == "checkin")
        .expect("the project Skill is listed");
    let skill_id = entry.definition.id.clone();
    let before = view_version(&session);
    catalog
        .set_enabled(&skill_id, false, Some(&project))
        .await
        .unwrap();
    wait_view_change(&session, &before).await;

    let submitted = client
        .rpc(
            "session.submit",
            json!({"sessionId": id, "branchId": "main",
                "text": "$skill: checkin disabled $skill: alpha enabled $skill: missing unknown"}),
        )
        .await;
    assert_ok(&submitted);
    let input = operator_inputs(&session).await.pop().expect("the input");
    let sent = texts(&input);
    assert_eq!(sent.len(), 2, "only the enabled Skill loads: {sent:?}");
    assert!(sent[1].starts_with("<dim-skill name=\"alpha\">"));
    assert!(sent[1].contains("ALPHA-BODY"));
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(&home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repeated_references_load_one_block_each_in_first_reference_order() {
    let home = home("order");
    let project = project_skill(&home, "beta", "Beta", "BETA-BODY");
    let alpha = project.join(".agents/skills/alpha");
    std::fs::create_dir_all(&alpha).unwrap();
    std::fs::write(
        alpha.join("SKILL.md"),
        "---\nname: alpha\ndescription: Alpha\n---\n\nALPHA-BODY\n",
    )
    .unwrap();
    let model = Arc::new(Recording::default());
    let (server, _) = launch(&home, model.clone()).await;
    let (mut client, id, session) = session(&server, &project, json!([])).await;

    let submitted = client
        .rpc(
            "session.submit",
            json!({"sessionId": id, "branchId": "main",
                "text": "$skill: beta one $skill: alpha two $skill: beta three"}),
        )
        .await;
    assert_ok(&submitted);
    let input = operator_inputs(&session).await.pop().expect("the input");
    let sent = texts(&input);
    assert_eq!(sent.len(), 3, "two Skills, each once: {sent:?}");
    assert!(sent[1].starts_with("<dim-skill name=\"beta\">"));
    assert!(sent[1].contains("BETA-BODY"));
    assert!(sent[2].starts_with("<dim-skill name=\"alpha\">"));
    assert!(sent[2].contains("ALPHA-BODY"));
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(&home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restored_session_keeps_the_block_in_its_recorded_input() {
    let home = home("resume");
    let project = project_skill(
        &home,
        "checkin",
        "Answers check-in questions",
        "RESUME-BODY",
    );
    let database = home.join("dimcode.sqlite");
    let model = Arc::new(Recording::default());
    let (server, _) = launch_in_database(&home, model.clone(), &database).await;
    let mut client = Client::new_version(&server, true, 3).await;
    let created = client
        .rpc(
            "session.create",
            json!({"cwd": project.to_string_lossy(), "tools": []}),
        )
        .await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = server
        .agent()
        .unwrap()
        .get_session(&id)
        .expect("the created Session");
    let submitted = client
        .rpc(
            "session.submit",
            json!({"sessionId": id, "branchId": "main", "text": "$skill: checkin resume"}),
        )
        .await;
    assert_ok(&submitted);
    let idle = client
        .rpc("session.idle", json!({"sessionId": id, "branchId": "main"}))
        .await;
    assert_ok(&idle);
    let recorded = operator_inputs(&session).await.pop().expect("the input");
    assert_eq!(texts(&recorded).len(), 2, "the submit carries the block");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    drop(session);

    let (server, _) = launch_in_database(&home, model, &database).await;
    let mut client = Client::new_version(&server, true, 3).await;
    assert_ok(&client.rpc("session.attach", json!({"sessionId": id})).await);
    let restored = server
        .agent()
        .unwrap()
        .get_session(&id)
        .expect("the restored Session");
    let inputs = operator_inputs(&restored).await;
    let input = inputs
        .iter()
        .find(|input| texts(input)[0] == "$skill: checkin resume")
        .expect("the recorded input is restored");
    let sent = texts(input);
    assert_eq!(sent.len(), 2, "the block survives the restart");
    assert!(sent[1].starts_with("<dim-skill name=\"checkin\">"));
    assert!(sent[1].contains("RESUME-BODY"));
    assert_eq!(
        sent,
        texts(&recorded),
        "the restored record is the recorded one"
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(&home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn steering_a_queued_reference_keeps_one_block() {
    let home = home("steer-again");
    let project = project_skill(
        &home,
        "checkin",
        "Answers check-in questions",
        "AGAIN-BODY $skill: second",
    );
    project_skill(&home, "second", "Answers follow-ups", "SECOND-BODY");
    let model = Arc::new(Recording::default());
    let (server, _) = launch(&home, model.clone()).await;
    let (mut client, id, session) = session(&server, &project, json!([])).await;
    let submitted = client
        .rpc(
            "session.submit",
            json!({"sessionId": id, "branchId": "main", "text": "$skill: checkin again"}),
        )
        .await;
    assert_ok(&submitted);
    let recorded = operator_inputs(&session).await.pop().expect("the input");
    let user = recorded.user.clone().expect("operator input");
    assert_eq!(texts(&recorded).len(), 2, "the submit carries the block");
    assert!(
        !texts(&recorded)
            .iter()
            .any(|text| text.contains("SECOND-BODY")),
        "the submit loads only the operator's reference: {:?}",
        texts(&recorded)
    );

    // A queued input is steered with the content it was submitted with.
    let withdrawn = client
        .rpc(
            "session.input.withdraw",
            json!({"sessionId": id, "branchId": "main", "inputId": "unknown",
                "steer": serde_json::to_value(&user).unwrap()}),
        )
        .await;
    assert_ok(&withdrawn);
    let steer = operator_inputs(&session)
        .await
        .pop()
        .expect("the steer input");
    let sent = texts(&steer);
    assert_eq!(
        sent.len(),
        2,
        "only the operator's own reference loads a Skill: {sent:?}"
    );
    assert_eq!(sent, texts(&recorded));
    assert!(
        !sent.iter().any(|text| text.contains("SECOND-BODY")),
        "a steer follows no reference in a loaded block: {sent:?}"
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(&home).unwrap();
}
