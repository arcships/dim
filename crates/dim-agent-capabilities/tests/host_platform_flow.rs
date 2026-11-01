//! HostPlatform acceptance on Unix (`docs/architecture/rust-agent/plugins/HostPlatform.md`).
#![cfg(unix)]
use dim_agent_capabilities::host_platform::*;
use rutis::host::CtxHost;
use std::{ffi::OsString, path::PathBuf, time::Duration};
use tokio::io::AsyncReadExt;

fn env(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
    pairs
        .iter()
        .map(|(key, value)| (OsString::from(key), OsString::from(value)))
        .collect()
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only probes the process.
    unsafe { libc::kill(pid, 0) == 0 }
}

async fn gone(pid: i32) -> bool {
    for _ in 0..100 {
        if !alive(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

#[tokio::test]
async fn shell_resolution_follows_the_preference_and_never_fails_the_service() {
    let path = env(&[("PATH", "/usr/bin:/bin"), ("SHELL", "/bin/sh")]);
    let ShellStatus::Available(login) = resolve_shell(Ok(None), &path).await else {
        panic!("a POSIX host has /bin/sh")
    };
    assert_eq!(login.executable_path, PathBuf::from("/bin/sh"));
    assert_eq!(login.edition, ShellEdition::Sh);
    assert_eq!(login.source, ShellSource::Login);
    assert_eq!(login.output_encoding, OutputEncoding::Utf8);

    let explicit = br#"{"version":1,"executablePath":"/bin/sh"}"#.to_vec();
    let ShellStatus::Available(chosen) = resolve_shell(Ok(Some(explicit)), &path).await else {
        panic!("/bin/sh is a candidate")
    };
    assert_eq!(chosen.source, ShellSource::Explicit);

    let missing = br#"{"version":1,"executablePath":"/nonexistent/zsh"}"#.to_vec();
    match resolve_shell(Ok(Some(missing)), &path).await {
        ShellStatus::Unavailable { code, reason } => {
            assert_eq!(code, EXEC_SHELL_UNAVAILABLE);
            assert!(reason.contains("/nonexistent/zsh"), "{reason}");
        }
        other => panic!("an explicit path never falls back: {other:?}"),
    }
    for invalid in [
        &br#"{"version":2}"#[..],
        br#"{"version":1,"shell":"bash"}"#,
        br#"{"version":1,"executablePath":"bin/bash"}"#,
        br#"{"version":1,"executablePath":""}"#,
        br#"{"executablePath":"/bin/sh"}"#,
        b"not json",
    ] {
        assert!(matches!(
            resolve_shell(Ok(Some(invalid.to_vec())), &path).await,
            ShellStatus::Unavailable {
                code: EXEC_SHELL_CONFIG_INVALID,
                ..
            }
        ));
    }
    assert!(matches!(
        resolve_shell(Err("unreadable".into()), &path).await,
        ShellStatus::Unavailable {
            code: EXEC_SHELL_CONFIG_INVALID,
            ..
        }
    ));
}

#[tokio::test]
async fn spawn_isolates_the_environment_and_terminate_releases_the_tree() {
    let platform = HostPlatform::current(ShellStatus::Unavailable {
        code: EXEC_SHELL_UNAVAILABLE,
        reason: "spawn does not need a shell".into(),
    })
    .unwrap();
    let cwd = std::env::temp_dir();

    let mut tree = platform
        .spawn(ProcessSpec {
            executable: "/bin/sh".into(),
            args: vec!["-c".into(), "env; cat".into()],
            cwd: cwd.clone(),
            env: env(&[("ONLY", "this")]),
        })
        .unwrap();
    let mut output = String::new();
    tree.stdout().read_to_string(&mut output).await.unwrap();
    assert!(tree.wait().await.unwrap().success());
    assert!(output.contains("ONLY=this"), "{output}");
    assert!(!output.contains("HOME="), "{output}");

    let spawn_tree = || {
        platform
            .spawn(ProcessSpec {
                executable: "/bin/sh".into(),
                args: vec!["-c".into(), "sleep 100 & echo $!; wait".into()],
                cwd: cwd.clone(),
                env: vec![],
            })
            .unwrap()
    };
    let grandchild = |tree: &mut ProcessTree| {
        let mut stdout = tree.stdout();
        async move {
            let mut line = Vec::new();
            let mut byte = [0u8];
            while stdout.read(&mut byte).await.unwrap() == 1 && byte[0] != b'\n' {
                line.push(byte[0]);
            }
            String::from_utf8(line).unwrap().parse::<i32>().unwrap()
        }
    };
    let mut tree = spawn_tree();
    let pid = grandchild(&mut tree).await;
    assert!(alive(pid));
    tree.terminate().await.unwrap();
    assert!(gone(pid).await, "terminate left the grandchild");

    let mut tree = spawn_tree();
    let pid = grandchild(&mut tree).await;
    drop(tree);
    assert!(gone(pid).await, "drop left the grandchild");

    assert!(
        platform
            .spawn(ProcessSpec {
                executable: "/bin/sh".into(),
                args: vec!["-c".into(), "true".into()],
                cwd: cwd.join("dim-missing-cwd-for-spawn"),
                env: vec![],
            })
            .is_err()
    );
}

#[tokio::test]
async fn unchanged_shell_preferences_reuse_detection_and_edits_recover() {
    const CHILD_HOME: &str = "DIM_TEST_SHELL_CACHE_HOME";
    let Some(home) = std::env::var_os(CHILD_HOME) else {
        let home = std::env::temp_dir().join(format!("dim-shell-cache-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        let home = home.canonicalize().unwrap();
        let bin = home.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let shell = bin.join("bash");
        std::os::unix::fs::symlink("/bin/bash", &shell).unwrap();
        std::fs::write(
            home.join("exec-shell.json"),
            serde_json::json!({
                "version": 1, "executablePath": shell,
            })
            .to_string(),
        )
        .unwrap();
        let output = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "unchanged_shell_preferences_reuse_detection_and_edits_recover",
                "--nocapture",
            ])
            .env(CHILD_HOME, &home)
            .env("SHELL", &shell)
            .env("PATH", &bin)
            .output()
            .await
            .unwrap();
        std::fs::remove_dir_all(home).unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    };
    let home = PathBuf::from(home);
    let shell = home.join("bin/bash");
    std::fs::remove_file(&shell).unwrap();
    let root = rutis::Ctx::root_with(tokio::runtime::Handle::current());
    let storage = root
        .plugin(dim_agent_capabilities::file_storage::FileStoragePlugin::new(vec![home.clone()]));
    (&storage).await.unwrap();
    let catalog = dim_agent_capabilities::catalog();
    let build = Default::default();
    let names = dim_agent_runtime::Names::copy(&catalog, &build);
    let _execution = root
        .provide(
            dim_agent_capabilities::execution_settings::ExecutionSettingsHost::fixed(
                Default::default(),
            ),
        )
        .unwrap();
    let platform = root.plugin(
        HostPlatformPlugin::new(
            &names,
            HostPlatformConfig {
                execution: names.key().unwrap(),
                exec_shell_file: Some((0, "exec-shell.json".into())),
            },
        )
        .unwrap(),
    );
    (&platform).await.unwrap();
    let platform = root.get::<HostPlatform>().unwrap();
    assert!(matches!(
        platform.exec_shell().await,
        ShellStatus::Unavailable {
            code: EXEC_SHELL_UNAVAILABLE,
            ..
        }
    ));
    std::os::unix::fs::symlink("/bin/bash", &shell).unwrap();
    let selected = platform.exec_shell().await;
    assert!(matches!(selected, ShellStatus::Available(_)));
    std::fs::remove_file(&shell).unwrap();
    let cloned = (*platform).clone();
    for status in futures_util::future::join_all([platform.exec_shell(), cloned.exec_shell()]).await
    {
        assert_eq!(status, selected);
    }
    let config = home.join("exec-shell.json");
    let changed = serde_json::json!({"version":1,"executablePath":shell}).to_string() + "\n";
    std::fs::write(&config, &changed).unwrap();
    assert!(matches!(
        platform.exec_shell().await,
        ShellStatus::Unavailable {
            code: EXEC_SHELL_UNAVAILABLE,
            ..
        }
    ));
    std::os::unix::fs::symlink("/bin/bash", &shell).unwrap();
    assert_eq!(platform.exec_shell().await, selected);
    std::fs::write(&config, "invalid").unwrap();
    assert!(matches!(
        platform.exec_shell().await,
        ShellStatus::Unavailable {
            code: EXEC_SHELL_CONFIG_INVALID,
            ..
        }
    ));
    std::fs::remove_file(&config).unwrap();
    assert!(matches!(
        platform.exec_shell().await,
        ShellStatus::Available(_)
    ));
    root.shutdown().await.unwrap();
}

/// A start whose stdin carries the payload and then closes it.
#[tokio::test]
async fn spawn_with_stdin_delivers_the_bytes_and_closes_the_pipe() {
    let platform = HostPlatform::current(ShellStatus::Unavailable {
        code: EXEC_SHELL_UNAVAILABLE,
        reason: "spawn does not need a shell".into(),
    })
    .unwrap();
    let cwd = std::env::temp_dir();
    let payload = br#"{"hook_event_name":"Stop"}"#.to_vec();
    let mut tree = platform
        .spawn_with_stdin(
            ProcessSpec {
                executable: "/bin/sh".into(),
                args: vec!["-c".into(), "cat; echo done".into()],
                cwd: cwd.clone(),
                env: vec![],
            },
            payload.clone(),
        )
        .unwrap();
    let mut output = String::new();
    tree.stdout().read_to_string(&mut output).await.unwrap();
    assert!(tree.wait().await.unwrap().success());
    assert_eq!(output, "{\"hook_event_name\":\"Stop\"}done\n");

    let mut tree = platform
        .spawn_with_stdin(
            ProcessSpec {
                executable: "/bin/sh".into(),
                args: vec!["-c".into(), "echo ok".into()],
                cwd,
                env: vec![],
            },
            vec![b'x'; 4 * 1024 * 1024],
        )
        .unwrap();
    let mut output = String::new();
    tree.stdout().read_to_string(&mut output).await.unwrap();
    assert!(tree.wait().await.unwrap().success());
    assert_eq!(output, "ok\n");
}

#[cfg(not(target_os = "macos"))]
#[tokio::test]
async fn sandbox_backend_reports_the_bubblewrap_preparation_uses() {
    use std::os::unix::fs::PermissionsExt;
    let base = tokio::fs::canonicalize(std::env::temp_dir())
        .await
        .unwrap()
        .join(format!("dim-backend-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&base).unwrap();
    let mut platform = HostPlatform::current(ShellStatus::Unavailable {
        code: "test",
        reason: String::new(),
    })
    .unwrap();
    platform.env = env(&[("PATH", base.to_str().unwrap())]);
    assert!(matches!(
        platform.sandbox_backend(),
        SandboxBackend::Unavailable { reason } if reason.contains("bubblewrap")
    ));
    let bwrap = base.join("bwrap");
    std::fs::write(&bwrap, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&bwrap, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(platform.sandbox_backend(), SandboxBackend::Available);
    std::fs::remove_dir_all(base).unwrap();
}
