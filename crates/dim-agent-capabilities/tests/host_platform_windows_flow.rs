//! HostPlatform confined helpers on Windows
//! (`docs/architecture/rust-agent/plugins/HostPlatform.md`). The platform has
//! no confinement backend, so the timeout, output caps and cancellation apply
//! and the result reports `confined: false`.
#![cfg(windows)]
use dim_agent_capabilities::execution_settings::{ExecutionSettings, ExecutionSettingsHost};
use dim_agent_capabilities::host_platform::*;
use std::{ffi::OsString, path::PathBuf, time::Duration};
use tokio_util::sync::CancellationToken;

/// A platform whose Process isolation switch is off.
fn platform() -> HostPlatform {
    HostPlatform::current(ShellStatus::Unavailable {
        code: "test",
        reason: String::new(),
    })
    .unwrap()
    .with_execution_settings(ExecutionSettingsHost::fixed(ExecutionSettings::default()))
}

fn command(script: &str) -> ConfinedSpec {
    let executable = std::env::var_os("ComSpec")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows\System32\cmd.exe"));
    ConfinedSpec {
        executable,
        args: ["/d", "/s", "/v:off", "/c", script]
            .into_iter()
            .map(OsString::from)
            .collect(),
        env: vec![],
    }
}

#[tokio::test]
async fn a_helper_without_a_backend_reports_no_confinement_and_keeps_every_limit() {
    let write = std::env::temp_dir().join(format!("dim-confined-{}", std::process::id()));
    std::fs::create_dir_all(&write).unwrap();
    let confinement = |timeout: Duration| Confinement {
        read: vec![],
        write: vec![write.clone()],
        timeout,
        stdout_limit: 1024,
        stderr_limit: 1024,
        services: SystemServices::None,
    };

    let ok = platform()
        .spawn_confined(
            command("echo ok"),
            confinement(Duration::from_secs(30)),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(ok.status.success());
    assert!(!ok.confined, "Windows cannot confine a host-owned helper");
    assert_eq!(String::from_utf8_lossy(&ok.stdout).trim(), "ok");

    let timed = platform()
        .spawn_confined(
            command("ping -n 61 127.0.0.1 >nul"),
            confinement(Duration::from_millis(300)),
            CancellationToken::new(),
        )
        .await;
    assert!(
        matches!(timed, Err(ConfinedError::TimedOut)),
        "{:?}",
        timed.err()
    );

    let flooded = platform()
        .spawn_confined(
            command("for /L %i in (1,1,100000) do @echo 01234567890123456789"),
            confinement(Duration::from_secs(30)),
            CancellationToken::new(),
        )
        .await;
    assert!(
        matches!(
            flooded,
            Err(ConfinedError::OutputLimit {
                stream: ConfinedStream::Stdout
            })
        ),
        "{:?}",
        flooded.err()
    );

    let cancel = CancellationToken::new();
    let cancel_platform = platform();
    let (cancelled, ()) = tokio::join!(
        cancel_platform.spawn_confined(
            command("ping -n 61 127.0.0.1 >nul"),
            confinement(Duration::from_secs(30)),
            cancel.clone(),
        ),
        async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            cancel.cancel();
        },
    );
    assert!(
        matches!(cancelled, Err(ConfinedError::Cancelled)),
        "{:?}",
        cancelled.err()
    );
    std::fs::remove_dir_all(&write).unwrap();
}
