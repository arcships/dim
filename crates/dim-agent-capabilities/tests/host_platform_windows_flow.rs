//! HostPlatform confined helpers on Windows
//! (`docs/architecture/rust-agent/plugins/HostPlatform.md`). The platform has
//! no confinement backend, so the timeout, output caps and cancellation apply
//! and the result reports `confined: false`.
use base64::Engine;
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

fn request(
    script: &str,
    timeout: Duration,
    stdout_limit: usize,
    write: &std::path::Path,
) -> ConfinedRequest {
    let executable = std::env::var_os("ComSpec")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows\System32\cmd.exe"));
    ConfinedRequest {
        program: executable.to_str().expect("ComSpec is UTF-8").to_owned(),
        args: ["/d", "/s", "/v:off", "/c", script]
            .into_iter()
            .map(OsString::from)
            .map(|arg| arg.to_str().expect("argument is UTF-8").to_owned())
            .collect(),
        read: Vec::new(),
        write: vec![write.to_str().expect("temporary path is UTF-8").to_owned()],
        env: Default::default(),
        timeout_ms: timeout.as_millis() as u64,
        output_limit: OutputLimits {
            stdout: stdout_limit,
            stderr: 1024,
        },
        services: SystemServices::None,
    }
}

fn cancelled(token: &CancellationToken) -> dim_agent_capabilities::host::CancelledFn {
    let token = token.clone();
    std::sync::Arc::new(move || {
        let token = token.clone();
        Box::pin(async move {
            token.cancelled().await;
            Ok(())
        })
    })
}

#[tokio::test]
async fn a_helper_without_a_backend_reports_no_confinement_and_keeps_every_limit() {
    let write = std::env::temp_dir().join(format!("dim-confined-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&write).unwrap();
    let platform = platform();

    let ok = platform
        .spawn_confined(
            request("echo ok", Duration::from_secs(30), 1024, &write),
            cancelled(&CancellationToken::new()),
        )
        .await
        .unwrap();
    assert_eq!(ok.status, Some(0));
    assert!(!ok.confined, "Windows cannot confine a host-owned helper");
    let stdout = base64::engine::general_purpose::STANDARD
        .decode(&ok.stdout)
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&stdout).trim(), "ok");

    let timed = platform
        .spawn_confined(
            request(
                "ping -n 61 127.0.0.1 >nul",
                Duration::from_millis(300),
                1024,
                &write,
            ),
            cancelled(&CancellationToken::new()),
        )
        .await;
    assert!(
        matches!(timed, Err(HostPlatformError::TimedOut)),
        "{:?}",
        timed.err()
    );

    let flooded = platform
        .spawn_confined(
            request(
                "for /L %i in (1,1,100000) do @echo 01234567890123456789",
                Duration::from_secs(30),
                1024,
                &write,
            ),
            cancelled(&CancellationToken::new()),
        )
        .await;
    assert!(
        matches!(
            flooded,
            Err(HostPlatformError::OutputLimit {
                stream: ConfinedStream::Stdout
            })
        ),
        "{:?}",
        flooded.err()
    );

    let cancel = CancellationToken::new();
    let cancel_task = async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel.cancel();
    };
    let (result, ()) = tokio::join!(
        platform.spawn_confined(
            request(
                "ping -n 61 127.0.0.1 >nul",
                Duration::from_secs(30),
                1024,
                &write,
            ),
            cancelled(&cancel),
        ),
        cancel_task,
    );
    assert!(
        matches!(result, Err(HostPlatformError::Cancelled)),
        "{:?}",
        result.err()
    );
    let _ = std::fs::remove_dir_all(&write);
}
