//! `/goal` at the line-mode prompt: shows the goal in force on the Session's
//! conversation, and writes it through the methods Goal registers
//! (cli-commands.md).
use crate::{
    Result,
    client::Client,
    display::Display,
    goal,
    session::{Interrupts, settle},
};

/// What the prompt says when Ctrl+C arrives while a form is in flight.
const NOTICE: &str = "Waiting for the goal command before accepting more input.";

/// Runs one `/goal`; `text` is the whole command line.
pub async fn command(
    client: &mut Client,
    session_id: &str,
    branch_id: &str,
    text: &str,
    display: &mut Display,
    interrupts: &mut Interrupts,
) -> Result<()> {
    let words = text.trim_start().strip_prefix("/goal").unwrap_or(text);
    let Some(request) = goal::request(words) else {
        display.record(goal::USAGE)?;
        return Ok(());
    };
    let operation = async {
        if !goal::available(client, session_id).await {
            return Ok::<Reply, crate::Error>(Reply::Unavailable);
        }
        let mut state: dim_agent_types::GoalRead = client
            .typed(
                "session.goal.get",
                serde_json::json!({"sessionId":session_id,"branchId":branch_id}),
            )
            .await?;
        let call = goal::call(&request, state.goal.as_ref());
        if let Some(call) = call {
            let mut params = serde_json::json!({"sessionId": session_id, "branchId": branch_id});
            if let Some(objective) = goal::objective(&request) {
                params["objective"] = serde_json::json!(objective);
            }
            if let Err(error) = client.rpc(call.method(), params).await {
                return match crate::client::app_error(&error) {
                    Some("unknown_method") => Ok(Reply::Unavailable),
                    _ => Err(error),
                };
            }
            state = client
                .typed(
                    "session.goal.get",
                    serde_json::json!({"sessionId":session_id,"branchId":branch_id}),
                )
                .await?;
        }
        Ok(Reply::Done(call, state.goal))
    };
    match settle(interrupts, NOTICE, operation).await? {
        Reply::Unavailable => display.record(goal::NOT_AVAILABLE)?,
        Reply::Done(call, goal) => {
            for line in goal::report(branch_id, call, goal.as_ref()) {
                display.record(&line)?;
            }
        }
    }
    Ok(())
}

enum Reply {
    /// The daemon serves no Goal.
    Unavailable,
    /// The call that ran, none for the read form.
    Done(Option<goal::Call>, Option<goal::Goal>),
}
