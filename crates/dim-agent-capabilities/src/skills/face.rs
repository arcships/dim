use super::{SkillIssue, SkillsError};
use crate::port_faces::{Empty, request, unknown_method};
use rutis_bridge::session::{HostDispatch, Reply, Value, arg};
use serde::{Deserialize, Serialize};
use std::{future::Future, path::PathBuf, sync::Arc};

/// The four plugin-content operations of the public `skillsCatalog` face.
#[derive(Clone)]
pub struct SkillsCatalog(Arc<super::SkillsCatalog>);

impl SkillsCatalog {
    /// Binds an embedding host's existing catalog without registering or copying owner state.
    pub fn new(owner: Arc<super::SkillsCatalog>) -> Self {
        Self(owner)
    }

    /// Registers and scans a plugin root through its existing owner.
    pub fn register_plugin<'a>(
        &'a self,
        plugin: &'a str,
        root: PathBuf,
    ) -> impl Future<Output = Result<Vec<SkillIssue>, SkillsError>> + Send + 'a {
        self.0.register_plugin(plugin, root)
    }

    /// Withdraws the plugin's persisted root through its existing owner.
    pub fn withdraw_plugin<'a>(
        &'a self,
        plugin: &'a str,
    ) -> impl Future<Output = Result<(), SkillsError>> + Send + 'a {
        self.0.withdraw_plugin(plugin)
    }

    /// Reads the registered plugin IDs.
    pub fn plugins(&self) -> impl Future<Output = Result<Vec<String>, SkillsError>> + Send + '_ {
        self.0.plugins()
    }

    /// Reads the current scan issues for one registered plugin.
    pub fn plugin_issues<'a>(
        &'a self,
        plugin: &'a str,
    ) -> impl Future<Output = Result<Vec<SkillIssue>, SkillsError>> + Send + 'a {
        self.0.plugin_issues(plugin)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Register {
    plugin: String,
    root: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Plugin {
    plugin: String,
}

fn reply<T: Serialize>(result: Result<T, SkillsError>) -> Reply {
    match result {
        Ok(value) => arg(&value),
        Err(error) => Err(rutis_bridge::exports::failure(
            "skillsCatalog",
            error.code(),
            error.to_string(),
            None,
        )),
    }
}

impl HostDispatch for SkillsCatalog {
    fn methods(&self) -> Option<serde_json::Value> {
        Some(
            serde_json::json!({"register":"async","withdraw":"async","plugins":"async","issues":"async"}),
        )
    }

    fn invoke(&self, method: &str, args: Value) -> Reply {
        match method {
            "register" => {
                let Register { plugin, root } = request("skillsCatalog", args)?;
                let owner = self.clone();
                Ok(Value::future(async move {
                    reply(owner.register_plugin(&plugin, root.into()).await)
                }))
            }
            "withdraw" => {
                let Plugin { plugin } = request("skillsCatalog", args)?;
                let owner = self.clone();
                Ok(Value::future(async move {
                    reply(owner.withdraw_plugin(&plugin).await)
                }))
            }
            "plugins" => {
                let Empty {} = request("skillsCatalog", args)?;
                let owner = self.clone();
                Ok(Value::future(async move { reply(owner.plugins().await) }))
            }
            "issues" => {
                let Plugin { plugin } = request("skillsCatalog", args)?;
                let owner = self.clone();
                Ok(Value::future(async move {
                    reply(owner.plugin_issues(&plugin).await)
                }))
            }
            _ => Err(unknown_method("skillsCatalog", method)),
        }
    }
}
