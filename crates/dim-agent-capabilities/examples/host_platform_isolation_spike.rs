//! Exercise OS isolation through the existing process host, without product bindings.

#[cfg(target_os = "macos")]
mod macos {
    use dim_agent_capabilities::host_platform::{
        EXEC_SHELL_UNAVAILABLE, HostPlatform, ProcessSpec, PtySize, ShellStatus,
    };
    use serde_json::json;
    use std::{
        ffi::OsString,
        io::Read,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
        process::Output,
        time::{Duration, Instant},
    };
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

    type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
    const LIMIT: Duration = Duration::from_secs(5);
    const PROFILE: &str = r#"
        (version 1)
        (deny default)
        (allow process*)
        (allow sysctl-read)
        (allow file-read-metadata)
        (allow file-read*
            (literal "/")
            (subpath "/System")
            (subpath "/usr")
            (subpath "/bin")
            (subpath "/sbin")
            (subpath "/Library/Apple")
            (subpath "/private/etc")
            (subpath (param "WORKSPACE"))
            (literal "/dev/random")
            (literal "/dev/urandom"))
        (allow file-write* (subpath (param "WORKSPACE")))
        (allow file-read* file-write* (literal "/dev/null"))
    "#;

    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn command(cwd: &Path, executable: &str, args: Vec<OsString>) -> ProcessSpec {
        ProcessSpec {
            executable: executable.into(),
            args,
            cwd: cwd.into(),
            env: vec![("PATH".into(), "/usr/bin:/bin".into())],
        }
    }

    fn isolated(mut spec: ProcessSpec, profile: &str) -> ProcessSpec {
        let mut args = vec![
            "-D".into(),
            format!("WORKSPACE={}", spec.cwd.display()).into(),
            "-p".into(),
            profile.into(),
            spec.executable.into_os_string(),
        ];
        args.append(&mut spec.args);
        spec.executable = "/usr/bin/sandbox-exec".into();
        spec.args = args;
        spec
    }

    async fn run(platform: &HostPlatform, spec: ProcessSpec) -> Result<Output> {
        let mut tree = platform.spawn_tree(spec)?;
        let (mut stdout, mut stderr) = (tree.stdout(), tree.stderr());
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let (status, _, _) = tokio::time::timeout(LIMIT, async {
            tokio::try_join!(
                tree.wait(),
                stdout.read_to_end(&mut out),
                stderr.read_to_end(&mut err)
            )
        })
        .await??;
        Ok(Output {
            status,
            stdout: out,
            stderr: err,
        })
    }

    fn success(output: &Output) {
        assert!(output.status.success(), "{output:?}");
    }

    fn denied(output: &Output) {
        assert!(!output.status.success(), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("Operation not permitted"),
            "{output:?}"
        );
    }

    pub async fn execute() -> Result<()> {
        let root = std::env::temp_dir().join(format!("dim-f5-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root)?;
        let fixture = Fixture(root.canonicalize()?);
        let workspace = fixture.0.join("workspace with ' quotes");
        let private = fixture.0.join("private");
        std::fs::create_dir(&workspace)?;
        std::fs::create_dir(&private)?;
        let secret = private.join("sentinel");
        std::fs::write(&secret, "private-sentinel")?;
        std::fs::write(workspace.join("input"), "workspace-input")?;
        std::os::unix::fs::symlink(&private, workspace.join("escape"))?;
        let platform = HostPlatform::current(ShellStatus::Unavailable {
            code: EXEC_SHELL_UNAVAILABLE,
            reason: "The probe passes explicit argv.".into(),
        })?;
        let mut checks = Vec::new();

        let output = run(
            &platform,
            isolated(
                command(
                    &workspace,
                    "/bin/sh",
                    vec!["-c".into(), "cat input; printf written > output".into()],
                ),
                PROFILE,
            ),
        )
        .await?;
        success(&output);
        assert_eq!(output.stdout, b"workspace-input");
        assert_eq!(std::fs::read(workspace.join("output"))?, b"written");
        checks.push("workspace read/write and quoted path");

        for target in [&secret, &workspace.join("escape/sentinel")] {
            let output = run(
                &platform,
                isolated(
                    command(
                        &workspace,
                        "/bin/sh",
                        vec![
                            "-c".into(),
                            "cat \"$1\" & wait $!".into(),
                            "probe".into(),
                            target.as_os_str().into(),
                        ],
                    ),
                    PROFILE,
                ),
            )
            .await?;
            denied(&output);
            assert!(output.stdout.is_empty());
        }
        checks.push("descendant private read and symlink escape denied");

        for target in [private.join("created"), workspace.join("escape/created")] {
            let output = run(
                &platform,
                isolated(
                    command(
                        &workspace,
                        "/usr/bin/touch",
                        vec![target.as_os_str().into()],
                    ),
                    PROFILE,
                ),
            )
            .await?;
            denied(&output);
            assert!(!target.exists());
        }
        checks.push("outside write and symlink escape denied");

        let mode = std::fs::metadata(&secret)?.permissions().mode();
        denied(
            &run(
                &platform,
                isolated(
                    command(
                        &workspace,
                        "/bin/chmod",
                        vec!["777".into(), secret.as_os_str().into()],
                    ),
                    PROFILE,
                ),
            )
            .await?,
        );
        assert_eq!(std::fs::metadata(&secret)?.permissions().mode(), mode);
        checks.push("outside metadata change denied");

        for address in ["127.0.0.1", "::1"] {
            let listener = tokio::net::TcpListener::bind((address, 0)).await?;
            let args = vec![
                "-v".into(),
                "-z".into(),
                "-w".into(),
                "1".into(),
                address.into(),
                listener.local_addr()?.port().to_string().into(),
            ];
            success(&run(&platform, command(&workspace, "/usr/bin/nc", args.clone())).await?);
            denied(
                &run(
                    &platform,
                    isolated(command(&workspace, "/usr/bin/nc", args), PROFILE),
                )
                .await?,
            );

            let listener = tokio::net::UdpSocket::bind((address, 0)).await?;
            let args = vec![
                "-v".into(),
                "-u".into(),
                "-z".into(),
                "-w".into(),
                "1".into(),
                address.into(),
                listener.local_addr()?.port().to_string().into(),
            ];
            success(&run(&platform, command(&workspace, "/usr/bin/nc", args.clone())).await?);
            let mut packet = [0; 64];
            assert!(tokio::time::timeout(LIMIT, listener.recv(&mut packet)).await?? > 0);
            denied(
                &run(
                    &platform,
                    isolated(command(&workspace, "/usr/bin/nc", args), PROFILE),
                )
                .await?,
            );
        }
        checks.push("reachable host IPv4/IPv6 TCP and UDP denied in sandbox");

        let marker = workspace.join("invalid-policy-ran");
        let invalid = run(
            &platform,
            isolated(
                command(
                    &workspace,
                    "/usr/bin/touch",
                    vec![marker.as_os_str().into()],
                ),
                "(version 1) (invalid-policy-operation)",
            ),
        )
        .await?;
        assert!(!invalid.status.success(), "{invalid:?}");
        assert!(!marker.exists());
        checks.push("invalid policy never executes payload");

        let mut tree = platform.spawn_tree(isolated(
            command(
                &workspace,
                "/bin/sh",
                vec!["-c".into(), "sleep 60 & echo $!; wait".into()],
            ),
            PROFILE,
        ))?;
        let mut line = String::new();
        tokio::time::timeout(LIMIT, BufReader::new(tree.stdout()).read_line(&mut line)).await??;
        let pid: i32 = line.trim().parse()?;
        // SAFETY: query only, for a child whose PID came from this fixture.
        assert_eq!(unsafe { libc::kill(pid, 0) }, 0);
        let terminating = Instant::now();
        tokio::time::timeout(LIMIT, tree.terminate()).await??;
        let terminate_ms = terminating.elapsed().as_secs_f64() * 1000.0;
        // SAFETY: query only; termination must have removed the whole process group.
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        checks.push("existing ProcessTree termination removes grandchild");

        let mut pty = platform.spawn_pty(
            isolated(
                command(
                    &workspace,
                    "/bin/sh",
                    vec!["-c".into(), "printf ready; sleep 60".into()],
                ),
                PROFILE,
            ),
            PtySize { cols: 80, rows: 24 },
        )?;
        let mut reader = pty.take_reader();
        let reading = tokio::task::spawn_blocking(move || {
            let mut ready = [0; 5];
            reader.read_exact(&mut ready)?;
            std::io::Result::Ok(ready)
        });
        assert_eq!(tokio::time::timeout(LIMIT, reading).await???, *b"ready");
        tokio::time::timeout(LIMIT, pty.terminate()).await??;
        checks.push("existing PTY startup and termination");

        let mut plain_ms = Vec::new();
        let mut sandbox_ms = Vec::new();
        for _ in 0..20 {
            let start = Instant::now();
            success(&run(&platform, command(&workspace, "/usr/bin/true", vec![])).await?);
            plain_ms.push(start.elapsed().as_secs_f64() * 1000.0);
            let start = Instant::now();
            success(
                &run(
                    &platform,
                    isolated(command(&workspace, "/usr/bin/true", vec![]), PROFILE),
                )
                .await?,
            );
            sandbox_ms.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "os": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
                "backend": "sandbox-exec",
                "checks": checks,
                "terminateMs": terminate_ms,
                "spawnToExitMs": { "plain": plain_ms, "sandbox": sandbox_ms },
                "linux": "not measured",
                "windows": "unavailable"
            }))?
        );
        Ok(())
    }
}

#[cfg(target_os = "macos")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    macos::execute().await
}

#[cfg(not(target_os = "macos"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    Err("This probe requires macOS sandbox-exec; other platforms are not implemented.".into())
}
