//! Branch permission through the public Session interfaces.
use dim_agent_capabilities::{
    approval::{AdmitCall, Approval},
    defaults, file_access,
    host::*,
    host_platform::{HostPlatform, ShellStatus},
    model_provider::ModelProviderPlugin,
    permission::{
        AdmissionDecision, AdviseTool, CommandRulesHost, Decision, NoCommandRules, Permission,
        PermissionState, PermissionUpdateError, ReadCommandRules, ToolAdviceReply,
        ToolAdviceRequest,
    },
    prompt::InitialPromptConfiguration,
    session_grants::{ReadSessionGrants, SessionGrantsHost},
    tool_registry::definition,
};
use dim_agent_runtime::testing::MinimalHost;
use dim_agent_runtime::*;
use dim_agent_types::{
    grant::{GrantKind, GrantMode, SessionGrant},
    *,
};
use rutis::host::{CtxHost, DynPlugin, Effect, Key, TypeKey};
use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

/// One tool per risk category; `hold` waits for the test.
#[derive(Default)]
struct RiskTools {
    calls: AtomicUsize,
    entered: Notify,
    release: Notify,
}
const TOOLS: [(&str, Option<&str>); 7] = [
    ("read", Some("read_filesystem")),
    ("write", Some("write_filesystem")),
    ("exec", Some("execute_process")),
    ("mcp", Some("network")),
    ("skill", Some("session")),
    ("plain", None),
    ("hold", Some("execute_process")),
];
impl ToolDriver for RiskTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        TOOLS
            .iter()
            .map(|(name, risk)| ToolDefinition {
                risk: risk.map(Into::into),
                ..definition(name, "Risk probe", serde_json::json!({"type":"object"}))
            })
            .collect()
    }
    fn execute(
        &self,
        call: ToolCall,
        _: ToolExecutionContext,
        _: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        Box::pin(async move {
            if call.name == "hold" {
                self.entered.notify_one();
                self.release.notified().await;
            }
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ToolResult::text(format!("{} ran", call.name)))
        })
    }
}

/// The user's command rules as the test sets them; `None` is an unreadable
/// source.
struct TestRules(Mutex<Option<Vec<CommandRule>>>);
impl TestRules {
    fn new(rules: Vec<CommandRule>) -> Arc<Self> {
        Arc::new(Self(Mutex::new(Some(rules))))
    }
    fn set(&self, rules: Option<Vec<CommandRule>>) {
        *self.0.lock().unwrap() = rules;
    }
}
impl ReadCommandRules for TestRules {
    fn rules(&self, _: CancellationToken) -> HostFuture<'_, Vec<CommandRule>> {
        let rules = self.0.lock().unwrap().clone();
        Box::pin(async move { rules.ok_or_else(|| HostError::failed("invalid rules file")) })
    }
}

/// The Session's grants as the test sets them; `None` is an unreadable
/// source.
struct TestGrants(Mutex<Option<Vec<SessionGrant>>>);
impl TestGrants {
    fn new(grants: Vec<SessionGrant>) -> Arc<Self> {
        Arc::new(Self(Mutex::new(Some(grants))))
    }
    fn set(&self, grants: Option<Vec<SessionGrant>>) {
        *self.0.lock().unwrap() = grants;
    }
}
impl ReadSessionGrants for TestGrants {
    fn grants(&self, _: CancellationToken) -> HostFuture<'_, Vec<SessionGrant>> {
        let grants = self.0.lock().unwrap().clone();
        Box::pin(async move { grants.ok_or_else(|| HostError::failed("invalid grants store")) })
    }
}

/// The Session's owned artifact directories as the test sets them; `fails`
/// is an unreadable ownership query.
#[derive(Default)]
struct TestFiles {
    owned: Mutex<Vec<PathBuf>>,
    fails: AtomicBool,
}
impl TestFiles {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    fn own(&self, path: impl Into<PathBuf>) {
        self.owned.lock().unwrap().push(path.into());
    }
    fn fail(&self, fails: bool) {
        self.fails.store(fails, Ordering::SeqCst);
    }
}
impl SessionFilesService for TestFiles {
    fn put(
        &self,
        _: String,
        _: String,
        _: Vec<u8>,
        _: CancellationToken,
    ) -> Pin<
        Box<
            dyn Future<Output = std::result::Result<StoredSessionFile, SessionFileError>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async {
            Err(SessionFileError {
                code: SessionFileErrorCode::NodeUnavailable,
                message: "the test stores no files".into(),
            })
        })
    }
    fn register(
        &self,
        _: String,
        _: PathBuf,
    ) -> Pin<Box<dyn Future<Output = std::result::Result<(), SessionFileError>> + Send + '_>> {
        Box::pin(async { Ok(()) })
    }
    fn owns(
        &self,
        _: String,
        path: PathBuf,
    ) -> Pin<Box<dyn Future<Output = std::result::Result<bool, SessionFileError>> + Send + '_>>
    {
        let owned = self.owned.lock().unwrap().clone();
        let fails = self.fails.load(Ordering::SeqCst);
        Box::pin(async move {
            if fails {
                return Err(SessionFileError {
                    code: SessionFileErrorCode::Storage,
                    message: "invalid ownership store".into(),
                });
            }
            Ok(owned.iter().any(|root| path.starts_with(root)))
        })
    }
    fn owned_paths(
        &self,
        _: String,
    ) -> Pin<
        Box<dyn Future<Output = std::result::Result<Vec<PathBuf>, SessionFileError>> + Send + '_>,
    > {
        let owned = self.owned.lock().unwrap().clone();
        Box::pin(async move { Ok(owned) })
    }
}
fn grant(path: impl Into<PathBuf>, kind: GrantKind, mode: GrantMode) -> SessionGrant {
    SessionGrant {
        path: path.into(),
        kind,
        mode,
    }
}
fn rule(command: &str, matching: CommandMatch, decision: CommandRuleDecision) -> CommandRule {
    CommandRule {
        command: command.into(),
        matching,
        decision,
    }
}

async fn agent(tools: Arc<RiskTools>) -> MinimalHost {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(Arc::new(MemoryTimelineStore::default()));
    defaults::install(&mut builder);
    defaults::mock_model_hosts(&mut builder);
    defaults::tool_host(&mut builder, tools);
    MinimalHost::start(&builder).await.unwrap()
}
/// The default profile with `rules` bound in place of its empty host.
async fn ruled_agent(tools: Arc<RiskTools>, rules: Arc<dyn ReadCommandRules>) -> MinimalHost {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder
        .timeline_store(Arc::new(MemoryTimelineStore::default()))
        .session_value(Arc::new(PromptHost(Arc::new(EmptyPromptHost))))
        .session_value(Arc::new(InitialPromptConfiguration::fixed(None)))
        .session_value(Arc::new(CommandRulesHost(rules)));
    defaults::install_with_prompt_capability(&mut builder, ModelProviderPlugin::new);
    defaults::mock_model_hosts(&mut builder);
    defaults::tool_host(&mut builder, tools);
    MinimalHost::start(&builder).await.unwrap()
}
/// The same profile with grants, the fixed facts and their `sessionFiles`
/// source bound.
async fn granted_agent(
    tools: Arc<RiskTools>,
    grants: Arc<dyn ReadSessionGrants>,
    files: Arc<TestFiles>,
) -> MinimalHost {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder
        .timeline_store(Arc::new(MemoryTimelineStore::default()))
        .session_value(Arc::new(PromptHost(Arc::new(EmptyPromptHost))))
        .session_value(Arc::new(InitialPromptConfiguration::fixed(None)))
        .session_value(Arc::new(CommandRulesHost(Arc::new(NoCommandRules))))
        .session_value(Arc::new(SessionGrantsHost(grants)))
        .process_value(Arc::new(SessionFilesHost(files)))
        .process_value(Arc::new(
            HostPlatform::current(ShellStatus::Unavailable {
                code: "PERMISSION_TEST",
                reason: "The permission flow starts no process".into(),
            })
            .unwrap(),
        ));
    defaults::install_with_prompt_capability(&mut builder, ModelProviderPlugin::new);
    file_access::install(&mut builder);
    defaults::mock_model_hosts(&mut builder);
    defaults::tool_host(&mut builder, tools);
    MinimalHost::start(&builder).await.unwrap()
}
fn config(policy: PermissionPolicy) -> SessionConfig {
    SessionConfig {
        tools: Some(TOOLS.iter().map(|(name, _)| name.to_string()).collect()),
        permission_policy: policy,
        ..SessionConfig::default()
    }
}
fn config_in(cwd: &Path, policy: PermissionPolicy) -> SessionConfig {
    SessionConfig {
        cwd: cwd.to_string_lossy().into_owned(),
        ..config(policy)
    }
}
fn preset(preset: PermissionPreset) -> PermissionPolicy {
    PermissionPolicy {
        preset,
        ..PermissionPolicy::default()
    }
}
/// Runs `tool:<name>` on main; returns whether its result was an error.
async fn call(session: &Session, name: &str) -> bool {
    session
        .submit(format!("tool:{name} {{}}"), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    last_result(session, "main").await.is_error == Some(true)
}
/// Submits `exec` with `command` on main.
async fn submit_exec(session: &Session, command: &str) {
    let input = serde_json::json!({ "command": command });
    session
        .submit(format!("tool:exec {input}"), InputMode::Queue, "main")
        .await
        .unwrap();
}
/// Runs `exec` with `command`; returns its result.
async fn exec(session: &Session, command: &str) -> ToolResult {
    submit_exec(session, command).await;
    session.idle("main").await.unwrap();
    last_result(session, "main").await
}
/// Waits for the approval request of a call with `command` on main.
async fn approval_request(session: &Session, command: &str) -> String {
    loop {
        let found = session
            .local("main", ..)
            .await
            .unwrap()
            .iter()
            .find_map(|event| match &event.kind {
                EventKind::ApprovalRequest(data)
                    if data
                        .call
                        .input
                        .get("command")
                        .and_then(|value| value.as_str())
                        == Some(command) =>
                {
                    Some(data.approval_id.clone())
                }
                _ => None,
            });
        if let Some(id) = found {
            return id;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
async fn approval_requests(session: &Session) -> usize {
    session
        .local("main", ..)
        .await
        .unwrap()
        .iter()
        .filter(|event| matches!(event.kind, EventKind::ApprovalRequest(_)))
        .count()
}
fn text(result: &ToolResult) -> &str {
    match &result.content {
        ToolContent::Text(text) => text,
        other => panic!("text result expected: {other:?}"),
    }
}
async fn last_result(session: &Session, branch: &str) -> ToolResult {
    session
        .history(branch, None)
        .await
        .unwrap()
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ToolResult(data) => Some(data.results[0].result.clone()),
            _ => None,
        })
        .unwrap()
}
/// Decides a call of `name` without input.
async fn verdict(permission: &Permission, name: &str, risk: Option<&str>) -> &'static str {
    let call = ApprovalCallData {
        name: name.into(),
        input: JsonObject::new(),
    };
    match permission
        .decide(&call, risk, &CancellationToken::new())
        .await
        .unwrap()
    {
        Decision::Allow => "allow",
        Decision::Ask { .. } => "ask",
        Decision::Deny { .. } => "deny",
    }
}
/// Decides a `write` call for `path`.
async fn decide_write(permission: &Permission, path: &Path) -> Decision {
    let mut input = JsonObject::new();
    input.insert("path".into(), path.to_string_lossy().into_owned().into());
    permission
        .decide(
            &ApprovalCallData {
                name: "write".into(),
                input,
            },
            Some("write_filesystem"),
            &CancellationToken::new(),
        )
        .await
        .unwrap()
}
/// Decides a `write` call for `path`; returns its verdict.
async fn write_verdict(permission: &Permission, path: &Path) -> &'static str {
    match decide_write(permission, path).await {
        Decision::Allow => "allow",
        Decision::Ask { .. } => "ask",
        Decision::Deny { .. } => "deny",
    }
}
/// Decides a file-read tool `name` for `path`; `None` omits the path input,
/// which `grep` and `glob` read as the Session cwd.
async fn decide_read(permission: &Permission, name: &str, path: Option<&Path>) -> Decision {
    let mut input = JsonObject::new();
    if let Some(path) = path {
        input.insert("path".into(), path.to_string_lossy().into_owned().into());
    }
    permission
        .decide(
            &ApprovalCallData {
                name: name.into(),
                input,
            },
            Some("read_filesystem"),
            &CancellationToken::new(),
        )
        .await
        .unwrap()
}
/// Decides a `read` call for `path`; returns its verdict.
async fn read_verdict(permission: &Permission, path: &Path) -> &'static str {
    match decide_read(permission, "read", Some(path)).await {
        Decision::Allow => "allow",
        Decision::Ask { .. } => "ask",
        Decision::Deny { .. } => "deny",
    }
}
fn temporary_directory(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("dim-permission-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    std::fs::canonicalize(path).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_approval_and_file_access_are_independent() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let root = temporary_directory("separate-controls");
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let inside = work.join("file.txt");
        let outside = root.join("outside.txt");
        let owned = root.join("owned.txt");
        let granted = root.join("granted.txt");
        let files = TestFiles::new();
        files.own(&owned);
        let grants = TestGrants::new(vec![grant(&granted, GrantKind::File, GrantMode::Write)]);
        let tools = Arc::new(RiskTools::default());
        let agent = granted_agent(tools.clone(), grants, files).await;
        for access in [
            FileAccess::ReadOnly,
            FileAccess::WorkspaceReadWrite,
            FileAccess::Unrestricted,
        ] {
            for approval in [ToolApproval::Ask, ToolApproval::Bypass] {
                let session = agent
                    .create_session(config_in(
                        &work,
                        PermissionPolicy {
                            preset: PermissionPreset::FullAccess,
                            file_access: Some(access),
                            tool_approval: Some(approval),
                            ..Default::default()
                        },
                    ))
                    .await
                    .unwrap();
                let permission = session.service::<Permission>("main").unwrap();
                let tool = if approval == ToolApproval::Ask {
                    "ask"
                } else {
                    "allow"
                };
                assert_eq!(read_verdict(&permission, &inside).await, tool);
                assert_eq!(verdict(&permission, "plain", None).await, tool);
                for path in [&inside, &outside, &owned, &granted] {
                    let expected = match access {
                        FileAccess::ReadOnly => "deny",
                        FileAccess::WorkspaceReadWrite if path == &owned => "deny",
                        FileAccess::WorkspaceReadWrite if path == &outside => "ask",
                        _ => tool,
                    };
                    assert_eq!(
                        write_verdict(&permission, path).await,
                        expected,
                        "{access:?}/{approval:?}: {}",
                        path.display()
                    );
                }
                assert_eq!(
                    read_verdict(&permission, &outside).await,
                    if access == FileAccess::Unrestricted {
                        tool
                    } else {
                        "ask"
                    }
                );
                session.close().await.unwrap();
            }
        }
        for access in [FileAccess::WorkspaceReadWrite, FileAccess::Unrestricted] {
            let session = agent
                .create_session(config_in(
                    &work,
                    PermissionPolicy {
                        preset: PermissionPreset::ReadOnly,
                        file_access: Some(access),
                        ..Default::default()
                    },
                ))
                .await
                .unwrap();
            let permission = session.service::<Permission>("main").unwrap();
            for path in [&inside, &outside, &granted] {
                assert_eq!(
                    write_verdict(&permission, path).await,
                    "deny",
                    "{access:?} must retain legacy tool denials: {}",
                    path.display()
                );
            }
            session.close().await.unwrap();
        }
        let session = agent
            .create_session(config_in(
                &work,
                PermissionPolicy {
                    preset: PermissionPreset::FullAccess,
                    file_access: Some(FileAccess::WorkspaceReadWrite),
                    tool_approval: Some(ToolApproval::Ask),
                    ..Default::default()
                },
            ))
            .await
            .unwrap();
        let input = serde_json::json!({"path":outside});
        session
            .submit(format!("tool:write {input}"), InputMode::Queue, "main")
            .await
            .unwrap();
        let approval = session.service::<Approval>("main").unwrap();
        let id = loop {
            let mut ids = Vec::new();
            approval.replay(|request| ids.push(request.approval_id.clone()));
            if let Some(id) = ids.pop() {
                break id;
            }
            tokio::task::yield_now().await;
        };
        assert_eq!(tools.calls.load(Ordering::SeqCst), 0);
        approval
            .respond(&id, ApprovalDecision::Allow)
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        assert_eq!(tools.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            session
                .local("main", ..)
                .await
                .unwrap()
                .iter()
                .filter(|event| matches!(event.kind, EventKind::ApprovalRequest(_)))
                .count(),
            1
        );
        let permission = session.service::<Permission>("main").unwrap();
        let mut policy = session
            .get::<PermissionState>("main")
            .unwrap()
            .unwrap()
            .policy;
        policy.file_access = Some(FileAccess::ReadOnly);
        policy.tool_approval = Some(ToolApproval::Bypass);
        policy.allowed_tools.push("write".into());
        permission.update(0, policy).await.unwrap();
        assert_eq!(write_verdict(&permission, &inside).await, "deny");
        session.close().await.unwrap();
        for approval in [ToolApproval::Ask, ToolApproval::Bypass] {
            let session = agent
                .create_session(config_in(
                    &work,
                    PermissionPolicy {
                        preset: PermissionPreset::ReadOnly,
                        tool_approval: Some(approval),
                        allowed_tools: vec!["write".into()],
                        ..Default::default()
                    },
                ))
                .await
                .unwrap();
            let permission = session.service::<Permission>("main").unwrap();
            assert_eq!(write_verdict(&permission, &inside).await, "allow");
            permission
                .update(
                    0,
                    PermissionPolicy {
                        preset: PermissionPreset::ReadOnly,
                        tool_approval: Some(approval),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            assert_eq!(write_verdict(&permission, &inside).await, "deny");
            session.close().await.unwrap();
        }
        agent.close().await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    })
    .await
    .expect("separate permission controls flow completed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_composition_without_facts_builds_and_asks() {
    let work = temporary_directory("no-grants");
    let agent = agent(Arc::new(RiskTools::default())).await;
    let session = agent
        .create_session(config_in(&work, preset(PermissionPreset::WorkspaceWrite)))
        .await
        .unwrap();
    let permission = session.service::<Permission>("main").unwrap();
    assert_eq!(
        write_verdict(&permission, &work.join("inside.txt")).await,
        "allow"
    );
    let target = work.parent().unwrap().join("outside.txt");
    assert!(matches!(
        decide_write(&permission, &target).await,
        Decision::Ask { .. }
    ));
    permission
        .update(
            0,
            PermissionPolicy {
                preset: PermissionPreset::ReadOnly,
                file_access: Some(FileAccess::WorkspaceReadWrite),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(write_verdict(&permission, &target).await, "deny");
    session.close().await.unwrap();
    agent.close().await.unwrap();
    let _ = std::fs::remove_dir_all(work);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn file_write_facts_decide_each_path_by_preset() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let root = temporary_directory("facts");
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let outside = root.join("outside.txt");
        let owned = root.join("owned").join("artifact.txt");
        std::fs::create_dir_all(owned.parent().unwrap()).unwrap();
        std::fs::write(&owned, b"owned").unwrap();
        let system =
            PathBuf::from("/usr/bin").join(format!("dim-permission-{}", uuid::Uuid::new_v4()));
        let grants = TestGrants::new(Vec::new());
        let files = TestFiles::new();
        files.own(owned.clone());
        let agent = granted_agent(Arc::new(RiskTools::default()), grants, files.clone()).await;

        let inside = work.join("file.txt");
        for (policy, expected) in [
            (PermissionPreset::ReadOnly, ["deny", "deny", "deny", "deny"]),
            (
                PermissionPreset::WorkspaceWrite,
                ["allow", "ask", "deny", "deny"],
            ),
            (
                PermissionPreset::FullAccess,
                ["allow", "allow", "allow", "allow"],
            ),
        ] {
            let session = agent
                .create_session(config_in(&work, preset(policy)))
                .await
                .unwrap();
            let permission = session.service::<Permission>("main").unwrap();
            for (path, expected) in [&inside, &outside, &owned, &system]
                .into_iter()
                .zip(expected)
            {
                assert_eq!(
                    write_verdict(&permission, path).await,
                    expected,
                    "{policy:?} {}",
                    path.display()
                );
            }
            session.close().await.unwrap();
        }

        // A relative target is resolved against the Session cwd with the
        // tools' resolver before the facts.
        let session = agent
            .create_session(config_in(&work, preset(PermissionPreset::WorkspaceWrite)))
            .await
            .unwrap();
        let permission = session.service::<Permission>("main").unwrap();
        assert_eq!(
            write_verdict(&permission, Path::new("inside.txt")).await,
            "allow"
        );
        assert_eq!(
            write_verdict(&permission, Path::new("../outside.txt")).await,
            "ask"
        );
        let escape = PathBuf::from("../".repeat(work.components().count() - 1)).join("etc/hosts");
        assert_eq!(write_verdict(&permission, &escape).await, "deny");
        assert_eq!(write_verdict(&permission, Path::new("")).await, "ask");
        session.close().await.unwrap();

        // An explicit tool allow does not bypass a path fact.
        let session = agent
            .create_session(config_in(
                &work,
                PermissionPolicy {
                    preset: PermissionPreset::ReadOnly,
                    allowed_tools: vec!["write".into()],
                    ..Default::default()
                },
            ))
            .await
            .unwrap();
        let permission = session.service::<Permission>("main").unwrap();
        assert_eq!(write_verdict(&permission, &inside).await, "allow");
        assert_eq!(write_verdict(&permission, &owned).await, "deny");
        assert_eq!(write_verdict(&permission, &system).await, "deny");
        session.close().await.unwrap();

        // An unreadable ownership or grants source denies instead of asking.
        files.fail(true);
        let session = agent
            .create_session(config_in(&work, preset(PermissionPreset::WorkspaceWrite)))
            .await
            .unwrap();
        let permission = session.service::<Permission>("main").unwrap();
        for path in [&inside, &outside] {
            assert_eq!(write_verdict(&permission, path).await, "deny");
        }
        session.close().await.unwrap();
        agent.close().await.unwrap();
        let _ = std::fs::remove_dir_all(root);
    })
    .await
    .expect("file facts flow completed");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn file_read_facts_decide_each_path_by_preset() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let root = temporary_directory("reads");
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let inside = work.join("file.txt");
        std::fs::write(&inside, b"inside").unwrap();
        let outside = root.join("outside.txt");
        std::fs::write(&outside, b"outside").unwrap();
        let owned = root.join("owned").join("artifact.txt");
        std::fs::create_dir_all(owned.parent().unwrap()).unwrap();
        std::fs::write(&owned, b"owned").unwrap();
        let system =
            PathBuf::from("/usr/bin").join(format!("dim-permission-{}", uuid::Uuid::new_v4()));
        let granted = root.join("granted");
        std::fs::create_dir_all(&granted).unwrap();
        let read_granted = granted.join("read.txt");
        std::fs::write(&read_granted, b"read").unwrap();
        let write_granted = granted.join("write.txt");
        std::fs::write(&write_granted, b"write").unwrap();
        let grants = TestGrants::new(vec![
            grant(&read_granted, GrantKind::File, GrantMode::Read),
            grant(&write_granted, GrantKind::File, GrantMode::Write),
        ]);
        let files = TestFiles::new();
        files.own(owned.clone());
        let agent = granted_agent(
            Arc::new(RiskTools::default()),
            grants.clone(),
            files.clone(),
        )
        .await;

        for (policy, expected) in [
            (
                PermissionPreset::ReadOnly,
                ["allow", "ask", "allow", "allow", "allow", "ask"],
            ),
            (
                PermissionPreset::WorkspaceWrite,
                ["allow", "ask", "allow", "allow", "allow", "ask"],
            ),
            (PermissionPreset::FullAccess, ["allow"; 6]),
        ] {
            let session = agent
                .create_session(config_in(&work, preset(policy)))
                .await
                .unwrap();
            let permission = session.service::<Permission>("main").unwrap();
            let ask = root.join("ask.txt");
            for (path, expected) in [&inside, &outside, &owned, &system, &read_granted, &ask]
                .into_iter()
                .zip(expected)
            {
                assert_eq!(
                    read_verdict(&permission, path).await,
                    expected,
                    "{policy:?} {}",
                    path.display()
                );
            }
            // A write grant also permits reads.
            assert_eq!(read_verdict(&permission, &write_granted).await, "allow");
            session.close().await.unwrap();
        }

        // A relative read is resolved against the Session cwd with the tools'
        // resolver before the facts. `grep` and `glob` without a path search
        // the cwd, which is the workspace.
        let session = agent
            .create_session(config_in(&work, preset(PermissionPreset::WorkspaceWrite)))
            .await
            .unwrap();
        let permission = session.service::<Permission>("main").unwrap();
        assert_eq!(
            read_verdict(&permission, Path::new("file.txt")).await,
            "allow"
        );
        assert_eq!(
            read_verdict(&permission, Path::new("../outside.txt")).await,
            "ask"
        );
        for name in ["grep", "glob"] {
            for path in [None, Some(Path::new(""))] {
                assert_eq!(
                    decide_read(&permission, name, path).await,
                    Decision::Allow,
                    "{name} without a path searches the cwd"
                );
            }
            assert!(
                matches!(
                    decide_read(&permission, name, Some(Path::new("../outside.txt"))).await,
                    Decision::Ask { .. }
                ),
                "{name} outside the workspace"
            );
        }
        session.close().await.unwrap();

        // A workspace symlink that points outside does not match the workspace,
        // and one that points inside does.
        let escape = work.join("escape");
        let into_workspace = root.join("into-workspace");
        std::fs::create_dir_all(&into_workspace).unwrap();
        std::os::unix::fs::symlink(&root, &escape).unwrap();
        std::os::unix::fs::symlink(&work, into_workspace.join("work")).unwrap();
        let session = agent
            .create_session(config_in(&work, preset(PermissionPreset::WorkspaceWrite)))
            .await
            .unwrap();
        let permission = session.service::<Permission>("main").unwrap();
        assert_eq!(
            read_verdict(&permission, &escape.join("outside.txt")).await,
            "ask"
        );
        assert_eq!(
            read_verdict(&permission, &into_workspace.join("work/file.txt")).await,
            "allow"
        );
        session.close().await.unwrap();

        // An explicit tool allow does not bypass a path fact.
        let session = agent
            .create_session(config_in(
                &work,
                PermissionPolicy {
                    preset: PermissionPreset::ReadOnly,
                    allowed_tools: vec!["read".into()],
                    ..Default::default()
                },
            ))
            .await
            .unwrap();
        let permission = session.service::<Permission>("main").unwrap();
        assert_eq!(read_verdict(&permission, &inside).await, "allow");
        assert_eq!(read_verdict(&permission, &outside).await, "ask");
        session.close().await.unwrap();

        // An unreadable ownership or grants source denies instead of asking.
        files.fail(true);
        let session = agent
            .create_session(config_in(&work, preset(PermissionPreset::WorkspaceWrite)))
            .await
            .unwrap();
        let permission = session.service::<Permission>("main").unwrap();
        for path in [&inside, &outside] {
            assert_eq!(read_verdict(&permission, path).await, "deny");
        }
        session.close().await.unwrap();
        grants.set(None);
        files.fail(false);
        let session = agent
            .create_session(config_in(&work, preset(PermissionPreset::WorkspaceWrite)))
            .await
            .unwrap();
        let permission = session.service::<Permission>("main").unwrap();
        assert_eq!(read_verdict(&permission, &outside).await, "deny");
        session.close().await.unwrap();

        agent.close().await.unwrap();
        let _ = std::fs::remove_dir_all(root);
    })
    .await
    .expect("file read facts flow completed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_grant_matches_only_the_canonical_target_and_mode() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let root = temporary_directory("grants");
        let work = root.join("work");
        let granted = root.join("granted");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&granted).unwrap();
        let grants = TestGrants::new(Vec::new());
        let files = TestFiles::new();
        let tools = Arc::new(RiskTools::default());
        let agent = granted_agent(tools.clone(), grants.clone(), files).await;
        let session = agent
            .create_session(config_in(&work, preset(PermissionPreset::WorkspaceWrite)))
            .await
            .unwrap();
        let permission = session.service::<Permission>("main").unwrap();
        let target = granted.join("new.txt");

        assert!(matches!(
            decide_write(&permission, &target).await,
            Decision::Ask { .. }
        ));
        grants.set(Some(vec![grant(
            &target,
            GrantKind::File,
            GrantMode::Write,
        )]));
        assert_eq!(decide_write(&permission, &target).await, Decision::Allow);

        // A read grant cannot authorize a write.
        grants.set(Some(vec![grant(&target, GrantKind::File, GrantMode::Read)]));
        assert!(matches!(
            decide_write(&permission, &target).await,
            Decision::Ask { .. }
        ));

        // A directory grant covers children but not a `..` escape.
        grants.set(Some(vec![grant(
            &granted,
            GrantKind::Dir,
            GrantMode::Write,
        )]));
        assert_eq!(
            decide_write(&permission, &granted.join("child.txt")).await,
            Decision::Allow
        );
        assert!(matches!(
            decide_write(&permission, &granted.join("..").join("escaped.txt")).await,
            Decision::Ask { .. }
        ));

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            // A symlink is matched by its canonical target: a file grant on
            // the target admits the link, and a directory grant does not
            // cover a link that resolves outside it.
            let real = granted.join("real.txt");
            let link = granted.join("link.txt");
            std::fs::write(&real, b"real").unwrap();
            symlink(&real, &link).unwrap();
            grants.set(Some(vec![grant(&real, GrantKind::File, GrantMode::Write)]));
            assert_eq!(decide_write(&permission, &link).await, Decision::Allow);

            let outside = root.join("outside");
            let escape = granted.join("escape");
            std::fs::create_dir_all(&outside).unwrap();
            symlink(&outside, &escape).unwrap();
            grants.set(Some(vec![grant(
                &granted,
                GrantKind::Dir,
                GrantMode::Write,
            )]));
            assert!(matches!(
                decide_write(&permission, &escape.join("child.txt")).await,
                Decision::Ask { .. }
            ));
        }

        // The end-to-end call re-checks through Permission and runs without
        // an approval prompt.
        grants.set(Some(vec![grant(
            &granted,
            GrantKind::Dir,
            GrantMode::Write,
        )]));
        let input = serde_json::json!({"path": target});
        session
            .submit(format!("tool:write {input}"), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        assert_ne!(last_result(&session, "main").await.is_error, Some(true));
        assert_eq!(approval_requests(&session).await, 0);
        assert_eq!(tools.calls.load(Ordering::SeqCst), 1);

        // A grant source that cannot be read denies instead of asking.
        grants.set(None);
        assert!(matches!(
            decide_write(&permission, &target).await,
            Decision::Deny { .. }
        ));

        session.close().await.unwrap();
        agent.close().await.unwrap();
        let _ = std::fs::remove_dir_all(root);
    })
    .await
    .expect("grant flow completed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn presets_decide_by_risk_and_denied_calls_do_not_run() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let tools = Arc::new(RiskTools::default());
        let agent = agent(tools.clone()).await;
        let expected = [
            (
                PermissionPreset::ReadOnly,
                ["allow", "deny", "deny", "deny", "allow", "deny"],
            ),
            (
                PermissionPreset::WorkspaceWrite,
                ["allow", "ask", "ask", "ask", "allow", "ask"],
            ),
            (PermissionPreset::FullAccess, ["allow"; 6]),
        ];
        for (policy, verdicts) in expected {
            let session = agent.create_session(config(preset(policy))).await.unwrap();
            let permission = session.service::<Permission>("main").unwrap();
            for ((name, risk), expected) in TOOLS.iter().zip(verdicts) {
                assert_eq!(
                    verdict(&permission, name, *risk).await,
                    expected,
                    "{policy:?} {name}"
                );
            }
            session.close().await.unwrap();
        }
        let session = agent
            .create_session(config(PermissionPolicy::default()))
            .await
            .unwrap();
        assert!(call(&session, "write").await);
        let denied = last_result(&session, "main").await;
        assert_eq!(
            denied.content,
            ToolContent::Text(
                "Tool `write` is not allowed by the read-only preset (writing files)".into()
            )
        );
        assert!(!call(&session, "read").await);
        assert_eq!(tools.calls.load(Ordering::SeqCst), 1);
        session.close().await.unwrap();
        agent.close().await.unwrap();
    })
    .await
    .expect("permission flow completed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_lists_override_presets_and_disallowed_wins() {
    let agent = agent(Arc::new(RiskTools::default())).await;
    let cases = [
        (
            PermissionPolicy {
                preset: PermissionPreset::FullAccess,
                allowed_tools: vec![],
                disallowed_tools: vec!["read".into()],
                ..Default::default()
            },
            "deny",
        ),
        (
            PermissionPolicy {
                preset: PermissionPreset::ReadOnly,
                allowed_tools: vec!["exec".into()],
                disallowed_tools: vec![],
                ..Default::default()
            },
            "allow",
        ),
        (
            PermissionPolicy {
                preset: PermissionPreset::FullAccess,
                allowed_tools: vec!["read".into(), "exec".into()],
                disallowed_tools: vec!["read".into(), "exec".into()],
                ..Default::default()
            },
            "deny",
        ),
        (
            PermissionPolicy {
                tool_approval: Some(ToolApproval::Bypass),
                disallowed_tools: vec!["exec".into()],
                ..Default::default()
            },
            "deny",
        ),
        (
            PermissionPolicy {
                tool_approval: Some(ToolApproval::Ask),
                allowed_tools: vec!["exec".into()],
                ..Default::default()
            },
            "allow",
        ),
    ];
    for (policy, expected) in cases {
        let name = policy
            .disallowed_tools
            .first()
            .or(policy.allowed_tools.first())
            .unwrap()
            .clone();
        let risk = TOOLS.iter().find(|(tool, _)| *tool == name).unwrap().1;
        let session = agent.create_session(config(policy)).await.unwrap();
        let permission = session.service::<Permission>("main").unwrap();
        assert_eq!(verdict(&permission, &name, risk).await, expected);
        session.close().await.unwrap();
    }
    agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn updates_are_versioned_per_branch_take_effect_when_dispatched_and_survive_restore() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let tools = Arc::new(RiskTools::default());
        let agent = agent(tools.clone()).await;
        let session = agent
            .create_session(config(preset(PermissionPreset::FullAccess)))
            .await
            .unwrap();
        let permission = session.service::<Permission>("main").unwrap();
        let changes = async |session: &Session, branch: &str| {
            session
                .local(branch, ..)
                .await
                .unwrap()
                .iter()
                .filter(|event| matches!(event.kind, EventKind::PermissionChanged(_)))
                .count()
        };

        session
            .submit("tool:hold {}".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        tools.entered.notified().await;
        let version = permission
            .update(0, preset(PermissionPreset::ReadOnly))
            .await
            .unwrap();
        assert!(version > 0);
        assert_eq!(
            session.get::<PermissionState>("main").unwrap().unwrap(),
            PermissionState {
                version,
                policy: preset(PermissionPreset::ReadOnly)
            }
        );
        tools.release.notify_one();
        session.idle("main").await.unwrap();
        assert_eq!(
            last_result(&session, "main").await.content,
            ToolContent::Text("hold ran".into())
        );
        assert!(call(&session, "exec").await);

        assert_eq!(
            permission
                .update(0, preset(PermissionPreset::FullAccess))
                .await,
            Err(PermissionUpdateError::Conflict { current: version })
        );
        assert_eq!(changes(&session, "main").await, 1);
        let (first, second) = tokio::join!(
            permission.update(version, preset(PermissionPreset::WorkspaceWrite)),
            permission.update(version, preset(PermissionPreset::FullAccess)),
        );
        let (won, lost) = match (first, second) {
            (Ok(won), lost) | (lost, Ok(won)) => (won, lost),
            other => panic!("one update must win: {other:?}"),
        };
        assert_eq!(lost, Err(PermissionUpdateError::Conflict { current: won }));
        assert_eq!(changes(&session, "main").await, 2);
        let current = session.get::<PermissionState>("main").unwrap().unwrap();
        assert_eq!(current.version, won);

        let tail = session.sequence("main").unwrap();
        let child = session
            .branch(
                "main".into(),
                tail,
                BranchOptions {
                    prompt: "child".into(),
                    source: BranchSource::Host {
                        operation_id: "permission-child".into(),
                    },
                },
            )
            .await
            .unwrap();
        let inherited = session.get::<PermissionState>(&child).unwrap().unwrap();
        assert_eq!(
            inherited,
            PermissionState {
                version: 0,
                policy: current.policy.clone()
            }
        );
        let child_permission = session.service::<Permission>(&child).unwrap();
        child_permission
            .update(0, preset(PermissionPreset::ReadOnly))
            .await
            .unwrap();
        assert_eq!(
            session.get::<PermissionState>("main").unwrap().unwrap(),
            current
        );

        let id = session.id().to_owned();
        session.close().await.unwrap();
        drop(session);
        let restored = agent.restore_session(id).await.unwrap();
        assert_eq!(
            restored.get::<PermissionState>("main").unwrap().unwrap(),
            current
        );
        restored.close().await.unwrap();
        agent.close().await.unwrap();
    })
    .await
    .expect("permission flow completed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_allowed_approval_is_rechecked_against_the_current_policy() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let tools = Arc::new(RiskTools::default());
        let agent = agent(tools.clone()).await;
        let session = agent
            .create_session(config(preset(PermissionPreset::WorkspaceWrite)))
            .await
            .unwrap();
        session
            .submit("tool:exec {}".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        let approval_id = loop {
            let found = session
                .local("main", ..)
                .await
                .unwrap()
                .iter()
                .find_map(|event| match &event.kind {
                    EventKind::ApprovalRequest(data) => Some(data.approval_id.clone()),
                    _ => None,
                });
            if let Some(id) = found {
                break id;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        let permission = session.service::<Permission>("main").unwrap();
        permission
            .update(
                0,
                PermissionPolicy {
                    preset: PermissionPreset::WorkspaceWrite,
                    allowed_tools: vec![],
                    disallowed_tools: vec!["exec".into()],
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let approval = session.service::<Approval>("main").unwrap();
        approval
            .respond(&approval_id, ApprovalDecision::Allow)
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        assert!(last_result(&session, "main").await.is_error == Some(true));
        assert!(
            session
                .local("main", ..)
                .await
                .unwrap()
                .iter()
                .any(|event| matches!(
                    &event.kind,
                    EventKind::ApprovalCancelled(data) if data.approval_id == approval_id
                ))
        );
        assert_eq!(tools.calls.load(Ordering::SeqCst), 0);
        session.close().await.unwrap();
        agent.close().await.unwrap();
    })
    .await
    .expect("approval flow completed");
}

/// Commits an `allow` for every pending approval while `stop/interrupt` is
/// dispatched, after ToolExecutor cancelled the batch and before the admit
/// waiting on it runs: the response then finds its request cancelled.
struct RespondOnStop {
    dispatcher: rutis::host::Key<Dispatcher>,
    approval: rutis::host::Key<Approval>,
}
impl RespondOnStop {
    fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            dispatcher: names.key()?,
            approval: names.key()?,
        })
    }
}
impl rutis::host::DynPlugin for RespondOnStop {
    fn name(&self) -> &str {
        "RespondOnStop"
    }
    fn injects(&self) -> Vec<rutis::host::TypeKey> {
        vec![self.dispatcher.erase(), self.approval.erase()]
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
    ) -> rutis::host::BoxFuture<'a, rutis::Result<rutis::host::Effect>> {
        use futures_util::FutureExt;
        use rutis::host::CtxHost;
        Box::pin(async move {
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let approval = ctx.require_at(&self.approval)?;
            dispatcher.subscribe(ctx, "RespondOnStop", &["stop/interrupt"], move |_| {
                let mut pending = Vec::new();
                approval.replay(|request| pending.push(request.approval_id.clone()));
                for id in pending {
                    let _ = approval
                        .respond(&id, ApprovalDecision::Allow)
                        .now_or_never();
                }
                Box::pin(async { Ok(()) })
            })?;
            Ok(rutis::host::Effect::Done)
        })
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_response_that_finds_its_batch_stopped_ends_as_an_interrupt() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let tools = Arc::new(RiskTools::default());
        let mut builder = AgentBuilder::new(AgentConfig::default());
        builder.timeline_store(Arc::new(MemoryTimelineStore::default()));
        defaults::install(&mut builder);
        defaults::mock_model_hosts(&mut builder);
        defaults::tool_host(&mut builder, tools.clone());
        builder.require_feature(
            Feature::new("RespondOnStop").branch("RespondOnStop", |names| {
                Ok(Box::new(RespondOnStop::new(names)?))
            }),
        );
        let agent = MinimalHost::start(&builder).await.unwrap();
        let session = agent
            .create_session(config(preset(PermissionPreset::WorkspaceWrite)))
            .await
            .unwrap();
        session
            .submit("tool:exec {}".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        let approval = session.service::<Approval>("main").unwrap();
        loop {
            let mut pending = false;
            approval.replay(|_| pending = true);
            if pending {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        session.stop("main", "user stop".into()).await.unwrap();
        session.idle("main").await.unwrap();
        let events = session.local("main", ..).await.unwrap();
        let outcome = events
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                EventKind::TurnEnd(data) => Some(data.outcome.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            outcome,
            TurnOutcome::Cancelled {
                reason: "user stop".into()
            }
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event.kind, EventKind::ApprovalRespond(_)))
        );
        assert_eq!(tools.calls.load(Ordering::SeqCst), 0);
        session.close().await.unwrap();
        agent.close().await.unwrap();
    })
    .await
    .expect("stopped approval settled");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn allow_rules_answer_approval_only_for_simple_commands_within_the_preset() {
    use CommandMatch::Prefix;
    use CommandRuleDecision::Allow;
    tokio::time::timeout(Duration::from_secs(10), async {
        let tools = Arc::new(RiskTools::default());
        let rules = TestRules::new(vec![rule("cargo test", Prefix, Allow)]);
        let agent = ruled_agent(tools.clone(), rules).await;
        let session = agent
            .create_session(config(preset(PermissionPreset::WorkspaceWrite)))
            .await
            .unwrap();
        assert_eq!(text(&exec(&session, "cargo test -p x").await), "exec ran");
        assert_eq!(approval_requests(&session).await, 0);
        let approval = session.service::<Approval>("main").unwrap();
        for command in ["cargo test; rm x", "cargo test > out", "cargo testx"] {
            submit_exec(&session, command).await;
            let id = approval_request(&session, command).await;
            approval.respond(&id, ApprovalDecision::Deny).await.unwrap();
            session.idle("main").await.unwrap();
        }
        assert_eq!(approval_requests(&session).await, 3);
        session.close().await.unwrap();

        let session = agent
            .create_session(config(preset(PermissionPreset::ReadOnly)))
            .await
            .unwrap();
        assert_eq!(
            text(&exec(&session, "cargo test").await),
            "Tool `exec` is not allowed by the read-only preset (starting processes)"
        );
        assert_eq!(tools.calls.load(Ordering::SeqCst), 1);
        session.close().await.unwrap();
        for approval in [ToolApproval::Ask, ToolApproval::Bypass] {
            let session = agent
                .create_session(config(PermissionPolicy {
                    preset: PermissionPreset::ReadOnly,
                    tool_approval: Some(approval),
                    ..Default::default()
                }))
                .await
                .unwrap();
            assert_eq!(text(&exec(&session, "cargo test -p x").await), "exec ran");
            submit_exec(&session, "cargo test; rm x").await;
            if approval == ToolApproval::Ask {
                let id = approval_request(&session, "cargo test; rm x").await;
                session
                    .service::<Approval>("main")
                    .unwrap()
                    .respond(&id, ApprovalDecision::Deny)
                    .await
                    .unwrap();
            }
            session.idle("main").await.unwrap();
            assert_eq!(
                approval_requests(&session).await,
                usize::from(approval == ToolApproval::Ask)
            );
            session.close().await.unwrap();
        }
        agent.close().await.unwrap();
    })
    .await
    .expect("allow rule flow completed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deny_rules_match_any_segment_under_every_preset_and_are_read_at_use_time() {
    use CommandMatch::Prefix;
    use CommandRuleDecision::{Allow, Deny};
    tokio::time::timeout(Duration::from_secs(10), async {
        let tools = Arc::new(RiskTools::default());
        let rules = TestRules::new(vec![]);
        let agent = ruled_agent(tools.clone(), rules.clone()).await;
        let session = agent
            .create_session(config(preset(PermissionPreset::FullAccess)))
            .await
            .unwrap();
        assert_eq!(text(&exec(&session, "git push origin").await), "exec ran");
        rules.set(Some(vec![
            rule("git push", Prefix, Deny),
            rule("git push", Prefix, Allow),
        ]));
        for command in ["git push origin", "echo ok && git push", "x=$(git push)"] {
            let result = exec(&session, command).await;
            assert_eq!(result.is_error, Some(true), "{command}");
            assert_eq!(
                text(&result),
                "Command denied by command rule `git push` (prefix)"
            );
        }
        assert_eq!(text(&exec(&session, "git status").await), "exec ran");
        assert_eq!(tools.calls.load(Ordering::SeqCst), 2);
        session.close().await.unwrap();
        for approval in [ToolApproval::Ask, ToolApproval::Bypass] {
            let session = agent
                .create_session(config(PermissionPolicy {
                    preset: PermissionPreset::FullAccess,
                    tool_approval: Some(approval),
                    allowed_tools: vec!["exec".into()],
                    ..Default::default()
                }))
                .await
                .unwrap();
            assert_eq!(
                exec(&session, "echo ok && git push").await.is_error,
                Some(true)
            );
            assert_eq!(approval_requests(&session).await, 0);
            session.close().await.unwrap();
        }
        assert_eq!(tools.calls.load(Ordering::SeqCst), 2);
        agent.close().await.unwrap();
    })
    .await
    .expect("deny rule flow completed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_deny_rule_written_while_approval_is_pending_cancels_the_allowed_call() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let tools = Arc::new(RiskTools::default());
        let rules = TestRules::new(vec![]);
        let agent = ruled_agent(tools.clone(), rules.clone()).await;
        let session = agent
            .create_session(config(preset(PermissionPreset::WorkspaceWrite)))
            .await
            .unwrap();
        submit_exec(&session, "git push").await;
        let id = approval_request(&session, "git push").await;
        rules.set(Some(vec![rule(
            "git push",
            CommandMatch::Exact,
            CommandRuleDecision::Deny,
        )]));
        let approval = session.service::<Approval>("main").unwrap();
        approval
            .respond(&id, ApprovalDecision::Allow)
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        let reason = "Command denied by command rule `git push` (exact)";
        assert_eq!(text(&last_result(&session, "main").await), reason);
        assert!(
            session
                .local("main", ..)
                .await
                .unwrap()
                .iter()
                .any(|event| matches!(
                    &event.kind,
                    EventKind::ApprovalCancelled(data)
                        if data.approval_id == id && data.reason == reason
                ))
        );
        assert_eq!(tools.calls.load(Ordering::SeqCst), 0);
        session.close().await.unwrap();
        agent.close().await.unwrap();
    })
    .await
    .expect("recheck flow completed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unreadable_rules_deny_command_calls_only() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let tools = Arc::new(RiskTools::default());
        let rules = TestRules::new(vec![]);
        rules.set(None);
        let agent = ruled_agent(tools.clone(), rules).await;
        let session = agent
            .create_session(config(preset(PermissionPreset::FullAccess)))
            .await
            .unwrap();
        assert_eq!(
            text(&exec(&session, "ls").await),
            "Command rules could not be read: invalid rules file"
        );
        assert!(!call(&session, "read").await);
        assert_eq!(tools.calls.load(Ordering::SeqCst), 1);
        session.close().await.unwrap();
        agent.close().await.unwrap();
    })
    .await
    .expect("unreadable rules flow completed");
}

/// No rules; while `hold` is set a read waits for `release`.
#[derive(Default)]
struct HeldRules {
    hold: std::sync::atomic::AtomicBool,
    entered: Notify,
    release: Notify,
}
impl ReadCommandRules for HeldRules {
    fn rules(&self, _: CancellationToken) -> HostFuture<'_, Vec<CommandRule>> {
        Box::pin(async move {
            if self.hold.load(Ordering::SeqCst) {
                self.entered.notify_one();
                self.release.notified().await;
            }
            Ok(Vec::new())
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_policy_tightened_during_the_rules_read_decides_the_call() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let tools = Arc::new(RiskTools::default());
        let rules = Arc::new(HeldRules::default());
        let agent = ruled_agent(tools.clone(), rules.clone()).await;

        let session = agent
            .create_session(config(preset(PermissionPreset::FullAccess)))
            .await
            .unwrap();
        rules.hold.store(true, Ordering::SeqCst);
        submit_exec(&session, "ls").await;
        rules.entered.notified().await;
        let permission = session.service::<Permission>("main").unwrap();
        permission
            .update(0, preset(PermissionPreset::ReadOnly))
            .await
            .unwrap();
        rules.release.notify_one();
        session.idle("main").await.unwrap();
        assert_eq!(
            text(&last_result(&session, "main").await),
            "Tool `exec` is not allowed by the read-only preset (starting processes)"
        );
        session.close().await.unwrap();

        rules.hold.store(false, Ordering::SeqCst);
        let session = agent
            .create_session(config(preset(PermissionPreset::WorkspaceWrite)))
            .await
            .unwrap();
        submit_exec(&session, "ls").await;
        let id = approval_request(&session, "ls").await;
        rules.hold.store(true, Ordering::SeqCst);
        let approval = session.service::<Approval>("main").unwrap();
        approval
            .respond(&id, ApprovalDecision::Allow)
            .await
            .unwrap();
        rules.entered.notified().await;
        let permission = session.service::<Permission>("main").unwrap();
        permission
            .update(
                0,
                PermissionPolicy {
                    preset: PermissionPreset::WorkspaceWrite,
                    allowed_tools: vec![],
                    disallowed_tools: vec!["exec".into()],
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        rules.release.notify_one();
        session.idle("main").await.unwrap();
        assert_eq!(
            text(&last_result(&session, "main").await),
            "Tool `exec` is disallowed by the permission policy"
        );
        assert_eq!(tools.calls.load(Ordering::SeqCst), 0);
        session.close().await.unwrap();
        agent.close().await.unwrap();
    })
    .await
    .expect("policy race flow completed");
}

/// What one advisor a test registers answers (Hooks A.6.1).
enum Advice {
    Allow,
    Ask(&'static str),
    Deny(&'static str),
    None,
    Error,
    /// Waits on the token it is handed, then records that it saw it
    /// cancelled.
    Hold,
    /// Never answers and never looks at its token, so only Permission
    /// dropping the call ends it.
    Hang,
}

/// One advisor a test part registers with the branch's Permission.
struct TestAdvisor {
    label: &'static str,
    answer: Advice,
    /// The labels of the advisors asked, in the order they were asked.
    asked: Arc<Mutex<Vec<&'static str>>>,
    /// The requests the advisors received.
    requests: Arc<Mutex<Vec<ToolAdviceRequest>>>,
    /// Released when the advisor runs, so a test can act on a pending advice.
    entered: Arc<tokio::sync::Semaphore>,
    /// Whether the advisor saw its token cancelled.
    cancelled: Arc<AtomicBool>,
}
impl AdviseTool for TestAdvisor {
    fn advise(
        &self,
        request: ToolAdviceRequest,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ToolAdviceReply> {
        self.asked.lock().unwrap().push(self.label);
        self.requests.lock().unwrap().push(request);
        self.entered.add_permits(1);
        Box::pin(async move {
            match self.answer {
                Advice::Hold => {
                    cancel.cancelled().await;
                    self.cancelled.store(true, Ordering::SeqCst);
                    Ok(ToolAdviceReply::None)
                }
                Advice::Hang => {
                    std::future::pending::<()>().await;
                    Ok(ToolAdviceReply::None)
                }
                Advice::Allow => Ok(ToolAdviceReply::Allow),
                Advice::Ask(reason) => Ok(ToolAdviceReply::Ask {
                    reason: reason.into(),
                }),
                Advice::Deny(reason) => Ok(ToolAdviceReply::Deny {
                    reason: reason.into(),
                }),
                Advice::None => Ok(ToolAdviceReply::None),
                Advice::Error => Err(HostError::failed("the advisor failed")),
            }
        })
    }
}

/// The advisors a test part registers, and what a test observes of them.
#[derive(Default)]
struct Advised {
    /// Whether each registration succeeded, in entry order.
    registered: Arc<Mutex<Vec<bool>>>,
    /// The mounted part's view, which a test disposes to unload it.
    views: Arc<Mutex<Vec<rutis::host::FiberView>>>,
}
impl Advised {
    /// Disposes the mounted part, withdrawing its registrations.
    async fn unload(&self) {
        let view = self
            .views
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("an advisor part");
        view.dispose().await.unwrap();
    }
}

/// Registers the entries on apply, as a branch part does.
struct AdvicePart {
    permission: Key<Permission>,
    entries: Vec<(String, Arc<dyn AdviseTool>)>,
    registered: Arc<Mutex<Vec<bool>>>,
}
impl DynPlugin for AdvicePart {
    fn name(&self) -> &str {
        "TestAdvice"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.permission.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a rutis::Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let permission = ctx.require_at(&self.permission)?;
            for (name, advise) in &self.entries {
                self.registered.lock().unwrap().push(
                    permission
                        .register_advice(ctx, name, advise.clone())
                        .is_ok(),
                );
            }
            Ok(Effect::Done)
        })
    }
}

/// Mounts [`AdvicePart`] as a child of a branch part, so a test can unload it
/// on its own.
struct AdviceOwner {
    mounted: Mutex<Option<AdvicePart>>,
    injects: Vec<TypeKey>,
    views: Arc<Mutex<Vec<rutis::host::FiberView>>>,
}
impl DynPlugin for AdviceOwner {
    fn name(&self) -> &str {
        "TestAdviceOwner"
    }
    fn injects(&self) -> Vec<TypeKey> {
        self.injects.clone()
    }
    fn apply<'a>(&'a self, ctx: &'a rutis::Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let part = self
                .mounted
                .lock()
                .unwrap()
                .take()
                .expect("the owner mounts its part once");
            let view = dim_agent_runtime::mount_active(ctx, part).await?;
            self.views.lock().unwrap().push(view);
            Ok(Effect::Done)
        })
    }
}

/// The default profile with `advisors` registered with each branch's
/// Permission, and `rules` bound in place of its empty host.
async fn advised_agent(
    tools: Arc<RiskTools>,
    rules: Arc<dyn ReadCommandRules>,
    advisors: Vec<(&'static str, Arc<dyn AdviseTool>)>,
) -> (MinimalHost, Advised) {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder
        .timeline_store(Arc::new(MemoryTimelineStore::default()))
        .session_value(Arc::new(PromptHost(Arc::new(EmptyPromptHost))))
        .session_value(Arc::new(InitialPromptConfiguration::fixed(None)))
        .session_value(Arc::new(CommandRulesHost(rules)));
    defaults::install_with_prompt_capability(&mut builder, ModelProviderPlugin::new);
    defaults::mock_model_hosts(&mut builder);
    defaults::tool_host(&mut builder, tools);
    let mounted = Advised::default();
    let entries: Vec<(String, Arc<dyn AdviseTool>)> = advisors
        .into_iter()
        .map(|(name, advise)| (name.to_owned(), advise))
        .collect();
    let registered = mounted.registered.clone();
    let views = mounted.views.clone();
    builder.require_feature(
        Feature::new("TestAdvice").branch("TestAdvice", move |names| {
            Ok(Box::new(AdviceOwner {
                injects: vec![names.key::<Permission>()?.erase()],
                mounted: Mutex::new(Some(AdvicePart {
                    permission: names.key()?,
                    entries: entries.clone(),
                    registered: registered.clone(),
                })),
                views: views.clone(),
            }))
        }),
    );
    let agent = MinimalHost::start(&builder).await.unwrap();
    (agent, mounted)
}

/// Admits a call the test answers no question for; an admission that does not
/// settle fails the test instead of hanging it.
async fn admit(
    approval: &Approval,
    call: AdmitCall,
    risk: Option<&str>,
    cancel: CancellationToken,
) -> std::result::Result<(), HostError> {
    tokio::time::timeout(Duration::from_secs(10), approval.admit(call, risk, cancel))
        .await
        .expect("the admission did not settle")
}

/// Waits for the approval request of a call with `command`; a request that
/// never comes fails the test instead of hanging it.
async fn asked_approval(session: &Session, command: &str) -> String {
    tokio::time::timeout(Duration::from_secs(10), approval_request(session, command))
        .await
        .expect("the call asked for no approval")
}

/// One advisor with its observations and its entry into the test. `asked`
/// is shared by the advisors of one test, so it holds their order.
fn advisor(
    label: &'static str,
    answer: Advice,
    asked: Arc<Mutex<Vec<&'static str>>>,
) -> (Arc<dyn AdviseTool>, AdvisorHandles) {
    let handles = AdvisorHandles::default();
    let advisor = TestAdvisor {
        label,
        answer,
        asked,
        requests: handles.requests.clone(),
        entered: handles.entered.clone(),
        cancelled: handles.cancelled.clone(),
    };
    (Arc::new(advisor), handles)
}

#[derive(Clone)]
struct AdvisorHandles {
    requests: Arc<Mutex<Vec<ToolAdviceRequest>>>,
    entered: Arc<tokio::sync::Semaphore>,
    cancelled: Arc<AtomicBool>,
}
/// Waits for the advisor to run; a missing call fails the test instead of
/// hanging it.
async fn entered(handles: &AdvisorHandles) {
    tokio::time::timeout(Duration::from_secs(10), handles.entered.acquire())
        .await
        .expect("the advisor did not run")
        .unwrap()
        .forget();
}
impl Default for AdvisorHandles {
    fn default() -> Self {
        Self {
            requests: Arc::default(),
            entered: Arc::new(tokio::sync::Semaphore::new(0)),
            cancelled: Arc::default(),
        }
    }
}

/// An admission of `exec` with `command`, as ToolExecutor asks for one.
fn exec_call(command: &str) -> AdmitCall {
    let mut input = JsonObject::new();
    input.insert("command".into(), command.to_owned().into());
    AdmitCall {
        source: BranchSource::Tool {
            turn_id: "turn".into(),
            request_id: "request".into(),
            call_id: "call".into(),
        },
        call: ApprovalCallData {
            name: "exec".into(),
            input,
        },
    }
}

/// A.6.1: the advisors are asked in name order, after Permission's own
/// decision, and any ask turns its allow into an ask carrying the first
/// ask's reason. The request carries the call, its source, its risk and
/// Permission's own decision.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn advisors_are_asked_in_name_order_and_an_ask_refines_an_allow() {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let (alpha, alpha_handles) = advisor("alpha", Advice::Ask("alpha asks"), asked.clone());
    let (mid, _) = advisor("mid", Advice::None, asked.clone());
    let (zeta, _) = advisor("zeta", Advice::Allow, asked.clone());
    let (agent, _mounted) = advised_agent(
        Arc::new(RiskTools::default()),
        Arc::new(NoCommandRules),
        vec![("zeta", zeta), ("alpha", alpha), ("mid", mid)],
    )
    .await;
    let session = agent
        .create_session(config(preset(PermissionPreset::FullAccess)))
        .await
        .unwrap();
    let approval = session.service::<Approval>("main").unwrap();
    let admitted = tokio::spawn({
        let approval = approval.clone();
        async move {
            approval
                .admit(
                    exec_call("ls"),
                    Some("execute_process"),
                    CancellationToken::new(),
                )
                .await
        }
    });
    let request = asked_approval(&session, "ls").await;
    let reason = session
        .local("main", ..)
        .await
        .unwrap()
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ApprovalRequest(data) if data.approval_id == request => {
                Some(data.reason.clone())
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(reason, "alpha asks", "the first ask's reason is used");
    approval
        .respond(&request, ApprovalDecision::Allow)
        .await
        .unwrap();
    admitted.await.unwrap().unwrap();
    assert_eq!(*asked.lock().unwrap(), ["alpha", "mid", "zeta"]);
    {
        let requests = alpha_handles.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(request.call.name, "exec");
        assert_eq!(
            request.call.input.get("command").and_then(|v| v.as_str()),
            Some("ls")
        );
        assert_eq!(request.risk.as_deref(), Some("execute_process"));
        assert_eq!(request.decision, AdmissionDecision::Allow);
        assert!(matches!(
            &request.source,
            BranchSource::Tool { call_id, .. } if call_id == "call"
        ));
    }
    session.close().await.unwrap();
    agent.close().await.unwrap();
}

/// A.6.1: an allow advice admits a call the preset would have asked about,
/// with no `approval/request`; Permission's own decision is the ask the
/// advisor sees.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_allow_advice_admits_a_call_the_preset_would_ask_about() {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let (hooks, handles) = advisor("hooks", Advice::Allow, asked.clone());
    let (agent, _mounted) = advised_agent(
        Arc::new(RiskTools::default()),
        Arc::new(NoCommandRules),
        vec![("hooks", hooks)],
    )
    .await;
    let session = agent
        .create_session(config(preset(PermissionPreset::WorkspaceWrite)))
        .await
        .unwrap();
    let approval = session.service::<Approval>("main").unwrap();
    admit(
        &approval,
        exec_call("ls"),
        Some("execute_process"),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        approval_requests(&session).await,
        0,
        "an allowed call asked for approval"
    );
    assert_eq!(*asked.lock().unwrap(), ["hooks"]);
    assert_eq!(
        handles.requests.lock().unwrap()[0].decision,
        AdmissionDecision::Ask
    );
    session.close().await.unwrap();
    agent.close().await.unwrap();
}

/// A.6.1: any deny wins over an allow and an ask, and the first deny in name
/// order is the reason.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_deny_advice_wins_over_an_allow_and_an_ask() {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let (alpha, _) = advisor("alpha", Advice::Deny("alpha denies"), asked.clone());
    let (mid, _) = advisor("mid", Advice::Allow, asked.clone());
    let (zeta, _) = advisor("zeta", Advice::Deny("zeta denies"), asked.clone());
    let (agent, _mounted) = advised_agent(
        Arc::new(RiskTools::default()),
        Arc::new(NoCommandRules),
        vec![("zeta", zeta), ("mid", mid), ("alpha", alpha)],
    )
    .await;
    let session = agent
        .create_session(config(preset(PermissionPreset::FullAccess)))
        .await
        .unwrap();
    let approval = session.service::<Approval>("main").unwrap();
    let error = admit(
        &approval,
        exec_call("ls"),
        Some("execute_process"),
        CancellationToken::new(),
    )
    .await
    .expect_err("a deny advice denies the call");
    assert_eq!(error.to_string(), "alpha denies");
    assert_eq!(approval_requests(&session).await, 0);
    session.close().await.unwrap();
    agent.close().await.unwrap();
}

/// A.6.1: a second advisor of the same name fails; the first one stands.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_registration_of_the_same_name_fails() {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let (first, _) = advisor("first", Advice::Allow, asked.clone());
    let (second, _) = advisor("second", Advice::Allow, asked.clone());
    let (agent, mounted) = advised_agent(
        Arc::new(RiskTools::default()),
        Arc::new(NoCommandRules),
        vec![("hooks", first), ("hooks", second)],
    )
    .await;
    let session = agent
        .create_session(config(preset(PermissionPreset::FullAccess)))
        .await
        .unwrap();
    let _ = session.service::<Permission>("main").unwrap();
    assert_eq!(*mounted.registered.lock().unwrap(), [true, false]);
    session.close().await.unwrap();
    agent.close().await.unwrap();
}

/// A.6.1: a registration belongs to its part and is withdrawn when it
/// unloads; without an advisor the preset decides alone again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_registration_is_withdrawn_when_its_part_unloads() {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let (hooks, _handles) = advisor("hooks", Advice::Allow, asked.clone());
    let (agent, mounted) = advised_agent(
        Arc::new(RiskTools::default()),
        Arc::new(NoCommandRules),
        vec![("hooks", hooks)],
    )
    .await;
    let session = agent
        .create_session(config(preset(PermissionPreset::WorkspaceWrite)))
        .await
        .unwrap();
    let approval = session.service::<Approval>("main").unwrap();
    admit(
        &approval,
        exec_call("ls"),
        Some("execute_process"),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(approval_requests(&session).await, 0);
    mounted.unload().await;
    let admitted = tokio::spawn({
        let approval = approval.clone();
        async move {
            approval
                .admit(
                    exec_call("pwd"),
                    Some("execute_process"),
                    CancellationToken::new(),
                )
                .await
        }
    });
    let request = asked_approval(&session, "pwd").await;
    approval
        .respond(&request, ApprovalDecision::Deny)
        .await
        .unwrap();
    assert!(admitted.await.unwrap().is_err());
    assert_eq!(
        *asked.lock().unwrap(),
        ["hooks"],
        "the withdrawn advisor was asked again"
    );
    session.close().await.unwrap();
    agent.close().await.unwrap();
}

/// A.6.1: advice is not asked when the policy already denies, whether the
/// tool is disallowed or the preset refuses it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn advisors_are_not_asked_when_the_policy_denies() {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let (hooks, _handles) = advisor("hooks", Advice::Allow, asked.clone());
    let (agent, _mounted) = advised_agent(
        Arc::new(RiskTools::default()),
        Arc::new(NoCommandRules),
        vec![("hooks", hooks)],
    )
    .await;
    for policy in [
        PermissionPolicy {
            preset: PermissionPreset::FullAccess,
            disallowed_tools: vec!["exec".into()],
            ..Default::default()
        },
        preset(PermissionPreset::ReadOnly),
    ] {
        let session = agent.create_session(config(policy.clone())).await.unwrap();
        let approval = session.service::<Approval>("main").unwrap();
        let error = approval
            .admit(
                exec_call("ls"),
                Some("execute_process"),
                CancellationToken::new(),
            )
            .await
            .expect_err("the policy denies the call");
        assert!(!error.to_string().is_empty());
        assert_eq!(approval_requests(&session).await, 0);
        session.close().await.unwrap();
    }
    assert!(
        asked.lock().unwrap().is_empty(),
        "a denied call asked an advisor"
    );
    agent.close().await.unwrap();
}

/// A.6.1: a command-rule deny is final too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn advisors_are_not_asked_when_a_command_rule_denies() {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let (hooks, _handles) = advisor("hooks", Advice::Allow, asked.clone());
    let rules = TestRules::new(vec![rule(
        "rm -rf",
        CommandMatch::Exact,
        CommandRuleDecision::Deny,
    )]);
    let (agent, _mounted) = advised_agent(
        Arc::new(RiskTools::default()),
        rules,
        vec![("hooks", hooks)],
    )
    .await;
    let session = agent
        .create_session(config(preset(PermissionPreset::WorkspaceWrite)))
        .await
        .unwrap();
    let approval = session.service::<Approval>("main").unwrap();
    let error = approval
        .admit(
            exec_call("rm -rf"),
            Some("execute_process"),
            CancellationToken::new(),
        )
        .await
        .expect_err("the command rule denies the call");
    assert_eq!(
        error.to_string(),
        "Command denied by command rule `rm -rf` (exact)"
    );
    assert!(
        asked.lock().unwrap().is_empty(),
        "a denied call asked an advisor"
    );
    session.close().await.unwrap();
    agent.close().await.unwrap();
}

/// A.6.1: Approval's recheck after an answer is a plain decision and asks no
/// advisor again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn advisors_are_not_asked_on_the_recheck_after_an_answer() {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let (hooks, _handles) = advisor("hooks", Advice::None, asked.clone());
    let (agent, _mounted) = advised_agent(
        Arc::new(RiskTools::default()),
        Arc::new(NoCommandRules),
        vec![("hooks", hooks)],
    )
    .await;
    let session = agent
        .create_session(config(preset(PermissionPreset::WorkspaceWrite)))
        .await
        .unwrap();
    let approval = session.service::<Approval>("main").unwrap();
    let admitted = tokio::spawn({
        let approval = approval.clone();
        async move {
            approval
                .admit(
                    exec_call("ls"),
                    Some("execute_process"),
                    CancellationToken::new(),
                )
                .await
        }
    });
    let request = asked_approval(&session, "ls").await;
    approval
        .respond(&request, ApprovalDecision::Allow)
        .await
        .unwrap();
    admitted.await.unwrap().unwrap();
    assert_eq!(
        *asked.lock().unwrap(),
        ["hooks"],
        "the recheck asked the advisor again"
    );
    session.close().await.unwrap();
    agent.close().await.unwrap();
}

/// A.6.1: a provider error counts as none: it neither denies nor stops the
/// other advisors.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_advisor_error_counts_as_none() {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let (allow, _) = advisor("allow", Advice::Allow, asked.clone());
    let (broken, _) = advisor("broken", Advice::Error, asked.clone());
    let (agent, _mounted) = advised_agent(
        Arc::new(RiskTools::default()),
        Arc::new(NoCommandRules),
        vec![("broken", broken), ("allow", allow)],
    )
    .await;
    let session = agent
        .create_session(config(preset(PermissionPreset::WorkspaceWrite)))
        .await
        .unwrap();
    let approval = session.service::<Approval>("main").unwrap();
    admit(
        &approval,
        exec_call("ls"),
        Some("execute_process"),
        CancellationToken::new(),
    )
    .await
    .expect("the allow advice admits the call");
    assert_eq!(approval_requests(&session).await, 0);
    session.close().await.unwrap();
    agent.close().await.unwrap();
}

/// A.6.1: cancelling the batch drops the admission as cancelled, and the
/// advisor sees the token it is handed cancelled.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_the_batch_fails_the_admission_and_the_advisor_sees_it() {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let (hooks, handles) = advisor("hooks", Advice::Hold, asked.clone());
    let (agent, _mounted) = advised_agent(
        Arc::new(RiskTools::default()),
        Arc::new(NoCommandRules),
        vec![("hooks", hooks)],
    )
    .await;
    let session = agent
        .create_session(config(preset(PermissionPreset::FullAccess)))
        .await
        .unwrap();
    let approval = session.service::<Approval>("main").unwrap();
    let cancel = CancellationToken::new();
    let admitted = tokio::spawn({
        let approval = approval.clone();
        let cancel = cancel.clone();
        async move {
            approval
                .admit(exec_call("ls"), Some("execute_process"), cancel)
                .await
        }
    });
    entered(&handles).await;
    cancel.cancel();
    let error = admitted
        .await
        .unwrap()
        .expect_err("the admission was cancelled");
    assert!(
        matches!(error, HostError::Cancelled(_)),
        "the admission did not fail as cancelled: {error}"
    );
    assert!(
        handles.cancelled.load(Ordering::SeqCst),
        "the advisor did not see its token cancelled"
    );
    session.close().await.unwrap();
    agent.close().await.unwrap();
}

/// A.6.1: cancelling the batch makes Permission drop an advice that never
/// answers and never looks at its token, so the admission fails as cancelled
/// in a bound instead of waiting for the advisor, and no approval is asked.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_the_batch_drops_an_advice_that_never_answers() {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let (hooks, handles) = advisor("hooks", Advice::Hang, asked.clone());
    let (agent, _mounted) = advised_agent(
        Arc::new(RiskTools::default()),
        Arc::new(NoCommandRules),
        vec![("hooks", hooks)],
    )
    .await;
    let session = agent
        .create_session(config(preset(PermissionPreset::FullAccess)))
        .await
        .unwrap();
    let approval = session.service::<Approval>("main").unwrap();
    let cancel = CancellationToken::new();
    let admitted = tokio::spawn({
        let approval = approval.clone();
        let cancel = cancel.clone();
        async move {
            approval
                .admit(exec_call("ls"), Some("execute_process"), cancel)
                .await
        }
    });
    entered(&handles).await;
    cancel.cancel();
    let error = tokio::time::timeout(Duration::from_secs(10), admitted)
        .await
        .expect("the admission waited for an advice that never answers")
        .unwrap()
        .expect_err("the admission was cancelled");
    assert!(
        matches!(error, HostError::Cancelled(_)),
        "the admission did not fail as cancelled: {error}"
    );
    assert_eq!(approval_requests(&session).await, 0);
    session.close().await.unwrap();
    agent.close().await.unwrap();
}

/// A.6.1: an advisor withdrawn while its advice is pending has that advice
/// dropped, counted as none, so Permission decides alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_advisor_withdrawn_while_its_advice_is_pending_counts_as_none() {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let (hooks, handles) = advisor("hooks", Advice::Hold, asked.clone());
    let (agent, mounted) = advised_agent(
        Arc::new(RiskTools::default()),
        Arc::new(NoCommandRules),
        vec![("hooks", hooks)],
    )
    .await;
    let session = agent
        .create_session(config(preset(PermissionPreset::WorkspaceWrite)))
        .await
        .unwrap();
    let approval = session.service::<Approval>("main").unwrap();
    let admitted = tokio::spawn({
        let approval = approval.clone();
        async move {
            approval
                .admit(
                    exec_call("ls"),
                    Some("execute_process"),
                    CancellationToken::new(),
                )
                .await
        }
    });
    entered(&handles).await;
    mounted.unload().await;
    let request = asked_approval(&session, "ls").await;
    approval
        .respond(&request, ApprovalDecision::Deny)
        .await
        .unwrap();
    assert!(admitted.await.unwrap().is_err());
    assert_eq!(*asked.lock().unwrap(), ["hooks"]);
    assert!(
        !handles.cancelled.load(Ordering::SeqCst),
        "the dropped advice was still running"
    );
    session.close().await.unwrap();
    agent.close().await.unwrap();
}
