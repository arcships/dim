//! Cold `session.attach` cost by stored size, over a disposable native store.
//!
//! ```text
//! cargo run --release --locked -p dim-app-server --example attach_cost -- \
//!     [plain|product] [1001,5001,6865] [7]
//! ```
//!
//! The first phase serves a daemon on a fresh database in a temporary home and
//! grows one Session per size on `main` through real mock-model turns: a plain
//! turn submits text (one model call, 10 events); a product turn alternates text
//! and `tool:echo` with 1,000 bytes, under the product context window and
//! summary strategy, so the window compacts. The Sessions are left stored. The
//! later phases are daemon restarts: each serves a new daemon on the same
//! database, attaches every Session once (cold: the Session is not loaded in
//! that process, and its store has not validated it), then attaches it again in
//! the same process (warm: the Session stays loaded and leased). `attachMs` is
//! `session.attach` from sending the request to reading its reply.
//!
//! The process is not new per restart, so the OS page cache and the allocator
//! are warm; a real daemon start adds its own startup, which is outside the
//! timed call.
//!
//! The home is a fresh directory under `ATTACH_COST_ROOT` (default
//! `/tmp/dim-attach-<uuid>`, a short path for the Unix socket), removed at the
//! end. Every cold attach appends one `branch/resume`, so later repetitions
//! time a store one event larger per prior repetition; `events` is the head
//! before the first cold attach.
use dim_agent_capabilities::{adapters::MockTools, defaults};
use dim_agent_runtime::*;
use dim_app_server::{RunningServer, ServerConfig, serve};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};
#[cfg(unix)]
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

#[cfg(unix)]
#[allow(dead_code)]
#[path = "support/product.rs"]
mod product_support;

type Error = Box<dyn std::error::Error + Send + Sync>;

#[cfg(unix)]
struct Client {
    stream: BufReader<UnixStream>,
    id: u64,
    notices: Vec<Value>,
}

#[cfg(unix)]
impl Client {
    async fn new(server: &RunningServer) -> std::result::Result<Self, Error> {
        let stream = UnixStream::connect(server.unix_path.as_ref().ok_or("no unix path")?).await?;
        let mut this = Self {
            stream: BufReader::new(stream),
            id: 0,
            notices: vec![],
        };
        let result = this
            .rpc("hello", json!({"protocolVersion":3,"clientKind":"other"}))
            .await?;
        if result.get("error").is_some() {
            return Err(format!("hello failed: {result}").into());
        }
        Ok(this)
    }

    async fn send(&mut self, method: &str, params: Value) -> std::result::Result<u64, Error> {
        self.id += 1;
        let text =
            json!({"jsonrpc":"2.0","id":self.id,"method":method,"params":params}).to_string();
        self.stream.get_mut().write_all(text.as_bytes()).await?;
        self.stream.get_mut().write_all(b"\n").await?;
        Ok(self.id)
    }

    async fn read(&mut self) -> std::result::Result<Value, Error> {
        let mut line = String::new();
        if self.stream.read_line(&mut line).await? == 0 {
            return Err("connection closed".into());
        }
        Ok(serde_json::from_str(&line)?)
    }

    async fn rpc(&mut self, method: &str, params: Value) -> std::result::Result<Value, Error> {
        let id = self.send(method, params).await?;
        if let Some(index) = self.notices.iter().position(|value| value["id"] == id) {
            return Ok(self.notices.remove(index));
        }
        loop {
            let value = self.read().await?;
            if value["id"] == id {
                return Ok(value);
            }
            self.notices.push(value);
        }
    }

    async fn ok(&mut self, method: &str, params: Value) -> std::result::Result<Value, Error> {
        let value = self.rpc(method, params).await?;
        match value.get("error") {
            Some(error) => Err(format!("{method} failed: {error}").into()),
            None => Ok(value["result"].clone()),
        }
    }

    async fn sequence(&mut self, id: &str) -> std::result::Result<u64, Error> {
        let listed = self.ok("session.list", json!({})).await?;
        let sessions = listed
            .as_array()
            .ok_or("session.list did not return an array")?;
        Ok(sessions
            .iter()
            .find(|session| session["sessionId"] == id)
            .and_then(|session| session["sequence"].as_u64())
            .ok_or("attached Session is not listed")?)
    }
}

#[cfg(unix)]
fn config(home: &Path, socket: &Path) -> ServerConfig {
    let mut config = ServerConfig {
        database_path: Some(home.join("dimcode.sqlite")),
        unix_path: Some(socket.to_path_buf()),
        ..Default::default()
    };
    config.session_defaults.cwd = home.display().to_string();
    config
}

#[cfg(unix)]
async fn start(
    home: &Path,
    socket: &Path,
    mix: &str,
) -> std::result::Result<(RunningServer, Client), Error> {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    if mix == "product" {
        product_support::product(&mut builder)?;
        product_support::product_process(&mut builder);
    } else {
        defaults::install(&mut builder);
        defaults::mock_model_hosts(&mut builder);
        defaults::tool_host(&mut builder, Arc::new(MockTools));
    }
    let server = serve(builder, config(home, socket)).await?;
    let client = Client::new(&server).await?;
    Ok((server, client))
}

#[cfg(unix)]
async fn stop(server: RunningServer) -> std::result::Result<(), Error> {
    server.shutdown();
    server.join().await?;
    Ok(())
}

#[cfg(unix)]
#[tokio::main]
async fn main() -> std::result::Result<(), Error> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
    let mut args = std::env::args().skip(1);
    let mix = args.next().unwrap_or_else(|| "plain".into());
    if !["plain", "product"].contains(&mix.as_str()) {
        return Err("expected [plain|product] [sizes] [reps]".into());
    }
    let sizes: Vec<u64> = args
        .next()
        .unwrap_or_else(|| "1001,5001,6865".into())
        .split(',')
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()?;
    let reps: usize = args.next().map_or(Ok(7), |value| value.parse())?;
    if sizes.is_empty() || reps == 0 {
        return Err("expected at least one size and one repetition".into());
    }
    let root = std::env::var_os("ATTACH_COST_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let product = std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".dimcode"));
    if let Some(product) = product {
        let root = std::path::absolute(&root)?;
        let product = std::path::absolute(product)?;
        if root == product || root.starts_with(&product) {
            return Err(
                format!("ATTACH_COST_ROOT must not be inside {}", product.display()).into(),
            );
        }
    }
    let directory = root.join(format!("dim-attach-{}", uuid::Uuid::new_v4()));
    if directory.as_os_str().len() > 80 {
        return Err(format!(
            "disposable home path is too long for a Unix socket: {}",
            directory.display()
        )
        .into());
    }
    std::fs::create_dir_all(&directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
    }
    let home = std::fs::canonicalize(&directory)?;
    let socket = |phase: &str| home.join(format!("server-{phase}.sock"));

    let (server, mut client) = start(&home, &socket("grow"), &mix).await?;
    let tools: Value = match mix.as_str() {
        "product" => json!(["echo"]),
        _ => json!([]),
    };
    let policy = match mix.as_str() {
        "product" => json!({"preset":"full-access"}),
        _ => json!({"preset":"read-only"}),
    };
    let mut sessions = vec![];
    for &size in &sizes {
        let created = client
            .ok(
                "session.create",
                json!({"cwd":home,"tools":tools,"permissionPolicy":policy}),
            )
            .await?;
        let id = created["sessionId"]
            .as_str()
            .ok_or("no sessionId")?
            .to_owned();
        let start = Instant::now();
        let mut turn = 0;
        while client.sequence(&id).await? < size {
            let text = match mix.as_str() {
                "product" if turn % 2 == 1 => format!(
                    "tool:echo {{\"text\":\"tool turn {turn} {}\"}}",
                    "x".repeat(1000)
                ),
                "product" => format!("plain turn {turn} {}", "x".repeat(1000)),
                _ => format!("plain turn {turn}"),
            };
            client
                .ok(
                    "session.submit",
                    json!({"sessionId":id,"branchId":"main","text":text}),
                )
                .await?;
            client
                .ok("session.idle", json!({"sessionId":id,"branchId":"main"}))
                .await?;
            turn += 1;
        }
        let events = client.sequence(&id).await?;
        eprintln!(
            "grew {events} events in {turn} turns, {:.0} ms",
            start.elapsed().as_secs_f64() * 1000.
        );
        sessions.push((id, events));
    }
    stop(server).await?;

    let mut cold = std::collections::BTreeMap::<u64, Vec<f64>>::new();
    let mut warm = std::collections::BTreeMap::<u64, Vec<f64>>::new();
    for rep in 0..reps {
        let (server, mut client) = start(&home, &socket(&format!("rep-{rep}")), &mix).await?;
        for (id, events) in &sessions {
            let started = Instant::now();
            client.ok("session.attach", json!({"sessionId":id})).await?;
            let cold_ms = started.elapsed().as_secs_f64() * 1000.;
            let warm_start = Instant::now();
            client.ok("session.attach", json!({"sessionId":id})).await?;
            let warm_ms = warm_start.elapsed().as_secs_f64() * 1000.;
            cold.entry(*events).or_default().push(cold_ms);
            warm.entry(*events).or_default().push(warm_ms);
        }
        stop(server).await?;
    }
    let median = |mut values: Vec<f64>| {
        values.sort_by(f64::total_cmp);
        values[values.len() / 2]
    };
    for (events, cold) in cold {
        let fastest = cold.iter().copied().fold(f64::MAX, f64::min);
        let worst = cold.iter().copied().fold(f64::MIN, f64::max);
        let warm = warm
            .remove(&events)
            .ok_or("missing warm samples for a cold size")?;
        println!(
            "{}",
            json!({"scenario":"attach","mix":mix,"reps":reps,"events":events,
            "attachMinMs":fastest,
            "attachMedianMs":median(cold),
            "attachMaxMs":worst,
            "warmAttachMedianMs":median(warm),
            "debugAssertions":cfg!(debug_assertions)})
        );
    }
    std::fs::remove_dir_all(&directory)?;
    Ok(())
}

#[cfg(not(unix))]
fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    Err("attach_cost requires the protected Unix transport".into())
}
