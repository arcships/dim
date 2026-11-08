//! One layout OCR task under a lowered descriptor limit: the host path opens
//! and copies one artifact at a time, so a task publishes every page while the
//! process holds far fewer descriptors than the page count.
#![cfg(unix)]
use base64::Engine;
use dim_agent_capabilities::{adapters::MockTools, defaults};
use dim_agent_runtime::{AgentBuilder, AgentConfig};
use dim_app_server::{RunningServer, ServerConfig, serve};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// The pages the stub helper writes; well above [`FD_MARGIN`].
const PAGES: u32 = 200;
/// Descriptors the test allows above its measured baseline. The task must run
/// with fewer free descriptors than the page count.
const FD_MARGIN: u64 = 100;
/// The input's bytes; the engine never decodes them.
const IMAGE_BASE64: &str = "aW1hZ2U=";

fn home() -> std::path::PathBuf {
    let home =
        std::path::PathBuf::from("/tmp").join(format!("dm-descriptors-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    home
}

/// The stub helper: writes `result.json`, `text.txt` and [`PAGES`] page
/// images, one file at a time.
fn stub_helper(home: &std::path::Path) -> std::path::PathBuf {
    let path = home.join("stub-ocr-helper.sh");
    std::fs::write(
        &path,
        format!(
            r#"#!/bin/sh
artifacts=""
while [ $# -gt 0 ]; do
  case "$1" in
    --artifacts) artifacts="$2"; shift 2 ;;
    --output-mode) shift 2 ;;
    *) shift ;;
  esac
done
pages={PAGES}
{{
  printf '{{"text":"descriptor test","pageCount":%d,"pages":[' "$pages"
  i=1
  while [ "$i" -le "$pages" ]; do
    if [ "$i" -gt 1 ]; then printf ','; fi
    printf '{{"page":%d,"text":"p%d","width":1,"height":1,"regions":[]}}' "$i" "$i"
    i=$((i + 1))
  done
  printf ']}}'
}} > "$artifacts/result.json"
printf 'descriptor test\n' > "$artifacts/text.txt"
i=1
while [ "$i" -le "$pages" ]; do
  printf 'PNG' > "$artifacts/$(printf 'page-%04d.png' "$i")"
  i=$((i + 1))
done
printf '{{"ok":true,"pageCount":%d}}\n' "$pages"
"#
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

async fn launch(home: &std::path::Path) -> RunningServer {
    let mut config = ServerConfig {
        product_home: Some(home.to_path_buf()),
        target_home: Some(home.join("native")),
        ..Default::default()
    };
    config.unix_path = Some(std::path::PathBuf::from("/tmp").join(format!(
        "dim-modality-descriptors-{}/server.sock",
        uuid::Uuid::new_v4()
    )));
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(Arc::new(dim_agent_runtime::MemoryTimelineStore::default()));
    defaults::install(&mut builder);
    defaults::mock_model_hosts(&mut builder);
    dim_agent_capabilities::defaults::tool_host(&mut builder, Arc::new(MockTools));
    dim_app_server::collaboration::install(&mut builder);
    dim_modality::install_with(
        &mut builder,
        dim_modality::InstallOptions {
            ocr_helper: Some(stub_helper(home)),
            ..Default::default()
        },
    );
    serve(builder, config).await.unwrap()
}

struct Client {
    stream: BufReader<UnixStream>,
    id: u64,
}

impl Client {
    async fn connect(server: &RunningServer) -> Self {
        let stream = BufReader::new(
            UnixStream::connect(server.unix_path.as_ref().unwrap())
                .await
                .unwrap(),
        );
        let mut this = Self { stream, id: 0 };
        let reply = this
            .rpc("hello", json!({"protocolVersion":3,"clientKind":"other"}))
            .await;
        assert_eq!(reply["result"]["protocolVersion"], 3);
        this
    }

    async fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        let request = json!({"jsonrpc":"2.0","id":self.id,"method":method,"params":params});
        self.stream
            .get_mut()
            .write_all(request.to_string().as_bytes())
            .await
            .unwrap();
        self.stream.get_mut().write_all(b"\n").await.unwrap();
        loop {
            let value = tokio::time::timeout(Duration::from_secs(30), async {
                let mut line = String::new();
                assert_ne!(
                    self.stream.read_line(&mut line).await.unwrap(),
                    0,
                    "connection unexpectedly closed"
                );
                serde_json::from_str::<Value>(&line).unwrap()
            })
            .await
            .unwrap_or_else(|error| panic!("deadline waiting for {method}: {error}"));
            if value["id"] == self.id {
                return value;
            }
        }
    }
}

fn assert_ok(reply: &Value) {
    assert!(reply.get("error").is_none(), "{reply}");
}

async fn wait_terminal(client: &mut Client, task_id: &str) -> Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let reply = client
            .rpc(
                "modality.task.wait",
                json!({"task_id": task_id, "timeout_ms": 1000}),
            )
            .await;
        assert_ok(&reply);
        let task = reply["result"]["task"].clone();
        let state = task["state"].as_str().unwrap();
        if !matches!(state, "queued" | "running") {
            return task;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "task {task_id} never finished: {task}"
        );
    }
}

async fn result_bytes(client: &mut Client, task_id: &str, name: &str, length: usize) -> Vec<u8> {
    let reply = client
        .rpc(
            "modality.task.result",
            json!({"task_id": task_id, "name": name, "offset": 0, "length": length}),
        )
        .await;
    assert_ok(&reply);
    base64::engine::general_purpose::STANDARD
        .decode(reply["result"]["data_base64"].as_str().unwrap())
        .unwrap()
}

/// The descriptors this process holds now.
fn open_descriptors() -> u64 {
    std::fs::read_dir("/dev/fd")
        .map(|entries| entries.count() as u64)
        .unwrap_or(0)
}

/// Lowers the soft descriptor limit to the current usage plus [`FD_MARGIN`].
fn lower_descriptor_limit() -> (u64, u64) {
    let mut limits = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limits) },
        0
    );
    let baseline = open_descriptors();
    let target = baseline + FD_MARGIN;
    assert!(
        target <= limits.rlim_max,
        "cannot lower to {target} above the hard limit {}",
        limits.rlim_max
    );
    let lowered = libc::rlimit {
        rlim_cur: target,
        rlim_max: limits.rlim_max,
    };
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &lowered) }, 0);
    (baseline, target)
}

fn restore_descriptor_limit(soft: u64, hard: u64) {
    let limits = libc::rlimit {
        rlim_cur: soft,
        rlim_max: hard,
    };
    let _ = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limits) };
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_layout_task_publishes_every_page_under_a_low_descriptor_limit() {
    let home = home();
    let server = launch(&home).await;
    let mut client = Client::connect(&server).await;

    let mut original = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut original) },
        0
    );
    let (baseline, limit) = lower_descriptor_limit();
    eprintln!("descriptor baseline: {baseline}, soft limit: {limit}");

    let submitted = client
        .rpc(
            "modality.task.submit",
            json!({
                "category": "ocr",
                "operation": "recognize",
                "params": {"outputMode": "layout"},
                "files": [{
                    "name": "scan.png",
                    "media_type": "image/png",
                    "data_base64": IMAGE_BASE64,
                }],
                "client_task_id": "descriptor-bound",
            }),
        )
        .await;
    assert_ok(&submitted);
    let task_id = submitted["result"]["task"]["task_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let done = wait_terminal(&mut client, &task_id).await;
    assert_eq!(done["state"], "succeeded", "{done}");

    // Every page is published at the size the helper wrote, with no page lost
    // to a descriptor shortage.
    let files = done["result"]["files"].as_array().unwrap();
    assert_eq!(files.len(), PAGES as usize + 2, "{done}");
    let mut published: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    for file in files {
        published.insert(
            file["name"].as_str().unwrap().to_owned(),
            file["bytes"].as_u64().unwrap(),
        );
    }
    assert_eq!(published.get("text.txt"), Some(&16), "{published:?}");
    assert!(published.get("result.json").is_some_and(|bytes| *bytes > 0));
    for page in 1..=PAGES {
        let name = format!("page-{page:04}.png");
        assert_eq!(published.get(&name), Some(&3), "{name}: {published:?}");
    }

    // data.pages is the published result.json.
    let result_json = result_bytes(&mut client, &task_id, "result.json", 1 << 20).await;
    let document: Value = serde_json::from_slice(&result_json).unwrap();
    let expected: Vec<Value> = document["pages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|page| json!({"page": page["page"], "text": page["text"]}))
        .collect();
    assert_eq!(expected.len(), PAGES as usize);
    assert_eq!(done["result"]["data"]["pages"], json!(expected), "{done}");

    restore_descriptor_limit(original.rlim_cur, original.rlim_max);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}
