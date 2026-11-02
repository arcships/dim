//! `dimcode session rename|delete|cleanup`: the manual title of a Session,
//! permanent deletion, and cleanup composed from `session.summaries` and
//! `session.delete` ([S5c §5.1](../../../docs/design/appserver/session-deletion-and-fork.md#51-cleanup-by-age-or-count)).

use crate::{
    Result,
    client::{Client, RpcError},
    model::session::RenameReply,
    sanitize::safe,
};
use serde::Deserialize;
use serde_json::json;
use std::{
    io::{IsTerminal, Write},
    time::Duration,
};

const RENAME_UNSUPPORTED: &str = "This daemon does not support renaming Sessions";
const DELETE_UNSUPPORTED: &str = "This daemon does not support deleting Sessions";
const CLEANUP_UNSUPPORTED: &str = "This daemon does not support cleaning up Sessions";
/// `includeClosed` and `requireInactive` land together, so a server that
/// refuses one refuses both, and cleanup cannot delete safely there.
const TOO_OLD: &str =
    "This App Server is too old to clean up Sessions safely; nothing was deleted.";

#[derive(Deserialize)]
struct Renamed {
    title: Option<String>,
}

/// The `session.delete` reply. An App Server before the field answers `{}`,
/// which reads as `None` and as a delete that removed the rows.
#[derive(Deserialize)]
struct Deleted {
    #[serde(default)]
    deleted: Option<bool>,
}

/// One stored Session as `session.summaries` reports it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Summary {
    session_id: String,
    /// When the Session was last active, in milliseconds since the epoch:
    /// its newest stored event, or its creation when it has none.
    updated_at: u64,
    #[serde(default)]
    title: Option<String>,
    /// Whether this daemon closed the Session; its rows stay stored.
    #[serde(default)]
    closed: bool,
    /// Whether a turn is running on the Session's `main` line.
    #[serde(default)]
    running: bool,
    /// How many clients are attached to the Session.
    #[serde(default)]
    attached_clients: usize,
    /// The Session's creation origin; absent on a daemon that records none.
    #[serde(default)]
    origin: Option<String>,
}

impl Summary {
    /// Why cleanup keeps an active Session, or `None` when it is idle.
    fn active_reason(&self) -> Option<String> {
        let mut reasons = Vec::new();
        if self.running {
            reasons.push("running".to_owned());
        }
        match self.attached_clients {
            0 => {}
            1 => reasons.push("1 client attached".to_owned()),
            clients => reasons.push(format!("{clients} clients attached")),
        }
        (!reasons.is_empty()).then(|| reasons.join(", "))
    }
    /// Whether `/resume`'s default list shows this Session, which is every
    /// origin but `internal` and `acp`
    /// ([classification](../../../docs/design/appserver/session-classification.md)).
    /// Cleanup removes no Session `/resume` hides: a member goes with its
    /// leader's delete, a launched Session with its plugin, and an ACP
    /// Session is deleted by its id.
    fn resumable(&self) -> bool {
        !matches!(self.origin.as_deref(), Some("internal" | "acp"))
    }
}

/// `dimcode session rename`: sets the manual title, or clears it when `title`
/// is `None`, and prints what the server reports.
pub async fn rename(client: &mut Client, session_id: &str, title: Option<&str>) -> Result<()> {
    match set_title(client, session_id, title).await {
        RenameReply::Titled(title) => {
            println!("Renamed to {}", one_line(&title));
            Ok(())
        }
        RenameReply::Untitled => {
            println!("The Session is now untitled.");
            Ok(())
        }
        RenameReply::Unsupported => Err(RENAME_UNSUPPORTED.into()),
        RenameReply::Failed(message) => Err(message.into()),
    }
}

/// Sets or clears the manual title, as both `dimcode session rename` and
/// `/rename` report it.
///
/// `session.rename` is a Session method, so `hello.features` does not list
/// it: calling it is how a client finds out whether the Session serves it,
/// and `unknown_method` is the answer that it does not
/// ([session boundary](../../../docs/design/contracts/session-boundary.md)).
pub(crate) async fn set_title(
    client: &mut Client,
    session_id: &str,
    title: Option<&str>,
) -> RenameReply {
    match client
        .typed::<Renamed>(
            "session.rename",
            json!({"sessionId": session_id, "title": title}),
        )
        .await
    {
        Ok(renamed) => match renamed.title {
            Some(title) => RenameReply::Titled(title),
            None => RenameReply::Untitled,
        },
        Err(error) => match crate::client::app_error(&error) {
            Some("unknown_method") => RenameReply::Unsupported,
            _ => RenameReply::Failed(error.to_string()),
        },
    }
}

/// `dimcode session delete`: permanently deletes the named Session.
///
/// The reply says whether this call removed it. An id this daemon holds no
/// rows of is still a success, but nothing was deleted, and the command
/// reports that instead of a delete.
pub async fn delete(client: &mut Client, session_id: &str) -> Result<()> {
    if !client.supports(&["session.delete"]) {
        return Err(DELETE_UNSUPPORTED.into());
    }
    let reply: Deleted = client
        .typed("session.delete", json!({"sessionId": session_id}))
        .await?;
    if reply.deleted == Some(false) {
        return Err(format!(
            "Nothing was removed: no stored Session {}.",
            one_line(session_id)
        )
        .into());
    }
    println!("Deleted {}", one_line(session_id));
    Ok(())
}

/// `dimcode session cleanup`: shows the Sessions one selector names, asks for
/// confirmation, then deletes every one of them. A Session `/resume` hides
/// (origin `internal` or `acp`) is never selected. A Session with a running
/// turn or an attached client is shown as kept and never deleted, and
/// `--keep N` counts it as kept without its consuming one of the N.
pub async fn cleanup(
    client: &mut Client,
    older_than: Option<Duration>,
    keep: Option<usize>,
    yes: bool,
) -> Result<i32> {
    if !client.supports(&["session.delete", "session.summaries"]) {
        return Err(CLEANUP_UNSUPPORTED.into());
    }
    let mut sessions = summaries(client).await?;
    sessions.retain(Summary::resumable);
    sessions.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.session_id.cmp(&right.session_id))
    });
    let now = now_ms();
    let (selected, nothing_selected): (Vec<Summary>, String) = match (older_than, keep) {
        (Some(age), _) => {
            let cutoff = now.saturating_sub(age.as_millis().try_into().unwrap_or(u64::MAX));
            (
                sessions
                    .into_iter()
                    .filter(|session| session.updated_at <= cutoff)
                    .collect(),
                format!(
                    "Nothing to delete: no Session is older than {}.",
                    age_label(age)
                ),
            )
        }
        (None, Some(keep)) => (
            sessions.into_iter().skip(keep).collect(),
            format!("Nothing to delete: --keep {keep} keeps every Session."),
        ),
        (None, None) => return Err("session cleanup needs --older-than or --keep".into()),
    };
    let (active, selected): (Vec<Summary>, Vec<Summary>) = selected
        .into_iter()
        .partition(|session| session.active_reason().is_some());
    if selected.is_empty() && active.is_empty() {
        println!("{nothing_selected}");
        return Ok(0);
    }
    for session in &selected {
        println!(
            "{}\t{}\t{}{}",
            one_line(&session.session_id),
            session_title(session),
            crate::ui::sessions_panel::relative_time(session.updated_at, now),
            if session.closed { "\tclosed" } else { "" }
        );
    }
    if !active.is_empty() {
        println!("Kept (active):");
        for session in &active {
            println!(
                "{}\t{}\t{}\t{}",
                one_line(&session.session_id),
                session_title(session),
                crate::ui::sessions_panel::relative_time(session.updated_at, now),
                session
                    .active_reason()
                    .expect("an active Session has a reason")
            );
        }
    }
    if selected.is_empty() {
        println!("Nothing to delete: every matching Session is active.");
        return Ok(0);
    }
    if !yes && !confirmed(selected.len())? {
        println!("Nothing was deleted.");
        return Ok(0);
    }
    let mut deleted = 0;
    let mut failed = Vec::new();
    let mut refused = Vec::new();
    for session in &selected {
        match client
            .typed::<Deleted>(
                "session.delete",
                json!({"sessionId": session.session_id, "requireInactive": true}),
            )
            .await
        {
            // An explicit `false` means another caller removed it first:
            // neither deleted by this run nor a failure. A reply without the
            // field is from a server before it, and the rows are gone either
            // way.
            Ok(Deleted {
                deleted: Some(false),
            }) => println!("Already gone: {}", one_line(&session.session_id)),
            Ok(_) => {
                deleted += 1;
                println!("Deleted {}", one_line(&session.session_id));
            }
            Err(error) => {
                match crate::client::app_error(&error) {
                    // The preview kept what the summaries showed as inactive;
                    // the owner is the one that knows at the delete.
                    Some("session_active") => refused.push((session, reason(&error))),
                    // `requireInactive` joins `includeClosed`, so a server that
                    // refuses it here is the too-old one; nothing was deleted.
                    Some("invalid_params") if deleted == 0 && failed.is_empty() => {
                        return Err(TOO_OLD.into());
                    }
                    _ => {
                        eprintln!(
                            "Cannot delete {}: {}",
                            one_line(&session.session_id),
                            crate::sanitize::safe(&error.to_string()).replace('\n', " ")
                        );
                        failed.push(session.session_id.clone());
                    }
                }
            }
        }
    }
    if !refused.is_empty() {
        println!("Kept (in use at the delete):");
        for (session, why) in &refused {
            println!(
                "{}\t{}\t{}\t{}",
                one_line(&session.session_id),
                session_title(session),
                crate::ui::sessions_panel::relative_time(session.updated_at, now),
                why
            );
        }
    }
    if deleted == 0 && failed.is_empty() && !refused.is_empty() {
        println!("Nothing to delete: every matching Session is active.");
    }
    if failed.is_empty() {
        return Ok(0);
    }
    eprintln!("Retry each failed Session:");
    for id in &failed {
        eprintln!("dimcode session delete {}", one_line(id));
    }
    Ok(1)
}

/// Asks on the terminal, where only `y` or `yes` confirms; elsewhere the
/// caller's `--yes` is the only confirmation.
fn confirmed(count: usize) -> Result<bool> {
    if !std::io::stdin().is_terminal() {
        return Err(
            "Cleanup deletes Sessions and this is not a terminal; pass --yes to delete without a prompt"
                .into(),
        );
    }
    eprint!("Delete {count} Sessions? [y/N] ");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

/// The title column of the preview, or `-` when the Session has none.
fn session_title(session: &Summary) -> String {
    session
        .title
        .as_deref()
        .map(one_line)
        .unwrap_or_else(|| "-".into())
}

/// Every stored Session `session.summaries` reports, the ones this daemon
/// closed included. A server that refuses the field is too old to clean up
/// safely, so the run stops before it deletes anything.
async fn summaries(client: &mut Client) -> Result<Vec<Summary>> {
    match client
        .typed("session.summaries", json!({"includeClosed": true}))
        .await
    {
        Ok(sessions) => Ok(sessions),
        Err(error) if crate::client::app_error(&error) == Some("invalid_params") => {
            Err(TOO_OLD.into())
        }
        Err(error) => Err(error),
    }
}

/// The App Server's own sentence for a refused call, as one printable line.
fn reason(error: &crate::Error) -> String {
    match error.downcast_ref::<RpcError>() {
        Some(error) if !error.message.is_empty() => one_line(&error.message),
        _ => one_line(&error.to_string()),
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

/// An `--older-than` length as the count and unit that flag takes, measured
/// in the largest unit that divides it exactly.
fn age_label(age: Duration) -> String {
    let seconds = age.as_secs();
    for (size, unit) in [(86_400, "d"), (3_600, "h")] {
        if seconds.is_multiple_of(size) {
            return format!("{}{unit}", seconds / size);
        }
    }
    format!("{}m", seconds / 60)
}

/// Text a daemon supplies as one printable line.
fn one_line(text: &str) -> String {
    safe(text).replace(['\n', '\t'], " ")
}
