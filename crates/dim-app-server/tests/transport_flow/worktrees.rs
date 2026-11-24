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
use dim_agent_runtime::testing::chain_events;
use dim_agent_runtime::{AgentError, Session, SessionConfig};
use dim_agent_types::{ApprovalCallData, InputMode, PermissionPolicy, PermissionPreset};
use dim_app_server::protocol::SessionOrigin;
use dim_app_server::storage::SessionStore;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

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
            boundary: None,
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
    chain_events(session, "main", ..)
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
    let directories: Vec<_> = store
        .worktree_directories()
        .await
        .unwrap()
        .into_iter()
        .map(|record| record.directory)
        .collect();
    assert_eq!(directories, std::slice::from_ref(&directory));
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_worktree_ownership_and_tool_workspace_survive_host_restart() {
    for relative_cwd in ["", "packages/a"] {
        let work = Work::new();
        let server = host(&work).await;
        let mut client = Client::new_version(&server, false, 3).await;
        let source_cwd = work.repo.join(relative_cwd);
        let created = client
            .rpc("session.create", json!({"cwd": source_cwd, "worktree": {}}))
            .await;
        assert_ok(&created);
        let session_id = created["result"]["sessionId"].as_str().unwrap().to_owned();
        let task_id = created["result"]["worktree"]["taskId"]
            .as_str()
            .unwrap()
            .to_owned();
        let root = PathBuf::from(created["result"]["worktree"]["root"].as_str().unwrap());
        let cwd = root.join(relative_cwd).canonicalize().unwrap();
        assert!(root.starts_with(work.home.join("worktrees")));
        assert_eq!(created["result"]["worktree"]["cwd"], json!(cwd));
        assert!(root.join("packages/a/seed.txt").is_file());
        let listed = client.rpc("worktree.list", json!({})).await;
        assert_ok(&listed);
        assert_eq!(listed["result"]["worktrees"].as_array().unwrap().len(), 1);
        assert_eq!(listed["result"]["worktrees"][0]["taskId"], task_id);
        assert_eq!(
            listed["result"]["worktrees"][0]["sessions"],
            json!([session_id])
        );
        drop(client);
        server.shutdown();
        server.join().await.unwrap();

        let server = host(&work).await;
        let mut client = Client::new_version(&server, false, 3).await;
        let restored = client.rpc("worktree.list", json!({})).await;
        assert_ok(&restored);
        assert_eq!(restored["result"], listed["result"]);
        let attached = client
            .rpc("session.attach", json!({"sessionId": session_id}))
            .await;
        assert_ok(&attached);
        assert_eq!(attached["result"]["cwd"], json!(cwd));
        let managed = server.agent().unwrap().get_session(&session_id).unwrap();
        let written = cwd.join("after-restart.txt");
        succeeded(
            run(
                &managed,
                "write",
                json!({"path":"after-restart.txt","content":"managed workspace after restart"}),
            )
            .await,
        );
        assert_eq!(
            std::fs::read_to_string(&written).unwrap(),
            "managed workspace after restart"
        );
        assert!(!source_cwd.join("after-restart.txt").exists());
        assert!(
            chain_events(&managed, "main", ..)
                .await
                .unwrap()
                .iter()
                .all(|event| !matches!(event.kind, EventKind::ApprovalRequest(_))),
            "workspace-write must not ask for per-file approval in the restored cwd"
        );
        let refused = client
            .rpc("worktree.remove", json!({"taskId": task_id, "force": true}))
            .await;
        assert_eq!(refused["error"]["data"]["appError"], "worktree_in_use");
        assert_eq!(
            std::fs::read_to_string(&written).unwrap(),
            "managed workspace after restart"
        );

        let shared = session(&server, &source_cwd).await;
        succeeded(run(&shared, "read", json!({"path": written})).await);
        let owners = client.rpc("worktree.list", json!({})).await;
        assert_ok(&owners);
        let owners = owners["result"]["worktrees"][0]["sessions"]
            .as_array()
            .unwrap();
        assert_eq!(owners.len(), 2);
        assert!(owners.contains(&json!(session_id)));
        assert!(owners.contains(&json!(shared.id())));
        assert_ok(
            &client
                .rpc("session.delete", json!({"sessionId": session_id}))
                .await,
        );
        let refused = client
            .rpc("worktree.remove", json!({"taskId": task_id, "force": true}))
            .await;
        assert_eq!(refused["error"]["data"]["appError"], "worktree_in_use");
        assert_ok(
            &client
                .rpc("session.delete", json!({"sessionId": shared.id()}))
                .await,
        );
        let dirty = client.rpc("worktree.list", json!({})).await;
        assert_ok(&dirty);
        assert_eq!(dirty["result"]["worktrees"][0]["sessions"], json!([]));
        assert_eq!(dirty["result"]["worktrees"][0]["untracked"], 1);
        let refused = client
            .rpc("worktree.remove", json!({"taskId": task_id}))
            .await;
        assert_eq!(refused["error"]["data"]["appError"], "worktree_in_use");
        assert!(written.exists());
        std::fs::remove_file(written).unwrap();
        let removed = client
            .rpc("worktree.remove", json!({"taskId": task_id}))
            .await;
        assert_ok(&removed);
        assert_eq!(removed["result"]["removed"], true);
        assert!(!root.exists());
        let empty = client.rpc("worktree.list", json!({})).await;
        assert_ok(&empty);
        assert_eq!(empty["result"]["worktrees"], json!([]));
        server.shutdown();
        server.join().await.unwrap();
    }
}

/// A home that never created a worktree has no root directory; listing and
/// pruning there answer an empty set instead of a storage failure.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_worktree_listing_and_pruning_on_a_fresh_home_are_empty() {
    let work = Work::new();
    let server = host(&work).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let listed = client.rpc("worktree.list", json!({})).await;
    assert_ok(&listed);
    assert_eq!(listed["result"]["worktrees"], json!([]));
    let pruned = client
        .rpc("worktree.prune", json!({"keep": 1, "apply": false}))
        .await;
    assert_ok(&pruned);
    assert_eq!(pruned["result"]["candidates"], json!([]));
    server.shutdown();
    server.join().await.unwrap();
}

/// A task directory swapped for a symlink must not let a recorded removal
/// reach outside the managed root.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_swapped_task_directory_symlink_cannot_escape_the_managed_root() {
    let work = Work::new();
    let server = host(&work).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client
        .rpc("session.create", json!({"cwd": work.repo, "worktree": {}}))
        .await;
    assert_ok(&created);
    let session_id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let task_id = created["result"]["worktree"]["taskId"]
        .as_str()
        .unwrap()
        .to_owned();
    let root = PathBuf::from(created["result"]["worktree"]["root"].as_str().unwrap());
    assert_ok(
        &client
            .rpc("session.delete", json!({"sessionId": session_id}))
            .await,
    );
    let outside = work.home.join("outside");
    std::fs::create_dir_all(outside.join("repo")).unwrap();
    std::fs::write(outside.join("repo/secret.txt"), "secret").unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    let task = root.parent().unwrap().to_path_buf();
    std::fs::remove_dir_all(&task).unwrap();
    std::os::unix::fs::symlink(&outside, &task).unwrap();
    let removed = client
        .rpc("worktree.remove", json!({"taskId": task_id, "force": true}))
        .await;
    assert!(removed.get("error").is_some(), "{removed}");
    assert_eq!(
        std::fs::read_to_string(outside.join("repo/secret.txt")).unwrap(),
        "secret"
    );
    server.shutdown();
    server.join().await.unwrap();
}

fn unregistered_task(work: &Work, task_id: &str) -> PathBuf {
    let task = work.home.join("worktrees").join(task_id);
    std::fs::create_dir_all(&task).unwrap();
    task
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unregistered_remove_with_an_ordinary_file_needs_force() {
    let work = Work::new();
    let server = host(&work).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let task = unregistered_task(&work, "aaaaaa");
    std::fs::write(task.join("notes.txt"), "keep").unwrap();
    let refused = client
        .rpc("worktree.remove", json!({"taskId": "aaaaaa"}))
        .await;
    assert_eq!(refused["error"]["data"]["appError"], "worktree_in_use");
    assert_eq!(
        std::fs::read_to_string(task.join("notes.txt")).unwrap(),
        "keep"
    );
    assert!(task.is_dir());
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unregistered_remove_with_force_deletes_ordinary_content() {
    let work = Work::new();
    let server = host(&work).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let task = unregistered_task(&work, "aaaaaa");
    std::fs::write(task.join("notes.txt"), "keep").unwrap();
    let removed = client
        .rpc(
            "worktree.remove",
            json!({"taskId": "aaaaaa", "force": true}),
        )
        .await;
    assert_ok(&removed);
    assert_eq!(removed["result"]["removed"], true);
    assert!(!task.exists());
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unregistered_remove_deletes_an_empty_repository_directory_without_force() {
    let work = Work::new();
    let server = host(&work).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let task = unregistered_task(&work, "aaaaaa");
    std::fs::create_dir_all(task.join("repo")).unwrap();
    let removed = client
        .rpc("worktree.remove", json!({"taskId": "aaaaaa"}))
        .await;
    assert_ok(&removed);
    assert_eq!(removed["result"]["removed"], true);
    assert!(!task.exists());
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unregistered_remove_checks_every_child_before_deleting() {
    let work = Work::new();
    let server = host(&work).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let task = unregistered_task(&work, "aaaaaa");
    std::fs::create_dir_all(task.join("empty")).unwrap();
    std::fs::write(task.join("notes.txt"), "keep").unwrap();
    let refused = client
        .rpc("worktree.remove", json!({"taskId": "aaaaaa"}))
        .await;
    assert_eq!(refused["error"]["data"]["appError"], "worktree_in_use");
    assert!(task.join("empty").is_dir());
    assert_eq!(
        std::fs::read_to_string(task.join("notes.txt")).unwrap(),
        "keep"
    );
    server.shutdown();
    server.join().await.unwrap();
}

#[cfg(unix)]
struct RestorePermissions(PathBuf);
#[cfg(unix)]
impl Drop for RestorePermissions {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

/// A task directory that cannot be emptied must report the failure, and the
/// permissions must not outlive the test. File permissions do not bind a user
/// that bypasses them, such as root, so the test skips itself there.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unregistered_remove_reports_a_failed_deletion() {
    let work = Work::new();
    let task = unregistered_task(&work, "aaaaaa");
    std::fs::write(task.join("notes.txt"), "keep").unwrap();
    std::fs::set_permissions(&task, std::fs::Permissions::from_mode(0o500)).unwrap();
    let _restore = RestorePermissions(task.clone());
    if std::fs::write(task.join("probe"), b"").is_ok() {
        return;
    }
    std::fs::set_permissions(&task, std::fs::Permissions::from_mode(0o555)).unwrap();
    let server = host(&work).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let failed = client
        .rpc(
            "worktree.remove",
            json!({"taskId": "aaaaaa", "force": true}),
        )
        .await;
    assert_eq!(failed["error"]["data"]["appError"], "worktree_store_failed");
    assert_eq!(
        std::fs::read_to_string(task.join("notes.txt")).unwrap(),
        "keep"
    );
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unregistered_remove_refuses_a_symlink_and_keeps_its_target() {
    let work = Work::new();
    let server = host(&work).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let task = unregistered_task(&work, "aaaaaa");
    let target = work.home.join("outside");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("secret.txt"), "secret").unwrap();
    std::os::unix::fs::symlink(&target, task.join("linked")).unwrap();
    let refused = client
        .rpc("worktree.remove", json!({"taskId": "aaaaaa"}))
        .await;
    assert_eq!(refused["error"]["data"]["appError"], "worktree_in_use");
    assert!(
        task.join("linked")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        std::fs::read_to_string(target.join("secret.txt")).unwrap(),
        "secret"
    );
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unregistered_remove_refuses_a_git_entry_even_with_force() {
    let work = Work::new();
    let server = host(&work).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let task = unregistered_task(&work, "aaaaaa");
    std::fs::write(task.join(".git"), "gitdir: elsewhere").unwrap();
    let refused = client
        .rpc(
            "worktree.remove",
            json!({"taskId": "aaaaaa", "force": true}),
        )
        .await;
    assert_eq!(refused["error"]["data"]["appError"], "worktree_in_use");
    assert!(task.join(".git").is_file());
    server.shutdown();
    server.join().await.unwrap();
}

/// A `.git` entry that is a dangling symlink still counts: the directory is
/// never deleted, with or without `--force`.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unregistered_remove_refuses_a_dangling_git_symlink_even_with_force() {
    let work = Work::new();
    let server = host(&work).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let task = unregistered_task(&work, "aaaaaa");
    std::os::unix::fs::symlink(work.home.join("missing-gitdir"), task.join(".git")).unwrap();
    let refused = client
        .rpc(
            "worktree.remove",
            json!({"taskId": "aaaaaa", "force": true}),
        )
        .await;
    assert_eq!(refused["error"]["data"]["appError"], "worktree_in_use");
    assert!(
        task.join(".git")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink()
    );
    server.shutdown();
    server.join().await.unwrap();
}

/// A `.git` directory below a direct child is still inside the tree a forced
/// removal deletes: the removal refuses and leaves the whole tree.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unregistered_remove_refuses_a_nested_git_directory_even_with_force() {
    let work = Work::new();
    let server = host(&work).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let task = unregistered_task(&work, "aaaaaa");
    let nested = task.join("repo/a/b");
    std::fs::create_dir_all(nested.join(".git")).unwrap();
    std::fs::write(nested.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::write(nested.join("keep.txt"), "keep").unwrap();
    let refused = client
        .rpc(
            "worktree.remove",
            json!({"taskId": "aaaaaa", "force": true}),
        )
        .await;
    assert_eq!(refused["error"]["data"]["appError"], "worktree_in_use");
    assert_eq!(
        std::fs::read_to_string(nested.join(".git/HEAD")).unwrap(),
        "ref: refs/heads/main\n"
    );
    assert_eq!(
        std::fs::read_to_string(nested.join("keep.txt")).unwrap(),
        "keep"
    );
    server.shutdown();
    server.join().await.unwrap();
}

/// A gitdir file below a direct child is Git metadata too, even with `--force`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unregistered_remove_refuses_a_nested_gitdir_file_even_with_force() {
    let work = Work::new();
    let server = host(&work).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let task = unregistered_task(&work, "aaaaaa");
    let nested = task.join("repo/a");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join(".git"), "gitdir: elsewhere\n").unwrap();
    std::fs::write(nested.join("keep.txt"), "keep").unwrap();
    let refused = client
        .rpc(
            "worktree.remove",
            json!({"taskId": "aaaaaa", "force": true}),
        )
        .await;
    assert_eq!(refused["error"]["data"]["appError"], "worktree_in_use");
    assert_eq!(
        std::fs::read_to_string(nested.join(".git")).unwrap(),
        "gitdir: elsewhere\n"
    );
    assert_eq!(
        std::fs::read_to_string(nested.join("keep.txt")).unwrap(),
        "keep"
    );
    server.shutdown();
    server.join().await.unwrap();
}

/// A dangling `.git` symlink below a direct child still counts as Git metadata:
/// the walk does not follow it, and the removal refuses the whole tree.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unregistered_remove_refuses_a_nested_dangling_git_symlink_even_with_force() {
    let work = Work::new();
    let server = host(&work).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let task = unregistered_task(&work, "aaaaaa");
    let nested = task.join("repo/a/b");
    std::fs::create_dir_all(&nested).unwrap();
    let target = work.home.join("missing-gitdir");
    std::os::unix::fs::symlink(&target, nested.join(".git")).unwrap();
    std::fs::write(nested.join("keep.txt"), "keep").unwrap();
    let refused = client
        .rpc(
            "worktree.remove",
            json!({"taskId": "aaaaaa", "force": true}),
        )
        .await;
    assert_eq!(refused["error"]["data"]["appError"], "worktree_in_use");
    assert!(
        nested
            .join(".git")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read_link(nested.join(".git")).unwrap(), target);
    assert_eq!(
        std::fs::read_to_string(nested.join("keep.txt")).unwrap(),
        "keep"
    );
    server.shutdown();
    server.join().await.unwrap();
}

/// A deep tree without Git metadata is ordinary content: `--force` removes it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unregistered_remove_with_force_deletes_a_deep_tree_without_git_metadata() {
    let work = Work::new();
    let server = host(&work).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let task = unregistered_task(&work, "aaaaaa");
    let nested = task.join("repo/a/b/c");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("notes.txt"), "delete").unwrap();
    let removed = client
        .rpc(
            "worktree.remove",
            json!({"taskId": "aaaaaa", "force": true}),
        )
        .await;
    assert_ok(&removed);
    assert_eq!(removed["result"]["removed"], true);
    assert!(!task.exists());
    server.shutdown();
    server.join().await.unwrap();
}

/// A Session whose workspace is the task directory itself keeps it, with or
/// without `--force`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unregistered_remove_refuses_a_task_directory_a_session_uses() {
    let work = Work::new();
    let server = host(&work).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let empty = unregistered_task(&work, "aaaaaa");
    std::fs::create_dir_all(empty.join("repo")).unwrap();
    assert_ok(&client.rpc("session.create", json!({"cwd": empty})).await);
    let refused = client
        .rpc("worktree.remove", json!({"taskId": "aaaaaa"}))
        .await;
    assert_eq!(refused["error"]["data"]["appError"], "worktree_in_use");
    assert!(empty.join("repo").is_dir());
    let filled = unregistered_task(&work, "bbbbbb");
    std::fs::write(filled.join("notes.txt"), "keep").unwrap();
    assert_ok(&client.rpc("session.create", json!({"cwd": filled})).await);
    let refused = client
        .rpc(
            "worktree.remove",
            json!({"taskId": "bbbbbb", "force": true}),
        )
        .await;
    assert_eq!(refused["error"]["data"]["appError"], "worktree_in_use");
    assert_eq!(
        std::fs::read_to_string(filled.join("notes.txt")).unwrap(),
        "keep"
    );
    server.shutdown();
    server.join().await.unwrap();
}

/// A Session created after the content check but before the removal keeps
/// the directory: the store re-checks under the ownership lock creation uses.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unregistered_remove_refuses_a_session_created_after_the_content_check() {
    let work = Work::new();
    let server = host(&work).await;
    let task = unregistered_task(&work, "aaaaaa");
    std::fs::create_dir_all(task.join("repo")).unwrap();
    let hold = server
        .hold_next_unregistered_removal()
        .expect("the ManagedWorktrees part exists");
    let mut remover = Client::new_version(&server, false, 3).await;
    let removal = tokio::spawn(async move {
        remover
            .rpc("worktree.remove", json!({"taskId": "aaaaaa"}))
            .await
    });
    tokio::time::timeout(Duration::from_secs(30), hold.reached.notified())
        .await
        .expect("the removal reaches the end of its content check");
    let mut creator = Client::new_version(&server, false, 3).await;
    assert_ok(&creator.rpc("session.create", json!({"cwd": task})).await);
    hold.release.notify_one();
    let refused = removal.await.unwrap();
    assert_eq!(refused["error"]["data"]["appError"], "worktree_in_use");
    assert!(task.is_dir());
    assert!(task.join("repo").is_dir());
    server.shutdown();
    server.join().await.unwrap();
}

/// A directory recorded under the task root after the content check keeps the
/// whole task directory: the store re-checks the directory records under the
/// ownership lock an association takes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unregistered_remove_refuses_a_directory_associated_after_the_content_check() {
    let work = Work::new();
    let server = host(&work).await;
    let store = server.service::<SessionStore>().unwrap();
    let task = unregistered_task(&work, "aaaaaa");
    std::fs::write(task.join("notes.txt"), "keep").unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client
        .rpc("session.create", json!({"cwd": work.repo}))
        .await;
    assert_ok(&created);
    let session_id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let hold = server
        .hold_next_unregistered_removal()
        .expect("the ManagedWorktrees part exists");
    let mut remover = Client::new_version(&server, false, 3).await;
    let removal = tokio::spawn(async move {
        remover
            .rpc(
                "worktree.remove",
                json!({"taskId": "aaaaaa", "force": true}),
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(30), hold.reached.notified())
        .await
        .expect("the removal reaches the end of its content check");
    let sibling = task.join("repo");
    git(
        &work.repo,
        &[
            "worktree",
            "add",
            "--detach",
            "-q",
            sibling.to_str().unwrap(),
            "HEAD",
        ],
    );
    store
        .associate_worktree(
            &session_id,
            WorktreeDirectory {
                repository: work.repo.clone(),
                root: sibling.clone(),
            },
            sibling.clone(),
        )
        .await
        .unwrap();
    hold.release.notify_one();
    let refused = removal.await.unwrap();
    assert_eq!(refused["error"]["data"]["appError"], "worktree_in_use");
    assert!(task.is_dir());
    assert_eq!(
        std::fs::read_to_string(task.join("notes.txt")).unwrap(),
        "keep"
    );
    assert!(sibling.join(".git").is_file());
    server.shutdown();
    server.join().await.unwrap();
}

/// A recorded directory at or under the task root is refused before the
/// removal callback runs, even though no Session's cwd is under the task
/// directory.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unregistered_removal_refuses_a_recorded_directory_before_the_callback() {
    let work = Work::new();
    let server = host(&work).await;
    let store = server.service::<SessionStore>().unwrap();
    let task = unregistered_task(&work, "aaaaaa");
    std::fs::write(task.join("notes.txt"), "keep").unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client
        .rpc("session.create", json!({"cwd": work.repo}))
        .await;
    assert_ok(&created);
    let session_id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let sibling = task.join("repo");
    git(
        &work.repo,
        &[
            "worktree",
            "add",
            "--detach",
            "-q",
            sibling.to_str().unwrap(),
            "HEAD",
        ],
    );
    store
        .associate_worktree(
            &session_id,
            WorktreeDirectory {
                repository: work.repo.clone(),
                root: sibling.clone(),
            },
            sibling.clone(),
        )
        .await
        .unwrap();
    let called = Arc::new(AtomicBool::new(false));
    let error = store
        .remove_unregistered_directory(task.clone(), {
            let called = called.clone();
            move |path| {
                Box::pin(async move {
                    called.store(true, Ordering::SeqCst);
                    tokio::fs::remove_dir_all(&path)
                        .await
                        .map_err(|error| AgentError::Storage(error.to_string()))
                })
            }
        })
        .await
        .expect_err("a recorded directory is refused");
    assert!(
        error.to_string().contains("recorded worktree directory"),
        "{error}"
    );
    assert!(!called.load(Ordering::SeqCst));
    assert!(task.is_dir());
    assert_eq!(
        std::fs::read_to_string(task.join("notes.txt")).unwrap(),
        "keep"
    );
    assert!(sibling.join(".git").is_file());
    server.shutdown();
    server.join().await.unwrap();
}

/// A `.git` entry that appears after the content check refuses the removal:
/// the store re-checks the unregistered facts under the ownership lock before
/// it deletes anything.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unregistered_remove_refuses_a_git_entry_added_after_the_content_check() {
    let work = Work::new();
    let server = host(&work).await;
    let task = unregistered_task(&work, "aaaaaa");
    std::fs::write(task.join("notes.txt"), "keep").unwrap();
    let hold = server
        .hold_next_unregistered_removal()
        .expect("the ManagedWorktrees part exists");
    let mut remover = Client::new_version(&server, false, 3).await;
    let removal = tokio::spawn(async move {
        remover
            .rpc(
                "worktree.remove",
                json!({"taskId": "aaaaaa", "force": true}),
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(30), hold.reached.notified())
        .await
        .expect("the removal reaches the end of its content check");
    std::fs::write(task.join(".git"), "gitdir: elsewhere").unwrap();
    hold.release.notify_one();
    let refused = removal.await.unwrap();
    assert_eq!(refused["error"]["data"]["appError"], "worktree_in_use");
    assert_eq!(
        std::fs::read_to_string(task.join(".git")).unwrap(),
        "gitdir: elsewhere"
    );
    assert_eq!(
        std::fs::read_to_string(task.join("notes.txt")).unwrap(),
        "keep"
    );
    server.shutdown();
    server.join().await.unwrap();
}

/// A removal that holds the ownership lock first deletes the directory, so a
/// creation queued behind it fails its own cwd check instead of leaving a
/// Session that points at a deleted workspace.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_creation_after_an_unregistered_removal_fails_its_workspace_check() {
    let work = Work::new();
    let server = host(&work).await;
    let store = server.service::<SessionStore>().unwrap();
    let task = unregistered_task(&work, "aaaaaa");
    std::fs::create_dir_all(task.join("repo")).unwrap();
    let reached = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let removal = tokio::spawn({
        let store = store.clone();
        let task = task.clone();
        let reached = reached.clone();
        let release = release.clone();
        async move {
            store
                .remove_unregistered_directory(task, move |path| {
                    Box::pin(async move {
                        reached.notify_one();
                        release.notified().await;
                        tokio::fs::remove_dir_all(&path)
                            .await
                            .map_err(|error| AgentError::Storage(error.to_string()))
                    })
                })
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(30), reached.notified())
        .await
        .expect("the removal holds the ownership lock");
    let config = SessionConfig {
        cwd: task.to_string_lossy().into_owned(),
        ..Default::default()
    };
    let creation = tokio::spawn({
        let store = store.clone();
        async move {
            store
                .create_owned_session(
                    "fixture-session",
                    &config,
                    "fixture-agent",
                    4,
                    None,
                    SessionOrigin::Client,
                    None,
                )
                .await
        }
    });
    release.notify_one();
    let error = creation
        .await
        .unwrap()
        .expect_err("the removed workspace fails the creation check");
    assert!(error.to_string().contains("no longer exists"), "{error}");
    removal.await.unwrap().unwrap();
    assert!(!task.exists());
    assert!(!store.has_records("fixture-session").await.unwrap());
    server.shutdown();
    server.join().await.unwrap();
}

/// The store face is public to trusted plugins: a relative path or one with
/// `..` compares lexically against stored workspaces while the filesystem
/// resolves it, so the owner must refuse it before the removal runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unregistered_removal_refuses_a_path_that_is_not_absolute_and_normal() {
    let work = Work::new();
    let server = host(&work).await;
    let store = server.service::<SessionStore>().unwrap();
    let task = unregistered_task(&work, "aaaaaa");
    std::fs::write(task.join("notes.txt"), "keep").unwrap();
    for path in [
        PathBuf::from("relative/task"),
        task.join("..").join("aaaaaa"),
    ] {
        let error = store
            .remove_unregistered_directory(path, |path| {
                Box::pin(async move {
                    Err(AgentError::Storage(format!(
                        "the refused path reached removal: {}",
                        path.display()
                    )))
                })
            })
            .await
            .expect_err("a path outside the store's absolute-normal form must be refused");
        assert!(
            error
                .to_string()
                .contains("absolute without relative components"),
            "{error}"
        );
        assert_eq!(
            std::fs::read_to_string(task.join("notes.txt")).unwrap(),
            "keep"
        );
    }
    server.shutdown();
    server.join().await.unwrap();
}

/// A removal that passed its content check but enters the ownership lock
/// after another removal deleted the directory reports not found.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unregistered_remove_reports_not_found_when_another_removal_won() {
    let work = Work::new();
    let server = host(&work).await;
    let task = unregistered_task(&work, "aaaaaa");
    std::fs::write(task.join("notes.txt"), "keep").unwrap();
    let hold = server
        .hold_next_unregistered_removal()
        .expect("the ManagedWorktrees part exists");
    let mut parked = Client::new_version(&server, false, 3).await;
    let parked_removal = tokio::spawn(async move {
        parked
            .rpc("worktree.remove", json!({"taskId":"aaaaaa","force":true}))
            .await
    });
    tokio::time::timeout(Duration::from_secs(30), hold.reached.notified())
        .await
        .expect("the parked removal reaches the end of its content check");
    let mut winner = Client::new_version(&server, false, 3).await;
    assert_ok(
        &winner
            .rpc("worktree.remove", json!({"taskId":"aaaaaa","force":true}))
            .await,
    );
    assert!(!task.exists());
    hold.release.notify_one();
    let late = parked_removal.await.unwrap();
    assert_eq!(late["error"]["data"]["appError"], "worktree_not_found");
    server.shutdown();
    server.join().await.unwrap();
}

/// Dropping the caller of an unregistered removal must not release the
/// ownership lock while the deletion is still running.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_the_caller_keeps_the_removal_lock_until_the_deletion_finishes() {
    let work = Work::new();
    let server = host(&work).await;
    let store = server.service::<SessionStore>().unwrap();
    let task = unregistered_task(&work, "aaaaaa");
    std::fs::create_dir_all(task.join("repo")).unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    let listed = client.rpc("agent.list", json!({})).await;
    assert_ok(&listed);
    let agent_id = listed["result"][0]["agentId"].as_str().unwrap().to_owned();
    let reached = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let removal = tokio::spawn({
        let store = store.clone();
        let task = task.clone();
        let reached = reached.clone();
        let release = release.clone();
        async move {
            store
                .remove_unregistered_directory(task, move |path| {
                    Box::pin(async move {
                        reached.notify_one();
                        release.notified().await;
                        tokio::fs::remove_dir_all(&path)
                            .await
                            .map_err(|error| AgentError::Storage(error.to_string()))
                    })
                })
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(30), reached.notified())
        .await
        .expect("the removal holds the ownership lock");
    removal.abort();
    assert!(removal.await.unwrap_err().is_cancelled());
    let config = SessionConfig {
        cwd: task.to_string_lossy().into_owned(),
        ..Default::default()
    };
    let creation = tokio::spawn({
        let store = store.clone();
        async move {
            store
                .create_owned_session(
                    "dropped-caller-session",
                    &config,
                    &agent_id,
                    4,
                    None,
                    SessionOrigin::Client,
                    None,
                )
                .await
        }
    });
    release.notify_one();
    let error = creation
        .await
        .unwrap()
        .expect_err("the removed workspace fails the creation check");
    assert!(error.to_string().contains("no longer exists"), "{error}");
    assert!(!task.exists());
    server.shutdown();
    server.join().await.unwrap();
}

#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn committed_shell_completion_recovers_after_notice_failure_and_host_loss() {
    const CHILD_HOME: &str = "DIM_TEST_EXEC_RECOVERY_HOME";
    if let Some(home) = std::env::var_os(CHILD_HOME) {
        let home = PathBuf::from(home);
        let work = Work {
            repo: home.join("repo"),
            home,
        };
        let server = host(&work).await;
        let session = session(&server, &work.repo).await;
        session.idle("main").await.unwrap();
        let connection =
            rusqlite::Connection::open(work.home.join("native/dimcode.sqlite")).unwrap();
        connection.execute_batch("CREATE TRIGGER fail_exec_notice BEFORE INSERT ON timeline_events WHEN NEW.kind = 'input/message' AND EXISTS (SELECT 1 FROM timeline_events WHERE kind = 'exec/ended' AND session_id = NEW.session_id) BEGIN SELECT RAISE(ABORT, 'injected exec notice failure'); END").unwrap();
        session
            .service::<ShellSession>("main")
            .unwrap()
            .run(
                "echo executed >> execution-count".into(),
                CallContext {
                    turn_id: "fixture-turn".into(),
                    request_id: "fixture-request".into(),
                    call_id: "fixture-call".into(),
                },
                10_000,
                true,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let error = tokio::time::timeout(Duration::from_secs(30), session.closed())
            .await
            .unwrap()
            .unwrap_err();
        assert!(
            error.to_string().contains("injected exec notice failure"),
            "{error}"
        );
        let ready = work.home.join("ready.tmp");
        std::fs::write(&ready, session.id()).unwrap();
        std::fs::rename(ready, work.home.join("ready")).unwrap();
        std::future::pending::<()>().await;
        return;
    }

    let work = Work::new();
    let log = std::fs::File::create(work.home.join("child.log")).unwrap();
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "worktrees::committed_shell_completion_recovers_after_notice_failure_and_host_loss",
            "--nocapture",
        ])
        .env(CHILD_HOME, &work.home)
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(60), async {
        while !work.home.join("ready").exists() {
            if let Some(status) = child.try_wait().unwrap() {
                panic!(
                    "child exited {status}: {}",
                    std::fs::read_to_string(work.home.join("child.log")).unwrap()
                );
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "child did not reach the committed-end/missing-notice boundary: {}",
            std::fs::read_to_string(work.home.join("child.log")).unwrap()
        );
    });
    child.kill().await.unwrap();
    let status = child.wait().await.unwrap();
    assert!(
        !status.success(),
        "the host must exit without graceful shutdown"
    );
    let id = std::fs::read_to_string(work.home.join("ready")).unwrap();
    let connection = rusqlite::Connection::open(work.home.join("native/dimcode.sqlite")).unwrap();
    let terminals: i64 = connection
        .query_row(
            "SELECT count(*) FROM timeline_events WHERE session_id=?1 AND kind='exec/ended'",
            [&id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        terminals, 1,
        "the terminal survived before notification failed"
    );
    let pending_notices: i64 = connection.query_row(
        "SELECT count(*) FROM timeline_events WHERE session_id=?1 AND kind='input/message' AND sequence > (SELECT max(sequence) FROM timeline_events WHERE session_id=?1 AND kind='exec/ended')",
        [&id], |row| row.get(0),
    ).unwrap();
    assert_eq!(
        pending_notices, 0,
        "no post-terminal input committed before host loss"
    );
    connection
        .execute_batch("DROP TRIGGER fail_exec_notice")
        .unwrap();
    drop(connection);
    let mut notice_id = None;
    for generation in 0..2 {
        let server = host(&work).await;
        let session = server
            .agent()
            .unwrap()
            .restore_session(id.clone())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(30), session.idle("main"))
            .await
            .unwrap()
            .unwrap();
        let input = format!("user input after restore {generation}");
        session
            .submit(input.clone(), InputMode::Queue, "main")
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(30), session.idle("main"))
            .await
            .unwrap()
            .unwrap();
        let events = chain_events(&session, "main", ..).await.unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e.kind, EventKind::ExecStarted(_)))
                .count(),
            1
        );
        let ends = events
            .iter()
            .filter_map(|e| match &e.kind {
                EventKind::ExecEnded(data) => Some((e, data)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(ends.len(), 1);
        assert_eq!(ends[0].1.status, dim_agent_types::ExecStatus::Completed);
        let notices = events
            .iter()
            .filter(|e| {
                matches!(e.kind, EventKind::InputMessage(_))
                    && e.tags
                        .iter()
                        .flatten()
                        .any(|tag| tag.starts_with("exec-wake-result:"))
            })
            .collect::<Vec<_>>();
        assert_eq!(
            notices.len(),
            1,
            "restore must publish exactly one missing notice"
        );
        assert!(
            notices[0]
                .tags
                .as_ref()
                .unwrap()
                .contains(&format!("exec-wake-result:{}", ends[0].0.id))
        );
        if let Some(previous) = &notice_id {
            assert_eq!(&notices[0].id, previous);
        }
        notice_id = Some(notices[0].id.clone());
        assert!(
            events.iter().any(|event| matches!(&event.kind,
                EventKind::InputConsumed(data) if data.input_id == notices[0].id
            )),
            "the recovered notice must reach the Loop's input handoff"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::Text(data) if data.delta.contains(&input))),
            "new user input must still produce model output"
        );
        assert_eq!(
            std::fs::read_to_string(work.repo.join("execution-count")).unwrap(),
            "executed\n",
            "restore must not rerun the completed command"
        );
        server.shutdown();
        server.join().await.unwrap();
    }
}
