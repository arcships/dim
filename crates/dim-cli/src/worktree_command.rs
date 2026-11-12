//! `dimcode worktree`: the managed worktrees under the product home
//! (#1451). Listing, removal and pruning are the daemon's `worktree`
//! namespace, which owns the records and asks Git; the client keeps no state
//! of its own and prints what the daemon answered.

use crate::{Result, client::Client, sanitize::safe_line, ui::sessions_panel::relative_time};
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{IsTerminal, Write};

/// What `dimcode worktree` names besides the commands themselves.
pub const USAGE: &str = "Use worktree list, worktree remove TASK_ID [--force] or \
worktree prune (--older-than DURATION | --keep N) [--yes] [--force]";

/// `worktree prune` takes exactly one selector, as `session cleanup` does.
pub const SELECTOR_USAGE: &str =
    "Use worktree prune --older-than DURATION or worktree prune --keep N";

const LIST_UNSUPPORTED: &str = "This daemon does not support listing managed worktrees";
const REMOVE_UNSUPPORTED: &str = "This daemon does not support removing managed worktrees";
const PRUNE_UNSUPPORTED: &str = "This daemon does not support pruning managed worktrees";

/// The columns [`Worktree::row`] prints, named once beside the table.
const COLUMNS: &str =
    "Columns: task id, root, branch, head, changed, untracked, last used, Sessions, state";

/// One `dimcode worktree` command.
#[derive(Clone, Debug)]
pub enum Command {
    /// `worktree list`: every managed worktree and its state.
    List,
    /// `worktree remove TASK_ID`: one worktree and its record.
    Remove {
        task_id: String,
        /// Remove uncommitted and untracked content too.
        force: bool,
    },
    /// `worktree prune`: the unused worktrees one selector names.
    Prune {
        /// Sessions last used at least this long ago, as the flag was given:
        /// the daemon parses the same grammar.
        older_than: Option<String>,
        /// The most recently used worktrees to keep, leaving the rest.
        keep: Option<usize>,
        /// Remove without asking.
        yes: bool,
        /// Remove a worktree whose content is not committed too.
        force: bool,
    },
}

/// Runs one `worktree` command and returns the exit code.
pub async fn run(client: &mut Client, command: &Command, json: bool) -> Result<i32> {
    match command {
        Command::List => list(client, json).await,
        Command::Remove { task_id, force } => remove(client, task_id, *force, json).await,
        Command::Prune {
            older_than,
            keep,
            yes,
            force,
        } => prune(client, older_than.as_deref(), *keep, *yes, *force, json).await,
    }
}

/// `dimcode worktree list`: one row per managed worktree, under the product
/// home, and its state.
pub async fn list(client: &mut Client, json: bool) -> Result<i32> {
    if !client.supports(&["worktree.list"]) {
        return Err(LIST_UNSUPPORTED.into());
    }
    let reply = client.rpc("worktree.list", json!({})).await?;
    if json {
        print_json(&reply)?;
        return Ok(0);
    }
    let listed: Listed = decode(reply, "worktree.list")?;
    if listed.worktrees.is_empty() {
        println!("No managed worktree is recorded.");
        return Ok(0);
    }
    let now = now_ms();
    for worktree in &listed.worktrees {
        println!("{}", worktree.row(now));
    }
    eprintln!("{COLUMNS}");
    Ok(0)
}

/// `dimcode worktree remove TASK_ID`: removes one managed worktree and its
/// record. The daemon refuses one a Session still uses, and, without
/// `--force`, one whose content is not committed.
pub async fn remove(client: &mut Client, task_id: &str, force: bool, json: bool) -> Result<i32> {
    if !client.supports(&["worktree.remove"]) {
        return Err(REMOVE_UNSUPPORTED.into());
    }
    let params = json!({"taskId": task_id, "force": force});
    let reply = client.rpc("worktree.remove", params).await?;
    if json {
        print_json(&reply)?;
        return Ok(0);
    }
    let removed: Removed = decode(reply, "worktree.remove")?;
    let task_id = safe_line(task_id);
    if removed.removed {
        println!("Removed {task_id}");
    } else {
        // Another caller removed the record first: nothing of this run failed.
        println!("Already gone: {task_id}");
    }
    Ok(0)
}

/// `dimcode worktree prune`: shows the unused worktrees one selector names,
/// with the reasons the rest stay, then removes them after confirmation.
/// Without a terminal only `--yes` confirms; `--json` prints the daemon's
/// reply, which is the preview without `--yes` and the applied prune with it.
pub async fn prune(
    client: &mut Client,
    older_than: Option<&str>,
    keep: Option<usize>,
    yes: bool,
    force: bool,
    json: bool,
) -> Result<i32> {
    if !client.supports(&["worktree.prune"]) {
        return Err(PRUNE_UNSUPPORTED.into());
    }
    let reply = client
        .rpc(
            "worktree.prune",
            prune_params(older_than, keep, false, force, &[]),
        )
        .await?;
    let selection: Pruned = decode(reply.clone(), "worktree.prune")?;
    if json && !yes {
        print_json(&reply)?;
        return Ok(0);
    }
    let now = now_ms();
    for worktree in &selection.candidates {
        println!("{}", worktree.row(now));
    }
    if !selection.kept.is_empty() {
        println!("Kept:");
        for kept in &selection.kept {
            println!("{}\t{}", kept.worktree.row(now), safe_line(&kept.reason));
        }
    }
    if selection.candidates.is_empty() {
        println!("{}", nothing_to_prune(older_than, keep));
        return Ok(0);
    }
    if !yes && !confirmed(selection.candidates.len())? {
        println!("Nothing was removed.");
        return Ok(0);
    }
    // Apply exactly the previewed task ids: a selector re-read would remove a
    // set the operator never saw.
    let ids: Vec<String> = selection
        .candidates
        .iter()
        .map(|candidate| candidate.task_id.clone())
        .collect();
    let reply = client
        .rpc(
            "worktree.prune",
            prune_params(older_than, keep, true, force, &ids),
        )
        .await?;
    if json {
        print_json(&reply)?;
        return Ok(0);
    }
    let applied = decode::<Pruned>(reply, "worktree.prune")?;
    for task_id in &applied.removed {
        println!("Removed {}", safe_line(task_id));
    }
    for failure in &applied.failed {
        eprintln!(
            "Failed {}: {}",
            safe_line(&failure.task_id),
            safe_line(&failure.message)
        );
    }
    if applied.removed.is_empty() && applied.failed.is_empty() {
        println!("Nothing was removed.");
    }
    Ok(if applied.failed.is_empty() { 0 } else { 1 })
}

/// One managed worktree as `worktree.list` and `worktree.prune` report it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Worktree {
    task_id: String,
    root: String,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    head: Option<String>,
    /// How many tracked files Git reports changed.
    #[serde(default)]
    dirty: usize,
    /// How many untracked files Git reports.
    #[serde(default)]
    untracked: usize,
    #[serde(default)]
    last_used_at_ms: Option<i64>,
    #[serde(default)]
    sessions: Vec<String>,
    /// The directory is gone; the record is a candidate.
    #[serde(default)]
    missing: bool,
    /// The directory exists but Git no longer names it as a worktree.
    #[serde(default)]
    git_missing: bool,
    /// No record names it: a directory a failed creation left behind.
    #[serde(default)]
    unregistered: bool,
}

impl Worktree {
    fn row(&self, now: u64) -> String {
        let last_used = self
            .last_used_at_ms
            .and_then(|time| u64::try_from(time).ok())
            .map(|time| relative_time(time, now))
            .unwrap_or_else(|| "-".to_owned());
        let sessions = if self.sessions.is_empty() {
            "-".to_owned()
        } else {
            self.sessions
                .iter()
                .map(|session| safe_line(session))
                .collect::<Vec<_>>()
                .join(",")
        };
        format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{last_used}\t{sessions}\t{}",
            safe_line(&self.task_id),
            safe_line(&self.root),
            column(&self.branch),
            column(&self.head),
            self.dirty,
            self.untracked,
            self.state(),
        )
    }

    /// What is wrong with the worktree, or `ok` when nothing is.
    fn state(&self) -> &'static str {
        match (self.missing, self.git_missing, self.unregistered) {
            (false, false, false) => "ok",
            (true, _, false) => "missing",
            (_, true, false) => "git-missing",
            (false, false, true) => "unregistered",
            _ => "unusable",
        }
    }
}

/// An optional column, `-` when the daemon reported none.
fn column(value: &Option<String>) -> String {
    value
        .as_deref()
        .map(safe_line)
        .unwrap_or_else(|| "-".into())
}

/// The `worktree.list` reply.
#[derive(Deserialize)]
struct Listed {
    worktrees: Vec<Worktree>,
}

/// The `worktree.remove` reply.
#[derive(Deserialize)]
struct Removed {
    removed: bool,
}

/// The `worktree.prune` reply: what the selector chose, what it kept with
/// each reason, and the task ids an apply removed.
#[derive(Deserialize)]
struct Pruned {
    candidates: Vec<Worktree>,
    #[serde(default)]
    kept: Vec<Kept>,
    #[serde(default)]
    removed: Vec<String>,
    #[serde(default)]
    failed: Vec<Failed>,
}

/// One candidate an apply could not remove; its record stays.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Failed {
    task_id: String,
    message: String,
}

/// One worktree prune keeps.
#[derive(Deserialize)]
struct Kept {
    worktree: Worktree,
    reason: String,
}

/// Prints a reply as the daemon sent it, for `--json`.
fn print_json(reply: &Value) -> Result<()> {
    println!("{}", serde_json::to_string(reply)?);
    Ok(())
}

fn decode<T: serde::de::DeserializeOwned>(value: Value, method: &str) -> Result<T> {
    serde_json::from_value(value).map_err(|_| format!("Invalid {method} response").into())
}

/// The `worktree.prune` params: the selector, whether to apply, and whether
/// content is removed regardless.
fn prune_params(
    older_than: Option<&str>,
    keep: Option<usize>,
    apply: bool,
    force: bool,
    task_ids: &[String],
) -> Value {
    let mut params = json!({"apply": apply, "force": force});
    if let Some(older_than) = older_than {
        params["olderThan"] = json!(older_than);
    }
    if let Some(keep) = keep {
        params["keep"] = json!(keep);
    }
    if !task_ids.is_empty() {
        params["taskIds"] = json!(task_ids);
    }
    params
}

/// Asks on the terminal, where only `y` or `yes` confirms; elsewhere the
/// caller's `--yes` is the only confirmation.
fn confirmed(count: usize) -> Result<bool> {
    if !std::io::stdin().is_terminal() {
        return Err(
            "Prune removes worktrees and this is not a terminal; pass --yes to remove them without a prompt"
                .into(),
        );
    }
    eprint!("Remove {count} worktrees? [y/N] ");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

/// What a prune with no candidate says, naming the selector that found none.
fn nothing_to_prune(older_than: Option<&str>, keep: Option<usize>) -> String {
    match (older_than, keep) {
        (Some(age), _) => format!("Nothing to remove: no managed worktree is older than {age}."),
        (None, Some(keep)) => format!("Nothing to remove: --keep {keep} keeps every worktree."),
        (None, None) => "Nothing to remove.".to_owned(),
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_names_every_column_and_its_state() {
        let mut worktree = Worktree {
            task_id: "4f2abc".into(),
            root: "/home/worktrees/4f2abc/dim".into(),
            branch: Some("dim/4f2abc".into()),
            head: Some("abc1234".into()),
            dirty: 2,
            untracked: 1,
            last_used_at_ms: None,
            sessions: vec!["s1".into(), "s2".into()],
            missing: false,
            git_missing: false,
            unregistered: false,
        };
        assert_eq!(
            worktree.row(0),
            "4f2abc\t/home/worktrees/4f2abc/dim\tdim/4f2abc\tabc1234\t2\t1\t-\ts1,s2\tok"
        );
        worktree.branch = None;
        worktree.head = None;
        worktree.sessions.clear();
        worktree.unregistered = true;
        assert_eq!(
            worktree.row(0),
            "4f2abc\t/home/worktrees/4f2abc/dim\t-\t-\t2\t1\t-\t-\tunregistered"
        );
        worktree.missing = true;
        assert_eq!(
            worktree.row(0),
            "4f2abc\t/home/worktrees/4f2abc/dim\t-\t-\t2\t1\t-\t-\tunusable"
        );
    }
}
