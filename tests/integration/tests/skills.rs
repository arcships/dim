mod local_support;

use dim_agent_capabilities::{
    settings::{SettingsChanged, SettingsScope, settings_changes},
    skill_settings::{SOURCE, SkillInstallSource, SkillInstallation, SkillSettings},
    skills::{
        CatalogChanged, MemorySkillSettings, RefreshTarget, Replace, SelectedSkill, SkillIssue,
        SkillPreview, SkillStatus, SkillsBranch, SkillsError, catalog_changes,
    },
};
use dim_agent_runtime::*;
use dim_agent_types::*;
use local_support::*;
use rutis::host::CtxHost;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

fn list(input: &InputMessageData) -> Option<Option<String>> {
    state_value(input, "dim/skills/list")
}

/// Every Skill list input of `main`, oldest first; `None` clears the list.
async fn lists(session: &Session) -> Vec<Option<String>> {
    inputs(session).await.iter().filter_map(list).collect()
}

struct Layout {
    cwd: PathBuf,
    project: PathBuf,
}

/// User root: `a`. Project root: `b`, a shadowed `a`, invalid `c`, and `d`
/// whose description has an unquoted colon.
fn layout(dirs: &Dirs) -> Layout {
    let cwd = dirs.dir("work");
    let user = dirs.home.join(".agents/skills");
    let project = cwd.join(".agents/skills");
    dirs.write(
        &user.join("a/SKILL.md"),
        &skill_file("a", "user a", "USER-A"),
    );
    dirs.write(
        &project.join("b/SKILL.md"),
        &skill_file("b", "project b", "BODY-B-1 at ${SKILL_ROOT}"),
    );
    dirs.write(
        &project.join("a/SKILL.md"),
        &skill_file("a", "project a", "PROJECT-A"),
    );
    dirs.write(
        &project.join("c/SKILL.md"),
        "---\nname: c\n---\nno description\n",
    );
    dirs.write(
        &project.join("nested/d/SKILL.md"),
        &skill_file("d", "use when: colons appear", "BODY-D"),
    );
    Layout { cwd, project }
}

async fn refresh(env: &Env, session: &Session, cwd: &Path) {
    let before = view_version(session);
    env.catalog()
        .refresh(RefreshTarget {
            user: true,
            project: Some(cwd.to_owned()),
        })
        .await
        .unwrap();
    wait_view_change(session, &before).await;
}

fn view_version(session: &Session) -> String {
    session
        .service::<SkillsBranch>("main")
        .unwrap()
        .session()
        .current()
        .unwrap()
        .version
        .clone()
}

async fn wait_view_change(session: &Session, before: &str) {
    for _ in 0..200 {
        if view_version(session) != before {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("Skill view did not change");
}

async fn entered(env: &Env) {
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        env.gate.entered.notified(),
    )
    .await
    .expect("the turn reached the gate");
}

async fn last_skill_result(session: &Session) -> ToolResult {
    tool_results(session)
        .await
        .into_iter()
        .rev()
        .find(|result| {
            result
                .structured_content
                .as_ref()
                .is_some_and(|content| content.contains_key("success"))
                || result.is_error == Some(true)
        })
        .unwrap()
}

async fn load(env: &Env, session: &Session, name: &'static str) -> ToolResult {
    env.model.push(Step::Tool(
        "skill",
        serde_json::json!({ "skill_name": name }),
    ));
    run(session, "load").await;
    last_skill_result(session).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovery_precedence_and_list_injection() {
    let dirs = Dirs::new();
    let layout = layout(&dirs);
    let env = env(&dirs, Options::default()).await;
    let view = env.catalog().query(Some(&layout.cwd)).await.unwrap();
    let status = |id: &str| {
        view.entries
            .iter()
            .find(|entry| entry.definition.id == id)
            .unwrap()
            .status
            .clone()
    };
    let project_id = |rel: &str| format!("workspace:{}:{rel}", layout.project.to_string_lossy());
    assert_eq!(status("user:a"), SkillStatus::Active);
    assert_eq!(status(&project_id("b")), SkillStatus::Active);
    assert_eq!(status(&project_id("nested/d")), SkillStatus::Active);
    assert_eq!(
        status(&project_id("a")),
        SkillStatus::Shadowed {
            by: "user:a".into()
        }
    );
    assert!(view.issues.iter().any(|issue| matches!(issue,
        SkillIssue::InvalidManifest { path, .. } if path.ends_with("c/SKILL.md"))));

    let session = env.session(&layout.cwd, "").await.unwrap();
    run(&session, "first").await;
    run(&session, "second").await;
    let sent = inputs(&session).await;
    let user = sent.iter().position(|input| input.user.is_some()).unwrap();
    let at = sent.iter().position(|input| list(input).is_some()).unwrap();
    assert!(at < user);
    let first = list(&sent[at]).unwrap().unwrap();
    assert_eq!(sent[at].mode, Some(InputMode::Append));
    assert!(first.contains("- a: user a\n- b: project b\n- d: use when: colons appear\n"));
    assert!(!first.contains("- c"));
    assert_eq!(lists(&session).await.len(), 1);
    let request = requests(&session).await.remove(0);
    let system = system_text(request.system.as_deref().unwrap());
    assert!(system.contains("<skills_instructions>"));
    assert!(system.contains("skill({ \"skill_name\": \"<name>\" })"));
    assert!(!system.contains("- a: user a"));
    assert!(
        request_text(&request).contains("<dim-state source=\"dim/skills/list\">\n- a: user a\n")
    );
    assert!(!request_text(&request).contains("BLOCKING REQUIREMENT"));
    env.agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_configured_instructions_replace_the_builtin_text() {
    let dirs = Dirs::new();
    let cwd = dirs.dir("work");
    let instructions: std::sync::Arc<str> =
        "<skills_instructions>\n## Skills\nA candidate's how-to text.\n</skills_instructions>"
            .into();
    let env = env(
        &dirs,
        Options {
            instructions: Some(instructions.clone()),
            ..Options::default()
        },
    )
    .await;
    let session = env.session(&cwd, "").await.unwrap();
    run(&session, "first").await;
    let request = requests(&session).await.remove(0);
    let system = system_text(request.system.as_deref().unwrap());
    assert!(system.contains(&*instructions), "{system}");
    assert!(
        !system.contains("A skill is a local instruction bundle"),
        "{system}"
    );
    env.agent.close().await.unwrap();
}

/// A profile that requires the Skills parts, registered by the catalog
/// plugin, without installing the capability creates its Session, and the
/// built-in instructions are in force: the factory configuration is only
/// declared and read where an install provides one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn parts_required_without_install_keep_the_builtin_instructions() {
    let dirs = Dirs::new();
    let cwd = dirs.dir("work");
    let env = env(
        &dirs,
        Options {
            require_without_install: true,
            ..Options::default()
        },
    )
    .await;
    let session = tokio::time::timeout(std::time::Duration::from_secs(20), env.session(&cwd, ""))
        .await
        .expect("the Session is created within the wait")
        .unwrap();
    run(&session, "first").await;
    let request = requests(&session).await.remove(0);
    let system = system_text(request.system.as_deref().unwrap());
    assert!(
        system.contains("A skill is a local instruction bundle"),
        "{system}"
    );
    env.agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn load_returns_the_body_as_a_plain_tool_result() {
    let dirs = Dirs::new();
    let layout = layout(&dirs);
    let env = env(&dirs, Options::default()).await;
    let session = env.session(&layout.cwd, "").await.unwrap();
    let result = load(&env, &session, "b").await;
    let content = result_text(&result);
    let skill_root = layout.project.join("b");
    assert!(content.starts_with("Skill: b\nDescription: project b\nFile: "));
    assert!(content.contains(&format!("BODY-B-1 at {}", skill_root.display())));

    run(&session, "next").await;
    let text = request_text(requests(&session).await.last().unwrap());
    assert!(text.contains("BODY-B-1") && !text.contains("skill:b"));
    assert!(
        session
            .get::<dim_agent_capabilities::prompt_state::PromptSnapshot>("main")
            .unwrap()
            .unwrap()
            .entries
            .keys()
            .all(|(source, _)| !source.starts_with("skill:"))
    );
    assert_eq!(
        result_text(&load(&env, &session, "missing").await),
        "Unknown skill: missing"
    );
    env.model
        .push(Step::Tool("skill", serde_json::json!({ "skill_name": 1 })));
    run(&session, "invalid").await;
    assert_eq!(
        result_text(&last_skill_result(&session).await),
        "Invalid input: skill_name must be a string"
    );
    env.agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_view_change_inside_a_turn_reaches_the_next_request() {
    let dirs = Dirs::new();
    let layout = layout(&dirs);
    let body = layout.project.join("b/SKILL.md");
    let env = env(&dirs, Options::default()).await;
    let session = env.session(&layout.cwd, "").await.unwrap();
    run(&session, "first").await;

    env.model.push(Step::Tool("gate", serde_json::json!({})));
    env.model.push(Step::Tool(
        "skill",
        serde_json::json!({ "skill_name": "b" }),
    ));
    session
        .submit("go".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    entered(&env).await;
    dirs.write(&body, &skill_file("b", "project b", "BODY-B-2"));
    refresh(&env, &session, &layout.cwd).await;
    env.gate.release.notify_one();
    session.idle("main").await.unwrap();
    assert!(result_text(&last_skill_result(&session).await).contains("BODY-B-2"));
    assert_eq!(lists(&session).await.len(), 1);

    env.model.push(Step::Tool("gate", serde_json::json!({})));
    env.model.push(Step::Tool(
        "skill",
        serde_json::json!({ "skill_name": "b" }),
    ));
    session
        .submit("go".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    entered(&env).await;
    std::fs::remove_dir_all(layout.project.join("b")).unwrap();
    refresh(&env, &session, &layout.cwd).await;
    env.gate.release.notify_one();
    session.idle("main").await.unwrap();
    assert_eq!(
        result_text(&last_skill_result(&session).await),
        "Unknown skill: b"
    );
    assert!(
        !lists(&session)
            .await
            .last()
            .unwrap()
            .as_ref()
            .unwrap()
            .contains("- b")
    );
    env.agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skill_loads_the_latest_view_after_queue_and_steer_interleave() {
    let dirs = Dirs::new();
    let layout = layout(&dirs);
    let body = layout.project.join("b/SKILL.md");
    let env = env(&dirs, Options::default()).await;
    let session = env.session(&layout.cwd, "").await.unwrap();
    env.model.push(Step::Tool("gate", serde_json::json!({})));
    session
        .submit("hold".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    entered(&env).await;
    // The closure borrows the fixture; the host stays owned for its close.
    let (shared, session, cwd) = (&env, &session, &layout.cwd);
    let change = |version: usize| {
        dirs.write(
            &body,
            &skill_file(
                "b",
                &format!("project b v{version}"),
                &format!("BODY-B-{version}"),
            ),
        );
        async move {
            refresh(shared, session, cwd).await;
            for _ in 0..200 {
                if lists(session).await.len() == version {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            panic!("list v{version} was not appended");
        }
    };
    change(2).await;
    session
        .submit("queued".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    change(3).await;
    session
        .submit("steer".into(), InputMode::Steer, "main")
        .await
        .unwrap();
    change(4).await;
    env.model.push(Step::Text("held"));
    env.model.push(Step::Tool(
        "skill",
        serde_json::json!({ "skill_name": "b" }),
    ));
    env.gate.release.notify_one();
    session.idle("main").await.unwrap();
    assert!(result_text(&last_skill_result(session).await).contains("BODY-B-4"));
    assert_eq!(lists(session).await.len(), 4);
    let text = request_text(requests(session).await.last().unwrap());
    let lines: Vec<&str> = text.lines().collect();
    let at = |marker: &str| {
        lines
            .iter()
            .position(|line| *line == marker)
            .unwrap_or_else(|| panic!("{marker} missing from\n{text}"))
    };
    let order = [
        "- b: project b",
        "hold",
        "- b: project b v2",
        "- b: project b v3",
        "steer",
        "- b: project b v4",
        "queued",
        "BODY-B-4",
    ]
    .map(at);
    assert!(order.is_sorted(), "{order:?}\n{text}");
    env.agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enablement_changes_the_list_and_loading() {
    let dirs = Dirs::new();
    let layout = layout(&dirs);
    let env = env(&dirs, Options::default()).await;
    let (announced, mut seen) = tokio::sync::watch::channel(0);
    env.root
        .listen::<CatalogChanged>()
        .name(catalog_changes())
        .on(move |changed, _| {
            announced.send_replace(changed.version);
            async { Ok(None) }
        })
        .unwrap();
    let session = env.session(&layout.cwd, "").await.unwrap();
    run(&session, "first").await;
    let b = format!("workspace:{}:b", layout.project.to_string_lossy());

    let before = view_version(&session);
    let (entry, version) = env
        .catalog()
        .set_enabled(&b, false, Some(&layout.cwd))
        .await
        .unwrap();
    assert!(!entry.enabled);
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        seen.wait_for(|seen| *seen == version),
    )
    .await
    .unwrap()
    .unwrap();
    let persisted = env
        .settings
        .skills(Default::default())
        .await
        .unwrap()
        .disabled;
    assert!(persisted.contains(&b));
    wait_view_change(&session, &before).await;
    assert_eq!(
        result_text(&load(&env, &session, "b").await),
        "Skill is disabled: b"
    );
    let listed = lists(&session).await.pop().unwrap().unwrap();
    assert!(listed.contains("- a: ") && !listed.contains("- b"));

    let before = view_version(&session);
    env.catalog()
        .set_enabled(&b, true, Some(&layout.cwd))
        .await
        .unwrap();
    wait_view_change(&session, &before).await;
    assert!(result_text(&load(&env, &session, "b").await).contains("BODY-B-1"));

    let before = view_version(&session);
    let current = env.settings.skills(Default::default()).await.unwrap();
    let revision = env
        .settings
        .clone()
        .update_skills(current.version, b.clone(), false, Default::default())
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(view_version(&session), before);
    env.root
        .emit(SettingsChanged {
            name: settings_changes(SOURCE),
            source: SOURCE.into(),
            scope: SettingsScope::User,
            revision,
        })
        .unwrap();
    wait_view_change(&session, &before).await;
    assert_eq!(
        result_text(&load(&env, &session, "b").await),
        "Skill is disabled: b"
    );

    assert_eq!(
        env.catalog()
            .set_enabled("user:missing", false, Some(&layout.cwd))
            .await
            .unwrap_err(),
        SkillsError::UnknownSkill("user:missing".into())
    );

    for id in [
        "user:a".to_owned(),
        format!("workspace:{}:nested/d", layout.project.to_string_lossy()),
    ] {
        let before = view_version(&session);
        env.catalog()
            .set_enabled(&id, false, Some(&layout.cwd))
            .await
            .unwrap();
        wait_view_change(&session, &before).await;
    }
    assert_eq!(lists(&session).await.pop().unwrap(), None);
    env.agent.close().await.unwrap();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unreadable_root_keeps_the_view() {
    use std::os::unix::fs::PermissionsExt;
    let dirs = Dirs::new();
    let layout = layout(&dirs);
    let env = env(&dirs, Options::default()).await;
    let session = env.session(&layout.cwd, "").await.unwrap();
    run(&session, "first").await;
    std::fs::set_permissions(&layout.project, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read_dir(&layout.project).is_ok() {
        return;
    }
    let error = env
        .catalog()
        .refresh(RefreshTarget {
            user: false,
            project: Some(layout.cwd.clone()),
        })
        .await
        .unwrap_err();
    assert!(matches!(error, SkillsError::RootUnreadable { .. }));
    std::fs::set_permissions(&layout.project, std::fs::Permissions::from_mode(0o755)).unwrap();
    run(&session, "second").await;
    assert_eq!(list(inputs(&session).await.last().unwrap()), None);
    assert!(result_text(&load(&env, &session, "b").await).contains("BODY-B-1"));
    env.agent.close().await.unwrap();
}

/// Holds the response of the first update after committing it.
struct SlowFirstResponse {
    inner: std::sync::Arc<dim_agent_capabilities::skills::MemorySkillSettings>,
    first: std::sync::atomic::AtomicBool,
    committed: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
impl SkillSettings for SlowFirstResponse {
    fn set_plugin(
        self: std::sync::Arc<Self>,
        plugin: String,
        root: Option<std::path::PathBuf>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> rutis::host::BoxFuture<
        'static,
        std::result::Result<String, dim_agent_capabilities::skill_settings::SkillSettingsError>,
    > {
        self.inner.clone().set_plugin(plugin, root, cancel)
    }
    fn set_installation(
        self: std::sync::Arc<Self>,
        id: String,
        installation: Option<dim_agent_capabilities::skill_settings::SkillInstallation>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> rutis::host::BoxFuture<
        'static,
        std::result::Result<String, dim_agent_capabilities::skill_settings::SkillSettingsError>,
    > {
        self.inner
            .clone()
            .set_installation(id, installation, cancel)
    }
    fn skills(
        &self,
        cancel: tokio_util::sync::CancellationToken,
    ) -> rutis::host::BoxFuture<
        '_,
        std::result::Result<
            dim_agent_capabilities::skill_settings::SkillSettingsSnapshot,
            dim_agent_capabilities::skill_settings::SkillSettingsError,
        >,
    > {
        self.inner.skills(cancel)
    }
    fn update_skills(
        self: std::sync::Arc<Self>,
        expected: String,
        id: String,
        enabled: bool,
        cancel: tokio_util::sync::CancellationToken,
    ) -> rutis::host::BoxFuture<
        'static,
        std::result::Result<String, dim_agent_capabilities::skill_settings::SkillSettingsError>,
    > {
        Box::pin(async move {
            let version = self
                .inner
                .clone()
                .update_skills(expected, id, enabled, cancel)
                .await?;
            if self.first.swap(false, std::sync::atomic::Ordering::SeqCst) {
                self.committed.notify_one();
                self.release.notified().await;
            }
            Ok(version)
        })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_late_update_response_does_not_overwrite_a_newer_setting() {
    use dim_agent_capabilities::{
        host_platform::HostPlatform,
        skill_settings::SkillSettingsHost,
        skills::{SkillsCatalog, SkillsCatalogPlugin},
    };
    use std::sync::Arc;
    let dirs = Dirs::new();
    let layout = layout(&dirs);
    let root = rutis::Ctx::root_with(tokio::runtime::Handle::current());
    root.provide_as::<HostPlatform>(Arc::new(platform(&dirs.home)))
        .unwrap();
    let settings = Arc::new(SlowFirstResponse {
        inner: Arc::default(),
        first: true.into(),
        committed: Default::default(),
        release: Default::default(),
    });
    root.provide_as::<SkillSettingsHost>(Arc::new(SkillSettingsHost(settings.clone())))
        .unwrap();
    root.provide_as::<dim_agent_runtime::PartRegistry>(Arc::new(
        dim_agent_runtime::PartRegistry::default(),
    ))
    .unwrap();
    let services = dim_agent_capabilities::catalog();
    let build = rutis_loader::resolve::Build::default();
    let names = dim_agent_runtime::Names::copy(&services, &build);
    (&root.plugin(SkillsCatalogPlugin::new(&names).unwrap()))
        .await
        .unwrap();
    let catalog = root.get::<SkillsCatalog>().unwrap();
    let b = format!("workspace:{}:b", layout.project.to_string_lossy());

    let disabling = tokio::spawn({
        let (catalog, b, cwd) = (catalog.clone(), b.clone(), layout.cwd.clone());
        async move { catalog.set_enabled(&b, false, Some(&cwd)).await }
    });
    settings.committed.notified().await;
    catalog
        .set_enabled(&b, true, Some(&layout.cwd))
        .await
        .unwrap();
    settings.release.notify_one();
    let (entry, version) = disabling.await.unwrap().unwrap();
    let view = catalog.query(Some(&layout.cwd)).await.unwrap();
    assert_eq!(version, view.version);
    assert_eq!(
        entry.enabled,
        view.entries
            .iter()
            .find(|entry| entry.definition.id == b)
            .unwrap()
            .enabled
    );

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let enabled = |view: dim_agent_capabilities::skills::CatalogView| {
        view.entries
            .into_iter()
            .find(|entry| entry.definition.id == b)
            .unwrap()
            .enabled
    };
    assert!(enabled(catalog.query(Some(&layout.cwd)).await.unwrap()));
    root.shutdown().await.unwrap();
}

/// Fails the next `failing` settings reads; a write fails its read-back.
struct FailingReads {
    inner: std::sync::Arc<dim_agent_capabilities::skills::MemorySkillSettings>,
    failing: std::sync::atomic::AtomicUsize,
}
impl SkillSettings for FailingReads {
    fn set_plugin(
        self: std::sync::Arc<Self>,
        plugin: String,
        root: Option<std::path::PathBuf>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> rutis::host::BoxFuture<
        'static,
        std::result::Result<String, dim_agent_capabilities::skill_settings::SkillSettingsError>,
    > {
        self.inner.clone().set_plugin(plugin, root, cancel)
    }
    fn set_installation(
        self: std::sync::Arc<Self>,
        id: String,
        installation: Option<dim_agent_capabilities::skill_settings::SkillInstallation>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> rutis::host::BoxFuture<
        'static,
        std::result::Result<String, dim_agent_capabilities::skill_settings::SkillSettingsError>,
    > {
        self.inner
            .clone()
            .set_installation(id, installation, cancel)
    }
    fn skills(
        &self,
        cancel: tokio_util::sync::CancellationToken,
    ) -> rutis::host::BoxFuture<
        '_,
        std::result::Result<
            dim_agent_capabilities::skill_settings::SkillSettingsSnapshot,
            dim_agent_capabilities::skill_settings::SkillSettingsError,
        >,
    > {
        use std::sync::atomic::Ordering;
        if self
            .failing
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            return Box::pin(async {
                Err(
                    dim_agent_capabilities::skill_settings::SkillSettingsError::Failed(
                        "injected".into(),
                    ),
                )
            });
        }
        self.inner.skills(cancel)
    }
    fn update_skills(
        self: std::sync::Arc<Self>,
        expected: String,
        id: String,
        enabled: bool,
        cancel: tokio_util::sync::CancellationToken,
    ) -> rutis::host::BoxFuture<
        'static,
        std::result::Result<String, dim_agent_capabilities::skill_settings::SkillSettingsError>,
    > {
        Box::pin(async move {
            self.failing.store(1, std::sync::atomic::Ordering::SeqCst);
            self.inner
                .clone()
                .update_skills(expected, id, enabled, cancel)
                .await
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unavailable_settings_isolate_skills_and_refresh_recovers_existing_sessions() {
    use std::sync::{Arc, atomic::Ordering};

    let dirs = Dirs::new();
    let layout = layout(&dirs);
    let settings = Arc::new(FailingReads {
        inner: Arc::default(),
        failing: usize::MAX.into(),
    });
    let env = env(
        &dirs,
        Options {
            settings: settings.clone(),
            ..Options::default()
        },
    )
    .await;
    let catalog = env.catalog();
    let b = format!("workspace:{}:b", layout.project.to_string_lossy());
    assert!(matches!(
        catalog.query(Some(&layout.cwd)).await,
        Err(SkillsError::SettingsRead(reason)) if reason.contains("injected")
    ));
    let session = env.session(&layout.cwd, "").await.unwrap();
    let skills = session.service::<SkillsBranch>("main").unwrap().session();
    assert!(matches!(skills.list(), Err(SkillsError::SettingsRead(_))));

    env.model.push(Step::Tool("gate", serde_json::json!({})));
    env.gate.release.notify_one();
    run(&session, "ordinary programming").await;
    assert!(
        tool_results(&session)
            .await
            .iter()
            .any(|result| result_text(result) == "gated")
    );
    let result = load(&env, &session, "b").await;
    assert_eq!(result.is_error, Some(true));
    assert!(result_text(&result).contains("Skills unavailable: Skill settings read failed:"));
    assert!(result_text(&result).contains("injected"));
    assert!(
        lists(&session)
            .await
            .last()
            .unwrap()
            .as_ref()
            .unwrap()
            .contains("injected")
    );

    settings.failing.store(0, Ordering::SeqCst);
    catalog
        .refresh(RefreshTarget {
            user: true,
            project: Some(layout.cwd.clone()),
        })
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !lists(&session)
            .await
            .last()
            .and_then(Option::as_deref)
            .is_some_and(|list| list.contains("- b: project b"))
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(skills.list().unwrap().iter().any(|skill| skill.name == "b"));
    assert!(result_text(&load(&env, &session, "b").await).contains("BODY-B-1"));
    assert_eq!(
        env.agent
            .diagnostics()
            .unwrap()
            .plugins
            .iter()
            .filter(|plugin| plugin.name == "SkillContent"
                && plugin.state == rutis::host::FiberState::Active)
            .count(),
        3
    );

    settings.failing.store(usize::MAX, Ordering::SeqCst);
    assert!(matches!(
        catalog.set_enabled(&b, false, Some(&layout.cwd)).await,
        Err(SkillsError::SettingsRead(_))
    ));
    assert!(matches!(
        catalog.refresh(RefreshTarget::default()).await,
        Err(SkillsError::SettingsRead(_))
    ));
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while skills.list().is_ok() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let result = load(&env, &session, "b").await;
    assert_eq!(result.is_error, Some(true));
    assert!(!result_text(&result).contains("BODY-B-1"));
    assert!(
        env.agent
            .diagnostics()
            .unwrap()
            .plugins
            .iter()
            .all(|plugin| plugin.name != "SkillContent"
                || plugin.state != rutis::host::FiberState::Active)
    );
    env.agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_read_back_is_reported_and_recovered_by_the_next_query() {
    use dim_agent_capabilities::{
        host_platform::HostPlatform,
        skill_settings::SkillSettingsHost,
        skills::{SkillsCatalog, SkillsCatalogPlugin},
    };
    use std::sync::Arc;
    let dirs = Dirs::new();
    let layout = layout(&dirs);
    let root = rutis::Ctx::root_with(tokio::runtime::Handle::current());
    root.provide_as::<HostPlatform>(Arc::new(platform(&dirs.home)))
        .unwrap();
    let settings = Arc::new(FailingReads {
        inner: Arc::default(),
        failing: Default::default(),
    });
    root.provide_as::<SkillSettingsHost>(Arc::new(SkillSettingsHost(settings.clone())))
        .unwrap();
    root.provide_as::<dim_agent_runtime::PartRegistry>(Arc::new(
        dim_agent_runtime::PartRegistry::default(),
    ))
    .unwrap();
    let services = dim_agent_capabilities::catalog();
    let build = rutis_loader::resolve::Build::default();
    let names = dim_agent_runtime::Names::copy(&services, &build);
    (&root.plugin(SkillsCatalogPlugin::new(&names).unwrap()))
        .await
        .unwrap();
    let catalog = root.get::<SkillsCatalog>().unwrap();
    let b = format!("workspace:{}:b", layout.project.to_string_lossy());

    let failed = catalog.set_enabled(&b, false, Some(&layout.cwd)).await;
    assert!(
        matches!(failed, Err(SkillsError::SettingsSync(_))),
        "{failed:?}"
    );
    let view = catalog.query(Some(&layout.cwd)).await.unwrap();
    let entry = view
        .entries
        .iter()
        .find(|entry| entry.definition.id == b)
        .unwrap();
    assert!(!entry.enabled);
    root.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn content_instances_are_session_owned_and_only_changed_content_is_replaced() {
    let dirs = Dirs::new();
    let layout = layout(&dirs);
    let env = env(&dirs, Options::default()).await;
    let first = env.session(&layout.cwd, "").await.unwrap();
    let second = env.session(&layout.cwd, "").await.unwrap();
    let content_ids = || {
        let snapshot = env.agent.diagnostics().unwrap();
        let ids: std::collections::HashSet<_> = snapshot
            .plugins
            .iter()
            .filter(|plugin| {
                plugin.name == "SkillContent" && plugin.state == rutis::host::FiberState::Active
            })
            .map(|plugin| plugin.id)
            .collect();
        assert_eq!(
            snapshot
                .bindings
                .iter()
                .filter(|binding| ids.contains(&binding.provider))
                .count(),
            ids.len()
        );
        ids
    };
    let original = content_ids();
    assert_eq!(original.len(), 6);
    let second_before = view_version(&second);
    dirs.write(
        &layout.project.join("b/SKILL.md"),
        &skill_file("b", "project b", "BODY-B-2"),
    );
    refresh(&env, &first, &layout.cwd).await;
    wait_view_change(&second, &second_before).await;
    let refreshed = content_ids();
    assert_eq!(refreshed.len(), 6);
    assert_eq!(original.intersection(&refreshed).count(), 4);
    assert!(result_text(&load(&env, &second, "b").await).contains("BODY-B-2"));

    env.catalog()
        .refresh(RefreshTarget {
            user: true,
            project: Some(layout.cwd.clone()),
        })
        .await
        .unwrap();
    run(&second, "unchanged").await;
    assert_eq!(content_ids(), refreshed);

    first.close().await.unwrap();
    let surviving = content_ids();
    assert_eq!(surviving.len(), 3);
    assert!(surviving.is_subset(&refreshed));
    assert!(result_text(&load(&env, &second, "b").await).contains("BODY-B-2"));
    let b = format!("workspace:{}:b", layout.project.to_string_lossy());
    let before = view_version(&second);
    env.catalog()
        .set_enabled(&b, false, Some(&layout.cwd))
        .await
        .unwrap();
    wait_view_change(&second, &before).await;
    let disabled = content_ids();
    assert_eq!(disabled.len(), 2);
    assert!(disabled.is_subset(&surviving));
    assert_eq!(
        result_text(&load(&env, &second, "b").await),
        "Skill is disabled: b"
    );
    let before = view_version(&second);
    env.catalog()
        .set_enabled(&b, true, Some(&layout.cwd))
        .await
        .unwrap();
    wait_view_change(&second, &before).await;
    let enabled = content_ids();
    assert_eq!(enabled.len(), 3);
    assert_eq!(enabled.intersection(&surviving).count(), 2);
    assert!(result_text(&load(&env, &second, "b").await).contains("BODY-B-2"));
    second.close().await.unwrap();
    assert!(content_ids().is_empty());
    env.agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn catalog_changes_during_assembly_wait_for_session_publication() {
    let dirs = Dirs::new();
    let layout = layout(&dirs);
    let hold = std::sync::Arc::new(AssemblyHold::default());
    let env = env(
        &dirs,
        Options {
            hold: Some(hold.clone()),
            ..Options::default()
        },
    )
    .await;
    let active_content = || {
        env.agent
            .diagnostics()
            .map(|snapshot| {
                snapshot
                    .plugins
                    .into_iter()
                    .filter(|plugin| {
                        plugin.name == "SkillContent"
                            && plugin.state == rutis::host::FiberState::Active
                    })
                    .map(|plugin| plugin.id)
                    .collect::<std::collections::HashSet<_>>()
            })
            .unwrap_or_default()
    };
    hold.arm();
    let agent = env.agent.clone();
    let cwd = layout.cwd.to_string_lossy().into_owned();
    let creating = tokio::spawn(async move {
        agent
            .create_session(SessionConfig {
                cwd,
                tools: Some(vec!["skill".into(), "gate".into()]),
                ..SessionConfig::default()
            })
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), hold.entered.notified())
        .await
        .expect("assembly reached the hold");
    let mut initial = active_content();
    for _ in 0..200 {
        if initial.len() == 3 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        initial = active_content();
    }
    assert_eq!(initial.len(), 3);

    dirs.write(
        &layout.project.join("b/SKILL.md"),
        &skill_file("b", "project b", "BODY-B-2"),
    );
    env.catalog()
        .refresh(RefreshTarget {
            user: true,
            project: Some(layout.cwd.clone()),
        })
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(active_content(), initial);

    hold.release.notify_one();
    let session = creating.await.unwrap().unwrap();
    let mut adopted = active_content();
    for _ in 0..1000 {
        if adopted != initial && adopted.len() == 3 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        adopted = active_content();
    }
    assert_eq!(adopted.len(), 3);
    assert_eq!(adopted.intersection(&initial).count(), 2);
    let mut body = String::new();
    for _ in 0..100 {
        body = result_text(&load(&env, &session, "b").await);
        if body.contains("BODY-B-2") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(body.contains("BODY-B-2"), "{body}");
    env.agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_closing_session_creates_no_content_instances() {
    let dirs = Dirs::new();
    let layout = layout(&dirs);
    let env = env(&dirs, Options::default()).await;
    let session = env.session(&layout.cwd, "").await.unwrap();
    let content = || {
        env.agent
            .diagnostics()
            .unwrap()
            .plugins
            .into_iter()
            .filter(|plugin| plugin.name == "SkillContent")
            .map(|plugin| plugin.id)
            .collect::<std::collections::HashSet<_>>()
    };
    let before = content();
    assert_eq!(before.len(), 3);
    env.model.push(Step::Tool("gate", serde_json::json!({})));
    session
        .submit("go".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    entered(&env).await;
    let closing = session.close();
    assert!(session.is_closing());
    dirs.write(
        &layout.project.join("b/SKILL.md"),
        &skill_file("b", "project b", "BODY-B-2"),
    );
    env.catalog()
        .refresh(RefreshTarget {
            user: true,
            project: Some(layout.cwd.clone()),
        })
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        content().is_subset(&before),
        "a closing Session created content instances"
    );
    env.gate.release.notify_one();
    closing.await.unwrap();
    env.agent.close().await.unwrap();
}

/// A source of three Skills with a bundled file, and no manifest.
fn installation_source(dirs: &Dirs) -> PathBuf {
    let root = dirs.dir("src/acme-skills");
    dirs.write(
        &root.join("writing/review/SKILL.md"),
        &skill_file("review", "reviews a change", "REVIEW"),
    );
    dirs.write(
        &root.join("writing/review/references/checklist.md"),
        "CHECK-1",
    );
    dirs.write(
        &root.join("tooling/lint/SKILL.md"),
        &skill_file("lint", "lints a change", "LINT"),
    );
    dirs.write(
        &root.join("notes/deploy/SKILL.md"),
        &skill_file("deploy", "deploys a change", "DEPLOY"),
    );
    root.canonicalize().unwrap()
}

/// An operation directory beside the user root, as the App Server hands one
/// to the owner.
fn operation_dir(dirs: &Dirs) -> PathBuf {
    dirs.dir("operation")
}

fn user_root(dirs: &Dirs) -> PathBuf {
    dirs.home.join(".agents/skills")
}

async fn installation_env(dirs: &Dirs) -> (Env, Arc<MemorySkillSettings>) {
    let settings = Arc::new(MemorySkillSettings::default());
    let env = env(
        dirs,
        Options {
            settings: settings.clone(),
            ..Options::default()
        },
    )
    .await;
    (env, settings)
}

async fn recorded(settings: &Arc<MemorySkillSettings>) -> BTreeMap<String, SkillInstallation> {
    settings
        .skills(tokio_util::sync::CancellationToken::new())
        .await
        .unwrap()
        .installations
}

fn selected(preview: &SkillPreview, path: &str) -> SelectedSkill {
    let skill = preview
        .skills
        .iter()
        .find(|skill| skill.path == path)
        .unwrap_or_else(|| panic!("{path} in {preview:?}"));
    SelectedSkill {
        path: skill.path.clone(),
        revision: Some(skill.revision.clone()),
    }
}

fn path_source(source: &Path) -> SkillInstallSource {
    SkillInstallSource::Path {
        path: source.to_owned(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preview_lists_a_source_without_installing_it() {
    let dirs = Dirs::new();
    let source = installation_source(&dirs);
    let (env, settings) = installation_env(&dirs).await;
    let catalog = env.catalog();
    let preview = catalog.preview(&source, "acme-skills", None).await.unwrap();
    let paths: Vec<&str> = preview
        .skills
        .iter()
        .map(|skill| skill.path.as_str())
        .collect();
    assert_eq!(paths, ["notes/deploy", "tooling/lint", "writing/review"]);
    let review = preview
        .skills
        .iter()
        .find(|skill| skill.path == "writing/review")
        .unwrap();
    assert_eq!(review.name, "review");
    assert_eq!(review.description, "reviews a change");
    assert!(review.revision.starts_with("sha256:"), "{review:?}");
    assert!(preview.conflicts.is_empty(), "{preview:?}");
    assert!(!user_root(&dirs).join("acme-skills").exists());
    assert!(recorded(&settings).await.is_empty());
    assert!(catalog.installations().await.unwrap().is_empty());
    env.agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn install_copies_only_the_selected_skill() {
    let dirs = Dirs::new();
    let source = installation_source(&dirs);
    let (env, settings) = installation_env(&dirs).await;
    dirs.write(
        &user_root(&dirs).join("hand/writing/SKILL.md"),
        &skill_file("writing", "hand written", "HAND"),
    );
    let catalog = env.catalog();
    let preview = catalog.preview(&source, "acme-skills", None).await.unwrap();
    let outcome = catalog
        .install(
            "acme-skills",
            path_source(&source),
            &source,
            &operation_dir(&dirs),
            &[selected(&preview, "writing/review")],
            Replace::Create,
        )
        .await
        .unwrap();
    assert!(outcome.previous.is_none());
    assert_eq!(outcome.installation.skills, ["writing/review"]);
    assert!(outcome.installation.revision.starts_with("sha256:"));
    let installed = user_root(&dirs).join("acme-skills");
    assert!(installed.join("writing/review/SKILL.md").is_file());
    assert!(
        installed
            .join("writing/review/references/checklist.md")
            .is_file()
    );
    assert!(!installed.join("tooling/lint").exists());
    assert!(!installed.join("notes/deploy").exists());
    assert!(source.join("tooling/lint/SKILL.md").is_file());
    assert!(user_root(&dirs).join("hand/writing/SKILL.md").is_file());
    let view = catalog.query(None).await.unwrap();
    assert!(
        view.entries.iter().any(|entry| {
            entry.definition.id == "user:acme-skills/writing/review"
                && entry.definition.name == "review"
        }),
        "{view:?}"
    );
    let record = recorded(&settings).await;
    assert_eq!(record["acme-skills"].skills, ["writing/review"]);
    assert_eq!(
        record["acme-skills"].revision,
        outcome.installation.revision
    );
    env.agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_change_under_a_selected_skill_makes_the_preview_stale() {
    let dirs = Dirs::new();
    let source = installation_source(&dirs);
    let (env, settings) = installation_env(&dirs).await;
    let catalog = env.catalog();
    let review = selected(
        &catalog.preview(&source, "acme-skills", None).await.unwrap(),
        "writing/review",
    );
    dirs.write(
        &source.join("writing/review/references/checklist.md"),
        "CHECK-2",
    );
    let stale = catalog
        .install(
            "acme-skills",
            path_source(&source),
            &source,
            &operation_dir(&dirs),
            &[review],
            Replace::Create,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(stale, SkillsError::StalePreview { ref path, .. } if path == "writing/review"),
        "{stale}"
    );
    assert!(!user_root(&dirs).join("acme-skills").exists());
    assert!(recorded(&settings).await.is_empty());

    let review = selected(
        &catalog.preview(&source, "acme-skills", None).await.unwrap(),
        "writing/review",
    );
    dirs.write(&source.join("writing/review/SKILL.md"), "not a Skill");
    let gone = catalog
        .install(
            "acme-skills",
            path_source(&source),
            &source,
            &operation_dir(&dirs),
            &[review],
            Replace::Create,
        )
        .await
        .unwrap_err();
    assert!(matches!(gone, SkillsError::SelectionInvalid(_)), "{gone}");
    assert!(!user_root(&dirs).join("acme-skills").exists());
    env.agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn install_refuses_conflicts_and_selections_outside_the_tree() {
    let dirs = Dirs::new();
    let source = installation_source(&dirs);
    dirs.write(
        &source.join("tooling/lint-copy/SKILL.md"),
        &skill_file("lint", "a second lint", "LINT-2"),
    );
    dirs.write(
        &source.join("notes/deploy-copy/SKILL.md"),
        &skill_file("deploy", "a second deploy", "DEPLOY-2"),
    );
    dirs.write(
        &user_root(&dirs).join("hand/lint/SKILL.md"),
        &skill_file("lint", "hand written", "HAND"),
    );
    let (env, settings) = installation_env(&dirs).await;
    let catalog = env.catalog();
    let preview = catalog.preview(&source, "acme-skills", None).await.unwrap();
    assert_eq!(
        preview
            .conflicts
            .iter()
            .map(|conflict| conflict.path.as_str())
            .collect::<Vec<_>>(),
        ["tooling/lint", "tooling/lint-copy"],
        "{preview:?}"
    );
    assert!(preview.shadows.is_empty(), "{preview:?}");

    let conflict = catalog
        .install(
            "acme-skills",
            path_source(&source),
            &source,
            &operation_dir(&dirs),
            &[selected(&preview, "tooling/lint")],
            Replace::Create,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(conflict, SkillsError::InstallConflict(_)),
        "{conflict}"
    );

    let twice = catalog
        .install(
            "acme-skills",
            path_source(&source),
            &source,
            &operation_dir(&dirs),
            &[
                selected(&preview, "notes/deploy"),
                selected(&preview, "notes/deploy-copy"),
            ],
            Replace::Create,
        )
        .await
        .unwrap_err();
    assert!(matches!(twice, SkillsError::SelectionInvalid(_)), "{twice}");

    for path in ["../escape", "/etc", "missing/skill"] {
        let error = catalog
            .install(
                "acme-skills",
                path_source(&source),
                &source,
                &operation_dir(&dirs),
                &[SelectedSkill {
                    path: path.into(),
                    revision: None,
                }],
                Replace::Create,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(error, SkillsError::SelectionInvalid(_)),
            "{path}: {error}"
        );
    }
    assert!(!user_root(&dirs).join("acme-skills").exists());
    assert!(recorded(&settings).await.is_empty());

    #[cfg(unix)]
    {
        dirs.write(
            &source.join("linked/SKILL.md"),
            &skill_file("linked", "with a link", "LINKED"),
        );
        std::os::unix::fs::symlink(
            source.join("writing/review/SKILL.md"),
            source.join("linked/review.md"),
        )
        .unwrap();
        let preview = catalog.preview(&source, "acme-skills", None).await.unwrap();
        assert!(
            !preview.skills.iter().any(|skill| skill.path == "linked"),
            "{preview:?}"
        );
        assert!(
            preview.issues.iter().any(|issue| matches!(
                issue,
                SkillIssue::Unreadable { path, .. } if path.ends_with("linked/SKILL.md")
            )),
            "{preview:?}"
        );
    }
    env.agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn install_replaces_an_installation_only_with_overwrite() {
    let dirs = Dirs::new();
    let source = installation_source(&dirs);
    let (env, settings) = installation_env(&dirs).await;
    dirs.write(
        &user_root(&dirs).join("hand/writing/SKILL.md"),
        &skill_file("writing", "hand written", "HAND"),
    );
    let catalog = env.catalog();
    let first = catalog
        .install(
            "acme-skills",
            path_source(&source),
            &source,
            &operation_dir(&dirs),
            &[selected(
                &catalog.preview(&source, "acme-skills", None).await.unwrap(),
                "writing/review",
            )],
            Replace::Create,
        )
        .await
        .unwrap();

    let preview = catalog.preview(&source, "acme-skills", None).await.unwrap();
    assert!(preview.conflicts.is_empty(), "{preview:?}");
    let refused = catalog
        .install(
            "acme-skills",
            path_source(&source),
            &source,
            &operation_dir(&dirs),
            &[selected(&preview, "tooling/lint")],
            Replace::Create,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(refused, SkillsError::InstallationExists(ref id) if id == "acme-skills"),
        "{refused}"
    );
    assert!(
        user_root(&dirs)
            .join("acme-skills/writing/review/SKILL.md")
            .is_file()
    );

    let outcome = catalog
        .install(
            "acme-skills",
            path_source(&source),
            &source,
            &operation_dir(&dirs),
            &[selected(&preview, "tooling/lint")],
            Replace::Recorded,
        )
        .await
        .unwrap();
    let previous = outcome.previous.expect("the replaced record");
    assert_eq!(previous.skills, ["writing/review"]);
    assert_eq!(previous.revision, first.installation.revision);
    let installed = user_root(&dirs).join("acme-skills");
    assert!(!installed.join("writing/review").exists());
    assert!(installed.join("tooling/lint/SKILL.md").is_file());
    assert!(user_root(&dirs).join("hand/writing/SKILL.md").is_file());
    assert_eq!(
        recorded(&settings).await["acme-skills"].skills,
        ["tooling/lint"]
    );
    env.agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_keeps_the_selection_and_reports_the_previous_record() {
    let dirs = Dirs::new();
    let source = installation_source(&dirs);
    let (env, settings) = installation_env(&dirs).await;
    let catalog = env.catalog();
    let preview = catalog.preview(&source, "acme-skills", None).await.unwrap();
    let installed = catalog
        .install(
            "acme-skills",
            path_source(&source),
            &source,
            &operation_dir(&dirs),
            &[
                selected(&preview, "writing/review"),
                selected(&preview, "tooling/lint"),
            ],
            Replace::Create,
        )
        .await
        .unwrap();
    let record = catalog.installation("acme-skills").await.unwrap().unwrap();
    assert_eq!(record.skills, ["writing/review", "tooling/lint"]);
    dirs.write(
        &source.join("tooling/lint/SKILL.md"),
        &skill_file("lint", "lints a change", "LINT-2"),
    );
    let selection: Vec<SelectedSkill> = record
        .skills
        .iter()
        .map(|path| SelectedSkill {
            path: path.clone(),
            revision: None,
        })
        .collect();
    let updated = catalog
        .install(
            "acme-skills",
            path_source(&source),
            &source,
            &operation_dir(&dirs),
            &selection,
            Replace::Recorded,
        )
        .await
        .unwrap();
    let previous = updated.previous.clone().expect("the previous record");
    assert_eq!(previous.revision, installed.installation.revision);
    assert_eq!(previous.skills, record.skills);
    assert_eq!(updated.installation.skills, record.skills);
    assert_ne!(updated.installation.revision, previous.revision);
    assert!(
        std::fs::read_to_string(user_root(&dirs).join("acme-skills/tooling/lint/SKILL.md"))
            .unwrap()
            .contains("LINT-2")
    );

    std::fs::remove_dir_all(source.join("writing/review")).unwrap();
    let missing = catalog
        .install(
            "acme-skills",
            path_source(&source),
            &source,
            &operation_dir(&dirs),
            &selection,
            Replace::Recorded,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(missing, SkillsError::SelectionInvalid(_)),
        "{missing}"
    );
    assert!(
        user_root(&dirs)
            .join("acme-skills/writing/review/SKILL.md")
            .is_file()
    );
    assert_eq!(
        recorded(&settings).await["acme-skills"].revision,
        updated.installation.revision
    );
    env.agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remove_deletes_only_what_the_record_owns() {
    let dirs = Dirs::new();
    let source = installation_source(&dirs);
    let (env, settings) = installation_env(&dirs).await;
    dirs.write(
        &user_root(&dirs).join("hand/writing/SKILL.md"),
        &skill_file("writing", "hand written", "HAND"),
    );
    let catalog = env.catalog();
    let preview = catalog.preview(&source, "acme-skills", None).await.unwrap();
    catalog
        .install(
            "acme-skills",
            path_source(&source),
            &source,
            &operation_dir(&dirs),
            &[selected(&preview, "notes/deploy")],
            Replace::Create,
        )
        .await
        .unwrap();
    catalog
        .install(
            "other-skills",
            path_source(&source),
            &source,
            &operation_dir(&dirs),
            &[selected(&preview, "tooling/lint")],
            Replace::Create,
        )
        .await
        .unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(user_root(&dirs), std::fs::Permissions::from_mode(0o500)).unwrap();
        let failed = catalog.uninstall("acme-skills").await.unwrap_err();
        assert!(matches!(failed, SkillsError::RemoveFailed(_)), "{failed}");
        assert_eq!(
            recorded(&settings)
                .await
                .get("acme-skills")
                .map(|record| record.skills.clone()),
            Some(vec!["notes/deploy".to_string()])
        );
        std::fs::set_permissions(user_root(&dirs), std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let (removed, version) = catalog.uninstall("acme-skills").await.unwrap();
    assert_eq!(removed.skills, ["notes/deploy"]);
    assert_eq!(version, catalog.query(None).await.unwrap().version);
    assert!(!user_root(&dirs).join("acme-skills").exists());
    assert!(user_root(&dirs).join("other-skills/tooling/lint").exists());
    assert!(user_root(&dirs).join("hand/writing/SKILL.md").is_file());
    assert!(!recorded(&settings).await.contains_key("acme-skills"));
    let unknown = catalog.uninstall("acme-skills").await.unwrap_err();
    assert!(
        matches!(unknown, SkillsError::UnknownInstallation(_)),
        "{unknown}"
    );
    env.agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_catalog_restores_installations_from_settings() {
    use dim_agent_capabilities::{host_platform::HostPlatform, skill_settings::SkillSettingsHost};

    let dirs = Dirs::new();
    let source = installation_source(&dirs);
    let (env, settings) = installation_env(&dirs).await;
    let catalog = env.catalog();
    catalog
        .install(
            "acme-skills",
            path_source(&source),
            &source,
            &operation_dir(&dirs),
            &[selected(
                &catalog.preview(&source, "acme-skills", None).await.unwrap(),
                "writing/review",
            )],
            Replace::Create,
        )
        .await
        .unwrap();
    env.agent.close().await.unwrap();

    let root = rutis::Ctx::root_with(tokio::runtime::Handle::current());
    root.provide_as::<HostPlatform>(Arc::new(platform(&dirs.home)))
        .unwrap();
    let settings: Arc<dyn SkillSettings> = settings;
    root.provide_as::<SkillSettingsHost>(Arc::new(SkillSettingsHost(settings)))
        .unwrap();
    root.provide_as::<dim_agent_runtime::PartRegistry>(Arc::new(
        dim_agent_runtime::PartRegistry::default(),
    ))
    .unwrap();
    let services = dim_agent_capabilities::catalog();
    let build = rutis_loader::resolve::Build::default();
    let names = dim_agent_runtime::Names::copy(&services, &build);
    (&root.plugin(dim_agent_capabilities::skills::SkillsCatalogPlugin::new(&names).unwrap()))
        .await
        .unwrap();
    let restored = root
        .get::<dim_agent_capabilities::skills::SkillsCatalog>()
        .unwrap();
    let installations = restored.installations().await.unwrap();
    assert_eq!(installations["acme-skills"].skills, ["writing/review"]);
    let view = restored.query(None).await.unwrap();
    assert!(
        view.entries
            .iter()
            .any(|entry| entry.definition.id == "user:acme-skills/writing/review")
    );
    root.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn install_replaces_only_a_directory_the_record_owns() {
    let dirs = Dirs::new();
    let source = installation_source(&dirs);
    let (env, settings) = installation_env(&dirs).await;
    dirs.write(
        &user_root(&dirs).join("acme-skills/hand-written/SKILL.md"),
        &skill_file("hand", "hand written", "HAND"),
    );
    let catalog = env.catalog();
    let review = selected(
        &catalog.preview(&source, "acme-skills", None).await.unwrap(),
        "writing/review",
    );
    for replace in [Replace::Create, Replace::Recorded] {
        let refused = catalog
            .install(
                "acme-skills",
                path_source(&source),
                &source,
                &operation_dir(&dirs),
                std::slice::from_ref(&review),
                replace,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(refused, SkillsError::UnmanagedDirectory(ref message)
                if message.contains("\"acme-skills\"") && message.contains("hand")),
            "{refused}"
        );
    }
    assert!(
        user_root(&dirs)
            .join("acme-skills/hand-written/SKILL.md")
            .is_file()
    );
    assert!(recorded(&settings).await.is_empty());

    dirs.write(&user_root(&dirs).join("from-file"), "not a directory");
    let refused = catalog
        .install(
            "from-file",
            path_source(&source),
            &source,
            &operation_dir(&dirs),
            &[review],
            Replace::Recorded,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(refused, SkillsError::UnmanagedDirectory(_)),
        "{refused}"
    );
    assert!(user_root(&dirs).join("from-file").is_file());
    assert!(recorded(&settings).await.is_empty());
    env.agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_update_cannot_reinstall_an_installation_removed_or_replaced_meanwhile() {
    let dirs = Dirs::new();
    let source = installation_source(&dirs);
    let (env, settings) = installation_env(&dirs).await;
    let catalog = env.catalog();
    let preview = catalog.preview(&source, "acme-skills", None).await.unwrap();
    catalog
        .install(
            "acme-skills",
            path_source(&source),
            &source,
            &operation_dir(&dirs),
            &[selected(&preview, "writing/review")],
            Replace::Create,
        )
        .await
        .unwrap();
    let record = catalog.installation("acme-skills").await.unwrap().unwrap();
    catalog.uninstall("acme-skills").await.unwrap();

    let resurrect = catalog
        .install(
            "acme-skills",
            path_source(&source),
            &source,
            &operation_dir(&dirs),
            &[SelectedSkill {
                path: "writing/review".into(),
                revision: None,
            }],
            Replace::Expected(record),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(resurrect, SkillsError::UnknownInstallation(ref id) if id == "acme-skills"),
        "{resurrect}"
    );
    assert!(!user_root(&dirs).join("acme-skills").exists());
    assert!(recorded(&settings).await.is_empty());

    catalog
        .install(
            "acme-skills",
            path_source(&source),
            &source,
            &operation_dir(&dirs),
            &[selected(&preview, "writing/review")],
            Replace::Create,
        )
        .await
        .unwrap();
    let stale = catalog.installation("acme-skills").await.unwrap().unwrap();
    catalog
        .install(
            "acme-skills",
            path_source(&source),
            &source,
            &dirs.dir("operation-2"),
            &[selected(&preview, "tooling/lint")],
            Replace::Recorded,
        )
        .await
        .unwrap();
    let refused = catalog
        .install(
            "acme-skills",
            path_source(&source),
            &source,
            &dirs.dir("operation-3"),
            &[SelectedSkill {
                path: "writing/review".into(),
                revision: None,
            }],
            Replace::Expected(stale),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(refused, SkillsError::UnknownInstallation(_)),
        "{refused}"
    );
    assert!(
        user_root(&dirs)
            .join("acme-skills/tooling/lint/SKILL.md")
            .is_file()
    );
    assert_eq!(
        recorded(&settings).await["acme-skills"].skills,
        ["tooling/lint"]
    );
    env.agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_file_too_large_for_a_revision_is_reported_and_not_read() {
    let dirs = Dirs::new();
    let source = installation_source(&dirs);
    std::fs::write(
        source.join("tooling/lint/bundle.bin"),
        vec![b'x'; 8 * 1024 * 1024 + 1],
    )
    .unwrap();
    let (env, settings) = installation_env(&dirs).await;
    let catalog = env.catalog();

    let preview = catalog.preview(&source, "acme-skills", None).await.unwrap();
    assert!(
        !preview
            .skills
            .iter()
            .any(|skill| skill.path == "tooling/lint"),
        "{preview:?}"
    );
    assert!(
        preview.issues.iter().any(|issue| matches!(
            issue,
            SkillIssue::Unreadable { path, reason }
                if path.ends_with("tooling/lint/SKILL.md") && reason.contains("larger than")
        )),
        "{preview:?}"
    );

    let refused = catalog
        .install(
            "acme-skills",
            path_source(&source),
            &source,
            &operation_dir(&dirs),
            &[SelectedSkill {
                path: "tooling/lint".into(),
                revision: None,
            }],
            Replace::Create,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(refused, SkillsError::SelectionInvalid(ref message) if message.contains("larger than")),
        "{refused}"
    );
    assert!(!user_root(&dirs).join("acme-skills").exists());
    assert!(recorded(&settings).await.is_empty());
    env.agent.close().await.unwrap();
}

/// Settings whose second installation write fails, as storage does after the
/// files were swapped in.
struct FailingRecord {
    inner: Arc<MemorySkillSettings>,
    writes: std::sync::atomic::AtomicUsize,
}
impl SkillSettings for FailingRecord {
    fn skills(
        &self,
        cancel: tokio_util::sync::CancellationToken,
    ) -> rutis::host::BoxFuture<
        '_,
        std::result::Result<
            dim_agent_capabilities::skill_settings::SkillSettingsSnapshot,
            dim_agent_capabilities::skill_settings::SkillSettingsError,
        >,
    > {
        self.inner.skills(cancel)
    }
    fn update_skills(
        self: Arc<Self>,
        expected: String,
        id: String,
        enabled: bool,
        cancel: tokio_util::sync::CancellationToken,
    ) -> rutis::host::BoxFuture<
        'static,
        std::result::Result<String, dim_agent_capabilities::skill_settings::SkillSettingsError>,
    > {
        self.inner
            .clone()
            .update_skills(expected, id, enabled, cancel)
    }
    fn set_plugin(
        self: Arc<Self>,
        plugin: String,
        root: Option<PathBuf>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> rutis::host::BoxFuture<
        'static,
        std::result::Result<String, dim_agent_capabilities::skill_settings::SkillSettingsError>,
    > {
        self.inner.clone().set_plugin(plugin, root, cancel)
    }
    fn set_installation(
        self: Arc<Self>,
        id: String,
        installation: Option<SkillInstallation>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> rutis::host::BoxFuture<
        'static,
        std::result::Result<String, dim_agent_capabilities::skill_settings::SkillSettingsError>,
    > {
        Box::pin(async move {
            if self
                .writes
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                == 1
            {
                return Err(
                    dim_agent_capabilities::skill_settings::SkillSettingsError::Failed(
                        "storage is read-only".into(),
                    ),
                );
            }
            self.inner
                .clone()
                .set_installation(id, installation, cancel)
                .await
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_record_write_restores_the_previous_installation() {
    let dirs = Dirs::new();
    let source = installation_source(&dirs);
    let settings = Arc::new(FailingRecord {
        inner: Arc::new(MemorySkillSettings::default()),
        writes: std::sync::atomic::AtomicUsize::new(0),
    });
    let env = env(
        &dirs,
        Options {
            settings: settings.clone(),
            ..Options::default()
        },
    )
    .await;
    let catalog = env.catalog();
    let preview = catalog.preview(&source, "acme-skills", None).await.unwrap();
    let first = catalog
        .install(
            "acme-skills",
            path_source(&source),
            &source,
            &operation_dir(&dirs),
            &[selected(&preview, "writing/review")],
            Replace::Create,
        )
        .await
        .unwrap();
    let refused = catalog
        .install(
            "acme-skills",
            path_source(&source),
            &source,
            &dirs.dir("operation-2"),
            &[selected(&preview, "tooling/lint")],
            Replace::Recorded,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(refused, SkillsError::SettingsWrite(_)),
        "{refused}"
    );
    let installed = user_root(&dirs).join("acme-skills");
    assert!(installed.join("writing/review/SKILL.md").is_file());
    assert!(!installed.join("tooling").exists());
    let record = catalog.installation("acme-skills").await.unwrap().unwrap();
    assert_eq!(record.skills, ["writing/review"]);
    assert_eq!(record.revision, first.installation.revision);
    env.agent.close().await.unwrap();
}
