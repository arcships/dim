use super::{McpPluginRefusal, Result, management};
use rutis_bridge::session::{Error, HostDispatch, Reply, Value, arg};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{future::Future, path::PathBuf, sync::Arc};
use tokio_util::sync::CancellationToken;

/// The four plugin-content operations of the public `mcpService` face.
#[derive(Clone)]
pub struct McpService(pub(super) Arc<super::McpService>);

impl McpService {
    /// Registers one complete document using the MCP owner's existing validation and settlement.
    pub fn register_plugin<'a>(
        &'a self,
        plugin: String,
        dir: PathBuf,
        document: serde_json::Value,
        cancel: &'a CancellationToken,
    ) -> impl Future<Output = Result<()>> + Send + 'a {
        self.0.register_plugin(plugin, dir, document, cancel)
    }

    /// Withdraws a plugin's persisted document through the same owner.
    pub fn withdraw_plugin<'a>(
        &'a self,
        plugin: String,
        cancel: &'a CancellationToken,
    ) -> impl Future<Output = Result<()>> + Send + 'a {
        self.0.withdraw_plugin(plugin, cancel)
    }

    /// Reads the registered plugin IDs.
    pub fn plugins<'a>(
        &'a self,
        cancel: &'a CancellationToken,
    ) -> impl Future<Output = Result<Vec<String>>> + Send + 'a {
        self.0.plugins(cancel)
    }

    /// Reads the last refused registration's message and stable reason.
    pub fn plugin_refusal(&self, plugin: &str) -> Option<McpPluginRefusal> {
        self.0.plugin_refusal(plugin)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Register {
    plugin: String,
    dir: String,
    document: serde_json::Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Plugin {
    plugin: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

fn request<T: DeserializeOwned>(args: Value) -> std::result::Result<T, Error> {
    let invalid = || {
        rutis_bridge::exports::failure(
            "mcpService",
            "invalid_request",
            "expected one mcpService request object".into(),
            None,
        )
    };
    let args = args.list().map_err(|_| invalid())?;
    let [value] = args.as_slice() else {
        return Err(invalid());
    };
    let value = value.clone().json().map_err(|_| invalid())?;
    if !value.is_object() {
        return Err(invalid());
    }
    serde_json::from_value(value).map_err(|_| invalid())
}

fn reply<T: Serialize>(result: Result<T>) -> Reply {
    match result {
        Ok(value) => arg(&value),
        Err(error) => {
            let error = management::error(error);
            let data = error.data.expect("MCP errors have an application code");
            Err(rutis_bridge::exports::failure(
                "mcpService",
                &data.app_error,
                error.message,
                None,
            ))
        }
    }
}

impl HostDispatch for McpService {
    fn methods(&self) -> Option<serde_json::Value> {
        Some(
            serde_json::json!({"register":"async","withdraw":"async","plugins":"async","refusal":"sync"}),
        )
    }

    fn invoke(&self, method: &str, args: Value) -> Reply {
        match method {
            "register" => {
                let Register {
                    plugin,
                    dir,
                    document,
                } = request(args)?;
                let owner = self.clone();
                let cancel = CancellationToken::new();
                Ok(Value::future(async move {
                    let _cancel = cancel.clone().drop_guard();
                    reply(
                        owner
                            .register_plugin(plugin, dir.into(), document, &cancel)
                            .await,
                    )
                }))
            }
            "withdraw" => {
                let Plugin { plugin } = request(args)?;
                let owner = self.clone();
                let cancel = CancellationToken::new();
                Ok(Value::future(async move {
                    let _cancel = cancel.clone().drop_guard();
                    reply(owner.withdraw_plugin(plugin, &cancel).await)
                }))
            }
            "plugins" => {
                let Empty {} = request(args)?;
                let owner = self.clone();
                let cancel = CancellationToken::new();
                Ok(Value::future(async move {
                    let _cancel = cancel.clone().drop_guard();
                    reply(owner.plugins(&cancel).await)
                }))
            }
            "refusal" => {
                let Plugin { plugin } = request(args)?;
                arg(&self.plugin_refusal(&plugin))
            }
            _ => Err(Error::value(format!("mcpService has no method {method}"))),
        }
    }
}
