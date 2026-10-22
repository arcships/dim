//! Public embedding example: isolated native storage and a protected Unix connection.
#[cfg(unix)]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use dim_agent_capabilities::defaults;
    use dim_agent_runtime::{AgentBuilder, AgentConfig};
    use dim_app_server::{ServerConfig, serve, session_typescript::SessionTypeScriptConfig};
    use serde_json::{Value, json};
    use std::path::PathBuf;
    use tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
        net::UnixStream,
    };

    async fn rpc(
        stream: &mut BufReader<UnixStream>,
        id: u64,
        method: &str,
        params: Value,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        let mut frame = serde_json::to_vec(
            &json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}),
        )?;
        frame.push(b'\n');
        stream.get_mut().write_all(&frame).await?;
        loop {
            let mut line = String::new();
            if stream.read_line(&mut line).await? == 0 {
                return Err("server disconnected".into());
            }
            let reply: Value = serde_json::from_str(&line)?;
            if reply["id"] == id {
                return Ok(reply);
            }
        }
    }

    let mut args = std::env::args().skip(1);
    let node = PathBuf::from(
        args.next()
            .ok_or("usage: session_typescript /absolute/node /absolute/rutis-runtime")?,
    );
    let js_runtime = PathBuf::from(args.next().ok_or("missing runtime package path")?);
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let home = std::env::temp_dir().join(format!(
        "dm-ts-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    let socket = home.join("daemon.sock");
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    defaults::mock_hosts(&mut builder);
    let server = serve(builder, ServerConfig {
        unix_path: Some(socket.clone()), websocket_address: None,
        database_path: Some(home.join("sessions.sqlite")),
        session_typescript: Some(SessionTypeScriptConfig {
        process_rows: Vec::new(),
            node, js_runtime, anchor: workspace.join("runtime/package.json"),
            rows: vec![json!({
                "id":"typescript", "name":workspace.join("runtime/fixtures/session-part.ts"),
                "provides":serde_json::from_str::<Value>(include_str!("../../../runtime/fixtures/session-part.json"))?,
            })],
        }), ..Default::default()
    }).await?;
    let mut stream = BufReader::new(UnixStream::connect(&socket).await?);
    println!(
        "{}",
        rpc(
            &mut stream,
            1,
            "hello",
            json!({"protocolVersion":3,"clientKind":"other"})
        )
        .await?
    );
    let created = rpc(&mut stream, 2, "session.create", json!({"cwd":workspace,"tools":["echo","ts_echo"],"permissionPolicy":{"allowedTools":["echo","ts_echo"]}})).await?;
    let session_id = created["result"]["sessionId"]
        .as_str()
        .ok_or("session.create failed")?;
    println!(
        "{}",
        rpc(
            &mut stream,
            3,
            "session.typescript.status",
            json!({"sessionId":session_id})
        )
        .await?
    );
    // Optional rows may still be Pending. A listed inactive method is retryable;
    // tools omitted at the first sample wait for the next registry reload.
    // Successful submit/idle replies acknowledge native processing; they do not
    // certify that an omitted TypeScript tool executed. Inspect durable history.
    println!("{}", rpc(&mut stream, 4, "session.submit", json!({"sessionId":session_id,"branchId":"main","text":"tool:ts_echo {\"text\":\"hello\"}"})).await?);
    println!(
        "{}",
        rpc(
            &mut stream,
            5,
            "session.idle",
            json!({"sessionId":session_id,"branchId":"main"})
        )
        .await?
    );
    println!(
        "{}",
        rpc(
            &mut stream,
            6,
            "session.history",
            json!({"sessionId":session_id,"branchId":"main","after":0,"limit":100})
        )
        .await?
    );
    server.shutdown();
    server.join().await?;
    std::fs::remove_dir_all(home)?;
    Ok(())
}

#[cfg(not(unix))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    Err("this embedding example requires the protected Unix transport".into())
}
