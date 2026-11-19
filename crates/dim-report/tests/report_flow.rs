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
use dim_agent_types::{
    ContentBlock, EventKind, InputMessageData, InputMode, ModelOutputOutcome, ModelRequest,
    ModelSelection, markup::runtime_context,
};
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

fn report_home() -> tempfile::TempDir {
    let home = tempfile::Builder::new()
        .prefix("dim-report-")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    home
}

fn report_builder() -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
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
    dim_report::install(&mut builder);
    builder
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_report_names_the_daemons_build_and_a_client_of_another_one() {
    let home = report_home();
    let mut builder = report_builder();
    defaults::mock_hosts(&mut builder);
    let server = serve(
        builder,
        ServerConfig {
            platform_home: Some(home.path().into()),
            unix_path: Some(home.path().join("server.sock")),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut owner = connect(&server).await;
    let label = dim_agent_capabilities::build::current();
    let generated = async |client: &mut Client, claimed: &str| {
        let observed = json!({
            "operation":"session_read", "code":"session_closed",
            "observedAt":"2026-11-10T00:00:00Z", "clientVersion":claimed,
        });
        let prepared = client
            .rpc(
                "report.prepare",
                json!({"error":{"kind":"observed","observed":observed}}),
            )
            .await
            .unwrap();
        client
            .rpc(
                "report.generate",
                json!({"reportId":prepared["reportId"], "planId":prepared["plan"]["planId"]}),
            )
            .await
            .unwrap()
    };

    let report = generated(&mut owner, &label).await;
    assert!(
        report["body"]
            .as_str()
            .unwrap()
            .contains(&format!("- Dim {label} (protocol 4)\n")),
        "the Dim line names the label this build reports: {}",
        report["body"]
    );
    assert!(
        report["body"]
            .as_str()
            .unwrap()
            .contains("- Observed at 2026-11-10T00:00:00+00:00\n"),
        "the observed instant keeps its UTC offset: {}",
        report["body"]
    );
    let diagnostics = archive(&report);
    assert_eq!(
        diagnostics["evidence"]["process"]["productVersion"],
        json!(label)
    );
    assert!(
        diagnostics["evidence"]["process"]
            .get("clientVersion")
            .is_none(),
        "a client of this build adds no line"
    );

    let report = generated(&mut owner, "0.0.0-alpha.1-source+ffffffff").await;
    assert!(
        report["body"]
            .as_str()
            .unwrap()
            .contains("- Client 0.0.0-alpha.1-source+ffffffff\n"),
        "a report reveals a client running another build: {}",
        report["body"]
    );
    let diagnostics = archive(&report);
    assert_eq!(
        diagnostics["evidence"]["process"]["clientVersion"],
        json!("0.0.0-alpha.1-source+ffffffff")
    );

    drop(owner);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn report_rpc_captures_a_session_without_an_error_at_a_fixed_cutoff() {
    let home = report_home();
    let mut builder = report_builder();
    defaults::mock_hosts(&mut builder);
    let server = serve(
        builder,
        ServerConfig {
            platform_home: Some(home.path().into()),
            unix_path: Some(home.path().join("server.sock")),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut owner = connect(&server).await;
    let created = owner
        .rpc("session.create", json!({"cwd":home.path(), "tools":[]}))
        .await
        .unwrap();
    let id = &created["sessionId"];

    let prepare = json!({"sessionId":id});
    let before_first_turn = owner
        .rpc("session.report.prepare", prepare.clone())
        .await
        .unwrap();
    assert_eq!(
        before_first_turn["plan"]["evidence"]["timeline"]["recent"][0]["kind"],
        "branch/start"
    );
    let rejected = owner.rpc("report.prepare", json!({})).await.unwrap_err();
    assert_eq!(app_error(&rejected), Some("invalid_params"));
    let rejected = owner
        .rpc(
            "session.report.prepare",
            json!({"sessionId":id,"error":null}),
        )
        .await
        .unwrap_err();
    assert_eq!(app_error(&rejected), Some("invalid_params"));

    let turn = async |owner: &mut Client| {
        owner
            .rpc(
                "session.submit",
                json!({"sessionId":id,"branchId":"main","text":SECRET}),
            )
            .await
            .unwrap();
        owner
            .rpc("session.idle", json!({"sessionId":id,"branchId":"main"}))
            .await
            .unwrap();
    };
    for _ in 0..4 {
        turn(&mut owner).await;
    }
    let session = server
        .agent()
        .unwrap()
        .get_session(id.as_str().unwrap())
        .unwrap();
    let cutoff = session.sequence("main").unwrap();
    let prepared = owner.rpc("session.report.prepare", prepare).await.unwrap();
    assert_eq!(
        prepared["defaultModel"],
        json!({"providerId":"test","modelId":"report-fixture"})
    );
    let evidence = &prepared["plan"]["evidence"];
    assert!(evidence.get("capturedError").is_none());
    assert_eq!(evidence["timeline"]["truncated"], true);
    let recent = evidence["timeline"]["recent"].as_array().unwrap();
    assert!(recent.len() <= 16);
    assert!(recent.iter().any(
        |record| record["kind"] == "step/model-output-end" && record["outcome"] == "completed"
    ));
    assert!(recent.iter().all(|record| {
        let sequence = record["sequence"].as_u64().unwrap();
        (cutoff - 31..=cutoff).contains(&sequence)
    }));
    assert!(!prepared.to_string().contains(SECRET));

    turn(&mut owner).await;
    let head = session.sequence("main").unwrap();
    assert!(head > cutoff);
    let plan = owner
        .rpc(
            "session.report.plan",
            json!({"sessionId":id,"reportId":prepared["reportId"],"generation":{"kind":"local"},"redact":true}),
        )
        .await
        .unwrap();
    let generated = owner
        .rpc(
            "session.report.generate",
            json!({
                "sessionId":id,"reportId":prepared["reportId"],"planId":plan["planId"]
            }),
        )
        .await
        .unwrap();
    assert_eq!(generated["title"], "Session report");
    let diagnostics = archive(&generated);
    assert_eq!(diagnostics["version"], 2);
    assert!(diagnostics["evidence"].get("capturedError").is_none());
    assert_eq!(diagnostics["evidence"]["timeline"], evidence["timeline"]);
    assert_eq!(session.sequence("main").unwrap(), head);

    drop(owner);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn report_rpc_keeps_evidence_private_and_returns_the_reviewed_zip() {
    let home = report_home();
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
    let mut builder = report_builder();
    let model = Arc::new(FailedModel::default());
    defaults::mock_hosts_with_model(&mut builder, HistoryRenderer::default(), model.clone());
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conversation_excludes_unconsumed_inputs_and_multiline_runtime_context() {
    let home = report_home();
    let mut builder = report_builder();
    defaults::mock_hosts_with_model(
        &mut builder,
        HistoryRenderer::default(),
        Arc::new(FailedModel::default()),
    );
    let server = serve(
        builder,
        ServerConfig {
            platform_home: Some(home.path().into()),
            unix_path: Some(home.path().join("server.sock")),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut owner = connect(&server).await;
    let created = owner
        .rpc("session.create", json!({"cwd":home.path(),"tools":[]}))
        .await
        .unwrap();
    let id = &created["sessionId"];
    let session = server
        .agent()
        .unwrap()
        .get_session(id.as_str().unwrap())
        .unwrap();
    let visible = "Operator-visible sentence.";
    let hidden = "<dim-hooks kind=\"prompt-submit\">\nHIDDEN-MULTILINE-CONTEXT\nAnother private instruction.\n</dim-hooks>";
    let mut input = InputMessageData::text(visible, Some(InputMode::Queue));
    input
        .user
        .as_mut()
        .unwrap()
        .content
        .push(ContentBlock::Text {
            text: hidden.into(),
        });
    let submitted = session.input(input, "main").await.unwrap();
    session.idle("main").await.unwrap();
    let pending = session
        .input(
            InputMessageData::text("PENDING-NOT-CONVERSATION", Some(InputMode::Append)),
            "main",
        )
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let events = branch_events(&session, "main", ..).await.unwrap();
    let EventKind::InputMessage(input) = &submitted.kind else {
        panic!("expected input")
    };
    assert!(input.user.as_ref().unwrap().content.iter().enumerate().any(|(index, block)| matches!(block, ContentBlock::Text { text } if runtime_context(index, text) && text.contains("HIDDEN-MULTILINE-CONTEXT"))));
    assert!(events.iter().any(|event| matches!(&event.kind, EventKind::InputConsumed(data) if data.input_id == submitted.id)));
    assert!(!events.iter().any(
        |event| matches!(&event.kind, EventKind::InputConsumed(data) if data.input_id == pending.id)
    ));
    let prepared = owner
        .rpc("session.report.prepare", json!({"sessionId":id}))
        .await
        .unwrap();
    let plan = owner.rpc("session.report.plan", json!({"sessionId":id,"reportId":prepared["reportId"],"generation":{"kind":"local"},"redact":false})).await.unwrap();
    let excerpts = plan["evidence"]["conversation"].as_array().unwrap();
    assert!(
        excerpts
            .iter()
            .any(|entry| entry["role"] == "user" && entry["text"] == visible)
    );
    for forbidden in [
        "PENDING-NOT-CONVERSATION",
        "HIDDEN-MULTILINE-CONTEXT",
        "Another private instruction.",
        "dim-hooks",
    ] {
        assert!(
            !excerpts.iter().any(|entry| entry["role"] == "user"
                && entry["text"].as_str().unwrap().contains(forbidden))
        );
    }
    let generated = owner
        .rpc(
            "session.report.generate",
            json!({"sessionId":id,"reportId":prepared["reportId"],"planId":plan["planId"]}),
        )
        .await
        .unwrap();
    archive(&generated);
    for forbidden in [
        "PENDING-NOT-CONVERSATION",
        "HIDDEN-MULTILINE-CONTEXT",
        "Another private instruction.",
        "dim-hooks",
    ] {
        assert!(!plan.to_string().contains(forbidden));
        assert!(!generated.to_string().contains(forbidden));
    }
    drop(owner);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conversation_text_is_opt_in_anchored_and_only_in_the_report_member() {
    let home = report_home();
    let mut builder = report_builder();
    defaults::mock_hosts(&mut builder);
    let server = serve(
        builder,
        ServerConfig {
            platform_home: Some(home.path().into()),
            unix_path: Some(home.path().join("server.sock")),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut owner = connect(&server).await;
    let created = owner
        .rpc("session.create", json!({"cwd":home.path(),"tools":[]}))
        .await
        .unwrap();
    let id = &created["sessionId"];
    let submit = async |owner: &mut Client, text: &str| {
        owner
            .rpc(
                "session.submit",
                json!({"sessionId":id,"branchId":"main","text":text}),
            )
            .await
            .unwrap();
        owner
            .rpc("session.idle", json!({"sessionId":id,"branchId":"main"}))
            .await
            .unwrap();
    };
    let prompt = "Explain why the sky is blue in one sentence.";
    submit(&mut owner, prompt).await;
    let prepared = owner
        .rpc("session.report.prepare", json!({"sessionId":id}))
        .await
        .unwrap();
    assert!(!prepared.to_string().contains(prompt));
    let default = owner.rpc("session.report.generate", json!({"sessionId":id,"reportId":prepared["reportId"],"planId":prepared["plan"]["planId"]})).await.unwrap();
    archive(&default);
    assert!(!default.to_string().contains(prompt));

    let prepared = owner
        .rpc("session.report.prepare", json!({"sessionId":id}))
        .await
        .unwrap();
    submit(&mut owner, "LATER-BEFORE-PLAN").await;
    let plan = owner.rpc("session.report.plan", json!({"sessionId":id,"reportId":prepared["reportId"],"generation":{"kind":"local"},"redact":false})).await.unwrap();
    let excerpts = plan["evidence"]["conversation"].as_array().unwrap();
    assert!(
        excerpts
            .iter()
            .any(|entry| entry["role"] == "user" && entry["text"] == prompt)
    );
    assert!(excerpts.iter().any(|entry| entry["role"] == "assistant" && entry["text"] == format!("Echo: {prompt}")));
    assert!(!plan.to_string().contains("LATER-BEFORE-PLAN"));
    submit(&mut owner, "LATER-BEFORE-GENERATE").await;
    let generated = owner
        .rpc(
            "session.report.generate",
            json!({"sessionId":id,"reportId":prepared["reportId"],"planId":plan["planId"]}),
        )
        .await
        .unwrap();
    archive(&generated);
    let report = generated["files"]["report.md"].as_str().unwrap();
    assert!(report.contains(prompt));
    assert!(report.contains(&format!("Echo: {prompt}")));
    assert!(!report.contains("LATER-"));
    for value in [
        &generated["files"]["diagnostics.json"],
        &generated["sharing"],
        &generated["body"],
    ] {
        assert!(!value.to_string().contains(prompt));
        assert!(!value.to_string().contains("Echo:"));
    }

    submit(&mut owner, "Why did /report return 2/3?\nAuthorization: Bearer fixture-not-a-real-credential\napi_key=fixture-value").await;
    let prepared = owner
        .rpc("session.report.prepare", json!({"sessionId":id}))
        .await
        .unwrap();
    let plan = owner.rpc("session.report.plan", json!({"sessionId":id,"reportId":prepared["reportId"],"generation":{"kind":"local"},"redact":false})).await.unwrap();
    let preview = plan.to_string();
    assert!(preview.contains("Why did /report return 2/3?"));
    for forbidden in ["fixture-not-a-real-credential", "fixture-value"] {
        assert!(!preview.contains(forbidden));
    }
    let hidden = owner.rpc("session.report.plan", json!({"sessionId":id,"reportId":prepared["reportId"],"generation":{"kind":"local"},"redact":true})).await.unwrap();
    assert!(hidden["evidence"].get("conversation").is_none());
    let omitted = owner.rpc("session.report.plan", json!({"sessionId":id,"reportId":prepared["reportId"],"generation":{"kind":"local"},"redact":false,"instructions":"omit timeline"})).await.unwrap();
    assert!(omitted["evidence"].get("conversation").is_none());
    for (label, eol) in [("LF", "\n"), ("CRLF", "\r\n"), ("CR", "\r")] {
        let message = format!("Example {label}{eol}{eol}# Literal heading");
        submit(&mut owner, &message).await;
        let prepared = owner
            .rpc("session.report.prepare", json!({"sessionId":id}))
            .await
            .unwrap();
        let plan = owner.rpc("session.report.plan", json!({"sessionId":id,"reportId":prepared["reportId"],"generation":{"kind":"local"},"redact":false})).await.unwrap();
        let generated = owner
            .rpc(
                "session.report.generate",
                json!({"sessionId":id,"reportId":prepared["reportId"],"planId":plan["planId"]}),
            )
            .await
            .unwrap();
        archive(&generated);
        let report = generated["files"]["report.md"].as_str().unwrap();
        for prefix in ["", "Echo: "] {
            let literal = format!("\n    {prefix}Example {label}\n    \n    # Literal heading\n");
            assert!(
                report.contains(&literal),
                "{label} excerpt lost its literal block"
            );
        }
        assert!(!report.contains('\r'));
    }
    let long = "合成可见文本".repeat(3000);
    for _ in 0..4 {
        submit(&mut owner, &long).await;
    }
    let prepared = owner
        .rpc("session.report.prepare", json!({"sessionId":id}))
        .await
        .unwrap();
    let plan = owner.rpc("session.report.plan", json!({"sessionId":id,"reportId":prepared["reportId"],"generation":{"kind":"local"},"redact":false})).await.unwrap();
    let excerpts = plan["evidence"]["conversation"].as_array().unwrap();
    assert!(!excerpts.is_empty());
    assert!(excerpts.len() <= 6);
    let mut total = 0;
    let cutoff = server
        .agent()
        .unwrap()
        .get_session(id.as_str().unwrap())
        .unwrap()
        .sequence("main")
        .unwrap();
    for entry in excerpts {
        let text = entry["text"].as_str().unwrap();
        assert!(text.len() <= 2048);
        total += text.len();
        assert!(
            (cutoff.saturating_sub(31)..=cutoff).contains(&entry["sequence"].as_u64().unwrap())
        );
    }
    assert!(total <= 8 * 1024);
    let generated = owner
        .rpc(
            "session.report.generate",
            json!({"sessionId":id,"reportId":prepared["reportId"],"planId":plan["planId"]}),
        )
        .await
        .unwrap();
    archive(&generated);
    drop(owner);
    server.shutdown();
    server.join().await.unwrap();
}
