use super::Worker;
use crate::model::session::Update;
use crate::report::{Action, Captured, Draft, Prepared, Reply, Request, session_admission_failed};
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
            Action::Prepare(error) => self.prepare_report(error).await,
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

    async fn prepare_report(&mut self, error: Captured) -> crate::Result<Reply> {
        let process_report = self.client.offers("report.prepare");
        if !error.session.is_empty() {
            match self.prepare_session_report(&error).await {
                Ok(Some(prepared)) => return Ok(Reply::Prepared(Some(error.session), prepared)),
                Ok(None) => {}
                Err(failure)
                    if process_report
                        && (session_admission_failed(&failure)
                            || matches!(
                                dim_client::app_error(&failure),
                                Some(
                                    "unknown_method"
                                        | "report_cancelled"
                                        | "service_reloading"
                                        | "service_unavailable"
                                )
                            )) => {}
                Err(failure) => return Err(failure),
            }
        }
        if !process_report {
            return Err("Error reporting is unavailable on this App Server".into());
        }
        self.client
            .typed::<Prepared>("report.prepare", error.params(false))
            .await
            .map(|prepared| Reply::Prepared(None, prepared))
    }

    async fn prepare_session_report(
        &mut self,
        error: &Captured,
    ) -> crate::Result<Option<Prepared>> {
        let method = "session.report.prepare";
        if self.session_id != error.session || !self.methods.contains(method) {
            if !self.client.offers("session.methods") {
                return Ok(None);
            }
            let table: dim_client::wire::MethodTable = self
                .client
                .typed("session.methods", json!({"sessionId": error.session}))
                .await?;
            if !table.methods.iter().any(|entry| entry.name == method) {
                return Ok(None);
            }
        }
        let mut params = error.params(true);
        params["sessionId"] = json!(error.session);
        self.client.typed(method, params).await.map(Some)
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
    async fn only_session_admission_is_reportable_and_fallback_keeps_the_error_private() {
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
            let failure = |code: &str| {
                json!({"error":{
                    "code":-32000, "message":"private detail", "data":{"appError":code}
                }})
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
            let listed =
                json!({"result":{"methods":[{"name":"session.report.prepare","mutating":true}]}});
            for (methods, prepare_failure) in [
                (internal.clone(), None),
                (json!({"result":{"methods":[]}}), None),
                (listed.clone(), Some(failure("unknown_method"))),
                (listed.clone(), Some(failure("report_cancelled"))),
                (listed, Some(internal)),
            ] {
                let table = exchange("session.methods", methods).await;
                assert_eq!(table["sessionId"], "original");
                if let Some(prepare_failure) = prepare_failure {
                    let params = exchange("session.report.prepare", prepare_failure).await;
                    assert_eq!(params["sessionId"], "original");
                    assert!(!params.to_string().contains("private detail"));
                }
                let params = exchange(
                    "report.prepare",
                    json!({"result":{
                        "reportId":"r1", "plan":{
                            "planId":"p1", "generation":{"kind":"local"},
                            "evidence":{"unavailable":"no_reference"}, "instructionsStatus":"none"
                        }
                    }}),
                )
                .await;
                assert_eq!(params["error"]["kind"], "observed");
                assert!(params["error"]["observed"]["code"].is_null());
                assert_eq!(params.as_object().unwrap().len(), 1);
                assert!(!params.to_string().contains("private detail"));
                assert!(!params.to_string().contains("original"));
            }
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
        for panel in 0..5 {
            worker
                .report_request(Request {
                    panel,
                    action: Action::Prepare(error.clone()),
                })
                .await;
            assert!(matches!(
                receiver.try_recv().unwrap(),
                Update::ReportReply { panel: received, reply: Reply::Prepared(None, _) } if received == panel
            ));
        }
        daemon.await.unwrap();
    }
}
