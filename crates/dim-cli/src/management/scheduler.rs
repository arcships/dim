//! The Scheduler area: the jobs it holds and their runs (`schedule.*`).
//!
//! A job is the entry. Its name is the label; its schedule, next window and
//! last run are its detail; and its own document — the fields `schedule.save`
//! takes, as `schedule.list` reports them — is what edit writes at the
//! revision the owner reported. The id and the revision travel with the
//! request, not in the document, and the owner derives `created_by`,
//! `policy_cap` and `enabled_at`.

use std::path::Path;

use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::Groups;
use crate::client::Client;
use crate::model::session::{
    ActionKind, EntryDocument, EntryState, ManagementAction, ManagementDetail, ManagementEntry,
    ManagementView,
};
use crate::ui::i18n::{t, tf};

/// How many runs a detail asks for: what the owner calls recent history.
const RECENT_RUNS: u32 = 20;

/// The fields `schedule.save` takes, as `schedule.list` reports them.
const EDITABLE: &[&str] = &[
    "name",
    "schedule",
    "prompt",
    "cwd",
    "model",
    "mode",
    "policy",
    "max_run_minutes",
    "enabled",
    "session_id",
    "agent_id",
];

#[derive(Debug, Deserialize)]
pub struct Catalog {
    pub jobs: Vec<Job>,
}

/// One job as `schedule.list` reports it.
#[derive(Debug, Deserialize)]
pub struct Job {
    pub id: String,
    pub name: String,
    pub schedule: Schedule,
    pub enabled: bool,
    pub revision: i64,
    /// The next window the owner computed; none while the job is disabled,
    /// or when it cannot use the schedule.
    #[serde(default)]
    pub next_window: Option<String>,
    /// Why the owner cannot use the schedule, when it cannot.
    #[serde(default)]
    pub schedule_error: Option<String>,
    #[serde(default)]
    pub last_run: Option<Run>,
    /// The rest of the record, which the document takes from.
    #[serde(flatten)]
    fields: Map<String, Value>,
}

impl Job {
    /// The fields `schedule.save` takes, as the owner reported them. A field
    /// the job did not report is left out rather than invented.
    pub fn document(&self) -> Value {
        let mut document = Map::new();
        for key in EDITABLE {
            let value = match *key {
                "name" => json!(self.name),
                "schedule" => schedule_value(&self.schedule),
                "enabled" => json!(self.enabled),
                other => match self.fields.get(other) {
                    Some(value) => value.clone(),
                    None => continue,
                },
            };
            document.insert((*key).to_owned(), value);
        }
        Value::Object(document)
    }
}

/// A job's schedule (Scheduler A.4).
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Schedule {
    Every {
        every_minutes: u64,
    },
    At {
        at: String,
    },
    Cron {
        cron: String,
        #[serde(default)]
        tz: Option<String>,
    },
}

/// One run of a job (Scheduler A.1): `schedule.history` reports these, and
/// `schedule.run_now` answers with the run it accepted or skipped.
#[derive(Debug, Deserialize)]
pub struct Run {
    pub state: String,
    pub created_at: String,
    #[serde(default)]
    pub ended_at: Option<String>,
    /// Why the run ended that way, when the owner says.
    #[serde(default)]
    pub detail: String,
}

impl Run {
    /// When the run ended, or when it was created while it still runs.
    pub fn instant(&self) -> &str {
        self.ended_at.as_deref().unwrap_or(&self.created_at)
    }

    /// What the run came to: its state, and the owner's reason for it.
    pub fn outcome(&self) -> String {
        if self.detail.is_empty() {
            self.state.clone()
        } else {
            format!("{} ({})", self.state, self.detail)
        }
    }
}

#[derive(Debug, Deserialize)]
struct Runs {
    runs: Vec<Run>,
}

/// What `schedule.save` answered for a create.
#[derive(Debug, Deserialize)]
struct Saved {
    job: SavedJob,
}

#[derive(Debug, Deserialize)]
struct SavedJob {
    name: String,
}

/// Why a Scheduler action did not happen.
pub enum Failure {
    /// The jobs changed since they were read: nothing was written, and this
    /// is the area as it now is, or why the read that reports it failed.
    Conflict {
        message: String,
        catalog: Result<Box<Catalog>, String>,
    },
    /// The action was applied and what follows it could not be read: the
    /// change was made, the owner's answer is kept when it was read, and this
    /// is why the jobs are not shown.
    ReadFailed { note: Option<String>, error: String },
    /// The daemon or the owner refused it: nothing was written.
    Refused(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict { message, catalog } => match catalog {
                Ok(_) => formatter.write_str(message),
                Err(error) => write!(formatter, "{message} {error}"),
            },
            Self::ReadFailed { error: message, .. } | Self::Refused(message) => {
                formatter.write_str(message)
            }
        }
    }
}

/// What a Scheduler action came to.
pub enum Answer {
    /// The jobs after the action, and what the owner answered when that is
    /// more than the catalog.
    Jobs {
        catalog: Catalog,
        note: Option<String>,
    },
    /// An inspect: one job's recent runs, not a view.
    Detail(ManagementDetail),
}

/// What the owner answered, when its reply says more than the catalog.
enum Answered {
    Nothing,
    Created(String),
    Run { id: String, run: Run },
}

/// Performs `action`, then reads the area back as its owner reports it.
pub async fn apply(client: &mut Client, action: &ManagementAction) -> Result<Answer, Failure> {
    let answered = match action {
        ManagementAction::List | ManagementAction::Reload => Answered::Nothing,
        ManagementAction::Toggle { id, enabled } => {
            let written = client
                .rpc(
                    "schedule.set_enabled",
                    json!({"job_id": id, "enabled": enabled}),
                )
                .await;
            if let Err(error) = written {
                return Err(conflict(client, &error, ConflictKind::Job).await);
            }
            Answered::Nothing
        }
        ManagementAction::Add { document } => {
            let saved = client.rpc("schedule.save", json!({"job": document})).await;
            let saved = match saved {
                Ok(saved) => saved,
                Err(error) => return Err(conflict(client, &error, ConflictKind::Create).await),
            };
            // The method returned, so the job was written. What failed is the
            // answer: reporting a refusal here would keep the draft and invite
            // a duplicate.
            let saved: Saved =
                serde_json::from_value(saved).map_err(|error| Failure::ReadFailed {
                    note: None,
                    error: format!("schedule.save: {error}"),
                })?;
            Answered::Created(saved.job.name)
        }
        ManagementAction::Save {
            id,
            expected_revision,
            document,
        } => {
            let Some(id) = id else {
                return Err(Failure::Refused(
                    t("The Scheduler has no area document.").to_owned(),
                ));
            };
            save(client, id, expected_revision, document).await?
        }
        ManagementAction::Remove { id } => {
            let deleted = client.rpc("schedule.delete", json!({"job_id": id})).await;
            if let Err(error) = deleted {
                return Err(conflict(client, &error, ConflictKind::Remove).await);
            }
            Answered::Nothing
        }
        ManagementAction::Run { id } => {
            let run = client
                .rpc("schedule.run_now", json!({"job_id": id}))
                .await
                .map_err(|error| Failure::Refused(error.to_string()))?;
            // The method returned, so the run was accepted. What failed is
            // the answer, not the work.
            let run: Run = serde_json::from_value(run).map_err(|error| Failure::ReadFailed {
                note: None,
                error: format!("schedule.run_now: {error}"),
            })?;
            Answered::Run {
                id: id.clone(),
                run,
            }
        }
        ManagementAction::Inspect { id } => return detail(client, id).await,
    };
    // Every action that gets here but a read called a method its owner
    // declares mutating; `schedule.history` is the other read, and it
    // returned above.
    let applied = !matches!(action, ManagementAction::List | ManagementAction::Reload);
    let catalog = match catalog(client).await {
        Ok(catalog) => catalog,
        Err(error) if applied => {
            // The action was applied: only the read that reports the jobs
            // failed, so the change stands and the owner's answer is kept.
            let note = match &answered {
                Answered::Nothing => None,
                Answered::Created(name) => Some(tf("Added {name}.", &[("name", name)])),
                // The read that names the job failed; the id names it.
                Answered::Run { id, run } => Some(run_note(id, run)),
            };
            return Err(Failure::ReadFailed { note, error });
        }
        Err(error) => return Err(Failure::Refused(error)),
    };
    let note = match &answered {
        Answered::Nothing => None,
        Answered::Created(name) => Some(tf("Added {name}.", &[("name", name)])),
        Answered::Run { id, run } => Some(run_note(&label(&catalog, id), run)),
    };
    Ok(Answer::Jobs { catalog, note })
}

/// Writes one job's edit as the members the draft changed. `schedule.save`
/// merges a patch over the stored job, so the adapter reads the job again,
/// refuses a draft that is no longer at its revision, and sends only the
/// members that differ: a member left out would silently keep its stored
/// value, and one the draft deleted cannot be cleared at all.
async fn save(
    client: &mut Client,
    id: &str,
    expected_revision: &str,
    document: &Value,
) -> Result<Answered, Failure> {
    let catalog = catalog(client).await.map_err(Failure::Refused)?;
    let Some(job) = catalog.jobs.iter().find(|job| job.id == id) else {
        return Err(Failure::Refused(
            t("No scheduled job has that id.").to_owned(),
        ));
    };
    if job.revision.to_string() != expected_revision {
        return Err(Failure::Conflict {
            message: t(conflict_message(ConflictKind::Job)).to_owned(),
            catalog: Ok(Box::new(catalog)),
        });
    }
    let Some(draft) = document.as_object() else {
        return Err(Failure::Refused(
            "schedule.save: the document is not an object.".to_owned(),
        ));
    };
    let stored = job.document();
    let stored = stored.as_object().expect("a job document is an object");
    let mut patch = Map::new();
    for key in EDITABLE {
        match draft.get(*key) {
            Some(value) => {
                if stored.get(*key) != Some(value) {
                    patch.insert((*key).to_owned(), value.clone());
                }
            }
            // The patch schema has no null: a member the draft deleted would
            // keep its stored value, so it is refused by name. The exception
            // is the Session of a job that becomes a create, which the owner
            // clears itself when the patch leaves it out.
            None if stored.contains_key(*key) => {
                if *key == "session_id"
                    && draft.get("mode").and_then(Value::as_str) == Some("create")
                {
                    continue;
                }
                return Err(Failure::Refused(tf(
                    "The document deleted {field}, which the owner cannot clear; nothing was written.",
                    &[("field", key)],
                )));
            }
            None => {}
        }
    }
    if patch.is_empty() {
        return Err(Failure::Refused(
            t("The document changes no field; nothing was written.").to_owned(),
        ));
    }
    patch.insert("id".to_owned(), json!(id));
    let written = client
        .rpc(
            "schedule.save",
            json!({"job": patch, "revision": job.revision}),
        )
        .await;
    if let Err(error) = written {
        return Err(conflict(client, &error, ConflictKind::Job).await);
    }
    Ok(Answered::Nothing)
}

/// Whether a conflict concerns one job's revision or the jobs as a whole.
#[derive(Clone, Copy)]
enum ConflictKind {
    /// One job's document changed under the client.
    Job,
    /// The jobs changed while a new one was being created.
    Create,
    /// The jobs changed while one was being removed.
    Remove,
}

/// The owner's sentence for a conflict of `kind`: what changed, and that
/// nothing was written. Where the text still is belongs to the surface.
fn conflict_message(kind: ConflictKind) -> &'static str {
    match kind {
        ConflictKind::Job => "The job changed since it was read; nothing was written.",
        ConflictKind::Create => "The jobs changed since they were read; nothing was written.",
        ConflictKind::Remove => "The job changed since it was read; nothing was removed.",
    }
}

/// The area after a refusal, when the owner refused because the jobs
/// changed; any other refusal leaves the draft where it is. A read that
/// fails is still a conflict, and says why it cannot show the area.
async fn conflict(client: &mut Client, error: &crate::Error, kind: ConflictKind) -> Failure {
    if crate::client::app_error(error) != Some("schedule_conflict") {
        return Failure::Refused(error.to_string());
    }
    Failure::Conflict {
        message: t(conflict_message(kind)).to_owned(),
        catalog: catalog(client).await.map(Box::new),
    }
}

/// One job's recent runs, with the name the catalog reports for its title.
async fn detail(client: &mut Client, id: &str) -> Result<Answer, Failure> {
    let catalog = catalog(client).await.map_err(Failure::Refused)?;
    let job = catalog
        .jobs
        .iter()
        .find(|job| job.id == id)
        .ok_or_else(|| Failure::Refused(t("No scheduled job has that id.").to_owned()))?;
    let runs: Runs = client
        .typed(
            "schedule.history",
            json!({"job_id": id, "limit": RECENT_RUNS}),
        )
        .await
        .map_err(|error| Failure::Refused(error.to_string()))?;
    let lines = if runs.runs.is_empty() {
        vec![t("No run yet.").to_owned()]
    } else {
        runs.runs
            .iter()
            .map(|run| format!("{}  {}", run.instant(), run.outcome()))
            .collect()
    };
    Ok(Answer::Detail(ManagementDetail {
        title: tf("Recent runs of {name}", &[("name", &job.name)]),
        lines,
    }))
}

/// The jobs as their owner reports them.
pub async fn catalog(client: &mut Client) -> Result<Catalog, String> {
    client
        .typed("schedule.list", json!({}))
        .await
        .map_err(|error| error.to_string())
}

/// The jobs as entries, with the kinds the daemon can serve.
pub(super) fn view(catalog: &Catalog, available: &[ActionKind], cwd: &Path) -> ManagementView {
    let mut groups = Groups::default();
    for job in &catalog.jobs {
        let mut states = Vec::new();
        if job.schedule_error.is_some() {
            states.push(EntryState::Invalid);
        }
        groups.push(
            t("jobs"),
            ManagementEntry {
                id: job.id.clone(),
                label: job.name.clone(),
                detail: detail_lines(job),
                enabled: Some(job.enabled),
                states,
                actions: super::entry_kinds(available),
                document: Some(EntryDocument {
                    revision: job.revision.to_string(),
                    document: job.document(),
                }),
            },
        );
    }
    ManagementView {
        revision: None,
        document: None,
        create: Some(create(cwd)),
        actions: super::area_kinds(available),
        groups: groups.into_vec(),
        issues: Vec::new(),
    }
}

/// What a job says about itself: its schedule, its next window and how its
/// last run ended.
fn detail_lines(job: &Job) -> Vec<String> {
    let mut lines = vec![schedule_text(&job.schedule)];
    match &job.next_window {
        Some(instant) => lines.push(tf("Next run {instant}.", &[("instant", instant)])),
        None => lines.push(t("No next run.").to_owned()),
    }
    match &job.last_run {
        Some(run) => lines.push(tf(
            "Last run {outcome} at {instant}.",
            &[("outcome", &run.outcome()), ("instant", run.instant())],
        )),
        None => lines.push(t("No run yet.").to_owned()),
    }
    if let Some(error) = &job.schedule_error {
        lines.push(error.clone());
    }
    lines
}

/// A schedule as one line.
pub fn schedule_text(schedule: &Schedule) -> String {
    match schedule {
        Schedule::Every { every_minutes } => tf(
            "every {minutes} minutes",
            &[("minutes", &every_minutes.to_string())],
        ),
        Schedule::At { at } => tf("at {instant}", &[("instant", at)]),
        Schedule::Cron {
            cron,
            tz: Some(zone),
        } => tf(
            "cron {expression} in {zone}",
            &[("expression", cron), ("zone", zone)],
        ),
        Schedule::Cron { cron, tz: None } => tf("cron {expression}", &[("expression", cron)]),
    }
}

/// A schedule as the owner wrote it.
fn schedule_value(schedule: &Schedule) -> Value {
    match schedule {
        Schedule::Every { every_minutes } => json!({"every_minutes": every_minutes}),
        Schedule::At { at } => json!({"at": at}),
        Schedule::Cron {
            cron,
            tz: Some(zone),
        } => json!({"cron": cron, "tz": zone}),
        Schedule::Cron { cron, tz: None } => json!({"cron": cron}),
    }
}

/// What a run reported, as the message after it was sent.
fn run_note(name: &str, run: &Run) -> String {
    tf(
        "Run {name}: {outcome}.",
        &[("name", name), ("outcome", &run.outcome())],
    )
}

/// The name the catalog reports for `id`, or the id itself.
fn label(catalog: &Catalog, id: &str) -> String {
    catalog
        .jobs
        .iter()
        .find(|job| job.id == id)
        .map(|job| job.name.clone())
        .unwrap_or_else(|| id.to_owned())
}

/// The document an add starts from: the owner's create shape, filled with
/// example values.
fn create(cwd: &Path) -> Value {
    json!({
        "name": "example",
        "schedule": {"every_minutes": 60},
        "prompt": "Summarize what changed in this workspace today.",
        "cwd": cwd.to_string_lossy(),
    })
}
