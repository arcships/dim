//! The local execution product profile end to end: real file tools, exec,
//! Permission and Approval, PromptSources and Skills; only the model is
//! scripted.

mod local_support;

use dim_agent_capabilities::{
    approval::Approval,
    defaults,
    execution_settings::{ExecutionSettings, ExecutionSettingsHost},
    history::HistoryRenderer,
    host::*,
    host_platform::{
        ExecShellRuntime, HostPlatform, OutputEncoding, ShellEdition, ShellSource, ShellStatus,
    },
    model_provider::ModelProviderPlugin,
    permission::{CommandRulesHost, NoCommandRules, Permission},
    skill_settings::SkillSettingsHost,
    skills::{MemorySkillSettings, SkillsCatalogPlugin},
    workspace_tools::WorkspaceToolsConfig,
};
use dim_agent_runtime::testing::MinimalHost;
use dim_agent_runtime::*;
use dim_agent_types::*;
use local_support::*;
use std::{path::Path, sync::Arc, time::Duration};

struct Product {
    agent: MinimalHost,
    model: Arc<ScriptedModel>,
}

async fn product(dirs: &Dirs) -> Product {
    let mut platform = HostPlatform::current(ShellStatus::Available(ExecShellRuntime {
        executable_path: "/bin/sh".into(),
        edition: ShellEdition::Sh,
        source: ShellSource::Fallback,
        output_encoding: OutputEncoding::Utf8,
    }))
    .unwrap()
    .with_execution_settings(ExecutionSettingsHost::fixed(ExecutionSettings::default()));
    platform.home = dirs.home.clone();
    let model = Arc::new(ScriptedModel::default());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder
        .session_value(Arc::new(HistoryRenderer::default()))
        .process_value(Arc::new(ModelHost(model.clone())))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CommandRulesHost(Arc::new(NoCommandRules))))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(
                    dim_agent_capabilities::adapters::DefaultPrepareModelPlugin::new(names)?,
                ))
            }),
        );
    defaults::install_local_execution(
        &mut builder,
        ModelProviderPlugin::new,
        WorkspaceToolsConfig {
            output_root: dirs.dir("outputs"),
        },
        dim_agent_capabilities::timeline_tree::BranchToolConfig::default(),
    );
    builder.plugin(|names| Ok(Box::new(SkillsCatalogPlugin::new(names)?)));
    let agent = MinimalHost::start_with(&builder, |root, _| {
        root.provide_as::<HostPlatform>(Arc::new(platform))?;
        root.provide_as::<SkillSettingsHost>(Arc::new(SkillSettingsHost(Arc::new(
            MemorySkillSettings::default(),
        ))))?;
        Ok(())
    })
    .await
    .unwrap();
    Product { agent, model }
}

async fn session(product: &Product, cwd: &Path) -> Session {
    product
        .agent
        .create_session(SessionConfig {
            cwd: cwd.to_string_lossy().into_owned(),
            tools: Some(
                [
                    "read", "write", "edit", "glob", "grep", "exec", "wait", "skill",
                ]
                .map(String::from)
                .to_vec(),
            ),
            permission_policy: PermissionPolicy {
                preset: PermissionPreset::WorkspaceWrite,
                ..Default::default()
            },
            ..SessionConfig::default()
        })
        .await
        .unwrap()
}

/// Waits for the first pending approval of the main branch.
async fn approval_request(session: &Session) -> ApprovalRequestData {
    for _ in 0..400 {
        let mut pending = None;
        session
            .service::<Approval>("main")
            .unwrap()
            .replay(|request| {
                pending.get_or_insert_with(|| request.clone());
            });
        if let Some(data) = pending {
            return data;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("no approval request");
}

async fn tool_results(session: &Session) -> Vec<ToolResult> {
    session
        .history("main", None)
        .await
        .unwrap()
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ToolResult(data) => Some(data.results.clone()),
            _ => None,
        })
        .flatten()
        .map(|call| call.result)
        .collect()
}

/// The product assembly with its default plugins is sound once its Session
/// and a branch are live: the assembly check reports nothing.
#[tokio::test(flavor = "multi_thread")]
async fn the_product_assembly_passes_the_assembly_check() {
    let dirs = Dirs::new();
    let product = product(&dirs).await;
    let session = session(&product, &dirs.dir("work")).await;
    session
        .branch(
            "main".into(),
            session.sequence("main").unwrap(),
            BranchOptions {
                prompt: "check".into(),
                source: BranchSource::Host {
                    operation_id: "check".into(),
                },
            },
        )
        .await
        .unwrap();
    let findings = product.agent.check_assembly().unwrap();
    assert!(findings.is_empty(), "{findings:?}");
    product.agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn workspace_write_asks_for_each_external_mutation() {
    let dirs = Dirs::new();
    let cwd = dirs.dir("work");
    let outside = dirs.dir("work-sibling");
    let product = product(&dirs).await;
    let session = session(&product, &cwd).await;
    let permission = session.service::<Permission>("main").unwrap();
    let version = permission
        .update(
            0,
            PermissionPolicy {
                preset: PermissionPreset::WorkspaceWrite,
                allowed_tools: vec!["write".into(), "edit".into()],
                disallowed_tools: vec![],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    #[allow(unused_mut)]
    let mut targets = vec![
        (
            "../work-sibling/new/note.txt".to_owned(),
            outside.join("new/note.txt"),
        ),
        ("~/note.txt".to_owned(), dirs.home.join("note.txt")),
        (
            dirs.root
                .join("outputs")
                .join(session.id())
                .join("model.txt")
                .to_string_lossy()
                .into_owned(),
            dirs.root
                .join("outputs")
                .join(session.id())
                .join("model.txt"),
        ),
    ];
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&outside, cwd.join("escape")).unwrap();
        targets.push((
            "escape/linked/note.txt".into(),
            outside.join("linked/note.txt"),
        ));
    }
    for (input, target) in targets {
        product.model.push(Step::Tool(
            "write",
            serde_json::json!({"path": input, "content": "before"}),
        ));
        product.model.push(Step::Text("done"));
        session
            .submit("write outside".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        let first = approval_request(&session).await;
        assert_eq!(first.call.name, "write");
        assert!(
            first.reason.contains(&target.to_string_lossy().to_string()),
            "{}",
            first.reason
        );
        assert!(!target.exists(), "an outside write must wait");
        #[cfg(unix)]
        if input == "escape/linked/note.txt" {
            assert_eq!(first.call.input["path"], serde_json::json!(target));
            std::fs::remove_file(cwd.join("escape")).unwrap();
            std::os::unix::fs::symlink(&dirs.home, cwd.join("escape")).unwrap();
        }
        session
            .service::<Approval>("main")
            .unwrap()
            .respond(&first.approval_id, ApprovalDecision::Allow)
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "before");
        #[cfg(unix)]
        if input == "escape/linked/note.txt" {
            assert!(!dirs.home.join("linked/note.txt").exists());
            std::fs::remove_file(cwd.join("escape")).unwrap();
            std::os::unix::fs::symlink(&outside, cwd.join("escape")).unwrap();
        }

        for (decision, old, new) in [
            (ApprovalDecision::Deny, "before", "after"),
            (ApprovalDecision::Allow, "before", "after"),
            (ApprovalDecision::Allow, "after", "again"),
        ] {
            product
                .model
                .push(Step::Tool("read", serde_json::json!({"path": input})));
            product.model.push(Step::Tool(
                "edit",
                serde_json::json!({"path": input, "oldText": old, "newText": new}),
            ));
            product.model.push(Step::Text("done"));
            session
                .submit("edit outside".into(), InputMode::Queue, "main")
                .await
                .unwrap();
            let request = approval_request(&session).await;
            assert_eq!(
                request.call.name, "edit",
                "outside reads run without asking when the facts are absent"
            );
            assert_ne!(request.approval_id, first.approval_id);
            assert_eq!(std::fs::read_to_string(&target).unwrap(), old);
            session
                .service::<Approval>("main")
                .unwrap()
                .respond(&request.approval_id, decision)
                .await
                .unwrap();
            session.idle("main").await.unwrap();
            assert_eq!(
                std::fs::read_to_string(&target).unwrap(),
                if decision == ApprovalDecision::Allow {
                    new
                } else {
                    old
                }
            );
        }
    }
    let state = session
        .get::<dim_agent_capabilities::permission::PermissionState>("main")
        .unwrap()
        .unwrap();
    assert_eq!(
        state.version, version,
        "approvals must not change the policy"
    );
    for (preset, allowed) in [
        (PermissionPreset::ReadOnly, false),
        (PermissionPreset::FullAccess, true),
    ] {
        let version = session
            .get::<dim_agent_capabilities::permission::PermissionState>("main")
            .unwrap()
            .unwrap()
            .version;
        permission
            .update(
                version,
                PermissionPolicy {
                    preset,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let target = outside.join(format!("{preset:?}.txt"));
        product.model.push(Step::Tool(
            "write",
            serde_json::json!({"path": target, "content": "other preset"}),
        ));
        product.model.push(Step::Text("done"));
        session
            .submit("other preset".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        assert_eq!(target.exists(), allowed);
        assert_eq!(
            tool_results(&session).await.last().unwrap().is_error == Some(true),
            !allowed
        );
    }
    session.close().await.unwrap();
    product.agent.close().await.unwrap();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn unresolved_write_targets_fail_without_mutation() {
    let dirs = Dirs::new();
    let cwd = dirs.dir("work");
    std::os::unix::fs::symlink(dirs.root.join("missing"), cwd.join("dangling")).unwrap();
    std::os::unix::fs::symlink("loop", cwd.join("loop")).unwrap();
    let product = product(&dirs).await;
    let session = session(&product, &cwd).await;
    for path in ["dangling/new/file.txt", "loop/file.txt"] {
        product.model.push(Step::Tool(
            "write",
            serde_json::json!({"path": path, "content": "blocked"}),
        ));
        product.model.push(Step::Text("done"));
        session
            .submit("invalid target".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        assert_eq!(
            tool_results(&session).await.last().unwrap().is_error,
            Some(true)
        );
    }
    assert!(!dirs.root.join("missing").exists());
    assert!(
        session
            .history("main", None)
            .await
            .unwrap()
            .iter()
            .all(|event| !matches!(event.kind, EventKind::ApprovalRequest(_)))
    );
    session.close().await.unwrap();
    product.agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_turn_reads_writes_and_runs_an_approved_command_with_real_tools() {
    let dirs = Dirs::new();
    let cwd = dirs.dir("work");
    dirs.write(&cwd.join("AGENTS.md"), "Project rule MARKER-P");
    dirs.write(&cwd.join("input.txt"), "hello\n");
    dirs.write(
        &dirs.home.join(".agents/skills/review/SKILL.md"),
        &skill_file("review", "Review code", "Review body"),
    );
    let product = product(&dirs).await;
    let session = session(&product, &cwd).await;

    product
        .model
        .push(Step::Tool("read", serde_json::json!({"path": "input.txt"})));
    product.model.push(Step::Tool(
        "write",
        serde_json::json!({"path": "notes.txt", "content": "written\n"}),
    ));
    product.model.push(Step::Tool(
        "exec",
        serde_json::json!({"command": "echo ran > ran.txt"}),
    ));
    product.model.push(Step::Text("done"));
    session
        .submit("go".into(), InputMode::Queue, "main")
        .await
        .unwrap();

    let request = approval_request(&session).await;
    assert_eq!(request.call.name, "exec");
    assert!(!cwd.join("ran.txt").exists(), "exec must wait for approval");
    session
        .service::<Approval>("main")
        .unwrap()
        .respond(&request.approval_id, ApprovalDecision::Allow)
        .await
        .unwrap();
    session.idle("main").await.unwrap();

    assert_eq!(
        std::fs::read_to_string(cwd.join("notes.txt")).unwrap(),
        "written\n"
    );
    assert_eq!(
        std::fs::read_to_string(cwd.join("ran.txt")).unwrap(),
        "ran\n"
    );
    let results = tool_results(&session).await;
    assert_eq!(results.len(), 3);
    assert!(results.iter().all(|result| result.is_error != Some(true)));

    let requests = requests(&session).await;
    let first = &requests[0];
    let system = system_text(first.system.as_ref().unwrap());
    let messages = request_text(first);
    assert!(
        !system.contains("MARKER-P") && !system.contains("<cwd>"),
        "the system holds only static content"
    );
    assert!(
        messages
            .contains("<dim-rule source=\"dim/instructions/project\" scope=\"branch\" op=\"set\">")
            && messages.contains("MARKER-P"),
        "AGENTS.md arrives as a rule"
    );
    assert!(messages.contains("<dim-state source=\"dim/environment/context\">"));
    assert!(!system.contains("Review code"), "skills are not in system");
    assert!(
        request_text(first).contains("Review code"),
        "skills arrive as input state"
    );
    let mut declared: Vec<_> = first
        .tools
        .as_ref()
        .unwrap()
        .iter()
        .map(|tool| tool.name.as_str())
        .collect();
    declared.sort_unstable();
    assert_eq!(
        declared,
        [
            "edit", "exec", "glob", "grep", "read", "skill", "wait", "write"
        ]
    );
    let exec: Vec<_> = requests
        .iter()
        .map(|request| {
            let tools = request.tools.as_ref().unwrap();
            let exec = tools.iter().find(|tool| tool.name == "exec").unwrap();
            exec.description.clone()
        })
        .collect();
    assert!(exec.len() > 1 && exec.iter().all(|description| *description == exec[0]));
    assert_eq!(
        exec[0].contains("Process isolation in /permissions")
            && !exec[0].contains("host-isolation.json"),
        cfg!(unix),
        "{}",
        exec[0]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_denied_command_has_no_side_effect() {
    let dirs = Dirs::new();
    let cwd = dirs.dir("work");
    let product = product(&dirs).await;
    let session = session(&product, &cwd).await;

    product.model.push(Step::Tool(
        "exec",
        serde_json::json!({"command": "echo ran > ran.txt"}),
    ));
    product.model.push(Step::Text("done"));
    session
        .submit("go".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    let request = approval_request(&session).await;
    session
        .service::<Approval>("main")
        .unwrap()
        .respond(&request.approval_id, ApprovalDecision::Deny)
        .await
        .unwrap();
    session.idle("main").await.unwrap();

    assert!(!cwd.join("ran.txt").exists());
    let results = tool_results(&session).await;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].is_error, Some(true));
}
