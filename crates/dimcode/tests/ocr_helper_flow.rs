//! The OCR helper subcommand through a confined spawn: the process contract
//! a Modality host task relies on.
#![cfg(target_os = "macos")]
use dim_agent_capabilities::host_platform::{
    ConfinedError, ConfinedOutput, ConfinedSpec, Confinement, HostPlatform, ShellStatus,
    SystemServices,
};
use serde_json::Value;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// A fresh directory for one call.
fn scratch() -> PathBuf {
    let path =
        std::env::temp_dir().join(format!("dim-ocr-helper-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

/// The checked-in fixtures.
fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../dim-ocr/tests/fixtures")
        .join(name)
}

/// Runs `dimcode ocr-helper` under its confinement.
async fn run(
    base: &Path,
    input: &Path,
    extra_args: &[&str],
) -> Result<ConfinedOutput, ConfinedError> {
    let work = base.join("work");
    let tmp = base.join("tmp");
    let cache = base.join("cache");
    for path in [&work, &tmp, &cache] {
        std::fs::create_dir_all(path).unwrap();
    }
    let platform = HostPlatform::current(ShellStatus::Unavailable {
        code: "test",
        reason: String::new(),
    })
    .unwrap();
    let mut args = vec![
        "ocr-helper".into(),
        "--input".into(),
        input.to_owned().into_os_string(),
        "--artifacts".into(),
        work.clone().into_os_string(),
        "--output-mode".into(),
        "text".into(),
    ];
    args.extend(extra_args.iter().copied().map(OsString::from));
    platform
        .spawn_confined(
            ConfinedSpec {
                executable: PathBuf::from(env!("CARGO_BIN_EXE_dimcode")),
                args,
                env: vec![
                    (OsString::from("HOME"), cache.clone().into_os_string()),
                    (
                        OsString::from("CFFIXED_USER_HOME"),
                        cache.clone().into_os_string(),
                    ),
                    (OsString::from("TMPDIR"), tmp.clone().into_os_string()),
                ],
            },
            Confinement {
                read: vec![input.to_owned()],
                write: vec![work, tmp, cache],
                timeout: Duration::from_secs(300),
                stdout_limit: 64 * 1024,
                stderr_limit: 64 * 1024,
                services: SystemServices::OnDeviceRecognition,
            },
            CancellationToken::new(),
        )
        .await
}

/// One JSON line on stdout, parsed.
fn line(output: &ConfinedOutput) -> Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "one line, got {stdout:?}");
    serde_json::from_str(lines[0]).unwrap()
}

#[cfg(target_arch = "aarch64")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Manual Apple Vision acceptance; see #1446"]
async fn an_image_is_recognized_under_the_confinement() {
    let base = scratch();
    let output = run(&base, &fixture("latin.png"), &[]).await.unwrap();
    assert!(output.confined);
    assert!(output.status.success(), "{:?}", output.stderr);
    let payload = line(&output);
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["pageCount"], 1);
    let text = std::fs::read_to_string(base.join("work/text.txt")).unwrap();
    assert!(
        text.to_ascii_lowercase().contains("dimcode"),
        "recognized text: {text:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unknown_flag_is_a_typed_exit_two() {
    let input = fixture("latin.png");
    let output = run(&scratch(), &input, &["--bogus"]).await.unwrap();
    assert!(output.confined);
    assert_eq!(output.status.code(), Some(2));
    let payload = line(&output);
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "invalid_input");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_missing_input_is_a_typed_exit_two() {
    let base = scratch();
    let output = run(&base, &base.join("missing.png"), &[]).await;
    // The confinement refuses a read path that does not exist before the
    // helper starts.
    assert!(output.is_err());
}
