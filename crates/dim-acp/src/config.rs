use crate::rpc::Rpc;
use agent_client_protocol::Error;
use dim_agent_types::{ModelId, PermissionPolicy};
use dim_client::wire::{Configuration, Event, EventContent};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

pub(crate) const MODES: [&str; 3] = ["read-only", "workspace-write", "full-access"];

#[derive(Clone, Deserialize)]
pub(crate) struct Policy {
    pub version: u64,
    pub policy: PermissionPolicy,
}
#[derive(Clone)]
pub(crate) struct Model {
    pub provider: String,
    pub id: String,
    pub name: String,
}
#[derive(Default, Clone)]
pub(crate) struct Config {
    pub methods: HashSet<String>,
    pub policy: Option<Policy>,
    pub model: Option<Configuration>,
    pub models: Vec<Model>,
}

pub(crate) async fn models(rpc: &Rpc) -> Result<Vec<Model>, Error> {
    #[derive(Deserialize)]
    struct Providers {
        providers: Vec<Provider>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Provider {
        provider_id: String,
        enabled: bool,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Managed {
        provider_id: String,
    }
    #[derive(Deserialize)]
    struct Snapshot {
        catalog: Option<Catalog>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Catalog {
        models: Vec<Entry>,
        disabled_model_ids: Vec<String>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Entry {
        id: String,
        name: String,
        unavailable_reason: Option<String>,
    }
    let mut providers = Vec::new();
    if rpc.supports("provider.list") {
        let listed: Providers = rpc.typed("provider.list", json!({})).await?;
        providers.extend(
            listed
                .providers
                .into_iter()
                .filter(|p| p.enabled)
                .map(|p| p.provider_id),
        );
    }
    if rpc.supports("account.providers") {
        let managed: Vec<Managed> = rpc.typed("account.providers", json!({})).await?;
        providers.extend(managed.into_iter().map(|p| p.provider_id));
    }
    let mut choices = Vec::new();
    for provider in providers {
        let snapshot: Snapshot = rpc
            .typed("provider.catalog", json!({"providerId":provider}))
            .await?;
        if let Some(catalog) = snapshot.catalog {
            choices.extend(
                catalog
                    .models
                    .into_iter()
                    .filter(|m| {
                        m.unavailable_reason.is_none()
                            && !catalog.disabled_model_ids.contains(&m.id)
                    })
                    .map(|m| Model {
                        provider: provider.clone(),
                        id: m.id,
                        name: m.name,
                    }),
            );
        }
    }
    // ACP model values retain native IDs. Ambiguous IDs cannot select a provider.
    let mut counts = HashMap::new();
    for choice in &choices {
        *counts.entry(choice.id.clone()).or_insert(0) += 1;
    }
    choices.retain(|choice| counts[&choice.id] == 1);
    Ok(choices)
}

impl Config {
    pub(crate) async fn read(rpc: &Rpc, id: &str, history: &[Event]) -> Result<Self, Error> {
        #[derive(Deserialize)]
        struct Methods {
            methods: Vec<Method>,
        }
        #[derive(Deserialize)]
        struct Method {
            name: String,
        }
        let mut config = Self::default();
        for event in history {
            config.event(event);
        }
        if rpc.supports("session.methods") {
            let methods: Methods = rpc
                .typed("session.methods", json!({"sessionId":id}))
                .await?;
            config.methods = methods
                .methods
                .into_iter()
                .map(|method| method.name)
                .collect();
        }
        // Built-ins are advertised by hello, outside the contributed method table.
        config.methods.extend(
            [
                "session.permission.get",
                "session.permission.update",
                "session.model.update",
            ]
            .into_iter()
            .filter(|method| rpc.supports(method))
            .map(str::to_owned),
        );
        if config.methods.contains("session.permission.get")
            && config.methods.contains("session.permission.update")
        {
            config.policy = Some(
                rpc.typed(
                    "session.permission.get",
                    json!({"sessionId":id,"branchId":"main"}),
                )
                .await?,
            );
        }
        if config.methods.contains("session.model.update") && rpc.supports("provider.catalog") {
            config.models = models(rpc).await?;
        }
        Ok(config)
    }

    pub(crate) fn event(&mut self, event: &Event) {
        match &event.content {
            EventContent::Configuration(model) => self.model = Some(model.clone()),
            EventContent::PermissionChanged(data) => {
                if let Some(policy) = &mut self.policy {
                    policy.policy = data.policy.clone();
                }
            }
            _ => {}
        }
    }

    pub(crate) fn options(&self) -> Vec<Value> {
        let mut options = Vec::new();
        if let Some(policy) = &self.policy {
            options.push(json!({"id":"mode","name":"Permission mode","category":"mode","type":"select","currentValue":policy.policy.preset,"options":MODES.map(|mode| json!({"value":mode,"name":mode}))}));
        }
        if let Some(current) = &self.model
            && self
                .models
                .iter()
                .any(|model| model.id == current.model_id && model.provider == current.provider_id)
        {
            options.push(json!({"id":"model","name":"Model","category":"model","type":"select","currentValue":current.model_id,"options":self.models.iter().map(|model| json!({"value":model.id,"name":model.name})).collect::<Vec<_>>()}));
        }
        options
    }

    pub(crate) fn response(&self) -> Value {
        let mut value = json!({"configOptions":self.options()});
        if let Some(policy) = &self.policy {
            value["modes"] = json!({"currentModeId":policy.policy.preset,"availableModes":MODES.map(|mode|json!({"id":mode,"name":mode}))});
        }
        value
    }

    pub(crate) async fn set(
        mut self,
        rpc: &Rpc,
        id: &str,
        key: &str,
        value: &str,
    ) -> Result<Self, Error> {
        match key {
            "mode" if self.policy.is_some() && MODES.contains(&value) => {
                let current: Policy = rpc
                    .typed(
                        "session.permission.get",
                        json!({"sessionId":id,"branchId":"main"}),
                    )
                    .await?;
                let mut policy = current.policy;
                policy.preset =
                    serde_json::from_value(json!(value)).map_err(|_| Error::invalid_params())?;
                rpc.call("session.permission.update",json!({"sessionId":id,"branchId":"main","expectedVersion":current.version,"policy":policy})).await?;
                self.policy = Some(
                    rpc.typed(
                        "session.permission.get",
                        json!({"sessionId":id,"branchId":"main"}),
                    )
                    .await?,
                );
            }
            "model" => {
                #[derive(Deserialize)]
                struct Adopted {
                    model: ModelId,
                    outcome: String,
                }
                let model = self
                    .models
                    .iter()
                    .find(|model| model.id == value)
                    .ok_or_else(Error::invalid_params)?;
                let result: Adopted = rpc.typed("session.model.update",json!({"sessionId":id,"branchId":"main","providerId":model.provider,"modelId":model.id})).await?;
                if !matches!(result.outcome.as_str(), "adopted" | "unchanged") {
                    return Err(Error::internal_error());
                }
                self.model = Some(Configuration {
                    provider_id: result.model.provider_id,
                    model_id: result.model.model_id,
                    capability_snapshot: None,
                    model_preferences: None,
                });
                self.models = models(rpc).await?;
            }
            _ => return Err(Error::invalid_params()),
        }
        Ok(self)
    }
}
