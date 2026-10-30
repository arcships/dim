//! The Session configuration commands both clients offer: `/effort` and
//! `/reasoning` (reasoning effort), `/fast` (service tier) and `/agent`.

use crate::{Result, client::Client, wire::Configuration};
use dim_agent_types::{ModelCapabilities, ModelReasoning, ModelServiceTier};
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
struct Configured {
    outcome: String,
}

/// What a configuration command did.
pub struct Report {
    pub lines: Vec<String>,
    /// The Session's configuration changed, so its history has new events.
    pub changed: bool,
}

/// Whether `text` reaches for the Session: every configuration command
/// given a value writes the shown Session (`/agent ID`, `/effort VALUE`,
/// `/reasoning VALUE`, `/fast on|off`). Bare commands read a catalog and
/// answer on their own.
pub fn needs_session(text: &str) -> bool {
    text.split_whitespace().nth(1).is_some()
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
    let value = words.get(1).copied();
    if command == "/agent" {
        if let Some(agent_id) = value {
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
            });
        }
    } else {
        let model = model.ok_or("The adopted model is unavailable")?;
        #[derive(Deserialize)]
        struct Snapshot {
            catalog: Option<Catalog>,
        }
        #[derive(Deserialize)]
        struct Catalog {
            models: Vec<CatalogModel>,
        }
        #[derive(Deserialize)]
        struct CatalogModel {
            id: String,
            capabilities: ModelCapabilities,
            metadata: Option<Preferences>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Preferences {
            reasoning: Option<ModelReasoning>,
            service_tier: Option<ModelServiceTier>,
        }
        let snapshot: Snapshot = client
            .typed("provider.catalog", json!({"providerId":model.provider_id}))
            .await?;
        let catalog = snapshot
            .catalog
            .ok_or("Model preference catalog is unavailable")?;
        let catalog_model = catalog
            .models
            .into_iter()
            .find(|entry| entry.id == model.model_id)
            .ok_or("The adopted model is absent from the catalog")?;
        let metadata = catalog_model
            .metadata
            .ok_or("This model does not advertise preference choices")?;
        let preference = if command == "/fast" {
            metadata
                .service_tier
                .as_ref()
                .filter(|tier| {
                    tier.supported
                        && ["priority", "default"]
                            .iter()
                            .all(|value| tier.options.iter().any(|option| option == value))
                })
                .ok_or("This model does not advertise priority/default service tiers")?;
            match value {
                Some("on") => Some(json!({"serviceTier":"priority"})),
                Some("off") => Some(json!({"serviceTier":"default"})),
                Some(_) => return Err("Usage: /fast [on|off]".into()),
                None => {
                    lines.push("Service tier choices: on (priority), off (default)".into());
                    None
                }
            }
        } else {
            let reasoning = metadata
                .reasoning
                .as_ref()
                .filter(|reasoning| reasoning.supported && catalog_model.capabilities.reasoning)
                .ok_or("This model does not advertise reasoning preferences")?;
            let mut choices = reasoning.effort_options.clone().unwrap_or_default();
            if reasoning.off.is_some() && !choices.iter().any(|choice| choice == "none") {
                choices.push("none".into());
            }
            if let Some(value) = value {
                if !choices.iter().any(|choice| choice == value) {
                    return Err("Reasoning effort is not advertised by this model".into());
                }
                Some(json!({"reasoningEffort":value}))
            } else {
                lines.push(format!(
                    "Reasoning effort choices: {}",
                    if choices.is_empty() {
                        "unavailable".into()
                    } else {
                        choices.join(", ")
                    }
                ));
                None
            }
        };
        if let Some(preference) = preference {
            let result: Configured = client
                .typed(
                    "session.model.update",
                    json!({"sessionId":session_id,"branchId":"main","modelPreferences":preference}),
                )
                .await?;
            lines.push(format!(
                "{} configuration {}: {}",
                if command == "/fast" {
                    "Service tier"
                } else {
                    "Reasoning effort"
                },
                result.outcome,
                value.expect("preference value")
            ));
        } else {
            return Ok(Report {
                lines,
                changed: false,
            });
        }
    }
    Ok(Report {
        lines,
        changed: true,
    })
}
