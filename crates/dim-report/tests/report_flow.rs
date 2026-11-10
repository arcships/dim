#![cfg(unix)]

use base64::{Engine, engine::general_purpose::STANDARD};
use dim_agent_capabilities::{
    defaults,
    history::HistoryRenderer,
    host::{
        EmptyPromptHost, HostError, HostErrorKind, HostFuture, ModelConnection, ModelDriver,
        ModelStream,
    },
};
use dim_agent_runtime::{AgentBuilder, AgentConfig, ConfigureUpdate, testing::branch_events};
use dim_agent_types::{EventKind, ModelOutputOutcome, ModelRequest, ModelSelection};
use dim_app_server::{RunningServer, ServerConfig, serve};
use dim_client::{Client, ConnectionConfig, Server, app_error};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    io::{Cursor, Read},
    os::unix::fs::PermissionsExt,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};
use tokio_util::sync::CancellationToken;

const SECRET: &str = "private-fixture-content";

#[derive(Default)]
struct FailedModel(AtomicUsize);

impl ModelDriver for FailedModel {
    fn invoke(
        &self,
        _: Arc<ModelRequest>,
        _: ModelConnection,
        _: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Box::pin(async { Err(HostError::classified(HostErrorKind::InvalidRequest, SECRET)) })
    }
}

async fn connect(server: &RunningServer) -> Client {
    Client::connect(&ConnectionConfig {
        endpoint: server.unix_path.clone().unwrap(),
        server: Server {
            program: "dimcode".into(),
            embedded: true,
        },
        profile: None,
        client_kind: "other",
        client_name: "Report flow",
        protocol_version: 4,
    })
    .await
    .unwrap()
}

fn archive(result: &Value) -> Value {
    let bytes = STANDARD
        .decode(result["archive"]["base64"].as_str().unwrap())
        .unwrap();
    assert!(bytes.len() <= 64 * 1024);
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    assert_eq!(zip.len(), 2);
    for name in ["report.md", "diagnostics.json"] {
        let mut text = String::new();
        zip.by_name(name)
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        assert_eq!(text, result["files"][name].as_str().unwrap());
        assert!(!text.contains(SECRET));
    }
    assert!(
        result["files"]["report.md"]
            .as_str()
            .unwrap()
            .contains(result["body"].as_str().unwrap())
    );
    serde_json::from_str(result["files"]["diagnostics.json"].as_str().unwrap()).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn report_rpc_keeps_evidence_private_and_returns_the_reviewed_zip() {
    let home = tempfile::Builder::new()
        .prefix("dim-report-")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let assets = home.path().join("web");
    std::fs::create_dir_all(assets.join("assets")).unwrap();
    std::fs::write(
        assets.join("index.html"),
        "<!doctype html><title>Dim</title>",
    )
    .unwrap();
    std::fs::write(assets.join("assets/app.js"), "export {}").unwrap();
    std::fs::write(
        assets.join("dimcode-web.json"),
        r#"{"version":"0.0.1","protocolVersion":4,"sourceCommit":"test"}"#,
    )
    .unwrap();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    let model = Arc::new(FailedModel::default());
    defaults::install_with_prompt(
        &mut builder,
        Arc::new(EmptyPromptHost),
        Some(ConfigureUpdate {
            sources: vec![],
            model: Some(ModelSelection {
                provider_id: "test".into(),
                model_id: "report-fixture".into(),
                capability_snapshot: None,
                config: None,
                metadata: None,
            }),
        }),
    );
    defaults::mock_hosts_with_model(&mut builder, HistoryRenderer::default(), model.clone());
    dim_report::install(&mut builder);
    let server = serve(
        builder,
        ServerConfig {
            platform_home: Some(home.path().into()),
            unix_path: Some(home.path().join("server.sock")),
            web_assets: Some(assets),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut owner = connect(&server).await;
    let mut other = connect(&server).await;
    assert!(
        owner
            .features()
            .iter()
            .any(|method| method == "report.prepare")
    );
    let created = owner
        .rpc("session.create", json!({"cwd": home.path(), "tools":[]}))
        .await
        .unwrap();
    let id = &created["sessionId"];
    let methods = owner
        .rpc("session.methods", json!({"sessionId":id}))
        .await
        .unwrap();
    assert!(
        methods["methods"]
            .as_array()
            .unwrap()
            .iter()
            .any(|decl| decl["name"] == "session.report.prepare")
    );
    owner
        .rpc(
            "session.submit",
            json!({"sessionId":id, "branchId":"main", "text":SECRET}),
        )
        .await
        .unwrap();
    owner
        .rpc("session.idle", json!({"sessionId":id, "branchId":"main"}))
        .await
        .unwrap();
    let session = server
        .agent()
        .unwrap()
        .get_session(id.as_str().unwrap())
        .unwrap();
    let failure = branch_events(&session, "main", ..).await.unwrap().into_iter().find(|event| {
        matches!(&event.kind, EventKind::ModelOutputEnd(end) if matches!(end.outcome, ModelOutputOutcome::Failed { .. }))
    }).unwrap();
    let calls = model.0.load(Ordering::Relaxed);
    let head = session.sequence("main").unwrap();
    let observed = json!({
        "operation":"model_generation", "code":SECRET, "observedAt":"2026-11-10T00:00:00Z", "clientVersion":"0.1.0"
    });
    let error = json!({"kind":"timeline", "branchId":"main", "sequence":failure.sequence, "observed":observed});
    let prepared = owner
        .rpc(
            "session.report.prepare",
            json!({"sessionId":id, "error":error}),
        )
        .await
        .unwrap();
    assert_eq!(
        prepared["defaultModel"],
        json!({"providerId":"test","modelId":"report-fixture"})
    );
    assert_eq!(prepared["plan"]["generation"]["kind"], "local");
    let evidence = &prepared["plan"]["evidence"];
    assert_eq!(
        evidence["timeline"]["failure"]["sequence"],
        failure.sequence
    );
    assert!(evidence["timeline"].get("correlation").is_none());
    assert!(!prepared.to_string().contains(SECRET));

    let report_id = &prepared["reportId"];
    let original = json!({"sessionId":id,"reportId":report_id,"planId":prepared["plan"]["planId"]});
    let foreign = other
        .rpc("session.report.generate", original.clone())
        .await
        .unwrap_err();
    assert_eq!(app_error(&foreign), Some("report_not_found"));
    let unavailable = owner.rpc("session.report.plan", json!({
        "sessionId":id, "reportId":report_id, "generation":{"kind":"model", "model":{"providerId":"test","modelId":"report-fixture"}}, "redact":true
    })).await.unwrap_err();
    assert_eq!(app_error(&unavailable), Some("report_model_unavailable"));
    let redacted = owner
        .rpc(
            "session.report.plan",
            json!({
                "sessionId":id, "reportId":report_id, "generation":{"kind":"local"},
                "redact":true, "instructions":"keep correlation ids, omit timeline"
            }),
        )
        .await
        .unwrap();
    assert_eq!(redacted["instructionsStatus"], "partially_applied");
    assert_eq!(redacted["evidence"]["unavailable"], "omitted");
    assert!(redacted["evidence"].get("timeline").is_none());
    let plan = owner
        .rpc(
            "session.report.plan",
            json!({
                "sessionId":id, "reportId":report_id, "generation":{"kind":"local"}, "redact":false,
                "instructions":format!("omit error codes, {SECRET}"),
            }),
        )
        .await
        .unwrap();
    assert_eq!(plan["instructionsStatus"], "partially_applied");
    let stale = owner
        .rpc("session.report.generate", original)
        .await
        .unwrap_err();
    assert_eq!(app_error(&stale), Some("report_plan_stale"));
    let inactive = owner.rpc("session.report.cancel", json!({
        "sessionId":id, "reportId":report_id, "target":"generation", "planId":prepared["plan"]["planId"]
    })).await.unwrap();
    assert_eq!(inactive["cancelled"], false);
    let generated = owner
        .rpc(
            "session.report.generate",
            json!({
                "sessionId":id,"reportId":report_id,"planId":plan["planId"]
            }),
        )
        .await
        .unwrap();
    let diagnostics = archive(&generated);
    assert_eq!(diagnostics["version"], 1);
    assert!(diagnostics.get("architecture").is_none());
    let mut reviewed = plan["evidence"].clone();
    let unavailable = reviewed
        .as_object_mut()
        .unwrap()
        .remove("unavailable")
        .unwrap();
    assert_eq!(diagnostics["evidence"], reviewed);
    assert_eq!(diagnostics["unavailable"], unavailable);
    assert_eq!(
        diagnostics["evidence"]["timeline"]["correlation"]["eventId"],
        failure.id
    );
    assert!(
        diagnostics["evidence"]["capturedError"]
            .get("code")
            .is_none()
    );
    assert_eq!(model.0.load(Ordering::Relaxed), calls);

    assert_eq!(session.sequence("main").unwrap(), head);

    let missing = owner
        .rpc(
            "session.report.prepare",
            json!({"sessionId":id, "error":{
                "kind":"timeline","branchId":"missing","sequence":1,"observed":observed
            }}),
        )
        .await
        .unwrap();
    assert_eq!(
        missing["plan"]["evidence"]["unavailable"],
        "failure_unavailable"
    );
    let cancelled = owner
        .rpc(
            "session.report.cancel",
            json!({
                "sessionId":id, "reportId":missing["reportId"], "target":"report"
            }),
        )
        .await
        .unwrap();
    assert_eq!(cancelled["cancelled"], true);
    let cancelled = owner
        .rpc(
            "session.report.generate",
            json!({
                "sessionId":id, "reportId":missing["reportId"], "planId":missing["plan"]["planId"]
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(app_error(&cancelled), Some("report_not_found"));
    let no_session = owner
        .rpc(
            "session.report.prepare",
            json!({"sessionId":"missing","error":error}),
        )
        .await;
    assert!(no_session.is_err());
    let rejected = owner
        .rpc("report.prepare", json!({"error":error}))
        .await
        .unwrap_err();
    assert_eq!(app_error(&rejected), Some("invalid_params"));

    let observed = json!({"error":{"kind":"observed","observed":{
        "operation":"session_read","code":"session_closed","observedAt":"2026-11-10T00:00:00Z","clientVersion":"0.1.0"
    }}});
    let mut invalid = observed.clone();
    invalid["error"]["observed"]["message"] = SECRET.into();
    let rejected = owner.rpc("report.prepare", invalid).await.unwrap_err();
    assert_eq!(app_error(&rejected), Some("invalid_params"));

    let opened = owner.rpc("web.open", json!({"port":0})).await.unwrap();
    let address = opened["address"].as_str().unwrap();
    let url = url::Url::parse(opened["url"].as_str().unwrap()).unwrap();
    let token = url.fragment().unwrap().strip_prefix("bootstrap=").unwrap();
    let origin = format!("http://{address}");
    let mut request = format!("ws://{address}/ws").into_client_request().unwrap();
    request
        .headers_mut()
        .insert("Origin", origin.parse().unwrap());
    assert!(
        tokio_tungstenite::connect_async(request.clone())
            .await
            .is_err()
    );
    let exchange = reqwest::Client::new()
        .post(format!("{origin}/api/auth/exchange"))
        .header("Origin", &origin)
        .json(&json!({"token":token}))
        .send()
        .await
        .unwrap();
    assert_eq!(exchange.status(), 204);
    let cookie = exchange.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    request
        .headers_mut()
        .insert("Cookie", cookie.parse().unwrap());
    let (mut web, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let mut rpc = async |id: u32, method: &str, params: Value| {
        web.send(Message::Text(
            json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
        loop {
            let frame = web.next().await.unwrap().unwrap();
            if let Message::Text(text) = frame {
                let reply: Value = serde_json::from_str(&text).unwrap();
                if reply["id"] == id {
                    return reply["result"]
                        .as_object()
                        .map(|_| reply["result"].clone())
                        .unwrap_or_else(|| panic!("{reply}"));
                }
            }
        }
    };
    let hello = rpc(1, "hello", json!({"protocolVersion":4, "clientKind":"web"})).await;
    assert!(
        hello["features"]
            .as_array()
            .unwrap()
            .contains(&json!("report.prepare"))
    );
    let prepared = rpc(2, "report.prepare", observed).await;
    let generated = rpc(
        3,
        "report.generate",
        json!({"reportId":prepared["reportId"], "planId":prepared["plan"]["planId"]}),
    )
    .await;
    let diagnostics = archive(&generated);
    assert_eq!(diagnostics["unavailable"], "no_reference");
    assert_eq!(
        diagnostics["evidence"]["capturedError"]["code"],
        "session_closed"
    );
    assert_eq!(model.0.load(Ordering::Relaxed), calls);

    drop(web);
    drop(owner);
    drop(other);
    server.shutdown();
    server.join().await.unwrap();
}
