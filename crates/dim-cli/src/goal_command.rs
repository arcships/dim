//! `/goal` at the line-mode prompt: shows the goal in force on `main`, and
//! writes it through the methods Goal registers (cli-commands.md).
use crate::{
    Result,
    client::Client,
    display::Display,
    goal,
    session::{Interrupts, read_history, settle},
};

/// What the prompt says when Ctrl+C arrives while a form is in flight.
const NOTICE: &str = "Waiting for the goal command before accepting more input.";

/// Runs one `/goal`; `text` is the whole command line.
pub async fn command(
    client: &mut Client,
    session_id: &str,
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
        // Read the line before choosing `set` or `edit`: the fold holds only
        // what earlier reads applied.
        display.cursor = catch_up(client, session_id, display).await?;
        let call = goal::call(&request, display.goal.goal().as_ref());
        if let Some(call) = call {
            let mut params = serde_json::json!({"sessionId": session_id, "branchId": "main"});
            if let Some(objective) = goal::objective(&request) {
                params["objective"] = serde_json::json!(objective);
            }
            if let Err(error) = client.rpc(call.method(), params).await {
                return match crate::client::app_error(&error) {
                    Some("unknown_method") => Ok(Reply::Unavailable),
                    _ => Err(error),
                };
            }
            display.cursor = catch_up(client, session_id, display).await?;
        }
        Ok(Reply::Done(call))
    };
    match settle(interrupts, NOTICE, operation).await? {
        Reply::Unavailable => display.record(goal::NOT_AVAILABLE)?,
        Reply::Done(call) => {
            for line in goal::report("main", call, display.goal.goal().as_ref()) {
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
    Done(Option<goal::Call>),
}

/// Folds the events `main` holds that `display` has not applied yet.
async fn catch_up(client: &mut Client, session_id: &str, display: &mut Display) -> Result<u64> {
    read_history(client, session_id, display.cursor, |value| {
        display.event(serde_json::from_value(value)?, true)
    })
    .await
}
