//! Exercise Landlock through existing process handles; this is not a product policy.

#[cfg(target_os = "linux")]
mod linux {
    use dim_agent_capabilities::host_platform::{
        EXEC_SHELL_UNAVAILABLE, HostPlatform, ProcessSpec, PtySize, ShellStatus,
    };
    use serde_json::json;
    use std::{
        ffi::{CString, OsString},
        io::{self, Read},
        net::{TcpListener, TcpStream},
        os::{
            fd::{AsRawFd, FromRawFd, OwnedFd},
            unix::{ffi::OsStrExt, process::CommandExt},
        },
        path::{Path, PathBuf},
        process::{Command, Output},
        time::{Duration, Instant},
    };
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

    type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
    const LIMIT: Duration = Duration::from_secs(5);
    const READ_EXEC: u64 = (1 << 0) | (1 << 2) | (1 << 3);
    // ABI 4: filesystem rights through TRUNCATE, and TCP bind/connect.
    const FILESYSTEM: u64 = (1 << 15) - 1;

    #[repr(C)]
    struct Ruleset {
        filesystem: u64,
        network: u64,
    }
    #[repr(C, packed)]
    struct PathRule {
        access: u64,
        fd: i32,
    }

    fn checked(value: libc::c_long) -> io::Result<i32> {
        if value == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(value as i32)
        }
    }
    fn abi() -> io::Result<i32> {
        checked(unsafe {
            libc::syscall(
                libc::SYS_landlock_create_ruleset,
                std::ptr::null::<Ruleset>(),
                0usize,
                1u32,
            )
        })
    }
    fn allow(ruleset: &OwnedFd, path: &Path, access: u64) -> Result<()> {
        let path = CString::new(path.as_os_str().as_bytes())?;
        let fd = checked(unsafe {
            libc::open(path.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) as libc::c_long
        })?;
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let rule = PathRule {
            access,
            fd: fd.as_raw_fd(),
        };
        checked(unsafe {
            libc::syscall(
                libc::SYS_landlock_add_rule,
                ruleset.as_raw_fd(),
                1u32,
                &rule,
                0u32,
            )
        })?;
        Ok(())
    }
    fn restrict(workspace: &Path) -> Result<()> {
        if abi()? < 4 {
            return Err("This probe requires Landlock ABI 4 or newer.".into());
        }
        let ruleset = Ruleset {
            filesystem: FILESYSTEM,
            network: 3,
        };
        let fd = checked(unsafe {
            libc::syscall(
                libc::SYS_landlock_create_ruleset,
                &ruleset,
                std::mem::size_of::<Ruleset>(),
                0u32,
            )
        })?;
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        for path in ["/usr", "/bin", "/lib", "/lib64"] {
            if Path::new(path).exists() {
                allow(&fd, Path::new(path), READ_EXEC)?;
            }
        }
        allow(&fd, &std::env::current_exe()?, (1 << 0) | (1 << 2))?;
        allow(&fd, Path::new("/dev/null"), (1 << 1) | (1 << 2))?;
        allow(&fd, workspace, FILESYSTEM)?;
        checked(unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) as libc::c_long })?;
        checked(unsafe { libc::syscall(libc::SYS_landlock_restrict_self, fd.as_raw_fd(), 0u32) })?;
        Ok(())
    }

    pub fn main() -> Result<()> {
        let mut args = std::env::args_os().skip(1);
        match args.next().as_deref() {
            Some(mode) if mode == "--landlock" => {
                let workspace = PathBuf::from(args.next().ok_or("missing workspace")?);
                let executable = args.next().ok_or("missing executable")?;
                restrict(&workspace)?;
                Err(Command::new(executable).args(args).exec().into())
            }
            Some(mode) if mode == "--connect" => {
                let port = args
                    .next()
                    .ok_or("missing port")?
                    .into_string()
                    .map_err(|_| "invalid port")?
                    .parse::<u16>()?;
                TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), LIMIT)?;
                Ok(())
            }
            None => tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?
                .block_on(execute()),
            _ => Err("Unknown probe argument".into()),
        }
    }

    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn command(cwd: &Path, executable: &Path, args: Vec<OsString>) -> ProcessSpec {
        ProcessSpec {
            executable: executable.into(),
            args,
            cwd: cwd.into(),
            env: vec![("PATH".into(), "/usr/bin:/bin".into())],
        }
    }
    fn isolated(mut spec: ProcessSpec, workspace: &Path) -> Result<ProcessSpec> {
        let mut args = vec![
            "--landlock".into(),
            workspace.as_os_str().into(),
            spec.executable.into_os_string(),
        ];
        args.append(&mut spec.args);
        spec.executable = std::env::current_exe()?;
        spec.args = args;
        Ok(spec)
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
            String::from_utf8_lossy(&output.stderr).contains("Permission denied"),
            "{output:?}"
        );
    }

    async fn execute() -> Result<()> {
        let abi = abi()?;
        if abi < 4 {
            return Err("This probe requires Landlock ABI 4 or newer.".into());
        }
        let root = std::env::temp_dir().join(format!("dim-f5-landlock-{}", uuid::Uuid::new_v4()));
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
        let shell = |script: &str, args: Vec<OsString>| {
            let mut argv = vec!["-c".into(), script.into(), "probe".into()];
            argv.extend(args);
            command(&workspace, Path::new("/bin/sh"), argv)
        };
        let mut checks = Vec::new();
        let output = run(
            &platform,
            isolated(
                shell("cat input; printf written > output", vec![]),
                &workspace,
            )?,
        )
        .await?;
        success(&output);
        assert_eq!(output.stdout, b"workspace-input");
        assert_eq!(std::fs::read(workspace.join("output"))?, b"written");
        checks.push("workspace read/write and quoted path");
        for path in [&secret, &workspace.join("escape/sentinel")] {
            let output = run(
                &platform,
                isolated(
                    shell("cat \"$1\" & wait $!", vec![path.as_os_str().into()]),
                    &workspace,
                )?,
            )
            .await?;
            denied(&output);
            assert!(!String::from_utf8_lossy(&output.stdout).contains("private-sentinel"));
        }
        checks.push("descendant private read and symlink escape denied");
        for path in [private.join("outside"), workspace.join("escape/outside")] {
            denied(
                &run(
                    &platform,
                    isolated(
                        shell("printf forbidden > \"$1\"", vec![path.as_os_str().into()]),
                        &workspace,
                    )?,
                )
                .await?,
            );
            assert!(!path.exists());
        }
        checks.push("outside write and symlink write denied");
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let executable = std::env::current_exe()?;
        let port = listener.local_addr()?.port().to_string();
        let connect = || {
            command(
                &workspace,
                &executable,
                vec!["--connect".into(), port.clone().into()],
            )
        };
        success(&run(&platform, connect()).await?);
        denied(&run(&platform, isolated(connect(), &workspace)?).await?);
        checks.push("TCP denied with a reachable positive control");
        let marker = workspace.join("invalid-policy-payload");
        let output = run(
            &platform,
            isolated(
                shell("touch invalid-policy-payload", vec![]),
                &fixture.0.join("missing"),
            )?,
        )
        .await?;
        assert!(!output.status.success());
        assert!(!marker.exists());
        checks.push("invalid policy does not execute payload");

        let mut tree = platform.spawn_tree(isolated(
            shell(
                "sleep 60 & child=$!; printf '%s\\n' \"$child\"; wait \"$child\"",
                vec![],
            ),
            &workspace,
        )?)?;
        let mut lines = BufReader::new(tree.stdout()).lines();
        let child = tokio::time::timeout(LIMIT, lines.next_line())
            .await??
            .ok_or("child did not announce readiness")?
            .parse::<i32>()?;
        assert_eq!(unsafe { libc::kill(child, 0) }, 0);
        let start = Instant::now();
        tokio::time::timeout(LIMIT, tree.terminate()).await??;
        tokio::time::timeout(LIMIT, async {
            while unsafe { libc::kill(child, 0) } == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await?;
        let terminate_ms = start.elapsed().as_secs_f64() * 1000.0;
        checks.push("grandchild terminates through the existing tree handle");
        let mut pty = platform.spawn_pty(
            isolated(shell("printf ready; sleep 60", vec![]), &workspace)?,
            PtySize { rows: 24, cols: 80 },
        )?;
        let mut reader = pty.take_reader();
        tokio::time::timeout(
            LIMIT,
            tokio::task::spawn_blocking(move || {
                let mut ready = [0; 5];
                reader.read_exact(&mut ready)?;
                assert_eq!(&ready, b"ready");
                io::Result::Ok(())
            }),
        )
        .await???;
        tokio::time::timeout(LIMIT, pty.terminate()).await??;
        checks.push("PTY starts and terminates through the existing handle");
        let (mut plain_ms, mut sandbox_ms) = (Vec::new(), Vec::new());
        for _ in 0..20 {
            let start = Instant::now();
            success(
                &run(
                    &platform,
                    command(&workspace, Path::new("/bin/true"), vec![]),
                )
                .await?,
            );
            plain_ms.push(start.elapsed().as_secs_f64() * 1000.0);
            let start = Instant::now();
            success(
                &run(
                    &platform,
                    isolated(
                        command(&workspace, Path::new("/bin/true"), vec![]),
                        &workspace,
                    )?,
                )
                .await?,
            );
            sandbox_ms.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "os": std::env::consts::OS, "arch": std::env::consts::ARCH,
                "backend": "landlock", "abi": abi, "uid": unsafe { libc::geteuid() },
                "checks": checks, "terminateMs": terminate_ms,
                "spawnToExitMs": { "plain": plain_ms, "sandbox": sandbox_ms }
            }))?
        );
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    Err("This probe requires Linux with Landlock ABI 4 or newer.".into())
}
