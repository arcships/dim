use super::{fill, record};
use crate::native::Native;
use crate::{Error, Migration, Outcome, Result, TaskState, source::Source};
use serde_json::{Value, json};

pub(super) async fn import(
    owner: &Migration,
    task: &TaskState,
    source: &Source,
    client: &Native,
) -> Result<()> {
    let shared = source
        .json("data/plugins/web/state.json", &owner.stopping)
        .await;
    let state = match shared {
        Ok(Some(state)) => Some(state),
        Ok(None) => crate::engine::document(owner, task, source, "web-plugin.json").await?,
        Err(Error::Object(reason)) => {
            return record(owner, task, "web".into(), Outcome::Failed, Some(reason)).await;
        }
        Err(error) => return Err(error),
    };
    let Some(state) = state else { return Ok(()) };
    let valid = state.get("version") == Some(&json!(1))
        && state.get("enabled").is_none_or(Value::is_boolean)
        && state
            .get("activeProviderExplicit")
            .is_none_or(Value::is_boolean)
        && matches!(
            state.get("activeProvider").and_then(Value::as_str),
            Some("serper" | "tavily" | "brave" | "bocha" | "deepseek" | "dim-oauth")
        )
        && state
            .get("providers")
            .and_then(Value::as_object)
            .is_some_and(|providers| {
                providers.keys().all(|key| {
                    matches!(
                        key.as_str(),
                        "serper" | "tavily" | "brave" | "bocha" | "deepseek"
                    )
                })
            });
    if !valid {
        return record(
            owner,
            task,
            "web".into(),
            Outcome::Unsupported,
            Some("Legacy Web state is incompatible; source preserved".into()),
        )
        .await;
    }
    let providers = state["providers"].as_object().unwrap();
    let selected = state["activeProvider"].as_str().unwrap();
    let explicit = state.get("activeProviderExplicit") == Some(&Value::Bool(true));
    let explicit_brave = explicit && selected == "brave";
    if explicit_brave {
        if let Some(provider) = providers.get("brave")
            && provider.as_object().is_some_and(|fields| {
                fields
                    .keys()
                    .all(|key| matches!(key.as_str(), "apiKey" | "apiEndpoint"))
            })
            && provider
                .get("apiKey")
                .and_then(Value::as_str)
                .is_some_and(|key| !key.is_empty())
            && provider.get("apiEndpoint").is_none_or(Value::is_string)
        {
            let mut service = json!({"kind":"brave","apiKey":provider["apiKey"]});
            if let Some(endpoint) = provider.get("apiEndpoint") {
                service["endpoint"] = endpoint.clone();
            }
            fill(owner, task, client, "web", json!({"searchService":service})).await?;
        } else {
            record(
                owner,
                task,
                "web.searchService".into(),
                Outcome::Unsupported,
                Some("Selected Brave configuration is incomplete or incompatible".into()),
            )
            .await?;
        }
    } else if !explicit || selected == "dim-oauth" {
        if explicit {
            fill(
                owner,
                task,
                client,
                "web",
                json!({"searchService":{"kind":"dim-oauth"}}),
            )
            .await?;
        }
    } else {
        record(owner, task, "web.activeProvider".into(), Outcome::Unsupported, Some("Native Web supports dim-oauth and Brave selection; other adapters remain in the source".into())).await?;
    }
    for key in providers
        .keys()
        .filter(|key| !explicit_brave || key.as_str() != "brave")
    {
        record(
            owner,
            task,
            format!("web.providers.{key}"),
            Outcome::Deferred,
            Some("Inactive provider configuration has no native storage slot".into()),
        )
        .await?;
    }
    for key in state.as_object().unwrap().keys().filter(|key| {
        !matches!(
            key.as_str(),
            "version" | "enabled" | "activeProvider" | "activeProviderExplicit" | "providers"
        )
    }) {
        record(
            owner,
            task,
            format!("web.{key}"),
            Outcome::Unsupported,
            Some("No equivalent Web configuration field".into()),
        )
        .await?;
    }
    let selection = client
        .plugin(
            "web",
            state
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            &owner.stopping,
        )
        .await;
    let (outcome, reason) = match selection {
        Ok(outcome) => (outcome, None),
        Err(Error::Object(reason)) => (Outcome::Conflict, Some(reason)),
        Err(error) => return Err(error),
    };
    record(owner, task, "web.enabled".into(), outcome, reason).await
}
