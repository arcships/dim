//! The Workflow area: its definitions and the kinds the client can do on
//! them (`workflow.list`, `workflow.save` and `workflow.start`, plus the
//! Settings client methods the toggle and a user definition's edit use).
//!
//! A definition's enabled state lives in the user Settings document, keyed
//! by workspace and name, so it is a client write of that document; the
//! owner's own service refuses the member. A user definition's write reads
//! the same document for its revision. Project definitions are files in the
//! repository: the owner serves no per-file document, so the area lists,
//! enables and starts them, and they are edited where they live.

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

/// The Settings source the owner declares for its document.
const SOURCE: &str = "workflow";

/// One definition as `workflow.list` reports it (`visible()` in
/// `runtime/plugins/workflow/index.ts`).
#[derive(Debug, Deserialize)]
pub struct Entry {
    pub name: String,
    /// `user`, `project` or `plugin`.
    pub source: String,
    #[serde(default)]
    pub package_id: Option<String>,
    pub enabled: bool,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub parameters: Vec<Parameter>,
    /// Whether the owner could use the definition.
    pub valid: bool,
    #[serde(default)]
    pub issues: Vec<String>,
    #[serde(default)]
    pub issue_count: usize,
}

/// One declared parameter of a definition.
#[derive(Debug, Deserialize)]
pub struct Parameter {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default: Option<String>,
}

/// The `workflow.list` reply: the definitions, and whether the owner cut the
/// listing at one frame's payload.
#[derive(Debug, Deserialize)]
struct Catalog {
    definitions: Vec<Entry>,
    #[serde(default)]
    truncated: bool,
}

/// The user Settings document and its revision, read as the owner stores it.
#[derive(Debug, Deserialize)]
struct Snapshot {
    revision: String,
    document: Value,
}

/// The definitions as their owner reports them, with the user document when
/// the daemon serves `settings.read`.
pub struct Listing {
    pub entries: Vec<Entry>,
    settings: Option<Snapshot>,
    /// The owner stopped at one payload: later definitions are not listed.
    pub truncated: bool,
}

impl Listing {
    /// The entry `id` names: one of the panel's ids (`user:<name>`,
    /// `project:<name>`, `plugin:<package>:<name>`), or a bare name.
    ///
    /// A bare name resolves by source precedence, project then user then
    /// package, and an enabled entry first: a name several sources define
    /// takes the one that can start, and a name only a disabled definition
    /// carries still resolves, since a project definition starts disabled.
    pub fn find(&self, id: &str) -> Option<&Entry> {
        if let Some((source, rest)) = id.split_once(':') {
            return match source {
                "user" | "project" => self
                    .entries
                    .iter()
                    .find(|entry| entry.source == source && entry.name == rest),
                "plugin" => {
                    let (package, name) = rest.split_once(':')?;
                    self.entries.iter().find(|entry| {
                        entry.source == "plugin"
                            && entry.name == name
                            && entry.package_id.as_deref() == Some(package)
                    })
                }
                // A name with a colon of its own is not one of the ids this
                // client builds.
                _ => None,
            };
        }
        let named = |source: &&str| {
            self.entries
                .iter()
                .find(|entry| entry.name == id && entry.source == *source)
        };
        ["project", "user", "plugin"]
            .iter()
            .find_map(|source| named(source).filter(|entry| entry.enabled))
            .or_else(|| ["project", "user", "plugin"].iter().find_map(named))
    }

    /// The user definition `entry` names, with the revision a write of it
    /// takes: the whole user document's.
    pub fn document(&self, entry: &Entry) -> Option<(&str, &Value)> {
        if entry.source != "user" {
            return None;
        }
        let settings = self.settings.as_ref()?;
        let definition = settings.document.get("definitions")?.get(&entry.name)?;
        Some((&settings.revision, definition))
    }
}

/// Why a Workflow action did not produce a view.
pub enum Failure {
    /// The user document changed since it was read: nothing was written, and
    /// this is the area as it now is, or why the read that reports it failed.
    Conflict {
        message: String,
        listing: Result<Box<Listing>, String>,
    },
    /// The action was applied and the read that follows failed.
    ReadFailed { note: Option<String>, error: String },
    /// The daemon or the owner refused it: nothing was written.
    Refused(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict { message, listing } => match listing {
                Ok(_) => formatter.write_str(message),
                Err(error) => write!(formatter, "{message} {error}"),
            },
            Self::ReadFailed { error: message, .. } | Self::Refused(message) => {
                formatter.write_str(message)
            }
        }
    }
}

/// What a Workflow action came to.
pub enum Answer {
    /// The definitions after the action, and what the owner answered when
    /// that is more than the list.
    Definitions {
        listing: Listing,
        note: Option<String>,
    },
    /// An inspect: one definition's detail, not a view.
    Detail(ManagementDetail),
}

/// What `workflow.save` answers.
#[derive(Debug, Deserialize)]
struct Saved {
    name: String,
}

/// One run as `workflow.status` reports it, for the parts an inspect shows.
#[derive(Debug, Deserialize)]
pub struct Run {
    pub run_id: String,
    pub name: String,
    pub source: String,
    /// The workspace the run started in.
    pub cwd: String,
    pub state: String,
    pub created_at: String,
    #[serde(default)]
    pub ended_at: Option<String>,
}

impl Run {
    /// When the run ended, or when it was created while it still runs.
    fn instant(&self) -> &str {
        self.ended_at.as_deref().unwrap_or(&self.created_at)
    }
}

/// Performs `action`, then reads the area back as its owner reports it.
pub async fn apply(
    client: &mut Client,
    action: &ManagementAction,
    cwd: &Path,
    line: Option<&super::SessionLine>,
) -> Result<Answer, Failure> {
    let cwd = cwd.to_string_lossy().into_owned();
    let mut note = None;
    match action {
        ManagementAction::List | ManagementAction::Reload => {}
        ManagementAction::Toggle { id, enabled } => {
            toggle(client, id, *enabled, &cwd).await?;
        }
        ManagementAction::Add { document } => {
            let (source, name) = add(client, document, &cwd).await?;
            note = Some(if source == "project" {
                tf(
                    "Saved {name}. It stays disabled until you enable it.",
                    &[("name", &name)],
                )
            } else {
                tf("Saved {name}.", &[("name", &name)])
            });
        }
        ManagementAction::Save {
            id,
            expected_revision,
            document,
        } => {
            let Some(id) = id else {
                return Err(Failure::Refused(
                    "The Workflow area has no document of its own.".to_owned(),
                ));
            };
            // The save's name is the shared line's: the surface composes
            // "Saved {name}." from the read that follows, which holds it.
            save(client, id, expected_revision, document, &cwd).await?;
        }
        ManagementAction::Run { id, document, seed } => {
            let (name, run_id) =
                start(client, id, document.as_ref(), seed.as_ref(), &cwd, line).await?;
            note = Some(tf(
                "Started {name}: {run_id}.",
                &[("name", &name), ("run_id", &run_id)],
            ));
        }
        ManagementAction::Inspect { id } => return inspect(client, id, &cwd).await,
        // `management::supported` refuses a kind this area does not offer
        // before the adapter runs.
        ManagementAction::Remove { .. } => {
            return Err(Failure::Refused(
                "The Workflow area has no such action.".to_owned(),
            ));
        }
    }
    let applied = !matches!(action, ManagementAction::List | ManagementAction::Reload);
    match read(client, Path::new(&cwd)).await {
        Ok(listing) => Ok(Answer::Definitions { listing, note }),
        Err(error) if applied => Err(Failure::ReadFailed { note, error }),
        Err(error) => Err(Failure::Refused(error)),
    }
}

/// The definitions as their owner reports them, with the user document when
/// the daemon serves its read.
pub async fn read(client: &mut Client, cwd: &Path) -> Result<Listing, String> {
    let catalog = catalog(client, &cwd.to_string_lossy()).await?;
    let settings = if client.supports(&["settings.read"]) {
        Some(
            client
                .typed("settings.read", json!({"source": SOURCE, "scope": "user"}))
                .await
                .map_err(|error| error.to_string())?,
        )
    } else {
        None
    };
    Ok(Listing {
        entries: catalog.definitions,
        settings,
        truncated: catalog.truncated,
    })
}

/// `workflow.list` alone, for a read that needs no user document.
async fn catalog(client: &mut Client, cwd: &str) -> Result<Catalog, String> {
    client
        .typed("workflow.list", json!({"cwd": cwd}))
        .await
        .map_err(|error| error.to_string())
}

/// Enables or disables the project definition `id` names.
async fn toggle(client: &mut Client, id: &str, enabled: bool, cwd: &str) -> Result<(), Failure> {
    let name = id.strip_prefix("project:").unwrap_or(id);
    // The panel lists project entries only; the CLI twin's name is typed, so
    // it is checked against the owner's list rather than trusted.
    let entries = catalog(client, cwd)
        .await
        .map_err(Failure::Refused)?
        .definitions;
    if !entries
        .iter()
        .any(|entry| entry.source == "project" && entry.name == name)
    {
        return Err(Failure::Refused(tf(
            "No project definition is named {name}.",
            &[("name", name)],
        )));
    }
    let snapshot: Snapshot = client
        .typed("settings.read", json!({"source": SOURCE, "scope": "user"}))
        .await
        .map_err(|error| Failure::Refused(error.to_string()))?;
    let document = enabled_document(&snapshot.document, cwd, name, enabled).ok_or_else(|| {
        Failure::Refused("The workflow settings document is not an object.".to_owned())
    })?;
    let written = client
        .rpc(
            "settings.update",
            json!({
                "source": SOURCE,
                "scope": "user",
                "expectedRevision": snapshot.revision,
                "document": document,
            }),
        )
        .await;
    if let Err(error) = written {
        if crate::client::app_error(&error) != Some("settings_conflict") {
            return Err(Failure::Refused(error.to_string()));
        }
        return Err(conflict(
            client,
            cwd,
            t("The definitions changed since they were read; nothing was written."),
        )
        .await);
    }
    Ok(())
}

/// Writes the user definition `id` names at `expected_revision`, and answers
/// the name the owner saved.
async fn save(
    client: &mut Client,
    id: &str,
    expected_revision: &str,
    document: &Value,
    cwd: &str,
) -> Result<String, Failure> {
    let name = id.strip_prefix("user:").unwrap_or(id);
    // The owner keys a definition by the document's name, and this area has
    // no remove: a rename would save a second definition and leave the old
    // one behind.
    if let Some(saved_as) = document.get("name").and_then(Value::as_str)
        && saved_as != name
    {
        return Err(Failure::Refused(
            t("A definition's name is its key; add it to save it under another name.").to_owned(),
        ));
    }
    let request = json!({
        "source": "user",
        "expectedRevision": expected_revision,
        "document": document,
    });
    let saved = match client.rpc("workflow.save", request).await {
        Ok(saved) => saved,
        Err(error) => {
            if crate::client::app_error(&error) != Some("conflict") {
                return Err(Failure::Refused(error.to_string()));
            }
            return Err(conflict(
                client,
                cwd,
                t("The definition changed since it was read; nothing was written."),
            )
            .await);
        }
    };
    let saved: Saved = serde_json::from_value(saved)
        .map_err(|error| Failure::Refused(format!("workflow.save: {error}")))?;
    Ok(saved.name)
}

/// Creates or replaces a definition from the add document `{source,
/// document}`, and answers the source it went to and the name it saved.
async fn add(client: &mut Client, input: &Value, cwd: &str) -> Result<(String, String), Failure> {
    let Some(object) = input.as_object() else {
        return Err(Failure::Refused(
            t("A workflow definition is saved as an object with a source and a document.")
                .to_owned(),
        ));
    };
    let source = object
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("user");
    let document = object.get("document").cloned().unwrap_or(Value::Null);
    let request = match source {
        "user" => json!({"source": "user", "document": document}),
        "project" => json!({"source": "project", "cwd": cwd, "document": document}),
        _ => {
            return Err(Failure::Refused(
                t("A definition is saved to the user source or the project source.").to_owned(),
            ));
        }
    };
    let saved = match client.rpc("workflow.save", request).await {
        Ok(saved) => saved,
        Err(error) => {
            if crate::client::app_error(&error) != Some("conflict") {
                return Err(Failure::Refused(error.to_string()));
            }
            return Err(conflict(
                client,
                cwd,
                t("The definitions changed since they were read; nothing was written."),
            )
            .await);
        }
    };
    let saved: Saved = serde_json::from_value(saved)
        .map_err(|error| Failure::Refused(format!("workflow.save: {error}")))?;
    Ok((source.to_owned(), saved.name))
}

/// What `workflow.start` answers.
#[derive(Debug, Deserialize)]
struct Started {
    run_id: String,
}

/// Why `id` names no definition: when the listing was cut at its size limit,
/// the definition may be past the cut, and the message says so.
fn not_found(listing: &Listing, id: &str) -> Failure {
    if listing.truncated {
        Failure::Refused(tf(
            "No workflow definition named {name} is in the list, which reached its size limit.",
            &[("name", name_of(id))],
        ))
    } else {
        Failure::Refused(tf(
            "No workflow definition is named {name}.",
            &[("name", name_of(id))],
        ))
    }
}

/// Starts the definition `id` names with its parameters, and answers the
/// name and the run the owner started.
async fn start(
    client: &mut Client,
    id: &str,
    document: Option<&Value>,
    seed: Option<&Value>,
    cwd: &str,
    line: Option<&super::SessionLine>,
) -> Result<(String, String), Failure> {
    let catalog = catalog(client, cwd).await.map_err(Failure::Refused)?;
    let listing = Listing {
        entries: catalog.definitions,
        settings: None,
        truncated: catalog.truncated,
    };
    let entry = listing.find(id).ok_or_else(|| not_found(&listing, id))?;
    // The editor's document is the user's parameter input, checked at the
    // client boundary: an object whose keys the definition declares and
    // whose values are strings. An empty string is a supplied value.
    let params = match document {
        None => json!({}),
        Some(document) => {
            let well_formed = document.as_object().is_some_and(|object| {
                object.values().all(Value::is_string)
                    && object.keys().all(|key| {
                        entry
                            .parameters
                            .iter()
                            .any(|parameter| parameter.name == *key)
                    })
            });
            if !well_formed {
                return Err(Failure::Refused(
                    t("Parameters are a JSON object of strings.").to_owned(),
                ));
            }
            document.clone()
        }
    };
    let params = sent_params(params, seed);
    let mut request = json!({
        "name": entry.name,
        "source": entry.source,
        "cwd": cwd,
        "params": params,
    });
    // A panel start reports to the Session's line, which is how `/tasks`
    // finds the run; the command line has no Session and reports nowhere.
    if let Some(line) = line {
        request["report_to"] = json!({
            "session_id": line.session_id,
            "branch_id": line.branch_id,
        });
    }
    let started: Started = client
        .typed("workflow.start", request)
        .await
        .map_err(|error| Failure::Refused(error.to_string()))?;
    Ok((entry.name.clone(), started.run_id))
}

/// The area after a conflict, when the definitions changed since they were
/// read: nothing was written, and this is the area as it now is, or why the
/// read that reports it failed.
async fn conflict(client: &mut Client, cwd: &str, message: &str) -> Failure {
    Failure::Conflict {
        message: message.to_owned(),
        listing: read(client, Path::new(cwd)).await.map(Box::new),
    }
}

/// `document` with `projectEnabled[cwd][name]` set to `enabled`, copied as it
/// was read.
///
/// The member is the one the Workflow owner declares user-only in
/// `runtime/plugins/workflow/settings.ts` (`userOnly: ["/projectEnabled"]`):
/// the owner's own write refuses it, so the client writes the document
/// through `settings.update`, which may change any field. Copying the
/// document as read keeps every member this client does not know and every
/// other workspace's map.
fn enabled_document(document: &Value, cwd: &str, name: &str, enabled: bool) -> Option<Value> {
    let mut document = document.clone();
    let root = document.as_object_mut()?;
    let projects = root
        .entry("projectEnabled")
        .or_insert_with(|| Value::Object(Map::new()));
    let projects = projects.as_object_mut()?;
    let workspace = projects
        .entry(cwd)
        .or_insert_with(|| Value::Object(Map::new()));
    let workspace = workspace.as_object_mut()?;
    workspace.insert(name.to_owned(), json!(enabled));
    Some(document)
}

/// One definition's recent runs, or its detail when no run store is read.
async fn inspect(client: &mut Client, id: &str, cwd: &str) -> Result<Answer, Failure> {
    let catalog = catalog(client, cwd).await.map_err(Failure::Refused)?;
    let listing = Listing {
        entries: catalog.definitions,
        settings: None,
        truncated: catalog.truncated,
    };
    let entry = listing.find(id).ok_or_else(|| not_found(&listing, id))?;
    let runs: Vec<Run> = client
        .typed("workflow.status", json!({}))
        .await
        .map_err(|error| Failure::Refused(error.to_string()))?;
    // A project definition belongs to one workspace, so only its runs
    // started there are its history. A user or package definition is the
    // same everywhere, so every run of it is, and each line names the
    // workspace it started in.
    let mut runs: Vec<Run> = runs
        .into_iter()
        .filter(|run| run.name == entry.name && run.source == entry.source)
        .filter(|run| entry.source != "project" || run.cwd == cwd)
        .collect();
    runs.sort_by(|a, b| b.instant().cmp(a.instant()));
    runs.truncate(10);
    let mut lines = detail_lines(entry, Path::new(cwd));
    lines.push(t("Recent runs").to_owned());
    if runs.is_empty() {
        lines.push(t("No run yet.").to_owned());
    } else {
        lines.extend(runs.iter().map(|run| {
            let mut line = format!("{}  {}  {}", run.instant(), run.state, run.run_id);
            if entry.source != "project" {
                line.push_str(&format!("  {}", run.cwd));
            }
            line
        }));
    }
    Ok(Answer::Detail(ManagementDetail {
        title: entry.name.clone(),
        lines,
    }))
}

/// The name an entry id carries, for a message about an id the area no
/// longer lists.
fn name_of(id: &str) -> &str {
    id.rsplit(':').next().unwrap_or(id)
}

/// The definitions as entries, with the kinds the daemon can serve.
pub(super) fn view(listing: &Listing, available: &[ActionKind], cwd: &Path) -> ManagementView {
    let mut groups = Groups::default();
    for entry in &listing.entries {
        let mut states = Vec::new();
        if !entry.valid {
            states.push(EntryState::Invalid);
        }
        let heading = match entry.source.as_str() {
            "plugin" => format!("plugin:{}", entry.package_id.as_deref().unwrap_or("")),
            source => source.to_owned(),
        };
        let document = listing
            .document(entry)
            .map(|(revision, document)| EntryDocument {
                revision: revision.to_owned(),
                document: document.clone(),
            });
        groups.push(
            &heading,
            ManagementEntry {
                id: id_of(entry),
                label: entry.name.clone(),
                detail: detail_lines(entry, cwd),
                enabled: Some(entry.enabled),
                states,
                actions: kinds(entry)
                    .iter()
                    .filter(|kind| available.contains(kind))
                    .copied()
                    .collect(),
                document,
                run: run_template(entry),
            },
        );
    }
    let mut issues = Vec::new();
    if listing.truncated {
        issues.push(
            t("The list reached its size limit; later definitions are not shown.").to_owned(),
        );
    }
    ManagementView {
        revision: None,
        document: None,
        create: Some(create()),
        actions: super::area_kinds(available),
        groups: groups.into_vec(),
        issues,
    }
}

/// The id a panel action carries back: the source, the package when the
/// definition came from one, and the name.
fn id_of(entry: &Entry) -> String {
    match (entry.source.as_str(), entry.package_id.as_deref()) {
        ("user" | "project", _) => format!("{}:{}", entry.source, entry.name),
        (_, Some(package)) => format!("plugin:{package}:{}", entry.name),
        (_, None) => format!("plugin::{}", entry.name),
    }
}

/// The kinds one entry offers before the daemon's advertisement is applied:
/// a disabled or invalid entry never starts, and a user entry is always
/// enabled.
fn kinds(entry: &Entry) -> &'static [ActionKind] {
    match (entry.source.as_str(), entry.valid, entry.enabled) {
        ("user", true, _) => &[ActionKind::Edit, ActionKind::Run, ActionKind::Inspect],
        ("user", false, _) => &[ActionKind::Edit, ActionKind::Inspect],
        ("project", true, true) => &[ActionKind::Toggle, ActionKind::Run, ActionKind::Inspect],
        ("project", _, _) => &[ActionKind::Toggle, ActionKind::Inspect],
        ("plugin", true, _) => &[ActionKind::Run, ActionKind::Inspect],
        _ => &[ActionKind::Inspect],
    }
}

/// The operation's input document: one key per declared parameter, its
/// default where the definition gives one and an empty string where it does
/// not. A definition with no parameters starts at once, so it carries none.
fn run_template(entry: &Entry) -> Option<Value> {
    if !kinds(entry).contains(&ActionKind::Run) || entry.parameters.is_empty() {
        return None;
    }
    Some(Value::Object(
        entry
            .parameters
            .iter()
            .map(|parameter| {
                (
                    parameter.name.clone(),
                    json!(parameter.default.clone().unwrap_or_default()),
                )
            })
            .collect(),
    ))
}

/// The parameters to send for a run. With a seed (the document the panel's
/// editor opened with), a parameter still at its seed value is left out, so
/// the owner applies the definition's full default even when the seed held a
/// cut preview. An empty seed value is the placeholder of a parameter with
/// no default, not a default, and stays a supplied value. Without a seed the
/// document came from a file or stdin and is sent as given.
fn sent_params(params: Value, seed: Option<&Value>) -> Value {
    let Some(seed) = seed.and_then(Value::as_object) else {
        return params;
    };
    let mut params = params;
    if let Some(object) = params.as_object_mut() {
        for (name, seeded) in seed {
            let Some(seeded) = seeded.as_str().filter(|value| !value.is_empty()) else {
                continue;
            };
            if object.get(name).and_then(Value::as_str) == Some(seeded) {
                object.remove(name);
            }
        }
    }
    params
}

/// What a definition says about itself: its description, its parameters, the
/// source it is saved in and the issues that make it unusable.
fn detail_lines(entry: &Entry, cwd: &Path) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(description) = &entry.description
        && !description.is_empty()
    {
        lines.push(description.clone());
    }
    for parameter in entry.parameters.iter().take(10) {
        lines.push(parameter_line(parameter));
    }
    match entry.source.as_str() {
        "user" => lines.push(t("Saved in the user workflow source; always enabled.").to_owned()),
        "project" => {
            lines.push(project_path(cwd, &entry.name).display().to_string());
            if !entry.enabled {
                lines.push(t("Starts only after you enable it.").to_owned());
            }
        }
        _ => lines.push(tf(
            "Defined by package {package}.",
            &[("package", entry.package_id.as_deref().unwrap_or(""))],
        )),
    }
    if !entry.valid {
        lines.extend(entry.issues.iter().cloned());
        if entry.issue_count > entry.issues.len() {
            lines.push(tf(
                "and {count} more",
                &[(
                    "count",
                    &(entry.issue_count - entry.issues.len()).to_string(),
                )],
            ));
        }
    }
    lines
}

/// Where a project definition is saved: the fixed project layout of
/// `docs/design/plugins/Workflow.md` §1.3.
pub fn project_path(cwd: &Path, name: &str) -> std::path::PathBuf {
    cwd.join(".dimcode")
        .join("workflows")
        .join(format!("{name}.yaml"))
}

/// One parameter as a line: its name, and what the definition says about it.
fn parameter_line(parameter: &Parameter) -> String {
    match (&parameter.default, parameter.description.as_str()) {
        (Some(default), _) => tf(
            "{name} — default: {default}",
            &[("name", &parameter.name), ("default", default)],
        ),
        (None, description) if !description.is_empty() && parameter.required => tf(
            "{name} — {description} (required)",
            &[("name", &parameter.name), ("description", description)],
        ),
        (None, description) if !description.is_empty() => tf(
            "{name} — {description}",
            &[("name", &parameter.name), ("description", description)],
        ),
        (None, _) if parameter.required => tf("{name} — required", &[("name", &parameter.name)]),
        (None, _) => parameter.name.clone(),
    }
}

/// The add document: the `workflow.save` request without the workspace, so
/// the destination is chosen in the same edit as the definition.
fn create() -> Value {
    json!({
        "source": "user",
        "document": {
            "schema": "dim.workflow/v1",
            "name": "example",
            "description": "Say what this workflow does.",
            "parameters": [
                {"name": "target", "description": "Path to review", "required": true}
            ],
            "steps": [
                {"stage": {"id": "review", "prompt": "Review {{params.target}} and list the problems."}}
            ]
        }
    })
}

#[cfg(test)]
mod tests {
    use super::sent_params;
    use serde_json::json;

    /// The rule's two cases: a value equal to a non-empty seed value is left
    /// out so the owner applies the definition's full default, while an
    /// untouched empty placeholder stays a supplied value.
    #[test]
    fn a_seeded_document_leaves_only_unchanged_non_empty_values_out() {
        let seed = json!({
            "target": "a long default the listing cut …",
            "style": "",
            "edited": "old",
        });
        let document = json!({
            "target": "a long default the listing cut …",
            "style": "",
            "edited": "new",
        });
        assert_eq!(
            sent_params(document, Some(&seed)),
            json!({"style": "", "edited": "new"})
        );
    }

    /// A document with no seed came from a file or stdin: it is sent as the
    /// caller wrote it, whatever the listing says.
    #[test]
    fn a_document_without_a_seed_is_sent_as_given() {
        let document = json!({"target": "a long default the listing cut …"});
        assert_eq!(sent_params(document.clone(), None), document);
    }
}
