//! Exercise bubblewrap through existing process and PTY handles; no product policy.

#[cfg(target_os = "linux")]
mod linux {
    use dim_agent_capabilities::host_platform::{
        EXEC_SHELL_UNAVAILABLE, HostPlatform, ProcessSpec, PtySize, ShellStatus,
    };
    use serde_json::json;
    use std::{
        ffi::OsString,
        io::Write,
        net::{SocketAddr, TcpStream, UdpSocket},
        os::unix::net::UnixStream,
        path::{Path, PathBuf},
        process::Output,
        time::Duration,
    };
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

    type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
    const LIMIT: Duration = Duration::from_secs(5);
    const PACKET: &[u8] = b"isolated-probe-packet";

    pub fn main() -> Result<()> {
        let args: Vec<_> = std::env::args().collect();
        match args.get(1).map(String::as_str) {
            Some("--tty-check") => {
                println!(
                    "{}",
                    json!({
                        "isatty": unsafe {libc::isatty(0)},
                        "processGroup": unsafe {libc::getpgrp()},
                        "terminalGroup": unsafe {libc::tcgetpgrp(0)},
                        "controllingTerminalOpen": std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty").is_ok()
                    })
                );
                Ok(())
            }
            Some("--terminal-controls") => tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?
                .block_on(terminal_controls()),
            Some("--background-exit") => tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?
                .block_on(background_exit()),
            Some("--tcp") => {
                TcpStream::connect_timeout(&args[2].parse()?, Duration::from_millis(600))?
                    .write_all(PACKET)?;
                Ok(())
            }
            Some("--udp") => {
                let destination: SocketAddr = args[2].parse()?;
                let local = if destination.is_ipv4() {
                    "0.0.0.0:0"
                } else {
                    "[::]:0"
                };
                UdpSocket::bind(local)?.send_to(PACKET, destination)?;
                Ok(())
            }
            Some("--unix") => {
                UnixStream::connect(&args[2])?.write_all(PACKET)?;
                Ok(())
            }
            None => tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?
                .block_on(execute()),
            _ => Err("Unknown probe mode".into()),
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

    fn isolated(mut spec: ProcessSpec, readonly: &Path) -> ProcessSpec {
        let mut args: Vec<OsString> = [
            "--unshare-user",
            "--unshare-pid",
            "--unshare-net",
            "--unshare-ipc",
            "--unshare-uts",
            "--die-with-parent",
            "--cap-drop",
            "ALL",
            "--ro-bind",
            "/usr",
            "/usr",
            "--ro-bind",
            "/etc",
            "/etc",
            "--symlink",
            "usr/bin",
            "/bin",
            "--symlink",
            "usr/sbin",
            "/sbin",
            "--symlink",
            "usr/lib",
            "/lib",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--ro-bind",
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        args.extend([
            readonly.as_os_str().into(),
            readonly.as_os_str().into(),
            "--bind".into(),
            spec.cwd.as_os_str().into(),
            spec.cwd.as_os_str().into(),
            "--remount-ro".into(),
            "/".into(),
            "--chdir".into(),
            spec.cwd.as_os_str().into(),
            "--clearenv".into(),
            "--setenv".into(),
            "PATH".into(),
            "/usr/bin:/bin".into(),
            "--".into(),
            spec.executable.into_os_string(),
        ]);
        args.append(&mut spec.args);
        spec.executable = "/usr/bin/bwrap".into();
        spec.args = args;
        spec
    }

    async fn run(platform: &HostPlatform, spec: ProcessSpec) -> Result<Output> {
        let mut tree = platform.spawn(spec)?;
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
    fn refusal(output: &Output) {
        assert!(!output.status.success(), "{output:?}");
    }

    type PtyReader = std::io::BufReader<Box<dyn std::io::Read + Send>>;
    async fn read_through_line(
        mut reader: PtyReader,
        marker: &'static str,
    ) -> Result<(PtyReader, String)> {
        let reading = tokio::task::spawn_blocking(move || {
            let mut transcript = String::new();
            loop {
                let mut line = String::new();
                if std::io::BufRead::read_line(&mut reader, &mut line)? == 0 {
                    return Err(std::io::Error::other(transcript));
                }
                transcript.push_str(&line);
                if line.trim() == marker {
                    return Ok((reader, transcript));
                }
            }
        });
        Ok(tokio::time::timeout(LIMIT, reading).await???)
    }

    async fn terminal_controls() -> Result<()> {
        let root = std::env::temp_dir().join(format!("dim-f5-terminal-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root)?;
        let fixture = Fixture(root);
        let workspace = fixture.0.join("workspace");
        let readonly = fixture.0.join("readonly");
        std::fs::create_dir(&workspace)?;
        std::fs::create_dir(&readonly)?;
        let helper = readonly.join("probe");
        std::fs::copy(std::env::current_exe()?, &helper)?;
        let platform = HostPlatform::current(ShellStatus::Unavailable {
            code: EXEC_SHELL_UNAVAILABLE,
            reason: "Explicit probe argv".into(),
        })?;
        let mut observations = Vec::new();
        for mode in ["plain", "new-session", "existing-session"] {
            let mut spec = command(&workspace, &helper, vec!["--tty-check".into()]);
            if mode != "plain" {
                spec = isolated(spec, &readonly);
            }
            if mode == "new-session" {
                spec.args.insert(0, "--new-session".into());
            }
            let mut pty = platform.spawn_pty(spec, PtySize { cols: 80, rows: 24 })?;
            let mut reader = std::io::BufReader::new(pty.take_reader());
            let reading = tokio::task::spawn_blocking(move || {
                let mut line = String::new();
                std::io::BufRead::read_line(&mut reader, &mut line)?;
                std::io::Result::Ok(line)
            });
            let line = tokio::time::timeout(LIMIT, reading).await???;
            assert!(pty.wait()?.success());
            let data: serde_json::Value = serde_json::from_str(line.trim())?;
            if mode == "plain" {
                assert_eq!(data["isatty"], 1);
                assert_eq!(data["controllingTerminalOpen"], true);
                assert_eq!(data["processGroup"], data["terminalGroup"]);
            }
            observations.push(json!({"mode":mode,"result":data}));
        }
        println!("{}", serde_json::to_string_pretty(&observations)?);
        Ok(())
    }

    fn descendants() -> Result<Vec<i32>> {
        let mut parents = Vec::new();
        for entry in std::fs::read_dir("/proc")? {
            let entry = entry?;
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<i32>().ok())
            else {
                continue;
            };
            let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
                continue;
            };
            let suffix = stat.rsplit_once(')').ok_or("Invalid /proc stat")?.1;
            let parent = suffix
                .split_whitespace()
                .nth(1)
                .ok_or("Missing parent PID")?
                .parse::<i32>()?;
            parents.push((pid, parent));
        }
        let mut found = vec![std::process::id() as i32];
        let mut index = 0;
        while index < found.len() {
            let parent = found[index];
            found.extend(
                parents
                    .iter()
                    .filter_map(|&(pid, ppid)| (ppid == parent).then_some(pid)),
            );
            index += 1;
        }
        Ok(found.into_iter().skip(1).collect())
    }

    async fn gone(pids: &[i32]) -> Result<()> {
        tokio::time::timeout(LIMIT, async {
            loop {
                // SAFETY: signal 0 only observes this probe's recorded descendants.
                let gone = pids.iter().all(|pid| {
                    (unsafe { libc::kill(*pid, 0) } == -1)
                        && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
                });
                if gone {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await?;
        Ok(())
    }

    async fn background_exit() -> Result<()> {
        let root = std::env::temp_dir().join(format!("dim-f5-background-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root)?;
        let fixture = Fixture(root);
        let readonly = fixture.0.join("readonly");
        std::fs::create_dir(&readonly)?;
        let platform = HostPlatform::current(ShellStatus::Unavailable {
            code: EXEC_SHELL_UNAVAILABLE,
            reason: "Explicit probe argv".into(),
        })?;
        let mut observations = Vec::new();
        for mode in ["raw", "private-pid-parent-death", "private-pid-only"] {
            let workspace = fixture.0.join(mode);
            std::fs::create_dir(&workspace)?;
            std::fs::write(workspace.join("heartbeat"), b"")?;
            let prepare = |spec| {
                if mode == "raw" {
                    return spec;
                }
                let mut spec = isolated(spec, &readonly);
                if mode == "private-pid-only" {
                    spec.args.retain(|arg| arg != "--die-with-parent");
                }
                spec
            };
            let script = "(while :; do printf x >> heartbeat; sleep 0.05; done) > background.log 2>&1 & echo $! > child.pid; echo ready; while test ! -f release; do sleep 0.01; done";
            let mut tree = platform.spawn(prepare(command(
                &workspace,
                Path::new("/bin/sh"),
                vec!["-c".into(), script.into()],
            )))?;
            let mut line = String::new();
            tokio::time::timeout(LIMIT, BufReader::new(tree.stdout()).read_line(&mut line))
                .await??;
            assert_eq!(line.trim(), "ready");
            let namespace_pid = std::fs::read_to_string(workspace.join("child.pid"))?
                .trim()
                .parse::<i32>()?;
            let pids = descendants()?;
            let host_pid = pids
                .iter()
                .copied()
                .find(|pid| {
                    std::fs::read_to_string(format!("/proc/{pid}/status"))
                        .ok()
                        .and_then(|status| {
                            status
                                .lines()
                                .find(|line| line.starts_with("NSpid:"))
                                .and_then(|line| line.split_whitespace().last())
                                .and_then(|pid| pid.parse::<i32>().ok())
                        })
                        == Some(namespace_pid)
                })
                .ok_or("Background child is absent from the probe's descendants")?;
            std::fs::write(workspace.join("release"), b"")?;
            let status = match tokio::time::timeout(LIMIT, tree.wait()).await {
                Ok(status) => Some(status?),
                Err(_) => {
                    tree.terminate().await?;
                    None
                }
            };
            drop(tree);
            let heartbeat = workspace.join("heartbeat");
            let before = std::fs::read(&heartbeat)?.len();
            let continued = tokio::time::timeout(Duration::from_millis(500), async {
                loop {
                    if std::fs::read(&heartbeat)?.len() > before {
                        return Ok::<_, std::io::Error>(());
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await;
            let keeps_running = match continued {
                Ok(result) => {
                    result?;
                    true
                }
                Err(_) => false,
            };
            let later = run(
                &platform,
                prepare(command(
                    &workspace,
                    Path::new("/bin/sh"),
                    vec![
                        "-c".into(),
                        "kill -0 \"$1\"".into(),
                        "pid-check".into(),
                        namespace_pid.to_string().into(),
                    ],
                )),
            )
            .await?;
            let observation = json!({
                "mode":mode,"rootExitCode":status.and_then(|status| status.code()),
                "rootWaitTimedOut":status.is_none(),"backgroundContinuesAfterWaitAndDrop":keeps_running,
                "namespacePid":namespace_pid,"hostPid":host_pid,
                "laterCommandCanAddressPid":later.status.success(),"recordedHostPids":pids
            });
            for &pid in &pids {
                // SAFETY: only descendants recorded in this disposable probe are terminated.
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
            }
            gone(&pids).await?;
            if mode == "raw" {
                assert_eq!(observation["rootExitCode"], 0);
                assert!(keeps_running && later.status.success(), "{observation}");
            }
            observations.push(observation);
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "scope":"actual HostPlatform wait/drop and redirected background work; no product integration or policy change",
                "observations":observations
            }))?
        );
        Ok(())
    }

    async fn execute() -> Result<()> {
        let root = std::env::temp_dir().join(format!("dim-f5-bwrap-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root)?;
        let fixture = Fixture(root.canonicalize()?);
        let workspace = fixture.0.join("workspace with ' quotes");
        let readonly = fixture.0.join("readonly");
        let private = fixture.0.join("private");
        for path in [&workspace, &readonly, &private] {
            std::fs::create_dir(path)?;
        }
        let helper = readonly.join("probe");
        std::fs::copy(std::env::current_exe()?, &helper)?;
        let platform = HostPlatform::current(ShellStatus::Unavailable {
            code: EXEC_SHELL_UNAVAILABLE,
            reason: "Explicit probe argv".into(),
        })?;
        let shell = |script: &str| {
            command(
                &workspace,
                Path::new("/bin/sh"),
                vec!["-c".into(), script.into()],
            )
        };
        let mut checks = Vec::new();
        let uid = unsafe { libc::geteuid() };
        let output = run(
            &platform,
            isolated(shell("id -u; readlink /proc/self/ns/net"), &readonly),
        )
        .await?;
        success(&output);
        let text = String::from_utf8(output.stdout)?;
        let mut lines = text.lines();
        assert_eq!(lines.next().ok_or("Missing uid")?.parse::<u32>()?, uid);
        let netns = lines.next().ok_or("Missing network namespace")?;
        assert_ne!(Path::new(netns), std::fs::read_link("/proc/self/ns/net")?);
        checks.push("unprivileged UID retained and private network namespace");

        for address in ["127.0.0.1", "::1"] {
            let tcp = tokio::net::TcpListener::bind((address, 0)).await?;
            let target = tcp.local_addr()?.to_string();
            let args = vec!["--tcp".into(), target.into()];
            success(&run(&platform, command(&workspace, &helper, args.clone())).await?);
            let (mut stream, _) = tokio::time::timeout(LIMIT, tcp.accept()).await??;
            let mut bytes = Vec::new();
            tokio::time::timeout(LIMIT, stream.read_to_end(&mut bytes)).await??;
            assert_eq!(bytes, PACKET);
            refusal(
                &run(
                    &platform,
                    isolated(command(&workspace, &helper, args), &readonly),
                )
                .await?,
            );
            assert!(
                tokio::time::timeout(Duration::from_millis(300), tcp.accept())
                    .await
                    .is_err()
            );

            let udp = tokio::net::UdpSocket::bind((address, 0)).await?;
            let args = vec!["--udp".into(), udp.local_addr()?.to_string().into()];
            success(&run(&platform, command(&workspace, &helper, args.clone())).await?);
            let mut bytes = [0; 128];
            let size = tokio::time::timeout(LIMIT, udp.recv(&mut bytes)).await??;
            assert_eq!(&bytes[..size], PACKET);
            let _output = run(
                &platform,
                isolated(command(&workspace, &helper, args), &readonly),
            )
            .await?;
            assert!(
                tokio::time::timeout(Duration::from_millis(300), udp.recv(&mut bytes))
                    .await
                    .is_err()
            );
        }
        checks.push(
            "live host IPv4/IPv6 TCP and UDP controls receive; isolated payloads do not reach them",
        );
        eprintln!("network checks passed");

        let socket = private.join("host.sock");
        let listener = tokio::net::UnixListener::bind(&socket)?;
        let args = vec!["--unix".into(), socket.as_os_str().into()];
        success(&run(&platform, command(&workspace, &helper, args.clone())).await?);
        let (mut stream, _) = tokio::time::timeout(LIMIT, listener.accept()).await??;
        let mut bytes = Vec::new();
        tokio::time::timeout(LIMIT, stream.read_to_end(&mut bytes)).await??;
        assert_eq!(bytes, PACKET);
        refusal(
            &run(
                &platform,
                isolated(command(&workspace, &helper, args), &readonly),
            )
            .await?,
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(300), listener.accept())
                .await
                .is_err()
        );
        checks.push("host UNIX socket outside grants is inaccessible");

        let mut tree = platform.spawn(isolated(shell("sleep 60 & echo ready; wait"), &readonly))?;
        let mut line = String::new();
        tokio::time::timeout(LIMIT, BufReader::new(tree.stdout()).read_line(&mut line)).await??;
        assert_eq!(line.trim(), "ready");
        let tree_pids = descendants()?;
        assert!(tree_pids.len() >= 2, "{tree_pids:?}");
        tokio::time::timeout(LIMIT, tree.terminate()).await??;
        gone(&tree_pids).await?;
        checks.push(
            "actual ProcessTree terminate removes monitor, namespace init and shell descendants",
        );
        eprintln!("ProcessTree checks passed: {tree_pids:?}");

        let mut pty = platform.spawn_pty(
            isolated(
                shell("sleep 60 & printf 'ready\\n'; read line; stty size; wait"),
                &readonly,
            ),
            PtySize { cols: 80, rows: 24 },
        )?;
        let reader = std::io::BufReader::new(pty.take_reader());
        let (reader, line) = read_through_line(reader, "ready").await?;
        assert_eq!(line.trim(), "ready");
        pty.resize(PtySize {
            cols: 132,
            rows: 41,
        })?;
        pty.write(b"continue\n")?;
        let (_reader, _output) = read_through_line(reader, "41 132").await?;
        let pty_pids = descendants()?;
        assert!(pty_pids.len() >= 2, "{pty_pids:?}");
        tokio::time::timeout(LIMIT, pty.terminate()).await??;
        gone(&pty_pids).await?;
        checks.push("actual PTY input, resize, termination and descendant removal");
        eprintln!("PTY checks passed: {pty_pids:?}");

        let script = r#"/bin/sh -c 'kill -STOP $$; printf "foreground-ready\n"; read line; printf "child-finished\n"' & job=$!; wait "$job"; fg; sleep 60 & printf 'job-control-finished\n'; wait"#;
        let spec = command(
            &workspace,
            Path::new("/bin/bash"),
            vec![
                "--noprofile".into(),
                "--norc".into(),
                "-i".into(),
                "-c".into(),
                script.into(),
            ],
        );
        let mut interactive =
            platform.spawn_pty(isolated(spec, &readonly), PtySize { cols: 80, rows: 24 })?;
        let reader = std::io::BufReader::new(interactive.take_reader());
        let (reader, mut transcript) = read_through_line(reader, "foreground-ready").await?;
        assert!(!transcript.contains("no job control"), "{transcript}");
        assert!(
            !transcript.contains("cannot set terminal process group"),
            "{transcript}"
        );
        interactive.write(b"continue\n")?;
        let (_reader, completed) = read_through_line(reader, "job-control-finished").await?;
        transcript.push_str(&completed);
        assert!(
            transcript
                .lines()
                .any(|line| line.trim() == "child-finished"),
            "{transcript}"
        );
        let interactive_pids = descendants()?;
        assert!(interactive_pids.len() >= 2, "{interactive_pids:?}");
        tokio::time::timeout(LIMIT, interactive.terminate()).await??;
        gone(&interactive_pids).await?;
        checks.push("interactive bash resumes a stopped job in the foreground; terminal input and whole-tree termination work");
        eprintln!("interactive job control passed: {interactive_pids:?}");

        let marker = workspace.join("invalid-ran");
        let spec = command(
            &workspace,
            Path::new("/usr/bin/touch"),
            vec![marker.as_os_str().into()],
        );
        refusal(&run(&platform, isolated(spec, &fixture.0.join("missing-bind"))).await?);
        assert!(!marker.exists());
        checks.push("invalid bind refuses before payload marker");
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "backend":"bubblewrap", "uid":uid,"checks":checks,
                "processTreePids":tree_pids,"ptyPids":pty_pids,"interactivePids":interactive_pids,
                "scope":"actual HostPlatform handles in a disposable Linux VM; not product exec/PTY integration",
                "terminal":"existing HostPlatform setsid/dedicated PTY; no bubblewrap --new-session",
                "interactiveTranscript":transcript
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
    Err("This probe requires Linux bubblewrap; other platforms are not implemented.".into())
}
