//! The Session configuration commands both clients offer: `/effort` and
//! `/reasoning` (reasoning effort), `/fast` (service tier) and `/agent`.

use crate::{
    Result,
    client::Client,
    wire::{Configuration, ProviderCatalogSnapshot},
};
use dim_agent_types::ModelPreferences;
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
struct Configured {
    outcome: String,
}

/// The adopted model's advertised reasoning efforts and effective selection.
#[derive(Debug, PartialEq)]
pub struct EffortChoices {
    pub model: String,
    pub current: Option<String>,
    pub choices: Vec<String>,
}

/// What a configuration command did.
pub struct Report {
    pub lines: Vec<String>,
    /// The Session's configuration changed, so its history has new events.
    pub changed: bool,
    /// Present for a bare reasoning-effort query.
    pub effort: Option<EffortChoices>,
}

/// What a `/effort`, `/reasoning` or `/fast` line asks of the model it names.
pub enum Preference {
    /// The `modelPreferences` members to apply, and the value the line named.
    Apply {
        preferences: ModelPreferences,
        value: String,
    },
    /// The line a bare command reports, and the choices the effort dialog draws.
    List {
        line: String,
        effort: Option<EffortChoices>,
    },
}

/// The notice for a preference command with no model to read: Home shown
/// before the daemon resolved one.
pub const NO_MODEL: &str =
    "No model to configure: sign in with /login or choose a model with /models.";

/// Whether `text` writes the shown Session: `/agent ID` switches the profile
/// the Session runs, so it needs one. The preference commands act on the
/// model the interface shows, which at Home is the one the first message's
/// Session adopts. Bare commands read advertised choices and answer on their
/// own.
pub fn needs_session(text: &str) -> bool {
    let mut words = text.split_whitespace();
    words.next() == Some("/agent") && words.next().is_some()
}

/// Resolves `text`'s preference command against `model`, the model a client
/// shows: the members its value names, or the choices its bare form lists.
/// The declaration comes from the configuration's own snapshot, else from
/// `provider.catalog`. Writes nothing.
pub async fn preference(
    client: &mut Client,
    model: &Configuration,
    text: &str,
) -> Result<Preference> {
    let words: Vec<_> = text.split_whitespace().collect();
    let command = words[0];
    if words.len() > 2 {
        return Err(format!("Usage: {command} [VALUE]").into());
    }
    let value = words.get(1).copied();
    let (reasoning, service_tier, supports_reasoning) =
        if let Some(snapshot) = &model.capability_snapshot {
            (
                snapshot.reasoning.clone(),
                snapshot.service_tier.clone(),
                snapshot.capabilities.reasoning,
            )
        } else {
            let snapshot: ProviderCatalogSnapshot = client
                .typed(
                    "provider.catalog",
                    json!({"providerId":model.provider_id,"modelId":model.model_id}),
                )
                .await?;
            let catalog = snapshot
                .catalog
                .ok_or("Model preference catalog is unavailable")?;
            let catalog_model = catalog
                .models
                .into_iter()
                .find(|entry| entry.id == model.model_id)
                .ok_or("This model is absent from the catalog")?;
            let metadata = catalog_model
                .metadata
                .ok_or("This model does not advertise preference choices")?;
            (
                metadata.reasoning,
                metadata.service_tier,
                catalog_model.capabilities.reasoning,
            )
        };
    if command == "/fast" {
        service_tier
            .as_ref()
            .filter(|tier| {
                tier.supported
                    && ["priority", "default"]
                        .iter()
                        .all(|value| tier.options.iter().any(|option| option == value))
            })
            .ok_or("This model does not advertise priority/default service tiers")?;
        return Ok(match value {
            Some(tier @ ("on" | "off")) => Preference::Apply {
                preferences: ModelPreferences {
                    reasoning_effort: None,
                    service_tier: Some(
                        if tier == "on" { "priority" } else { "default" }.to_owned(),
                    ),
                },
                value: tier.to_owned(),
            },
            Some(_) => return Err("Usage: /fast [on|off]".into()),
            None => Preference::List {
                line: "Service tier choices: on (priority), off (default)".into(),
                effort: None,
            },
        });
    }
    let reasoning = reasoning
        .as_ref()
        .filter(|reasoning| reasoning.supported && supports_reasoning)
        .ok_or("This model does not advertise reasoning preferences")?;
    let choices = reasoning.offered_efforts();
    Ok(match value {
        Some(value) => {
            if !choices.iter().any(|choice| choice == value) {
                return Err("Reasoning effort is not advertised by this model".into());
            }
            Preference::Apply {
                preferences: ModelPreferences {
                    reasoning_effort: Some(value.to_owned()),
                    service_tier: None,
                },
                value: value.to_owned(),
            }
        }
        None => Preference::List {
            line: format!(
                "Reasoning effort choices: {}",
                if choices.is_empty() {
                    "unavailable".into()
                } else {
                    choices.join(", ")
                }
            ),
            effort: Some(EffortChoices {
                model: format!("{} / {}", model.provider_id, model.model_id),
                current: model
                    .model_preferences
                    .as_ref()
                    .and_then(|preferences| preferences.reasoning_effort.clone()),
                choices,
            }),
        },
    })
}

/// Runs `text`, a configuration command line; `model` is the model the
/// Session adopted, which the preference commands read choices for.
pub async fn run(
    client: &mut Client,
    session_id: &str,
    model: Option<&Configuration>,
    text: &str,
) -> Result<Report> {
    let mut lines = Vec::new();
    let words: Vec<_> = text.split_whitespace().collect();
    let command = words[0];
    if words.len() > 2 {
        return Err(format!("Usage: {command} [VALUE]").into());
    }
    if command == "/agent" {
        if let Some(agent_id) = words.get(1).copied() {
            let result: Configured = client
                .typed(
                    "session.agent.update",
                    json!({"sessionId":session_id,"branchId":"main","agentId":agent_id}),
                )
                .await?;
            lines.push(format!(
                "Agent prompt/model configuration {}; Session tools unchanged.",
                result.outcome
            ));
        } else {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Agent {
                agent_id: String,
                name: String,
                description: String,
            }
            let agents: Vec<Agent> = client.typed("agent.list", json!({})).await?;
            lines.push("ID\tNAME\tDESCRIPTION".into());
            for agent in agents {
                lines.push(format!(
                    "{}\t{}\t{}",
                    agent.agent_id, agent.name, agent.description
                ));
            }
            return Ok(Report {
                lines,
                changed: false,
                effort: None,
            });
        }
    } else {
        let model = model.ok_or("The adopted model is unavailable")?;
        match preference(client, model, text).await? {
            Preference::Apply { preferences, value } => {
                let result: Configured = client
                    .typed(
                        "session.model.update",
                        json!({"sessionId":session_id,"branchId":"main","modelPreferences":preferences}),
                    )
                    .await?;
                lines.push(format!(
                    "{} configuration {}: {value}",
                    if command == "/fast" {
                        "Service tier"
                    } else {
                        "Reasoning effort"
                    },
                    result.outcome,
                ));
            }
            Preference::List { line, effort } => {
                lines.push(line);
                return Ok(Report {
                    lines,
                    changed: false,
                    effort,
                });
            }
        }
    }
    Ok(Report {
        lines,
        changed: true,
        effort: None,
    })
}
