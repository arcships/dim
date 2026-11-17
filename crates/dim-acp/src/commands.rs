use crate::{config::Config, rpc::Rpc};
use agent_client_protocol::{Error, schema::v1 as acp};
use dim_agent_types::{GoalRead, GoalViewStatus};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

const GOAL_USAGE: &str =
    "Usage: /goal [OBJECTIVE | set OBJECTIVE | edit OBJECTIVE | pause | resume | clear]";
const LOOP_USAGE: &str = "Usage: /loop [PROMPT | INTERVAL PROMPT | stop [ID]]";

pub(crate) fn parse(prompt: &[acp::ContentBlock]) -> Option<(&str, &str)> {
    let [acp::ContentBlock::Text(text)] = prompt else {
        return None;
    };
    let text = text.text.trim();
    let (name, words) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
    matches!(name, "/compact" | "/usage" | "/goal" | "/loop").then(|| (&name[1..], words.trim()))
}

pub(crate) fn available(config: &Config) -> Vec<acp::AvailableCommand> {
    let mut commands = Vec::new();
    for (name, description, hint, methods) in [
        (
            "compact",
            "Compact the current conversation context",
            None,
            &["session.context.compact"][..],
        ),
        (
            "goal",
            "Show or manage the conversation goal",
            Some("[OBJECTIVE | set OBJECTIVE | edit OBJECTIVE | pause | resume | clear]"),
            &[
                "session.goal.get",
                "session.goal.set",
                "session.goal.edit",
                "session.goal.pause",
                "session.goal.resume",
                "session.goal.clear",
            ][..],
        ),
        (
            "loop",
            "List, start or stop loops on this conversation",
            Some("[PROMPT | INTERVAL PROMPT | stop [ID]]"),
            &[
                "session.loop.list",
                "session.loop.start",
                "session.loop.stop",
            ][..],
        ),
    ] {
        if methods
            .iter()
            .all(|method| config.methods.contains(*method))
        {
            let mut command = acp::AvailableCommand::new(name, description);
            if let Some(hint) = hint {
                command = command.input(acp::AvailableCommandInput::Unstructured(
                    acp::UnstructuredCommandInput::new(hint),
                ));
            }
            commands.push(command);
        }
    }
    if config
        .model
        .as_ref()
        .is_some_and(|model| config.usage_providers.contains(&model.provider_id))
    {
        commands.push(acp::AvailableCommand::new(
            "usage",
            "Show subscription quotas and reset windows for the selected provider",
        ));
    }
    commands
}

pub(crate) async fn run(
    rpc: &Rpc,
    id: &str,
    config: &Config,
    name: &str,
    words: &str,
    cancelled: &CancellationToken,
) -> Result<Option<String>, Error> {
    if cancelled.is_cancelled() {
        return Err(Error::request_cancelled());
    }
    if !available(config).iter().any(|command| command.name == name) {
        return Ok(Some(format!(
            "/{name} is unavailable for this session's selected provider or native capabilities."
        )));
    }
    let text = match name {
        "compact" if words.is_empty() => {
            let result = rpc
                .call(
                    "session.context.compact",
                    json!({"sessionId":id,"branchId":"main"}),
                )
                .await;
            match result {
                Ok(value) if matches!(value["status"].as_str(), Some("completed" | "skipped")) => {
                    return Ok(None);
                }
                Err(error) => return Err(error),
                Ok(_) => return Err(Error::internal_error()),
            }
        }
        "compact" => "Usage: /compact".into(),
        "usage" if words.is_empty() => {
            usage(rpc, &config.model.as_ref().unwrap().provider_id, cancelled).await?
        }
        "usage" => "Usage: /usage".into(),
        "goal" => goal(rpc, id, words, cancelled).await?,
        "loop" => loops(rpc, id, words, cancelled).await?,
        _ => unreachable!(),
    };
    Ok(Some(text))
}

async fn goal(
    rpc: &Rpc,
    id: &str,
    words: &str,
    cancelled: &CancellationToken,
) -> Result<String, Error> {
    let (verb, rest) = words.split_once(char::is_whitespace).unwrap_or((words, ""));
    let rest = rest.trim();
    if matches!(verb, "set" | "edit") && rest.is_empty() {
        return Ok(GOAL_USAGE.into());
    }
    let query = json!({"sessionId":id,"branchId":"main"});
    let mut read: GoalRead = rpc.typed("session.goal.get", query.clone()).await?;
    if !words.is_empty() {
        let (method, objective) = match verb {
            "set" => ("session.goal.set", Some(rest)),
            "edit" => ("session.goal.edit", Some(rest)),
            "pause" if rest.is_empty() => ("session.goal.pause", None),
            "resume" if rest.is_empty() => ("session.goal.resume", None),
            "clear" if rest.is_empty() => ("session.goal.clear", None),
            _ if read.goal.as_ref().is_some_and(|goal| {
                matches!(
                    goal.status,
                    GoalViewStatus::Active | GoalViewStatus::Paused(_)
                )
            }) =>
            {
                ("session.goal.edit", Some(words))
            }
            _ => ("session.goal.set", Some(words)),
        };
        if objective.is_some_and(|text| text.chars().count() > 4000) {
            return Ok(format!(
                "{GOAL_USAGE}\nThe objective must contain 1–4000 characters."
            ));
        }
        let mut params = query.clone();
        if let Some(objective) = objective {
            params["objective"] = json!(objective);
        }
        if cancelled.is_cancelled() {
            return Err(Error::request_cancelled());
        }
        rpc.call(method, params).await?;
        read = rpc.typed("session.goal.get", query).await?;
    }
    let Some(goal) = read.goal else {
        return Ok(format!("No goal on {}.\n{GOAL_USAGE}", read.branch_id));
    };
    let status = match goal.status {
        GoalViewStatus::Active => "active".into(),
        GoalViewStatus::Paused(reason) => format!("paused ({})", reason.replace('_', " ")),
        GoalViewStatus::Completed => "completed".into(),
        GoalViewStatus::Blocked => "blocked".into(),
        GoalViewStatus::None => "no status".into(),
    };
    Ok(format!(
        "Goal on {} · {status}{}\n{}",
        read.branch_id,
        if goal.evaluator {
            " · evaluator judge"
        } else {
            ""
        },
        goal.objective
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Loop {
    loop_id: String,
    state: String,
    round: u64,
    next_at: Option<String>,
}
#[derive(Deserialize)]
struct LoopList {
    loops: Vec<Loop>,
}
fn loop_report(loops: &[Loop]) -> String {
    if loops.is_empty() {
        return "No loop on main.".into();
    }
    loops
        .iter()
        .map(|record| {
            format!(
                "{} · {} · round {}{}",
                record.loop_id,
                record.state,
                record.round,
                record
                    .next_at
                    .as_ref()
                    .map(|at| format!(" · next {at}"))
                    .unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

async fn loops(
    rpc: &Rpc,
    id: &str,
    words: &str,
    cancelled: &CancellationToken,
) -> Result<String, Error> {
    let (verb, rest) = words.split_once(char::is_whitespace).unwrap_or((words, ""));
    let rest = rest.trim();
    if verb == "stop" && rest.contains(char::is_whitespace) {
        return Ok(LOOP_USAGE.into());
    }
    if words.is_empty() || verb == "stop" {
        let listed: LoopList = rpc
            .typed(
                "session.loop.list",
                json!({"sessionId":id,"branchId":"main"}),
            )
            .await?;
        if words.is_empty() {
            return Ok(format!("{LOOP_USAGE}\n{}", loop_report(&listed.loops)));
        }
        let target = if rest.is_empty() {
            let live: Vec<_> = listed
                .loops
                .iter()
                .filter(|record| matches!(record.state.as_str(), "waiting" | "running"))
                .collect();
            if live.is_empty() {
                return Ok("No live loop on main.".into());
            }
            if live.len() > 1 {
                return Ok(format!(
                    "No single live loop on main; choose /loop stop ID.\n{}",
                    loop_report(&listed.loops)
                ));
            }
            &live[0].loop_id
        } else {
            let Some(record) = listed.loops.iter().find(|record| record.loop_id == rest) else {
                return Ok(format!(
                    "Loop {rest} is not on main.\n{}",
                    loop_report(&listed.loops)
                ));
            };
            &record.loop_id
        };
        #[derive(Deserialize)]
        struct Stopped {
            r#loop: Loop,
            settled: Option<bool>,
        }
        if cancelled.is_cancelled() {
            return Err(Error::request_cancelled());
        }
        let stopped: Stopped = rpc
            .typed("session.loop.stop", json!({"sessionId":id,"loopId":target}))
            .await?;
        return Ok(
            if stopped.settled == Some(false)
                && !matches!(stopped.r#loop.state.as_str(), "stopped" | "expired")
            {
                format!(
                    "Loop {target} stop requested; an accepted round is still settling. Use /loop to inspect it."
                )
            } else {
                format!("Loop {target} {}.", stopped.r#loop.state)
            },
        );
    }
    let seconds = [("s", 1u64), ("m", 60), ("h", 3600), ("d", 86400)]
        .into_iter()
        .find_map(|(suffix, scale)| {
            verb.strip_suffix(suffix)?
                .parse::<u64>()
                .ok()?
                .checked_mul(scale)
        })
        .filter(|seconds| *seconds > 0 && !rest.is_empty());
    let mut params =
        json!({"sessionId":id,"branchId":"main","prompt":if seconds.is_some() {rest} else {words}});
    if let Some(seconds) = seconds {
        params["everySeconds"] = json!(seconds);
    }
    #[derive(Deserialize)]
    struct Started {
        r#loop: Loop,
    }
    let started: Started = rpc.typed("session.loop.start", params).await?;
    Ok(format!(
        "Loop {} started on main ({}).",
        started.r#loop.loop_id,
        seconds
            .map(|seconds| format!("every {seconds} seconds"))
            .unwrap_or_else(|| "self-paced".into())
    ))
}

#[derive(Deserialize)]
struct Usage {
    plan: Option<Plan>,
    credits: Option<Credits>,
    #[serde(default)]
    windows: Vec<Window>,
    #[serde(default)]
    features: Vec<Feature>,
}
#[derive(Deserialize)]
struct Plan {
    name: Option<String>,
    status: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Credits {
    used_units: Option<u64>,
    total_units: Option<u64>,
    remaining_units: Option<u64>,
    unlimited: Option<bool>,
    resets_at: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Window {
    duration_hours: u64,
    token_used: u64,
    token_cap: u64,
    expires_at: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Feature {
    key: String,
    used: Option<u64>,
    allowance: Option<u64>,
    remaining: Option<u64>,
    unit: Option<String>,
    unlimited: Option<bool>,
    period_end: Option<String>,
}

fn meter(
    used: Option<u64>,
    total: Option<u64>,
    remaining: Option<u64>,
    unlimited: Option<bool>,
    unit: Option<&str>,
    reset: Option<&str>,
) -> String {
    let unit = unit.map(|unit| format!(" {unit}")).unwrap_or_default();
    let mut parts = Vec::new();
    if unlimited == Some(true) {
        parts.push("unlimited".into());
    } else {
        if let Some(remaining) = remaining {
            parts.push(format!("{remaining}{unit} remaining"));
        }
        if let Some(total) = total {
            parts.push(match used {
                Some(used) => format!("{used}/{total}{unit} used"),
                None => format!("{total}{unit} total"),
            });
        }
    }
    if let Some(used) = used
        && (total.is_none() || unlimited == Some(true))
    {
        parts.push(format!("{used}{unit} used"));
    }
    if let Some(reset) = reset {
        parts.push(format!("resets {reset}"));
    }
    parts.join(" · ")
}

async fn usage(rpc: &Rpc, provider: &str, cancelled: &CancellationToken) -> Result<String, Error> {
    #[derive(Deserialize)]
    struct Status {
        account: Option<Value>,
    }
    let status: Status = rpc
        .typed("account.status", json!({"providerId":provider}))
        .await?;
    if status.account.is_none() {
        return Ok(format!("Sign in to {provider} to view subscription usage."));
    }
    if cancelled.is_cancelled() {
        return Err(Error::request_cancelled());
    }
    let report: Usage = rpc
        .typed("account.usage", json!({"providerId":provider}))
        .await?;
    let mut lines = Vec::new();
    if let Some(plan) = report.plan {
        let fields: Vec<_> = [plan.name, plan.status].into_iter().flatten().collect();
        if !fields.is_empty() {
            lines.push(format!("Plan: {}", fields.join(" · ")));
        }
    }
    if let Some(credits) = report.credits {
        let text = meter(
            credits.used_units,
            credits.total_units,
            credits.remaining_units,
            credits.unlimited,
            Some("credits"),
            credits.resets_at.as_deref(),
        );
        if !text.is_empty() {
            lines.push(format!("Credits: {text}"));
        }
    }
    for window in report.windows {
        lines.push(format!(
            "{} hour window: {}",
            window.duration_hours,
            meter(
                Some(window.token_used),
                Some(window.token_cap),
                None,
                None,
                Some("tokens"),
                window.expires_at.as_deref()
            )
        ));
    }
    for feature in report.features {
        let text = meter(
            feature.used,
            feature.allowance,
            feature.remaining,
            feature.unlimited,
            feature.unit.as_deref(),
            feature.period_end.as_deref(),
        );
        if !text.is_empty() {
            lines.push(format!("{}: {text}", feature.key));
        }
    }
    if lines.is_empty() {
        Ok(format!("Subscription usage is unavailable for {provider}."))
    } else {
        Ok(format!(
            "Subscription usage · {provider}\n{}",
            lines.join("\n")
        ))
    }
}
