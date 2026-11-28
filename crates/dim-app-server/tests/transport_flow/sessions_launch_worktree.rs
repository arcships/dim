use super::*;
use dim_agent_runtime::SessionConfig;
use dim_agent_runtime::SessionLines;
use std::path::{Path, PathBuf};
use tokio::sync::{mpsc, oneshot};

type Creation = (PathBuf, oneshot::Sender<Result<(), String>>);

#[derive(Clone)]
pub(super) struct CreationControl {
    root: PathBuf,
    sender: mpsc::UnboundedSender<Creation>,
}
impl CreationControl {
    pub(super) fn install(self, builder: &mut AgentBuilder) {
        builder.require_feature(Feature::new("managed-launch-gate").session(
            "managed-launch-gate",
            move |names| {
                Ok(Box::new(CreationGate {
                    control: self.clone(),
                    config: names.key()?,
                }))
            },
        ));
    }
}
struct CreationGate {
    control: CreationControl,
    config: Key<SessionScope>,
}
impl rutis::host::DynPlugin for CreationGate {
    fn name(&self) -> &str {
        "managed-launch-gate"
    }
    fn injects(&self) -> Vec<rutis::host::TypeKey> {
        vec![self.config.erase()]
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
    ) -> rutis::host::BoxFuture<'a, rutis::Result<rutis::host::Effect>> {
        Box::pin(async move {
            let config = ctx.require_at(&self.config)?;
            let cwd = PathBuf::from(&config.cwd);
            if cwd.starts_with(&self.control.root) {
                let (sender, receiver) = oneshot::channel();
                self.control.sender.send((cwd, sender)).unwrap();
                receiver.await.unwrap().map_err(|message| {
                    rutis::Error::from(rutis::host::ErrorKind::PluginFailed(message.into()))
                })?;
            }
            Ok(rutis::host::Effect::Done)
        })
    }
}

fn git(cwd: &Path, args: &[&str]) -> String {
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
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}
fn repository(store: &Store) -> PathBuf {
    let repo = store.0.join("repo");
    std::fs::create_dir_all(repo.join("packages/a")).unwrap();
    std::fs::write(repo.join("packages/a/seed.txt"), "seed").unwrap();
    git(&repo, &["init", "-q"]);
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
    repo.canonicalize().unwrap()
}
async fn host(store: &Store, creation: Option<CreationControl>) -> Served {
    let configured = store.database().exists();
    let mut served = serve_with(Setup {
        database: Some(store.database()),
        home: Some(store.0.clone()),
        creation,
        ..Default::default()
    })
    .await;
    if !configured {
        super::super::process_typescript::select_model(&mut served.client).await;
    }
    client_session(&mut served.client, &served.agent, host_policy()).await;
    served
}
async fn listed(client: &mut Client) -> Value {
    let reply = client.rpc("worktree.list", json!({})).await;
    assert_ok(&reply);
    reply["result"]["worktrees"].clone()
}
fn worktree_features(client: &Client, expected: bool) {
    let features = client.features.as_array().unwrap();
    for name in ["session.create.worktree", "sessions.launch.worktree"] {
        assert_eq!(features.contains(&json!(name)), expected, "{features:?}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_launch_maps_root_and_subtree_and_restores_after_host_restart() {
    let store = Store::new();
    let repo = repository(&store);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    let branch = git(&repo, &["branch", "--show-current"]);
    let Served {
        server,
        agent,
        sessions,
        mut client,
        ..
    } = host(&store, None).await;
    worktree_features(&client, true);
    let plain = sessions.ok("launch", json!({"cwd":repo})).await;
    assert!(plain.get("worktree").is_none(), "{plain}");
    assert_eq!(
        agent
            .get_session(plain["sessionId"].as_str().unwrap())
            .unwrap()
            .config()
            .cwd,
        repo.to_str().unwrap()
    );
    let mut created = Vec::new();
    for subtree in ["", "packages/a"] {
        let reply = sessions
            .ok(
                "launch",
                json!({"cwd":repo.join(subtree), "worktree":{"base":"HEAD"},
                    "origin":"automation", "policy":host_policy(), "title":"Managed launch"}),
            )
            .await;
        let id = reply["sessionId"].as_str().unwrap();
        let tree = &reply["worktree"];
        assert_eq!(tree.as_object().unwrap().len(), 6, "{tree}");
        let root = Path::new(tree["root"].as_str().unwrap());
        let cwd = Path::new(tree["cwd"].as_str().unwrap());
        assert_eq!(cwd, root.join(subtree));
        assert_eq!(tree["repository"], repo.join(".git").to_str().unwrap());
        assert_eq!(tree["base"], head);
        assert_eq!(
            tree["branch"],
            format!("dim/{}", tree["taskId"].as_str().unwrap())
        );
        assert_eq!(git(root, &["branch", "--show-current"]), tree["branch"]);
        let session = agent.get_session(id).unwrap();
        assert_eq!(session.config().cwd, cwd.to_str().unwrap());
        assert!(!session.config().interactive);
        assert_eq!(reply["policy"], host_policy());
        let attach = client
            .rpc("session.attach", json!({"sessionId":id, "expectedCwd":cwd}))
            .await;
        assert_ok(&attach);
        assert_eq!(attach["result"]["origin"], "automation");
        assert_ok(&client.rpc("session.detach", json!({"sessionId":id})).await);
        session.idle("main").await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while !session.unload_if_idle().await.unwrap() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the detached managed Session unloads after becoming idle");
        assert!(agent.get_session(id).is_none(), "{subtree}");
        let ownership = listed(&mut client).await;
        let entry = ownership
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["taskId"] == tree["taskId"])
            .unwrap();
        assert_eq!(entry["sessions"], json!([id]), "{subtree}");
        assert_eq!(entry["root"], tree["root"], "{subtree}");
        let attach = client
            .rpc("session.attach", json!({"sessionId":id, "expectedCwd":cwd}))
            .await;
        assert_ok(&attach);
        assert_eq!(agent.get_session(id).unwrap().config().cwd, tree["cwd"]);
        created.push((id.to_owned(), tree.clone()));
    }
    let before = listed(&mut client).await;
    assert_eq!(before.as_array().unwrap().len(), 2, "{before}");
    for (id, tree) in &created {
        let entry = before
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["taskId"] == tree["taskId"])
            .unwrap();
        assert_eq!(entry["sessions"], json!([id]));
        assert_eq!(entry["root"], tree["root"]);
    }
    let instance = client.instance.clone();
    drop(client);
    drop(sessions);
    drop(agent);
    server.shutdown();
    server.join().await.unwrap();
    let Served {
        server,
        agent,
        mut client,
        ..
    } = host(&store, None).await;
    assert_ne!(client.instance, instance);
    assert_eq!(listed(&mut client).await, before);
    for (id, tree) in created {
        let reply = client
            .rpc(
                "session.attach",
                json!({"sessionId":id, "expectedCwd":tree["cwd"]}),
            )
            .await;
        assert_ok(&reply);
        assert_eq!(agent.get_session(&id).unwrap().config().cwd, tree["cwd"]);
    }
    assert_eq!(git(&repo, &["branch", "--show-current"]), branch);
    assert_eq!(git(&repo, &["status", "--porcelain"]), "");
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_launch_refuses_conflicts_and_rolls_back_failed_association() {
    let store = Store::new();
    let repo = repository(&store);
    let Served {
        server,
        agent,
        sessions,
        mut client,
        ..
    } = host(&store, None).await;
    let count = agent.sessions().len();
    git(&repo, &["branch", "existing-launch"]);
    for (worktree, code) in [
        (
            json!({"branch":"existing-launch"}),
            "worktree_branch_exists",
        ),
        (json!({"base":"missing-launch-base"}), "worktree_git_failed"),
        (json!({"branch":"invalid branch"}), "worktree_git_failed"),
    ] {
        let reply = sessions
            .call("launch", json!({"cwd":repo,"worktree":worktree}))
            .await;
        assert_eq!(reply["error"]["code"], "failed", "{reply}");
        assert!(
            reply["error"]["message"].as_str().unwrap().contains(code),
            "{reply}"
        );
    }
    for worktree in [
        Value::Null,
        json!([]),
        json!([null, null]),
        json!({"branch":null}),
        json!({"base":17}),
        json!({"unknown":true}),
    ] {
        assert_eq!(
            sessions
                .error("launch", json!({"cwd":repo,"worktree":worktree}))
                .await,
            "rejected"
        );
        let reply = client
            .rpc("session.create", json!({"cwd":repo,"worktree":worktree}))
            .await;
        assert_eq!(
            reply["error"]["data"]["appError"], "invalid_params",
            "{reply}"
        );
    }
    assert_eq!(listed(&mut client).await, json!([]));
    assert_eq!(agent.sessions().len(), count);
    let connection = rusqlite::Connection::open(store.database()).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_launch_worktree BEFORE INSERT ON session_worktrees BEGIN SELECT RAISE(ABORT, 'launch association unavailable'); END;").unwrap();
    let reply = sessions
        .call(
            "launch",
            json!({"cwd":repo,"worktree":{"branch":"failed-launch"}}),
        )
        .await;
    assert_eq!(reply["error"]["code"], "failed", "{reply}");
    assert!(
        reply["error"]["message"]
            .as_str()
            .unwrap()
            .contains("launch association unavailable"),
        "{reply}"
    );
    assert_eq!(listed(&mut client).await, json!([]));
    assert_eq!(git(&repo, &["branch", "--list", "failed-launch"]), "");
    assert_eq!(
        git(&repo, &["branch", "--list", "existing-launch"]),
        "existing-launch"
    );
    assert_eq!(
        std::fs::read_dir(store.0.join("worktrees"))
            .unwrap()
            .count(),
        0
    );
    assert_eq!(agent.sessions().len(), count);
    connection
        .execute_batch("DROP TRIGGER reject_launch_worktree")
        .unwrap();
    drop(connection);
    let recovered = sessions
        .ok(
            "launch",
            json!({"cwd":repo,"worktree":{"branch":"failed-launch"}}),
        )
        .await;
    assert_eq!(recovered["worktree"]["branch"], "failed-launch");
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_launch_keeps_owned_creation_when_caller_leaves_and_preserves_unsafe_residue() {
    let store = Store::new();
    let repo = repository(&store);
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let control = CreationControl {
        root: store.0.canonicalize().unwrap().join("worktrees"),
        sender,
    };
    let Served {
        server,
        sessions,
        mut client,
        ..
    } = host(&store, Some(control)).await;
    let call = sessions.clone();
    let cwd = repo.clone();
    let caller = tokio::spawn(async move {
        call.ok(
            "launch",
            json!({"cwd":cwd,"worktree":{"branch":"detached-launch"}}),
        )
        .await
    });
    let (cwd, release) = tokio::time::timeout(Duration::from_secs(10), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    release.send(Ok(())).unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let trees = listed(&mut client).await;
            if trees.as_array().unwrap().iter().any(|tree| {
                tree["branch"] == "detached-launch"
                    && !tree["sessions"].as_array().unwrap().is_empty()
            }) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(cwd.join("packages/a/seed.txt").exists());
    let call = sessions.clone();
    let source = repo.clone();
    let caller = tokio::spawn(async move {
        call.call(
            "launch",
            json!({"cwd":source,"worktree":{"branch":"unsafe-residue"}}),
        )
        .await
    });
    let (cwd, release) = tokio::time::timeout(Duration::from_secs(10), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    std::fs::write(cwd.join("concurrent.txt"), "do not discard").unwrap();
    release
        .send(Err("injected launch assembly failure".into()))
        .unwrap();
    let reply = caller.await.unwrap();
    assert_eq!(reply["error"]["code"], "failed", "{reply}");
    assert!(
        reply["error"]["message"]
            .as_str()
            .unwrap()
            .contains("injected launch assembly failure"),
        "{reply}"
    );
    assert_eq!(
        std::fs::read_to_string(cwd.join("concurrent.txt")).unwrap(),
        "do not discard"
    );
    let trees = listed(&mut client).await;
    let residue = trees
        .as_array()
        .unwrap()
        .iter()
        .find(|tree| tree["root"] == cwd.to_str().unwrap())
        .unwrap();
    assert_eq!(residue["sessions"], json!([]));
    assert_eq!(residue["unregistered"], false);
    assert_eq!(
        git(
            &repo,
            &[
                "branch",
                "--list",
                "--format=%(refname:short)",
                "unsafe-residue"
            ]
        ),
        "unsafe-residue"
    );
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_launch_preserves_a_clean_worktree_used_by_a_concurrent_session() {
    let store = Store::new();
    let repo = repository(&store);
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let control = CreationControl {
        root: store.0.canonicalize().unwrap().join("worktrees"),
        sender,
    };
    let Served {
        server,
        sessions,
        mut client,
        ..
    } = host(&store, Some(control)).await;
    for (branch, subtree) in [("root-in-use", ""), ("subtree-in-use", "packages/a")] {
        let call = sessions.clone();
        let source = repo.clone();
        let caller = tokio::spawn(async move {
            call.call("launch", json!({"cwd":source,"worktree":{"branch":branch}}))
                .await
        });
        let (root, fail) = tokio::time::timeout(Duration::from_secs(10), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        let cwd = root.join(subtree);
        let id = client.send("session.create", json!({"cwd":cwd})).await;
        let (created_cwd, release) = tokio::time::timeout(Duration::from_secs(10), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(created_cwd, cwd);
        release.send(Ok(())).unwrap();
        let created = client.response(id).await;
        assert!(created.get("error").is_none(), "{created}");
        let owner = created["result"]["sessionId"].as_str().unwrap();
        fail.send(Err("injected concurrent launch failure".into()))
            .unwrap();
        let reply = caller.await.unwrap();
        assert_eq!(reply["error"]["code"], "failed", "{reply}");
        assert!(
            reply["error"]["message"]
                .as_str()
                .unwrap()
                .contains("injected concurrent launch failure"),
            "{reply}"
        );
        assert!(root.join("packages/a/seed.txt").exists());
        assert_eq!(git(&root, &["branch", "--show-current"]), branch);
        assert_eq!(git(&root, &["status", "--porcelain"]), "");
        let attached = client
            .rpc(
                "session.attach",
                json!({"sessionId":owner,"expectedCwd":cwd}),
            )
            .await;
        assert!(attached.get("error").is_none(), "{attached}");
        let trees = listed(&mut client).await;
        let residue = trees
            .as_array()
            .unwrap()
            .iter()
            .find(|tree| tree["root"] == root.to_str().unwrap())
            .unwrap();
        assert_eq!(residue["unregistered"], false);
        assert_eq!(residue["sessions"], json!([]));
    }
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_launch_requires_a_product_home_and_native_store() {
    let store = Store::new();
    let repo = repository(&store);
    for database in [None, Some(store.database())] {
        let Served {
            server,
            agent,
            sessions,
            mut client,
            ..
        } = serve_sessions(database).await;
        worktree_features(&client, false);
        client_session(&mut client, &agent, host_policy()).await;
        let reply = sessions
            .call("launch", json!({"cwd":repo,"worktree":{}}))
            .await;
        assert_eq!(reply["error"]["code"], "failed", "{reply}");
        assert!(
            reply["error"]["message"]
                .as_str()
                .unwrap()
                .contains("worktree_unavailable"),
            "{reply}"
        );
        let plain = sessions.ok("launch", json!({"cwd":repo})).await;
        assert!(plain.get("worktree").is_none(), "{plain}");
        server.shutdown();
        server.join().await.unwrap();
    }
}
