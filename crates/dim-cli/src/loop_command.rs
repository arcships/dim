//! `/loop` at the line-mode prompt: reads and writes the loops Scheduler owns
//! through the methods its Session part registers (cli-commands.md).
use crate::{
    Result,
    client::Client,
    display::Display,
    loop_view,
    session::{Interrupts, settle},
};

/// What the prompt says when Ctrl+C arrives while a form is in flight.
const NOTICE: &str = "Waiting for the loop command before accepting more input.";

/// Runs one `/loop`; `text` is the whole command line.
pub async fn command(
    client: &mut Client,
    session_id: &str,
    branch_id: &str,
    text: &str,
    display: &mut Display,
    interrupts: &mut Interrupts,
) -> Result<()> {
    let words = text.trim_start().strip_prefix("/loop").unwrap_or(text);
    let Some(request) = loop_view::request(words) else {
        display.record(loop_view::USAGE)?;
        return Ok(());
    };
    let operation = async {
        if !available(client, session_id).await {
            return Ok::<Reply, crate::Error>(Reply::Unavailable);
        }
        match request {
            loop_view::Request::Show => {
                let loops = list(client, session_id, branch_id).await?;
                Ok(Reply::Listed(loops))
            }
            loop_view::Request::Start {
                prompt,
                every_seconds,
            } => {
                let created = client
                    .rpc(
                        "session.loop.start",
                        start_request(session_id, branch_id, &prompt, every_seconds),
                    )
                    .await?;
                Ok(Reply::Started(
                    created["loop"]["loopId"].as_str().map(str::to_owned),
                    loop_view::cadence_text(every_seconds),
                ))
            }
            loop_view::Request::Stop(loop_id) => {
                let loop_id = match loop_id {
                    Some(id) => id,
                    None => match only_live(client, session_id, branch_id).await? {
                        Some(id) => id,
                        None => return Ok(Reply::NoSingleLoop),
                    },
                };
                client
                    .rpc(
                        "session.loop.stop",
                        serde_json::json!({"sessionId": session_id, "loopId": loop_id}),
                    )
                    .await?;
                Ok(Reply::Stopped(loop_id))
            }
        }
    };
    match settle(interrupts, NOTICE, operation).await? {
        Reply::Unavailable => display.record(loop_view::NOT_AVAILABLE)?,
        Reply::Listed(loops) => {
            for line in loop_view::report(branch_id, &loops) {
                display.record(&line)?;
            }
        }
        Reply::Started(Some(id), cadence) => {
            display.record(&format!("Loop {id} started on {branch_id} ({cadence})."))?
        }
        Reply::Started(None, cadence) => {
            display.record(&format!("Loop started on {branch_id} ({cadence})."))?
        }
        Reply::Stopped(id) => display.record(&format!("Loop {id} stopped."))?,
        Reply::NoSingleLoop => display.record(&format!(
            "No single live loop on {branch_id}; name one with /loop stop ID."
        ))?,
    }
    Ok(())
}

enum Reply {
    Unavailable,
    Listed(Vec<loop_view::Loop>),
    Started(Option<String>, String),
    Stopped(String),
    NoSingleLoop,
}

/// The `session.loop.start` request: a named interval starts a fixed loop, and
/// its absence a self-paced one.
pub fn start_request(
    session_id: &str,
    branch_id: &str,
    prompt: &str,
    every_seconds: Option<u64>,
) -> serde_json::Value {
    let mut request = serde_json::json!({
        "sessionId": session_id,
        "branchId": branch_id,
        "prompt": prompt,
    });
    if let Some(seconds) = every_seconds {
        request["everySeconds"] = serde_json::json!(seconds);
    }
    request
}

/// The loops of the branch the client shows.
pub(crate) async fn list(
    client: &mut Client,
    session_id: &str,
    branch_id: &str,
) -> Result<Vec<loop_view::Loop>> {
    let reply = client
        .rpc(
            "session.loop.list",
            serde_json::json!({"sessionId": session_id, "branchId": branch_id}),
        )
        .await?;
    Ok(reply["loops"]
        .as_array()
        .map(|loops| {
            loops
                .iter()
                .filter_map(loop_view::Loop::from_wire)
                .collect()
        })
        .unwrap_or_default())
}

/// The branch's only live loop, when it has exactly one.
pub(crate) async fn only_live(
    client: &mut Client,
    session_id: &str,
    branch_id: &str,
) -> Result<Option<String>> {
    let live: Vec<loop_view::Loop> = list(client, session_id, branch_id)
        .await?
        .into_iter()
        .filter(|record| !matches!(record.state.as_str(), "stopped" | "completed" | "expired"))
        .collect();
    Ok((live.len() == 1).then(|| live[0].loop_id.clone()))
}

/// Whether this daemon serves `session.loop.start`.
pub(crate) async fn available(client: &mut Client, session_id: &str) -> bool {
    if !client.offers("session.methods") {
        return true;
    }
    match client
        .typed::<dim_client::wire::MethodTable>(
            "session.methods",
            serde_json::json!({"sessionId": session_id}),
        )
        .await
    {
        Ok(table) => table
            .methods
            .iter()
            .any(|decl| decl.name == "session.loop.start"),
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The form a client sends: a named interval is a fixed loop, and its
    /// absence is a self-paced one. `5m` reaches the owner as 300 seconds, so
    /// `/loop 5m ...` never starts a self-paced loop.
    #[test]
    fn a_start_request_carries_the_interval_only_when_one_is_named() {
        let fixed = start_request("s1", "main", "check the deploy", Some(300));
        assert_eq!(
            fixed,
            serde_json::json!({
                "sessionId": "s1",
                "branchId": "main",
                "prompt": "check the deploy",
                "everySeconds": 300,
            })
        );

        let self_paced = start_request("s1", "main", "check the deploy", None);
        assert_eq!(
            self_paced,
            serde_json::json!({
                "sessionId": "s1",
                "branchId": "main",
                "prompt": "check the deploy",
            })
        );
        assert!(
            self_paced.get("everySeconds").is_none(),
            "a self-paced start names no interval at all"
        );
    }

    /// The interval a parsed request carries is the one the wire gets.
    #[test]
    fn the_parsed_interval_reaches_the_request() {
        let Some(loop_view::Request::Start {
            prompt,
            every_seconds,
        }) = loop_view::request("5m check the deploy")
        else {
            panic!("the interval form parses");
        };
        assert_eq!(
            start_request("s1", "main", &prompt, every_seconds)["everySeconds"],
            serde_json::json!(300)
        );
    }
}
