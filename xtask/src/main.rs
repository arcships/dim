mod allocation;
mod app_server;
mod catalog;
mod modules;
mod plugins;
mod prompt_eval;
mod rule;
mod scenario_eval;
mod summary_eval;
mod types;
#[global_allocator]
static ALLOCATOR: allocation::MeasuredAllocator = allocation::MeasuredAllocator;
use dim_agent_runtime::{testing::MinimalHost, *};
use rutis::host::CtxHost;
use rutis::host::diagnostics::RuntimeDiagnostics;
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Instant};

#[cfg(target_os = "linux")]
unsafe extern "C" {
    fn malloc_trim(pad: usize) -> i32;
    fn mallopt(param: i32, value: i32) -> i32;
}

fn trim_heap() -> Option<i32> {
    #[cfg(target_os = "linux")]
    unsafe {
        Some(malloc_trim(0))
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    #[cfg(target_os = "linux")]
    unsafe {
        mallopt(-3, 64 * 1024);
        mallopt(-8, 1);
    }
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?
        .block_on(run())
}

async fn run() -> std::result::Result<(), Box<dyn std::error::Error>> {
    match std::env::args().nth(1).as_deref() {
        Some("graph") => graph().await?,
        Some("measure") => measure().await?,
        Some("context-replay") => context_replay().await?,
        Some("restore-replay") => restore_replay().await?,
        Some("context-breakdown") => context_breakdown().await?,
        Some("summary-eval") => summary_eval::run().await?,
        Some("scenario-eval") => scenario_eval::run().await?,
        Some("prompt-eval") => prompt_eval::run().await?,
        Some("check") => {
            boundaries(false)?;
            catalog::check(&root()).await?;
            types::check(&root())?;
        }
        Some("types") => types::write(&root())?,
        _ => {
            return Err(
                "usage: cargo run -p xtask -- graph|measure [--model-config PATH]|context-replay|restore-replay DB SESSION|context-breakdown DB SESSION|summary-eval [OPTIONS]|scenario-eval [OPTIONS]|check|types"
                    .into(),
            );
        }
    }
    Ok(())
}
async fn agent() -> dim_agent_runtime::Result<MinimalHost> {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    dim_agent_capabilities::defaults::mock_hosts(&mut builder);
    dim_agent_capabilities::defaults::install(&mut builder);
    MinimalHost::start(&builder).await
}
async fn agent_with_store(store: Arc<dyn TimelineStore>) -> dim_agent_runtime::Result<MinimalHost> {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    dim_agent_capabilities::defaults::mock_hosts(&mut builder);
    dim_agent_capabilities::defaults::install(&mut builder);
    builder.timeline_store(store);
    MinimalHost::start(&builder).await
}
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}
/// Instance identities are allocated per process; the graph names their scope instead.
fn diagnostics(mut d: RuntimeDiagnostics, scopes: &[(rutis::host::InstanceId, &str)]) -> Value {
    let describe = |key: &rutis::host::TypeKey| {
        let text = key.describe();
        match key.instance_id() {
            Some(id) => {
                let scope = scopes
                    .iter()
                    .find(|(instance, _)| *instance == id)
                    .map_or("?", |(_, name)| name);
                text.replace(&format!("@{id}"), &format!("@{scope}"))
            }
            None => text,
        }
    };
    for p in &mut d.plugins {
        p.injects
            .sort_by_key(|i| (i.key.describe(), i.scope.clone()));
        p.resolved_dependencies
            .sort_by_key(|i| (i.key.describe(), i.scope.clone(), i.provider));
        p.accesses
            .sort_by_key(|i| (i.key.describe(), i.scope.clone(), i.provider));
    }
    d.bindings.sort_by_key(|b| (b.key.describe(), b.provider));
    json!({"shuttingDown":d.shutting_down,"plugins":d.plugins.into_iter().map(|p|json!({
        "id":p.id.0,"parent":p.parent.map(|id|id.0),"name":p.name,"state":format!("{:?}",p.state),"generation":p.generation,
        "injects":p.injects.into_iter().map(|i|json!({"service":describe(&i.key),"scope":i.scope,"status":format!("{:?}",i.status)})).collect::<Vec<_>>(),
        "resolved":p.resolved_dependencies.into_iter().map(|r|json!({"service":describe(&r.key),"scope":r.scope,"provider":r.provider.0,"generation":r.generation})).collect::<Vec<_>>(),
        "reads":p.accesses.into_iter().map(|r|json!({"service":describe(&r.key),"provider":r.provider.map(|id|id.0),"declared":r.declared,"external":r.external})).collect::<Vec<_>>()
    })).collect::<Vec<_>>(),"bindings":d.bindings.into_iter().map(|b|json!({"service":describe(&b.key),"provider":b.provider.0,"generation":b.generation,"removing":b.removing})).collect::<Vec<_>>()})
}
/// The Agent the graphs and measurements run on, in a minimal host, its
/// Session configuration, and whether it runs native models.
async fn configured_agent()
-> std::result::Result<(MinimalHost, SessionConfig, bool), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(2);
    let model_config = match args.next().as_deref() {
        None => None,
        Some("--model-config") => Some(args.next().ok_or("--model-config requires a path")?),
        _ => return Err("graph/measure accept only --model-config PATH".into()),
    };
    if args.next().is_some() {
        return Err("unexpected argument".into());
    }
    let (agent, config, native) = if let Some(path) = &model_config {
        use dim_agent_aimux::{
            AimuxModels, AimuxPlugin, ConfiguredModelsPlugin, config::ModelConfigFile,
        };
        use dim_agent_capabilities::{adapters::MockTools, history::HistoryRenderer, host::*};
        use std::sync::Arc;
        let models = Arc::new(AimuxModels::from_config(ModelConfigFile::load(
            std::path::Path::new(path),
        )?)?);
        let model = models.default_model().clone();
        let mut builder = AgentBuilder::new(AgentConfig {
            model: model.clone(),
            ..Default::default()
        });
        dim_agent_capabilities::defaults::install_with_process_model(&mut builder);
        dim_agent_capabilities::defaults::install_compaction(
            &mut builder,
            models.clone(),
            dim_agent_capabilities::summary_strategy::ProductSummaryStrategyConfig::default(),
        );
        builder
            .session_value(Arc::new(HistoryRenderer::default()))
            .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
            .session_value(Arc::new(ToolHost(Arc::new(MockTools))));
        dim_agent_aimux::install(&mut builder);
        builder.plugin(|_| Ok(Box::new(AimuxPlugin)));
        builder.plugin(move |_| Ok(Box::new(ConfiguredModelsPlugin(models.clone()))));
        (
            MinimalHost::start(&builder).await?,
            SessionConfig {
                model: dim_agent_types::ModelSelection {
                    provider_id: model.provider_id,
                    model_id: model.model_id,
                    capability_snapshot: None,
                    config: None,
                    metadata: None,
                },
                ..Default::default()
            },
            true,
        )
    } else {
        (agent().await?, SessionConfig::default(), false)
    };
    Ok((agent, config, native))
}
/// The host's process plugins in `root`'s snapshot: the root's own children
/// and the loader's rows that provide a service, without the part registry.
fn process_rows(root: &rutis::Ctx) -> RuntimeDiagnostics {
    let mut snapshot = root.diagnostics();
    let id = root.root_view().expect("process root").id;
    let loader = snapshot
        .plugins
        .iter()
        .find(|plugin| plugin.parent == Some(id) && plugin.name == "rutis-loader")
        .map(|plugin| plugin.id);
    let providers: std::collections::BTreeSet<_> = snapshot
        .bindings
        .iter()
        .map(|binding| binding.provider)
        .collect();
    snapshot.plugins.retain(|plugin| {
        plugin.parent == Some(id)
            || (plugin.parent.is_some()
                && plugin.parent == loader
                && providers.contains(&plugin.id)
                && plugin.name != "PartRegistry")
    });
    let members: Vec<_> = snapshot.plugins.iter().map(|plugin| plugin.id).collect();
    snapshot
        .bindings
        .retain(|binding| members.contains(&binding.provider));
    snapshot
}
/// One tree: branch capabilities resolve Session services across the subgraphs.
fn mermaid(graphs: &BTreeMap<&str, Value>) -> String {
    let mut mermaid =
        String::from("%% Generated from live rutis diagnostics; do not edit.\nflowchart LR\n");
    let mut nodes = std::collections::BTreeSet::new();
    for (scope, snapshot) in graphs {
        mermaid += &format!("  subgraph {scope}[\"{scope}\"]\n");
        for p in snapshot["plugins"].as_array().unwrap() {
            nodes.insert(p["id"].as_u64().unwrap());
            mermaid += &format!(
                "    p{}[\"{}\"]\n",
                p["id"],
                p["name"].as_str().unwrap().replace('"', "'")
            );
        }
        mermaid += "  end\n";
    }
    for snapshot in graphs.values() {
        for p in snapshot["plugins"].as_array().unwrap() {
            for edge in p["resolved"].as_array().unwrap() {
                if nodes.contains(&edge["provider"].as_u64().unwrap()) {
                    mermaid += &format!(
                        "  p{} -->|\"{}\"| p{}\n",
                        edge["provider"],
                        edge["service"].as_str().unwrap().replace('"', "'"),
                        p["id"]
                    );
                }
            }
        }
    }
    mermaid
}
async fn graph() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let (agent, config, native) = configured_agent().await?;
    let session = agent.create_session(config).await?;
    let session_diagnostics = session.diagnostics()?;
    let main_diagnostics = session.branch_diagnostics("main")?;
    let instance = |d: &RuntimeDiagnostics, name: &str| {
        let scope = d
            .plugins
            .iter()
            .find(|p| p.name == name)
            .expect("scope plugin");
        d.plugins
            .iter()
            .find(|p| Some(p.id) == scope.parent)
            .map(|p| p.instance)
            .expect("scope group")
    };
    let scopes = [
        (instance(&session_diagnostics, "SessionScope"), "session"),
        (instance(&main_diagnostics, "BranchScope"), "main"),
    ];
    let mut graphs = BTreeMap::from([
        ("session", diagnostics(session_diagnostics, &scopes)),
        ("main", diagnostics(main_diagnostics, &scopes)),
    ]);
    if native {
        graphs.insert("process", diagnostics(process_rows(agent.root()), &scopes));
    }
    let mermaid = mermaid(&graphs);
    let mut out = root().join("docs/architecture/rust-agent/generated");
    if native {
        out = out.join("aimux");
    }
    std::fs::create_dir_all(&out)?;
    std::fs::write(
        out.join("services.json"),
        serde_json::to_string_pretty(&graphs)? + "\n",
    )?;
    std::fs::write(out.join("services.mmd"), &mermaid)?;
    let command = if native {
        "cargo run -p xtask -- graph --model-config crates/dim-agent-aimux/examples/offline.json"
    } else {
        "cargo run -p xtask -- graph"
    };
    std::fs::write(
        out.join("services.md"),
        format!(
            "# 默认运行组合\n\n由真实 rutis 装配生成。重新生成：`{command}`。\n\n```mermaid\n{mermaid}```\n"
        ),
    )?;
    if !native {
        let branch = session
            .branch(
                "main".into(),
                0,
                BranchOptions {
                    prompt: "diagram".into(),
                    source: dim_agent_types::BranchSource::Host {
                        operation_id: "diagram".into(),
                    },
                },
            )
            .await?;
        session.idle(&branch).await?;
    }
    instances(&session, &out)?;
    agent.close().await?;
    if !native {
        plugins::write(&root(), &plugins::local_graph().await?)?;
        app_server::write(&root(), app_server::graph().await?)?;
        catalog::write(&root()).await?;
    }
    boundaries(true)?;
    println!("generated {}", out.display());
    Ok(())
}
/// `graph` writes the generated architecture files; `check` renders them again and fails when the committed bytes differ.
fn boundaries(write: bool) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let output = std::process::Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(root())
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
    }
    let metadata: Value = serde_json::from_slice(&output.stdout)?;
    let mut edges = vec![];
    let mut graph = String::from("%% Generated from cargo metadata; do not edit.\nflowchart LR\n");
    for p in metadata["packages"].as_array().unwrap() {
        let name = p["name"].as_str().unwrap();
        for d in p["dependencies"].as_array().unwrap() {
            let dep = d["name"].as_str().unwrap();
            if name == "dim-agent-types" && !modules::TYPES_VALUE_DEPENDENCIES.contains(&dep) {
                return Err("types must not depend on runtime/capabilities".into());
            }
            if name == "dim-agent-runtime"
                && (dep == "dim-agent-capabilities" || dep == "dim-app-server")
            {
                return Err("runtime must not depend on capabilities/server".into());
            }
            if name == "dim-agent-capabilities" && dep == "dim-app-server" {
                return Err("capabilities must not depend on server".into());
            }
            if dep.starts_with("dim-") || dep == "rutis" {
                edges.push(json!({"from":name,"to":dep}));
                graph += &format!(
                    "  {}[\"{name}\"] --> {}[\"{dep}\"]\n",
                    name.replace('-', "_"),
                    dep.replace('-', "_")
                );
            }
        }
    }
    let out = root().join("docs/architecture/rust-agent/generated");
    let mut files = vec![
        ("crates.json", serde_json::to_string_pretty(&edges)? + "\n"),
        ("crates.mmd", graph),
    ];
    files.extend(modules::generate(&root())?);
    if write {
        std::fs::create_dir_all(&out)?;
        for (name, text) in &files {
            std::fs::write(out.join(name), text)?;
        }
    } else {
        let stale: Vec<_> = files
            .iter()
            .filter(|(name, text)| {
                std::fs::read_to_string(out.join(name)).ok().as_ref() != Some(text)
            })
            .map(|(name, _)| out.join(name).display().to_string())
            .collect();
        if !stale.is_empty() {
            return Err(format!(
                "generated architecture files are stale: {}; run `cargo run -p xtask -- graph`",
                stale.join(", ")
            )
            .into());
        }
    }
    println!("crate and module boundaries passed");
    plugins::check(&root())?;
    Ok(())
}
async fn measure() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let before = tokio::runtime::Handle::current()
        .metrics()
        .num_alive_tasks();
    let (agent, config, native) = configured_agent().await?;
    let mut runs = vec![];
    for count in [1usize, 10, 100, 1000] {
        let bytes_before = allocation::live();
        let start = Instant::now();
        let mut sessions = vec![];
        for _ in 0..count {
            sessions.push(agent.create_session(config.clone()).await?);
        }
        let create_ms = start.elapsed().as_secs_f64() * 1000.;
        let bytes_active = allocation::live();
        let active = tokio::runtime::Handle::current()
            .metrics()
            .num_alive_tasks();
        let start = Instant::now();
        for session in &sessions {
            session.close().await?;
        }
        drop(sessions);
        tokio::task::yield_now().await;
        runs.push(json!({"sessions":count,"allocationBytesBefore":bytes_before,"allocationBytesActive":bytes_active,"allocationBytesAfterClose":allocation::live(),"createMs":create_ms,"closeMs":start.elapsed().as_secs_f64()*1000.,"activeTasks":active,"tasksAfterClose":tokio::runtime::Handle::current().metrics().num_alive_tasks()}));
    }
    let mut concurrent = Vec::new();
    for count in [1usize, 16, 64].into_iter().filter(|_| !native) {
        let session = agent.create_session(config.clone()).await?;
        let mut branches = vec!["main".to_owned()];
        for index in 1..count {
            branches.push(
                session
                    .branch(
                        "main".into(),
                        0,
                        BranchOptions {
                            prompt: "warmup".into(),
                            source: dim_agent_types::BranchSource::Host {
                                operation_id: format!("measure-{index}"),
                            },
                        },
                    )
                    .await?,
            );
        }
        for branch in &branches {
            session.idle(branch).await?;
        }
        let mut jobs = tokio::task::JoinSet::new();
        let wall = Instant::now();
        for branch in branches {
            let session = session.clone();
            jobs.spawn(async move {
                let start = Instant::now();
                session
                    .submit(
                        "measurement".into(),
                        dim_agent_types::InputMode::Queue,
                        &branch,
                    )
                    .await?;
                session.idle(&branch).await?;
                Ok::<_, AgentError>(start.elapsed().as_secs_f64() * 1000.)
            });
        }
        let mut latency = Vec::new();
        while let Some(result) = jobs.join_next().await {
            latency.push(result??);
        }
        latency.sort_by(f64::total_cmp);
        let percentile =
            |p: f64| latency[((latency.len() as f64 * p).ceil() as usize).saturating_sub(1)];
        concurrent.push(json!({"activeBranches":count,"wallMs":wall.elapsed().as_secs_f64()*1000.,"p50Ms":percentile(0.50),"p95Ms":percentile(0.95),"p99Ms":percentile(0.99)}));
        session.close().await?;
    }
    agent.close().await?;
    let settling = Instant::now();
    while tokio::runtime::Handle::current()
        .metrics()
        .num_alive_tasks()
        > before
        && settling.elapsed() < std::time::Duration::from_secs(30)
    {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let report = json!({"modelHost": if native { "aimux" } else { "mock" }, "debugAssertions":cfg!(debug_assertions),"runtimeWorkers":2,"taskBaseline":before,"cleanupSettleMs":settling.elapsed().as_secs_f64()*1000.,"tasksAfterAgentClose":tokio::runtime::Handle::current().metrics().num_alive_tasks(),"allocationBytesAfterAgentClose":allocation::live(),"peakRustAllocationBytes":allocation::peak(),"runs":runs,"concurrentMockTurns":concurrent,"agentEventBytes":std::mem::size_of::<dim_agent_types::AgentEvent>(),"modelRequestBytes":std::mem::size_of::<dim_agent_types::ModelRequest>()});
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

/// Opt-in baseline: 100 turns of 10k characters through the mock model, no
/// compaction. Run with `cargo run --release -p xtask -- context-replay`.
async fn context_replay() -> std::result::Result<(), Box<dyn std::error::Error>> {
    const TURNS: usize = 100;
    const CHARS_PER_TURN: usize = 10_000;
    let chunk = "x".repeat(CHARS_PER_TURN);
    let baseline = allocation::live();
    let rss_baseline = rss_kb();
    let directory = std::env::temp_dir().join(format!("dim-context-replay-{}", std::process::id()));
    std::fs::create_dir_all(&directory)?;
    let database = directory.join("timeline.sqlite");
    let db = dim_app_server::storage::SqliteStorage::open(database.clone())?;
    let store = dim_app_server::storage::SqliteTimelineStore::new(db.clone());
    let store_handle = store.clone();
    let agent = agent_with_store(store).await?;
    let live_agent = allocation::live();
    let rss_agent = rss_kb();
    let session = agent.create_session(SessionConfig::default()).await?;
    let live_session = allocation::live();
    let rss_session = rss_kb();
    let mut steps = Vec::new();
    for turn in 1..=TURNS {
        let start = Instant::now();
        session
            .submit(
                format!("Turn {turn}: {chunk}"),
                dim_agent_types::InputMode::Queue,
                "main",
            )
            .await?;
        let live_submit = allocation::live();
        session.idle("main").await?;
        steps.push(json!({
            "turn": turn,
            "liveSubmit": live_submit,
            "live": allocation::live(),
            "peak": allocation::peak(),
            "rssKb": rss_kb(),
            "elapsedMs": start.elapsed().as_secs_f64() * 1000.,
        }));
    }
    let live_active = allocation::live();
    let peak = allocation::peak();
    let rss_active = rss_kb();
    let tasks_active = tokio::runtime::Handle::current()
        .metrics()
        .num_alive_tasks();
    session.close().await?;
    let live_close_immediate = allocation::live();
    let tasks_close_immediate = tokio::runtime::Handle::current()
        .metrics()
        .num_alive_tasks();
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    let live_settled = allocation::live();
    let tasks_settled = tokio::runtime::Handle::current()
        .metrics()
        .num_alive_tasks();
    let rss_settled = rss_kb();
    agent.close().await?;
    let live_after_agent = allocation::live();
    let rss_after_agent = rss_kb();
    store_handle.close().await?;
    db.close().await?;
    std::fs::remove_dir_all(&directory)?;
    let report = json!({
        "modelHost": "mock",
        "store": "sqlite",
        "platform": std::env::consts::OS,
        "debugAssertions": cfg!(debug_assertions),
        "turns": TURNS,
        "charsPerTurn": CHARS_PER_TURN,
        "approxInputTokens": TURNS * CHARS_PER_TURN * 2 / 4,
        "liveBaseline": baseline,
        "liveAfterAgent": live_agent,
        "liveSession": live_session,
        "liveActive": live_active,
        "peak": peak,
        "liveCloseImmediate": live_close_immediate,
        "liveSettled": live_settled,
        "liveAfterAgentClose": live_after_agent,
        "rssKb": {
            "baseline": rss_baseline,
            "agent": rss_agent,
            "session": rss_session,
            "active": rss_active,
            "settled": rss_settled,
            "afterAgentClose": rss_after_agent,
        },
        "tasks": {
            "active": tasks_active,
            "closeImmediate": tasks_close_immediate,
            "settled": tasks_settled,
        },
        "steps": steps,
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

/// Measures the four context stages of one stored branch: read, project,
/// messages, request. Run with
/// `cargo run --release -p xtask -- context-breakdown DB SESSION`.
async fn context_breakdown() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(2).collect();
    let (database, session_id) = match args.as_slice() {
        [database, session] => (PathBuf::from(database), session.clone()),
        _ => return Err("context-breakdown needs DB and SESSION".into()),
    };
    let (directory, database) = disposable_database(&database)?;
    let db = dim_app_server::storage::SqliteStorage::open(database)?;
    let store = dim_app_server::storage::SqliteTimelineStore::new(db.clone());
    let agent = agent_with_store(store).await?;
    let session = agent.restore_session(session_id.clone()).await?;
    let through = session.sequence("main")?;
    let mut phases = Vec::new();
    let read_start = allocation::live();
    let context = session.context("main", through).await?;
    phases.push(json!({
        "phase": "read",
        "live": allocation::live(),
        "delta": allocation::live() - read_start,
        "peak": allocation::peak(),
        "rssKb": rss_kb(),
    }));
    let project_start = allocation::live();
    let history = dim_agent_capabilities::history::project_context_history(&context)?;
    phases.push(json!({
        "phase": "project",
        "live": allocation::live(),
        "delta": allocation::live() - project_start,
        "peak": allocation::peak(),
        "rssKb": rss_kb(),
    }));
    let history_entries = history.len();
    let baseline_entries: usize = history.iter().map(|entry| entry.baseline.len()).sum();
    let entry_message_bytes: usize = history
        .iter()
        .map(|entry| {
            serde_json::to_vec(entry.message.as_ref())
                .map(|bytes| bytes.len())
                .unwrap_or(0)
        })
        .sum();
    let baseline_message_bytes: usize = history
        .iter()
        .flat_map(|entry| entry.baseline.iter())
        .map(|entry| {
            serde_json::to_vec(&entry.message)
                .map(|bytes| bytes.len())
                .unwrap_or(0)
        })
        .sum();
    let messages_start = allocation::live();
    let messages = dim_agent_capabilities::history::context_messages(&context, &history);
    phases.push(json!({
        "phase": "messages",
        "live": allocation::live(),
        "delta": allocation::live() - messages_start,
        "peak": allocation::peak(),
        "rssKb": rss_kb(),
    }));
    let request_start = allocation::live();
    let request =
        dim_agent_capabilities::host::ModelSettings::new("mock", "deterministic").request(messages);
    let request_messages = request.messages.len();
    phases.push(json!({
        "phase": "request",
        "live": allocation::live(),
        "delta": allocation::live() - request_start,
        "peak": allocation::peak(),
        "rssKb": rss_kb(),
    }));
    let live_held = allocation::live();
    let peak_held = allocation::peak();
    drop(request);
    let live_after_request = allocation::live();
    drop(history);
    let live_after_history = allocation::live();
    drop(context);
    let live_after_context = allocation::live();
    let report = json!({
        "store": "sqlite",
        "sessionId": session_id,
        "through": through,
        "historyEntries": history_entries,
        "baselineEntries": baseline_entries,
        "baselineSlotsBytes": baseline_entries * std::mem::size_of::<dim_agent_types::ContextBaselineEntry>(),
        "entryMessageBytes": entry_message_bytes,
        "baselineMessageBytes": baseline_message_bytes,
        "phases": phases,
        "liveHeld": live_held,
        "peakHeld": peak_held,
        "liveAfterRequest": live_after_request,
        "liveAfterHistory": live_after_history,
        "liveAfterContext": live_after_context,
        "requestMessages": request_messages,
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    session.close().await?;
    agent.close().await?;
    drop(db);
    std::fs::remove_dir_all(&directory)?;
    Ok(())
}

/// Restores one stored Session from a prepared SQLite copy and runs one mock
/// turn, so the real event/tool shape is replayed without the provider.
/// Run with `cargo run --release -p xtask -- restore-replay DB SESSION`.
async fn restore_replay() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(2).collect();
    let (database, session_id) = match args.as_slice() {
        [database, session] => (PathBuf::from(database), session.clone()),
        _ => return Err("restore-replay needs DB and SESSION".into()),
    };
    let baseline = allocation::live();
    let rss_baseline = rss_kb();
    let (directory, database) = disposable_database(&database)?;
    let db = dim_app_server::storage::SqliteStorage::open(database)?;
    let store = dim_app_server::storage::SqliteTimelineStore::new(db.clone());
    let agent = agent_with_store(store).await?;
    let live_agent = allocation::live();
    let rss_agent = rss_kb();
    let start = Instant::now();
    let session = agent.restore_session(session_id.clone()).await?;
    let restore_ms = start.elapsed().as_secs_f64() * 1000.;
    let live_restored = allocation::live();
    let peak_restored = allocation::peak();
    let rss_restored = rss_kb();
    let trim_restored = trim_heap();
    let rss_restored_trim = rss_kb();
    allocation::reset_peak_now();
    session
        .submit(
            "measure restored context".into(),
            dim_agent_types::InputMode::Queue,
            "main",
        )
        .await?;
    session.idle("main").await?;
    let live_active = allocation::live();
    let peak = allocation::peak();
    let rss_active = rss_kb();
    let trim_active = trim_heap();
    let rss_active_trim = rss_kb();
    let tasks_active = tokio::runtime::Handle::current()
        .metrics()
        .num_alive_tasks();
    session.close().await?;
    let live_close_immediate = allocation::live();
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    let live_settled = allocation::live();
    let tasks_settled = tokio::runtime::Handle::current()
        .metrics()
        .num_alive_tasks();
    let rss_settled = rss_kb();
    agent.close().await?;
    let live_after_agent = allocation::live();
    let rss_after_agent = rss_kb();
    drop(db);
    std::fs::remove_dir_all(&directory)?;
    let trim_result = trim_heap();
    let rss_after_trim = rss_kb();
    let live_after_trim = allocation::live();
    let report = json!({
        "modelHost": "mock",
        "store": "sqlite",
        "platform": std::env::consts::OS,
        "debugAssertions": cfg!(debug_assertions),
        "sessionId": session_id,
        "restoreMs": restore_ms,
        "trimResult": trim_result,
        "liveAfterTrim": live_after_trim,
        "rssAfterTrim": rss_after_trim,
        "liveBaseline": baseline,
        "liveAfterAgent": live_agent,
        "liveRestored": live_restored,
        "liveActive": live_active,
        "peak": peak_restored.max(peak),
        "peakRestored": peak_restored,
        "peakTurn": peak,
        "liveCloseImmediate": live_close_immediate,
        "liveSettled": live_settled,
        "liveAfterAgentClose": live_after_agent,
        "trimRestored": trim_restored,
        "rssRestoredTrim": rss_restored_trim,
        "trimActive": trim_active,
        "rssActiveTrim": rss_active_trim,
        "rssKb": {
            "baseline": rss_baseline,
            "agent": rss_agent,
            "restored": rss_restored,
            "active": rss_active,
            "settled": rss_settled,
            "afterAgentClose": rss_after_agent,
        },
        "tasks": {
            "active": tasks_active,
            "settled": tasks_settled,
        },
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn disposable_database(
    database: &std::path::Path,
) -> std::result::Result<(PathBuf, PathBuf), Box<dyn std::error::Error>> {
    let directory = std::env::temp_dir().join(format!("dim-replay-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory)?;
    let copy = directory.join("timeline.sqlite");
    let source = rusqlite::Connection::open_with_flags(
        database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    source.backup(rusqlite::MAIN_DB, &copy, None)?;
    Ok((directory, copy))
}

fn rss_kb() -> Option<u64> {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status.lines().find_map(|line| {
                line.strip_prefix("VmRSS:")
                    .and_then(|value| value.trim().trim_end_matches(" kB").parse().ok())
            })
        })
}

fn instances(
    session: &Session,
    out: &std::path::Path,
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut branches = session.branches();
    branches.sort_by_key(|branch| (branch.parent.is_some(), branch.id.clone()));
    let aliases: BTreeMap<_, _> = branches
        .iter()
        .enumerate()
        .map(|(index, b)| (b.id.clone(), format!("timeline_{index}")))
        .collect();
    let mut graph = String::from(
        "%% Generated from live Session/Timeline registrations. Random identities are aliased.\nflowchart TB\n  agent[Agent] --> session[Session]\n",
    );
    let mut nodes = Vec::new();
    for branch in branches {
        let alias = &aliases[&branch.id];
        let label = if branch.parent.is_none() {
            "main".to_owned()
        } else {
            alias.clone()
        };
        let status = session.state(&branch.id)?;
        let phase = session
            .get::<LoopSnapshot>(&branch.id)?
            .map_or(LoopPhase::Idle, |s| s.phase);
        let queued = session
            .get::<InputSnapshot>(&branch.id)?
            .map_or(0, |s| s.queued);
        let parent = branch
            .parent
            .as_ref()
            .and_then(|id| aliases.get(id))
            .cloned();
        graph +=
            &format!("  session --> {alias}[\"{label} / phase={phase:?} / queued={queued}\"]\n");
        if let Some(parent) = &parent {
            graph += &format!("  {parent} -.->|branch| {alias}\n");
        }
        nodes.push(json!({"alias":alias,"parent":parent,"branchPoint":branch.branch_point,"createAnchor":branch.create_anchor,"status":status,"phase":phase,"queued":queued,"pluginDrivers":session.branch_diagnostics(&branch.id)?.plugins.len()}));
    }
    std::fs::write(
        out.join("instances.json"),
        serde_json::to_string_pretty(
            &json!({"source":"Session branches/state and typed capability snapshots","identities":"stable aliases for generated random IDs","timelines":nodes}),
        )? + "\n",
    )?;
    std::fs::write(out.join("instances.mmd"), graph)?;
    Ok(())
}
