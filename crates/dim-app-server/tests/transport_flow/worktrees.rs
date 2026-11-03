use super::*;
#[cfg(target_os = "macos")]
use dim_agent_capabilities::shell_session::{CallContext, ShellSession};
use dim_agent_capabilities::{
    file_access::WorktreeDirectory,
    model_provider::ModelProviderPlugin,
    permission::{Decision, Permission},
    shell_session::ShellSessionPlugin,
    workspace_tools::WorkspaceToolsConfig,
};
use dim_agent_runtime::{Session, SessionConfig};
use dim_agent_types::{ApprovalCallData, InputMode, PermissionPolicy, PermissionPreset};
use dim_app_server::storage::SessionStore;
use std::path::{Path, PathBuf};

struct Work {
    home: PathBuf,
    repo: PathBuf,
}
impl Work {
    fn new() -> Self {
        let home = PathBuf::from("/tmp").join(format!("dim-wt-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(home.join("repo/packages/a")).unwrap();
        std::fs::create_dir_all(home.join("repo/packages/b")).unwrap();
        let home = home.canonicalize().unwrap();
        let repo = home.join("repo");
        git(&repo, &["init", "-q"]);
        std::fs::write(repo.join("packages/a/seed.txt"), "seed").unwrap();
        std::fs::write(repo.join("packages/b/seed.txt"), "seed").unwrap();
        git(&repo, &["add", "."]);
        git(
            &repo,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-qm",
                "initial",
            ],
        );
        Self { home, repo }
    }
    fn tree(&self, name: &str) -> PathBuf {
        let path = self.home.join(name);
        git(
            &self.repo,
            &[
                "worktree",
                "add",
                "--detach",
                "-q",
                path.to_str().unwrap(),
                "HEAD",
            ],
        );
        path
    }
}
impl Drop for Work {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}
fn git(cwd: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .current_dir(cwd)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
async fn host(work: &Work) -> RunningServer {
    let configured = work.home.join("native/dimcode.sqlite").exists();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install_local_execution(
        &mut builder,
        ModelProviderPlugin::new,
        WorkspaceToolsConfig {
            output_root: work.home.join("outputs"),
        },
        Default::default(),
    );
    defaults::mock_model_hosts(&mut builder);
    dim_app_server::permission_rules::install(&mut builder);
    dim_app_server::session_grants::install(&mut builder);
    dim_agent_capabilities::file_access::install(&mut builder);
    builder.require_feature(
        Feature::new("ShellSession").branch("ShellSession", |names| {
            Ok(Box::new(ShellSessionPlugin::session(names)?))
        }),
    );
    let server = serve(
        builder,
        ServerConfig {
            product_home: Some(work.home.clone()),
            target_home: Some(work.home.join("native")),
            platform_home: Some(work.home.join("user")),
            file_roots: vec![work.home.clone()],
            unix_path: Some(work.home.join("native/server.sock")),
            websocket_address: None,
            session_defaults: SessionConfig {
                tools: Some(
                    ["read", "write", "edit", "glob", "grep", "exec"]
                        .map(String::from)
                        .to_vec(),
                ),
                permission_policy: PermissionPolicy {
                    preset: PermissionPreset::WorkspaceWrite,
                    allowed_tools: ["read", "write", "edit", "glob", "grep", "exec"]
                        .map(String::from)
                        .to_vec(),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await
    .unwrap();
    if !configured {
        let mut client = Client::new_version(&server, false, 3).await;
        let caps = dim_agent_types::CatalogCapabilities::with_limits(32000, 2048);
        assert_ok(&client.rpc("provider.create", json!({"providerId":"018ec73f-0000-7000-8000-000000000099","displayName":"Fixture","connection":{"driverKind":"openai-compatible","baseUrl":"https://model.example/v1","auth":{"type":"bearer"},"credential":{"action":"set","value":"fixture-access"}},"models":[{"modelId":"alpha","displayName":"alpha","status":"active","capabilities":caps}],"enabled":true})).await);
        assert_ok(
            &client
                .rpc(
                    "provider.select",
                    json!({"providerId":"018ec73f-0000-7000-8000-000000000099","modelId":"alpha"}),
                )
                .await,
        );
    }
    server
}
async fn session(server: &RunningServer, cwd: &Path) -> Session {
    let mut client = Client::new_version(server, false, 3).await;
    let created = client.rpc("session.create", json!({"cwd":cwd})).await;
    assert_ok(&created);
    server
        .agent()
        .unwrap()
        .get_session(created["result"]["sessionId"].as_str().unwrap())
        .unwrap()
}

async fn run(session: &Session, name: &str, input: Value) -> ToolResult {
    session
        .submit(format!("tool:{name} {input}"), InputMode::Queue, "main")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(20), session.idle("main"))
        .await
        .unwrap()
        .unwrap();
    session
        .history("main", None)
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
async fn verdict(session: &Session, path: &Path, write: bool) -> Decision {
    session
        .service::<Permission>("main")
        .unwrap()
        .decide(
            &ApprovalCallData {
                name: if write { "write" } else { "read" }.into(),
                input: json!({"path":path}).as_object().unwrap().clone(),
            },
            Some(if write {
                "write_filesystem"
            } else {
                "read_filesystem"
            }),
            &CancellationToken::new(),
        )
        .await
        .unwrap()
}
fn succeeded(result: ToolResult) {
    assert_ne!(result.is_error, Some(true), "{result:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_exec_worktrees_admit_files_and_share_current_branch_policy() {
    let work = Work::new();
    let server = host(&work).await;
    let session = session(&server, &work.repo).await;
    let tree = work.home.join("linked");
    succeeded(
        run(
            &session,
            "exec",
            json!({"command":format!("git worktree add --detach '{}' HEAD", tree.display())}),
        )
        .await,
    );
    succeeded(
        run(
            &session,
            "write",
            json!({"path":tree.join("new.txt"),"content":"first"}),
        )
        .await,
    );
    succeeded(
        run(
            &session,
            "write",
            json!({"path":tree.join("packages/a/other.txt"),"content":"second"}),
        )
        .await,
    );
    succeeded(run(&session, "read", json!({"path":tree.join("new.txt")})).await);
    succeeded(
        run(
            &session,
            "edit",
            json!({"path":tree.join("new.txt"),"oldText":"first","newText":"updated"}),
        )
        .await,
    );
    succeeded(run(&session, "read", json!({"path":tree.join("new.txt")})).await);
    succeeded(run(&session, "glob", json!({"path":tree,"pattern":"**/*.txt"})).await);
    succeeded(run(&session, "grep", json!({"path":tree,"pattern":"seed"})).await);
    assert_eq!(
        std::fs::read_to_string(tree.join("new.txt")).unwrap(),
        "updated"
    );
    let store = server.service::<SessionStore>().unwrap();
    let directory = WorktreeDirectory {
        repository: work.repo.join(".git"),
        root: tree.clone(),
    };
    assert_eq!(
        store.worktree_directories().await.unwrap(),
        std::slice::from_ref(&directory)
    );
    assert_eq!(
        store
            .worktree_usages(directory.clone())
            .await
            .unwrap()
            .len(),
        1
    );
    let branch = session
        .branch(
            "main".into(),
            0,
            dim_agent_runtime::BranchOptions {
                prompt: "inspect".into(),
                source: dim_agent_types::BranchSource::Host {
                    operation_id: "worktree-flow".into(),
                },
            },
        )
        .await
        .unwrap();
    session.idle(&branch).await.unwrap();
    let child = session.service::<Permission>(&branch).unwrap();
    assert_eq!(
        child
            .decide(
                &ApprovalCallData {
                    name: "write".into(),
                    input: json!({"path":tree.join("child.txt")})
                        .as_object()
                        .unwrap()
                        .clone()
                },
                Some("write_filesystem"),
                &CancellationToken::new()
            )
            .await
            .unwrap(),
        Decision::Allow
    );
    let policy = PermissionPolicy {
        preset: PermissionPreset::ReadOnly,
        ..Default::default()
    };
    child.update(0, policy.clone()).await.unwrap();
    assert!(matches!(
        child
            .decide(
                &ApprovalCallData {
                    name: "write".into(),
                    input: json!({"path":tree.join("child.txt")})
                        .as_object()
                        .unwrap()
                        .clone()
                },
                Some("write_filesystem"),
                &CancellationToken::new()
            )
            .await
            .unwrap(),
        Decision::Deny { .. }
    ));
    assert_eq!(
        verdict(&session, &tree.join("leader.txt"), true).await,
        Decision::Allow
    );
    assert_eq!(store.worktree_usages(directory).await.unwrap().len(), 1);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn worktree_subtrees_symlinks_replacements_and_unrelated_repositories_keep_the_boundary() {
    let work = Work::new();
    let tree = work.tree("linked with spaces");
    let server = host(&work).await;
    let session = session(&server, &work.repo.join("packages/a")).await;
    assert_eq!(
        verdict(&session, &tree.join("packages/a/new/deep/file.txt"), true).await,
        Decision::Allow
    );
    assert!(matches!(
        verdict(&session, &tree.join("packages/b/file.txt"), true).await,
        Decision::Ask { .. }
    ));
    let link = tree.join("packages/a/escape");
    std::os::unix::fs::symlink(tree.join("packages/b"), &link).unwrap();
    assert!(matches!(
        verdict(&session, &link.join("file.txt"), true).await,
        Decision::Ask { .. }
    ));
    std::fs::rename(tree.join("packages/a"), tree.join("packages/saved")).unwrap();
    std::os::unix::fs::symlink(tree.join("packages/saved"), tree.join("packages/a")).unwrap();
    assert!(matches!(
        verdict(&session, &tree.join("packages/a/file.txt"), true).await,
        Decision::Ask { .. }
    ));
    std::fs::remove_file(tree.join("packages/a")).unwrap();
    std::fs::rename(tree.join("packages/saved"), tree.join("packages/a")).unwrap();
    assert_eq!(
        verdict(&session, &tree.join("packages/a/restored.txt"), true).await,
        Decision::Allow
    );
    let clone = work.home.join("clone");
    git(
        &work.home,
        &[
            "clone",
            "-q",
            work.repo.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    assert!(matches!(
        verdict(&session, &clone.join("packages/a/file.txt"), true).await,
        Decision::Ask { .. }
    ));
    let spoof = work.home.join("spoof");
    std::fs::create_dir_all(&spoof).unwrap();
    std::fs::write(
        spoof.join(".git"),
        format!("gitdir: {}\n", work.repo.join(".git").display()),
    )
    .unwrap();
    assert!(matches!(
        verdict(&session, &spoof.join("packages/a/file.txt"), true).await,
        Decision::Ask { .. }
    ));
    let alias = work.home.join("gitfile-alias");
    std::fs::create_dir_all(&alias).unwrap();
    std::os::unix::fs::symlink(tree.join(".git"), alias.join(".git")).unwrap();
    assert!(matches!(
        verdict(&session, &alias.join("packages/a/file.txt"), true).await,
        Decision::Ask { .. }
    ));
    let later = work.home.join("later");
    std::fs::create_dir_all(&later).unwrap();
    assert!(matches!(
        verdict(&session, &later.join("packages/a/file.txt"), true).await,
        Decision::Ask { .. }
    ));
    std::fs::remove_dir(&later).unwrap();
    git(
        &work.repo,
        &[
            "worktree",
            "add",
            "--detach",
            "-q",
            later.to_str().unwrap(),
            "HEAD",
        ],
    );
    assert_eq!(
        verdict(&session, &later.join("packages/a/file.txt"), true).await,
        Decision::Allow
    );
    std::fs::rename(&tree, work.home.join("moved")).unwrap();
    std::fs::create_dir_all(&tree).unwrap();
    git(&tree, &["init", "-q"]);
    assert!(matches!(
        verdict(&session, &tree.join("packages/a/file.txt"), true).await,
        Decision::Ask { .. }
    ));
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_association_keeps_approval_and_restore_reverifies_git_metadata() {
    let work = Work::new();
    let tree = work.tree("linked");
    let server = host(&work).await;
    let session = session(&server, &work.repo).await;
    let id = session.id().to_owned();
    let store = server.service::<SessionStore>().unwrap();
    let connection = rusqlite::Connection::open(work.home.join("native/dimcode.sqlite")).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_worktree BEFORE INSERT ON session_worktrees BEGIN SELECT RAISE(ABORT, 'association unavailable'); END;").unwrap();
    let denied = verdict(&session, &tree.join("new.txt"), true).await;
    assert!(
        matches!(denied, Decision::Ask { reason } if reason.contains("association unavailable"))
    );
    assert!(store.worktree_directories().await.unwrap().is_empty());
    connection
        .execute_batch("DROP TRIGGER reject_worktree")
        .unwrap();
    drop(connection);
    assert_eq!(
        verdict(&session, &tree.join("new.txt"), true).await,
        Decision::Allow
    );
    let directory = WorktreeDirectory {
        repository: work.repo.join(".git"),
        root: tree.clone(),
    };
    assert_eq!(
        store
            .worktree_usages(directory.clone())
            .await
            .unwrap()
            .len(),
        1
    );
    let mut client = Client::new_version(&server, false, 3).await;
    let fork = client
        .rpc("session.fork", json!({"sessionId":id,"at":0}))
        .await;
    assert_ok(&fork);
    assert_eq!(
        store
            .worktree_usages(directory.clone())
            .await
            .unwrap()
            .len(),
        1
    );
    let administrative = std::fs::read_to_string(tree.join(".git")).unwrap();
    let administrative = PathBuf::from(administrative.trim().strip_prefix("gitdir: ").unwrap());
    let backlink = administrative.join("gitdir");
    let original = std::fs::read(&backlink).unwrap();
    std::fs::write(&backlink, format!("{}\n", work.repo.join(".git").display())).unwrap();
    assert_eq!(
        verdict(&session, &tree.join("cached.txt"), true).await,
        Decision::Allow
    );
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    let server = host(&work).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_ok(&client.rpc("session.attach", json!({"sessionId":id})).await);
    let restored = server.agent().unwrap().get_session(&id).unwrap();
    assert!(matches!(
        verdict(&restored, &tree.join("restored.txt"), true).await,
        Decision::Ask { .. }
    ));
    assert_eq!(
        server
            .service::<SessionStore>()
            .unwrap()
            .worktree_usages(directory)
            .await
            .unwrap()
            .len(),
        1
    );
    std::fs::write(backlink, original).unwrap();
    assert_eq!(
        verdict(&restored, &tree.join("rechecked.txt"), true).await,
        Decision::Allow
    );
    server.shutdown();
    server.join().await.unwrap();
}

#[cfg(target_os = "macos")]
async fn shell(session: &Session, command: String) -> ToolResult {
    let identity = uuid::Uuid::new_v4().to_string();
    session
        .service::<ShellSession>("main")
        .unwrap()
        .run(
            command,
            CallContext {
                turn_id: identity.clone(),
                request_id: identity.clone(),
                call_id: identity,
            },
            10_000,
            false,
            CancellationToken::new(),
        )
        .await
        .unwrap()
}

#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn new_processes_use_confirmed_roots_while_existing_shells_keep_their_boundary() {
    let work = Work::new();
    let tree = work.tree("linked");
    let unknown = work.tree("unknown");
    let server = host(&work).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let settings = client
        .rpc(
            "settings.read",
            json!({"source":"execution","scope":"user"}),
        )
        .await;
    assert_ok(&settings);
    assert_ok(&client.rpc("settings.update", json!({"source":"execution","scope":"user","expectedRevision":settings["result"]["revision"],"document":{"sandbox":true,"network":false}})).await);
    let first = session(&server, &work.repo).await;
    succeeded(shell(&first, "printf initialized".into()).await);
    let result = run(&first, "exec", json!({"cwd":tree,"command":format!("printf allowed > allowed.txt; printf blocked > '{}/blocked.txt'", unknown.display())})).await;
    assert_eq!(result.is_error, Some(true), "{result:?}");
    assert_eq!(
        std::fs::read_to_string(tree.join("allowed.txt")).unwrap(),
        "allowed"
    );
    assert!(!unknown.join("blocked.txt").exists());
    let result = shell(
        &first,
        format!("printf blocked > '{}/old-shell.txt'", tree.display()),
    )
    .await;
    assert_eq!(result.is_error, Some(true), "{result:?}");
    assert!(!tree.join("old-shell.txt").exists());
    let second = session(&server, &work.repo).await;
    assert_eq!(
        verdict(&second, &tree.join("new-shell.txt"), true).await,
        Decision::Allow
    );
    succeeded(
        shell(
            &second,
            format!("printf allowed > '{}/new-shell.txt'", tree.display()),
        )
        .await,
    );
    assert_eq!(
        std::fs::read_to_string(tree.join("new-shell.txt")).unwrap(),
        "allowed"
    );
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_git_metadata_cannot_block_discovery_or_publish_a_root() {
    let work = Work::new();
    let tree = work.tree("linked");
    let server = host(&work).await;
    let session = session(&server, &work.repo).await;
    let administrative = std::fs::read_to_string(tree.join(".git")).unwrap();
    let administrative = PathBuf::from(administrative.trim().strip_prefix("gitdir: ").unwrap());
    let backlink = administrative.join("gitdir");
    let original = std::fs::read(&backlink).unwrap();
    std::fs::write(&backlink, vec![b'x'; 128 * 1024]).unwrap();
    assert!(matches!(
        tokio::time::timeout(
            Duration::from_secs(2),
            verdict(&session, &tree.join("large.txt"), true)
        )
        .await
        .unwrap(),
        Decision::Ask { .. }
    ));
    std::fs::remove_file(&backlink).unwrap();
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&backlink)
            .status()
            .unwrap()
            .success()
    );
    assert!(matches!(
        tokio::time::timeout(
            Duration::from_secs(2),
            verdict(&session, &tree.join("pipe.txt"), true)
        )
        .await
        .unwrap(),
        Decision::Ask { .. }
    ));
    assert!(
        server
            .service::<SessionStore>()
            .unwrap()
            .worktree_directories()
            .await
            .unwrap()
            .is_empty()
    );
    std::fs::remove_file(&backlink).unwrap();
    std::fs::write(&backlink, original).unwrap();
    assert_eq!(
        verdict(&session, &tree.join("retry.txt"), true).await,
        Decision::Allow
    );
    server.shutdown();
    server.join().await.unwrap();
}
