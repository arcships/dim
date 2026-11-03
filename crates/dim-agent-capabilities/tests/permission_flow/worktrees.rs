use super::*;
use std::os::unix::fs::PermissionsExt;

#[derive(Default)]
struct Associations(AtomicUsize);
impl file_access::AssociateWorktree for Associations {
    fn associate(
        &self,
        _: String,
        _: file_access::WorktreeDirectory,
        _: PathBuf,
    ) -> HostFuture<'_, ()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
}
struct Probes {
    root: PathBuf,
    repo: PathBuf,
    tree: PathBuf,
}
impl Probes {
    fn new() -> Self {
        let root = temporary_directory("worktree-probes")
            .canonicalize()
            .unwrap();
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .current_dir(&repo)
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
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "initial",
        ]);
        let tree = root.join("linked");
        git(&[
            "worktree",
            "add",
            "--detach",
            "-q",
            tree.to_str().unwrap(),
            "HEAD",
        ]);
        std::fs::create_dir(root.join("bin")).unwrap();
        let probes = Self { root, repo, tree };
        probes.wrapper();
        probes
    }
    fn wrapper(&self) {
        let file = self.root.join("bin/git");
        std::fs::write(&file, format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}/calls'\nif test -f '{}/pause'; then\n  : > '{}/entered'\n  while test -f '{}/pause'; do /bin/sleep 0.01; done\nfi\nexec /usr/bin/git \"$@\"\n", self.root.display(), self.root.display(), self.root.display(), self.root.display())).unwrap();
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    fn platform(&self) -> HostPlatform {
        let mut platform = HostPlatform::current(ShellStatus::Unavailable {
            code: "WORKTREE_TEST",
            reason: "No model shell is used".into(),
        })
        .unwrap();
        platform.env.retain(|(key, _)| key != "PATH");
        platform
            .env
            .push(("PATH".into(), self.root.join("bin").into_os_string()));
        platform
            .env
            .push(("GIT_DIR".into(), self.root.join("spoof").into_os_string()));
        platform.env.push((
            "GIT_WORK_TREE".into(),
            self.root.join("spoof").into_os_string(),
        ));
        platform
    }
    fn count(&self) -> usize {
        std::fs::read_to_string(self.root.join("calls"))
            .unwrap_or_default()
            .lines()
            .count()
    }
    async fn entered(&self) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while !self.root.join("entered").exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
    fn call(&self, name: &str) -> ApprovalCallData {
        ApprovalCallData {
            name: "write".into(),
            input: serde_json::json!({"path":self.tree.join(name)})
                .as_object()
                .unwrap()
                .clone(),
        }
    }
}
impl Drop for Probes {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn writable() -> PermissionPolicy {
    PermissionPolicy {
        preset: PermissionPreset::WorkspaceWrite,
        allowed_tools: vec!["write".into()],
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_discovery_is_shared_caller_cancellation_is_local_and_cache_is_session_scoped() {
    let probes = Probes::new();
    let associations = Arc::new(Associations::default());
    let grants = TestGrants::new(vec![grant(
        probes.tree.join("granted.txt"),
        GrantKind::File,
        GrantMode::Write,
    )]);
    let agent = granted_agent_with(
        Arc::new(RiskTools::default()),
        grants,
        TestFiles::new(),
        probes.platform(),
        Some(associations.clone()),
    )
    .await;
    let session = agent
        .create_session(config_in(
            &probes.repo,
            PermissionPolicy {
                disallowed_tools: vec!["write".into()],
                ..writable()
            },
        ))
        .await
        .unwrap();
    let permission = session.service::<Permission>("main").unwrap();
    assert!(matches!(
        permission
            .decide(
                &probes.call("denied.txt"),
                Some("write_filesystem"),
                &CancellationToken::new()
            )
            .await
            .unwrap(),
        Decision::Deny { .. }
    ));
    let version = permission.update(0, writable()).await.unwrap();
    assert_eq!(
        permission
            .decide(
                &probes.call("granted.txt"),
                Some("write_filesystem"),
                &CancellationToken::new()
            )
            .await
            .unwrap(),
        Decision::Allow
    );
    assert_eq!(probes.count(), 0);
    std::fs::write(probes.root.join("pause"), "").unwrap();
    let cancel = CancellationToken::new();
    let first = {
        let permission = permission.clone();
        let call = probes.call("cancelled.txt");
        let cancel = cancel.clone();
        tokio::spawn(async move {
            permission
                .decide(&call, Some("write_filesystem"), &cancel)
                .await
        })
    };
    probes.entered().await;
    let second = {
        let permission = permission.clone();
        let call = probes.call("kept.txt");
        tokio::spawn(async move {
            permission
                .decide(&call, Some("write_filesystem"), &CancellationToken::new())
                .await
        })
    };
    cancel.cancel();
    assert!(matches!(first.await.unwrap(), Err(HostError::Cancelled(_))));
    assert!(!second.is_finished());
    std::fs::remove_file(probes.root.join("pause")).unwrap();
    assert_eq!(second.await.unwrap().unwrap(), Decision::Allow);
    assert_eq!(probes.count(), 6);
    assert_eq!(associations.0.load(Ordering::SeqCst), 1);
    assert_eq!(
        permission
            .decide(
                &probes.call("another/new.txt"),
                Some("write_filesystem"),
                &CancellationToken::new()
            )
            .await
            .unwrap(),
        Decision::Allow
    );
    permission
        .update(
            version,
            PermissionPolicy {
                preset: PermissionPreset::ReadOnly,
                ..writable()
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        permission
            .decide(
                &probes.call("readonly.txt"),
                Some("write_filesystem"),
                &CancellationToken::new()
            )
            .await
            .unwrap(),
        Decision::Deny { .. }
    ));
    assert_eq!(probes.count(), 6);
    let other = agent
        .create_session(config_in(&probes.repo, writable()))
        .await
        .unwrap();
    assert_eq!(
        other
            .service::<Permission>("main")
            .unwrap()
            .decide(
                &probes.call("other.txt"),
                Some("write_filesystem"),
                &CancellationToken::new()
            )
            .await
            .unwrap(),
        Decision::Allow
    );
    assert_eq!(probes.count(), 12);
    assert_eq!(associations.0.load(Ordering::SeqCst), 2);
    let calls = std::fs::read_to_string(probes.root.join("calls")).unwrap();
    assert!(!calls.contains("worktree list"), "{calls}");
    agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_policy_tightened_during_git_discovery_decides_the_waiting_call() {
    let probes = Probes::new();
    let associations = Arc::new(Associations::default());
    let agent = granted_agent_with(
        Arc::new(RiskTools::default()),
        TestGrants::new(Vec::new()),
        TestFiles::new(),
        probes.platform(),
        Some(associations),
    )
    .await;
    let session = agent
        .create_session(config_in(&probes.repo, writable()))
        .await
        .unwrap();
    let permission = session.service::<Permission>("main").unwrap();
    std::fs::write(probes.root.join("pause"), "").unwrap();
    let pending = {
        let permission = permission.clone();
        let call = probes.call("waiting.txt");
        tokio::spawn(async move {
            permission
                .decide(&call, Some("write_filesystem"), &CancellationToken::new())
                .await
        })
    };
    probes.entered().await;
    permission
        .update(
            0,
            PermissionPolicy {
                preset: PermissionPreset::ReadOnly,
                ..writable()
            },
        )
        .await
        .unwrap();
    std::fs::remove_file(probes.root.join("pause")).unwrap();
    assert!(matches!(
        pending.await.unwrap().unwrap(),
        Decision::Deny { .. }
    ));
    agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn git_unavailability_timeout_reload_and_session_close_do_not_publish_permission() {
    let probes = Probes::new();
    let associations = Arc::new(Associations::default());
    let agent = granted_agent_with(
        Arc::new(RiskTools::default()),
        TestGrants::new(Vec::new()),
        TestFiles::new(),
        probes.platform(),
        Some(associations.clone()),
    )
    .await;
    let session = agent
        .create_session(config_in(&probes.repo, writable()))
        .await
        .unwrap();
    let permission = session.service::<Permission>("main").unwrap();
    std::fs::remove_file(probes.root.join("bin/git")).unwrap();
    assert!(
        matches!(permission.decide(&probes.call("missing.txt"), Some("write_filesystem"), &CancellationToken::new()).await.unwrap(), Decision::Ask { reason } if reason.contains("Worktree discovery unavailable"))
    );
    probes.wrapper();
    std::fs::write(probes.root.join("pause"), "").unwrap();
    let timed = tokio::time::timeout(
        Duration::from_secs(5),
        permission.decide(
            &probes.call("timeout.txt"),
            Some("write_filesystem"),
            &CancellationToken::new(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(timed, Decision::Ask { reason } if reason.contains("timed out")));
    assert_eq!(associations.0.load(Ordering::SeqCst), 0);
    let pending = {
        let permission = permission.clone();
        let call = probes.call("closing.txt");
        tokio::spawn(async move {
            permission
                .decide(&call, Some("write_filesystem"), &CancellationToken::new())
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(2), async {
        while probes.count() < 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(2), session.close())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        pending.await.unwrap(),
        Err(HostError::Cancelled(_))
    ));
    assert_eq!(associations.0.load(Ordering::SeqCst), 0);
    let restored = agent
        .restore_session(session.id().to_owned())
        .await
        .unwrap();
    let permission = restored.service::<Permission>("main").unwrap();
    let calls = probes.count();
    let pending = {
        let permission = permission.clone();
        let call = probes.call("reloading.txt");
        tokio::spawn(async move {
            permission
                .decide(&call, Some("write_filesystem"), &CancellationToken::new())
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(2), async {
        while probes.count() == calls {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(
        Duration::from_secs(2),
        agent.loader().restart("FileAccessFacts"),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(
        pending.await.unwrap(),
        Err(HostError::Cancelled(_))
    ));
    assert_eq!(associations.0.load(Ordering::SeqCst), 0);
    std::fs::remove_file(probes.root.join("pause")).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !restored.diagnostics().unwrap().plugins.iter().any(|part| {
            part.name == "FileAccessFacts" && part.state == rutis::host::FiberState::Active
        }) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        permission
            .decide(
                &probes.call("retry.txt"),
                Some("write_filesystem"),
                &CancellationToken::new()
            )
            .await
            .unwrap(),
        Decision::Allow
    );
    agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_source_that_becomes_a_git_worktree_is_rechecked_after_a_negative_probe() {
    let probes = Probes::new();
    let associations = Arc::new(Associations::default());
    let agent = granted_agent_with(
        Arc::new(RiskTools::default()),
        TestGrants::new(Vec::new()),
        TestFiles::new(),
        probes.platform(),
        Some(associations.clone()),
    )
    .await;
    let session = agent
        .create_session(config_in(&probes.repo, writable()))
        .await
        .unwrap();
    let permission = session.service::<Permission>("main").unwrap();
    std::fs::rename(probes.repo.join(".git"), probes.repo.join("saved-git")).unwrap();
    assert!(matches!(
        permission
            .decide(
                &probes.call("new.txt"),
                Some("write_filesystem"),
                &CancellationToken::new()
            )
            .await
            .unwrap(),
        Decision::Ask { .. }
    ));
    assert_eq!(associations.0.load(Ordering::SeqCst), 0);
    std::fs::rename(probes.repo.join("saved-git"), probes.repo.join(".git")).unwrap();
    assert_eq!(
        permission
            .decide(
                &probes.call("new.txt"),
                Some("write_filesystem"),
                &CancellationToken::new()
            )
            .await
            .unwrap(),
        Decision::Allow
    );
    assert_eq!(associations.0.load(Ordering::SeqCst), 1);
    agent.close().await.unwrap();
}
