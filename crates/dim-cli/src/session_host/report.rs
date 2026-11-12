use super::Worker;
use crate::model::session::Update;
use crate::report::{Action, Captured, Draft, Reply, Request, Target, session_admission_failed};
use serde_json::{Value, json};

impl Worker {
    pub(super) fn report_available(&self) -> bool {
        self.client.offers("report.prepare") || self.methods.contains("session.report.prepare")
    }

    pub(super) fn reportable(&self, error: Captured, show_error: bool) -> bool {
        if !self.report_available() {
            return false;
        }
        self.send(Update::Reportable { error, show_error });
        true
    }

    pub(super) async fn report_request(&mut self, request: Request) {
        let Request { panel, action } = request;
        let reply = match action {
            Action::Prepare(target) => self.prepare_report(target).await,
            Action::Plan {
                draft,
                redact,
                instructions,
            } => {
                let (method, params) = report_call(
                    &draft,
                    "plan",
                    json!({
                        "reportId": draft.report_id, "generation": {"kind":"local"},
                        "redact": redact, "instructions": instructions,
                    }),
                );
                self.client.typed(&method, params).await.map(Reply::Plan)
            }
            Action::Generate(draft) => {
                let (method, params) = report_call(
                    &draft,
                    "generate",
                    json!({
                        "reportId":draft.report_id,"planId":draft.plan_id,
                    }),
                );
                match self.client.send(&method, params).await {
                    Ok(id) => {
                        self.report_generations.insert(id, panel);
                        return;
                    }
                    Err(error) => Err(error),
                }
            }
            Action::Cancel { draft, generation } => {
                let params = if generation {
                    json!({"reportId":draft.report_id,"target":"generation","planId":draft.plan_id})
                } else {
                    json!({"reportId":draft.report_id,"target":"report"})
                };
                let (method, params) = report_call(&draft, "cancel", params);
                let _ = self.client.send(&method, params).await;
                return;
            }
        };
        self.report_reply(panel, reply);
    }

    async fn prepare_report(&mut self, target: Target) -> crate::Result<Reply> {
        let unavailable = "Reporting is unavailable on this App Server";
        let session = match target {
            Target::Session(session) => session,
            Target::Error(error) if self.client.offers("report.prepare") => {
                return self
                    .client
                    .typed("report.prepare", error.params())
                    .await
                    .map(|prepared| Reply::Prepared(None, prepared));
            }
            Target::Error(_) => return Err(unavailable.into()),
        };
        let method = "session.report.prepare";
        if self.session_id != session || !self.methods.contains(method) {
            if !self.client.offers("session.methods") {
                return Err(unavailable.into());
            }
            let table: dim_client::wire::MethodTable = self
                .client
                .typed("session.methods", json!({"sessionId": session}))
                .await?;
            if !table.methods.iter().any(|entry| entry.name == method) {
                return Err(unavailable.into());
            }
        }
        self.client
            .typed(method, json!({"sessionId": session}))
            .await
            .map(|prepared| Reply::Prepared(Some(session), prepared))
    }

    pub(super) fn report_reply(&self, panel: u64, result: crate::Result<Reply>) {
        self.send(Update::ReportReply {
            panel,
            reply: result.unwrap_or_else(|error| Reply::Failed {
                draft_missing: session_admission_failed(&error)
                    || matches!(
                        dim_client::app_error(&error),
                        Some("report_not_found" | "unknown_method" | "service_unavailable")
                    ),
                message: error.to_string(),
            }),
        });
    }
}

fn report_call(draft: &Draft, suffix: &str, mut params: Value) -> (String, Value) {
    let namespace = match &draft.session {
        Some(session) => {
            params["sessionId"] = json!(session);
            "session.report"
        }
        None => "report",
    };
    (format!("{namespace}.{suffix}"), params)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::args::Options;
    use crate::model::session::SwitchKind;
    use crate::report::Operation;
    use rutis::host::CtxHost;
    use std::os::unix::fs::PermissionsExt;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    #[tokio::test]
    async fn report_routes_sessions_and_keeps_admission_errors_private() {
        let home = tempfile::tempdir().unwrap();
        let endpoint = home.path().join("daemon.sock");
        let listener = tokio::net::UnixListener::bind(&endpoint).unwrap();
        std::fs::set_permissions(&endpoint, std::fs::Permissions::from_mode(0o600)).unwrap();
        let daemon = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (reader, mut writer) = stream.into_split();
            let mut lines = BufReader::new(reader).lines();
            let mut exchange = async |method: &str, result: Value| {
                let line = lines.next_line().await.unwrap().unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(request["method"], method);
                let mut reply = result;
                reply["id"] = request["id"].clone();
                reply["jsonrpc"] = json!("2.0");
                writer
                    .write_all(format!("{reply}\n").as_bytes())
                    .await
                    .unwrap();
                request["params"].clone()
            };
            exchange("hello", json!({"result":{
                "instanceId":"report-test", "serverVersion":"test", "protocolVersion":4,
                "features":["session.create","session.history","session.methods","report.prepare"],
                "daemonProfile":{"kind":"product"}
            }})).await;
            let internal = json!({"error":{"code":-32603, "message":"private detail"}});
            exchange("session.detach", internal.clone()).await;
            let attach = exchange("session.attach", internal.clone()).await;
            assert_eq!(attach["sessionId"], "original");
            let prepared = |evidence| {
                json!({"result":{
                    "reportId":"r1", "plan":{
                        "planId":"p1", "generation":{"kind":"local"},
                        "evidence":evidence, "instructionsStatus":"none"
                    }
                }})
            };
            let params = exchange(
                "report.prepare",
                prepared(json!({"unavailable":"no_reference"})),
            )
            .await;
            assert_eq!(params["error"]["kind"], "observed");
            assert!(params["error"]["observed"]["code"].is_null());
            assert_eq!(params.as_object().unwrap().len(), 1);
            assert!(!params.to_string().contains("private detail"));
            assert!(!params.to_string().contains("original"));

            let table = exchange(
                "session.methods",
                json!({"result":{"methods":[{"name":"session.report.prepare","mutating":true}]}}),
            )
            .await;
            assert_eq!(table, json!({"sessionId":"shown"}));
            let params = exchange(
                "session.report.prepare",
                prepared(json!({"timeline":{"recent":[]}})),
            )
            .await;
            assert_eq!(params, json!({"sessionId":"shown"}));
        });
        let client = dim_client::Client::connect(&dim_client::ConnectionConfig {
            endpoint,
            server: dim_client::Server {
                program: "dimcode".into(),
                embedded: false,
            },
            profile: None,
            client_kind: "cli",
            client_name: "dim-cli-test",
            protocol_version: 4,
        })
        .await
        .unwrap();
        let options =
            Options::parse(["--home".into(), home.path().to_str().unwrap().to_owned()].into_iter())
                .unwrap();
        let (updates, receiver) = std::sync::mpsc::channel();
        let mut worker = Worker::new(
            options,
            client,
            updates,
            rutis::Ctx::root().unwrap(),
            tokio::sync::oneshot::channel().0,
        );
        worker.session_id = "attached".into();
        assert!(
            worker
                .attach("original".into(), SwitchKind::Resumed)
                .await
                .is_err()
        );
        assert!(
            !receiver.try_iter().any(|update| matches!(
                update,
                Update::Switched { .. } | Update::Reportable { .. }
            ))
        );
        worker
            .attach("original".into(), SwitchKind::Resumed)
            .await
            .unwrap();
        let error = receiver
            .try_iter()
            .find_map(|update| match update {
                Update::Reportable {
                    error,
                    show_error: true,
                } => Some(error),
                _ => None,
            })
            .expect("the failed admission offered a report");
        assert!(matches!(error.operation, Operation::SessionRead));
        worker.session_id = "other".into();
        worker.methods.insert("session.report.prepare".into());
        worker
            .report_request(Request {
                panel: 1,
                action: Action::Prepare(Target::Error(error)),
            })
            .await;
        assert!(matches!(
            receiver.try_recv().unwrap(),
            Update::ReportReply {
                panel: 1,
                reply: Reply::Prepared(None, _)
            }
        ));
        worker
            .report_request(Request {
                panel: 2,
                action: Action::Prepare(Target::Session("shown".into())),
            })
            .await;
        assert!(
            matches!(receiver.try_recv().unwrap(), Update::ReportReply { panel: 2, reply: Reply::Prepared(Some(session), _) } if session == "shown")
        );
        daemon.await.unwrap();
    }
}
