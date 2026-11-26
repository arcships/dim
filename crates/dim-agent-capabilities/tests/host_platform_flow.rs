//! HostPlatform acceptance on Unix (`docs/architecture/rust-agent/plugins/HostPlatform.md`).
use base64::Engine;
use dim_agent_capabilities::execution_settings::{ExecutionSettings, ExecutionSettingsHost};
use dim_agent_capabilities::host_platform::*;
use dim_agent_capabilities::toolchain::{InstalledToolchain, ToolchainSourceHost};
use rutis::host::CtxHost;
use std::{ffi::OsString, path::PathBuf, time::Duration};
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

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

/// The grandchild's pid, once the script has written it. A pid file the
/// redirect has created but `echo` has not filled in yet does not count:
/// `echo` writes the digits and then one newline, so only the newline says
/// the write completed.
async fn recorded_pid(pid_file: &std::path::Path) -> i32 {
    let until = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < until {
        if let Ok(text) = std::fs::read_to_string(pid_file)
            && let Some(line) = text.strip_suffix('\n')
            && let Ok(pid) = line.parse()
        {
            return pid;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the script never recorded its grandchild");
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

fn path_string(path: impl AsRef<std::path::Path>) -> String {
    path.as_ref().to_string_lossy().into_owned()
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
        .spawn_tree(ProcessSpec {
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
            .spawn_tree(ProcessSpec {
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
            .spawn_tree(ProcessSpec {
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
        let _ = std::fs::remove_dir_all(&home);
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
                "host_platform_flow::unchanged_shell_preferences_reuse_detection_and_edits_recover",
                "--nocapture",
            ])
            .env(CHILD_HOME, &home)
            .env("SHELL", &shell)
            .env("PATH", &bin)
            .output()
            .await
            .unwrap();
        std::fs::remove_dir_all(home).unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stdout}\n{stderr}");
        assert!(
            stdout.contains("1 passed;"),
            "child shell-cache probe ran no test:\n{stdout}\n{stderr}"
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
    let _toolchain = root
        .provide(dim_agent_capabilities::toolchain::ToolchainSourceHost::fixed(vec![]))
        .unwrap();
    let platform = root.plugin(
        HostPlatformPlugin::new(
            &names,
            HostPlatformConfig {
                execution: names.key().unwrap(),
                toolchain: names.key().unwrap(),
                exec_shell_file: Some((0, "exec-shell.json".into())),
                home: None,
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
        .spawn_with_stdin_process(
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
        .spawn_with_stdin_process(
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

/// The host-owned helper is always confined where the platform can confine,
/// whatever the Process isolation switch says.
fn confined_platform(settings: ExecutionSettings) -> HostPlatform {
    HostPlatform::current(ShellStatus::Unavailable {
        code: "test",
        reason: String::new(),
    })
    .unwrap()
    .with_execution_settings(ExecutionSettingsHost::fixed(settings))
}

/// The user's Process isolation switch, off.
fn switch_off() -> ExecutionSettings {
    ExecutionSettings {
        sandbox: false,
        network: false,
    }
}

fn quote(path: &std::path::Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

fn unique(name: &str) -> String {
    format!("{name}-{}", uuid::Uuid::new_v4().simple())
}

/// A bash for the `/dev/tcp` loopback probe, which is not a POSIX feature:
/// `/bin/bash`, or `bash` on the captured `PATH`. A host without one skips
/// that probe.
fn bash() -> Option<PathBuf> {
    let bash = PathBuf::from("/bin/bash");
    if bash.is_file() {
        return Some(bash);
    }
    std::env::var_os("PATH").and_then(|entries| {
        std::env::split_paths(&entries)
            .map(|directory| directory.join("bash"))
            .find(|candidate| candidate.is_file())
    })
}

/// A perl for the `setpgrp` probe, from the captured `PATH`. A host without
/// one skips that probe.
fn perl() -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|entries| {
        std::env::split_paths(&entries)
            .map(|directory| directory.join("perl"))
            .find(|candidate| candidate.is_file())
    })
}

async fn confined(
    platform: &HostPlatform,
    script: &str,
    write: &std::path::Path,
    timeout: Duration,
) -> Result<ConfinedOutput, ConfinedError> {
    platform
        .spawn_confined_process(
            ConfinedSpec {
                executable: "/bin/sh".into(),
                args: vec!["-c".into(), script.into()],
                env: vec![],
            },
            Confinement {
                read: vec![],
                write: vec![write.to_owned()],
                timeout,
                stdout_limit: 64 * 1024,
                stderr_limit: 64 * 1024,
                services: SystemServices::None,
            },
            CancellationToken::new(),
        )
        .await
}

#[tokio::test]
async fn a_confined_helper_reads_its_grants_and_writes_only_its_directory() {
    let base = std::env::temp_dir().join(unique("dim-confined"));
    let read = base.join("read");
    let write = base.join("write");
    std::fs::create_dir_all(&read).unwrap();
    std::fs::create_dir_all(&write).unwrap();
    let input = read.join("input.txt");
    std::fs::write(&input, "confined-input").unwrap();
    let platform = confined_platform(switch_off());
    #[cfg(not(target_os = "macos"))]
    if !matches!(platform.sandbox_backend(), SandboxBackend::Available) {
        eprintln!("bubblewrap is not installed; the confined cases need it");
        std::fs::remove_dir_all(&base).unwrap();
        return;
    }

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let temp_escape = std::env::temp_dir().join(unique("dim-confined-escape"));
    let home_escape = unique("dim-confined-escape");
    let script = format!(
        "cat {input} > /dev/null && echo read=yes || echo read=no\n\
         printf '%s' \"$(cat {input})\" > {write}/result.txt && echo write=yes || echo write=no\n\
         echo x > {read}/escape.txt 2>/dev/null && echo read-side=yes || echo read-side=no\n\
         echo x > {temp} 2>/dev/null && echo temp=yes || echo temp=no\n\
         echo x > \"$HOME/{home_escape}\" 2>/dev/null && echo home=yes || echo home=no\n",
        input = quote(&input),
        write = quote(&write),
        read = quote(&read),
        temp = quote(&temp_escape),
    );
    let has_home = std::env::var_os("HOME").is_some();

    let mut control = platform
        .spawn_tree(ProcessSpec {
            executable: "/bin/sh".into(),
            args: vec!["-c".into(), script.clone().into()],
            cwd: write.clone(),
            env: vec![],
        })
        .unwrap();
    let mut control_output = String::new();
    control
        .stdout()
        .read_to_string(&mut control_output)
        .await
        .unwrap();
    assert!(control.wait().await.unwrap().success());
    for probe in ["read=yes", "write=yes", "read-side=yes", "temp=yes"] {
        assert!(
            control_output.contains(probe),
            "the unconfined control did not report {probe}: {control_output}"
        );
    }
    std::fs::remove_file(&temp_escape).unwrap();
    std::fs::remove_file(read.join("escape.txt")).unwrap();

    let output = platform
        .spawn_confined_process(
            ConfinedSpec {
                executable: "/bin/sh".into(),
                args: vec!["-c".into(), script.into()],
                env: vec![],
            },
            Confinement {
                read: vec![input.clone()],
                write: vec![write.clone()],
                timeout: Duration::from_secs(30),
                stdout_limit: 64 * 1024,
                stderr_limit: 64 * 1024,
                services: SystemServices::None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(output.confined, "the helper must report its confinement");
    assert!(
        output.status.success(),
        "{:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = String::from_utf8(output.stdout).unwrap();
    for probe in ["read=yes", "write=yes", "read-side=no", "temp=no"] {
        assert!(
            output.contains(probe),
            "the confined helper did not report {probe}: {output}"
        );
    }
    assert!(
        output.contains("home=no") || !has_home,
        "a confined helper must not write into the home: {output}"
    );
    assert_eq!(
        std::fs::read_to_string(write.join("result.txt")).unwrap(),
        "confined-input"
    );
    assert!(!read.join("escape.txt").exists());
    assert!(!temp_escape.exists());

    let Some(bash) = bash() else {
        eprintln!("bash is not installed; the loopback probe is skipped");
        std::fs::remove_dir_all(&base).unwrap();
        return;
    };
    let probe = format!(
        "if exec 3<>/dev/tcp/127.0.0.1/{port} 2>/dev/null; then echo tcp=yes; else echo tcp=no; fi\n"
    );
    let mut control = platform
        .spawn_tree(ProcessSpec {
            executable: bash.clone(),
            args: vec!["-c".into(), probe.clone().into()],
            cwd: write.clone(),
            env: vec![],
        })
        .unwrap();
    let mut control_output = String::new();
    control
        .stdout()
        .read_to_string(&mut control_output)
        .await
        .unwrap();
    assert!(control.wait().await.unwrap().success());
    assert!(
        control_output.contains("tcp=yes"),
        "the unconfined control did not reach the listener: {control_output}"
    );
    let output = platform
        .spawn_confined_process(
            ConfinedSpec {
                executable: bash,
                args: vec!["-c".into(), probe.into()],
                env: vec![],
            },
            Confinement {
                read: vec![],
                write: vec![write.clone()],
                timeout: Duration::from_secs(30),
                stdout_limit: 64 * 1024,
                stderr_limit: 64 * 1024,
                services: SystemServices::None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(output.confined, "the helper must report its confinement");
    assert!(output.status.success());
    let output = String::from_utf8(output.stdout).unwrap();
    assert!(
        output.contains("tcp=no"),
        "a confined helper reached a loopback listener: {output}"
    );
    std::fs::remove_dir_all(&base).unwrap();
}

#[tokio::test]
async fn a_confined_helper_timeout_kills_its_grandchild() {
    let base = std::env::temp_dir().join(unique("dim-confined-timeout"));
    let write = base.join("write");
    std::fs::create_dir_all(&write).unwrap();
    let pid_file = write.join("grandchild.pid");
    let script = format!("sleep 100 &\necho $! > {}\nwait\n", quote(&pid_file));
    let platform = confined_platform(switch_off());
    let result = confined(&platform, &script, &write, Duration::from_millis(300)).await;
    assert!(
        matches!(result, Err(ConfinedError::TimedOut)),
        "{:?}",
        result.err()
    );
    let pid: i32 = std::fs::read_to_string(&pid_file)
        .expect("the script records its grandchild")
        .trim()
        .parse()
        .unwrap();
    assert!(gone(pid).await, "the timeout left the grandchild {pid}");
    std::fs::remove_dir_all(&base).unwrap();
}

#[tokio::test]
async fn a_confined_helper_stdout_cap_kills_the_tree() {
    let base = std::env::temp_dir().join(unique("dim-confined-cap"));
    let write = base.join("write");
    std::fs::create_dir_all(&write).unwrap();
    let pid_file = write.join("grandchild.pid");
    let script = format!(
        "sleep 100 &\necho $! > {}\nwhile :; do echo 0123456789abcdef; done\n",
        quote(&pid_file)
    );
    let platform = confined_platform(switch_off());
    let result = platform
        .spawn_confined_process(
            ConfinedSpec {
                executable: "/bin/sh".into(),
                args: vec!["-c".into(), script.into()],
                env: vec![],
            },
            Confinement {
                read: vec![],
                write: vec![write.clone()],
                timeout: Duration::from_secs(30),
                stdout_limit: 1024,
                stderr_limit: 64 * 1024,
                services: SystemServices::None,
            },
            CancellationToken::new(),
        )
        .await;
    assert!(
        matches!(
            result,
            Err(ConfinedError::OutputLimit {
                stream: ConfinedStream::Stdout
            })
        ),
        "{:?}",
        result.err()
    );
    let pid: i32 = std::fs::read_to_string(&pid_file)
        .expect("the script records its grandchild")
        .trim()
        .parse()
        .unwrap();
    assert!(gone(pid).await, "the output cap left the grandchild {pid}");
    std::fs::remove_dir_all(&base).unwrap();
}

#[tokio::test]
async fn a_confined_helper_cancel_kills_the_tree() {
    let base = std::env::temp_dir().join(unique("dim-confined-cancel"));
    let write = base.join("write");
    std::fs::create_dir_all(&write).unwrap();
    let pid_file = write.join("grandchild.pid");
    let script = format!("sleep 100 &\necho $! > {}\nwait\n", quote(&pid_file));
    let platform = confined_platform(switch_off());
    let cancel = CancellationToken::new();
    let (result, (pid, cancelled_at)) = tokio::join!(
        platform.spawn_confined_process(
            ConfinedSpec {
                executable: "/bin/sh".into(),
                args: vec!["-c".into(), script.into()],
                env: vec![],
            },
            Confinement {
                read: vec![],
                write: vec![write.clone()],
                timeout: Duration::from_secs(30),
                stdout_limit: 64 * 1024,
                stderr_limit: 64 * 1024,
                services: SystemServices::None,
            },
            cancel.clone(),
        ),
        async {
            let pid = recorded_pid(&pid_file).await;
            let cancelled_at = std::time::Instant::now();
            cancel.cancel();
            (pid, cancelled_at)
        },
    );
    assert!(
        matches!(result, Err(ConfinedError::Cancelled)),
        "{:?}",
        result.err()
    );
    assert!(
        cancelled_at.elapsed() < Duration::from_secs(10),
        "cancel waited for the helper"
    );
    assert!(gone(pid).await, "cancel left the grandchild {pid}");
    std::fs::remove_dir_all(&base).unwrap();
}

#[tokio::test]
async fn a_confined_helper_refuses_relative_and_missing_paths() {
    let base = std::env::temp_dir().join(unique("dim-confined-invalid"));
    let write = base.join("write");
    std::fs::create_dir_all(&write).unwrap();
    let file = base.join("file");
    std::fs::write(&file, "x").unwrap();
    let platform = confined_platform(switch_off());
    let missing = base.join("missing");
    let cases = [
        Confinement {
            read: vec![PathBuf::from("relative.txt")],
            write: vec![write.clone()],
            timeout: Duration::from_secs(5),
            stdout_limit: 1024,
            stderr_limit: 1024,
            services: SystemServices::None,
        },
        Confinement {
            read: vec![missing],
            write: vec![write.clone()],
            timeout: Duration::from_secs(5),
            stdout_limit: 1024,
            stderr_limit: 1024,
            services: SystemServices::None,
        },
        Confinement {
            read: vec![],
            write: vec![file],
            timeout: Duration::from_secs(5),
            stdout_limit: 1024,
            stderr_limit: 1024,
            services: SystemServices::None,
        },
    ];
    for confinement in cases {
        let result = platform
            .spawn_confined_process(
                ConfinedSpec {
                    executable: "/bin/sh".into(),
                    args: vec!["-c".into(), "true".into()],
                    env: vec![],
                },
                confinement,
                CancellationToken::new(),
            )
            .await;
        assert!(
            matches!(result, Err(ConfinedError::InvalidPath { .. })),
            "{:?}",
            result.err()
        );
    }
    std::fs::remove_dir_all(&base).unwrap();
}

#[cfg(not(target_os = "macos"))]
#[tokio::test]
async fn a_host_without_bubblewrap_runs_the_confined_helper_unconfined() {
    let base = std::env::temp_dir().join(unique("dim-confined-unconfined"));
    let write = base.join("write");
    let bin = base.join("bin");
    std::fs::create_dir_all(&write).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    let mut platform = confined_platform(switch_off());
    platform.env = env(&[("PATH", bin.to_str().unwrap())]);
    let output = platform
        .spawn_confined_process(
            ConfinedSpec {
                executable: "/bin/sh".into(),
                args: vec!["-c".into(), "echo artifact > artifact.txt".into()],
                env: vec![],
            },
            Confinement {
                read: vec![],
                write: vec![write.clone()],
                timeout: Duration::from_secs(30),
                stdout_limit: 64 * 1024,
                stderr_limit: 64 * 1024,
                services: SystemServices::None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(output.status.success());
    assert!(!output.confined, "a host without bubblewrap cannot confine");
    assert_eq!(
        std::fs::read_to_string(write.join("artifact.txt")).unwrap(),
        "artifact\n"
    );
    std::fs::remove_dir_all(&base).unwrap();
}

#[tokio::test]
async fn the_isolation_switch_does_not_change_a_confined_helper() {
    let base = std::env::temp_dir().join(unique("dim-confined-switch"));
    let write = base.join("write");
    std::fs::create_dir_all(&write).unwrap();
    let platform = confined_platform(ExecutionSettings {
        sandbox: true,
        network: true,
    });
    #[cfg(not(target_os = "macos"))]
    if !matches!(platform.sandbox_backend(), SandboxBackend::Available) {
        eprintln!("bubblewrap is not installed; the confined cases need it");
        std::fs::remove_dir_all(&base).unwrap();
        return;
    }
    let Some(bash) = bash() else {
        eprintln!("bash is not installed; the loopback probe is skipped");
        std::fs::remove_dir_all(&base).unwrap();
        return;
    };
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let script = format!(
        "if exec 3<>/dev/tcp/127.0.0.1/{port} 2>/dev/null; then echo tcp=yes; else echo tcp=no; fi\n"
    );
    let output = platform
        .spawn_confined_process(
            ConfinedSpec {
                executable: bash,
                args: vec!["-c".into(), script.into()],
                env: vec![],
            },
            Confinement {
                read: vec![],
                write: vec![write.clone()],
                timeout: Duration::from_secs(30),
                stdout_limit: 64 * 1024,
                stderr_limit: 64 * 1024,
                services: SystemServices::None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(output.confined);
    let output = String::from_utf8(output.stdout).unwrap();
    assert!(
        output.contains("tcp=no"),
        "Sandboxed · Network allowed must not lift a confined helper's denial: {output}"
    );
    std::fs::remove_dir_all(&base).unwrap();
}

#[tokio::test]
async fn a_confined_helper_leaves_no_process_after_it_exits() {
    let base = std::env::temp_dir().join(unique("dim-confined-exit"));
    let write = base.join("write");
    std::fs::create_dir_all(&write).unwrap();
    let pid_file = write.join("grandchild.pid");
    let script = format!(
        "sleep 100 >/dev/null 2>&1 &\necho $! > {}\nexit 0\n",
        quote(&pid_file)
    );
    let platform = confined_platform(switch_off());
    let output = confined(&platform, &script, &write, Duration::from_secs(30))
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    let pid: i32 = std::fs::read_to_string(&pid_file)
        .expect("the script records its grandchild")
        .trim()
        .parse()
        .unwrap();
    assert!(gone(pid).await, "a finished helper left {pid} running");
    std::fs::remove_dir_all(&base).unwrap();
}

/// A descendant that keeps the write ends of both pipes must not keep a
/// finished helper waiting until its timeout.
#[tokio::test]
async fn a_confined_helper_returns_when_its_root_exits_with_a_descendant_on_its_pipes() {
    let base = std::env::temp_dir().join(unique("dim-confined-drain"));
    let write = base.join("write");
    std::fs::create_dir_all(&write).unwrap();
    let pid_file = write.join("grandchild.pid");
    let script = format!(
        "sleep 100 &\necho $! > {}\necho done\nexit 0\n",
        quote(&pid_file)
    );
    let platform = confined_platform(switch_off());
    let started = std::time::Instant::now();
    let output = confined(&platform, &script, &write, Duration::from_secs(30))
        .await
        .expect("the root exited at once");
    let elapsed = started.elapsed();
    assert!(
        output.status.success(),
        "{:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("done"), "{stdout:?}");
    assert!(
        elapsed < Duration::from_secs(5),
        "the call took {elapsed:?} for a root that exited at once"
    );
    let pid: i32 = std::fs::read_to_string(&pid_file)
        .expect("the script records its grandchild")
        .trim()
        .parse()
        .unwrap();
    assert!(gone(pid).await, "the finished helper left {pid} running");
    std::fs::remove_dir_all(&base).unwrap();
}

#[tokio::test]
async fn a_confined_helper_returns_when_an_escaped_descendant_holds_its_pipes() {
    let Some(perl) = perl() else {
        println!("skipping the escaped-descendant probe: perl is not on PATH");
        return;
    };
    let base = std::env::temp_dir().join(unique("dim-confined-escape"));
    let write = base.join("write");
    std::fs::create_dir_all(&write).unwrap();
    let pid_file = write.join("escapee.pid");
    let escape = format!(
        "use Fcntl qw(:flock); setpgrp(0, 0); open my $f, q(>), q({}) or die; \
         flock($f, LOCK_EX) or die; syswrite($f, $$) or die; sleep 100;",
        pid_file.display()
    );
    let script = format!(
        "{} -e '{}' &\nwhile [ ! -s {} ]; do sleep 0.01; done\necho done\nexit 0\n",
        quote(&perl),
        escape,
        quote(&pid_file)
    );
    let platform = confined_platform(switch_off());
    let started = std::time::Instant::now();
    let output = confined(&platform, &script, &write, Duration::from_secs(30))
        .await
        .expect("the root exited at once");
    let elapsed = started.elapsed();
    assert!(
        output.status.success(),
        "{:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("done"), "{stdout:?}");
    assert!(
        elapsed < Duration::from_secs(5),
        "the call took {elapsed:?} with the drain bound at one second"
    );
    #[cfg(target_os = "linux")]
    if output.confined {
        std::fs::File::open(&pid_file)
            .unwrap()
            .try_lock()
            .expect("the descendant must release its file lock when the PID namespace ends");
        std::fs::remove_dir_all(&base).unwrap();
        return;
    }
    let pid: i32 = std::fs::read_to_string(&pid_file)
        .expect("the escapee records its pid")
        .trim()
        .parse()
        .unwrap();
    assert!(
        alive(pid),
        "the escapee {pid} ended with the group, so this probe no longer proves the limit"
    );
    // SAFETY: the probe records the pid of the process it started, and the
    // kill ends that escaped process.
    let killed = unsafe { libc::kill(pid, libc::SIGKILL) };
    assert_eq!(killed, 0, "the escaped process {pid} could not be killed");
    assert!(gone(pid).await, "the escaped process {pid} survived a kill");
    std::fs::remove_dir_all(&base).unwrap();
}

#[tokio::test]
async fn a_confined_helper_stderr_cap_kills_the_tree() {
    let base = std::env::temp_dir().join(unique("dim-confined-stderr"));
    let write = base.join("write");
    std::fs::create_dir_all(&write).unwrap();
    let pid_file = write.join("grandchild.pid");
    let script = format!(
        "sleep 100 &\necho $! > {}\nwhile :; do echo 0123456789abcdef >&2; done\n",
        quote(&pid_file)
    );
    let platform = confined_platform(switch_off());
    let result = platform
        .spawn_confined_process(
            ConfinedSpec {
                executable: "/bin/sh".into(),
                args: vec!["-c".into(), script.into()],
                env: vec![],
            },
            Confinement {
                read: vec![],
                write: vec![write.clone()],
                timeout: Duration::from_secs(30),
                stdout_limit: 64 * 1024,
                stderr_limit: 1024,
                services: SystemServices::None,
            },
            CancellationToken::new(),
        )
        .await;
    assert!(
        matches!(
            result,
            Err(ConfinedError::OutputLimit {
                stream: ConfinedStream::Stderr
            })
        ),
        "{:?}",
        result.err()
    );
    let pid: i32 = std::fs::read_to_string(&pid_file)
        .expect("the script records its grandchild")
        .trim()
        .parse()
        .unwrap();
    assert!(gone(pid).await, "the stderr cap left the grandchild {pid}");
    std::fs::remove_dir_all(&base).unwrap();
}

#[tokio::test]
async fn a_confined_helper_gets_the_allowlist_plus_its_own_environment() {
    let base = std::env::temp_dir().join(unique("dim-confined-env"));
    let write = base.join("write");
    let temporary = write.join("tmp");
    std::fs::create_dir_all(&temporary).unwrap();
    let mut platform = confined_platform(switch_off());
    let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
    platform.env = env(&[
        ("PATH", &path),
        ("HOME", "/service-home"),
        ("LANG", "C"),
        ("DIM_CONFINED_FIXTURE_SECRET", "service-only"),
    ]);
    let mktemp = write.join("mktemp.txt");
    let script = format!(
        "env\nmktemp \"$TMPDIR/dim-helper.XXXXXX\" > {}\n",
        quote(&mktemp)
    );
    let output = platform
        .spawn_confined_process(
            ConfinedSpec {
                executable: "/bin/sh".into(),
                args: vec!["-c".into(), script.into()],
                env: env(&[
                    ("HELPER_ONLY", "1"),
                    ("HOME", write.to_str().unwrap()),
                    ("TMPDIR", temporary.to_str().unwrap()),
                ]),
            },
            Confinement {
                read: vec![],
                write: vec![write.clone()],
                timeout: Duration::from_secs(30),
                stdout_limit: 64 * 1024,
                stderr_limit: 64 * 1024,
                services: SystemServices::None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<&str> = output.lines().collect();
    assert!(
        !lines
            .iter()
            .any(|line| line.starts_with("DIM_CONFINED_FIXTURE_SECRET=")),
        "the service environment reached the helper: {output}"
    );
    assert!(
        lines.contains(&"HELPER_ONLY=1"),
        "the helper's own entry is missing: {output}"
    );
    let homes: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|line| line.starts_with("HOME="))
        .collect();
    let expected = format!("HOME={}", write.display());
    assert_eq!(homes, [expected.as_str()], "unexpected HOME: {output}");
    assert!(
        lines.contains(&format!("PATH={path}").as_str()),
        "PATH is not the captured value: {output}"
    );
    assert!(
        lines.contains(&"LANG=C"),
        "LANG is not the captured value: {output}"
    );
    let created = std::fs::read_to_string(&mktemp).expect("mktemp records its file");
    let created = PathBuf::from(created.trim());
    assert!(
        created.starts_with(&temporary),
        "{created:?} is not inside the helper's TMPDIR {temporary:?}"
    );
    assert!(created.exists());
    std::fs::remove_dir_all(&base).unwrap();
}

/// The directory an installed runtime lives in, from one `fixturetool` a test
/// writes there.
fn runtime_fixture(base: &std::path::Path, name: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = base.join(name);
    std::fs::create_dir_all(&bin).unwrap();
    let tool = bin.join("fixturetool");
    std::fs::write(&tool, "#!/bin/sh\necho fixture\n").unwrap();
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

/// A platform whose shell is `/bin/sh` and whose captured environment is
/// `env`, with `toolchains` as the runtimes Dim downloaded.
fn toolchain_platform(pairs: &[(&str, &str)], toolchains: Vec<InstalledToolchain>) -> HostPlatform {
    let mut platform = HostPlatform::current(ShellStatus::Available(ExecShellRuntime {
        executable_path: "/bin/sh".into(),
        edition: ShellEdition::Sh,
        source: ShellSource::Login,
        output_encoding: OutputEncoding::Utf8,
    }))
    .unwrap();
    platform.env = env(pairs);
    platform.with_toolchain_source(ToolchainSourceHost::fixed(toolchains))
}

/// An exec resolves a command only the toolchain source provides, and a
/// command already on the base PATH wins: the directories are appended after
/// it, never before.
#[tokio::test]
async fn an_exec_resolves_a_command_only_the_toolchain_source_provides() {
    let base = std::env::temp_dir().join(unique("dim-toolchain"));
    let bin = runtime_fixture(&base, "runtimes/node-v-test/bin");
    let installed = InstalledToolchain {
        bin: bin.clone(),
        root: bin.parent().unwrap().to_path_buf(),
    };
    let platform = toolchain_platform(&[("PATH", "/usr/bin:/bin")], vec![installed]);
    let (spec, _) = platform
        .build_process_spec("command -v fixturetool", &base)
        .await
        .unwrap();
    let mut tree = platform.spawn_tree(spec).unwrap();
    let mut output = String::new();
    tree.stdout().read_to_string(&mut output).await.unwrap();
    assert!(tree.wait().await.unwrap().success());
    assert_eq!(output.trim(), bin.join("fixturetool").to_str().unwrap());

    let shadow = runtime_fixture(&base, "shadow");
    let installed = InstalledToolchain {
        bin: bin.clone(),
        root: bin.parent().unwrap().to_path_buf(),
    };
    let platform = toolchain_platform(&[("PATH", shadow.to_str().unwrap())], vec![installed]);
    let (spec, _) = platform
        .build_process_spec("command -v fixturetool", &base)
        .await
        .unwrap();
    let mut tree = platform.spawn_tree(spec).unwrap();
    let mut output = String::new();
    tree.stdout().read_to_string(&mut output).await.unwrap();
    assert!(tree.wait().await.unwrap().success());
    assert_eq!(
        output.trim(),
        shadow.join("fixturetool").to_str().unwrap(),
        "a command on the base PATH is not replaced by the runtime's"
    );
    std::fs::remove_dir_all(base).unwrap();
}

fn never_cancelled() -> dim_agent_capabilities::host::CancelledFn {
    let never = CancellationToken::new();
    std::sync::Arc::new(move || {
        let never = never.clone();
        Box::pin(async move {
            never.cancelled().await;
            Ok(())
        })
    })
}

#[tokio::test]
async fn the_host_platform_service_answers_info() {
    let platform = HostPlatform::current(ShellStatus::Unavailable {
        code: "TEST",
        reason: "no shell".into(),
    })
    .unwrap();
    let root = rutis::Ctx::root_with(tokio::runtime::Handle::current());
    root.provide_as::<HostPlatform>(std::sync::Arc::new(platform.clone()))
        .unwrap();
    let face = root
        .get_at::<dyn rutis_bridge::session::HostDispatch>(&rutis_bridge::session::host_key(
            "hostPlatform",
        ))
        .expect("hostPlatform is served");
    let reply =
        rutis_bridge::session::settle(face.invoke("info", serde_json::json!([]).into()).unwrap())
            .await
            .unwrap()
            .json()
            .unwrap();
    assert_eq!(
        reply,
        serde_json::to_value(platform.info().await.unwrap()).unwrap()
    );
}

#[test]
fn public_output_has_no_event_fields() {
    assert!(
        serde_json::from_value::<OutputRequest>(serde_json::json!({
            "taskId": "task",
            "eventCursor": 0
        }))
        .is_err()
    );
    let reply = serde_json::to_value(OutputPageReply {
        text: "done".into(),
        bytes: 4,
        total_bytes: 4,
        next_cursor: None,
        truncated: false,
        retained_from: 0,
        reset: None,
    })
    .unwrap();
    assert!(reply.get("events").is_none());
    assert!(reply.get("nextEventCursor").is_none());
    assert!(reply.get("eventRetainedFrom").is_none());
    assert!(reply.get("state").is_none());
}

#[test]
fn task_failure_serializes_with_its_code() {
    assert_eq!(
        serde_json::to_value(TaskFailure::OutputWriteFailed {
            message: "disk full".into()
        })
        .unwrap(),
        serde_json::json!({"code": "output_write_failed", "message": "disk full"})
    );
    assert_eq!(
        serde_json::to_value(TaskFailure::ShellExited).unwrap(),
        serde_json::json!({"code": "shell_exited"})
    );
}

#[tokio::test]
async fn the_host_platform_service_starts_and_waits_for_a_command() {
    let dir = std::env::temp_dir().join(unique("dim-host-service"));
    std::fs::create_dir_all(&dir).unwrap();
    let root = rutis::Ctx::root_with(tokio::runtime::Handle::current());
    let catalog = dim_agent_capabilities::catalog();
    let build = Default::default();
    let names = dim_agent_runtime::Names::copy(&catalog, &build);
    let _execution = root
        .provide(ExecutionSettingsHost::fixed(Default::default()))
        .unwrap();
    let _toolchain = root.provide(ToolchainSourceHost::fixed(vec![])).unwrap();
    let plugin = root.plugin(
        HostPlatformPlugin::new(
            &names,
            HostPlatformConfig {
                execution: names.key().unwrap(),
                toolchain: names.key().unwrap(),
                exec_shell_file: None,
                home: Some(dir.clone()),
            },
        )
        .unwrap(),
    );
    (&plugin).await.unwrap();
    let platform = root.get::<HostPlatform>().unwrap();
    let face = root
        .get_at::<dyn rutis_bridge::session::HostDispatch>(&rutis_bridge::session::host_key(
            "hostPlatform",
        ))
        .unwrap();
    let invoke = |method: &'static str, request: serde_json::Value| {
        let face = face.clone();
        async move {
            rutis_bridge::session::settle(
                face.invoke(method, serde_json::json!([request]).into())
                    .unwrap(),
            )
            .await?
            .json()
        }
    };
    let path = dir.to_string_lossy().into_owned();
    let spawn = invoke(
        "spawn",
        serde_json::json!({
            "taskId": "service-task",
            "command": "printf hi",
            "cwd": path,
            "workspace": path,
            "roots": [],
            "outputDir": path,
            "timeoutMs": 10000
        }),
    )
    .await
    .unwrap();
    assert_eq!(spawn["taskId"], "service-task");
    let terminal = serde_json::to_value(
        platform
            .wait(
                WaitRequest {
                    task_id: "service-task".into(),
                    timeout_ms: None,
                },
                never_cancelled(),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(terminal["status"], "exited");
    assert_eq!(terminal["text"], "hi");
    assert!(terminal.get("failure").is_none());
    assert_eq!(terminal["preview"], "hi");
    assert_eq!(terminal["outputPath"], spawn["outputPath"]);
    let page = invoke(
        "output",
        serde_json::json!({"taskId": "service-task", "limit": 16}),
    )
    .await
    .unwrap();
    assert_eq!(page["text"], "hi");
    invoke("settle", serde_json::json!({"taskId": "service-task"}))
        .await
        .unwrap();
    let missing = platform
        .wait(
            WaitRequest {
                task_id: "service-task".into(),
                timeout_ms: None,
            },
            never_cancelled(),
        )
        .await
        .unwrap_err();
    assert!(matches!(missing, HostPlatformError::TaskUnknown));

    platform
        .spawn(SpawnRequest {
            task_id: "long-task".into(),
            command: "seq 1 2500".into(),
            cwd: path.clone(),
            workspace: path.clone(),
            roots: Vec::new(),
            output_dir: path.clone(),
            timeout_ms: Some(10_000),
        })
        .await
        .unwrap();
    let long = platform
        .wait(
            WaitRequest {
                task_id: "long-task".into(),
                timeout_ms: None,
            },
            never_cancelled(),
        )
        .await
        .unwrap();
    assert_eq!(long.status, WaitStatus::Exited);
    let text = long.text.expect("an inline task keeps its complete text");
    assert_eq!(text.lines().count(), 2_500);
    assert!(long.preview.lines().count() <= 2_000);
    assert!(long.preview.lines().count() < text.lines().count());
    let retained = platform
        .retain_output(TaskRequest {
            task_id: "long-task".into(),
        })
        .await
        .unwrap();
    let file = std::fs::read_to_string(&retained.output_path).unwrap();
    assert_eq!(file, text);
    let page = platform
        .output(OutputRequest {
            task_id: "long-task".into(),
            cursor: None,
            limit: Some(64 * 1024),
        })
        .await
        .unwrap();
    assert_eq!(page.text, text);
    platform
        .settle(TaskRequest {
            task_id: "long-task".into(),
        })
        .await
        .unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn the_host_platform_service_serves_a_shell() {
    let dir = std::env::temp_dir().join(unique("dim-host-shell"));
    std::fs::create_dir_all(&dir).unwrap();
    let root = rutis::Ctx::root_with(tokio::runtime::Handle::current());
    let catalog = dim_agent_capabilities::catalog();
    let build = Default::default();
    let names = dim_agent_runtime::Names::copy(&catalog, &build);
    let _execution = root
        .provide(ExecutionSettingsHost::fixed(Default::default()))
        .unwrap();
    let _toolchain = root.provide(ToolchainSourceHost::fixed(vec![])).unwrap();
    let plugin = root.plugin(
        HostPlatformPlugin::new(
            &names,
            HostPlatformConfig {
                execution: names.key().unwrap(),
                toolchain: names.key().unwrap(),
                exec_shell_file: None,
                home: Some(dir.clone()),
            },
        )
        .unwrap(),
    );
    (&plugin).await.unwrap();
    let platform = root.get::<HostPlatform>().unwrap();
    let face = root
        .get_at::<dyn rutis_bridge::session::HostDispatch>(&rutis_bridge::session::host_key(
            "hostPlatform",
        ))
        .unwrap();
    let invoke = |method: &'static str, request: serde_json::Value| {
        let face = face.clone();
        async move {
            rutis_bridge::session::settle(
                face.invoke(method, serde_json::json!([request]).into())
                    .unwrap(),
            )
            .await?
            .json()
        }
    };
    let path = dir.to_string_lossy().into_owned();
    invoke(
        "shell",
        serde_json::json!({
            "taskId": "service-shell",
            "cwd": path,
            "workspace": path,
            "roots": [],
            "outputDir": path,
            "cols": 80,
            "rows": 24
        }),
    )
    .await
    .unwrap();
    invoke(
        "write",
        serde_json::json!({"taskId": "service-shell", "data": "cHJpbnRmIGhpOyBleGl0IDcK"}),
    )
    .await
    .unwrap();
    let terminal = platform
        .wait(
            WaitRequest {
                task_id: "service-shell".into(),
                timeout_ms: Some(10000),
            },
            never_cancelled(),
        )
        .await
        .unwrap();
    assert_eq!(terminal.status, WaitStatus::Exited);
    assert_eq!(terminal.exit_code, Some(7));
    assert!(terminal.preview.contains("hi"));
    assert!(std::path::Path::new(&terminal.output_path).exists());
    assert!(
        std::fs::read_to_string(&terminal.output_path)
            .unwrap()
            .contains("hi")
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&terminal.output_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let page = invoke(
        "output",
        serde_json::json!({"taskId": "service-shell", "limit": 4096}),
    )
    .await
    .unwrap();
    assert!(page["text"].as_str().unwrap().contains("hi"));
    assert!(matches!(
        platform
            .write(ShellWrite {
                task_id: "service-shell".into(),
                data: String::new(),
                command: None,
                job: None,
            })
            .await,
        Err(HostPlatformError::TaskEnded)
    ));
    invoke("settle", serde_json::json!({"taskId": "service-shell"}))
        .await
        .unwrap();
    platform
        .settle(TaskRequest {
            task_id: "service-shell".into(),
        })
        .await
        .unwrap();
    assert!(std::path::Path::new(&terminal.output_path).exists());
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn the_host_platform_service_runs_a_command_with_stdin() {
    let dir = std::env::temp_dir().join(unique("dim-host-stdin"));
    std::fs::create_dir_all(&dir).unwrap();
    let root = rutis::Ctx::root_with(tokio::runtime::Handle::current());
    let catalog = dim_agent_capabilities::catalog();
    let build = Default::default();
    let names = dim_agent_runtime::Names::copy(&catalog, &build);
    let _execution = root
        .provide(ExecutionSettingsHost::fixed(Default::default()))
        .unwrap();
    let _toolchain = root.provide(ToolchainSourceHost::fixed(vec![])).unwrap();
    let plugin = root.plugin(
        HostPlatformPlugin::new(
            &names,
            HostPlatformConfig {
                execution: names.key().unwrap(),
                toolchain: names.key().unwrap(),
                exec_shell_file: None,
                home: Some(dir.clone()),
            },
        )
        .unwrap(),
    );
    (&plugin).await.unwrap();
    let platform = root.get::<HostPlatform>().unwrap();
    let never = CancellationToken::new();
    let cancelled: dim_agent_capabilities::host::CancelledFn = std::sync::Arc::new(move || {
        let never = never.clone();
        Box::pin(async move {
            never.cancelled().await;
            Ok(())
        })
    });
    let reply = platform
        .spawn_with_stdin(
            SpawnWithStdinRequest {
                command: "cat".into(),
                cwd: path_string(&dir),
                env: Default::default(),
                stdin: Some("aGk=".into()),
                timeout_ms: 10000,
                limits: OutputLimits {
                    stdout: 64 * 1024,
                    stderr: 64 * 1024,
                },
            },
            cancelled,
        )
        .await
        .unwrap();
    assert_eq!(reply.status, SpawnWithStdinStatus::Exited);
    assert_eq!(reply.exit_code, Some(0));
    assert_eq!(reply.stdout, "aGk=");
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn the_host_platform_service_runs_a_confined_helper() {
    let dir = std::env::temp_dir().join(unique("dim-host-confined"));
    std::fs::create_dir_all(&dir).unwrap();
    let root = rutis::Ctx::root_with(tokio::runtime::Handle::current());
    let catalog = dim_agent_capabilities::catalog();
    let build = Default::default();
    let names = dim_agent_runtime::Names::copy(&catalog, &build);
    let _execution = root
        .provide(ExecutionSettingsHost::fixed(Default::default()))
        .unwrap();
    let _toolchain = root.provide(ToolchainSourceHost::fixed(vec![])).unwrap();
    let plugin = root.plugin(
        HostPlatformPlugin::new(
            &names,
            HostPlatformConfig {
                execution: names.key().unwrap(),
                toolchain: names.key().unwrap(),
                exec_shell_file: None,
                home: Some(dir.clone()),
            },
        )
        .unwrap(),
    );
    (&plugin).await.unwrap();
    let platform = root.get::<HostPlatform>().unwrap();
    let never = CancellationToken::new();
    let cancelled: dim_agent_capabilities::host::CancelledFn = std::sync::Arc::new(move || {
        let never = never.clone();
        Box::pin(async move {
            never.cancelled().await;
            Ok(())
        })
    });
    let reply = platform
        .spawn_confined(
            ConfinedRequest {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), "printf hi; printf err >&2".into()],
                read: vec![],
                write: vec![path_string(&dir)],
                env: Default::default(),
                timeout_ms: 10000,
                output_limit: OutputLimits {
                    stdout: 64 * 1024,
                    stderr: 64 * 1024,
                },
                services: dim_agent_capabilities::host_platform::SystemServices::None,
            },
            cancelled,
        )
        .await
        .unwrap();
    assert_eq!(reply.status, Some(0));
    assert_eq!(reply.stdout, "aGk=");
    assert_eq!(reply.stderr, "ZXJy");
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn the_host_platform_service_removes_output_and_composes_a_path() {
    let dir = std::env::temp_dir().join(unique("dim-host-remove"));
    std::fs::create_dir_all(&dir).unwrap();
    let root = rutis::Ctx::root_with(tokio::runtime::Handle::current());
    let catalog = dim_agent_capabilities::catalog();
    let build = Default::default();
    let names = dim_agent_runtime::Names::copy(&catalog, &build);
    let _execution = root
        .provide(ExecutionSettingsHost::fixed(Default::default()))
        .unwrap();
    let _toolchain = root.provide(ToolchainSourceHost::fixed(vec![])).unwrap();
    let plugin = root.plugin(
        HostPlatformPlugin::new(
            &names,
            HostPlatformConfig {
                execution: names.key().unwrap(),
                toolchain: names.key().unwrap(),
                exec_shell_file: None,
                home: Some(dir.clone()),
            },
        )
        .unwrap(),
    );
    (&plugin).await.unwrap();
    let platform = root.get::<HostPlatform>().unwrap();
    let output = dir.join(".dim/exec/session/branch");
    std::fs::create_dir_all(&output).unwrap();
    std::fs::write(output.join("command.log"), b"output").unwrap();
    platform
        .remove_output_directory(RemoveOutputDirectoryRequest {
            path: path_string(&output),
        })
        .await
        .unwrap();
    assert!(!output.exists());
    let bin = dir.join("runtimes/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let with_toolchain = toolchain_platform(
        &[("PATH", "/bin")],
        vec![InstalledToolchain {
            bin,
            root: dir.join("runtimes"),
        }],
    );
    let path = with_toolchain
        .child_path(ChildPathRequest {
            directories: vec!["/opt/dim".into()],
            base: "/bin".into(),
        })
        .unwrap();
    assert!(path.contains("/opt/dim"));
    assert!(path.contains("/bin"));
    let refused = platform
        .remove_output_directory(RemoveOutputDirectoryRequest {
            path: path_string(dir.join(".dim/exec")),
        })
        .await
        .unwrap_err();
    assert!(matches!(refused, HostPlatformError::Invalid(_)));
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn a_wait_timeout_leaves_the_command_running() {
    let dir = std::env::temp_dir().join(unique("dim-host-wait-timeout"));
    std::fs::create_dir_all(&dir).unwrap();
    let root = rutis::Ctx::root_with(tokio::runtime::Handle::current());
    let catalog = dim_agent_capabilities::catalog();
    let build = Default::default();
    let names = dim_agent_runtime::Names::copy(&catalog, &build);
    let _execution = root
        .provide(ExecutionSettingsHost::fixed(Default::default()))
        .unwrap();
    let _toolchain = root.provide(ToolchainSourceHost::fixed(vec![])).unwrap();
    let plugin = root.plugin(
        HostPlatformPlugin::new(
            &names,
            HostPlatformConfig {
                execution: names.key().unwrap(),
                toolchain: names.key().unwrap(),
                exec_shell_file: None,
                home: Some(dir.clone()),
            },
        )
        .unwrap(),
    );
    (&plugin).await.unwrap();
    let platform = root.get::<HostPlatform>().unwrap();
    let path = dir.to_string_lossy().into_owned();
    let spawn: serde_json::Value = serde_json::to_value(
        platform
            .spawn(SpawnRequest {
                task_id: "wait-timeout".into(),
                command: "sleep 0.3".into(),
                cwd: path_string(&dir),
                workspace: path_string(&dir),
                roots: Vec::new(),
                output_dir: path_string(&dir),
                timeout_ms: None,
            })
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(spawn["taskId"], "wait-timeout");
    let first = platform
        .wait(
            WaitRequest {
                task_id: "wait-timeout".into(),
                timeout_ms: Some(10),
            },
            never_cancelled(),
        )
        .await
        .unwrap();
    assert_eq!(first.status, WaitStatus::Running);
    let second = platform
        .wait(
            WaitRequest {
                task_id: "wait-timeout".into(),
                timeout_ms: None,
            },
            never_cancelled(),
        )
        .await
        .unwrap();
    assert_eq!(second.status, WaitStatus::Exited);
    platform
        .settle(TaskRequest {
            task_id: "wait-timeout".into(),
        })
        .await
        .unwrap();
    let _ = path;
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn output_removal_refuses_traversal_and_links() {
    let dir = std::env::temp_dir().join(unique("dim-host-remove-safety"));
    std::fs::create_dir_all(&dir).unwrap();
    let root = rutis::Ctx::root_with(tokio::runtime::Handle::current());
    let catalog = dim_agent_capabilities::catalog();
    let build = Default::default();
    let names = dim_agent_runtime::Names::copy(&catalog, &build);
    let _execution = root
        .provide(ExecutionSettingsHost::fixed(Default::default()))
        .unwrap();
    let _toolchain = root.provide(ToolchainSourceHost::fixed(vec![])).unwrap();
    let plugin = root.plugin(
        HostPlatformPlugin::new(
            &names,
            HostPlatformConfig {
                execution: names.key().unwrap(),
                toolchain: names.key().unwrap(),
                exec_shell_file: None,
                home: Some(dir.clone()),
            },
        )
        .unwrap(),
    );
    (&plugin).await.unwrap();
    let platform = root.get::<HostPlatform>().unwrap();
    let exec_root = dir.join(".dim/exec");
    let outside = dir.join("outside");
    std::fs::create_dir_all(exec_root.join("session/branch")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("keep"), b"keep").unwrap();
    let traversal = exec_root.join("session/branch/../../outside");
    let error = platform
        .remove_output_directory(RemoveOutputDirectoryRequest {
            path: path_string(traversal),
        })
        .await
        .unwrap_err();
    assert!(matches!(error, HostPlatformError::Invalid(_)));
    assert!(outside.join("keep").exists());
    let root_error = platform
        .remove_output_directory(RemoveOutputDirectoryRequest {
            path: path_string(&exec_root),
        })
        .await
        .unwrap_err();
    assert!(matches!(root_error, HostPlatformError::Invalid(_)));
    #[cfg(unix)]
    {
        let link = exec_root.join("session/link");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let link_error = platform
            .remove_output_directory(RemoveOutputDirectoryRequest {
                path: path_string(link),
            })
            .await
            .unwrap_err();
        assert!(matches!(link_error, HostPlatformError::Invalid(_)));
        assert!(outside.join("keep").exists());
        let real = exec_root.join("session/real");
        std::fs::create_dir_all(real.join("child")).unwrap();
        let alias = exec_root.join("session/alias");
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        let component = platform
            .remove_output_directory(RemoveOutputDirectoryRequest {
                path: path_string(alias.join("child")),
            })
            .await
            .unwrap_err();
        assert!(matches!(component, HostPlatformError::Invalid(_)));
        assert!(real.join("child").exists());
    }
    platform
        .remove_output_directory(RemoveOutputDirectoryRequest {
            path: path_string(exec_root.join("session/absent")),
        })
        .await
        .unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn remove_output_directory_accepts_a_symlinked_home() {
    let dir = std::env::temp_dir().join(unique("dim-host-remove-symlink"));
    let home = dir.join("real-home");
    std::fs::create_dir_all(&home).unwrap();
    let alias = dir.join("alias-home");
    std::os::unix::fs::symlink(&home, &alias).unwrap();
    let root = rutis::Ctx::root_with(tokio::runtime::Handle::current());
    let catalog = dim_agent_capabilities::catalog();
    let build = Default::default();
    let names = dim_agent_runtime::Names::copy(&catalog, &build);
    let _execution = root
        .provide(ExecutionSettingsHost::fixed(Default::default()))
        .unwrap();
    let _toolchain = root.provide(ToolchainSourceHost::fixed(vec![])).unwrap();
    let plugin = root.plugin(
        HostPlatformPlugin::new(
            &names,
            HostPlatformConfig {
                execution: names.key().unwrap(),
                toolchain: names.key().unwrap(),
                exec_shell_file: None,
                home: Some(alias.clone()),
            },
        )
        .unwrap(),
    );
    (&plugin).await.unwrap();
    let platform = root.get::<HostPlatform>().unwrap();
    let output = alias.join(".dim/exec/session/branch");
    std::fs::create_dir_all(&output).unwrap();
    platform
        .remove_output_directory(RemoveOutputDirectoryRequest {
            path: path_string(&output),
        })
        .await
        .unwrap();
    assert!(!output.exists());
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn a_shell_burst_completes_without_a_private_subscriber() {
    let dir = std::env::temp_dir().join(unique("dim-host-burst"));
    std::fs::create_dir_all(&dir).unwrap();
    let root = rutis::Ctx::root_with(tokio::runtime::Handle::current());
    let catalog = dim_agent_capabilities::catalog();
    let build = Default::default();
    let names = dim_agent_runtime::Names::copy(&catalog, &build);
    let _execution = root
        .provide(ExecutionSettingsHost::fixed(Default::default()))
        .unwrap();
    let _toolchain = root.provide(ToolchainSourceHost::fixed(vec![])).unwrap();
    let plugin = root.plugin(
        HostPlatformPlugin::new(
            &names,
            HostPlatformConfig {
                execution: names.key().unwrap(),
                toolchain: names.key().unwrap(),
                exec_shell_file: None,
                home: Some(dir.clone()),
            },
        )
        .unwrap(),
    );
    (&plugin).await.unwrap();
    let platform = root.get::<HostPlatform>().unwrap();
    platform
        .shell(ShellRequest {
            task_id: "burst-shell".into(),
            cwd: path_string(&dir),
            workspace: path_string(&dir),
            roots: Vec::new(),
            output_dir: path_string(&dir),
            cols: 80,
            rows: 24,
        })
        .await
        .unwrap();
    let token = format!("BURST-{}", uuid::Uuid::new_v4().simple());
    std::fs::write(dir.join("burst-token"), &token).unwrap();
    let burst = format!(
        "printf '%s%s\\n' '__DIM_START_aaaabbbb_' 'ccccdddd__'; seq 1 400000; printf 'OUTPUT-'; cat '{}'; printf '\\n'; printf '%s%s%d\\n' '__DIM_END_eeeefaff_' '00001111__' 0",
        dir.join("burst-token").display()
    ) + "\n";
    platform
        .write(ShellWrite {
            task_id: "burst-shell".into(),
            data: base64::engine::general_purpose::STANDARD.encode(burst.as_bytes()),
            command: Some(ShellCommandStart {
                task_id: "burst-command".into(),
                start_marker: "__DIM_START_aaaabbbb_ccccdddd__".into(),
                end_marker: "__DIM_END_eeeefaff_00001111__".into(),
            }),
            job: None,
        })
        .await
        .unwrap();
    let mut seen = false;
    let mut last = String::new();
    for _ in 0..6000 {
        let page = platform
            .output(OutputRequest {
                task_id: "burst-shell".into(),
                cursor: None,
                limit: Some(64 * 1024),
            })
            .await
            .unwrap();
        last = page.text;
        if last.contains(&token) {
            seen = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        seen,
        "the shell output never reached the public page: {last:?}"
    );
    let terminal = platform
        .wait(
            WaitRequest {
                task_id: "burst-command".into(),
                timeout_ms: Some(30_000),
            },
            never_cancelled(),
        )
        .await
        .unwrap();
    assert_eq!(terminal.status, WaitStatus::Exited);
    platform
        .settle(TaskRequest {
            task_id: "burst-command".into(),
        })
        .await
        .unwrap();
    platform
        .kill(TaskRequest {
            task_id: "burst-shell".into(),
        })
        .await
        .unwrap();
    platform
        .settle(TaskRequest {
            task_id: "burst-shell".into(),
        })
        .await
        .unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn a_shell_command_capture_is_owned_by_the_host() {
    let dir = std::env::temp_dir().join(unique("dim-host-command-capture"));
    std::fs::create_dir_all(&dir).unwrap();
    let root = rutis::Ctx::root_with(tokio::runtime::Handle::current());
    let catalog = dim_agent_capabilities::catalog();
    let build = Default::default();
    let names = dim_agent_runtime::Names::copy(&catalog, &build);
    let _execution = root
        .provide(ExecutionSettingsHost::fixed(Default::default()))
        .unwrap();
    let _toolchain = root.provide(ToolchainSourceHost::fixed(vec![])).unwrap();
    let plugin = root.plugin(
        HostPlatformPlugin::new(
            &names,
            HostPlatformConfig {
                execution: names.key().unwrap(),
                toolchain: names.key().unwrap(),
                exec_shell_file: None,
                home: Some(dir.clone()),
            },
        )
        .unwrap(),
    );
    (&plugin).await.unwrap();
    let platform = root.get::<HostPlatform>().unwrap();
    platform
        .shell(ShellRequest {
            task_id: "capture-shell".into(),
            cwd: path_string(&dir),
            workspace: path_string(&dir),
            roots: Vec::new(),
            output_dir: path_string(&dir),
            cols: 80,
            rows: 24,
        })
        .await
        .unwrap();
    platform
        .write(ShellWrite {
            task_id: "capture-shell".into(),
            data: "cHJpbnRmICclcyVzXG4nICdfX0RJTV9TVEFSVF9hYWFhYmJiYl8nICdjY2NjZGRkZF9fJzsgcHJpbnRmICclcyVzJWRcbicgJ19fRElNX0VORF8xMTExMjIyMl8nICczMzMzNDQ0NF9fJyAwOyBwcmludGYgaGk7IHByaW50ZiAnJXMlcyVkXG4nICdfX0RJTV9FTkRfZWVlZWZmZmZfJyAnMDAwMDExMTFfXycgMAo=".into(),
            command: Some(ShellCommandStart {
                task_id: "owner-command".into(),
                start_marker: "__DIM_START_aaaabbbb_ccccdddd__".into(),
                end_marker: "__DIM_END_eeeeffff_00001111__".into(),
            }),
            job: None,
        })
        .await
        .unwrap();
    let finished = platform
        .wait(
            WaitRequest {
                task_id: "owner-command".into(),
                timeout_ms: Some(10_000),
            },
            never_cancelled(),
        )
        .await
        .unwrap();
    assert_eq!(finished.status, WaitStatus::Exited);
    let page = platform
        .output(OutputRequest {
            task_id: "owner-command".into(),
            cursor: None,
            limit: Some(4096),
        })
        .await
        .unwrap();
    assert!(page.text.contains("hi"), "{:?}", page.text);
    assert!(!page.text.contains("11112222"), "{:?}", page.text);
    let tail = platform
        .tail(TaskRequest {
            task_id: "owner-command".into(),
        })
        .await
        .unwrap();
    assert!(tail.text.contains("hi"), "{:?}", tail.text);
    let retained = platform
        .retain_output(TaskRequest {
            task_id: "owner-command".into(),
        })
        .await
        .unwrap();
    assert!(retained.output_path.ends_with(".log"));
    platform
        .settle(TaskRequest {
            task_id: "owner-command".into(),
        })
        .await
        .unwrap();
    let failing = "printf '%s%s' '__DIM_START_aaaabbbb_' 'ccccdddd__'; false; printf '%s%s%d\\n' '__DIM_END_eeeeffff_' '00001111__' 3\n";
    platform
        .write(ShellWrite {
            task_id: "capture-shell".into(),
            data: base64::engine::general_purpose::STANDARD.encode(failing.as_bytes()),
            command: Some(ShellCommandStart {
                task_id: "failing-command".into(),
                start_marker: "__DIM_START_aaaabbbb_ccccdddd__".into(),
                end_marker: "__DIM_END_eeeeffff_00001111__".into(),
            }),
            job: None,
        })
        .await
        .unwrap();
    let failed = platform
        .wait(
            WaitRequest {
                task_id: "failing-command".into(),
                timeout_ms: Some(10_000),
            },
            never_cancelled(),
        )
        .await
        .unwrap();
    assert_eq!(failed.status, WaitStatus::Failed);
    assert_eq!(failed.exit_code, Some(3));
    assert_eq!(failed.failure, Some(TaskFailure::CommandFailed));
    assert_eq!(failed.text, None);
    platform
        .settle(TaskRequest {
            task_id: "failing-command".into(),
        })
        .await
        .unwrap();
    let capped = "printf '%s%s' '__DIM_START_22223333_' '44445555__'; head -c 65533 /dev/zero | tr '\\0' a; printf '\\n\\303\\251\\n'; printf '%s%s%d\\n' '__DIM_END_66667777_' '88889999__' 0\n";
    platform
        .write(ShellWrite {
            task_id: "capture-shell".into(),
            data: base64::engine::general_purpose::STANDARD.encode(capped.as_bytes()),
            command: Some(ShellCommandStart {
                task_id: "capped-command".into(),
                start_marker: "__DIM_START_22223333_44445555__".into(),
                end_marker: "__DIM_END_66667777_88889999__".into(),
            }),
            job: None,
        })
        .await
        .unwrap();
    let capped_end = platform
        .wait(
            WaitRequest {
                task_id: "capped-command".into(),
                timeout_ms: Some(10_000),
            },
            never_cancelled(),
        )
        .await
        .unwrap();
    assert_eq!(capped_end.status, WaitStatus::Exited);
    let inside = platform
        .output(OutputRequest {
            task_id: "capped-command".into(),
            cursor: Some(65_535),
            limit: Some(16),
        })
        .await
        .unwrap();
    assert!(inside.text.is_empty(), "{:?}", inside.text);
    assert_eq!(inside.bytes, 0);
    assert_eq!(inside.total_bytes, capped_end.output_bytes);
    assert!(inside.next_cursor.is_none());
    let whole = platform
        .output(OutputRequest {
            task_id: "capped-command".into(),
            cursor: Some(65_534),
            limit: Some(16),
        })
        .await
        .unwrap();
    assert_eq!(whole.text, "é");
    assert_eq!(whole.total_bytes, capped_end.output_bytes);
    assert_eq!(whole.bytes, 2);
    platform
        .settle(TaskRequest {
            task_id: "capped-command".into(),
        })
        .await
        .unwrap();
    let long = "printf '%s%s\\n' '__DIM_START_aaaabbbb_' 'ccccdddd__'; sleep 30\n";
    platform
        .write(ShellWrite {
            task_id: "capture-shell".into(),
            data: base64::engine::general_purpose::STANDARD.encode(long.as_bytes()),
            command: Some(ShellCommandStart {
                task_id: "long-command".into(),
                start_marker: "__DIM_START_aaaabbbb_ccccdddd__".into(),
                end_marker: "__DIM_END_eeeefaff_00001111__".into(),
            }),
            job: None,
        })
        .await
        .unwrap();
    let second = platform
        .write(ShellWrite {
            task_id: "capture-shell".into(),
            data: base64::engine::general_purpose::STANDARD.encode(b"echo second\n"),
            command: Some(ShellCommandStart {
                task_id: "second-command".into(),
                start_marker: "__DIM_START_11112222_33334444__".into(),
                end_marker: "__DIM_END_55556666_77778888__".into(),
            }),
            job: None,
        })
        .await;
    assert!(matches!(second, Err(HostPlatformError::TaskRunning)));
    let retained = platform
        .retain_output(TaskRequest {
            task_id: "capture-shell".into(),
        })
        .await
        .unwrap();
    platform
        .kill(TaskRequest {
            task_id: "capture-shell".into(),
        })
        .await
        .unwrap();
    platform
        .settle(TaskRequest {
            task_id: "capture-shell".into(),
        })
        .await
        .unwrap();
    assert!(std::path::Path::new(&retained.output_path).exists());
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn a_job_write_terminalizes_and_reads_its_output() {
    let dir = std::env::temp_dir().join(unique("dim-host-job"));
    std::fs::create_dir_all(&dir).unwrap();
    let root = rutis::Ctx::root_with(tokio::runtime::Handle::current());
    let catalog = dim_agent_capabilities::catalog();
    let build = Default::default();
    let names = dim_agent_runtime::Names::copy(&catalog, &build);
    let _execution = root
        .provide(ExecutionSettingsHost::fixed(Default::default()))
        .unwrap();
    let _toolchain = root.provide(ToolchainSourceHost::fixed(vec![])).unwrap();
    let plugin = root.plugin(
        HostPlatformPlugin::new(
            &names,
            HostPlatformConfig {
                execution: names.key().unwrap(),
                toolchain: names.key().unwrap(),
                exec_shell_file: None,
                home: Some(dir.clone()),
            },
        )
        .unwrap(),
    );
    (&plugin).await.unwrap();
    let platform = root.get::<HostPlatform>().unwrap();
    platform
        .shell(ShellRequest {
            task_id: "job-shell".into(),
            cwd: path_string(&dir),
            workspace: path_string(&dir),
            roots: Vec::new(),
            output_dir: path_string(&dir),
            cols: 80,
            rows: 24,
        })
        .await
        .unwrap();
    let missing_parent = dir.join("missing").join("job-2.log");
    let invalid = platform
        .write(ShellWrite {
            task_id: "job-shell".into(),
            data: base64::engine::general_purpose::STANDARD.encode(b"printf 'x'\n"),
            command: None,
            job: Some(ShellJobStart {
                task_id: "job-2".into(),
                end_marker: "__DIM_END_aaaabbbb_ccccdddd__".into(),
                output_path: missing_parent.to_string_lossy().into_owned(),
            }),
        })
        .await;
    assert!(matches!(invalid, Err(HostPlatformError::Invalid(_))));
    let output_path = dir.join("job-1.log");
    let line = format!(
        "printf '\\033'; printf '\\033['; printf '\\033[2~'; printf '\\033]0;t\\033\\\\'; printf 'job-output\\n' > '{}'; printf '%s%s%d\\n' '__DIM_END_aaaabbbb_' 'ccccdddd__' 0\n",
        output_path.display()
    );
    platform
        .write(ShellWrite {
            task_id: "job-shell".into(),
            data: base64::engine::general_purpose::STANDARD.encode(line.as_bytes()),
            command: None,
            job: Some(ShellJobStart {
                task_id: "job-1".into(),
                end_marker: "__DIM_END_aaaabbbb_ccccdddd__".into(),
                output_path: output_path.to_string_lossy().into_owned(),
            }),
        })
        .await
        .unwrap();
    let reply = platform
        .wait(
            WaitRequest {
                task_id: "job-1".into(),
                timeout_ms: Some(5_000),
            },
            never_cancelled(),
        )
        .await
        .unwrap();
    assert_eq!(reply.status, WaitStatus::Exited, "job reply: {reply:?}");
    assert_eq!(reply.exit_code, Some(0));
    let page = platform
        .output(OutputRequest {
            task_id: "job-1".into(),
            cursor: None,
            limit: Some(16 * 1024),
        })
        .await
        .unwrap();
    assert!(page.text.contains("job-output"), "{}", page.text);
    platform
        .settle(TaskRequest {
            task_id: "job-1".into(),
        })
        .await
        .unwrap();
    let reused = platform
        .write(ShellWrite {
            task_id: "job-shell".into(),
            data: base64::engine::general_purpose::STANDARD.encode(b"printf 'again'\n"),
            command: None,
            job: Some(ShellJobStart {
                task_id: "job-1".into(),
                end_marker: "__DIM_END_aaaabbbb_ccccdddd__".into(),
                output_path: output_path.to_string_lossy().into_owned(),
            }),
        })
        .await;
    assert!(matches!(reused, Err(HostPlatformError::TaskExists)));
    platform
        .kill(TaskRequest {
            task_id: "job-shell".into(),
        })
        .await
        .unwrap();
    platform
        .settle(TaskRequest {
            task_id: "job-shell".into(),
        })
        .await
        .unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}
