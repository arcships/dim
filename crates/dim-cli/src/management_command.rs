//! `dimcode <area> ...`: a management area through its adapter, with no
//! Session (cli-commands.md "Management surfaces").
//!
//! The panel and these subcommands are two surfaces of the same adapter, so
//! an area's methods, view and refusals are written once. `hooks edit` and
//! `hooks set` are the edit kind at this surface: both write the document
//! with a revision read just before, and keep the user's text when the owner
//! refuses it.

use std::io::{IsTerminal, Read};
use std::path::Path;

use serde_json::Value;

use crate::Result;
use crate::args::{McpImport, Options};
use crate::client::Client;
use crate::management::{
    self, editor, hooks, mcp, models, permissions, scheduler, skills, workflow,
};
use crate::mcp_command::print_report;
use crate::model::session::{
    ActionKind, Area, ManagementAction, ManagementRequest, ManagementResult,
};
use crate::permission;
use crate::sanitize::{safe, safe_line};
use crate::ui::i18n::{t, tf};

/// Runs `request` and prints its area's report.
pub async fn run(
    client: &mut Client,
    options: &Options,
    request: &ManagementRequest,
) -> Result<i32> {
    match &request.area {
        Area::Skills => run_skills(client, options, request).await,
        Area::Hooks => run_hooks(client, options, request).await,
        Area::Mcp => run_mcp(client, options, request).await,
        Area::Scheduled => run_scheduled(client, request).await,
        Area::Models { provider_id } => run_models(client, provider_id, request).await,
        Area::Workflow => run_workflow(client, options, request).await,
        Area::Permissions => run_permissions(client, request).await,
    }
}

/// `<area> edit [ID]`: the area's document in the external editor, or one
/// entry's for an area whose documents are per entry.
pub async fn edit(
    client: &mut Client,
    options: &Options,
    area: Area,
    id: Option<String>,
) -> Result<i32> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        let set = if area.per_entry() {
            format!("{} set ID PATH", area.name())
        } else {
            format!("{} set PATH", area.name())
        };
        return Err(format!(
            "{} edit needs a terminal; {set} writes a file, or - its input",
            area.name()
        )
        .into());
    }
    // Refuse a write the daemon cannot serve before the user edits.
    management::supports_kind(client, &area, ActionKind::Edit)?;
    let (revision, document, source) = read(client, options, &area, id.as_deref()).await?;
    let text = serde_json::to_string_pretty(&document)
        .map(|text| format!("{text}\n"))
        .map_err(|error| error.to_string())?;
    let draft = editor::Draft::open(&options.home, area.clone(), &text)?;
    let kept = draft.path().display().to_string();
    let mut command = editor::command(draft.path());
    let program = command.get_program().to_string_lossy().into_owned();
    let status = match command.status() {
        Ok(status) => status,
        Err(error) => {
            // The editor never took the text: the draft is the stored document again.
            draft.remove();
            return Err(format!("{program}: {error}").into());
        }
    };
    if !status.success() {
        // The text the editor leaves is not the user's decision to write.
        return Err(format!(
            "{} Nothing was written. Your text is kept in {kept}.",
            editor::exited(status)
        )
        .into());
    }
    let text = draft
        .text()
        .map_err(|error| format!("{error} Your text is kept in {kept}."))?;
    let outcome = write(
        client,
        options,
        &Draft {
            area,
            id,
            source,
            text,
            revision,
            stored: document,
            kept,
        },
    )
    .await;
    if outcome.is_ok() {
        draft.remove();
    }
    outcome
}

/// `<area> set [ID] PATH`: the same write without an editor, from a file or
/// from stdin.
pub async fn set(
    client: &mut Client,
    options: &Options,
    area: Area,
    id: Option<String>,
    path: &Path,
) -> Result<i32> {
    // Refuse a write the daemon cannot serve before the input can hold the
    // call: the same check `edit` makes before it opens the editor.
    management::supports_kind(client, &area, ActionKind::Edit)?;
    // The input first and the revision second, so the revision is read
    // immediately before the write however long the input takes.
    let text = read_text(path)?;
    let (revision, stored, source) = read(client, options, &area, id.as_deref()).await?;
    if path == Path::new("-") {
        let draft = editor::Draft::open(&options.home, area.clone(), &text)?;
        let kept = draft.path().display().to_string();
        let outcome = write(
            client,
            options,
            &Draft {
                area,
                id,
                source,
                text,
                revision,
                stored,
                kept,
            },
        )
        .await;
        if outcome.is_ok() {
            draft.remove();
        }
        return outcome;
    }
    write(
        client,
        options,
        &Draft {
            area,
            id,
            source,
            text,
            revision,
            stored,
            kept: path.display().to_string(),
        },
    )
    .await
}

/// `<area> add PATH`: the non-interactive twin of the add kind, from a file
/// or from stdin.
pub async fn add(client: &mut Client, options: &Options, area: Area, path: &Path) -> Result<i32> {
    // Refuse a create the daemon cannot serve before the input can hold the
    // call, as `set` does.
    management::supports_kind(client, &area, ActionKind::Add)?;
    let text = read_text(path)?;
    if path == Path::new("-") {
        let draft = editor::Draft::open(&options.home, area.clone(), &text)?;
        let kept = draft.path().display().to_string();
        let outcome = create(client, options, area, &text, &kept).await;
        if outcome.is_ok() {
            draft.remove();
        }
        return outcome;
    }
    let kept = path.display().to_string();
    create(client, options, area, &text, &kept).await
}

/// `workflow start NAME [PATH]`: the definition's parameters from a file, or
/// from stdin for `-`, and no reporter, since the command line has no
/// Session.
pub async fn start(
    client: &mut Client,
    options: &Options,
    area: Area,
    id: String,
    path: Option<&Path>,
) -> Result<i32> {
    // Refuse a start the daemon cannot serve before the input can hold the
    // call, as `add` does.
    management::supports_kind(client, &area, ActionKind::Run)?;
    let document = match path {
        Some(path) => {
            let text = read_text(path)?;
            Some(
                serde_json::from_str::<Value>(&text)
                    .map_err(|error| format!("{}: {error}", path.display()))?,
            )
        }
        None => None,
    };
    let request = ManagementRequest {
        area,
        action: ManagementAction::Run {
            id,
            source: None,
            document,
            // A file or stdin document is the caller's own: it was never
            // opened in the panel's editor, so it has no seed.
            seed: None,
        },
    };
    run(client, options, &request).await
}

/// `mcp import PATH`, `mcp import --from ID` or `mcp import --discover`:
/// preview a standard `mcpServers` document, and with `--yes` apply the
/// entries the flags select. A path's text goes to the owner as it is; with
/// `--from`, the owner reads one known store of another local client and its
/// text never reaches this process. The preview's statuses, the apply's
/// refusals and the document's errors all come from there.
pub async fn import(client: &mut Client, options: &Options, import: &McpImport) -> Result<i32> {
    let McpImport {
        path,
        discover,
        from,
        yes,
        replace,
        rename,
        only,
    } = import;
    let cwd = options.workspace().as_str();
    if *discover {
        if !client.supports(&["mcp.import.sources"]) {
            return Err(t("This daemon cannot list other clients' MCP stores.")
                .to_owned()
                .into());
        }
        let sources = mcp::import_sources(client, cwd)
            .await
            .map_err(mcp::import_failure)?;
        print_import_sources(&sources);
        return Ok(0);
    }
    // Refuse a daemon that cannot serve the preview before the file is
    // opened, as `add` does, and one that cannot apply before `--yes` reads.
    if !client.supports(&["mcp.import.preview"])
        || (*yes && !client.supports(&["mcp.import.apply"]))
    {
        return Err(t("This daemon cannot import MCP servers.")
            .to_owned()
            .into());
    }
    let document = path
        .as_deref()
        .map(|path| read_text(Path::new(path)))
        .transpose()?;
    let input = match (document.as_deref(), from.as_deref()) {
        (Some(document), None) => mcp::ImportInput::Document(document),
        (None, Some(id)) => mcp::ImportInput::Store(id),
        _ => return Err("Use one of PATH, --from ID or --discover".into()),
    };
    let source = options.mcp_source.as_deref().unwrap_or("user");
    let preview = mcp::preview_import(client, cwd, source, input)
        .await
        .map_err(mcp::import_failure)?;
    print_import_preview(&preview, source, *yes);
    if !*yes {
        return Ok(0);
    }
    let resolutions = import_resolutions(&preview, replace, rename, only)?;
    let revision = mcp::apply_import(
        client,
        cwd,
        source,
        input,
        &preview.revision,
        preview.source_revision.clone(),
        &resolutions,
    )
    .await
    .map_err(mcp::import_failure)?;
    print_import_applied(&preview, &resolutions, &revision);
    Ok(0)
}

/// What `--discover` prints: one row per known store, then the columns and the
/// rule that no value is read out. A store that is not ready shows the owner's
/// message instead of a count.
fn print_import_sources(sources: &[mcp::ImportSource]) {
    let rows = sources
        .iter()
        .map(|source| {
            format!(
                "{}\t{}\t{}\t{}\t{}\t{}",
                safe_line(&source.id),
                safe_line(&source.client),
                safe_line(&source.scope),
                safe_line(&source.path),
                source.state.as_str(),
                safe_line(&source.detail()),
            )
        })
        .collect::<Vec<_>>();
    let notes = vec![
        t("Columns: ID, client, scope, path, state, entries or error.").to_owned(),
        t("No value from a store is read out or printed; --from ID previews one inside the daemon.")
            .to_owned(),
    ];
    print_report(rows, notes);
}

/// The resolutions `--yes` sends: each entry's default by status and the
/// flags that override it. A flag that names an id the document does not hold
/// is still sent, so the owner's own refusal names it.
fn import_resolutions(
    preview: &mcp::ImportPreview,
    replace: &[String],
    rename: &[(String, String)],
    only: &[String],
) -> Result<Vec<mcp::ImportResolution>> {
    use std::collections::{BTreeMap, BTreeSet};
    let replace: BTreeSet<&str> = replace.iter().map(String::as_str).collect();
    let renamed: BTreeMap<&str, &str> = rename
        .iter()
        .map(|(old, new)| (old.as_str(), new.as_str()))
        .collect();
    let only: BTreeSet<&str> = only.iter().map(String::as_str).collect();
    if let Some(old) = renamed.keys().copied().find(|old| replace.contains(*old)) {
        return Err(format!("--replace and --rename both name {old:?}; pick one.").into());
    }
    let mut resolutions: BTreeMap<String, mcp::ImportResolution> = BTreeMap::new();
    for entry in &preview.entries {
        let mut action = if entry.status.imports() {
            mcp::ImportAction::Import
        } else {
            mcp::ImportAction::Skip
        };
        if !only.is_empty() && !only.contains(entry.id.as_str()) {
            action = mcp::ImportAction::Skip;
        }
        if replace.contains(entry.id.as_str()) {
            action = mcp::ImportAction::Replace;
        }
        let new_id = renamed.get(entry.id.as_str()).map(|id| (*id).to_owned());
        if new_id.is_some() {
            action = mcp::ImportAction::Rename;
        }
        resolutions.insert(
            entry.id.clone(),
            mcp::ImportResolution {
                id: entry.id.clone(),
                action,
                new_id,
            },
        );
    }
    for id in replace {
        resolutions
            .entry(id.to_owned())
            .or_insert_with(|| mcp::ImportResolution {
                id: id.to_owned(),
                action: mcp::ImportAction::Replace,
                new_id: None,
            });
    }
    for (old, new) in &renamed {
        resolutions
            .entry((*old).to_owned())
            .or_insert_with(|| mcp::ImportResolution {
                id: (*old).to_owned(),
                action: mcp::ImportAction::Rename,
                new_id: Some((*new).to_owned()),
            });
    }
    for id in only {
        resolutions
            .entry(id.to_owned())
            .or_insert_with(|| mcp::ImportResolution {
                id: id.to_owned(),
                action: mcp::ImportAction::Skip,
                new_id: None,
            });
    }
    Ok(resolutions.into_values().collect())
}

/// What a preview prints: one row per entry, then the fields the owner's cap
/// left out, the top-level members the import would not carry and the
/// revision the source was read at. `--yes` prints the same rows before it
/// applies, so the user sees what is written.
fn print_import_preview(preview: &mcp::ImportPreview, source: &str, apply: bool) {
    let rows = preview
        .entries
        .iter()
        .map(|entry| {
            format!(
                "{:?}\t{}\t{}\t{}\t{}\t{}",
                entry.id,
                entry.status.as_str(),
                entry.transport.as_deref().unwrap_or("—"),
                import_fields(&entry.changes),
                import_unsupported(&entry.unsupported),
                entry
                    .error
                    .as_deref()
                    .map(safe_line)
                    .unwrap_or_else(|| "—".to_owned()),
            )
        })
        .collect::<Vec<_>>();
    let mut notes = Vec::new();
    if let Some(store) = &preview.store {
        notes.push(format!(
            "{} · {} · {} · {}",
            store.id,
            store.client,
            store.scope,
            safe_line(&store.path),
        ));
    }
    if preview.entries.is_empty() {
        notes.push(t("The document holds no mcpServers entries.").to_owned());
    } else {
        notes.push(t(
            "Columns: ID, status, transport, changed fields, unsupported fields, error.\n\
             A changed field names what a write would change and the kind of value it holds; no value is printed.\n\
             An unsupported field is kept unused or dropped, as shown after the field.",
        )
        .to_owned());
        if source == "project" {
            notes.push(t(
                "A project entry is written as its public skeleton: the workspace file gets the command, declared cwd, URL origin and public OAuth fields. Its arguments, environment and header values, bearer token, a URL with userinfo, path, query or fragment, and unknown members go to your user-scope mcp-secrets record, never to the workspace file.",
            )
            .to_owned());
        }
    }
    for entry in &preview.entries {
        if entry.omitted > 0 {
            notes.push(tf(
                "{id} has {count} more field names than the lists above show.",
                &[
                    ("id", &format!("{:?}", entry.id)),
                    ("count", &entry.omitted.to_string()),
                ],
            ));
        }
    }
    notes.push(match preview.unknown_top_level.as_slice() {
        [] => t("Top-level members the import does not carry: none.").to_owned(),
        names => tf(
            "Top-level members the import does not carry: {names}.",
            &[("names", &import_names(names))],
        ),
    });
    notes.push(format!(
        "The {source} source was read at revision {}.",
        safe_line(&preview.revision)
    ));
    if apply {
        notes.push(t("Applying the selection at this revision.").to_owned());
    }
    print_report(rows, notes);
}

/// What an applied import prints: each entry's action and destination, the
/// counts, and the state imported entries start in.
fn print_import_applied(
    preview: &mcp::ImportPreview,
    resolutions: &[mcp::ImportResolution],
    revision: &str,
) {
    use std::collections::BTreeMap;
    let resolutions: BTreeMap<&str, &mcp::ImportResolution> = resolutions
        .iter()
        .map(|resolution| (resolution.id.as_str(), resolution))
        .collect();
    let (mut imported, mut replaced, mut renamed, mut skipped, mut unchanged) = (0, 0, 0, 0, 0);
    let mut rows = Vec::new();
    for entry in &preview.entries {
        let Some(resolution) = resolutions.get(entry.id.as_str()) else {
            continue;
        };
        let (action, destination) = match resolution.action {
            mcp::ImportAction::Skip => {
                skipped += 1;
                ("skipped", "—".to_owned())
            }
            mcp::ImportAction::Import if entry.status == mcp::ImportStatus::Identical => {
                unchanged += 1;
                ("unchanged", entry.id.clone())
            }
            mcp::ImportAction::Import => {
                imported += 1;
                ("imported", entry.id.clone())
            }
            mcp::ImportAction::Replace => {
                replaced += 1;
                ("replaced", entry.id.clone())
            }
            mcp::ImportAction::Rename => {
                renamed += 1;
                (
                    "renamed",
                    resolution.new_id.clone().unwrap_or_else(|| "—".to_owned()),
                )
            }
        };
        rows.push(format!(
            "{:?}\t{action}\t{}",
            entry.id,
            safe_line(&destination)
        ));
    }
    let mut notes = vec![
        t(
            "Columns: ID, action, destination ID.\n\
             A renamed entry is written under its destination id; the source's entry with the old id is left as it was.",
        )
        .to_owned(),
        format!(
            "Imported {imported}, replaced {replaced}, renamed {renamed}, skipped {skipped}, unchanged {unchanged}."
        ),
        t(
            "Imported and replaced entries start disabled: a user entry is written with enabled: false, and a project entry has no enablement record until you enable it locally.",
        )
        .to_owned(),
    ];
    if unchanged > 0 {
        notes.push(
            t("An unchanged entry was already in the source and was not written.").to_owned(),
        );
    }
    notes.push(format!(
        "The source is now at revision {}.",
        safe_line(revision)
    ));
    print_report(rows, notes);
}

/// Changed fields as `field:kind` pairs.
fn import_fields(fields: &[mcp::ImportField]) -> String {
    if fields.is_empty() {
        return "—".to_owned();
    }
    fields
        .iter()
        .map(|field| format!("{}:{}", safe_line(&field.field), field.kind))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Unsupported members as `field:effect` pairs.
fn import_unsupported(fields: &[mcp::ImportUnsupported]) -> String {
    if fields.is_empty() {
        return "—".to_owned();
    }
    fields
        .iter()
        .map(|field| format!("{}:{}", safe_line(&field.field), field.effect.as_str()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Document names, each kept on one line.
fn import_names(names: &[String]) -> String {
    if names.is_empty() {
        return "—".to_owned();
    }
    names
        .iter()
        .map(|name| safe_line(name))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The document a command reads, from a file or from stdin for `-`.
fn read_text(path: &Path) -> Result<String> {
    if path == Path::new("-") {
        let mut text = String::new();
        std::io::stdin().read_to_string(&mut text)?;
        return Ok(text);
    }
    std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()).into())
}

/// The document and the revision to write it at, read immediately before,
/// with the source an MCP entry's row reports so the write names it.
async fn read(
    client: &mut Client,
    options: &Options,
    area: &Area,
    id: Option<&str>,
) -> Result<(String, Value, Option<String>)> {
    management::supported(
        client,
        &ManagementRequest {
            area: area.clone(),
            action: ManagementAction::List,
        },
    )?;
    match area {
        Area::Skills => Err("A Skill has no document to edit.".to_owned().into()),
        Area::Models { provider_id } => {
            let Some(id) = id else {
                return Err("models edit and set take a model id.".to_owned().into());
            };
            let provider = models::read(client, provider_id)
                .await
                .map_err(|failure| failure.to_string())?;
            let version = provider
                .version
                .ok_or("The provider has no model catalog to edit.")?;
            let model = provider
                .model(id)
                .ok_or_else(|| format!("No model has that id: {id}"))?;
            Ok((
                version.to_string(),
                models::edit_template(&provider, model),
                None,
            ))
        }
        Area::Hooks => {
            let catalog = hooks::catalog(client, options.workspace().as_str()).await?;
            let document = catalog
                .user_document
                .ok_or("The daemon reports no hook document to edit.")?;
            Ok((catalog.user_revision, document, None))
        }
        Area::Mcp => {
            let Some(id) = id else {
                return Err("mcp edit and set take an entry id.".to_owned().into());
            };
            let server = mcp::target(
                client,
                options.workspace().as_str(),
                id,
                options.mcp_source.as_deref(),
            )
            .await?;
            if server.source == "plugin" {
                return Err("plugin MCP entries change through their plugin".into());
            }
            let source = server.source.clone();
            let editable = mcp::editable(client, options.workspace().as_str(), &server).await?;
            // A plugin entry's read reports no revision; the refusal above
            // already covers it, and any other missing revision leaves
            // nothing to write.
            let revision = editable
                .revision
                .ok_or("The daemon reports no revision for this MCP entry to edit.")?;
            let document = mcp::seed(&editable.configuration)?;
            Ok((revision, document, Some(source)))
        }
        Area::Scheduled => {
            let Some(id) = id else {
                return Err("scheduled edit and set take a job id.".to_owned().into());
            };
            let catalog = scheduler::catalog(client).await?;
            let job = catalog
                .jobs
                .iter()
                .find(|job| job.id == id)
                .ok_or_else(|| format!("No scheduled job has that id: {id}"))?;
            Ok((job.revision.to_string(), job.document(), None))
        }
        Area::Workflow => {
            let Some(id) = id else {
                return Err("workflow edit and set take a definition name."
                    .to_owned()
                    .into());
            };
            let listing = workflow::read(client, options.workspace().as_str()).await?;
            let entry = listing
                .find(id)
                .ok_or_else(|| format!("No workflow definition is named {id}."))?;
            match entry.source.as_str() {
                "project" => {
                    return Err(format!(
                        "A project definition is a file; edit {}.",
                        workflow::project_path(options.workspace().path(), &entry.name).display()
                    )
                    .into());
                }
                "user" => {}
                _ => {
                    return Err("A package definition is installed by its package."
                        .to_owned()
                        .into());
                }
            }
            let (revision, document) = listing
                .document(entry)
                .ok_or("The user workflow source has no such definition.")?;
            Ok((revision.to_owned(), document.clone(), None))
        }
        Area::Permissions => Err("A command rule has no document to edit.".to_owned().into()),
    }
}

/// One document a command writes: where it goes, what it says, and where
/// the user's text stays when the owner refuses it.
struct Draft {
    area: Area,
    id: Option<String>,
    /// The source an MCP entry's row reports, which its save names.
    source: Option<String>,
    text: String,
    /// The revision the document was read at.
    revision: String,
    /// The document as the owner reported it, which `text` must differ from.
    stored: Value,
    /// The file that keeps the user's text when nothing is written.
    kept: String,
}

/// Writes a draft: nothing when it says what is stored, and a refusal names
/// `kept`, where the user's text stays.
async fn write(client: &mut Client, options: &Options, draft: &Draft) -> Result<i32> {
    let document: Value = serde_json::from_str(&draft.text)
        .map_err(|error| format!("{error} Your text is kept in {}.", draft.kept))?;
    if document == draft.stored {
        print_report(vec!["No change.".to_owned()], Vec::new());
        return Ok(0);
    }
    let request = ManagementRequest {
        area: draft.area.clone(),
        action: ManagementAction::Save {
            id: draft.id.clone(),
            source: draft.source.clone(),
            expected_revision: draft.revision.clone(),
            document,
        },
    };
    match management::apply(client, &request, options.named_workspace(), None)
        .await
        .result
    {
        ManagementResult::Refused(error) => {
            Err(format!("{error} Your text is kept in {}.", draft.kept).into())
        }
        // The document was replaced; only the report after it failed, so the
        // command did what it was asked and says why it cannot show the area.
        // The re-read never happened, so the entry goes by the id it was
        // sent with.
        ManagementResult::Applied { read_error, .. } => {
            let line = management::applied_line(&request.action, None);
            print_report(vec![safe_line(&line)], vec![read_error]);
            Ok(0)
        }
        // A save reports the area after it, whose read names the entry.
        ManagementResult::View { view, .. } => {
            let name = management::entry_name(&view, &request.action);
            let line = management::applied_line(&request.action, name);
            print_report(vec![safe_line(&line)], Vec::new());
            Ok(0)
        }
        // A detail belongs to an inspect, which applies nothing.
        ManagementResult::Detail(_) => {
            let line = management::applied_line(&request.action, None);
            print_report(vec![safe_line(&line)], Vec::new());
            Ok(0)
        }
    }
}

/// What a create writes, with the message the owner's answer gives.
async fn create(
    client: &mut Client,
    options: &Options,
    area: Area,
    text: &str,
    kept: &str,
) -> Result<i32> {
    let document: Value = serde_json::from_str(text)
        .map_err(|error| format!("{error} Your text is kept in {kept}."))?;
    let request = ManagementRequest {
        area,
        action: ManagementAction::Add { document },
    };
    match management::apply(client, &request, options.named_workspace(), None)
        .await
        .result
    {
        ManagementResult::View { note, .. } => {
            // The note names the entry the owner saved: made safe, as every
            // line the Scheduler prints is.
            let line = note.map_or_else(
                || management::applied_line(&request.action, None),
                |note| safe_line(&note),
            );
            print_report(vec![line], Vec::new());
            Ok(0)
        }
        // The entry was created; only the read after it failed. The action's
        // own line still stands, made safe, and the exit stays 0.
        ManagementResult::Applied { note, read_error } => {
            let line = note.unwrap_or_else(|| management::applied_line(&request.action, None));
            print_report(vec![safe_line(&line)], vec![read_error]);
            Ok(0)
        }
        // An add reports the entry it created; a detail belongs to an inspect.
        ManagementResult::Detail(detail) => {
            let lines = detail.lines.iter().map(|line| safe(line)).collect();
            print_report(lines, Vec::new());
            Ok(0)
        }
        ManagementResult::Refused(error) => {
            Err(format!("{error} Your text is kept in {kept}.").into())
        }
    }
}

async fn run_skills(
    client: &mut Client,
    options: &Options,
    request: &ManagementRequest,
) -> Result<i32> {
    management::supported(client, request)?;
    let catalog = match skills::apply(client, &request.action, options.workspace().as_str()).await {
        skills::Outcome::Catalog(catalog) => catalog,
        // A toggle or a rescan was applied, so the command did what it was
        // asked; only the report after it failed, and the action's own line
        // still stands.
        skills::Outcome::ReadFailed(error) => {
            let line = management::applied_line(&request.action, None);
            print_report(vec![safe_line(&line)], vec![error]);
            return Ok(0);
        }
        skills::Outcome::Refused(error) => return Err(error.into()),
    };
    match &request.action {
        ManagementAction::Toggle { id, .. } => {
            let name = catalog
                .entries
                .iter()
                .find(|entry| &entry.id == id)
                .map(|entry| entry.name.as_str());
            let line = management::applied_line(&request.action, name);
            print_report(vec![safe_line(&line)], Vec::new());
        }
        _ => print_report(rows(&catalog), notes(&catalog)),
    }
    Ok(0)
}

async fn run_hooks(
    client: &mut Client,
    options: &Options,
    request: &ManagementRequest,
) -> Result<i32> {
    management::supported(client, request)?;
    let catalog = match hooks::apply(client, &request.action, options.workspace().as_str()).await {
        Ok(catalog) => catalog,
        // A toggle was applied; only the report after it failed. Its line
        // names the workspace, not anything the missing read would carry.
        Err(hooks::Failure::ReadFailed(error)) => {
            let ManagementAction::Toggle { enabled, .. } = &request.action else {
                return Err(error.into());
            };
            let cwd = options.workspace().as_str();
            print_report(
                vec![format!(
                    "{} project hooks for {}.",
                    if *enabled { "Enabled" } else { "Disabled" },
                    safe_line(cwd)
                )],
                vec![toggle_note(*enabled).to_owned(), error],
            );
            return Ok(0);
        }
        Err(error) => return Err(error.to_string().into()),
    };
    match &request.action {
        ManagementAction::Toggle { enabled, .. } => {
            let cwd = options.workspace().as_str();
            print_report(
                vec![format!(
                    "{} project hooks for {}.",
                    if *enabled { "Enabled" } else { "Disabled" },
                    safe_line(cwd)
                )],
                vec![toggle_note(*enabled).to_owned()],
            );
        }
        _ => print_report(hook_rows(&catalog), hook_notes(&catalog)),
    }
    Ok(0)
}

/// `scheduled ...`: the jobs the Scheduler holds, printed as its owner
/// reports them.
async fn run_scheduled(client: &mut Client, request: &ManagementRequest) -> Result<i32> {
    management::supported(client, request)?;
    match scheduler::apply(client, &request.action).await {
        Ok(scheduler::Answer::Detail(detail)) => {
            // The owner names jobs and states reasons: what it wrote is
            // made safe before it reaches the terminal.
            let lines = detail.lines.iter().map(|line| safe(line)).collect();
            print_report(lines, Vec::new())
        }
        Ok(scheduler::Answer::Jobs { catalog, note }) => match (&request.action, note) {
            // The owner's own answer: the entry a create made, or a run's
            // state.
            (_, Some(note)) => print_report(vec![safe_line(&note)], Vec::new()),
            // A read reports what it read; an action reports its line.
            (ManagementAction::List | ManagementAction::Reload, None) => {
                print_report(job_rows(&catalog), job_notes(&catalog))
            }
            (_, None) => {
                // The jobs the adapter just re-read name the entry, unless a
                // remove took it out of the read.
                let name = management::named_id(&request.action).and_then(|id| {
                    catalog
                        .jobs
                        .iter()
                        .find(|job| job.id == id)
                        .map(|job| job.name.as_str())
                });
                let line = management::applied_line(&request.action, name);
                print_report(vec![safe_line(&line)], Vec::new())
            }
        },
        // The action was applied; the report after it failed. The action's own
        // line stands, made safe, and the read error goes to stderr with the
        // exit still 0.
        Err(scheduler::Failure::ReadFailed { note, error }) => {
            // The read that would name the entry failed; what the user typed
            // names it, except where the owner's own answer stands.
            let line = note.unwrap_or_else(|| management::applied_line(&request.action, None));
            print_report(vec![safe_line(&line)], vec![error])
        }
        Err(error) => return Err(error.to_string().into()),
    }
    Ok(0)
}

/// `models ...`: one Provider's models, printed as its owner answers.
async fn run_models(
    client: &mut Client,
    provider_id: &str,
    request: &ManagementRequest,
) -> Result<i32> {
    management::supported(client, request)?;
    match models::apply(client, provider_id, &request.action).await {
        Ok(models::Answer::Detail(detail)) => {
            // The owner names models and states reasons: what it wrote is
            // made safe before it reaches the terminal.
            let lines = detail.lines.iter().map(|line| safe(line)).collect();
            print_report(lines, Vec::new());
        }
        Ok(models::Answer::Provider { provider, note }) => {
            // The re-read names the entry: its label from the fresh view,
            // with the id only when that view has no such entry.
            let line = note.unwrap_or_else(|| {
                let actions = management::actions(client, &request.area);
                let view = models::view(&provider, &actions);
                let name = management::entry_name(&view, &request.action);
                management::applied_line(&request.action, name)
            });
            print_report(vec![safe_line(&line)], Vec::new());
        }
        // The action was applied; only the report after it failed. Its line
        // names what the user asked for, and the exit stays 0.
        Err(models::Failure::ReadFailed { note, error }) => {
            let line = note.unwrap_or_else(|| management::applied_line(&request.action, None));
            print_report(vec![safe_line(&line)], vec![error]);
        }
        Err(error) => return Err(error.to_string().into()),
    }
    Ok(0)
}

/// `workflow ...`: the definitions as its owner reports them, and the kinds
/// this twin performs.
async fn run_workflow(
    client: &mut Client,
    options: &Options,
    request: &ManagementRequest,
) -> Result<i32> {
    management::supported(client, request)?;
    match workflow::apply(client, &request.action, options.workspace().as_str(), None).await {
        Ok(workflow::Answer::Detail(detail)) => {
            // The owner names definitions and states reasons: what it wrote
            // is made safe before it reaches the terminal.
            let lines = detail.lines.iter().map(|line| safe(line)).collect();
            print_report(lines, Vec::new())
        }
        Ok(workflow::Answer::Definitions { listing, note }) => match (&request.action, note) {
            // The owner's own answer: the definition a create made, or a
            // run's start.
            (_, Some(note)) => print_report(vec![safe_line(&note)], Vec::new()),
            // A read reports what it read; an action reports its line.
            (ManagementAction::List | ManagementAction::Reload, None) => {
                print_report(workflow_rows(&listing), workflow_notes(&listing))
            }
            (_, None) => {
                // The definitions the adapter just re-read name the entry.
                let name = management::named_id(&request.action)
                    .and_then(|id| listing.find(id))
                    .map(|entry| entry.name.as_str());
                let line = management::applied_line(&request.action, name);
                print_report(vec![safe_line(&line)], Vec::new())
            }
        },
        // The action was applied; only the report after it failed. The
        // action's own line stands, made safe, and the read error goes to
        // stderr with the exit still 0.
        Err(workflow::Failure::ReadFailed { note, error }) => {
            // The read that would name the entry failed; what the user typed
            // names it, except where the owner's own answer stands.
            let line = note.unwrap_or_else(|| management::applied_line(&request.action, None));
            print_report(vec![safe_line(&line)], vec![error])
        }
        Err(error) => return Err(error.to_string().into()),
    }
    Ok(0)
}

/// `mcp ...`: the entries the catalog reports, or one entry's detail and
/// what the owner answered.
async fn run_mcp(
    client: &mut Client,
    options: &Options,
    request: &ManagementRequest,
) -> Result<i32> {
    management::supported(client, request)?;
    // A read reports the area exactly as `mcp list` does.
    if matches!(
        request.action,
        ManagementAction::List | ManagementAction::Reload
    ) {
        let (rows, notes) =
            crate::mcp::catalog_report(client, options.workspace().as_str()).await?;
        print_report(rows, notes);
        return Ok(0);
    }
    match mcp::apply_named(
        client,
        &request.action,
        options.workspace().as_str(),
        options.mcp_source.as_deref(),
    )
    .await
    {
        // A test's tools are typed: one row per tool, `name\tdescription`,
        // and the no-tools sentence when the owner found none.
        Ok(mcp::Answer::Tools { tools, .. }) => {
            let (lines, notes) = if tools.is_empty() {
                (
                    Vec::new(),
                    vec![
                        crate::ui::i18n::t("The server exposes no tools.").to_owned(),
                        "The connection was temporary; no configuration was saved.".to_owned(),
                    ],
                )
            } else {
                (
                    tools.iter().map(mcp::tool_row).collect(),
                    vec![
                        "Columns: tool, description.".to_owned(),
                        "The connection was temporary; no configuration was saved.".to_owned(),
                    ],
                )
            };
            print_report(lines, notes)
        }
        // An inspect's detail is printed as the adapter built it.
        Ok(mcp::Answer::Detail(detail)) => {
            let lines = detail.lines.iter().map(|line| safe(line)).collect();
            print_report(
                lines,
                vec!["Inspection does not connect or contact the server.".to_owned()],
            )
        }
        Ok(mcp::Answer::Area { note, catalog }) => match (&request.action, note) {
            // The owner's own answer, made safe: the entry a create made.
            (_, Some(note)) => print_report(vec![safe_line(&note)], Vec::new()),
            // A read reports what it read; an action reports its line.
            (ManagementAction::List | ManagementAction::Reload, None) => {
                print_report(Vec::new(), Vec::new())
            }
            (_, None) => {
                // The catalog the adapter just re-read names the entry, as
                // the panel's view does; a remove took its entry out of the
                // read, so the id the action was sent with names it then.
                let name = management::named_id(&request.action).and_then(|id| {
                    catalog
                        .servers
                        .iter()
                        .find(|server| server.id == id)
                        .map(|server| server.id.as_str())
                });
                let line = management::applied_line(&request.action, name);
                print_report(vec![safe_line(&line)], Vec::new())
            }
        },
        // The action was applied; the report after it failed. The action's own
        // line stands, made safe, and the read error goes to stderr with the
        // exit still 0.
        Err(mcp::Failure::ReadFailed { note, error }) => {
            let line = note.unwrap_or_else(|| management::applied_line(&request.action, None));
            print_report(vec![safe_line(&line)], vec![error])
        }
        Err(error) => return Err(error.to_string().into()),
    }
    Ok(0)
}

/// One definition: its name, source, state and description.
fn workflow_rows(listing: &workflow::Listing) -> Vec<String> {
    listing
        .entries
        .iter()
        .map(|entry| {
            let state = match (entry.enabled, entry.valid) {
                (true, true) => "enabled",
                (false, true) => "disabled",
                (true, false) => "invalid",
                (false, false) => "disabled, invalid",
            };
            format!(
                "{}\t{}\t{}\t{}",
                safe(&entry.name),
                safe(&entry.source),
                state,
                safe(entry.description.as_deref().unwrap_or("—")),
            )
        })
        .collect()
}

/// `permissions ...`: the user's command rules, printed as its owner reports
/// them.
async fn run_permissions(client: &mut Client, request: &ManagementRequest) -> Result<i32> {
    management::supported(client, request)?;
    match permissions::apply(client, &request.action).await {
        Ok(permissions::Answer::Rules { rules, note }) => {
            match note {
                Some(note) => print_report(vec![safe_line(&note)], Vec::new()),
                None => print_report(rule_rows(&rules), rule_notes(&rules)),
            }
            Ok(0)
        }
        // A refused write and a conflict both wrote nothing; the sentence the
        // adapter reported is the whole answer.
        Err(permissions::Failure::Refused(message)) => Err(message.into()),
        Err(permissions::Failure::Conflict { message, .. }) => Err(message.into()),
    }
}

/// One rule: its decision, match and command. The command is one column, so
/// the owner's text is kept on one line before it joins the row.
fn rule_rows(rules: &permission::Rules) -> Vec<String> {
    rules
        .command_rules
        .iter()
        .map(|rule| {
            format!(
                "{}\t{}\t{}",
                permission::decision_name(rule.decision),
                permission::match_name(rule.matching),
                safe_line(&rule.command),
            )
        })
        .collect()
}

fn workflow_notes(listing: &workflow::Listing) -> Vec<String> {
    let mut notes = if listing.entries.is_empty() {
        vec!["No workflow definition is configured.".to_owned()]
    } else {
        vec!["Columns: name, source, state, description.".to_owned()]
    };
    if listing.truncated {
        notes.push(
            t("The list reached its size limit; later definitions are not shown.").to_owned(),
        );
    }
    for entry in listing.entries.iter().filter(|entry| !entry.valid) {
        for issue in &entry.issues {
            notes.push(format!("{}: {}", safe(&entry.name), safe(issue)));
        }
    }
    notes
}

fn rule_notes(rules: &permission::Rules) -> Vec<String> {
    if rules.command_rules.is_empty() {
        vec!["No command rule is configured.".to_owned()]
    } else {
        vec![
            "Columns: decision, match, command. `permissions remove` takes a whole rule as one quoted argument.".to_owned(),
            "Rules apply in every Session; deny wins over allow.".to_owned(),
        ]
    }
}

/// One job: its id, name, schedule, state, next window, last outcome and
/// where it delivers.
fn job_rows(catalog: &scheduler::Catalog) -> Vec<String> {
    catalog
        .jobs
        .iter()
        .map(|job| {
            let next = job.next_window.as_deref().unwrap_or("—");
            let last = job.last_run.as_ref().map_or_else(
                || "—".to_owned(),
                |run| format!("{} at {}", run.outcome(), run.instant()),
            );
            let target = job
                .target
                .as_ref()
                .map_or_else(|| "—".to_owned(), scheduler::Target::column);
            format!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                safe_line(&job.id),
                safe_line(&job.name),
                safe_line(&scheduler::schedule_text(&job.schedule)),
                if job.enabled { "enabled" } else { "disabled" },
                safe_line(next),
                safe_line(&last),
                safe_line(&target),
            )
        })
        .collect()
}

fn job_notes(catalog: &scheduler::Catalog) -> Vec<String> {
    let mut notes = if catalog.jobs.is_empty() {
        vec!["No job is scheduled.".to_owned()]
    } else {
        vec!["Columns: id, name, schedule, state, next run, last run, target.".to_owned()]
    };
    for job in &catalog.jobs {
        if let Some(error) = &job.schedule_error {
            notes.push(format!("{}: {}", safe_line(&job.name), safe_line(error)));
        }
    }
    notes
}

/// What turning this workspace's project hooks on or off does to the file.
fn toggle_note(enabled: bool) -> &'static str {
    if enabled {
        "It runs this workspace's .dimcode/hooks.json, including later edits to it, in active and new sessions."
    } else {
        "The file is kept; it runs again after hooks enable."
    }
}

/// One Skill: its name, source, state, file and description.
fn rows(catalog: &skills::Catalog) -> Vec<String> {
    catalog
        .entries
        .iter()
        .map(|entry| {
            let mut state = if entry.enabled { "enabled" } else { "disabled" }.to_owned();
            if let Some(by) = &entry.shadowed_by {
                state.push_str(&format!(", shadowed by {}", safe_line(by)));
            }
            format!(
                "{}\t{}\t{}\t{}\t{}",
                safe_line(&entry.name),
                safe_line(entry.source.as_deref().unwrap_or("-")),
                state,
                safe_line(&entry.file_path),
                safe_line(&entry.description),
            )
        })
        .collect()
}

fn notes(catalog: &skills::Catalog) -> Vec<String> {
    let mut notes = if catalog.entries.is_empty() {
        vec!["No Skill is configured.".to_owned()]
    } else {
        vec!["Columns: name, source, state, file, description.".to_owned()]
    };
    notes.extend(
        catalog
            .issues
            .iter()
            .map(|issue| format!("{}: {}", safe_line(&issue.path), safe_line(&issue.message))),
    );
    notes
}

/// One handler: its event, matcher, source, enablement, state and settings.
fn hook_rows(catalog: &hooks::Catalog) -> Vec<String> {
    catalog
        .hooks
        .iter()
        .map(|hook| {
            let enabled = catalog
                .sources
                .iter()
                .find(|source| source.source == hook.source)
                .is_some_and(|source| source.enabled);
            format!(
                "{}\t{}\t{}\t{}\t{}\t{}s\t{}\t{}\t{}",
                safe_line(&hook.event),
                safe_line(hook.matcher.as_deref().unwrap_or("*")),
                safe_line(&hook.source),
                if enabled { "enabled" } else { "disabled" },
                safe_line(&hook.state),
                hook.timeout,
                safe_line(&hook.on_failure),
                if hook.r#async { "async" } else { "sync" },
                safe_line(hook.command.as_deref().unwrap_or("—")),
            )
        })
        .collect()
}

fn hook_notes(catalog: &hooks::Catalog) -> Vec<String> {
    // The columns describe rows: with no handler, the report says so instead.
    let mut notes = if catalog.hooks.is_empty() {
        vec!["No hook is configured.".to_owned()]
    } else {
        vec![
            "Columns: event, matcher, source, enablement, state, timeout, on failure, async, command.\nA handler whose state is ok runs at its event once its source is enabled; unsupported and invalid handlers never run.\nProject hooks stay disabled until you run hooks enable.".to_owned(),
        ]
    };
    for source in &catalog.sources {
        let name = match source.path.as_deref() {
            Some(path) => format!("{} hooks ({})", safe_line(&source.source), safe_line(path)),
            None => format!("{} hooks", safe_line(&source.source)),
        };
        if let Some(line) = hooks::state_line(source) {
            notes.push(format!("{name}: {}", safe_line(&line)));
        } else if !source.enabled {
            notes.push(format!("{name}: disabled"));
        }
    }
    if catalog.disable_all_hooks {
        notes.push("disableAllHooks in the user document turns every hook off.".to_owned());
    }
    notes
}
