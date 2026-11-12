//! The OCR helper subcommand through a confined spawn: the process contract
//! a Modality host task relies on.
#![cfg(target_os = "macos")]
use dim_agent_capabilities::host_platform::{
    ConfinedSpec, Confinement, HostPlatform, ShellStatus, SystemServices,
};
use serde_json::Value;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// A fresh directory for one call, removed with it.
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

/// The directories one call reads and writes.
struct Dirs {
    work: PathBuf,
    tmp: PathBuf,
    cache: PathBuf,
}

/// Runs `dimcode ocr-helper` under its confinement.
async fn run(
    input: &Path,
    mode: &str,
    args: Option<Vec<OsString>>,
) -> (
    PathBuf,
    Result<
        dim_agent_capabilities::host_platform::ConfinedOutput,
        dim_agent_capabilities::host_platform::ConfinedError,
    >,
) {
    let base = scratch();
    let dirs = Dirs {
        work: base.join("work"),
        tmp: base.join("tmp"),
        cache: base.join("cache"),
    };
    for path in [&dirs.work, &dirs.tmp, &dirs.cache] {
        std::fs::create_dir_all(path).unwrap();
    }
    let platform = HostPlatform::current(ShellStatus::Unavailable {
        code: "test",
        reason: String::new(),
    })
    .unwrap();
    let args = args.unwrap_or_else(|| {
        vec![
            "ocr-helper".into(),
            "--input".into(),
            input.to_owned().into_os_string(),
            "--artifacts".into(),
            dirs.work.clone().into_os_string(),
            "--output-mode".into(),
            mode.into(),
        ]
    });
    let output = platform
        .spawn_confined(
            ConfinedSpec {
                executable: PathBuf::from(env!("CARGO_BIN_EXE_dimcode")),
                args,
                env: vec![
                    (OsString::from("HOME"), dirs.cache.clone().into_os_string()),
                    (
                        OsString::from("CFFIXED_USER_HOME"),
                        dirs.cache.clone().into_os_string(),
                    ),
                    (OsString::from("TMPDIR"), dirs.tmp.clone().into_os_string()),
                ],
            },
            Confinement {
                read: vec![input.to_owned()],
                write: vec![dirs.work.clone(), dirs.tmp.clone(), dirs.cache.clone()],
                timeout: Duration::from_secs(300),
                stdout_limit: 64 * 1024,
                stderr_limit: 64 * 1024,
                services: SystemServices::OnDeviceRecognition,
            },
            CancellationToken::new(),
        )
        .await;
    (dirs.work, output)
}

/// One JSON line on stdout, parsed.
fn line(output: &dim_agent_capabilities::host_platform::ConfinedOutput) -> Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "one line, got {stdout:?}");
    serde_json::from_str(lines[0]).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_image_is_recognized_under_the_confinement() {
    let input = fixture("latin.png");
    let (work, output) = run(&input, "text", None).await;
    let output = output.unwrap();
    assert!(output.confined);
    assert!(output.status.success(), "{:?}", output.stderr);
    let payload = line(&output);
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["pageCount"], 1);
    let text = std::fs::read_to_string(work.join("text.txt")).unwrap();
    assert!(
        text.to_lowercase().contains("dimcode"),
        "recognized text: {text:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pdf_rasterizes_only_the_page_without_a_text_layer() {
    let input = fixture("mixed.pdf");
    let (work, output) = run(&input, "layout", None).await;
    let output = output.unwrap();
    assert!(output.status.success(), "{:?}", output.stderr);
    let payload = line(&output);
    assert_eq!(payload["pageCount"], 2);
    let text = std::fs::read_to_string(work.join("text.txt")).unwrap();
    assert!(text.contains("Native page one"), "text: {text:?}");
    assert!(text.to_lowercase().contains("dimcode"), "text: {text:?}");
    assert!(work.join("page-0001.png").is_file());
    assert!(work.join("page-0002.png").is_file());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unknown_flag_is_a_typed_exit_two() {
    let input = fixture("latin.png");
    let (_, output) = run(
        &input,
        "text",
        Some(vec![
            "ocr-helper".into(),
            "--input".into(),
            input.clone().into_os_string(),
            "--artifacts".into(),
            "/nonexistent".into(),
            "--bogus".into(),
        ]),
    )
    .await;
    let output = output.unwrap();
    assert_eq!(output.status.code(), Some(2));
    let payload = line(&output);
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "invalid_input");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_missing_input_is_a_typed_exit_two() {
    let input = scratch().join("missing.png");
    let (_, output) = run(&input, "text", None).await;
    // The confinement refuses a read path that does not exist before the
    // helper starts.
    assert!(output.is_err());
}
