use crate::rpc::Rpc;
use agent_client_protocol::{Error, schema::v1 as acp};
use dim_agent_types::{ModelId, PermissionPolicy, PermissionPreset, ROOT_BRANCH_ID};
use dim_client::wire::{AdoptedConfiguration, Configuration, Event, EventContent};
use serde::Deserialize;
use serde_json::json;
use std::collections::{HashMap, HashSet};

pub(crate) const MODES: [&str; 3] = ["read-only", "workspace-write", "full-access"];
/// A permission preset as the ACP mode id that names it: the same spelling
/// `MODES` lists and `session/set_mode` accepts.
pub(crate) fn mode(preset: PermissionPreset) -> &'static str {
    match preset {
        PermissionPreset::ReadOnly => "read-only",
        PermissionPreset::WorkspaceWrite => "workspace-write",
        PermissionPreset::FullAccess => "full-access",
    }
}
/// The `effort` value that says the Session sends no reasoning effort, so the
/// model's own default stays in force. It is not a native effort name, and
/// choosing it clears the Session's effort.
pub(crate) const PROVIDER_DEFAULT: &str = "provider-default";

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
/// The configuration part of a session response: the options every response
/// states, and the modes a Session with a permission policy has.
#[derive(Clone, Default)]
pub(crate) struct ConfigResponse {
    pub config_options: Vec<acp::SessionConfigOption>,
    pub modes: Option<acp::SessionModeState>,
}
#[derive(Default, Clone)]
pub(crate) struct Config {
    pub methods: HashSet<String>,
    pub usage_providers: HashSet<String>,
    pub policy: Option<Policy>,
    pub model: Option<Configuration>,
    pub models: Vec<Model>,
    /// Whether the Session offers `session.model.update` and the daemon
    /// advertises the selectable-model query, which only the product provider assembles.
    /// Effort choices come from the adopted declaration, so the option needs no
    /// catalog read, but a preference write needs a product provider.
    pub catalog: bool,
}

pub(crate) async fn models(rpc: &Rpc) -> Result<Vec<Model>, Error> {
    #[derive(Deserialize)]
    struct Page {
        models: Vec<Row>,
        next: Option<ModelId>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Row {
        provider_id: String,
        model: Entry,
    }
    #[derive(Deserialize)]
    struct Entry {
        id: String,
        name: String,
    }
    let mut choices = Vec::new();
    let mut after = None;
    loop {
        let mut params = json!({"selectableOnly":true,"limit":128});
        if let Some(cursor) = after {
            params["after"] = json!(cursor);
        }
        let page: Page = rpc.typed("provider.models.query", params).await?;
        choices.extend(page.models.into_iter().map(|row| Model {
            provider: row.provider_id,
            id: row.model.id,
            name: row.model.name,
        }));
        after = page.next;
        if after.is_none() {
            break;
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

/// The configuration `branch` adopted at `through`, or at its head when
/// omitted, with that line's own committed cutoff. Callers ask a server that
/// offers the read: one without it cannot name this line's configuration, and
/// the attach reply's is the root's (design §14).
pub(crate) async fn adopted(
    rpc: &Rpc,
    session_id: &str,
    branch: &str,
    through: Option<u64>,
) -> Result<(u64, Option<Configuration>), Error> {
    let mut params = json!({"sessionId":session_id,"branchId":branch});
    if let Some(through) = through {
        params["through"] = json!(through);
    }
    let reply: AdoptedConfiguration = rpc.typed("session.model.get", params).await?;
    if reply.branch_id != branch {
        return Err(Error::internal_error().data(json!({"code":"model_get_answered_another_line"})));
    }
    Ok((reply.through, reply.configuration.map(Configuration::from)))
}

impl Config {
    /// Reads the Session's configuration for `branch`, the line its
    /// conversation is on: its policy and its model are that line's (design §4).
    pub(crate) async fn read(
        rpc: &Rpc,
        id: &str,
        branch: &str,
        model: Option<Configuration>,
    ) -> Result<Self, Error> {
        let mut config = Self {
            model,
            ..Self::default()
        };
        if rpc.supports("session.methods") {
            let methods: dim_client::wire::MethodTable = rpc
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
                    json!({"sessionId":id,"branchId":branch}),
                )
                .await?,
            );
        }
        if config.methods.contains("session.model.update") && rpc.supports("provider.models.query")
        {
            config.catalog = true;
            config.models = models(rpc).await?;
        }
        if rpc.supports("account.providers") && rpc.supports("account.usage") {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Provider {
                provider_id: String,
            }
            let providers: Vec<Provider> = rpc.typed("account.providers", json!({})).await?;
            config.usage_providers = providers
                .into_iter()
                .map(|provider| provider.provider_id)
                .collect();
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

    /// The Session's configuration options and, when it has a permission
    /// policy, its modes: what `session/new`, `session/load`, `session/resume`
    /// and `session/set_config_option` answer, and what an update carries when
    /// the configuration changes.
    pub(crate) fn response(&self) -> ConfigResponse {
        let modes = self.policy.as_ref().map(|policy| {
            acp::SessionModeState::new(
                mode(policy.policy.preset),
                MODES
                    .iter()
                    .map(|mode| acp::SessionMode::new(*mode, *mode))
                    .collect(),
            )
        });
        ConfigResponse {
            config_options: self.options(),
            modes,
        }
    }

    pub(crate) fn options(&self) -> Vec<acp::SessionConfigOption> {
        fn choice(value: &str, name: &str) -> acp::SessionConfigSelectOption {
            acp::SessionConfigSelectOption::new(value.to_owned(), name.to_owned())
        }
        fn select(
            id: &str,
            name: &str,
            category: acp::SessionConfigOptionCategory,
            current: &str,
            choices: Vec<acp::SessionConfigSelectOption>,
        ) -> acp::SessionConfigOption {
            acp::SessionConfigOption::select(
                id.to_owned(),
                name.to_owned(),
                current.to_owned(),
                choices,
            )
            .category(category)
        }
        let mut options = Vec::new();
        if let Some(policy) = &self.policy {
            options.push(select(
                "mode",
                "Permission mode",
                acp::SessionConfigOptionCategory::Mode,
                mode(policy.policy.preset),
                MODES.iter().map(|mode| choice(mode, mode)).collect(),
            ));
        }
        if let Some(current) = &self.model
            && self.models.iter().any(|model| {
                model.id == current.model_id
                    && model.provider
                        == current
                            .catalog_provider_id
                            .as_deref()
                            .unwrap_or(&current.provider_id)
            })
        {
            options.push(select(
                "model",
                "Model",
                acp::SessionConfigOptionCategory::Model,
                &current.model_id,
                self.models
                    .iter()
                    .map(|model| choice(&model.id, &model.name))
                    .collect(),
            ));
        }
        if let Some((choices, current)) = self.effort() {
            let current = current.unwrap_or(PROVIDER_DEFAULT);
            let mut values: Vec<String> = std::iter::once(PROVIDER_DEFAULT.to_owned())
                .chain(choices)
                .collect();
            if !values.iter().any(|value| value == current) {
                values.push(current.to_owned());
            }
            options.push(select(
                "effort",
                "Reasoning effort",
                acp::SessionConfigOptionCategory::ThoughtLevel,
                current,
                values.iter().map(|value| choice(value, value)).collect(),
            ));
        }
        options
    }

    /// The effort choices the adopted model declares, and the effort the
    /// adopted configuration carries, which may be one the declaration does not
    /// list. A display-only value like that is shown so the option stays
    /// truthful; `set` validates against the declared choices alone.
    ///
    /// `None` when the model declares no reasoning effort, or when the Session
    /// cannot update it.
    fn effort(&self) -> Option<(Vec<String>, Option<&str>)> {
        let current = self.model.as_ref()?;
        if !self.catalog {
            return None;
        }
        let snapshot = current.capability_snapshot.as_ref()?;
        let reasoning = snapshot
            .reasoning
            .as_ref()
            .filter(|reasoning| reasoning.supported && snapshot.capabilities.reasoning)?;
        let choices = reasoning.offered_efforts();
        if choices.is_empty() {
            return None;
        }
        let adopted = current
            .model_preferences
            .as_ref()
            .and_then(|preferences| preferences.reasoning_effort.as_deref());
        Some((choices, adopted))
    }

    pub(crate) async fn recover_model(rpc: &Rpc, id: &str, value: &str) -> Result<Self, Error> {
        let branch = if rpc.supports("session.branches") {
            let directory: dim_client::wire::BranchDirectory = rpc
                .typed("session.branches", json!({"sessionId":id}))
                .await?;
            directory
                .default_branch_id
                .unwrap_or_else(|| ROOT_BRANCH_ID.to_owned())
        } else {
            ROOT_BRANCH_ID.to_owned()
        };
        let mut config = Self::read(rpc, id, &branch, None).await?;
        // `session.attach` checks its route against the root and `session.submit`
        // against the line it targets: restore an expired route on both.
        let root = config.set_model(rpc, id, ROOT_BRANCH_ID, value, true).await;
        if branch == ROOT_BRANCH_ID {
            root?;
        } else {
            let line = config.set_model(rpc, id, &branch, value, true).await;
            match (root, line) {
                (Ok(()), Ok(())) => {}
                (Ok(()), Err(error)) | (Err(error), Ok(()))
                    if error.data.as_ref().is_some_and(|data| {
                        data["nativeAppError"] == "provider_route_not_expired"
                    }) => {}
                (Err(error), _) => return Err(error),
                (_, Err(error)) => return Err(error),
            }
        }
        let (_, current) = adopted(rpc, id, &branch, None).await?;
        config.model = Some(current.ok_or_else(Error::internal_error)?);
        Ok(config)
    }

    async fn set_model(
        &mut self,
        rpc: &Rpc,
        id: &str,
        branch: &str,
        value: &str,
        require_expired_route: bool,
    ) -> Result<(), Error> {
        #[derive(Deserialize)]
        struct Adopted {
            model: ModelId,
            outcome: String,
        }
        #[derive(serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Select<'a> {
            session_id: &'a str,
            branch_id: &'a str,
            provider_id: &'a str,
            model_id: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            require_expired_route: Option<bool>,
        }
        let model = self
            .models
            .iter()
            .find(|model| model.id == value)
            .ok_or_else(Error::invalid_params)?;
        let params = serde_json::to_value(Select {
            session_id: id,
            branch_id: branch,
            provider_id: &model.provider,
            model_id: &model.id,
            require_expired_route: require_expired_route.then_some(true),
        })
        .map_err(|_| Error::internal_error())?;
        let result: Adopted = rpc.typed("session.model.update", params).await?;
        if !matches!(result.outcome.as_str(), "adopted" | "unchanged") {
            return Err(Error::internal_error());
        }
        self.model = Some(Configuration {
            catalog_provider_id: (model.provider != result.model.provider_id)
                .then(|| model.provider.clone()),
            provider_id: result.model.provider_id,
            model_id: result.model.model_id,
            capability_snapshot: None,
            model_preferences: None,
        });
        Ok(())
    }

    pub(crate) async fn set(
        mut self,
        rpc: &Rpc,
        id: &str,
        branch: &str,
        key: &str,
        value: &str,
    ) -> Result<Self, Error> {
        match key {
            "mode" if self.policy.is_some() && MODES.contains(&value) => {
                let current: Policy = rpc
                    .typed(
                        "session.permission.get",
                        json!({"sessionId":id,"branchId":branch}),
                    )
                    .await?;
                let mut policy = current.policy;
                policy.preset =
                    serde_json::from_value(json!(value)).map_err(|_| Error::invalid_params())?;
                rpc.call("session.permission.update",json!({"sessionId":id,"branchId":branch,"expectedVersion":current.version,"policy":policy})).await?;
                self.policy = Some(
                    rpc.typed(
                        "session.permission.get",
                        json!({"sessionId":id,"branchId":branch}),
                    )
                    .await?,
                );
            }
            "effort" => {
                #[derive(Deserialize)]
                struct Adopted {
                    outcome: String,
                }
                let Some((choices, _)) = self.effort() else {
                    return Err(Error::invalid_params());
                };
                let patch = if value == PROVIDER_DEFAULT {
                    json!({"action":"clear"})
                } else if choices.iter().any(|choice| choice == value) {
                    json!(value)
                } else {
                    return Err(Error::invalid_params());
                };
                let result: Adopted = rpc
                    .typed(
                        "session.model.update",
                        json!({"sessionId":id,"branchId":branch,"modelPreferences":{"reasoningEffort":patch}}),
                    )
                    .await?;
                if !matches!(result.outcome.as_str(), "adopted" | "unchanged") {
                    return Err(Error::internal_error());
                }
            }
            "model" => {
                self.set_model(rpc, id, branch, value, false).await?;
                self.models = models(rpc).await?;
            }
            _ => return Err(Error::invalid_params()),
        }
        Ok(self)
    }
}
