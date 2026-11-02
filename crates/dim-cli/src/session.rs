use crate::wire::{Event, Progress};
use crate::{
    Result,
    args::{Action, Options},
    client::{Client, Message, RequestTooLarge},
    composer::{self, Draft},
    configure,
    display::Display,
    export, goal_command,
    input::Input,
    mcp, permission, permission_command,
    question::line::Questions,
};
use dim_agent_types::{ApprovalCallData, ContentBlock, ModelId, PermissionPolicy, UserInput};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    future::{Future, pending},
    io::Write,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Created {
    session_id: String,
    model: ModelId,
}

enum Submission {
    Message(UserInput),
    Shell(String),
}

pub use dim_client::wire::History;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EventNotice {
    session_id: String,
    branch_id: String,
    event: Event,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Approval {
    session_id: String,
    branch_id: String,
    request_id: String,
    call: ApprovalCallData,
    reason: String,
    /// Set after a failed rule write: the user can still answer once.
    #[serde(skip)]
    once: bool,
    #[serde(skip)]
    member: Option<
        std::result::Result<
            dim_agent_types::MemberApprovalDescriptor,
            crate::member_approval::DetailError,
        >,
    >,
}

/// The rule an "always" answer adds before it answers allow (F2 §5.4).
struct RuleWrite {
    request_id: String,
    add: permission::AddRule,
    /// The outstanding request of `add`.
    call: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApprovalCancelled {
    session_id: String,
    branch_id: String,
    request_id: String,
}

struct ShellRun {
    request: Option<u64>,
    run_id: String,
    stop: Option<u64>,
    stopping: bool,
    progress_started: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ShellResult {
    run_id: String,
    status: String,
    exit_code: Option<i32>,
    signal: Option<String>,
    reason: Option<String>,
    output: String,
    truncated: bool,
    recorded: Option<Recorded>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Recorded {
    Written { sequence: u64 },
    Failed { error: RecordingError },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecordingError {
    app_error: Option<String>,
    message: String,
}

async fn start_shell(
    client: &mut Client,
    session_id: &str,
    text: &str,
    display: &mut Display,
) -> Result<Option<ShellRun>> {
    let (mode, command) = match text.strip_prefix("!!") {
        Some(command) => ("private", command),
        None => ("record", text.strip_prefix('!').expect("shell prefix")),
    };
    if command.trim().is_empty() {
        display.record("Usage: !command (record output) or !!command (private output)")?;
        return Ok(None);
    }
    let run_id = uuid::Uuid::new_v4().to_string();
    let request = client.send("shell.run", json!({"sessionId":session_id,"branchId":"main","runId":run_id,"command":command,"mode":mode})).await?;
    display.record(&format!("[shell started] {mode}"))?;
    Ok(Some(ShellRun {
        request: Some(request),
        run_id,
        stop: None,
        stopping: false,
        progress_started: false,
    }))
}

fn shell_result(value: Value, run_id: &str, display: &mut Display) -> Result<()> {
    let result: ShellResult =
        serde_json::from_value(value).map_err(|_| "Invalid shell.run response")?;
    if result.run_id != run_id {
        return Err("Unexpected shell.run identity".into());
    }
    display.record(&format!("[shell output] (final)\n{}", result.output))?;
    let mut status = format!("[shell {}]", result.status);
    if let Some(code) = result.exit_code {
        status.push_str(&format!(" exit: {code}"));
    }
    if let Some(signal) = result.signal {
        status.push_str(&format!(" signal: {signal}"));
    }
    if let Some(reason) = result.reason {
        status.push_str(&format!(" {reason}"));
    }
    if result.truncated {
        status.push_str(" (output truncated)");
    }
    display.record(&status)?;
    match result.recorded {
        Some(Recorded::Written { sequence }) => {
            display.record(&format!("[shell recorded] sequence: {sequence}"))?;
        }
        Some(Recorded::Failed { error }) => {
            display.record(&format!(
                "[shell recording failed] {}{}",
                error.message,
                error
                    .app_error
                    .map(|code| format!(" ({code})"))
                    .unwrap_or_default()
            ))?;
        }
        None => {}
    }
    Ok(())
}

fn decode<T: serde::de::DeserializeOwned>(value: Value, method: &str) -> Result<T> {
    serde_json::from_value(value).map_err(|_| format!("Invalid {method} notification").into())
}

/// Ctrl+C at the line-mode prompt.
#[cfg(unix)]
pub(crate) type Interrupts = tokio::signal::unix::Signal;
#[cfg(windows)]
pub(crate) type Interrupts = tokio::signal::windows::CtrlC;

/// Waits for a line command's operation; interrupts are reported and
/// consumed until it settles, so the prompt returns only after the daemon
/// answered.
pub(crate) async fn settle<T>(
    interrupts: &mut Interrupts,
    notice: &str,
    operation: impl Future<Output = T>,
) -> T {
    tokio::pin!(operation);
    loop {
        tokio::select! {
            biased;
            _ = interrupts.recv() => println!("\n{notice}"),
            result = &mut operation => break result,
        }
    }
}

pub async fn chat(
    client: &mut Client,
    options: &Options,
    images: Vec<ContentBlock>,
    notes: Vec<String>,
) -> Result<i32> {
    #[cfg(unix)]
    let mut interrupts = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    #[cfg(windows)]
    let mut interrupts = tokio::signal::windows::ctrl_c()?;
    let interactive = options.prompt.is_none() && !matches!(options.action, Action::Exec);
    let mut input = if interactive {
        Some(Input::new()?)
    } else {
        None
    };
    let mut display = Display::new(interactive, options.json);
    for note in &notes {
        display.record(note)?;
    }
    let session_id = if let Some(id) = &options.resume {
        id.clone()
    } else {
        let mut params = json!({"cwd":options.cwd});
        let mut preferences = None;
        let mut target = None;
        if let (Some(provider), Some(model)) = (&options.provider, &options.model) {
            params["model"] = json!({"provider":provider,"modelId":model});
            preferences =
                crate::session_host::saved_model_preferences(client, provider, model).await?;
            if let Some(preferences) = &preferences {
                params["modelPreferences"] = preferences.clone();
            }
            target = Some((provider.clone(), model.clone()));
        }
        if let Some(preset) = options.permission {
            params["permissionPolicy"] = json!({"preset":preset});
        }
        let created: Created = match client.typed("session.create", params).await {
            Ok(created) => created,
            Err(error) => {
                let error = crate::model_adoption_error(
                    client,
                    target.as_ref().map(|(p, m)| (p.as_str(), m.as_str())),
                    error,
                    preferences.as_ref(),
                )
                .await;
                return Err(format!("Cannot create session: {error}. Configure a provider or use --model-config PATH with an API-key environment reference").into());
            }
        };
        if interactive {
            println!(
                "dimcode · model: {}/{}",
                crate::sanitize::safe(&created.model.provider_id),
                crate::sanitize::safe(&created.model.model_id)
            );
        }
        created.session_id
    };
    let selected = if let (Some(preset), Some(_)) = (options.permission, &options.resume) {
        if !client.supports(&permission::PRESET_METHODS) {
            return Err(
                "This daemon cannot change a Session's permission preset; omit --permission".into(),
            );
        }
        Some(
            permission::select(client, &session_id, preset)
                .await
                .map_err(|error| format!("Cannot set the permission preset: {error}"))?,
        )
    } else {
        None
    };
    display.record(&format!(
        "{}: {}\nsession: {}",
        if interactive {
            "local files"
        } else {
            "workspace"
        },
        options.cwd.display(),
        session_id
    ))?;
    let policy = match (selected, options.permission) {
        (Some(policy), _) => Some(Ok(policy)),
        (None, Some(preset)) => Some(Ok(PermissionPolicy {
            preset,
            ..Default::default()
        })),
        (None, None) if interactive && client.supports(&["session.permission.get"]) => Some(
            permission::read(client, &session_id)
                .await
                .map(|state| state.policy),
        ),
        (None, None) => None,
    };
    match policy {
        Some(Ok(policy)) => {
            display.record(&format!("permission: {}", permission::name(policy.preset)))?;
            display.record(&format!(
                "File access: {}; Tool approval: {}",
                permission::file_label(policy.file_access()),
                permission::tool_policy_label(&policy)
            ))?;
        }
        Some(Err(error)) => display.record(&format!("permission: unavailable ({error})"))?,
        None => {}
    }
    client
        .rpc("session.attach", json!({"sessionId":session_id}))
        .await?;
    let mut questions = Questions::default();
    let result = async {
        display.muted = !interactive;
        display.cursor = read_history(client, &session_id, 0, |value| {
            questions.history("main", &value);
            if options.json { Ok(()) } else { display.event(serde_json::from_value(value)?, true) }
        }).await?;
        display.failed = false;
        display.muted = false;
        let mut code = wait_turn(client, &session_id, None, &mut display, &mut input, &mut interrupts, &mut questions, None).await?;
        if code != 0 || interactive && input.is_none() {
            return Ok(code);
        }
        if options.prompt.is_some() || !images.is_empty() {
            let content = options.prompt.iter().filter(|text| !text.trim().is_empty()).map(|text| ContentBlock::Text { text: text.clone() })
                .chain(images).collect();
            return wait_turn(client, &session_id, Some(Submission::Message(UserInput { content })), &mut display, &mut input, &mut interrupts, &mut questions, None).await;
        }
        let mut draft = Draft::default();
        let mut notice = None;
        let mut listing = None;
        loop {
            let reader = input.as_mut().expect("interactive input");
            if !composer::edit(reader, &mut draft, notice.take(), &options.cwd, &mut interrupts).await? {
                break;
            }
            let text = draft.plain().map(str::to_owned);
            if text.is_none() && draft.command().is_some() {
                notice = Some("Commands require plain single-line input; draft retained".into());
                continue;
            }
            match text.as_deref().and_then(|text| text.split_whitespace().next()) {
                Some("/mcp") => {
                    let text = text.as_deref().expect("plain MCP command");
                    drop(interrupts);
                    let result = crate::mcp_command::command(client, options, reader, &session_id, text).await;
                    #[cfg(unix)]
                    { interrupts = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?; }
                    #[cfg(windows)]
                    { interrupts = tokio::signal::windows::ctrl_c()?; }
                    if let Err(error) = result {
                        display.record(&format!("Error: {}", mcp::error(error)))?;
                    }
                    draft.clear();
                    continue;
                }
                Some("/effort" | "/reasoning" | "/fast" | "/agent") => {
                    let text = text.as_deref().expect("plain configuration command");
                    let result = settle(
                        &mut interrupts,
                        "Waiting for configuration result before accepting more input.",
                        configure(client, &session_id, text, &mut display),
                    )
                    .await;
                    if let Err(error) = result {
                        display.record(&format!("Error: {error}"))?;
                    }
                    draft.clear();
                    continue;
                }
                Some("/approvals" | "/permissions" | "/approval") => {
                    let text = text.as_deref().expect("plain approvals command");
                    if let Err(error) = permission_command::command(client, reader, &mut interrupts, &session_id, text, &mut display, &mut listing).await {
                        display.record(&format!("Error: {error}"))?;
                    }
                    draft.clear();
                    continue;
                }
                Some("/goal") => {
                    let text = text.as_deref().expect("plain goal command");
                    if let Err(error) =
                        goal_command::command(client, &session_id, text, &mut display, &mut interrupts).await
                    {
                        display.record(&format!("Error: {error}"))?;
                    }
                    draft.clear();
                    continue;
                }
                Some("/exit") => {
                    if text.as_deref().is_some_and(|text| text.trim() == "/exit") {
                        break;
                    }
                    display.record("Usage: /exit")?;
                    draft.clear();
                    continue;
                }
                Some("/export") => {
                    let operation = async {
                        let text = text.as_deref().expect("plain export command");
                        let rest = text.trim_start().strip_prefix("/export").expect("export command").trim_start();
                        let (format, path) = rest.split_once(char::is_whitespace)
                            .filter(|(_, path)| !path.trim_start().is_empty())
                            .ok_or("Usage: /export <markdown|json> PATH\nPATH is relative to the CLI current directory; ~ is literal.")?;
                        let path = path.trim_start();
                        export::write(client, &session_id, format.parse()?, Some(std::path::Path::new(path))).await
                    };
                    let result = settle(
                        &mut interrupts,
                        "Waiting for export result before accepting more input.",
                        operation,
                    )
                    .await;
                    if let Err(error) = result {
                        display.record(&format!("Error: {error}"))?;
                    }
                    draft.clear();
                    continue;
                }
                _ => {}
            }
            let submission = if let Some(text) = text.filter(|text| text.starts_with('!')) {
                Submission::Shell(text)
            } else {
                match draft.prepare(client, &session_id, &mut interrupts, &mut notice).await? {
                    Some(user) => Submission::Message(user),
                    None => continue,
                }
            };
            display.failed = false;
            code = match wait_turn(client, &session_id, Some(submission), &mut display, &mut input, &mut interrupts, &mut questions, Some(&mut draft)).await {
                Ok(code) => code,
                Err(error) if error.is::<RequestTooLarge>() => {
                    notice = Some(format!("{error}. Draft retained"));
                    continue;
                }
                Err(error) => return Err(error),
            };
            if input.is_none() {
                break;
            }
            if code != 0 && code != 130 && code != 2 {
                return Ok(code);
            }
        }
        Ok(0)
    }
    .await;
    display.finish_text()?;
    let detached = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        client.rpc("session.detach", json!({"sessionId":session_id})),
    )
    .await;
    match (result, detached) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(_)) => Err("Session detach timed out".into()),
        (Ok(_), Ok(Err(error))) => Err(error),
        (Ok(code), Ok(Ok(_))) => Ok(code),
    }
}

pub async fn read_history(
    client: &mut Client,
    session_id: &str,
    mut after: u64,
    mut consume: impl FnMut(Value) -> Result<()>,
) -> Result<u64> {
    let mut through = None;
    loop {
        let mut params =
            json!({"sessionId":session_id,"branchId":"main","after":after,"limit":256});
        if let Some(through) = through {
            params["through"] = json!(through);
        }
        let page: History = client.typed("session.history", params).await?;
        let next = page.next(after, through)?;
        through = Some(page.through);
        for event in page.events {
            consume(event)?;
        }
        match next {
            Some(next) => after = next,
            None => return Ok(page.through),
        }
    }
}

async fn configure(
    client: &mut Client,
    session_id: &str,
    text: &str,
    display: &mut Display,
) -> Result<()> {
    if !text.starts_with("/agent") {
        display.cursor = read_history(client, session_id, display.cursor, |value| {
            display.event(serde_json::from_value(value)?, true)
        })
        .await?;
        display.failed = false;
    }
    let report = configure::run(client, session_id, display.model.as_ref(), text).await?;
    for line in &report.lines {
        display.record(line)?;
    }
    if report.changed {
        display.cursor = read_history(client, session_id, display.cursor, |value| {
            display.event(serde_json::from_value(value)?, true)
        })
        .await?;
        display.failed = false;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn wait_turn(
    client: &mut Client,
    session_id: &str,
    submission: Option<Submission>,
    display: &mut Display,
    input: &mut Option<Input>,
    #[cfg(unix)] interrupts: &mut tokio::signal::unix::Signal,
    #[cfg(windows)] interrupts: &mut tokio::signal::windows::CtrlC,
    questions: &mut Questions,
    mut draft: Option<&mut Draft>,
) -> Result<i32> {
    let mut shell = None;
    let mut submit = match submission {
        Some(Submission::Shell(text)) => {
            shell = start_shell(client, session_id, &text, display).await?;
            if let Some(draft) = draft.take() {
                draft.clear();
            }
            None
        }
        Some(Submission::Message(user)) => {
            let mut params = json!({"sessionId":session_id,"branchId":"main","submissionId":uuid::Uuid::new_v4().to_string(),"mode":"queue"});
            match user.content.as_slice() {
                [ContentBlock::Text { text }] => params["text"] = json!(text),
                _ => params["user"] = json!(user),
            }
            Some(client.send("session.submit", params).await?)
        }
        None => None,
    };
    let mut idle = if submit.is_none() {
        Some(
            client
                .send(
                    "session.idle",
                    json!({"sessionId":session_id,"branchId":"main"}),
                )
                .await?,
        )
    } else {
        None
    };
    let mut stop = None;
    let mut approval_replies = HashSet::new();
    let mut member_reads: HashMap<u64, (String, String, bool)> = HashMap::new();
    let mut approvals: VecDeque<Approval> = VecDeque::new();
    let mut deferred = VecDeque::new();
    let mut prompted = false;
    let mut rule_stage = false;
    let mut rule_write: Option<RuleWrite> = None;
    let rules_available = client.supports(&permission::RULE_METHODS);
    let mut code = 0;
    let mut settled = None;
    loop {
        if let Some(sequence) = settled
            && submit.is_none()
            && idle.is_none()
            && stop.is_none()
            && approval_replies.is_empty()
            && member_reads.is_empty()
            && shell.is_none()
            && deferred.is_empty()
            && rule_write.is_none()
            && questions.idle(input.is_none() || code == 0)
        {
            display.cursor = display.cursor.max(sequence);
            display.finish_text()?;
            return Ok(if code != 0 {
                code
            } else if display.failed {
                2
            } else {
                0
            });
        }
        if let Some(approval) = approvals.front_mut()
            && let Some(reader) = input.as_mut()
            && !prompted
            && rule_write.is_none()
            && !questions.asking()
        {
            if approval.call.name == dim_agent_types::MEMBER_APPROVAL_ALLOW {
                if approval.member.is_none() {
                    if !member_reads.values().any(|(branch, request, _)| {
                        branch == &approval.branch_id && request == &approval.request_id
                    }) {
                        let id = client.send(dim_agent_types::MEMBER_APPROVAL_GET, json!({"sessionId":session_id,"branchId":approval.branch_id,"requestId":approval.request_id})).await?;
                        member_reads.insert(
                            id,
                            (
                                approval.branch_id.clone(),
                                approval.request_id.clone(),
                                false,
                            ),
                        );
                    }
                    reader.prompt("Loading member action... [n deny] ")?;
                    prompted = true;
                    continue;
                }
                display.record(&format!(
                    "[subagent approval] Leader branch: {}",
                    crate::sanitize::safe(&approval.branch_id)
                ))?;
                display.record(&crate::sanitize::safe(&format!(
                    "Leader reason: {}",
                    approval.reason
                )))?;
                match approval.member.as_ref().expect("member detail queried") {
                    Ok(descriptor) => {
                        for line in crate::member_approval::lines(descriptor) {
                            display.record(&line)?;
                        }
                    }
                    Err(error) => {
                        display.record(&format!("Cannot review member action: {error}"))?
                    }
                }
                reader.prompt(if matches!(approval.member, Some(Ok(_))) {
                    "Allow this member action once? [y/N] "
                } else if matches!(&approval.member, Some(Err(error)) if error.retryable()) {
                    "Retry details or deny? [r/N] "
                } else {
                    "Deny this member action? [N] "
                })?;
                prompted = true;
                continue;
            }
            display.record(&format!(
                "[approval] {}: {}",
                approval.call.name, approval.reason
            ))?;
            let keys: &[&str] = match approval.call.name.as_str() {
                "exec" | "execute" => &["command", "cwd", "description"],
                _ => &["path", "filePath", "description"],
            };
            for &key in keys {
                if let Some(Value::String(value)) = approval.call.input.get(key) {
                    display.record(&format!("  {key}: {value}"))?;
                }
            }
            let always = rules_available
                && !approval.once
                && permission::always_command(&approval.call).is_some();
            reader.prompt(if always {
                "Allow once, always, or deny? [y/a/N] "
            } else {
                "Allow once? [y/N] "
            })?;
            prompted = true;
            rule_stage = false;
        }
        if let Some(reader) = input.as_mut() {
            if !prompted && rule_write.is_none() && code == 0 {
                questions.open(display, reader)?;
            }
        } else {
            let branches = questions.unanswerable();
            if !branches.is_empty() {
                let reason = "Interactive answer required";
                if code == 0 {
                    display.record(
                        "Interactive answer required; stopping. Rerun in a terminal to answer it",
                    )?;
                    code = 3;
                }
                for branch_id in &branches {
                    questions
                        .stop(client, session_id, branch_id, reason)
                        .await?;
                }
                if stop.is_none() && settled.is_none() {
                    stop = Some(
                        client
                            .send(
                                "session.stop",
                                json!({"sessionId":session_id,"branchId":"main","reason":reason}),
                            )
                            .await?,
                    );
                }
            }
        }
        let event = if !prompted
            && !questions.asking()
            && let Some(event) = deferred.pop_front()
        {
            event
        } else {
            tokio::select! {
                _ = interrupts.recv() => {
                    if settled.is_some() && stop.is_none() && questions.asking() {
                        if !display.json { println!(); }
                        questions.interrupt(client, session_id).await?;
                        code = 130;
                        continue;
                    }
                    if (stop.is_some() || settled.is_some()) && shell.is_none() {
                        display.record("[stopping] Still waiting for resource settlement")?;
                        continue;
                    }
                    if !display.json { println!(); }
                    if let Some(run) = shell.as_mut().filter(|run| !prompted && !questions.asking() && !run.stopping) {
                        run.stop = Some(client.send("shell.stop", json!({"sessionId":session_id,"runId":run.run_id})).await?);
                        run.stopping = true;
                        display.record("[shell stopping] Waiting for command settlement")?;
                        continue;
                    }
                    if settled.is_some() || stop.is_some() {
                        display.record("[shell stopping] Still waiting for command settlement")?;
                        continue;
                    }
                    display.record("[stopping] Waiting for resource settlement")?;
                    code = 130;
                    approvals.clear(); prompted = false;
                    questions.interrupt(client, session_id).await?;
                    stop = Some(client.send("session.stop", json!({"sessionId":session_id,"branchId":"main","reason":"CLI interrupted"})).await?);
                    continue;
                }
                line = async {
                    match input.as_mut() { Some(reader) if stop.is_none() && rule_write.is_none() => reader.line().await, _ => pending().await }
                } => {
                    let line = line?;
                    if let (Some(text), Some(reader)) = (&line, input.as_mut()) && questions.asking() {
                        questions.line(client, session_id, text, display, reader).await?;
                        continue;
                    }
                    let always = approvals
                        .front()
                        .filter(|approval| prompted && rules_available && !approval.once)
                        .and_then(|approval| permission::always_command(&approval.call))
                        .map(str::to_owned);
                    let answer = line.as_ref().map(|line| line.trim().to_ascii_lowercase());
                    if let (true, Some(command), Some(text)) = (rule_stage, &always, &line) {
                        if let Some(rule) = permission::proposed(command, text) {
                            let request_id = approvals.front().expect("prompted approval").request_id.clone();
                            let (add, request) = permission::AddRule::start(rule);
                            let call = client.send(request.method, request.params).await?;
                            rule_write = Some(RuleWrite { request_id, add, call });
                        } else {
                            input.as_mut().expect("approval input").prompt("Type the command's leading words, or press Enter for the exact rule: ")?;
                        }
                    } else if let (false, Some(command), Some("a" | "always")) = (rule_stage, &always, answer.as_deref()) {
                        input.as_mut().expect("approval input").prompt(&format!("Always allow `{}` (exact). Press Enter, or type its leading words for a prefix rule: ", crate::sanitize::safe(command)))?;
                        rule_stage = true;
                    } else if prompted {
                        let mut approval = approvals.pop_front().ok_or("No approval awaits this answer")?;
                        let allow = !rule_stage && matches!(answer.as_deref(), Some("y" | "yes"));
                        if approval.call.name == dim_agent_types::MEMBER_APPROVAL_ALLOW {
                            if answer.as_deref() == Some("r") && matches!(&approval.member, Some(Err(error)) if error.retryable()) {
                                approval.member = None; approvals.push_front(approval); prompted = false; continue;
                            }
                            if allow {
                                if !matches!(approval.member, Some(Ok(_))) {
                                    approvals.push_front(approval); prompted = false; continue;
                                }
                                let id = client.send(dim_agent_types::MEMBER_APPROVAL_GET, json!({"sessionId":session_id,"branchId":approval.branch_id,"requestId":approval.request_id})).await?;
                                member_reads.insert(id, (approval.branch_id.clone(), approval.request_id.clone(), true));
                                approval.member = None;
                                approvals.push_front(approval);
                                input.as_mut().expect("approval input").prompt("Checking member action... [n deny] ")?;
                                continue;
                            }
                        }
                        approval_replies.insert(client.send("approval.respond", json!({"sessionId":session_id,"branchId":approval.branch_id,"requestId":approval.request_id,"decision":if allow {"allow"} else {"deny"}})).await?);
                        prompted = false;
                    } else if let Some(text) = &line {
                        if text.starts_with('!') {
                            if shell.is_some() {
                                display.record("[shell busy] This caller already has a pending command")?;
                            } else {
                                shell = start_shell(client, session_id, text, display).await?;
                            }
                        } else if !text.trim().is_empty() {
                            display.record("[busy] Wait for the prompt to send a message; !command and !!command remain available")?;
                        }
                    }
                    if line.is_none() {
                        *input = None;
                        if questions.interrupt(client, session_id).await? {
                            code = 130;
                        }
                        if let Some(run) = shell.as_mut().filter(|run| !run.stopping) {
                            run.stop = Some(client.send("shell.stop", json!({"sessionId":session_id,"runId":run.run_id})).await?);
                            run.stopping = true;
                        }
                    }
                    if line.is_none() && stop.is_none() && settled.is_none() {
                        code = 130;
                        stop = Some(client.send("session.stop", json!({"sessionId":session_id,"branchId":"main","reason":"CLI input closed"})).await?);
                    }
                    continue;
                }
                message = client.next() => message?,
            }
        };
        if display.json
            && let Message::Notice { method, params } = &event
            && params["sessionId"].as_str() == Some(session_id)
        {
            let mut output = std::io::stdout().lock();
            serde_json::to_writer(
                &mut output,
                &json!({"jsonrpc":"2.0","method":method,"params":params}),
            )?;
            writeln!(output)?;
            output.flush()?;
        }
        if let Message::Notice { method, params } = &event
            && params["sessionId"].as_str() == Some(session_id)
        {
            questions
                .notice(client, session_id, method, params, display)
                .await?;
        }
        match event {
            event @ Message::Response { id, .. }
                if (prompted || questions.asking())
                    && shell
                        .as_ref()
                        .is_some_and(|run| run.request == Some(id) || run.stop == Some(id)) =>
            {
                deferred.push_back(event)
            }
            Message::Response { id, result }
                if shell.as_ref().is_some_and(|run| run.request == Some(id)) =>
            {
                let run = shell.as_mut().expect("shell reply");
                let mut retained = false;
                match result {
                    Ok(value) => {
                        if let Err(error) = shell_result(value, &run.run_id, display) {
                            display.record(&format!("[shell error] {error}"))?;
                            retained = true;
                        }
                    }
                    Err(error) => {
                        retained = error
                            .data
                            .as_ref()
                            .and_then(|data| data.get("appError"))
                            .and_then(Value::as_str)
                            == Some("operation_interrupted");
                        display.record(&format!("[shell error] {error}"))?;
                    }
                }
                run.request = None;
                if retained {
                    display.record(&format!("[shell interrupted] Run {} may still be running. Record-mode output may still be recorded. Ctrl+C stops it.", run.run_id))?;
                } else if run.stop.is_none() {
                    shell = None;
                }
            }
            Message::Response { id, result }
                if shell.as_ref().is_some_and(|run| run.stop == Some(id)) =>
            {
                let interrupted = result.as_ref().err().is_some_and(|error| {
                    error
                        .data
                        .as_ref()
                        .and_then(|data| data.get("appError"))
                        .and_then(Value::as_str)
                        == Some("operation_interrupted")
                });
                if let Err(error) = result {
                    display.record(&format!("[shell stop error] {error}"))?;
                }
                let run = shell.as_mut().expect("shell stop reply");
                run.stop = None;
                if interrupted {
                    run.stopping = false;
                    display.record(
                        "[shell interrupted] Stop was not confirmed; Ctrl+C can request it again",
                    )?;
                } else if run.request.is_none() {
                    shell = None;
                }
            }
            Message::Response { id, result } if Some(id) == submit => {
                if let Err(error) = result {
                    match error.data.as_ref().and_then(|data| data.get("appError")).and_then(Value::as_str) {
                        Some("invalid_params" | "session_invalid_input" | "session_closed" | "session_build_failed" | "service_reloading" | "unknown_method" | "input_blocked") => return Err(error.into()),
                        _ => return Err(format!("Submission outcome could not be confirmed: {error}. The input may already be queued; resume the session instead of sending it again").into()),
                    }
                }
                if let Some(draft) = draft.take() {
                    draft.clear();
                }
                submit = None;
                if stop.is_none() && settled.is_none() {
                    idle = Some(
                        client
                            .send(
                                "session.idle",
                                json!({"sessionId":session_id,"branchId":"main"}),
                            )
                            .await?,
                    );
                }
            }
            Message::Response { id, result } if Some(id) == idle || Some(id) == stop => {
                let result = result?;
                #[derive(Deserialize)]
                struct Barrier {
                    sequence: u64,
                }
                let barrier: Barrier =
                    serde_json::from_value(result).map_err(|_| "Invalid output barrier")?;
                if Some(id) == idle {
                    idle = None;
                    if stop.is_some() {
                        continue;
                    }
                } else {
                    stop = None;
                }
                settled = Some(barrier.sequence);
                approvals.clear();
                prompted = false;
            }
            Message::Response { id, result }
                if rule_write.as_ref().is_some_and(|write| write.call == id) =>
            {
                let mut write = rule_write.take().expect("rule write reply");
                let outcome = match write.add.reply(result.map_err(Into::into)) {
                    permission::Step::Send(_) if code == 130 => {
                        Err("the turn was stopped before the rule was written".into())
                    }
                    permission::Step::Send(request) => {
                        write.call = client.send(request.method, request.params).await?;
                        rule_write = Some(write);
                        continue;
                    }
                    permission::Step::Done(outcome) => outcome,
                };
                let current = approvals
                    .front()
                    .is_some_and(|approval| approval.request_id == write.request_id);
                let rule = permission::describe(write.add.rule());
                match outcome {
                    Ok(added) => {
                        display.record(&format!(
                            "{} {rule}",
                            match added {
                                permission::Added::Added { .. } => "[rule saved]",
                                permission::Added::Present => "[rule present]",
                            }
                        ))?;
                        if current {
                            let approval = approvals.pop_front().expect("answered approval");
                            let answer = permission::allow(
                                session_id,
                                &approval.branch_id,
                                &approval.request_id,
                            );
                            approval_replies
                                .insert(client.send(answer.method, answer.params).await?);
                            prompted = false;
                        }
                    }
                    Err(error) => {
                        display.record(&format!("[rule not saved] {error}"))?;
                        if current {
                            approvals.front_mut().expect("unanswered approval").once = true;
                            prompted = false;
                        }
                    }
                }
            }
            Message::Response { id, result } if member_reads.contains_key(&id) => {
                let (branch, request, allow) =
                    member_reads.remove(&id).expect("matched member detail");
                let Some(index) = approvals.iter().position(|approval| {
                    approval.branch_id == branch && approval.request_id == request
                }) else {
                    continue;
                };
                let detail = result
                    .map_err(crate::member_approval::DetailError::from)
                    .and_then(|value| {
                        serde_json::from_value::<dim_agent_types::MemberApprovalDetail>(value)
                            .map_err(|error| {
                                crate::member_approval::DetailError::Failed(format!(
                                    "Invalid member approval detail: {error}"
                                ))
                            })
                    });
                let mut decision = None;
                match detail {
                    Ok(dim_agent_types::MemberApprovalDetail::Pending { descriptor, .. }) => {
                        approvals[index].member = Some(Ok(*descriptor));
                        if allow {
                            decision = Some("allow");
                        }
                    }
                    Ok(dim_agent_types::MemberApprovalDetail::Ended) => {
                        decision = Some("deny");
                        display.record("Member request ended")?;
                    }
                    Err(error) => approvals[index].member = Some(Err(error)),
                }
                if let Some(decision) = decision {
                    approvals.remove(index);
                    approval_replies.insert(client.send("approval.respond", json!({"sessionId":session_id,"branchId":branch,"requestId":request,"decision":decision})).await?);
                }
                if index == 0 {
                    prompted = false;
                    println!();
                }
            }
            Message::Response { id, result } if approval_replies.remove(&id) => {
                if code == 0 {
                    result?;
                }
            }
            Message::Response { id, result } if questions.owns(id) => {
                questions
                    .response(client, session_id, id, result, display, input)
                    .await?;
            }
            Message::Response { .. } => return Err("Unexpected session response identity".into()),
            Message::Notice { method, params } if method == "approval.request" => {
                let approval: Approval = decode(params, &method)?;
                if approval.session_id != session_id {
                    continue;
                }
                if stop.is_some() || settled.is_some() {
                    continue;
                }
                if input.is_none() {
                    display.record("Interactive approval required; the operation was denied. Rerun in a terminal to review it")?;
                    approval_replies.insert(client.send("approval.respond", json!({"sessionId":session_id,"branchId":approval.branch_id,"requestId":approval.request_id,"decision":"deny"})).await?);
                    code = 3;
                    stop = Some(client.send("session.stop", json!({"sessionId":session_id,"branchId":"main","reason":"Interactive approval required"})).await?);
                } else {
                    approvals.push_back(approval);
                }
            }
            Message::Notice { method, params }
                if matches!(method.as_str(), "approval.cancelled" | "approval.resolved") =>
            {
                let cancelled: ApprovalCancelled = decode(params, &method)?;
                if cancelled.session_id == session_id {
                    let current = approvals.front().is_some_and(|approval| {
                        approval.request_id == cancelled.request_id
                            && approval.branch_id == cancelled.branch_id
                    });
                    approvals.retain(|approval| {
                        approval.request_id != cancelled.request_id
                            || approval.branch_id != cancelled.branch_id
                    });
                    if current {
                        prompted = false;
                        println!();
                        display.record("[approval cancelled]")?;
                    }
                }
            }
            Message::Notice { method, params }
                if method == "session.notice"
                    && params["sessionId"] == session_id
                    && params["source"] == "Collaboration"
                    && params["kind"] == "memberApprovalEnded" =>
            {
                let ended: dim_agent_types::MemberApprovalEnded =
                    decode(params["data"].clone(), "memberApprovalEnded")?;
                let mut retained = VecDeque::new();
                while let Some(approval) = approvals.pop_front() {
                    if approval.branch_id == ended.branch_id
                        && approval.call.name == dim_agent_types::MEMBER_APPROVAL_ALLOW
                        && approval
                            .call
                            .input
                            .get("approvalId")
                            .is_some_and(|id| id == &ended.approval_id)
                    {
                        prompted = false;
                        approval_replies.insert(client.send("approval.respond", json!({"sessionId":session_id,"branchId":approval.branch_id,"requestId":approval.request_id,"decision":"deny"})).await?);
                        display.record("Member request ended")?;
                    } else {
                        retained.push_back(approval);
                    }
                }
                approvals = retained;
            }
            notice @ Message::Notice { .. } if prompted || questions.asking() => {
                deferred.push_back(notice)
            }
            Message::Notice { method, params }
                if method == "management.progress"
                    && params["id"].as_u64().is_some_and(|id| {
                        shell.as_ref().is_some_and(|run| run.request == Some(id))
                    })
                    && params["method"] == "shell.run" =>
            {
                #[derive(Deserialize)]
                struct ShellProgress {
                    text: String,
                    skipped: Option<u64>,
                }
                let Ok(progress) = serde_json::from_value::<ShellProgress>(params["data"].clone())
                else {
                    display.record("[shell progress error] Invalid shell.run progress")?;
                    continue;
                };
                if let Some(skipped) = progress.skipped {
                    display.record(&format!(
                        "[shell progress] skipped {skipped} bytes; final output follows"
                    ))?;
                }
                let run = shell.as_mut().expect("shell progress");
                if !run.progress_started {
                    display.record("[shell progress] (incomplete)")?;
                    run.progress_started = true;
                }
                display.shell_progress(&progress.text)?;
            }
            notice @ Message::Notice { .. } => {
                render(notice, session_id, display)?;
            }
        }
    }
}

fn render(notice: Message, session_id: &str, display: &mut Display) -> Result<()> {
    let Message::Notice { method, params } = notice else {
        unreachable!()
    };
    if display.json {
        if params["sessionId"].as_str() == Some(session_id) {
            if method == "session.closed" {
                return Err("Session closed while attached".into());
            }
            let event = &params["event"];
            if method == "session.event"
                && params["branchId"] == "main"
                && ((event["type"] == "turn" && event["name"] == "end")
                    || (event["type"] == "step" && event["name"] == "model-output-end"))
                && event["data"]["status"] == "failed"
            {
                display.failed = true;
            }
        }
        return Ok(());
    }
    match method.as_str() {
        "session.event" => {
            let changed = permission::changed(&params["event"]);
            let notice: EventNotice = decode(params, &method)?;
            if notice.session_id == session_id && notice.branch_id == "main" {
                if let Some((sequence, policy)) = changed
                    && sequence > display.cursor.max(display.permission_seen)
                {
                    display.record(&format!(
                        "[permission] main branch preset: {}; File access: {}; Tool approval: {}",
                        permission::name(policy.preset),
                        permission::file_label(policy.file_access()),
                        permission::tool_policy_label(&policy)
                    ))?;
                    display.permission_seen = sequence;
                }
                display.event(notice.event, false)?;
            }
        }
        "session.model.progress" => {
            let notice: Progress = decode(params, &method)?;
            if notice.session_id == session_id && notice.branch_id == "main" {
                display.progress(notice)?;
            }
        }
        "session.mcp.state" => {
            let state: mcp::State = decode(params, &method)?;
            if state.session_id == session_id {
                let required: std::collections::HashSet<_> = state
                    .servers
                    .into_iter()
                    .filter(|server| server.state == "auth_required")
                    .map(|server| server.id)
                    .collect();
                for id in &required {
                    if display.mcp_auth_required.contains(id) {
                        continue;
                    }
                    display.record(&format!(
                        "MCP {id:?}: sign-in required. Run mcp login {id:?} or /mcp login {id:?}."
                    ))?;
                }
                display.mcp_auth_required = required;
            }
        }
        "session.closed" if params["sessionId"].as_str() == Some(session_id) => {
            return Err("Session closed while attached".into());
        }
        _ => {}
    }
    std::io::stdout().flush()?;
    Ok(())
}
