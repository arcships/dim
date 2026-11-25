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
const USER_PROMPT: &str = "What happened during this turn?";

#[test]
fn evidence_redact_pipe_matches_unicode_variants_at_source_offsets() {
    let home = "/Users/İsmail";
    let account = "İsmail";
    let init = json!({
        "facts": {"homes":[home], "session":"", "identifiers":[], "terms":["ısland", "ΟΣ"]},
        "kept": [],
    });
    let input = "🧪 /Users/İsmail /users/ismail İsmail ISMAIL ısmail ISLAND ος οσ \\u0130smail \\u0131smail %c4%b0smail %c4%b1smail!";
    let expected = "🧪 /Users/userus /Users/userus userus userus userus [term-1] [term-2] [term-2] userus userus userus userus!";
    let requests = format!("{init}\n{}\n{}\n", json!(input), json!(expected));
    let mut response = Vec::new();
    dim_report::redaction::serve(Cursor::new(requests.as_bytes()), &mut response).unwrap();
    let response: Vec<Value> = std::str::from_utf8(&response)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(response.len(), 3);
    assert_eq!(response[1]["text"], expected);
    let spans = response[1]["spans"].as_array().unwrap();
    let expected_spans = [
        ("/Users/İsmail", home, "/Users/userus"),
        ("/users/ismail", home, "/Users/userus"),
        ("İsmail", account, "userus"),
        ("ISMAIL", account, "userus"),
        ("ısmail", account, "userus"),
        ("ISLAND", "ısland", "[term-1]"),
        ("ος", "ΟΣ", "[term-2]"),
        ("οσ", "ΟΣ", "[term-2]"),
        ("\\u0130smail", account, "userus"),
        ("\\u0131smail", account, "userus"),
        ("%c4%b0smail", account, "userus"),
        ("%c4%b1smail", account, "userus"),
    ];
    assert_eq!(spans.len(), expected_spans.len());
    let mut byte_offset = 0;
    for (span, (source, original, replacement)) in spans.iter().zip(expected_spans) {
        let start = byte_offset + input[byte_offset..].find(source).unwrap();
        byte_offset = start + source.len();
        let cp_start = input[..start].chars().count();
        let cp_end = cp_start + source.chars().count();
        assert_eq!(*span, json!([cp_start, cp_end, original, replacement]));
    }
    assert_eq!(response[2]["spans"], json!([]));

    let replacements = dim_report::redaction::Replacements::new(
        &dim_report::redaction::Facts {
            homes: vec![home.into()],
            session: String::new(),
            member_sessions: Vec::new(),
            identifiers: Vec::new(),
            terms: Vec::new(),
        },
        &[],
    )
    .unwrap();
    for variant in ["\\u0130smail", "\\u0131smail", "%c4%b0smail", "%c4%b1smail"] {
        assert!(
            replacements.has_residue(variant),
            "residue missed: {variant}"
        );
    }
}

#[test]
fn evidence_redact_pipe_bounds_unicode_forms_and_preserves_encoded_spans() {
    let long = "IΣKſ".repeat(64);
    let mixed = "\\u0130%cf%82Ks".repeat(64);
    let punctuation = "I/\\\"\\\nΣ";
    let home = "/Users/İ<&>name";
    let account = "İ<&>name";
    let quoted = serde_json::to_string(punctuation).unwrap();
    let json_body = &quoted[1..quoted.len() - 1];
    let sources = [
        mixed,
        "ISLAND".into(),
        "\\u0131sland".into(),
        "%C4%B0SLAND".into(),
        "\\uD801\\uDC28".into(),
        "%f0%90%90%a8".into(),
        punctuation.into(),
        json_body.into(),
        json_body.replace('Σ', "\\u03c2"),
        "I\\/\\\"\\\nΣ".into(),
        "%c4%b1%2f%5c%22%5c%0a%cf%82".into(),
        "/Users/\\u0131&lt;&amp;&gt;name".into(),
        "%c4%b0&lt;&amp;&gt;name".into(),
        "\\u0049s%6c%41nd".into(),
        "%2FUsers\\u002F%69%3c%26%3ename".into(),
    ];
    let labels = [
        "[term-1]",
        "[term-2]",
        "[term-2]",
        "[term-2]",
        "[term-3]",
        "[term-3]",
        "[term-4]",
        "[term-4]",
        "[term-4]",
        "[term-4]",
        "[term-4]",
        "/Users/useruser",
        "useruser",
        "[term-2]",
        "/Users/useruser",
    ];
    let facts = dim_report::redaction::Facts {
        homes: vec![home.into()],
        session: String::new(),
        member_sessions: Vec::new(),
        identifiers: Vec::new(),
        terms: vec![
            long.clone(),
            "island".into(),
            "𐐀".into(),
            punctuation.into(),
            "IΣ".into(),
        ],
    };
    let replacements = dim_report::redaction::Replacements::new(&facts, &[]).unwrap();
    for source in &sources {
        assert!(replacements.has_residue(source), "residue missed: {source}");
    }
    let blocked = [
        "xisland",
        "islandx",
        "\\u0130island",
        "island%c4%b1",
        "%78island",
        "island\\u0078",
        "%2569sland",
        "\\u005cu0069sland",
        r"\\u0069sland",
        "\\uD800",
        "\\uDC00",
        "%c0%af",
        "%ed%a0%80",
        "%f4%90%80%80",
        "%c4%",
    ]
    .join(" | ");
    assert_eq!(replacements.text(&blocked), blocked);
    assert!(!replacements.has_residue(&blocked));
    assert_eq!(
        replacements.text(r"%28\u0049s%6c%41nd\u0029"),
        r"%28[term-2]\u0029"
    );
    let input = format!("🧪 {} | {blocked}", sources.join(" | "));
    let expected = format!("🧪 {} | {blocked}", labels.join(" | "));
    let init = json!({
        "facts": {"homes":[home], "session":"", "identifiers":[], "terms":facts.terms},
        "kept": [],
    });
    let requests = format!("{init}\n{}\n{}\n", json!(input), json!(expected));
    let mut response = Vec::new();
    dim_report::redaction::serve(Cursor::new(requests.as_bytes()), &mut response).unwrap();
    let response: Vec<Value> = std::str::from_utf8(&response)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(response[1]["text"], expected);
    let originals = [
        long.as_str(),
        "island",
        "island",
        "island",
        "𐐀",
        "𐐀",
        punctuation,
        punctuation,
        punctuation,
        punctuation,
        punctuation,
        home,
        account,
        "island",
        home,
    ];
    let spans = response[1]["spans"].as_array().unwrap();
    assert_eq!(spans.len(), sources.len());
    let mut at = "🧪 ".len();
    for (((source, label), original), span) in sources.iter().zip(labels).zip(originals).zip(spans)
    {
        let start = input[..at].chars().count();
        let end = start + source.chars().count();
        assert_eq!(*span, json!([start, end, original, label]));
        at += source.len() + " | ".len();
    }
    assert_eq!(response[2]["spans"], json!([]));
    assert!(!replacements.has_residue(&expected));
}

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

fn report_builder(home: &std::path::Path) -> AgentBuilder {
    use dim_agent_capabilities::host_platform::{HostPlatform, ShellStatus};
    let mut builder = AgentBuilder::new(AgentConfig::default());
    let mut platform = HostPlatform::current(ShellStatus::Unavailable {
        code: "fixture",
        reason: "No shell in the report fixture".into(),
    })
    .unwrap();
    platform.home = home.to_owned();
    builder.process_value(Arc::new(platform));
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
    let mut builder = report_builder(home.path());
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
    let mut builder = report_builder(home.path());
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
                json!({"sessionId":id,"branchId":"main","text":USER_PROMPT}),
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
    let mut builder = report_builder(home.path());
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
            json!({"sessionId":id, "branchId":"main", "text":USER_PROMPT}),
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
    assert!(!prepared.to_string().contains(USER_PROMPT));
    assert!(evidence.get("conversation").is_none());

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
    let mut builder = report_builder(home.path());
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
    let plan = owner.rpc("session.report.plan", json!({"sessionId":id,"reportId":prepared["reportId"],"generation":{"kind":"local"},"redact":false,"includeConversation":true})).await.unwrap();
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
async fn conversation_modes_share_a_cutoff_and_only_attach_text_to_the_report_member() {
    let home = report_home();
    let synthetic_home = std::path::Path::new("/Users/İsmail");
    let host_home = synthetic_home.to_str().unwrap();
    let mut builder = report_builder(synthetic_home);
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
    let prompt = "Confidential fixture: the unreleased project is called Blue Sky.";
    submit(&mut owner, prompt).await;
    let prepared = owner
        .rpc("session.report.prepare", json!({"sessionId":id}))
        .await
        .unwrap();
    let diagnostics_only = |plan: &Value| {
        assert!(plan["evidence"].get("conversation").is_none());
        assert!(!plan.to_string().contains(prompt));
    };
    diagnostics_only(&prepared["plan"]);
    let default = owner.rpc("session.report.generate", json!({"sessionId":id,"reportId":prepared["reportId"],"planId":prepared["plan"]["planId"]})).await.unwrap();
    let diagnostics = archive(&default);
    assert!(diagnostics["evidence"]["timeline"].is_object());
    assert_eq!(prepared["plan"]["files"], default["files"]);
    assert!(!default.to_string().contains(prompt));

    let prepared = owner
        .rpc("session.report.prepare", json!({"sessionId":id}))
        .await
        .unwrap();
    submit(&mut owner, "LATER-BEFORE-PLAN").await;
    let plan = async |owner: &mut Client, options: Value| {
        let mut request =
            json!({"sessionId":id,"reportId":prepared["reportId"],"generation":{"kind":"local"}});
        request
            .as_object_mut()
            .unwrap()
            .extend(options.as_object().unwrap().clone());
        owner.rpc("session.report.plan", request).await.unwrap()
    };
    let included = plan(
        &mut owner,
        json!({"redact":true,"includeConversation":true}),
    )
    .await;
    assert!(included.to_string().contains(prompt));
    for options in [
        json!({"redact":true}),
        json!({"redact":false}),
        json!({"redact":false,"instructions":"keep timeline, keep conversation"}),
        json!({"redact":true,"includeConversation":false}),
    ] {
        diagnostics_only(&plan(&mut owner, options).await);
    }
    let plan = plan(
        &mut owner,
        json!({"redact":false,"includeConversation":true}),
    )
    .await;
    let excerpts = plan["evidence"]["conversation"].as_array().unwrap();
    assert!(
        excerpts
            .iter()
            .any(|entry| entry["role"] == "user" && entry["text"] == prompt)
    );
    assert!(excerpts.iter().any(|entry| entry["role"] == "assistant" && entry["text"] == format!("Echo: {prompt}")));
    assert!(!plan.to_string().contains("LATER-BEFORE-PLAN"));
    assert!(
        plan["files"]["report.md"]
            .as_str()
            .unwrap()
            .contains(prompt)
    );
    submit(&mut owner, "LATER-BEFORE-GENERATE").await;
    let generated = owner
        .rpc(
            "session.report.generate",
            json!({"sessionId":id,"reportId":prepared["reportId"],"planId":plan["planId"]}),
        )
        .await
        .unwrap();
    archive(&generated);
    assert_eq!(plan["files"], generated["files"]);
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

    let marker = "Why did /report return 2/3?";
    let session_id = id.as_str().unwrap();
    let fake_key = format!("sk-{}", "A".repeat(32));
    let encoded_home = r"%2FUs%65rs\u002F%49sma\u0069l";
    let discussion =
        "Discuss Cookie and Set-Cookie handling; the x-cookie header is not a credential field";
    let cookie_fixtures = [
        "fixture-cookie-value",
        "fixture-cookie-folded",
        "fixture-set-cookie-value",
        "fixture-cookie-json",
        "fixture-cookie-escaped-header",
        "fixture-cookie-next-line-json",
    ];
    let folded_fixtures = [
        "fixture-authorization-folded",
        "fixture-x-api-key-folded",
        "fixture-json-authorization-folded",
    ];
    let message = format!(
        "{marker} {session_id} at {host_home}/sample.txt; /USERS/ISMAIL/other.txt; owner İsmail, ISMAIL, ısmail; {encoded_home}/mixed.txt\n\
         {fake_key}\nAuthorization: Bearer fixture-not-a-real-credential\nAuthorization: configured\n  inline_description: visible-inline-description\nservice_credential = fixture-value\naccess_token = fixture-spaced-value\n\"token\": \"fixture-json-value\"\n\
         Authorization: Bearer\n  {}\nX-Api-Key:\n  {}\n{{\n  \"Authorization\":\n    \"{}\",\n  \"visible_sibling\": \"visible-sibling\"\n}}\npassword: configured\n  description: visible-nested-sibling\nThe authorization flow is documented below.\n\
         cOoKiE: sid={};\r\n\tother={}\n\
         curl -H 'Set-Cookie: sid={}; HttpOnly'\n\
         {{\"Cookie\": \"sid={}\"}}\n\
         HTTP/1.1 200 OK\\r\\nSet-Cookie: sid={}\n\
         {{\"Cookie\":\n\"sid={}\"}}\n{discussion}",
        folded_fixtures[0],
        folded_fixtures[1],
        folded_fixtures[2],
        cookie_fixtures[0],
        cookie_fixtures[1],
        cookie_fixtures[2],
        cookie_fixtures[3],
        cookie_fixtures[4],
        cookie_fixtures[5],
    );
    submit(&mut owner, &message).await;
    let prepared = owner
        .rpc("session.report.prepare", json!({"sessionId":id}))
        .await
        .unwrap();
    let visible = owner.rpc("session.report.plan", json!({"sessionId":id,"reportId":prepared["reportId"],"generation":{"kind":"local"},"redact":false,"includeConversation":true})).await.unwrap();
    let checked = owner.rpc("session.report.plan", json!({"sessionId":id,"reportId":prepared["reportId"],"generation":{"kind":"local"},"redact":true,"includeConversation":true})).await.unwrap();
    let selection = |plan: &Value| {
        plan["evidence"]["conversation"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                (
                    e["role"].as_str().unwrap().to_owned(),
                    e["sequence"].as_u64().unwrap(),
                )
            })
            .collect::<Vec<_>>()
    };
    let no_credentials = |value: &Value| {
        let value = value.to_string();
        for forbidden in [
            "fixture-not-a-real-credential",
            "service_credential",
            "fixture-value",
            "fixture-spaced-value",
            "fixture-json-value",
            fake_key.as_str(),
        ]
        .into_iter()
        .chain(cookie_fixtures)
        .chain(folded_fixtures)
        {
            assert!(!value.contains(forbidden), "fixture leaked: {forbidden}");
        }
    };
    for (plan, redact) in [(&visible, false), (&checked, true)] {
        no_credentials(plan);
        assert_eq!(selection(plan), selection(&visible));
        let conversation = plan["evidence"]["conversation"].as_array().unwrap();
        let report = plan["files"]["report.md"].as_str().unwrap();
        for role in ["user", "assistant"] {
            let text = conversation
                .iter()
                .find(|e| e["role"] == role && e["text"].as_str().unwrap().contains(marker))
                .unwrap()["text"]
                .as_str()
                .unwrap();
            let line = text.lines().find(|line| line.contains(marker)).unwrap();
            assert!(report.contains(line));
            assert!(line.contains("/sample.txt"));
            assert!(text.lines().any(|line| line == discussion));
            assert!(report.contains(discussion));
            assert!(text.contains("visible_sibling"));
            assert!(text.contains("visible-nested-sibling"));
            assert!(text.contains("visible-inline-description"));
            assert!(text.contains("The authorization flow is documented below."));
            assert_eq!(line.contains(encoded_home), !redact);
            assert_eq!(line.contains("/Users/userus/mixed.txt"), redact);
            assert_eq!(line.contains(session_id), !redact);
            assert_eq!(line.contains(host_home), !redact);
            for original in ["İsmail", "ISMAIL", "ısmail"] {
                assert_eq!(line.contains(original), !redact);
            }
            if redact {
                assert!(line.contains("00000000-0000-4000-8000-000000000000"));
                assert!(line.contains("/Users/userus/sample.txt; /Users/userus/other.txt"));
                assert!(line.contains("owner userus, userus, userus"));
            }
        }
        assert!(
            !plan["files"]["diagnostics.json"]
                .as_str()
                .unwrap()
                .contains(marker)
        );
    }
    let stale = owner
        .rpc(
            "session.report.generate",
            json!({"sessionId":id,"reportId":prepared["reportId"],"planId":visible["planId"]}),
        )
        .await
        .unwrap_err();
    assert_eq!(app_error(&stale), Some("report_plan_stale"));
    let generated = owner
        .rpc(
            "session.report.generate",
            json!({"sessionId":id,"reportId":prepared["reportId"],"planId":checked["planId"]}),
        )
        .await
        .unwrap();
    archive(&generated);
    no_credentials(&generated);
    assert_eq!(checked["files"], generated["files"]);
    for surface in [
        &generated["files"]["diagnostics.json"],
        &generated["body"],
        &generated["sharing"],
    ] {
        assert!(!surface.to_string().contains(marker));
        assert!(!surface.to_string().contains("Echo:"));
    }
    let prepared = owner
        .rpc("session.report.prepare", json!({"sessionId":id}))
        .await
        .unwrap();
    let unchecked = owner.rpc("session.report.plan", json!({"sessionId":id,"reportId":prepared["reportId"],"generation":{"kind":"local"},"redact":false,"includeConversation":true})).await.unwrap();
    let raw_zip = owner
        .rpc(
            "session.report.generate",
            json!({"sessionId":id,"reportId":prepared["reportId"],"planId":unchecked["planId"]}),
        )
        .await
        .unwrap();
    archive(&raw_zip);
    no_credentials(&raw_zip);
    assert_eq!(raw_zip["files"], visible["files"]);
    assert_eq!(
        raw_zip["files"]["diagnostics.json"],
        generated["files"]["diagnostics.json"]
    );
    let prepared = owner
        .rpc("session.report.prepare", json!({"sessionId":id}))
        .await
        .unwrap();
    for redact in [true, false] {
        let omitted = owner.rpc("session.report.plan", json!({"sessionId":id,"reportId":prepared["reportId"],"generation":{"kind":"local"},"redact":redact,"includeConversation":true,"instructions":"omit timeline"})).await.unwrap();
        assert!(omitted["evidence"].get("conversation").is_none());
        assert!(omitted["evidence"].get("timeline").is_none());
        assert!(!omitted["files"].to_string().contains(marker));
    }
    for (label, eol) in [("LF", "\n"), ("CRLF", "\r\n"), ("CR", "\r")] {
        let message = format!("Example {label}{eol}{eol}# Literal heading");
        submit(&mut owner, &message).await;
        let prepared = owner
            .rpc("session.report.prepare", json!({"sessionId":id}))
            .await
            .unwrap();
        let plan = owner.rpc("session.report.plan", json!({"sessionId":id,"reportId":prepared["reportId"],"generation":{"kind":"local"},"redact":false,"includeConversation":true})).await.unwrap();
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
    for sensitive in [
        format!("{}sk-{}", "x ".repeat(1006), "B".repeat(40)),
        format!(
            "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----",
            "BOUNDARYKEYDATA".repeat(160)
        ),
    ] {
        submit(&mut owner, &format!("Boundary example\n{sensitive}")).await;
        let prepared = owner
            .rpc("session.report.prepare", json!({"sessionId":id}))
            .await
            .unwrap();
        let unchecked = owner.rpc("session.report.plan", json!({"sessionId":id,"reportId":prepared["reportId"],"generation":{"kind":"local"},"redact":false,"includeConversation":true})).await.unwrap();
        let checked = owner.rpc("session.report.plan", json!({"sessionId":id,"reportId":prepared["reportId"],"generation":{"kind":"local"},"redact":true,"includeConversation":true})).await.unwrap();
        for plan in [&checked, &unchecked] {
            let conversation = plan["evidence"]["conversation"].as_array().unwrap();
            assert!(
                conversation
                    .iter()
                    .any(|e| e["text"].as_str().unwrap().contains("Boundary example"))
            );
            for forbidden in ["sk-B", "BOUNDARYKEYDATA", "END PRIVATE KEY"] {
                assert!(!plan.to_string().contains(forbidden));
            }
        }
    }
    let long = "合成可见文本".repeat(300);
    for _ in 0..4 {
        submit(&mut owner, &long).await;
    }
    let prepared = owner
        .rpc("session.report.prepare", json!({"sessionId":id}))
        .await
        .unwrap();
    let plan = owner.rpc("session.report.plan", json!({"sessionId":id,"reportId":prepared["reportId"],"generation":{"kind":"local"},"redact":false,"includeConversation":true})).await.unwrap();
    let excerpts = plan["evidence"]["conversation"].as_array().unwrap();
    assert!(prepared["plan"]["evidence"].get("conversation").is_none());
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

#[tokio::test]
async fn recognized_credentials_are_withheld_from_both_conversation_modes() {
    let home = report_home();
    let synthetic_home = std::path::Path::new("/Users/İsmail");
    let mut builder = report_builder(synthetic_home);
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
    let marker = "recognized-credential-fixture";
    let stripe = "sk_live_51abcdefghijklmnopqrstuv";
    let google = "AIzaSyA1234567890abcdefghijklmnopqrstu";
    let asia = "ASIA1234567890ABCDEF";
    let userinfo = "https://alice:pw123@internal.example/api";
    let bearer = "Bearer abcdefghijklmnopqrstuvwxyz";
    let aws = "aws-secret-fixture";
    let aws_json = "json-secret-fixture";
    let secret_key = "secret-key-fixture";
    let private_key = "private-key-fixture";
    let signing_key = "signing-key-fixture";
    let groq = "gsk_fixture";
    let proxy = "cHJveHk6cHc=";
    let prose = "Secretary: available";
    let visible = "credential-fixture-visible-tail";
    let message = format!(
        "{marker}\n{stripe}\n{google}\n{asia}\n{userinfo}\n{bearer}\n\
         aws_secret_access_key = {aws}\n\"aws_secret_access_key\": \"{aws_json}\"\n\
         secret_key = {secret_key}\nprivate_key = {private_key}\n\
         signing_key = {signing_key}\nGROQ_API_KEY={groq}\n\
         Proxy-Authorization: Basic {proxy}\n{prose}\n{visible}"
    );
    owner
        .rpc(
            "session.submit",
            json!({"sessionId":id,"branchId":"main","text":message}),
        )
        .await
        .unwrap();
    owner
        .rpc("session.idle", json!({"sessionId":id,"branchId":"main"}))
        .await
        .unwrap();
    for redact in [false, true] {
        let prepared = owner
            .rpc("session.report.prepare", json!({"sessionId":id}))
            .await
            .unwrap();
        let plan = owner
            .rpc(
                "session.report.plan",
                json!({"sessionId":id,"reportId":prepared["reportId"],"generation":{"kind":"local"},"redact":redact,"includeConversation":true}),
            )
            .await
            .unwrap();
        let text = plan.to_string();
        for secret in [
            stripe,
            google,
            asia,
            userinfo,
            bearer,
            aws,
            aws_json,
            secret_key,
            private_key,
            signing_key,
            groq,
            proxy,
        ] {
            assert!(
                !text.contains(secret),
                "plan leaked {secret} with redact={redact}"
            );
        }
        assert!(text.contains(marker), "marker missing with redact={redact}");
        assert!(
            text.contains(prose),
            "ordinary prose missing with redact={redact}"
        );
        assert!(
            text.contains(visible),
            "visible tail missing with redact={redact}"
        );
        assert!(
            text.contains("[credential omitted]"),
            "mask missing with redact={redact}"
        );
        assert!(
            text.contains("[excerpt withheld]"),
            "withheld line missing with redact={redact}"
        );
        let generated = owner
            .rpc(
                "session.report.generate",
                json!({"sessionId":id,"reportId":prepared["reportId"],"planId":plan["planId"]}),
            )
            .await
            .unwrap();
        archive(&generated);
        assert_eq!(plan["files"], generated["files"]);
        let text = generated.to_string();
        for secret in [
            stripe,
            google,
            asia,
            userinfo,
            bearer,
            aws,
            aws_json,
            secret_key,
            private_key,
            signing_key,
            groq,
            proxy,
        ] {
            assert!(
                !text.contains(secret),
                "ZIP leaked {secret} with redact={redact}"
            );
        }
        assert!(
            generated["files"]["report.md"]
                .as_str()
                .unwrap()
                .contains(marker)
        );
        assert!(
            generated["files"]["report.md"]
                .as_str()
                .unwrap()
                .contains(prose)
        );
    }
    drop(owner);
    server.shutdown();
    server.join().await.unwrap();
}
