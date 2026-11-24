use crate::rpc::Rpc;
use agent_client_protocol::{Error, schema::v1 as acp};
use dim_agent_types::ModelProtocol;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub(crate) const LIST: &str = "provider.routes.list";
pub(crate) const SET: &str = "provider.routes.set";
pub(crate) const DISABLE: &str = "provider.routes.disable";

pub(crate) fn supported(rpc: &Rpc) -> bool {
    [LIST, SET, DISABLE]
        .iter()
        .all(|method| rpc.supports(method))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Route {
    provider_id: String,
    protocol: ModelProtocol,
    base_url: Option<String>,
}

pub(crate) async fn list(rpc: &Rpc) -> Result<acp::ListProvidersResponse, Error> {
    let routes: Vec<Route> = rpc.typed(LIST, serde_json::json!({})).await?;
    let providers = routes
        .into_iter()
        .map(|row| {
            let api_type = match row.protocol {
                ModelProtocol::Anthropic => acp::LlmProtocol::Anthropic,
                ModelProtocol::OpenaiCompatible => acp::LlmProtocol::OpenAi,
                ModelProtocol::OpenaiResponses => {
                    acp::LlmProtocol::Other("openai-responses".into())
                }
                _ => return Err(Error::internal_error()),
            };
            Ok(acp::ProviderInfo::new(
                acp::ProviderId::new(row.provider_id),
                vec![api_type.clone()],
                false,
                row.base_url
                    .map(|url| acp::ProviderCurrentConfig::new(api_type, url)),
            ))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(acp::ListProvidersResponse::new(providers))
}

pub(crate) async fn set(
    rpc: &Rpc,
    request: acp::SetProviderRequest,
) -> Result<acp::SetProviderResponse, Error> {
    let api_format = match request.api_type {
        acp::LlmProtocol::Anthropic => ModelProtocol::Anthropic,
        acp::LlmProtocol::OpenAi => ModelProtocol::OpenaiCompatible,
        acp::LlmProtocol::Other(protocol) if protocol == "openai-responses" => {
            ModelProtocol::OpenaiResponses
        }
        _ => return Err(Error::invalid_params()),
    };
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Set {
        provider_id: String,
        connection: Connection,
    }
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Connection {
        api_format: ModelProtocol,
        base_url: String,
        headers: HashMap<String, String>,
    }
    let params = serde_json::to_value(Set {
        provider_id: request.provider_id.to_string(),
        connection: Connection {
            api_format,
            base_url: request.base_url,
            headers: request.headers,
        },
    })
    .map_err(|_| Error::internal_error())?;
    rpc.call(SET, params).await?;
    Ok(acp::SetProviderResponse::new())
}

pub(crate) async fn disable(
    rpc: &Rpc,
    request: acp::DisableProviderRequest,
) -> Result<bool, Error> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Target {
        provider_id: String,
    }
    let params = serde_json::to_value(Target {
        provider_id: request.provider_id.to_string(),
    })
    .map_err(|_| Error::internal_error())?;
    rpc.typed(DISABLE, params).await
}
