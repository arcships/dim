//! #548: the input guide composed from the entries in force
//! ([Prompt §3.3 and §3.4](../../../docs/design/plugins/Prompt.md#33-one-composition-two-paths)).

mod local_support;

#[path = "local_support/compaction.rs"]
mod compaction_support;

use dim_agent_capabilities::{
    adapters::{DefaultPrepareModelPlugin, EchoModel},
    defaults,
    history::HistoryRenderer,
    host::*,
    host_platform::HostPlatform,
    model_provider::ModelProviderPlugin,
    permission::{CommandRulesHost, NoCommandRules},
    prompt_guide::{GuideEntries, GuideEntry, GuideScope},
    prompt_sources,
    tool_registry::{
        ToolContribution, ToolHandler, ToolProvider, ToolRegistry, ToolSourceSet, definition,
    },
    workspace_tools::{self, WorkspaceToolsConfig},
};
use dim_agent_runtime::*;
use dim_agent_types::*;
use local_support::{Dirs, HoldingStore, platform, run, system_text};
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, FiberView, Key, TypeKey};
use rutis::{Ctx, Plugin};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

/// The frame's first paragraph: the guide opens with it.
const FRAME_FIRST: &str = "# How to read your input";
/// The frame's last paragraph: the guide ends with it.
const FRAME_LAST: &str = "they do not renew any permission.";

/// The channel rule in force: it states the one exception to "everything
/// the runtime adds arrives in separate messages".
const CHANNEL_RULE: &str = "The operator is the person running this session. The operator's words always arrive in their own user messages. Everything the runtime adds arrives in separate user messages, wrapped in the tags below. The one exception is an input a compaction shortened: it ends with an [input <id>], and the recall tool opens that id for the input's full text.";

/// The entry the probe registers between adoptions.
const PROBE_ENTRY: &str = r#"<dim-probe source="test">: an entry a contributor registered for this flow; the composition places it between the state and the rule entries."#;

/// #552: TimelineTree writes `<dim-branch-line>` and registers its entry.
const IDENTITY_ENTRY: &str = r#"<dim-branch-line branch="C" parent="main" source="tool">: which line of this session you are, placed by the runtime at the start of a line's own history. A later block of this name replaces the earlier one; it is state about the line, not a request."#;
/// #552: BranchLine writes `<dim-branch-lines>` and registers its entry.
const ROSTER_ENTRY: &str = r#"<dim-branch-lines branch="main">: on main, the runtime lists the lines you opened that are still open, each with the first line of its purpose. A later block of this name replaces the earlier one; it is state about the lines, not a request."#;

fn configuration(model: &str) -> ModelSelection {
    ModelSelection {
        provider_id: "test".into(),
        model_id: model.into(),
        capability_snapshot: Some(ModelCapabilitySnapshot {
            target: ModelId {
                provider_id: "test".into(),
                model_id: model.into(),
            },
            capabilities: ModelCapabilities {
                context_window: 135791,
                max_output_tokens: 6789,
                ..Default::default()
            },
            reasoning: None,
            service_tier: None,
        }),
        config: None,
        metadata: None,
    }
}

/// The host bindings every assembly here shares, minus the tool host: the
/// workspace tools provide it.
fn mock_model_hosts(builder: &mut AgentBuilder) {
    builder
        .session_value(Arc::new(HistoryRenderer::default()))
        .process_value(Arc::new(ModelHost(Arc::new(EchoModel))))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
}

/// The process root with the platform the workspace tools read.
fn platform_root(dirs: &Dirs) -> Ctx {
    let root = rutis::Ctx::root_with(tokio::runtime::Handle::current());
    root.provide_as::<HostPlatform>(Arc::new(platform(&dirs.home)))
        .unwrap();
    root
}

fn workspace(builder: &mut AgentBuilder, dirs: &Dirs) {
    workspace_tools::install(
        builder,
        WorkspaceToolsConfig {
            output_root: dirs.dir("output"),
        },
    );
}

fn work_config(dirs: &Dirs) -> SessionConfig {
    SessionConfig {
        cwd: dirs.dir("work").display().to_string(),
        tools: Some(vec!["exec".into()]),
        ..SessionConfig::default()
    }
}

/// The last model call on `branch`.
async fn last_call(session: &Session, branch: &str) -> ModelCallData {
    session
        .history(branch, None)
        .await
        .unwrap()
        .into_iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ModelCall(data) => Some(data.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no model call on {branch}"))
}

/// The last adopted configuration on `branch`.
async fn last_configured(session: &Session, branch: &str) -> ContextConfiguredData {
    session
        .history(branch, None)
        .await
        .unwrap()
        .into_iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ContextConfigured(data) => Some(data.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no adopted configuration on {branch}"))
}

/// Every adopted configuration on `branch`, oldest first.
async fn configurations(session: &Session, branch: &str) -> Vec<ContextConfiguredData> {
    session
        .history(branch, None)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|event| match &event.kind {
            EventKind::ContextConfigured(data) => Some(data.clone()),
            _ => None,
        })
        .collect()
}

/// `branch`'s adopted configurations once it has more than `count` of them.
async fn wait_for_configurations(
    session: &Session,
    branch: &str,
    count: usize,
) -> Vec<ContextConfiguredData> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let published = configurations(session, branch).await;
            if published.len() > count {
                return published;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{branch} adopted no configuration beyond {count}"))
}

/// Settles the branch's adoption queue: an empty configure is ordered behind
/// what the listener enqueued before it and publishes nothing itself.
async fn drain_adoptions(session: &Session, branch: &str) {
    assert_eq!(
        session
            .configure(
                ConfigureUpdate {
                    sources: Vec::new(),
                    model: None,
                },
                branch,
            )
            .await
            .unwrap(),
        ConfigureOutcome::Unchanged,
        "the empty configure behind the adoption queue settles unchanged"
    );
}

/// The system text of the last request on `branch`.
async fn last_system(session: &Session, branch: &str) -> String {
    let call = last_call(session, branch).await;
    system_text(
        call.request
            .system
            .as_deref()
            .unwrap_or_else(|| panic!("no system on {branch}")),
    )
}

/// The guide a system text carries: from the frame's first paragraph on.
fn guide_of(system: &str) -> &str {
    let at = system
        .find(FRAME_FIRST)
        .unwrap_or_else(|| panic!("the guide is missing from: {system:?}"));
    &system[at..]
}

fn assert_guide_shape(guide: &str, markers: &[&str]) {
    assert_eq!(
        guide.matches(FRAME_FIRST).count(),
        1,
        "one composition: {guide:?}"
    );
    assert!(guide.ends_with(FRAME_LAST), "{guide:?}");
    for marker in markers {
        assert!(
            guide.contains(marker),
            "{marker} entry missing from: {guide:?}"
        );
    }
}

/// One composition: a request on the adopted path and a request on the
/// host-prepared path carry the same frame and the same entries, because
/// both call the same composition over the same loaded writers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn both_paths_carry_one_composition() {
    let dirs = Dirs::new();

    // The adopted path: PromptSources supplies the prompt host, the Session
    // adopts with it, and the System comes from `context/configured`.
    let root = platform_root(&dirs);
    let mut builder = AgentBuilder::new(AgentConfig::default());
    mock_model_hosts(&mut builder);
    builder.session_value(Arc::new(CommandRulesHost(Arc::new(NoCommandRules))));
    defaults::install_with_prompt_capability(&mut builder, ModelProviderPlugin::new);
    prompt_sources::install_with_clock(
        &mut builder,
        Arc::new(|| "2026-09-30".into()),
        Duration::from_secs(60),
    );
    workspace(&mut builder, &dirs);
    let agent = builder.build_in(&root);
    let session = agent
        .create_session(SessionConfig {
            system_prompt: "configured rules".into(),
            ..work_config(&dirs)
        })
        .await
        .unwrap();
    session
        .configure(
            ConfigureUpdate {
                sources: Vec::new(),
                model: Some(configuration("configured")),
            },
            "main",
        )
        .await
        .unwrap();
    run(&session, "hello").await;
    let events = session.history("main", None).await.unwrap();
    let adopted_call = events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ModelCall(data) => Some(data.clone()),
            _ => None,
        })
        .unwrap();
    assert!(
        adopted_call.configuration.is_some(),
        "the adopted path carries the adopted System"
    );
    let published = events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ContextConfigured(data) => Some(data.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        adopted_call.request.system.as_deref(),
        published.system.as_ref(),
        "the request carries the System the branch adopted"
    );
    let adopted = system_text(adopted_call.request.system.as_deref().unwrap());
    agent.close().await.unwrap();

    // The host-prepared path: the Session never adopts, and Loop composes
    // into the request it assembles.
    let root = platform_root(&dirs);
    let mut builder = AgentBuilder::new(AgentConfig::default());
    mock_model_hosts(&mut builder);
    workspace(&mut builder, &dirs);
    defaults::install_with_prompt(&mut builder, Arc::new(EmptyPromptHost), None);
    let agent = builder.build_in(&root);
    let session = agent
        .create_session(SessionConfig {
            system_prompt: "host rules".into(),
            ..work_config(&dirs)
        })
        .await
        .unwrap();
    run(&session, "hello").await;
    let events = session.history("main", None).await.unwrap();
    let hosted_call = events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ModelCall(data) => Some(data.clone()),
            _ => None,
        })
        .unwrap();
    assert!(
        hosted_call.configuration.is_none(),
        "the host-prepared path has no adopted configuration"
    );
    let hosted = system_text(hosted_call.request.system.as_deref().unwrap());
    agent.close().await.unwrap();

    assert!(
        adopted.contains("configured rules\n\n# How to read your input"),
        "{adopted:?}"
    );
    assert!(
        hosted.starts_with("host rules\n\n# How to read your input"),
        "{hosted:?}"
    );
    let (adopted, hosted) = (guide_of(&adopted), guide_of(&hosted));
    assert_eq!(adopted, hosted, "one composition on both paths");
    assert_guide_shape(
        adopted,
        &[
            "<dim-notification",
            "<dim-branch-line",
            "<dim-branch-message",
            "<dim-state source=\"dim/environment/date\">",
            "<dim-rule source=\"dim/instructions/project\"",
        ],
    );
    assert_eq!(
        adopted.matches(IDENTITY_ENTRY).count(),
        1,
        "the entry of the identity block is the one its writer registers: {adopted:?}"
    );
    assert_eq!(
        adopted.matches(ROSTER_ENTRY).count(),
        1,
        "the entry of the roster is the one its writer registers: {adopted:?}"
    );
}

/// The request built from an adopted configuration carries the System Prompt
/// composed, with nothing added at the request, and the summarizer's request
/// carries the same System.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_adopted_request_and_the_summarizers_request_carry_one_system() {
    let dirs = Dirs::new();
    let root = platform_root(&dirs);
    let (strategy, requests) = compaction_support::Window::recording();
    let strategy = Arc::new(strategy);
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder
        .session_value(Arc::new(HistoryRenderer::default()))
        .process_value(Arc::new(ModelHost(Arc::new(EchoModel))))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(Some(
            strategy.clone() as Arc<dyn ResolveContextModel>
        ))))
        .session_value(Arc::new(CompactionStrategyHost(Some(
            strategy as Arc<dyn CompactionStrategy>,
        ))))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
    builder.session_value(Arc::new(CommandRulesHost(Arc::new(NoCommandRules))));
    defaults::install_with_prompt_capability(&mut builder, ModelProviderPlugin::new);
    prompt_sources::install_with_clock(
        &mut builder,
        Arc::new(|| "2026-09-30".into()),
        Duration::from_secs(60),
    );
    workspace(&mut builder, &dirs);
    let agent = builder.build_in(&root);
    let session = agent
        .create_session(SessionConfig {
            system_prompt: "configured rules".into(),
            ..work_config(&dirs)
        })
        .await
        .unwrap();
    session
        .configure(
            ConfigureUpdate {
                sources: Vec::new(),
                model: Some(configuration("configured")),
            },
            "main",
        )
        .await
        .unwrap();
    let text = "work: ".to_owned() + &"the quick brown fox jumps over the lazy dog. ".repeat(30);
    run(&session, &text).await;

    let published = last_configured(&session, "main").await;
    let call = last_call(&session, "main").await;
    assert_eq!(
        call.request.system.as_deref(),
        published.system.as_ref(),
        "the request carries the adopted System with nothing added"
    );
    let system = system_text(call.request.system.as_deref().unwrap());
    let markers = [
        "<dim-notification",
        "<dim-branch-line",
        "<dim-branch-message",
        "<dim-state source=\"dim/environment/date\">",
        "<dim-rule source=\"dim/instructions/project\"",
        "<dim-digest",
    ];
    assert_guide_shape(guide_of(&system), &markers);

    // The summarizer's request is built from the adopted configuration, so it
    // carries the same System.
    compaction_support::compact_now(&session).await;
    let summarizer = requests
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("the summarizer's request");
    assert_eq!(
        summarizer.system.as_deref(),
        call.request.system.as_deref(),
        "the summarizer's request carries the same System"
    );
    assert_guide_shape(
        guide_of(&system_text(summarizer.system.as_deref().unwrap())),
        &markers,
    );

    // The request after the compaction carries the same System too.
    run(&session, &text).await;
    let after = last_call(&session, "main").await;
    assert_eq!(
        after.request.system, call.request.system,
        "a compaction moves no System text"
    );
    agent.close().await.unwrap();
}

/// #661: on the adopted path an entry takes effect when its writer loads and
/// ends when it unloads. Prompt adopts the guide at that point: one
/// `context/configured` for each branch whose composition moved, its settings
/// and source text unchanged, and nothing at all for a load that registers no
/// entry.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_entry_takes_effect_when_its_writer_loads_and_ends_when_it_unloads() {
    let target = PromptSourceTarget {
        source: "rules".into(),
        scope: PromptScope::Branch {
            timeline: "main".into(),
        },
    };
    let hosts = Arc::new(Prompts(Mutex::new(BTreeMap::from([(
        "rules".to_string(),
        PromptEntry {
            source: "rules".into(),
            version: "1".into(),
            body: "rules one".into(),
            scope: PromptScope::Branch {
                timeline: "main".into(),
            },
            delivery: PromptDelivery::System,
            order: 0,
        },
    )]))));
    let flag = Arc::new(AtomicBool::new(false));
    let views = Arc::new(Mutex::new(Vec::new()));
    let store = Arc::new(HoldingStore::default());
    let (flag_owner, views_owner) = (flag.clone(), views.clone());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::mock_hosts(&mut builder);
    builder.timeline_store(store.clone());
    defaults::install_with_prompt(&mut builder, hosts.clone(), None);
    builder.require_feature(Feature::new("GuideProbeOwner").session(
        "GuideProbeOwner",
        move |names| {
            Ok(Box::new(GuideProbeOwner {
                flag: flag_owner.clone(),
                views: views_owner.clone(),
                key: names.key()?,
                scope: GuideScope::Session,
            }))
        },
    ));
    let agent = builder.build();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    assert_eq!(
        session
            .configure(
                ConfigureUpdate {
                    sources: vec![target.clone()],
                    model: Some(configuration("one")),
                },
                "main",
            )
            .await
            .unwrap(),
        ConfigureOutcome::Adopted
    );
    run(&session, "first").await;
    let first = last_system(&session, "main").await;
    assert_guide_shape(
        guide_of(&first),
        &[
            "<dim-branch-line",
            "<dim-state source=\"dim/environment/date\">",
        ],
    );
    assert!(!first.contains(PROBE_ENTRY));
    assert_eq!(
        configurations(&session, "main").await.len(),
        1,
        "the adoption composes the System once"
    );

    // Two turns with no writer loading share a prefix and publish nothing.
    run(&session, "second").await;
    assert_eq!(last_system(&session, "main").await, first);
    assert_eq!(
        configurations(&session, "main").await.len(),
        1,
        "a Session whose writers stay put adopts nothing while it runs"
    );

    // The writer loads: one configuration, its settings and source text
    // unchanged. The adoption's read is held, so the count below is settled
    // by the queue itself: the empty configure behind it drains the queue.
    store.hold_next_read_of("main");
    flag.store(true, Ordering::SeqCst);
    let view = views.lock().unwrap().last().cloned().expect("a probe");
    view.restart().await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), store.read_entered.notified())
        .await
        .expect("the load's adoption reads the context");
    store.release_read();
    drain_adoptions(&session, "main").await;
    let published = configurations(&session, "main").await;
    assert_eq!(published.len(), 2, "one load is one guide adoption");
    let (before, after) = (&published[0], &published[1]);
    assert_eq!(
        after.settings, before.settings,
        "a guide adoption keeps the model and the tools"
    );
    assert_eq!(
        after.system_source, before.system_source,
        "the source text stays what the branch adopted"
    );
    let loaded = system_text(after.system.as_ref().expect("the adopted System"));
    assert!(loaded.contains(PROBE_ENTRY), "{loaded:?}");
    assert_eq!(
        loaded.replacen(&format!("{PROBE_ENTRY}\n\n"), "", 1),
        first,
        "the new System differs from the last one only by the registered entry"
    );
    run(&session, "third").await;
    assert_eq!(
        last_system(&session, "main").await,
        loaded,
        "the next request carries the System the load adopted"
    );

    // A branch started after the load carries the entry.
    let at = session.sequence("main").unwrap();
    let child = session
        .branch(
            "main".into(),
            at,
            BranchOptions {
                prompt: "on the line".into(),
                source: BranchSource::Host {
                    operation_id: "child".into(),
                },
            },
        )
        .await
        .expect("the second line starts");
    session.idle(&child).await.unwrap();
    assert!(
        last_system(&session, &child).await.contains(PROBE_ENTRY),
        "a line started after the load carries the entry"
    );

    // The writer unloads: one more configuration, the System back to `first`.
    store.hold_next_read_of("main");
    flag.store(false, Ordering::SeqCst);
    let view = views.lock().unwrap().last().cloned().expect("a probe");
    view.restart().await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), store.read_entered.notified())
        .await
        .expect("the unload's adoption reads the context");
    store.release_read();
    drain_adoptions(&session, "main").await;
    let published = configurations(&session, "main").await;
    assert_eq!(published.len(), 3, "one unload is one guide adoption");
    assert_eq!(published[2].system_source, published[1].system_source);
    assert_eq!(
        system_text(published[2].system.as_ref().expect("the adopted System")),
        first,
        "the withdrawal ends where its writer unloads, byte for byte"
    );
    run(&session, "fourth").await;
    assert_eq!(last_system(&session, "main").await, first);

    // A load that registers no entry publishes nothing and moves no System:
    // it moves no composition, so the listener enqueues nothing.
    let view = views.lock().unwrap().last().cloned().expect("a probe");
    view.restart().await.unwrap();
    drain_adoptions(&session, "main").await;
    run(&session, "fifth").await;
    assert_eq!(
        configurations(&session, "main").await.len(),
        3,
        "a load that registers no entry adds no configuration"
    );
    assert_eq!(last_system(&session, "main").await, first);
    agent.close().await.unwrap();
}

/// #661: a branch-scoped entry changes one branch's composition. That branch
/// adopts the guide, and the other open branch composes the same guide it
/// adopted, publishes nothing and reads no context for the change.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_branch_scoped_entry_publishes_only_on_its_branch() {
    let dirs = Dirs::new();
    let root = platform_root(&dirs);
    let store = Arc::new(HoldingStore::default());
    let flag = Arc::new(AtomicBool::new(false));
    let views = Arc::new(Mutex::new(Vec::new()));
    let (flag_owner, views_owner) = (flag.clone(), views.clone());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    mock_model_hosts(&mut builder);
    builder.timeline_store(store.clone());
    workspace(&mut builder, &dirs);
    defaults::install_with_prompt(&mut builder, Arc::new(EmptyPromptHost), None);
    builder.require_feature(Feature::new("GuideProbeOwner").session(
        "GuideProbeOwner",
        move |names| {
            Ok(Box::new(GuideProbeOwner {
                flag: flag_owner.clone(),
                views: views_owner.clone(),
                key: names.key()?,
                scope: GuideScope::Branch("main".into()),
            }))
        },
    ));
    let agent = builder.build_in(&root);
    let session = agent.create_session(work_config(&dirs)).await.unwrap();
    assert_eq!(
        session
            .configure(
                ConfigureUpdate {
                    sources: Vec::new(),
                    model: Some(configuration("configured")),
                },
                "main",
            )
            .await
            .unwrap(),
        ConfigureOutcome::Adopted
    );
    run(&session, "on main").await;

    // The second line adopts a configuration of its own, composed for its own
    // entries.
    let at = session.sequence("main").unwrap();
    let child = session
        .branch(
            "main".into(),
            at,
            BranchOptions {
                prompt: "on the line".into(),
                source: BranchSource::Host {
                    operation_id: "child".into(),
                },
            },
        )
        .await
        .expect("the second line starts");
    session.idle(&child).await.unwrap();
    assert_eq!(
        session
            .configure(
                ConfigureUpdate {
                    sources: Vec::new(),
                    model: Some(configuration("line")),
                },
                &child,
            )
            .await
            .unwrap(),
        ConfigureOutcome::Adopted
    );
    session
        .submit("the line's own turn".into(), InputMode::Queue, &child)
        .await
        .unwrap();
    session.idle(&child).await.unwrap();
    let main_before = last_system(&session, "main").await;
    let line_before = last_system(&session, &child).await;
    let (main_count, line_count) = (
        configurations(&session, "main").await.len(),
        configurations(&session, &child).await.len(),
    );

    // The entry is scoped to main: only main composes a different guide. The
    // adoption's read is held, and the empty configure behind it drains the
    // queue before the read record is judged.
    store.take();
    store.hold_next_read_of("main");
    flag.store(true, Ordering::SeqCst);
    let view = views.lock().unwrap().last().cloned().expect("a probe");
    view.restart().await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), store.read_entered.notified())
        .await
        .expect("the entry's adoption reads its branch's context");
    store.release_read();
    drain_adoptions(&session, "main").await;
    let reads = store.take();
    assert!(
        reads.iter().all(|(branch, _)| branch != &child),
        "the other line reads no context for the entry: {reads:?}"
    );
    assert!(
        reads.iter().any(|(branch, _)| branch == "main"),
        "the entry's branch composes and adopts it: {reads:?}"
    );
    assert_eq!(
        configurations(&session, "main").await.len(),
        main_count + 1,
        "the entry's own branch adopts the guide"
    );
    assert_eq!(
        configurations(&session, &child).await.len(),
        line_count,
        "the other line composes the same System and publishes nothing"
    );
    run(&session, "after the load").await;
    let main_after = last_system(&session, "main").await;
    assert!(main_after.contains(PROBE_ENTRY), "{main_after:?}");
    assert_eq!(
        main_after.replacen(&format!("{PROBE_ENTRY}\n\n"), "", 1),
        main_before,
        "main's System moves by the entry and nothing else"
    );
    session
        .submit("the line's next turn".into(), InputMode::Queue, &child)
        .await
        .unwrap();
    session.idle(&child).await.unwrap();
    assert_eq!(
        last_system(&session, &child).await,
        line_before,
        "the other line's prefix stays byte-identical"
    );
    agent.close().await.unwrap();
}

/// #661: an entry registered while its branch opens, after the open's own
/// configuration composed and before its guide adoption reads, is adopted by
/// that adoption: the branch reports itself open with the entry, and the
/// request after the open carries it. The listener's baseline is a
/// composition taken before the open too, so the item it enqueues settles on
/// the System the open already published.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_entry_registered_while_its_branch_opens_is_adopted() {
    let dirs = Dirs::new();
    let root = platform_root(&dirs);
    let store = Arc::new(HoldingStore::default());
    let flag = Arc::new(AtomicBool::new(false));
    let views = Arc::new(Mutex::new(Vec::new()));
    let (flag_owner, views_owner) = (flag.clone(), views.clone());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    mock_model_hosts(&mut builder);
    builder.timeline_store(store.clone());
    workspace(&mut builder, &dirs);
    defaults::install_with_prompt(
        &mut builder,
        Arc::new(EmptyPromptHost),
        Some(ConfigureUpdate {
            sources: Vec::new(),
            model: Some(configuration("initial")),
        }),
    );
    builder.require_feature(Feature::new("GuideProbeOwner").session(
        "GuideProbeOwner",
        move |names| {
            Ok(Box::new(GuideProbeOwner {
                flag: flag_owner.clone(),
                views: views_owner.clone(),
                key: names.key()?,
                scope: GuideScope::Session,
            }))
        },
    ));
    let agent = builder.build_in(&root);

    // The open adoption composes the System, then waits for its publication.
    // The entry registers inside that window, after the composition.
    store.hold_next_configuration_on("main");
    let creation = agent.create_session(SessionConfig {
        system_prompt: "configured rules".into(),
        ..work_config(&dirs)
    });
    tokio::pin!(creation);
    let session = tokio::select! {
        _ = &mut creation => panic!("the Session opened before its publication was held"),
        () = async {
            store.entered.notified().await;
            // The open adopts the guide after its own publication, and the
            // adoption reads the context before it composes. Holding that
            // read parks the adoption behind the registration, and the
            // release below lets the open report itself open.
            store.hold_next_read_of("main");
            flag.store(true, Ordering::SeqCst);
            let view = views.lock().unwrap().last().cloned().expect("a probe");
            view.restart().await.unwrap();
            store.release();
            tokio::time::timeout(Duration::from_secs(10), store.read_entered.notified())
                .await
                .expect("the open's guide adoption reads the context");
            store.release_read();
        } => creation.await.unwrap(),
    };
    drain_adoptions(&session, "main").await;
    let published = configurations(&session, "main").await;
    assert_eq!(
        published.len(),
        2,
        "the entry registered during the open is one guide adoption"
    );
    let (opened, adopted) = (&published[0], &published[1]);
    let opened_system = system_text(opened.system.as_ref().expect("the open adopted a System"));
    assert!(
        !opened_system.contains(PROBE_ENTRY),
        "the open composed before the entry registered: {opened_system:?}"
    );
    assert_eq!(
        adopted.settings, opened.settings,
        "a guide adoption keeps the model and the tools"
    );
    assert_eq!(
        adopted.system_source, opened.system_source,
        "the source text stays what the branch adopted"
    );
    let with_entry = system_text(adopted.system.as_ref().expect("the adopted System"));
    assert!(with_entry.contains(PROBE_ENTRY), "{with_entry:?}");
    assert_eq!(
        with_entry.replacen(&format!("{PROBE_ENTRY}\n\n"), "", 1),
        opened_system,
        "the new System differs from the open's only by the registered entry"
    );
    run(&session, "after the open").await;
    assert_eq!(
        last_system(&session, "main").await,
        with_entry,
        "the request carries the System the open's guide adoption published"
    );
    agent.close().await.unwrap();
}

/// #661: a fork inherits its parent's configuration, so the child's own
/// branch-scoped entry reaches its System only through an adoption at its
/// open. That adoption composes the child's entries and leaves the parent's
/// System untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fork_composes_its_own_entries_at_its_open() {
    let dirs = Dirs::new();
    let root = platform_root(&dirs);
    let flag = Arc::new(AtomicBool::new(true));
    let text = Arc::new(Mutex::new(None));
    let views = Arc::new(Mutex::new(Vec::new()));
    let tables = Arc::new(Mutex::new(Vec::new()));
    let agent = branch_probe_agent(
        &dirs,
        &root,
        None,
        flag.clone(),
        text.clone(),
        None,
        views.clone(),
        tables.clone(),
    );
    let session = agent.create_session(work_config(&dirs)).await.unwrap();
    assert_eq!(
        session
            .configure(
                ConfigureUpdate {
                    sources: Vec::new(),
                    model: Some(configuration("configured")),
                },
                "main",
            )
            .await
            .unwrap(),
        ConfigureOutcome::Adopted
    );
    run(&session, "on main").await;
    let main_system = last_system(&session, "main").await;
    let main_entry = branch_probe_entry("main");
    assert!(main_system.contains(&main_entry), "{main_system:?}");
    let main_configurations = configurations(&session, "main").await.len();

    // The child loads its own copy of the writer, which registers for the
    // child's branch. Its open adopts the guide over the configuration it
    // inherited.
    let at = session.sequence("main").unwrap();
    let child = session
        .branch(
            "main".into(),
            at,
            BranchOptions {
                prompt: "on the line".into(),
                source: BranchSource::Host {
                    operation_id: "child".into(),
                },
            },
        )
        .await
        .expect("the second line starts");
    session.idle(&child).await.unwrap();
    let child_entry = branch_probe_entry(&child);
    let adopted = configurations(&session, &child).await;
    assert_eq!(
        adopted.len(),
        main_configurations + 1,
        "one guide adoption at the child's open"
    );
    let child_system = system_text(
        adopted
            .last()
            .expect("the child's configuration")
            .system
            .as_ref()
            .expect("the child's System"),
    );
    assert!(child_system.contains(&child_entry), "{child_system:?}");
    assert!(!child_system.contains(&main_entry), "{child_system:?}");
    assert_eq!(
        adopted.last().expect("the child's configuration").settings,
        last_configured(&session, "main").await.settings,
        "the adoption keeps the configuration the child inherited"
    );

    // The parent's System and its adopted configurations are unchanged.
    assert_eq!(
        configurations(&session, "main").await.len(),
        main_configurations,
        "the child's adoption records nothing on its parent"
    );
    assert_eq!(last_system(&session, "main").await, main_system);

    // A request on the child carries its own entry, once.
    session
        .submit("on the line".into(), InputMode::Queue, &child)
        .await
        .unwrap();
    session.idle(&child).await.unwrap();
    let line = last_system(&session, &child).await;
    assert!(line.contains(&child_entry), "{line:?}");
    assert_eq!(line.matches("<dim-probe").count(), 1, "{line:?}");
    assert!(!line.contains(&main_entry), "{line:?}");
    agent.close().await.unwrap();
}

/// #661: the reconciliation is part of the open, so a fork's first request
/// carries its own entry without anything waiting between the open and the
/// turn. The branch's own initial prompt is its first turn, and it runs as
/// soon as the branch reports itself open; an adoption the open merely
/// queued would let that request assemble from the inherited System, which
/// is the parent's. The flow holds the child's adoption read, so the open
/// cannot report itself open before the reconciliation is published, and
/// the ordering is pinned rather than raced.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_fork_carries_its_own_entry_in_its_first_request() {
    let dirs = Dirs::new();
    let root = platform_root(&dirs);
    let store = Arc::new(HoldingStore::default());
    let flag = Arc::new(AtomicBool::new(true));
    let text = Arc::new(Mutex::new(None));
    let views = Arc::new(Mutex::new(Vec::new()));
    let tables = Arc::new(Mutex::new(Vec::new()));
    let agent = branch_probe_agent(
        &dirs,
        &root,
        Some(store.clone()),
        flag.clone(),
        text.clone(),
        None,
        views.clone(),
        tables.clone(),
    );
    let session = agent.create_session(work_config(&dirs)).await.unwrap();
    assert_eq!(
        session
            .configure(
                ConfigureUpdate {
                    sources: Vec::new(),
                    model: Some(configuration("configured")),
                },
                "main",
            )
            .await
            .unwrap(),
        ConfigureOutcome::Adopted
    );
    run(&session, "on main").await;
    let main_system = last_system(&session, "main").await;
    let main_entry = branch_probe_entry("main");
    assert!(main_system.contains(&main_entry), "{main_system:?}");

    let at = session.sequence("main").unwrap();
    // A fork's open composes nothing on the child, so the child's first read
    // is its guide adoption's. Holding that read parks the reconciliation: an
    // open that queued the adoption instead of running it reports itself open
    // while this read is held, and its initial prompt assembles from the
    // inherited System; the open that adopts before it replies does not. The
    // flow runs on one worker thread so the adoption's read is issued before
    // the opening receipt's reconcile read, which otherwise races it.
    store.hold_read_of_the_next_branch(1);
    let released = tokio::spawn({
        let store = store.clone();
        async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            store.release_read();
        }
    });
    let child = session
        .branch(
            "main".into(),
            at,
            BranchOptions {
                prompt: "on the line".into(),
                source: BranchSource::Host {
                    operation_id: "child".into(),
                },
            },
        )
        .await
        .expect("the second line starts");
    // No wait before the turn: the creation submits the branch's initial
    // prompt itself once the branch is open.
    session.idle(&child).await.unwrap();
    released.await.unwrap();
    let child_entry = branch_probe_entry(&child);
    let line = last_system(&session, &child).await;
    assert!(
        line.contains(&child_entry),
        "the first request after the open carries the child's entry: {line:?}"
    );
    assert!(!line.contains(&main_entry), "{line:?}");
    agent.close().await.unwrap();
}

/// #661: a Session restored after a writer joined or left its set converges at
/// the restore's open. The baseline is the entries in force at load, so the
/// restored branch's adopted System would otherwise stay what the previous
/// run left.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restored_branch_converges_on_the_entries_its_writers_register() {
    let store = Arc::new(MemoryTimelineStore::default());
    let flag = Arc::new(AtomicBool::new(false));
    let views = Arc::new(Mutex::new(Vec::new()));
    let agent = restore_guide_agent(store.clone(), flag.clone(), views.clone());
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    assert_eq!(
        session
            .configure(
                ConfigureUpdate {
                    sources: Vec::new(),
                    model: Some(configuration("configured")),
                },
                "main",
            )
            .await
            .unwrap(),
        ConfigureOutcome::Adopted
    );
    run(&session, "first").await;
    assert!(!last_system(&session, "main").await.contains(PROBE_ENTRY));
    let id = session.id().to_owned();
    agent.close().await.unwrap();

    // The writer joins the Session's set: the restored branch adopts the
    // entry at its open.
    flag.store(true, Ordering::SeqCst);
    let agent = restore_guide_agent(store.clone(), flag.clone(), views.clone());
    let session = agent.restore_session(id.clone()).await.unwrap();
    wait_for_configurations(&session, "main", 1).await;
    let adopted = last_configured(&session, "main").await;
    let system = system_text(adopted.system.as_ref().expect("the adopted System"));
    assert!(system.contains(PROBE_ENTRY), "{system:?}");
    run(&session, "second").await;
    assert!(last_system(&session, "main").await.contains(PROBE_ENTRY));
    agent.close().await.unwrap();

    // The writer leaves it: the next restore drops the entry the same way.
    flag.store(false, Ordering::SeqCst);
    let agent = restore_guide_agent(store.clone(), flag.clone(), views.clone());
    let session = agent.restore_session(id).await.unwrap();
    wait_for_configurations(&session, "main", 2).await;
    let adopted = last_configured(&session, "main").await;
    let system = system_text(adopted.system.as_ref().expect("the adopted System"));
    assert!(!system.contains(PROBE_ENTRY), "{system:?}");
    run(&session, "third").await;
    assert!(!last_system(&session, "main").await.contains(PROBE_ENTRY));
    agent.close().await.unwrap();
}

/// #661: the same ordering holds on a restore. The restored branch's first
/// request carries the entry its writers registered at load, with no wait
/// between the restore and the turn: the open adopts the guide before it
/// reports itself open, so the request never assembles from the persisted
/// System.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restored_branch_carries_its_entries_in_its_first_request() {
    let store = Arc::new(MemoryTimelineStore::default());
    let flag = Arc::new(AtomicBool::new(false));
    let views = Arc::new(Mutex::new(Vec::new()));
    let agent = restore_guide_agent(store.clone(), flag.clone(), views.clone());
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    assert_eq!(
        session
            .configure(
                ConfigureUpdate {
                    sources: Vec::new(),
                    model: Some(configuration("configured")),
                },
                "main",
            )
            .await
            .unwrap(),
        ConfigureOutcome::Adopted
    );
    run(&session, "first").await;
    assert!(!last_system(&session, "main").await.contains(PROBE_ENTRY));
    let id = session.id().to_owned();
    agent.close().await.unwrap();

    // The writer joins the set. The restored branch's open adopts the guide
    // before it reports itself open, and the first request runs with no idle
    // between it and the open.
    flag.store(true, Ordering::SeqCst);
    let agent = restore_guide_agent(store.clone(), flag, views);
    let session = agent.restore_session(id).await.unwrap();
    session
        .submit("second".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let system = last_system(&session, "main").await;
    assert!(
        system.contains(PROBE_ENTRY),
        "the first request after the restore carries the entry: {system:?}"
    );
    agent.close().await.unwrap();
}

/// #661: a guide adoption that was reading when its branch started closing
/// records nothing. Its writer withdraws as the branch shuts down, so the
/// composition left behind is not one the branch ever ran with; the adopted
/// System stays the one it did run with, entry included.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_guide_adoption_racing_its_branch_close_records_nothing() {
    let dirs = Dirs::new();
    let root = platform_root(&dirs);
    let store = Arc::new(HoldingStore::default());
    let flag = Arc::new(AtomicBool::new(true));
    let text = Arc::new(Mutex::new(None));
    let views = Arc::new(Mutex::new(Vec::new()));
    let tables = Arc::new(Mutex::new(Vec::new()));
    let agent = branch_probe_agent(
        &dirs,
        &root,
        Some(store.clone()),
        flag.clone(),
        text.clone(),
        None,
        views.clone(),
        tables.clone(),
    );
    let session = agent.create_session(work_config(&dirs)).await.unwrap();
    assert_eq!(
        session
            .configure(
                ConfigureUpdate {
                    sources: Vec::new(),
                    model: Some(configuration("configured")),
                },
                "main",
            )
            .await
            .unwrap(),
        ConfigureOutcome::Adopted
    );
    run(&session, "on main").await;
    let at = session.sequence("main").unwrap();
    let child = session
        .branch(
            "main".into(),
            at,
            BranchOptions {
                prompt: "on the line".into(),
                source: BranchSource::Host {
                    operation_id: "child".into(),
                },
            },
        )
        .await
        .expect("the second line starts");
    session.idle(&child).await.unwrap();
    // The child adopts a configuration of its own, composed for its entries.
    assert_eq!(
        session
            .configure(
                ConfigureUpdate {
                    sources: Vec::new(),
                    model: Some(configuration("line")),
                },
                &child,
            )
            .await
            .unwrap(),
        ConfigureOutcome::Adopted
    );
    let child_entry = branch_probe_entry(&child);
    assert!(
        system_text(
            last_configured(&session, &child)
                .await
                .system
                .as_ref()
                .expect("the child's System")
        )
        .contains(&child_entry)
    );

    // The writer's next generation moves the child's composition, and the
    // adoption that follows reads the child's context. The read is held so
    // that the close begins while the adoption waits inside it.
    store.hold_next_read_of(&child);
    *text.lock().unwrap() = Some(child_entry.replace("registered", "re-registered"));
    branch_view(&views, &child).restart().await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), store.read_entered.notified())
        .await
        .expect("the adoption reads the child's context");

    // The branch closes. Its shutdown withdraws the child's own entry, and
    // the stop is recorded while the adoption still waits on the read.
    let table = tables.lock().unwrap()[0].clone();
    let ending = session.end_branch(
        &child,
        BranchSource::Host {
            operation_id: "end".into(),
        },
    );
    tokio::pin!(ending);
    tokio::select! {
        result = &mut ending => panic!("the close returned while its adoption was held: {result:?}"),
        () = wait_until_withdrawn(&table, &child) => {}
    }
    store.release_read();
    ending.await.unwrap();

    // Nothing is recorded after the stop: the last configuration is the one
    // the branch ran with, and it carries the entry.
    let key = TimelineKey {
        session_id: session.id().into(),
        branch_id: child.clone(),
    };
    let head = store.head(&key).await.unwrap();
    let events = store.read(&key, 1..=head).await.unwrap();
    let stopped = events
        .iter()
        .rposition(|record| matches!(record.event.kind, EventKind::StopInterrupt(_)))
        .expect("the close records its stop");
    assert_eq!(
        events[stopped + 1..]
            .iter()
            .filter(|record| matches!(record.event.kind, EventKind::ContextConfigured(_)))
            .count(),
        0,
        "a closing branch records no guide adoption"
    );
    let adopted = events
        .iter()
        .rev()
        .find_map(|record| match &record.event.kind {
            EventKind::ContextConfigured(data) => Some(data),
            _ => None,
        })
        .expect("the child's adopted configuration");
    let system = system_text(adopted.system.as_ref().expect("the adopted System"));
    assert!(system.contains(&child_entry), "{system:?}");
    agent.close().await.unwrap();
}

/// #661: the Session unload's shutdown records nothing either. A writer that
/// withdraws in it leaves the persisted System as the branch ran with it, and
/// the restored branch carries the entry its writer registers again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_entry_withdrawing_as_the_session_unloads_is_not_recorded() {
    let dirs = Dirs::new();
    let root = platform_root(&dirs);
    let store = Arc::new(HoldingStore::default());
    let flag = Arc::new(AtomicBool::new(true));
    let text = Arc::new(Mutex::new(None));
    let views = Arc::new(Mutex::new(Vec::new()));
    let tables = Arc::new(Mutex::new(Vec::new()));
    let agent = branch_probe_agent(
        &dirs,
        &root,
        Some(store.clone()),
        flag.clone(),
        text.clone(),
        None,
        views.clone(),
        tables.clone(),
    );
    let session = agent.create_session(work_config(&dirs)).await.unwrap();
    assert_eq!(
        session
            .configure(
                ConfigureUpdate {
                    sources: Vec::new(),
                    model: Some(configuration("configured")),
                },
                "main",
            )
            .await
            .unwrap(),
        ConfigureOutcome::Adopted
    );
    run(&session, "on main").await;
    let entry = branch_probe_entry("main");
    assert!(
        system_text(
            configurations(&session, "main").await[0]
                .system
                .as_ref()
                .expect("main's System")
        )
        .contains(&entry)
    );
    let recorded = configurations(&session, "main").await.len();
    let id = session.id().to_owned();

    // The next generation moves the composition; the adoption's read is held.
    store.hold_next_read_of("main");
    let reregistered = entry.replace("registered", "re-registered");
    *text.lock().unwrap() = Some(reregistered.clone());
    branch_view(&views, "main").restart().await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), store.read_entered.notified())
        .await
        .expect("the adoption reads main's context");

    // The Session unloads while the adoption waits: its shutdown withdraws
    // the writer's entry before the branch freezes.
    let table = tables.lock().unwrap()[0].clone();
    let closing = agent.close();
    tokio::pin!(closing);
    tokio::select! {
        result = &mut closing => panic!("the unload returned while its adoption was held: {result:?}"),
        () = wait_until_withdrawn(&table, "main") => {}
    }
    store.release_read();
    closing.await.unwrap();

    // The persisted history records no configuration beyond the one the
    // branch ran with.
    let key = TimelineKey {
        session_id: id.clone(),
        branch_id: "main".into(),
    };
    let head = store.head(&key).await.unwrap();
    let events = store.read(&key, 1..=head).await.unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|record| matches!(record.event.kind, EventKind::ContextConfigured(_)))
            .count(),
        recorded,
        "the unload records no guide adoption"
    );

    // The restored branch carries the entry its writer registers again.
    let agent = branch_probe_agent(
        &dirs,
        &platform_root(&dirs),
        Some(store.clone()),
        flag.clone(),
        text.clone(),
        None,
        views.clone(),
        tables.clone(),
    );
    let restored = agent.restore_session(id).await.unwrap();
    wait_for_configurations(&restored, "main", recorded).await;
    let adopted = last_configured(&restored, "main").await;
    let system = system_text(adopted.system.as_ref().expect("the adopted System"));
    assert!(system.contains(&reregistered), "{system:?}");
    agent.close().await.unwrap();
}

/// #661: a writer whose generation reloads withdraws and re-registers the
/// same entry. The adoption that the withdrawal enqueues composes when it
/// runs, not when it was enqueued, so it finds the composition where it
/// started and records nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reloaded_writer_that_keeps_its_entry_records_nothing() {
    let dirs = Dirs::new();
    let root = platform_root(&dirs);
    let store = Arc::new(HoldingStore::default());
    let flag = Arc::new(AtomicBool::new(true));
    let text = Arc::new(Mutex::new(None));
    let gate = Arc::new(LoadGate::default());
    let views = Arc::new(Mutex::new(Vec::new()));
    let tables = Arc::new(Mutex::new(Vec::new()));
    let agent = branch_probe_agent(
        &dirs,
        &root,
        Some(store.clone()),
        flag.clone(),
        text.clone(),
        Some(gate.clone()),
        views.clone(),
        tables.clone(),
    );
    let session = agent.create_session(work_config(&dirs)).await.unwrap();
    assert_eq!(
        session
            .configure(
                ConfigureUpdate {
                    sources: Vec::new(),
                    model: Some(configuration("configured")),
                },
                "main",
            )
            .await
            .unwrap(),
        ConfigureOutcome::Adopted
    );
    run(&session, "on main").await;
    let entry = branch_probe_entry("main");
    let recorded = configurations(&session, "main").await.len();
    assert!(
        system_text(
            last_configured(&session, "main")
                .await
                .system
                .as_ref()
                .expect("main's System")
        )
        .contains(&entry)
    );

    // The reload withdraws the entry and holds the new generation's load, so
    // the composition is "the writer is away" for as long as the test needs.
    gate.arm();
    store.hold_next_read_of("main");
    let reloading = branch_view(&views, "main").restart();
    tokio::pin!(reloading);
    tokio::select! {
        result = &mut reloading => panic!("the reload returned before its load was held: {result:?}"),
        () = gate.applying.notified() => {}
    }
    tokio::time::timeout(Duration::from_secs(10), store.read_entered.notified())
        .await
        .expect("the withdrawal's adoption reads the context");

    // The new generation registers the same entry; the adoption composes
    // now, finds the composition it started from and records nothing.
    gate.open();
    reloading.await.unwrap();
    store.release_read();
    assert_eq!(
        session
            .configure(
                ConfigureUpdate {
                    sources: Vec::new(),
                    model: None,
                },
                "main",
            )
            .await
            .unwrap(),
        ConfigureOutcome::Unchanged,
        "the configure is queued behind the adoption and settles after it"
    );
    assert_eq!(
        configurations(&session, "main").await.len(),
        recorded,
        "a reload that keeps its entry records nothing"
    );
    agent.close().await.unwrap();
}

/// #661: a composition that moved and returned while the branch opened is
/// adopted too. The open can compose the System from the middle of such a
/// change while the composition it compares against reads as before, so the
/// adoption after the open, which compares against the System the branch
/// carries, is what converges it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_composition_that_moved_and_returned_while_its_branch_opens_is_adopted() {
    let flag = Arc::new(AtomicBool::new(true));
    let views = Arc::new(Mutex::new(Vec::new()));
    let store = Arc::new(HoldingStore::default());
    let target = PromptSourceTarget {
        source: "rules".into(),
        scope: PromptScope::Branch {
            timeline: "main".into(),
        },
    };
    let hosts = Arc::new(GatedPrompts {
        entry: PromptEntry {
            source: "rules".into(),
            version: "1".into(),
            body: "rules one".into(),
            scope: PromptScope::Branch {
                timeline: "main".into(),
            },
            delivery: PromptDelivery::System,
            order: 0,
        },
        started: Notify::new(),
        release: Notify::new(),
    });
    let (flag_owner, views_owner) = (flag.clone(), views.clone());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::mock_hosts(&mut builder);
    builder.timeline_store(store.clone());
    defaults::install_with_prompt(
        &mut builder,
        hosts.clone(),
        Some(ConfigureUpdate {
            sources: vec![target],
            model: Some(configuration("configured")),
        }),
    );
    builder.require_feature(Feature::new("GuideProbeOwner").session(
        "GuideProbeOwner",
        move |names| {
            Ok(Box::new(GuideProbeOwner {
                flag: flag_owner.clone(),
                views: views_owner.clone(),
                key: names.key()?,
                scope: GuideScope::Session,
            }))
        },
    ));
    let agent = builder.build();

    // The open loads first, composes after its load returns, and then waits
    // for the publication. The entry withdraws inside the load and returns
    // inside the publication, so the adoption sees the composition moved
    // and came back.
    store.hold_next_configuration_on("main");
    let creation = agent.create_session(SessionConfig::default());
    tokio::pin!(creation);
    let session = tokio::select! {
        _ = &mut creation => panic!("the Session opened before its publication was held"),
        () = async {
            hosts.started.notified().await;
            flag.store(false, Ordering::SeqCst);
            let view = views.lock().unwrap().last().cloned().expect("a probe");
            view.restart().await.unwrap();
            hosts.release.notify_one();
            store.entered.notified().await;
            flag.store(true, Ordering::SeqCst);
            let view = views.lock().unwrap().last().cloned().expect("a probe");
            view.restart().await.unwrap();
            store.release();
        } => creation.await.unwrap(),
    };
    wait_for_configurations(&session, "main", 1).await;
    let published = configurations(&session, "main").await;
    let adopted = published.last().expect("the adoption after the open");
    let system = system_text(adopted.system.as_ref().expect("the adopted System"));
    assert!(system.contains(PROBE_ENTRY), "{system:?}");
    run(&session, "after the open").await;
    assert!(
        last_system(&session, "main").await.contains(PROBE_ENTRY),
        "the request carries the entry its writer registered again"
    );
    agent.close().await.unwrap();
}

/// #661: an entry that withdraws and registers again around the open's own
/// adoption is adopted too. The listener compares against the composition
/// taken before the open, so it has to be listening while the adoption runs:
/// started afterwards, it composes the composition the entry returned to,
/// reads it as its baseline and enqueues nothing, and the branch keeps the
/// intermediate System without a loaded writer's entry.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_entry_that_returns_during_the_open_adoption_is_adopted() {
    let flag = Arc::new(AtomicBool::new(true));
    let views = Arc::new(Mutex::new(Vec::new()));
    let store = Arc::new(HoldingStore::default());
    let target = PromptSourceTarget {
        source: "rules".into(),
        scope: PromptScope::Branch {
            timeline: "main".into(),
        },
    };
    let hosts = Arc::new(GatedPrompts {
        entry: PromptEntry {
            source: "rules".into(),
            version: "1".into(),
            body: "rules one".into(),
            scope: PromptScope::Branch {
                timeline: "main".into(),
            },
            delivery: PromptDelivery::System,
            order: 0,
        },
        started: Notify::new(),
        release: Notify::new(),
    });
    let (flag_owner, views_owner) = (flag.clone(), views.clone());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::mock_hosts(&mut builder);
    builder.timeline_store(store.clone());
    defaults::install_with_prompt(
        &mut builder,
        hosts.clone(),
        Some(ConfigureUpdate {
            sources: vec![target],
            model: Some(configuration("configured")),
        }),
    );
    builder.require_feature(Feature::new("GuideProbeOwner").session(
        "GuideProbeOwner",
        move |names| {
            Ok(Box::new(GuideProbeOwner {
                flag: flag_owner.clone(),
                views: views_owner.clone(),
                key: names.key()?,
                scope: GuideScope::Session,
            }))
        },
    ));
    let agent = builder.build();

    // The open loads and composes with the entry in force, and then waits
    // for the publication. The entry withdraws while that publication is
    // held, so the branch's own adoption composes the System without it;
    // that adoption's publication is held in turn, and the entry returns
    // before it is released, so the composition ends where the listener's
    // baseline started.
    store.hold_next_configuration_on("main");
    let creation = agent.create_session(SessionConfig::default());
    tokio::pin!(creation);
    let session = tokio::select! {
        _ = &mut creation => panic!("the Session opened before its publication was held"),
        () = async {
            hosts.started.notified().await;
            hosts.release.notify_one();
            store.entered.notified().await;
            flag.store(false, Ordering::SeqCst);
            let view = views.lock().unwrap().last().cloned().expect("a probe");
            view.restart().await.unwrap();
            store.hold_next_configuration_on("main");
            store.release();
            store.entered.notified().await;
            flag.store(true, Ordering::SeqCst);
            let view = views.lock().unwrap().last().cloned().expect("a probe");
            view.restart().await.unwrap();
            store.release();
        } => creation.await.unwrap(),
    };
    drain_adoptions(&session, "main").await;
    let published = configurations(&session, "main").await;
    let adopted = published.last().expect("the adoption after the open");
    let system = system_text(adopted.system.as_ref().expect("the adopted System"));
    assert!(system.contains(PROBE_ENTRY), "{system:?}");
    run(&session, "after the open").await;
    assert!(
        last_system(&session, "main").await.contains(PROBE_ENTRY),
        "the request carries the entry its writer registered again"
    );
    agent.close().await.unwrap();
}

/// The channel rule states the handle a compaction appends to an input it
/// shortened: the one case the "separate user messages" sentence is not
/// true of, and therefore part of the protocol.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_frame_states_the_compaction_handle() {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::mock_hosts(&mut builder);
    defaults::install_with_prompt(&mut builder, Arc::new(EmptyPromptHost), None);
    let agent = builder.build();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    run(&session, "hello").await;
    let system = last_system(&session, "main").await;
    let guide = guide_of(&system);
    assert_eq!(
        guide.split("\n\n").nth(2),
        Some(CHANNEL_RULE),
        "the frame's third paragraph is the channel rule: {guide:?}"
    );
    assert_eq!(
        guide.matches("[input <id>]").count(),
        1,
        "the handle is stated once: {guide:?}"
    );
    agent.close().await.unwrap();
}

/// A prompt host whose first load waits until the test opens it: the window
/// in which the open adoption has not composed the System yet.
struct GatedPrompts {
    entry: PromptEntry,
    started: Notify,
    release: Notify,
}
impl LoadPrompt for GatedPrompts {
    fn load(
        &self,
        _: PromptSourceTarget,
        _: CancellationToken,
    ) -> HostFuture<'_, Option<PromptEntry>> {
        Box::pin(async move {
            self.started.notify_one();
            self.release.notified().await;
            Ok(Some(self.entry.clone()))
        })
    }
}

/// A minimal prompt host: one entry per source, replaced by the test.
struct Prompts(Mutex<BTreeMap<String, PromptEntry>>);
impl LoadPrompt for Prompts {
    fn load(
        &self,
        target: PromptSourceTarget,
        _: CancellationToken,
    ) -> HostFuture<'_, Option<PromptEntry>> {
        let entry = self.0.lock().unwrap().get(&target.source).cloned();
        Box::pin(async move { Ok(entry) })
    }
}

/// The owner of the probe part: it mounts the probe and keeps its view so
/// the test can reload one generation of it.
struct GuideProbeOwner {
    flag: Arc<AtomicBool>,
    views: Arc<Mutex<Vec<FiberView>>>,
    key: Key<GuideEntries>,
    scope: GuideScope,
}
impl Plugin for GuideProbeOwner {
    type Deps = ();
    async fn apply(&self, ctx: &Ctx, _: ()) -> rutis::Result {
        let view = ctx.plugin(GuideProbe {
            flag: self.flag.clone(),
            key: self.key.clone(),
            scope: self.scope.clone(),
        });
        (&view).await.map_err(|error| {
            rutis::Error::from(rutis::host::ErrorKind::PluginFailed(
                format!("guide probe failed: {error}").into(),
            ))
        })?;
        self.views.lock().unwrap().push(view);
        Ok(())
    }
}

/// A contributor whose registration depends on the test's flag: with it
/// set, each generation registers an entry from its own `apply`.
struct GuideProbe {
    flag: Arc<AtomicBool>,
    key: Key<GuideEntries>,
    scope: GuideScope,
}
impl DynPlugin for GuideProbe {
    fn name(&self) -> &str {
        "GuideProbe"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.key.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let table = ctx.require_at(&self.key)?;
            if self.flag.load(Ordering::SeqCst) {
                table.register(
                    ctx,
                    self.scope.clone(),
                    GuideEntry {
                        markers: vec!["dim-probe".into()],
                        order: 65,
                        text: PROBE_ENTRY.into(),
                    },
                )?;
            }
            Ok(Effect::Done)
        })
    }
}

/// The entry a branch-scoped probe registers, for the branch that loads it.
fn branch_probe_entry(branch: &str) -> String {
    format!(
        r#"<dim-probe branch="{branch}">: an entry a contributor registered for the branch that loaded it; the composition places it between the state and the rule entries."#
    )
}

/// The branch-scoped probe's own view, by branch.
fn branch_view(views: &Mutex<Vec<(String, FiberView)>>, branch: &str) -> FiberView {
    views
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|(id, _)| id == branch)
        .map(|(_, view)| view.clone())
        .expect("a branch probe view")
}

/// A gate on a probe generation's load: a reload that is armed holds the
/// composition at "the writer is away" until the test opens it.
#[derive(Default)]
struct LoadGate {
    armed: AtomicBool,
    applying: Notify,
    release: Notify,
}
impl LoadGate {
    fn arm(&self) {
        self.armed.store(true, Ordering::SeqCst);
    }
    fn open(&self) {
        self.release.notify_one();
    }
}

/// The owner of the branch-scoped probe: a part of every branch, which
/// registers its entry for the branch that loads it and records its view.
struct BranchGuideProbeOwner {
    flag: Arc<AtomicBool>,
    text: Arc<Mutex<Option<String>>>,
    gate: Option<Arc<LoadGate>>,
    views: Arc<Mutex<Vec<(String, FiberView)>>>,
    tables: Arc<Mutex<Vec<Arc<GuideEntries>>>>,
    key: Key<GuideEntries>,
    identity: Key<TimelineIdentity>,
}
impl BranchGuideProbeOwner {
    fn new(
        names: &Names<'_>,
        flag: Arc<AtomicBool>,
        text: Arc<Mutex<Option<String>>>,
        gate: Option<Arc<LoadGate>>,
        views: Arc<Mutex<Vec<(String, FiberView)>>>,
        tables: Arc<Mutex<Vec<Arc<GuideEntries>>>>,
    ) -> rutis::Result<Self> {
        Ok(Self {
            flag,
            text,
            gate,
            views,
            tables,
            key: names.key()?,
            identity: names.key()?,
        })
    }
}
impl DynPlugin for BranchGuideProbeOwner {
    fn name(&self) -> &str {
        "BranchGuideProbeOwner"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.key.erase(), self.identity.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let identity = ctx.require_at(&self.identity)?;
            let table = ctx.require_at(&self.key)?;
            self.tables.lock().unwrap().push(table.clone());
            let branch = identity.branch_id.clone();
            let view = ctx.plugin(BranchGuideProbe {
                flag: self.flag.clone(),
                text: self.text.clone(),
                gate: self.gate.clone(),
                table,
                branch: branch.clone(),
            });
            (&view).await.map_err(|error| {
                rutis::Error::from(rutis::host::ErrorKind::PluginFailed(
                    format!("branch guide probe failed: {error}").into(),
                ))
            })?;
            self.views.lock().unwrap().push((branch, view));
            Ok(Effect::Done)
        })
    }
}

/// A contributor whose registration depends on the test's flag: with it set,
/// each generation registers an entry for the branch that loaded it.
struct BranchGuideProbe {
    flag: Arc<AtomicBool>,
    text: Arc<Mutex<Option<String>>>,
    gate: Option<Arc<LoadGate>>,
    table: Arc<GuideEntries>,
    branch: String,
}
impl DynPlugin for BranchGuideProbe {
    fn name(&self) -> &str {
        "BranchGuideProbe"
    }
    fn injects(&self) -> Vec<TypeKey> {
        Vec::new()
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            if let Some(gate) = &self.gate
                && gate.armed.load(Ordering::SeqCst)
            {
                gate.applying.notify_one();
                gate.release.notified().await;
            }
            if self.flag.load(Ordering::SeqCst) {
                let text = self
                    .text
                    .lock()
                    .unwrap()
                    .clone()
                    .unwrap_or_else(|| branch_probe_entry(&self.branch));
                self.table.register(
                    ctx,
                    GuideScope::Branch(self.branch.clone()),
                    GuideEntry {
                        markers: vec!["dim-probe".into()],
                        order: 65,
                        text,
                    },
                )?;
            }
            Ok(Effect::Done)
        })
    }
}

/// The agent the branch-probe flows run on: the workspace tools, a branch
/// probe on every branch, and the store the flow hands it.
#[allow(clippy::too_many_arguments)]
fn branch_probe_agent(
    dirs: &Dirs,
    root: &Ctx,
    store: Option<Arc<HoldingStore>>,
    flag: Arc<AtomicBool>,
    text: Arc<Mutex<Option<String>>>,
    gate: Option<Arc<LoadGate>>,
    views: Arc<Mutex<Vec<(String, FiberView)>>>,
    tables: Arc<Mutex<Vec<Arc<GuideEntries>>>>,
) -> Agent {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    mock_model_hosts(&mut builder);
    if let Some(store) = store {
        builder.timeline_store(store);
    }
    workspace(&mut builder, dirs);
    defaults::install_with_prompt(&mut builder, Arc::new(EmptyPromptHost), None);
    builder.require_feature(Feature::new("BranchGuideProbeOwner").branch(
        "BranchGuideProbeOwner",
        move |names| {
            Ok(Box::new(BranchGuideProbeOwner::new(
                names,
                flag.clone(),
                text.clone(),
                gate.clone(),
                views.clone(),
                tables.clone(),
            )?))
        },
    ));
    builder.build_in(root)
}

/// The agent the restore flow runs on: one in-memory store across Agents, and
/// the Session-scoped probe whose registration the flag controls.
fn restore_guide_agent(
    store: Arc<MemoryTimelineStore>,
    flag: Arc<AtomicBool>,
    views: Arc<Mutex<Vec<FiberView>>>,
) -> Agent {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::mock_hosts(&mut builder);
    builder.timeline_store(store);
    defaults::install_with_prompt(&mut builder, Arc::new(EmptyPromptHost), None);
    builder.require_feature(Feature::new("GuideProbeOwner").session(
        "GuideProbeOwner",
        move |names| {
            Ok(Box::new(GuideProbeOwner {
                flag: flag.clone(),
                views: views.clone(),
                key: names.key()?,
                scope: GuideScope::Session,
            }))
        },
    ));
    builder.build()
}

/// Waits until `branch`'s guide composes no probe entry: its writer
/// withdraws.
async fn wait_until_withdrawn(table: &GuideEntries, branch: &str) {
    let mut changes = table.changes();
    while table.compose(branch).contains("<dim-probe") {
        changes.changed().await.expect("the table stays open");
    }
}

/// F1: two branches each carry their own branch-scoped entry. Each line's
/// guide holds the entry exactly once, and ending one line leaves the
/// other's guide byte-identical.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_branch_keeps_its_own_entries() {
    let dirs = Dirs::new();
    let root = platform_root(&dirs);
    let mut builder = AgentBuilder::new(AgentConfig::default());
    mock_model_hosts(&mut builder);
    workspace(&mut builder, &dirs);
    defaults::install_with_prompt(&mut builder, Arc::new(EmptyPromptHost), None);
    let agent = builder.build_in(&root);
    let session = agent.create_session(work_config(&dirs)).await.unwrap();
    run(&session, "on main").await;
    let main = last_system(&session, "main").await;
    assert_eq!(
        main.matches("<dim-notification").count(),
        1,
        "main carries its own exec entry once: {main:?}"
    );

    // Opening a second line loads that line's own writers, which register
    // the same markers under that line's scope.
    let at = session.sequence("main").unwrap();
    let child = session
        .branch(
            "main".into(),
            at,
            BranchOptions {
                prompt: "on the line".into(),
                source: BranchSource::Host {
                    operation_id: "child".into(),
                },
            },
        )
        .await
        .expect("the second line loads its own branch-scoped entries");
    session.idle(&child).await.unwrap();
    let line = last_system(&session, &child).await;
    assert_eq!(
        line.matches("<dim-notification").count(),
        1,
        "the line carries its own exec entry once, not both lines': {line:?}"
    );
    assert_eq!(
        guide_of(&line),
        guide_of(&main),
        "both lines load the same writers, so their guides compose the same text"
    );

    // Ending the line withdraws its entries and leaves main's unchanged.
    session
        .end_branch(
            &child,
            BranchSource::Host {
                operation_id: "end".into(),
            },
        )
        .await
        .unwrap();
    run(&session, "after the line ended").await;
    let after = last_system(&session, "main").await;
    assert_eq!(
        after, main,
        "ending another line leaves this line's guide unchanged"
    );
    agent.close().await.unwrap();
}

/// The provider source the probe registers: one `probe` tool per generation,
/// so a reload of the probe is a tools-only adoption.
struct ProbeSource {
    version: String,
}
impl ToolProvider for ProbeSource {
    fn sets(&self) -> Vec<Arc<ToolSourceSet>> {
        vec![Arc::new(ToolSourceSet {
            key: "probe".into(),
            version: self.version.clone(),
            tools: vec![Arc::new(ToolContribution {
                definition: definition(
                    "probe",
                    "Probe a flow",
                    serde_json::json!({"type": "object"}),
                ),
                handler: Arc::new(ProbeCall),
            })],
        })]
    }
}

struct ProbeCall;
impl ToolHandler for ProbeCall {
    fn execute(
        &self,
        _: ToolCall,
        _: ToolExecutionContext,
        _: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        Box::pin(async { Ok(ToolResult::text("probed")) })
    }
}

/// A contributor whose `apply`, with the flag set, registers a tool source
/// and a guide entry: the tools-only adoption that loads it carries the
/// entry. Without the flag it registers neither, so loading it out of turn
/// never races the Session's opening.
struct GuideToolProbe {
    flag: Arc<AtomicBool>,
    tools: Key<ToolRegistry>,
    guide: Key<GuideEntries>,
}
impl DynPlugin for GuideToolProbe {
    fn name(&self) -> &str {
        "GuideToolProbe"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.tools.erase(), self.guide.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let tools = ctx.require_at(&self.tools)?;
            if !self.flag.load(Ordering::SeqCst) {
                return Ok(Effect::Done);
            }
            let guide = ctx.require_at(&self.guide)?;
            guide.register(
                ctx,
                GuideScope::Session,
                GuideEntry {
                    markers: vec!["dim-probe".into()],
                    order: 65,
                    text: PROBE_ENTRY.into(),
                },
            )?;
            tools.register_source(
                ctx,
                "probe",
                Arc::new(ProbeSource {
                    version: uuid::Uuid::new_v4().to_string(),
                }),
            )?;
            Ok(Effect::Done)
        })
    }
}

struct GuideToolProbeOwner {
    flag: Arc<AtomicBool>,
    views: Arc<Mutex<Vec<FiberView>>>,
    tools: Key<ToolRegistry>,
    guide: Key<GuideEntries>,
}
impl Plugin for GuideToolProbeOwner {
    type Deps = ();
    async fn apply(&self, ctx: &Ctx, _: ()) -> rutis::Result {
        let view = ctx.plugin(GuideToolProbe {
            flag: self.flag.clone(),
            tools: self.tools.clone(),
            guide: self.guide.clone(),
        });
        (&view).await.map_err(|error| {
            rutis::Error::from(rutis::host::ErrorKind::PluginFailed(
                format!("guide tool probe failed: {error}").into(),
            ))
        })?;
        self.views.lock().unwrap().push(view);
        Ok(())
    }
}

/// A tools-only adoption that changes the entries in force re-renders the
/// guide and keeps the adopted source text: two requests between adoptions
/// share a prefix, and the request after the adoption differs only by the
/// entry that adoption carried.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tools_only_adoption_carries_the_entry_its_writer_registers() {
    let dirs = Dirs::new();
    let root = platform_root(&dirs);
    let flag = Arc::new(AtomicBool::new(false));
    let views = Arc::new(Mutex::new(Vec::new()));
    let (flag_owner, views_owner) = (flag.clone(), views.clone());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    mock_model_hosts(&mut builder);
    workspace(&mut builder, &dirs);
    defaults::install_with_prompt(&mut builder, Arc::new(EmptyPromptHost), None);
    builder.require_feature(Feature::new("GuideToolProbeOwner").session(
        "GuideToolProbeOwner",
        move |names| {
            Ok(Box::new(GuideToolProbeOwner {
                flag: flag_owner.clone(),
                views: views_owner.clone(),
                tools: names.key()?,
                guide: names.key()?,
            }))
        },
    ));
    let agent = builder.build_in(&root);
    let session = agent.create_session(work_config(&dirs)).await.unwrap();
    assert_eq!(
        session
            .configure(
                ConfigureUpdate {
                    sources: Vec::new(),
                    model: Some(configuration("configured")),
                },
                "main",
            )
            .await
            .unwrap(),
        ConfigureOutcome::Adopted
    );
    run(&session, "first").await;
    let first = last_system(&session, "main").await;
    run(&session, "second").await;
    let second = last_system(&session, "main").await;
    assert_eq!(
        first, second,
        "two requests between adoptions share a prefix"
    );
    assert!(!second.contains(PROBE_ENTRY), "{second:?}");

    // The writer loads with a tool of its own: the tools-only adoption that
    // loads it carries the entry it registered.
    flag.store(true, Ordering::SeqCst);
    let view = views.lock().unwrap().last().cloned().expect("a probe");
    view.restart().await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let adopted = last_configured(&session, "main").await;
            if adopted
                .settings
                .tool_sources
                .iter()
                .any(|source| source.key == "probe")
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the tools-only adoption");
    run(&session, "third").await;
    let call = last_call(&session, "main").await;
    let third = system_text(call.request.system.as_deref().unwrap());
    assert!(
        third.contains(PROBE_ENTRY),
        "the adoption carries the entry: {third:?}"
    );
    assert_eq!(
        third.replacen(&format!("{PROBE_ENTRY}\n\n"), "", 1),
        second,
        "the new System differs from the last only by the registered entry"
    );
    let published = last_configured(&session, "main").await;
    assert_eq!(
        call.request.system.as_deref(),
        published.system.as_ref(),
        "the request carries the adopted System with nothing added"
    );
    assert!(
        call.request
            .tools
            .as_ref()
            .is_some_and(|tools| tools.iter().any(|tool| tool.name == "probe")),
        "the request declares the tool the adoption carried"
    );

    // The load registered a tool and an entry, so it may take a tool adoption
    // and a guide adoption; both compose one System, so the prefix moves once.
    let published = configurations(&session, "main").await;
    assert!(
        published.len() <= 3,
        "one load publishes at most one configuration per change"
    );
    assert!(
        published[1..]
            .windows(2)
            .all(|pair| pair[0].system == pair[1].system),
        "the two adoptions of one load compose one System"
    );
    agent.close().await.unwrap();
}
