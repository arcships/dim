use super::ToolBinding;
use crate::port_faces::{request, unknown_method};
use dim_agent_types::{ToolDefinition, ToolSourceVersion};
use rutis_bridge::session::{Error, HostDispatch, Reply, Value, arg};
use serde::Deserialize;

const NAME: &str = "toolBinding";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    request_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Bind {
    request_id: String,
    through: u64,
    tools: Vec<ToolDefinition>,
    tool_sources: Vec<ToolSourceVersion>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BindCurrent {
    request_id: String,
    declared: Option<Vec<ToolDefinition>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Transfer {
    from: String,
    to: String,
}

impl HostDispatch for ToolBinding {
    fn methods(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "bind": "sync", "bindCurrent": "sync", "definitions": "sync",
            "transfer": "sync", "release": "sync"
        }))
    }

    fn invoke(&self, method: &str, args: Value) -> Reply {
        match method {
            "bind" => {
                let data: Bind = request(NAME, args)?;
                self.bind(
                    &data.request_id,
                    data.through,
                    &data.tools,
                    &data.tool_sources,
                )
                .map_err(Error::value)?;
                Ok(Value::Undefined)
            }
            "bindCurrent" => {
                let data: BindCurrent = request(NAME, args)?;
                arg(&self
                    .bind_current(&data.request_id, data.declared.as_deref())
                    .map_err(Error::value)?)
            }
            "definitions" => {
                let data: Request = request(NAME, args)?;
                arg(&self.definitions(&data.request_id).map_err(Error::value)?)
            }
            "transfer" => {
                let data: Transfer = request(NAME, args)?;
                self.transfer(&data.from, &data.to).map_err(Error::value)?;
                Ok(Value::Undefined)
            }
            "release" => {
                let data: Request = request(NAME, args)?;
                self.release(&data.request_id);
                Ok(Value::Undefined)
            }
            _ => Err(unknown_method(NAME, method)),
        }
    }
}
