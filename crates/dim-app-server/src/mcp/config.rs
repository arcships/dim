//! Typed MCP documents and the effective configuration of each entry.
use crate::settings::{BusinessDocument, Result, Settings, SettingsError, SettingsScope};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    path::{Path, PathBuf},
};

/// Wakes the returned receiver on every committed write to MCP's Settings
/// sources and on every newly authorized project. The listeners belong to
/// `ctx` and end with it; register before the first read of the sources.
pub(super) fn settings_changes(
    ctx: &rutis::Ctx,
) -> std::result::Result<tokio::sync::watch::Receiver<()>, rutis::Error> {
    let wake = std::sync::Arc::new(tokio::sync::watch::Sender::new(()));
    for source in [
        McpSettings::NAMESPACE,
        McpProjectEnablement::NAMESPACE,
        McpPluginServers::NAMESPACE,
        McpSecrets::NAMESPACE,
    ] {
        let wake = wake.clone();
        ctx.listen::<crate::settings::SettingsChanged>()
            .name(crate::settings::settings_changes(source))
            .on(move |_, _| {
                wake.send_replace(());
                async { Ok(None) }
            })?;
    }
    let authorized = wake.clone();
    ctx.listen::<crate::settings::ProjectAuthorized>()
        .name(crate::settings::project_authorizations())
        .on(move |_, _| {
            authorized.send_replace(());
            async { Ok(None) }
        })?;
    Ok(wake.subscribe())
}

/// Why one entry did not decode. It names the entry and the field and states
/// the expected kind; it never repeats the value, which may hold a credential.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpEntryError {
    pub id: String,
    pub field: Option<String>,
    pub expected: String,
}
impl fmt::Display for McpEntryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let id = if self.id.is_empty() {
            String::new()
        } else {
            format!(" {}", self.id)
        };
        match &self.field {
            Some(field) => write!(
                formatter,
                "MCP server{id}: field {} must be {}",
                field, self.expected
            ),
            None => write!(formatter, "MCP server{id}: expected {}", self.expected),
        }
    }
}
impl std::error::Error for McpEntryError {}

/// Every entry under `mcpServers`. An entry that does not decode is kept out
/// of `mcp_servers` and its error is listed in `invalid`; `raw` holds each
/// entry's value in document order so a write can put every untouched entry
/// back as the file wrote it.
#[derive(Clone, Default, PartialEq)]
pub struct McpSettings {
    mcp_servers: BTreeMap<String, McpServerSettings>,
    invalid: BTreeMap<String, McpEntryError>,
    raw: Map<String, Value>,
    dirty: BTreeSet<String>,
    pub extra: BTreeMap<String, Value>,
}

/// A document with a server entry whose id is empty: it names no entry a
/// management call could address, so the whole document is refused.
pub(crate) const EMPTY_ENTRY_ID: SettingsError =
    SettingsError::shape("MCP server entry id is empty", None);

impl McpSettings {
    /// Replaces an entry. Only this entry is serialised from its typed value
    /// on the next write.
    pub fn insert(&mut self, id: impl Into<String>, configuration: McpServerSettings) {
        let id = id.into();
        self.invalid.remove(&id);
        self.mcp_servers.insert(id.clone(), configuration);
        self.dirty.insert(id);
    }

    /// Removes a decoded or a broken entry. Only this entry's value is
    /// dropped from the document; the other ids keep their order.
    pub fn remove(&mut self, id: &str) -> Option<McpServerSettings> {
        self.invalid.remove(id);
        self.raw.shift_remove(id);
        self.dirty.insert(id.to_owned());
        self.mcp_servers.remove(id)
    }

    /// A mutable decoded entry, marked for serialisation from its typed value.
    pub fn configure_mut(&mut self, id: &str) -> Option<&mut McpServerSettings> {
        if self.mcp_servers.contains_key(id) {
            self.dirty.insert(id.to_owned());
        }
        self.mcp_servers.get_mut(id)
    }

    pub fn invalid_error(&self, id: &str) -> Option<&McpEntryError> {
        self.invalid.get(id)
    }

    /// The decoded entries, by id.
    pub fn entries(&self) -> &BTreeMap<String, McpServerSettings> {
        &self.mcp_servers
    }

    /// One decoded entry, by id.
    pub fn get(&self, id: &str) -> Option<&McpServerSettings> {
        self.mcp_servers.get(id)
    }

    /// Every entry's value as the document wrote it, decoded or broken.
    pub(crate) fn raw_entries(&self) -> &Map<String, Value> {
        &self.raw
    }

    /// Whether `id` names a decoded entry.
    pub fn contains(&self, id: &str) -> bool {
        self.mcp_servers.contains_key(id)
    }

    /// Entries that did not decode, by id, holding only their errors.
    pub fn invalid_entries(&self) -> &BTreeMap<String, McpEntryError> {
        &self.invalid
    }

    /// Consumes the document into its decoded entries and its broken entries.
    pub fn into_parts(
        self,
    ) -> (
        BTreeMap<String, McpServerSettings>,
        BTreeMap<String, McpEntryError>,
    ) {
        (self.mcp_servers, self.invalid)
    }

    /// Decodes each entry on its own. An id that is empty names no entry that
    /// any management call could address, so it fails the whole document.
    fn from_entries(entries: Map<String, Value>, extra: BTreeMap<String, Value>) -> Result<Self> {
        if entries.keys().any(|id| id.trim().is_empty()) {
            return Err(EMPTY_ENTRY_ID);
        }
        let mut document = Self {
            extra,
            ..Self::default()
        };
        for (id, value) in entries {
            match decode_entry(&id, &value) {
                Ok(configuration) => {
                    document.mcp_servers.insert(id.clone(), configuration);
                }
                Err(error) => {
                    document.invalid.insert(id.clone(), error);
                }
            }
            document.raw.insert(id, value);
        }
        Ok(document)
    }

    /// Decodes a document that is already parsed, as an RPC request carries
    /// it.
    pub fn from_value(value: Value) -> Result<Self> {
        let object = match value {
            Value::Object(object) => object,
            _ => {
                return Err(SettingsError::shape(
                    "MCP document must be a JSON object",
                    None,
                ));
            }
        };
        let servers = match object.get("mcpServers") {
            Some(Value::Object(servers)) => servers.clone(),
            Some(_) => {
                return Err(SettingsError::shape(
                    "mcpServers must be a JSON object",
                    None,
                ));
            }
            None => {
                return Err(SettingsError::shape(
                    "MCP document must contain an mcpServers object",
                    None,
                ));
            }
        };
        let extra = object
            .into_iter()
            .filter(|(key, _)| key != "mcpServers")
            .collect();
        Self::from_entries(servers, extra)
    }

    /// Decodes each entry on its own. Only a file that is not JSON and an
    /// `mcpServers` that is not an object fail the whole document.
    pub fn from_file(bytes: &[u8]) -> Result<Self> {
        let value: Value = serde_json::from_slice(bytes).map_err(|error| {
            SettingsError::shape(
                "MCP document is not valid JSON",
                Some((error.line(), error.column())),
            )
        })?;
        let object = match value.as_object() {
            Some(object) => object,
            None => {
                let position = serde_json::from_slice::<BTreeMap<String, Value>>(bytes)
                    .err()
                    .map(|error| (error.line(), error.column()));
                return Err(SettingsError::shape(
                    "MCP document must be a JSON object",
                    position,
                ));
            }
        };
        let servers = match object.get("mcpServers") {
            Some(Value::Object(servers)) => servers,
            Some(_) => {
                let position = serde_json::from_slice::<RequiredServers>(bytes)
                    .err()
                    .map(|error| (error.line(), error.column()));
                return Err(SettingsError::shape(
                    "mcpServers must be a JSON object",
                    position,
                ));
            }
            None => {
                let position = serde_json::from_slice::<RequiredServers>(bytes)
                    .err()
                    .map(|error| (error.line(), error.column()));
                return Err(SettingsError::shape(
                    "MCP document must contain an mcpServers object",
                    position,
                ));
            }
        };
        let extra = object
            .iter()
            .filter(|(key, _)| key.as_str() != "mcpServers")
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        Self::from_entries(servers.clone(), extra)
    }
}
impl Serialize for McpSettings {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::{Error as _, SerializeMap};
        let mut map = serializer.serialize_map(Some(1 + self.extra.len()))?;
        let mut servers = Map::<String, Value>::new();
        for (id, raw) in &self.raw {
            if let Some(configuration) = self.mcp_servers.get(id) {
                let value = if self.dirty.contains(id) {
                    serde_json::to_value(configuration).map_err(S::Error::custom)?
                } else {
                    raw.clone()
                };
                servers.insert(id.clone(), value);
            } else if self.invalid.contains_key(id) {
                servers.insert(id.clone(), raw.clone());
            }
        }
        for (id, configuration) in &self.mcp_servers {
            if !self.raw.contains_key(id) {
                servers.insert(
                    id.clone(),
                    serde_json::to_value(configuration).map_err(S::Error::custom)?,
                );
            }
        }
        map.serialize_entry("mcpServers", &servers)?;
        for (key, value) in &self.extra {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}
impl<'de> Deserialize<'de> for McpSettings {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            #[serde(rename = "mcpServers")]
            mcp_servers: Map<String, Value>,
            #[serde(flatten)]
            extra: BTreeMap<String, Value>,
        }
        use serde::de::Error as _;
        let raw = Raw::deserialize(deserializer)?;
        Self::from_entries(raw.mcp_servers, raw.extra).map_err(D::Error::custom)
    }
}
#[derive(Deserialize)]
struct RequiredServers {
    #[serde(rename = "mcpServers")]
    _mcp_servers: BTreeMap<String, Value>,
}

enum Transport {
    Stdio,
    Http,
    Sse,
}

fn entry_error(id: &str, field: Option<&str>, expected: &str) -> McpEntryError {
    McpEntryError {
        id: id.to_owned(),
        field: field.map(str::to_owned),
        expected: expected.to_owned(),
    }
}

/// Decodes one entry. The error names the entry and the field and never
/// repeats the value.
pub fn decode_entry(
    id: &str,
    value: &Value,
) -> std::result::Result<McpServerSettings, McpEntryError> {
    if id.trim().is_empty() {
        return Err(entry_error(id, None, "a nonempty id"));
    }
    decode_value(Some(id), value)
}

/// The one decoder. Every optional member treats `null` as absent; the
/// required `command` and `url` do not.
fn decode_value(
    id: Option<&str>,
    value: &Value,
) -> std::result::Result<McpServerSettings, McpEntryError> {
    let id = id.unwrap_or_default();
    let entry = value
        .as_object()
        .ok_or_else(|| entry_error(id, None, "a JSON object"))?;
    let transport = match entry.get("type") {
        Some(Value::String(kind)) => match kind.as_str() {
            "stdio" => Transport::Stdio,
            "http" | "streamable-http" => Transport::Http,
            "sse" => Transport::Sse,
            "ws" => {
                return Err(entry_error(
                    id,
                    Some("type"),
                    "http or sse; ws is not a supported transport",
                ));
            }
            _ => return Err(entry_error(id, Some("type"), "stdio, http or sse")),
        },
        Some(Value::Null) | None if entry.contains_key("command") => Transport::Stdio,
        Some(Value::Null) | None if entry.contains_key("url") => {
            return Err(entry_error(
                id,
                Some("type"),
                "http or sse; an entry with a url must name its type",
            ));
        }
        Some(Value::Null) | None => {
            return Err(entry_error(
                id,
                Some("type"),
                "stdio, http or sse, or a command for stdio",
            ));
        }
        Some(_) => return Err(entry_error(id, Some("type"), "a string")),
    };
    if entry.contains_key("$generation") {
        return Err(entry_error(id, Some("$generation"), "not a reserved field"));
    }
    match transport {
        Transport::Stdio => decode_stdio(id, entry),
        Transport::Http => decode_remote(id, entry, RemoteTransport::Http),
        Transport::Sse => decode_remote(id, entry, RemoteTransport::Sse),
    }
}

fn field_string(
    entry: &Map<String, Value>,
    id: &str,
    field: &str,
    expected: &str,
) -> std::result::Result<Option<String>, McpEntryError> {
    match entry.get(field) {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(entry_error(id, Some(field), expected)),
    }
}

fn string_map(
    entry: &Map<String, Value>,
    id: &str,
    field: &str,
) -> std::result::Result<BTreeMap<String, String>, McpEntryError> {
    match entry.get(field) {
        None | Some(Value::Null) => Ok(BTreeMap::new()),
        Some(Value::Object(values)) => values
            .iter()
            .map(|(name, value)| {
                value
                    .as_str()
                    .map(|value| (name.clone(), value.to_owned()))
                    .ok_or_else(|| entry_error(id, Some(&format!("{field}.{name}")), "a string"))
            })
            .collect(),
        Some(_) => Err(entry_error(id, Some(field), "an object of strings")),
    }
}

fn field_enabled(
    entry: &Map<String, Value>,
    id: &str,
) -> std::result::Result<Option<bool>, McpEntryError> {
    match entry.get("enabled") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(entry_error(id, Some("enabled"), "a boolean")),
    }
}

/// The range an entry's `timeout` takes: one second to one day, in
/// milliseconds.
const MIN_TIMEOUT_MILLIS: u64 = 1_000;
const MAX_TIMEOUT_MILLIS: u64 = 86_400_000;

/// The members the decoder knows, for either transport. An overlay may not add
/// one of these: an unknown name there is the user's extension field, but a
/// known name would be written back as the entry's own member
/// (`merge_secret`).
pub(crate) const ENTRY_MEMBERS: &[&str] = &[
    "type", "command", "args", "env", "cwd", "enabled", "timeout", "url", "headers", "auth",
    "oauth",
];

/// The members of `oauth` the decoder reads. Any other name is an extension
/// field, kept in `McpOAuthSettings::extra` and unused.
pub(crate) const OAUTH_MEMBERS: &[&str] = &[
    "clientId",
    "callbackPort",
    "authServerMetadataUrl",
    "scopes",
];

fn field_timeout(
    entry: &Map<String, Value>,
    id: &str,
) -> std::result::Result<Option<u64>, McpEntryError> {
    const EXPECTED: &str = "an integer number of milliseconds from 1000 to 86400000";
    let Some(value) = entry.get("timeout") else {
        return Ok(None);
    };
    match value
        .as_u64()
        .filter(|millis| (MIN_TIMEOUT_MILLIS..=MAX_TIMEOUT_MILLIS).contains(millis))
    {
        Some(millis) => Ok(Some(millis)),
        None => Err(entry_error(id, Some("timeout"), EXPECTED)),
    }
}

fn decode_stdio(
    id: &str,
    entry: &Map<String, Value>,
) -> std::result::Result<McpServerSettings, McpEntryError> {
    let command = match field_string(entry, id, "command", "a nonempty string")? {
        Some(command) if !command.trim().is_empty() => command,
        _ => return Err(entry_error(id, Some("command"), "a nonempty string")),
    };
    let args = match entry.get("args") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(values)) => values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| entry_error(id, Some(&format!("args[{index}]")), "a string"))
            })
            .collect::<std::result::Result<Vec<_>, _>>()?,
        Some(_) => return Err(entry_error(id, Some("args"), "an array of strings")),
    };
    let env = string_map(entry, id, "env")?;
    let cwd = match entry.get("cwd") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => Some(value.clone()),
        Some(_) => {
            return Err(entry_error(id, Some("cwd"), "a string or null"));
        }
    };
    let enabled = field_enabled(entry, id)?;
    let timeout = field_timeout(entry, id)?;
    for member in ["url", "headers", "auth", "oauth"] {
        if entry.get(member).is_some_and(|value| !value.is_null()) {
            return Err(entry_error(
                id,
                Some(member),
                "not a member of the stdio transport",
            ));
        }
    }
    let extra = entry
        .iter()
        .filter(|(key, _)| !ENTRY_MEMBERS.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    Ok(McpServerSettings::Stdio {
        kind: matches!(entry.get("type"), Some(Value::String(_))).then_some(Stdio::Stdio),
        command,
        args,
        env,
        cwd,
        enabled,
        timeout,
        extra,
    })
}

fn decode_remote(
    id: &str,
    entry: &Map<String, Value>,
    kind: RemoteTransport,
) -> std::result::Result<McpServerSettings, McpEntryError> {
    let url = match field_string(entry, id, "url", "a nonempty string")? {
        Some(url) if !url.trim().is_empty() => url,
        _ => return Err(entry_error(id, Some("url"), "a nonempty string")),
    };
    let headers = string_map(entry, id, "headers")?;
    let auth = decode_auth(id, entry)?;
    let oauth = decode_oauth(id, entry)?;
    if oauth.is_some() {
        if auth.is_some() {
            return Err(entry_error(
                id,
                Some("oauth"),
                "absent when a bearer token is set",
            ));
        }
        if headers
            .keys()
            .any(|name| name.eq_ignore_ascii_case("authorization"))
        {
            return Err(entry_error(
                id,
                Some("oauth"),
                "absent when an Authorization header is set",
            ));
        }
    }
    let enabled = field_enabled(entry, id)?;
    let timeout = field_timeout(entry, id)?;
    for member in ["command", "args", "env", "cwd"] {
        if entry.get(member).is_some_and(|value| !value.is_null()) {
            return Err(entry_error(
                id,
                Some(member),
                "not a member of the remote transport",
            ));
        }
    }
    let extra = entry
        .iter()
        .filter(|(key, _)| !ENTRY_MEMBERS.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    Ok(McpServerSettings::Remote {
        kind,
        url,
        headers,
        auth,
        oauth,
        enabled,
        timeout,
        extra,
    })
}

fn decode_auth(
    id: &str,
    entry: &Map<String, Value>,
) -> std::result::Result<Option<BearerAuth>, McpEntryError> {
    let Some(value) = entry.get("auth").filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let auth = value
        .as_object()
        .ok_or_else(|| entry_error(id, Some("auth"), "an object"))?;
    match auth.get("type") {
        Some(Value::String(kind)) if kind == "bearer" => {}
        Some(Value::String(_)) => return Err(entry_error(id, Some("auth.type"), "bearer")),
        Some(_) => return Err(entry_error(id, Some("auth.type"), "a string")),
        None => return Err(entry_error(id, Some("auth.type"), "bearer")),
    }
    let token = auth
        .get("token")
        .and_then(Value::as_str)
        .filter(|token| !token.trim().is_empty())
        .ok_or_else(|| entry_error(id, Some("auth.token"), "a nonempty string"))?;
    Ok(Some(BearerAuth::Bearer {
        token: token.to_owned(),
    }))
}

fn decode_oauth(
    id: &str,
    entry: &Map<String, Value>,
) -> std::result::Result<Option<Box<McpOAuthSettings>>, McpEntryError> {
    let Some(value) = entry.get("oauth").filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let oauth = value
        .as_object()
        .ok_or_else(|| entry_error(id, Some("oauth"), "an object"))?;
    let client_id = match oauth.get("clientId") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) if !value.trim().is_empty() => Some(value.clone()),
        Some(_) => {
            return Err(entry_error(
                id,
                Some("oauth.clientId"),
                "a nonempty string or null",
            ));
        }
    };
    let callback_port = match oauth.get("callbackPort") {
        None | Some(Value::Null) => None,
        Some(Value::Number(value)) => match value.as_u64() {
            Some(port) if (1..=65535).contains(&port) => Some(port as u16),
            _ => {
                return Err(entry_error(
                    id,
                    Some("oauth.callbackPort"),
                    "an integer from 1 to 65535 or null",
                ));
            }
        },
        Some(_) => {
            return Err(entry_error(
                id,
                Some("oauth.callbackPort"),
                "an integer from 1 to 65535 or null",
            ));
        }
    };
    let auth_server_metadata_url = match oauth.get("authServerMetadataUrl") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) if valid_metadata_url(value) => Some(value.clone()),
        Some(_) => {
            return Err(entry_error(
                id,
                Some("oauth.authServerMetadataUrl"),
                "an https URL or a loopback http URL",
            ));
        }
    };
    let scopes = match oauth.get("scopes") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) if valid_scopes(value) => Some(value.clone()),
        Some(_) => {
            return Err(entry_error(
                id,
                Some("oauth.scopes"),
                "space-separated RFC 6749 scope tokens",
            ));
        }
    };
    let extra = oauth
        .iter()
        .filter(|(key, _)| !OAUTH_MEMBERS.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    Ok(Some(Box::new(McpOAuthSettings {
        client_id,
        callback_port,
        auth_server_metadata_url,
        scopes,
        extra,
    })))
}

fn valid_metadata_url(value: &str) -> bool {
    url::Url::parse(value).is_ok_and(|url| {
        url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
            && (url.scheme() == "https"
                || (url.scheme() == "http" && matches!(url.host(), Some(host) if loopback(&host))))
    })
}

fn valid_scopes(scopes: &str) -> bool {
    !scopes.trim().is_empty()
        && scopes
            .bytes()
            .all(|byte| byte == b' ' || (byte.is_ascii_graphic() && byte != b'"' && byte != b'\\'))
}

#[derive(Clone, Serialize, PartialEq)]
#[serde(untagged)]
pub enum McpServerSettings {
    Stdio {
        #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
        kind: Option<Stdio>,
        command: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        env: BTreeMap<String, String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        enabled: Option<bool>,
        /// The entry's `tools/call` limit in milliseconds; absent means the
        /// default.
        #[serde(skip_serializing_if = "Option::is_none")]
        timeout: Option<u64>,
        #[serde(flatten)]
        extra: BTreeMap<String, serde_json::Value>,
    },
    Remote {
        #[serde(rename = "type")]
        kind: RemoteTransport,
        url: String,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        headers: BTreeMap<String, String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        auth: Option<BearerAuth>,
        #[serde(skip_serializing_if = "Option::is_none")]
        oauth: Option<Box<McpOAuthSettings>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        enabled: Option<bool>,
        /// The entry's `tools/call` limit in milliseconds; absent means the
        /// default.
        #[serde(skip_serializing_if = "Option::is_none")]
        timeout: Option<u64>,
        #[serde(flatten)]
        extra: BTreeMap<String, serde_json::Value>,
    },
}
impl<'de> Deserialize<'de> for McpServerSettings {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        use serde::de::Error as _;
        let value = Value::deserialize(deserializer)?;
        decode_value(None, &value).map_err(D::Error::custom)
    }
}
impl McpServerSettings {
    /// Re-reads a typed entry through the one decoder. A typed value that an
    /// edit or a programmatic caller changed decodes again before it is
    /// written, so an invalid combination cannot persist.
    pub fn canonical(&self, id: &str) -> std::result::Result<Self, McpEntryError> {
        let value =
            serde_json::to_value(self).map_err(|_| entry_error(id, None, "a valid entry"))?;
        decode_entry(id, &value)
    }

    /// The enablement an entry's own field states; a project entry ignores it
    /// (`config::effective`).
    pub fn enabled(&self) -> bool {
        match self {
            Self::Stdio { enabled, .. } | Self::Remote { enabled, .. } => enabled.unwrap_or(true),
        }
    }

    /// The entry's `tools/call` limit in milliseconds; absent means the
    /// default.
    pub fn timeout(&self) -> Option<u64> {
        match self {
            Self::Stdio { timeout, .. } | Self::Remote { timeout, .. } => *timeout,
        }
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Stdio {
    Stdio,
}
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum RemoteTransport {
    Http,
    Sse,
}
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum BearerAuth {
    Bearer { token: String },
}

/// A remote entry's OAuth settings, with the field names and types of
/// Claude Code's `.mcp.json`. They carry no secret.
#[derive(Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct McpOAuthSettings {
    /// A pre-registered client (registration priority 1).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// Port of the redirect `http://localhost:{port}/callback`; default 1456.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callback_port: Option<u16>,
    /// Authorization server metadata used only when the server publishes no
    /// valid Protected Resource Metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_server_metadata_url: Option<String>,
    /// Space-separated scopes requested as they are.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scopes: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}
pub(crate) fn loopback(host: &url::Host<&str>) -> bool {
    match host {
        url::Host::Domain(host) => host.eq_ignore_ascii_case("localhost"),
        url::Host::Ipv4(ip) => ip.is_loopback(),
        url::Host::Ipv6(ip) => ip.is_loopback(),
    }
}

/// The hidden part of a project entry moved from user scope, keyed by
/// canonical workspace and server id. It never enters a project file and no
/// client method returns it.
#[derive(Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpSecrets {
    #[serde(default)]
    pub entries: BTreeMap<String, BTreeMap<String, McpSecretEntry>>,
}
impl McpSecrets {
    /// The records one workspace withholds, by id: the overlay a project
    /// source's entries and its revision token are computed with.
    pub(crate) fn overlays(&self, workspace: &str) -> BTreeMap<String, McpSecretEntry> {
        self.entries.get(workspace).cloned().unwrap_or_default()
    }
    pub(crate) fn get(&self, workspace: &str, id: &str) -> Option<&McpSecretEntry> {
        self.entries
            .get(workspace)
            .and_then(|servers| servers.get(id))
    }
    pub(crate) fn set(&mut self, workspace: &str, id: &str, secret: McpSecretEntry) {
        self.entries
            .entry(workspace.to_owned())
            .or_default()
            .insert(id.to_owned(), secret);
    }
    pub(crate) fn remove(&mut self, workspace: &str, id: &str) -> bool {
        let Some(servers) = self.entries.get_mut(workspace) else {
            return false;
        };
        let removed = servers.remove(id);
        if servers.is_empty() {
            self.entries.remove(workspace);
        }
        removed.is_some()
    }
}
impl BusinessDocument for McpSecrets {
    const NAMESPACE: &'static str = "mcp-secrets";
    const USER_FILE: &'static str = "mcp-secrets.json";

    fn validate(&self) -> Result<()> {
        for (workspace, servers) in &self.entries {
            if !Path::new(workspace).is_absolute() || workspace.is_empty() {
                return Err(SettingsError::Invalid("invalid MCP secret workspace"));
            }
            for (id, secret) in servers {
                if id.trim().is_empty() {
                    return Err(SettingsError::Invalid("empty MCP secret id"));
                }
                secret.validate()?;
            }
        }
        Ok(())
    }
}

/// The sparse overlay record: only the members the project file withholds.
#[derive(Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct McpSecretEntry {
    /// The record's generation, renewed on every write. A removal deletes the
    /// record only while it still carries the generation it captured, so a
    /// replacement with equal values keeps its own record. The reserved name
    /// keeps it out of the entry members flattened into `extra`.
    #[serde(
        rename = "$generation",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub generation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<BearerAuth>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<McpSecretOAuth>,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}
impl McpSecretEntry {
    pub(crate) fn is_empty(&self) -> bool {
        self.args.is_none()
            && self.env.is_none()
            && self.headers.is_none()
            && self.auth.is_none()
            && self.url.is_none()
            && self.oauth.is_none()
            && self.extra.is_empty()
    }
    fn validate(&self) -> Result<()> {
        if self.extra.contains_key("$generation") {
            return Err(SettingsError::Invalid("reserved MCP secret overlay field"));
        }
        if self
            .env
            .as_ref()
            .is_some_and(|env| env.keys().any(|name| name.trim().is_empty()))
            || self
                .headers
                .as_ref()
                .is_some_and(|headers| headers.keys().any(|name| name.trim().is_empty()))
            || self
                .auth
                .as_ref()
                .is_some_and(|BearerAuth::Bearer { token }| token.trim().is_empty())
        {
            return Err(SettingsError::Invalid("invalid MCP secret values"));
        }
        if let Some(url) = &self.url {
            if has_reference(url) {
                // A reference may hold the scheme itself, so only a literal
                // scheme written before it can be checked.
                if let Some(scheme) = literal_scheme(url)
                    && !scheme.eq_ignore_ascii_case("http")
                    && !scheme.eq_ignore_ascii_case("https")
                {
                    return Err(SettingsError::Invalid("invalid MCP secret URL"));
                }
            } else {
                let parsed = url::Url::parse(url)
                    .map_err(|_| SettingsError::Invalid("invalid MCP secret URL"))?;
                if parsed.scheme() != "http" && parsed.scheme() != "https" {
                    return Err(SettingsError::Invalid("invalid MCP secret URL"));
                }
            }
        }
        if let Some(oauth) = &self.oauth {
            oauth.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct McpSecretOAuth {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_server_metadata_url: Option<String>,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}
impl McpSecretOAuth {
    fn validate(&self) -> Result<()> {
        if let Some(value) = &self.auth_server_metadata_url {
            let url = url::Url::parse(value)
                .map_err(|_| SettingsError::Invalid("invalid MCP OAuth configuration"))?;
            if !url.username().is_empty()
                || url.password().is_some()
                || url.fragment().is_some()
                || !(url.scheme() == "https"
                    || (url.scheme() == "http"
                        && matches!(url.host(), Some(host) if loopback(&host))))
            {
                return Err(SettingsError::Invalid("invalid MCP OAuth configuration"));
            }
        }
        Ok(())
    }
}

/// The safe part of a project entry, without the overlay-held members.
///
/// A reference is not a URL, so parsing and serializing it would change its
/// spelling or reject the entry. With a reference, a value whose first `://`
/// comes before any `/`, `?` or `#` keeps its scheme and authority as
/// written, in the reference's own case, and everything the reference could
/// hide (userinfo, path, query and fragment) is dropped. A reference without
/// such a scheme keeps the text before any `?` or `#`.
/// Without a reference the previous rule applies: a URL reduces to its
/// origin, and unparseable text is returned as written.
pub(crate) fn public_url(value: &str) -> String {
    if has_reference(value) {
        return reference_public_url(value);
    }
    let Ok(url) = url::Url::parse(value) else {
        return value.to_owned();
    };
    if !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        url.origin().ascii_serialization()
    } else {
        value.to_owned()
    }
}

/// The endpoint a configuration view shows: a reference is kept as written;
/// otherwise the URL is parsed and reduced to its origin, and a value that
/// does not parse stays an error for the view.
pub(crate) fn public_endpoint(value: &str) -> std::result::Result<String, url::ParseError> {
    if has_reference(value) {
        return Ok(reference_public_url(value));
    }
    Ok(url::Url::parse(value)?.origin().ascii_serialization())
}

/// The names a value still references after expansion. A reference the
/// environment does not set keeps its text, so an expanded value names the
/// variables the daemon could not fill in (D19).
pub(crate) fn unresolved(value: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        let candidate = &rest[start + 2..];
        let Some((name, _, used)) = reference(candidate) else {
            rest = &rest[start + 1..];
            continue;
        };
        names.insert(name.to_owned());
        rest = &candidate[used..];
    }
    names
}

fn has_reference(value: &str) -> bool {
    !unresolved(value).is_empty()
}

fn reference_public_url(value: &str) -> String {
    let Some((scheme, rest)) = value.split_once("://") else {
        return before_query_or_fragment(value);
    };
    // A `://` after a `/`, `?` or `#` belongs to the withheld text, not to
    // the value's leading components.
    if scheme.contains(['/', '?', '#']) {
        return before_query_or_fragment(value);
    }
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    format!("{scheme}://{authority}")
}

fn before_query_or_fragment(value: &str) -> String {
    let end = value.find(['?', '#']).unwrap_or(value.len());
    value[..end].to_owned()
}

fn public_oauth(oauth: &McpOAuthSettings) -> Option<McpOAuthSettings> {
    let public = McpOAuthSettings {
        client_id: oauth.client_id.clone(),
        callback_port: oauth.callback_port,
        auth_server_metadata_url: None,
        scopes: oauth.scopes.clone(),
        extra: BTreeMap::new(),
    };
    (public.client_id.is_some() || public.callback_port.is_some() || public.scopes.is_some())
        .then_some(public)
}

/// Split one entry into the public skeleton written to `.mcp.json` and the
/// withheld part kept in `mcp-secrets`.
pub(crate) fn split_entry(
    entry: &McpServerSettings,
) -> (McpServerSettings, Option<McpSecretEntry>) {
    match entry {
        McpServerSettings::Stdio {
            kind,
            command,
            args,
            env,
            cwd,
            timeout,
            extra,
            ..
        } => {
            let secret = McpSecretEntry {
                args: (!args.is_empty()).then(|| args.clone()),
                env: (!env.is_empty()).then(|| env.clone()),
                extra: extra.clone(),
                ..Default::default()
            };
            let skeleton = McpServerSettings::Stdio {
                kind: kind.clone(),
                command: command.clone(),
                args: Vec::new(),
                env: BTreeMap::new(),
                cwd: cwd.clone(),
                enabled: None,
                timeout: *timeout,
                extra: BTreeMap::new(),
            };
            (skeleton, (!secret.is_empty()).then_some(secret))
        }
        McpServerSettings::Remote {
            kind,
            url,
            headers,
            auth,
            oauth,
            timeout,
            extra,
            ..
        } => {
            let public = public_url(url);
            let (metadata, oauth_extra) = oauth.as_ref().map_or((None, BTreeMap::new()), |oauth| {
                (oauth.auth_server_metadata_url.clone(), oauth.extra.clone())
            });
            let secret = McpSecretEntry {
                headers: (!headers.is_empty()).then(|| headers.clone()),
                auth: auth.clone(),
                url: (public != *url).then(|| url.clone()),
                oauth: (metadata.is_some() || !oauth_extra.is_empty()).then(|| McpSecretOAuth {
                    auth_server_metadata_url: metadata,
                    extra: oauth_extra,
                }),
                extra: extra.clone(),
                ..Default::default()
            };
            let skeleton = McpServerSettings::Remote {
                kind: kind.clone(),
                url: public,
                headers: BTreeMap::new(),
                auth: None,
                oauth: oauth.as_deref().and_then(public_oauth).map(Box::new),
                enabled: None,
                timeout: *timeout,
                extra: BTreeMap::new(),
            };
            (skeleton, (!secret.is_empty()).then_some(secret))
        }
    }
}

/// Merge the overlay into a declared project entry. A member of the wrong
/// transport is an invalid overlay, not a silently ignored value.
pub(crate) fn merge_secret(config: &mut McpServerSettings, secret: &McpSecretEntry) -> Result<()> {
    match config {
        McpServerSettings::Stdio {
            args, env, extra, ..
        } => {
            if secret.url.is_some()
                || secret.headers.as_ref().is_some_and(|map| !map.is_empty())
                || secret.auth.is_some()
                || secret.oauth.as_ref().is_some_and(|oauth| {
                    oauth.auth_server_metadata_url.is_some() || !oauth.extra.is_empty()
                })
            {
                return Err(SettingsError::Invalid(
                    "MCP secret overlay does not match the stdio transport",
                ));
            }
            if let Some(value) = &secret.args {
                *args = value.clone();
            }
            if let Some(values) = &secret.env {
                env.extend(values.clone());
            }
            extra.extend(
                secret
                    .extra
                    .iter()
                    .filter(|(name, _)| !ENTRY_MEMBERS.contains(&name.as_str()))
                    .map(|(name, value)| (name.clone(), value.clone())),
            );
        }
        McpServerSettings::Remote {
            url,
            headers,
            auth,
            oauth,
            extra,
            ..
        } => {
            if secret.args.as_ref().is_some_and(|args| !args.is_empty())
                || secret.env.as_ref().is_some_and(|env| !env.is_empty())
            {
                return Err(SettingsError::Invalid(
                    "MCP secret overlay does not match the remote transport",
                ));
            }
            if let Some(value) = &secret.url {
                *url = value.clone();
            }
            if let Some(values) = &secret.headers {
                headers.extend(values.clone());
            }
            if let Some(value) = &secret.auth {
                *auth = Some(value.clone());
            }
            if let Some(secret_oauth) = &secret.oauth {
                let oauth = oauth.get_or_insert_with(Default::default);
                if let Some(value) = &secret_oauth.auth_server_metadata_url {
                    oauth.auth_server_metadata_url = Some(value.clone());
                }
                oauth.extra.extend(secret_oauth.extra.clone());
            }
            extra.extend(
                secret
                    .extra
                    .iter()
                    .filter(|(name, _)| !ENTRY_MEMBERS.contains(&name.as_str()))
                    .map(|(name, value)| (name.clone(), value.clone())),
            );
        }
    }
    Ok(())
}

/// Remove the overlay-held members before writing a project entry back.
pub(crate) fn strip_secret(config: &mut McpServerSettings, secret: &McpSecretEntry) {
    match config {
        McpServerSettings::Stdio {
            args, env, extra, ..
        } => {
            if secret.args.is_some() {
                args.clear();
            }
            if let Some(values) = &secret.env {
                for name in values.keys() {
                    env.remove(name);
                }
            }
            for name in secret.extra.keys() {
                extra.remove(name);
            }
        }
        McpServerSettings::Remote {
            url,
            headers,
            auth,
            oauth,
            extra,
            ..
        } => {
            if secret.url.is_some() {
                *url = public_url(url);
            }
            if let Some(values) = &secret.headers {
                for name in values.keys() {
                    headers.remove(name);
                }
            }
            if secret.auth.is_some() {
                *auth = None;
            }
            if let Some(secret_oauth) = &secret.oauth
                && let Some(oauth) = oauth
            {
                if secret_oauth.auth_server_metadata_url.is_some() {
                    oauth.auth_server_metadata_url = None;
                }
                for name in secret_oauth.extra.keys() {
                    oauth.extra.remove(name);
                }
            }
            for name in secret.extra.keys() {
                extra.remove(name);
            }
        }
    }
}

impl BusinessDocument for McpSettings {
    const NAMESPACE: &'static str = "mcp";
    const USER_FILE: &'static str = "mcp.json";
    const PROJECT_FILE: Option<&'static str> = Some(".mcp.json");

    fn decode(bytes: &[u8]) -> Result<Self> {
        Self::from_file(bytes)
    }

    fn validate(&self) -> Result<()> {
        if self.extra.contains_key("mcpServers") {
            return Err(SettingsError::Invalid("reserved MCP configuration field"));
        }
        if self.mcp_servers.keys().any(|id| id.trim().is_empty()) {
            return Err(EMPTY_ENTRY_ID);
        }
        Ok(())
    }
}

/// The user's enablement of project entries (ADR 003 D3), keyed by canonical
/// workspace and server id. A project source starts disabled: its own files
/// cannot enable an entry, and only this user-local record does.
#[derive(Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct McpProjectEnablement {
    #[serde(default)]
    pub enabled: BTreeMap<String, BTreeSet<String>>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}
impl McpProjectEnablement {
    pub fn contains(&self, workspace: &str, id: &str) -> bool {
        self.enabled
            .get(workspace)
            .is_some_and(|ids| ids.contains(id))
    }
    /// Returns whether the record changed.
    pub fn set(&mut self, workspace: &str, id: &str, enabled: bool) -> bool {
        if enabled {
            return self
                .enabled
                .entry(workspace.to_owned())
                .or_default()
                .insert(id.to_owned());
        }
        let Some(ids) = self.enabled.get_mut(workspace) else {
            return false;
        };
        let changed = ids.remove(id);
        if ids.is_empty() {
            self.enabled.remove(workspace);
        }
        changed
    }
}
impl BusinessDocument for McpProjectEnablement {
    const NAMESPACE: &'static str = "mcp-projects";
    const USER_FILE: &'static str = "mcp-projects.json";

    fn validate(&self) -> Result<()> {
        if self.extra.contains_key("enabled")
            || self.enabled.iter().any(|(workspace, ids)| {
                !Path::new(workspace).is_absolute()
                    || ids.is_empty()
                    || ids.iter().any(|id| id.trim().is_empty())
            })
        {
            return Err(SettingsError::Invalid("invalid MCP project enablement"));
        }
        Ok(())
    }
}

/// The MCP documents plugins registered, by plugin id (PluginManager §3.7).
/// Mcp keeps them so they survive its reload; their entries run under the
/// ids `<plugin>:<server>`.
#[derive(Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct McpPluginServers {
    #[serde(default)]
    pub plugins: BTreeMap<String, McpPluginDocument>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct McpPluginDocument {
    /// The plugin directory: a stdio entry's `cwd` is resolved against it.
    pub dir: PathBuf,
    /// The registered document, decoded entry by entry like any other source.
    #[serde(flatten)]
    pub servers: McpSettings,
}
impl McpPluginServers {
    /// Every entry under its run id, with its effective configuration.
    pub(crate) fn entries(self) -> BTreeMap<String, McpServerSettings> {
        self.into_entries().0
    }

    /// Every decoded entry under its run id with its effective configuration,
    /// and every broken entry under its run id with its error.
    pub(crate) fn into_entries(
        self,
    ) -> (
        BTreeMap<String, McpServerSettings>,
        BTreeMap<String, McpEntryError>,
    ) {
        let mut servers = BTreeMap::new();
        let mut invalid = BTreeMap::new();
        for (plugin, document) in self.plugins {
            let dir = document.dir;
            let (entries, broken) = document.servers.into_parts();
            for (id, mut config) in entries {
                if let McpServerSettings::Stdio { cwd, .. } = &mut config {
                    let path = cwd.as_ref().map_or(dir.clone(), |cwd| dir.join(cwd));
                    *cwd = Some(path.to_string_lossy().into_owned());
                }
                servers.insert(format!("{plugin}:{id}"), config);
            }
            for (id, error) in broken {
                invalid.insert(format!("{plugin}:{id}"), error);
            }
        }
        (servers, invalid)
    }

    /// One entry as its plugin registered it, before the plugin directory
    /// resolves a stdio `cwd`. A broken entry answers its own error.
    pub(crate) fn declared(
        self,
        run_id: &str,
    ) -> std::result::Result<Option<McpServerSettings>, McpEntryError> {
        for (plugin, document) in self.plugins {
            let (entries, broken) = document.servers.into_parts();
            for (id, config) in entries {
                if format!("{plugin}:{id}") == run_id {
                    return Ok(Some(config));
                }
            }
            for (id, error) in broken {
                if format!("{plugin}:{id}") == run_id {
                    return Err(error);
                }
            }
        }
        Ok(None)
    }
}
impl BusinessDocument for McpPluginServers {
    const NAMESPACE: &'static str = "mcp-plugins";
    const USER_FILE: &'static str = "mcp-plugins.json";

    fn validate(&self) -> Result<()> {
        if self.extra.contains_key("plugins") {
            return Err(SettingsError::Invalid("reserved MCP plugin field"));
        }
        for (plugin, document) in &self.plugins {
            if plugin.trim().is_empty() || !document.dir.is_absolute() {
                return Err(SettingsError::Invalid("invalid MCP plugin registration"));
            }
            document.servers.validate()?;
            for config in document.servers.entries().values() {
                if let McpServerSettings::Stdio { cwd: Some(cwd), .. } = config
                    && !Path::new(cwd).components().all(|part| {
                        matches!(
                            part,
                            std::path::Component::Normal(_) | std::path::Component::CurDir
                        )
                    })
                {
                    return Err(OUTSIDE_PLUGIN);
                }
            }
        }
        Ok(())
    }
}

/// A plugin stdio entry's `cwd` that is absolute, climbs with `..`, or
/// resolves outside the plugin directory.
pub(crate) const OUTSIDE_PLUGIN: SettingsError =
    SettingsError::Invalid("MCP plugin cwd must be a directory inside the plugin");

impl McpPluginDocument {
    /// Refuses a stdio `cwd` that, after following symbolic links, is not a
    /// directory inside the canonical plugin directory.
    pub(crate) fn confine(&self) -> Result<()> {
        let dir = std::fs::canonicalize(&self.dir).map_err(|_| OUTSIDE_PLUGIN)?;
        for config in self.servers.entries().values() {
            if let McpServerSettings::Stdio { cwd: Some(cwd), .. } = config {
                match std::fs::canonicalize(dir.join(cwd)) {
                    Ok(resolved) if resolved.starts_with(&dir) && resolved.is_dir() => {}
                    _ => return Err(OUTSIDE_PLUGIN),
                }
            }
        }
        Ok(())
    }
}

/// A configuration with environment references expanded, and the names the
/// expansion read. The stored document is never changed.
#[derive(Clone, PartialEq)]
pub(crate) struct Expanded {
    pub configuration: McpServerSettings,
    pub referenced: BTreeSet<String>,
    pub unset: BTreeSet<String>,
}

/// Who wrote one entry's fields, which decides what a shared document may
/// expand.
#[derive(Clone, Copy)]
pub(crate) enum Origin<'a> {
    /// The user's own document: every field expands.
    User,
    /// A plugin document: every guarded field is guarded, and `cwd` is
    /// resolved inside the plugin directory at registration instead.
    Plugin,
    /// A workspace file, with the user overlay that supplied some members.
    Workspace { overlay: Option<&'a McpSecretEntry> },
}
impl Origin<'_> {
    fn guards_url(self) -> bool {
        match self {
            Self::User => false,
            Self::Plugin => true,
            Self::Workspace { overlay } => overlay.is_none_or(|overlay| overlay.url.is_none()),
        }
    }
    fn guards_header(self, name: &str) -> bool {
        match self {
            Self::User => false,
            Self::Plugin => true,
            Self::Workspace { overlay } => overlay.is_none_or(|overlay| {
                overlay
                    .headers
                    .as_ref()
                    .is_none_or(|headers| !headers.contains_key(name))
            }),
        }
    }
    fn guards_auth(self) -> bool {
        match self {
            Self::User => false,
            Self::Plugin => true,
            Self::Workspace { overlay } => overlay.is_none_or(|overlay| overlay.auth.is_none()),
        }
    }
    fn expands_cwd(self) -> bool {
        !matches!(self, Self::Plugin)
    }
}

/// Names whose values never expand into a guarded field of a shared MCP
/// document, matched without regard to case.
const PROTECTED_NAMES: &[&str] = &[
    // Model providers.
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "OPENAI_API_KEY",
    "AZURE_OPENAI_API_KEY",
    "AZURE_API_KEY",
    "GOOGLE_API_KEY",
    "GEMINI_API_KEY",
    "DEEPSEEK_API_KEY",
    "GROQ_API_KEY",
    "MISTRAL_API_KEY",
    "XAI_API_KEY",
    "OPENROUTER_API_KEY",
    "TOGETHER_API_KEY",
    "FIREWORKS_API_KEY",
    "COHERE_API_KEY",
    "PERPLEXITY_API_KEY",
    "MOONSHOT_API_KEY",
    "DASHSCOPE_API_KEY",
    "MINIMAX_API_KEY",
    "ARK_API_KEY",
    "REPLICATE_API_TOKEN",
    "HUGGINGFACE_API_TOKEN",
    "HF_TOKEN",
    // Cloud credentials.
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AWS_BEARER_TOKEN_BEDROCK",
    "AZURE_CLIENT_SECRET",
    "GOOGLE_APPLICATION_CREDENTIALS",
    // Proxies.
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    // Code hosting and package registries.
    "GITHUB_TOKEN",
    "GH_TOKEN",
    "GITLAB_TOKEN",
    "BITBUCKET_TOKEN",
    "NPM_TOKEN",
    "NODE_AUTH_TOKEN",
    "YARN_NPM_AUTH_TOKEN",
    "CARGO_REGISTRY_TOKEN",
    "CARGO_REGISTRIES_CRATES_IO_TOKEN",
    "PYPI_API_TOKEN",
    "TWINE_PASSWORD",
    "DOCKER_AUTH_CONFIG",
    "DOCKER_PASSWORD",
];

/// Whether `name` is protected: one of Dim's own names, the fixed list, or a
/// name the daemon's credential owner reported.
pub(crate) fn protected(name: &str, names: &BTreeSet<String>) -> bool {
    let upper = name.to_ascii_uppercase();
    upper == "DIM"
        || upper == "DIMCODE"
        || upper.starts_with("DIM_")
        || upper.starts_with("DIMCODE_")
        || PROTECTED_NAMES
            .iter()
            .any(|known| name.eq_ignore_ascii_case(known))
        || names.iter().any(|known| known.eq_ignore_ascii_case(name))
}

/// Expands `${NAME}` and `${NAME:-default}` in the fields an entry runs with:
/// `command`, each argument, each environment value, `cwd`, the remote `url`,
/// each header value and the bearer token. Header and environment names,
/// unknown members and OAuth settings are literal.
///
/// Only `${` followed by a name and `}` or `:-` is a reference; every other
/// `$` is literal. A variable set to the empty string counts as set, an
/// unset reference without a default keeps its text, and a default ends at
/// the first `}` without nesting.
///
/// A guarded field expands a protected name to the empty string, ignoring its
/// default; the name is still reported as referenced. The remote `url`, each
/// header value and the bearer token are guarded by origin; stdio fields are
/// not, because running a shared command already uses the user's authority.
pub(crate) fn expand(
    configuration: &McpServerSettings,
    origin: Origin<'_>,
    environment: &dyn Fn(&str) -> Option<String>,
    protected_names: &BTreeSet<String>,
) -> Expanded {
    let mut scan = Scan::default();
    let text = |scan: &mut Scan, value: &str, guarded: bool| {
        scan.text(value, environment, guarded, protected_names)
    };
    let configuration = match configuration {
        McpServerSettings::Stdio {
            kind,
            command,
            args,
            env,
            cwd,
            enabled,
            timeout,
            extra,
        } => McpServerSettings::Stdio {
            kind: kind.clone(),
            command: text(&mut scan, command, false),
            args: args
                .iter()
                .map(|argument| text(&mut scan, argument, false))
                .collect(),
            env: env
                .iter()
                .map(|(name, value)| (name.clone(), text(&mut scan, value, false)))
                .collect(),
            cwd: cwd.as_ref().map(|cwd| {
                if origin.expands_cwd() {
                    text(&mut scan, cwd, false)
                } else {
                    cwd.clone()
                }
            }),
            enabled: *enabled,
            timeout: *timeout,
            extra: extra.clone(),
        },
        McpServerSettings::Remote {
            kind,
            url,
            headers,
            auth,
            oauth,
            enabled,
            timeout,
            extra,
        } => McpServerSettings::Remote {
            kind: kind.clone(),
            url: text(&mut scan, url, origin.guards_url()),
            headers: headers
                .iter()
                .map(|(name, value)| {
                    let guarded = origin.guards_header(name);
                    (name.clone(), text(&mut scan, value, guarded))
                })
                .collect(),
            auth: auth
                .as_ref()
                .map(|BearerAuth::Bearer { token }| BearerAuth::Bearer {
                    token: text(&mut scan, token, origin.guards_auth()),
                }),
            oauth: oauth.clone(),
            enabled: *enabled,
            timeout: *timeout,
            extra: extra.clone(),
        },
    };
    Expanded {
        configuration,
        referenced: scan.referenced,
        unset: scan.unset,
    }
}

#[derive(Default)]
struct Scan {
    referenced: BTreeSet<String>,
    unset: BTreeSet<String>,
}

impl Scan {
    fn text(
        &mut self,
        value: &str,
        environment: &dyn Fn(&str) -> Option<String>,
        guarded: bool,
        protected_names: &BTreeSet<String>,
    ) -> String {
        let mut expanded = String::with_capacity(value.len());
        let mut rest = value;
        while let Some(start) = rest.find("${") {
            expanded.push_str(&rest[..start]);
            let candidate = &rest[start + 2..];
            let Some((name, default, used)) = reference(candidate) else {
                expanded.push('$');
                rest = &rest[start + 1..];
                continue;
            };
            self.referenced.insert(name.to_owned());
            if guarded && protected(name, protected_names) {
                rest = &candidate[used..];
                continue;
            }
            if let Some(value) = environment(name) {
                expanded.push_str(&value);
            } else if let Some(default) = default {
                expanded.push_str(default);
            } else {
                self.unset.insert(name.to_owned());
                expanded.push_str(&rest[start..start + 2 + used]);
            }
            rest = &candidate[used..];
        }
        expanded.push_str(rest);
        expanded
    }
}

/// One reference after its `${`: the name, the default text after `:-`, and
/// the bytes it occupies, closing brace included.
fn reference(input: &str) -> Option<(&str, Option<&str>, usize)> {
    let bytes = input.as_bytes();
    let first = *bytes.first()?;
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return None;
    }
    let mut used = 1;
    while used < bytes.len() && (bytes[used].is_ascii_alphanumeric() || bytes[used] == b'_') {
        used += 1;
    }
    let name = &input[..used];
    match bytes.get(used) {
        Some(b'}') => Some((name, None, used + 1)),
        Some(b':') if bytes.get(used + 1) == Some(&b'-') => {
            let start = used + 2;
            let close = input[start..].find('}')?;
            Some((name, Some(&input[start..start + close]), start + close + 1))
        }
        _ => None,
    }
}

/// The scheme written literally before the first reference of a value. A
/// reference may hold the `://` itself, and then there is no literal scheme
/// to check.
fn literal_scheme(value: &str) -> Option<&str> {
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        if reference(&rest[start + 2..]).is_some() {
            return rest[..start].split_once("://").map(|(scheme, _)| scheme);
        }
        rest = &rest[start + 1..];
    }
    None
}

/// The canonical workspace of an authorized project source, as the
/// enablement key.
pub(crate) fn workspace(settings: &Settings, scope: SettingsScope) -> Result<String> {
    let SettingsScope::Project(root) = scope else {
        return Err(SettingsError::Invalid("not a project MCP source"));
    };
    settings
        .projects()
        .into_iter()
        .find(|project| project.root == root)
        .ok_or(SettingsError::Invalid("unauthorized project"))?
        .cwd
        .to_str()
        .map(str::to_owned)
        .ok_or(SettingsError::Invalid("project cwd must be UTF-8"))
}

/// What a connection's configuration needs besides the declared entry: the
/// source it belongs to, the overlay to merge, who wrote each member, the
/// user's project enablement, and the environment to expand from.
pub(crate) struct Connection<'a> {
    pub workspace: Option<&'a str>,
    pub id: &'a str,
    pub secret: Option<&'a McpSecretEntry>,
    pub origin: Origin<'a>,
    pub enablement: &'a McpProjectEnablement,
    pub environment: &'a dyn Fn(&str) -> Option<String>,
    pub protected: &'a BTreeSet<String>,
}

/// The configuration a connection runs: merge the overlay into the declared
/// entry, expand environment references, then apply the project workspace
/// `cwd` and enablement. Every caller uses this one order, so a probe and a
/// published connection see the same values.
pub(crate) fn connected(
    configuration: McpServerSettings,
    connection: Connection<'_>,
) -> Result<Expanded> {
    let Connection {
        workspace,
        id,
        secret,
        origin,
        enablement,
        environment,
        protected,
    } = connection;
    let mut configuration = configuration;
    if let Some(secret) = secret {
        merge_secret(&mut configuration, secret)?;
    }
    let expansion = expand(&configuration, origin, environment, protected);
    let configuration = effective(workspace, id, expansion.configuration, enablement)?;
    Ok(Expanded {
        configuration,
        referenced: expansion.referenced,
        unset: expansion.unset,
    })
}

/// The declared entry as a management read shows it: the overlay merged and
/// the project workspace `cwd` joined, with references kept as written so a
/// client can edit them.
pub(crate) fn viewed(
    workspace: Option<&str>,
    id: &str,
    configuration: McpServerSettings,
    secret: Option<&McpSecretEntry>,
    enablement: &McpProjectEnablement,
) -> Result<McpServerSettings> {
    let mut configuration = configuration;
    if let Some(secret) = secret {
        merge_secret(&mut configuration, secret)?;
    }
    effective(workspace, id, configuration, enablement)
}

/// The configuration an entry runs with. A project entry resolves its stdio
/// cwd against the workspace and takes its enablement only from the user's
/// record; its own `enabled` field is ignored.
pub(crate) fn effective(
    workspace: Option<&str>,
    id: &str,
    mut config: McpServerSettings,
    enablement: &McpProjectEnablement,
) -> Result<McpServerSettings> {
    if let Some(workspace) = workspace {
        let (McpServerSettings::Stdio { enabled, .. } | McpServerSettings::Remote { enabled, .. }) =
            &mut config;
        *enabled = Some(enablement.contains(workspace, id));
        if let McpServerSettings::Stdio { cwd, .. } = &mut config {
            let workspace = PathBuf::from(workspace);
            let path = cwd
                .as_ref()
                .map_or(workspace.clone(), |cwd| workspace.join(cwd));
            *cwd = Some(
                path.to_str()
                    .ok_or(SettingsError::Invalid("project cwd must be UTF-8"))?
                    .to_owned(),
            );
        }
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_entry_member_named_generation_keeps_its_own_name() {
        let record = McpSecretEntry {
            generation: "g".into(),
            extra: BTreeMap::from([("generation".into(), json!("custom"))]),
            ..Default::default()
        };
        let value = serde_json::to_value(&record).unwrap();
        assert_eq!(value["$generation"], "g");
        assert_eq!(value["generation"], "custom");
        assert_eq!(
            serde_json::to_value(serde_json::from_value::<McpSecretEntry>(value).unwrap()).unwrap(),
            serde_json::to_value(&record).unwrap()
        );
    }

    #[test]
    fn a_reserved_generation_member_is_refused() {
        let document: McpSettings = serde_json::from_value(json!({
            "mcpServers": {
                "collision": {"type": "stdio", "command": "server", "$generation": "g"}
            }
        }))
        .unwrap();
        assert!(document.entries().is_empty());
        let error = document.invalid_error("collision").unwrap();
        assert_eq!(error.field.as_deref(), Some("$generation"));
        assert_eq!(error.expected, "not a reserved field");
        assert!(document.validate().is_ok());

        let record = McpSecretEntry {
            extra: BTreeMap::from([("$generation".into(), json!("g"))]),
            ..Default::default()
        };
        assert!(record.validate().is_err());
    }

    #[test]
    fn a_withheld_url_keeps_the_literal_scheme_check() {
        let accepted = |url: &str| {
            McpSecretEntry {
                url: Some(url.to_owned()),
                ..Default::default()
            }
            .validate()
            .is_ok()
        };
        assert!(accepted("https://${MCP_952_HOST}/mcp"));
        assert!(accepted("HTTPS://${MCP_952_HOST}/mcp"));
        assert!(accepted("${MCP_952_SCHEME}://${MCP_952_HOST}/mcp"));
        assert!(accepted("http://localhost:${MCP_952_PORT}/mcp"));
        assert!(accepted("https://host/${MCP_952_PATH}"));
        assert!(!accepted("ftp://${MCP_952_HOST}/mcp"));
        assert!(!accepted("ftp://host/mcp"));
        assert!(!accepted("not a url"));
    }

    #[test]
    fn entries_decode_one_by_one_and_keep_unread_entries_as_written() {
        let source = json!({
            "mcpServers": {
                "stdio": {"command": "server", "args": ["--flag"], "future": {"keep": [1, 2]}},
                "stream": {"type": "streamable-http", "url": "https://stream.test/mcp", "headers": {"X-Key": "value"}},
                "ws": {"type": "ws", "url": "wss://ws.test"},
                "untyped": {"url": "https://untyped.test/mcp"},
                "scalar": "not an object"
            },
            "extension": {"preserve": true}
        });
        let document: McpSettings = serde_json::from_value(source.clone()).unwrap();
        assert_eq!(document.entries().len(), 2);
        assert_eq!(document.invalid_entries().len(), 3);
        assert_eq!(
            document.invalid_entries()["ws"].field.as_deref(),
            Some("type")
        );
        assert_eq!(
            document.invalid_entries()["ws"].expected,
            "http or sse; ws is not a supported transport"
        );
        assert_eq!(
            document.invalid_entries()["untyped"].expected,
            "http or sse; an entry with a url must name its type"
        );
        assert_eq!(document.invalid_entries()["scalar"].field, None);
        assert!(document.validate().is_ok());

        let written = serde_json::to_value(&document).unwrap();
        for id in ["stdio", "stream", "ws", "untyped"] {
            assert_eq!(written["mcpServers"][id], source["mcpServers"][id], "{id}");
            assert_eq!(
                serde_json::to_string(&written["mcpServers"][id]).unwrap(),
                serde_json::to_string(&source["mcpServers"][id]).unwrap(),
                "{id}"
            );
        }
        assert_eq!(
            written["mcpServers"]["scalar"],
            source["mcpServers"]["scalar"]
        );
        assert_eq!(written["extension"], source["extension"]);
    }

    #[test]
    fn an_entry_error_names_the_field_and_never_repeats_the_value() {
        let marker = "token-like-value-8f2c";
        let document: McpSettings = serde_json::from_value(json!({
            "mcpServers": {
                "header": {"type": "http", "url": "https://example.test/mcp", "headers": {"Authorization": {"leak": marker}}},
                "arg": {"command": "server", "args": [marker, true]}
            }
        }))
        .unwrap();
        assert_eq!(document.invalid_entries().len(), 2);
        let header = &document.invalid_entries()["header"];
        assert_eq!(header.field.as_deref(), Some("headers.Authorization"));
        assert!(header.to_string().contains("must be a string"));
        let arg = &document.invalid_entries()["arg"];
        assert_eq!(arg.field.as_deref(), Some("args[1]"));
        assert!(arg.to_string().contains("must be a string"));
        assert!(!header.to_string().contains(marker));
        assert!(!arg.to_string().contains(marker));
    }

    #[test]
    fn a_streamable_http_entry_reads_as_http_and_is_written_back_as_written() {
        let mut document: McpSettings = serde_json::from_value(json!({
            "mcpServers": {
                "stream": {"type": "streamable-http", "url": "https://example.test/mcp", "future": 1}
            }
        }))
        .unwrap();
        let written = serde_json::to_value(&document).unwrap();
        assert_eq!(written["mcpServers"]["stream"]["type"], "streamable-http");
        assert_eq!(written["mcpServers"]["stream"]["future"], 1);
        assert!(document.configure_mut("stream").is_some());
        let written = serde_json::to_value(&document).unwrap();
        assert_eq!(written["mcpServers"]["stream"]["type"], "http");
        assert_eq!(written["mcpServers"]["stream"]["future"], 1);
    }

    #[test]
    fn a_broken_entry_is_removed_or_replaced_without_touching_others() {
        let mut document: McpSettings = serde_json::from_value(json!({
            "mcpServers": {
                "broken": {"type": "ws", "url": "wss://example.test"},
                "kept": {"command": "server", "future": {"keep": true}}
            }
        }))
        .unwrap();
        assert!(document.remove("broken").is_none());
        assert!(document.invalid_entries().is_empty());
        let written = serde_json::to_value(&document).unwrap();
        assert!(written["mcpServers"].get("broken").is_none());
        assert_eq!(
            serde_json::to_string(&written["mcpServers"]["kept"]).unwrap(),
            serde_json::to_string(&json!({"command": "server", "future": {"keep": true}})).unwrap()
        );

        let mut document: McpSettings = serde_json::from_value(json!({
            "mcpServers": {"broken": {"type": "ws", "url": "wss://example.test"}}
        }))
        .unwrap();
        let configuration = decode_entry("broken", &json!({"command": "server"})).unwrap();
        document.insert("broken", configuration);
        assert!(document.invalid_entries().is_empty());
        let written = serde_json::to_value(&document).unwrap();
        assert_eq!(written["mcpServers"]["broken"]["command"], "server");
        assert!(written["mcpServers"]["broken"].get("url").is_none());
    }

    #[test]
    fn only_a_document_level_failure_refuses_the_whole_file() {
        let marker = "secret-marker-4a1f";
        let invalid =
            McpSettings::from_file(format!("{{\"mcpServers\": \"{marker}\"}}").as_bytes())
                .err()
                .unwrap()
                .to_string();
        assert!(invalid.contains("mcpServers must be a JSON object at line 1 column"));
        assert!(!invalid.contains(marker));

        let not_json = McpSettings::from_file(format!("{{{marker}").as_bytes())
            .err()
            .unwrap()
            .to_string();
        assert!(not_json.contains("not valid JSON at line"));
        assert!(!not_json.contains(marker));

        let missing = McpSettings::from_file(b"{}").err().unwrap().to_string();
        assert!(missing.contains("must contain an mcpServers object"));
    }

    #[test]
    fn a_document_shape_refusal_carries_its_static_reason() {
        let cases: [(&[u8], &str); 4] = [
            (br#"{"mcpServers": {},}"#, "MCP document is not valid JSON"),
            (b"[]", "MCP document must be a JSON object"),
            (br#"{"mcpServers": 5}"#, "mcpServers must be a JSON object"),
            (
                br#"{"servers": {}}"#,
                "MCP document must contain an mcpServers object",
            ),
        ];
        for (bytes, reason) in cases {
            let error = McpSettings::from_file(bytes).err().unwrap();
            assert_eq!(error.reason(), Some(reason), "{bytes:?}");
            let display = error.to_string();
            assert!(
                display.starts_with(&format!(
                    "invalid settings document: {reason} at line 1 column "
                )),
                "{display}"
            );
        }

        let value = McpSettings::from_value(json!({"servers": {}}))
            .err()
            .unwrap();
        assert_eq!(
            value.reason(),
            Some("MCP document must contain an mcpServers object")
        );
        assert_eq!(
            value.to_string(),
            "invalid settings document: MCP document must contain an mcpServers object"
        );
    }

    #[test]
    fn null_optional_members_read_as_absent() {
        let document = McpSettings::from_file(
            br#"{"mcpServers":{
                "stdio": {"type": null, "command": "server", "enabled": null, "auth": null, "oauth": null},
                "remote": {"type": "http", "url": "https://example.test/mcp", "enabled": null, "auth": null, "oauth": null},
                "lists": {"command": "server", "args": null, "env": null, "headers": null},
                "metadata": {"command": "server", "metadata": null}
            }}"#,
        )
        .unwrap();
        assert_eq!(document.entries().len(), 4);
        assert!(document.invalid_entries().is_empty());
        let McpServerSettings::Stdio { kind, enabled, .. } = &document.entries()["stdio"] else {
            panic!("a null type with a command is stdio");
        };
        assert!(kind.is_none());
        assert_eq!(*enabled, None);
        let McpServerSettings::Remote {
            auth,
            oauth,
            enabled,
            ..
        } = &document.entries()["remote"]
        else {
            panic!("a typed url entry is remote");
        };
        assert!(auth.is_none());
        assert!(oauth.is_none());
        assert_eq!(*enabled, None);
        let McpServerSettings::Stdio { args, env, .. } = &document.entries()["lists"] else {
            panic!("a stdio entry with null lists");
        };
        assert!(args.is_empty());
        assert!(env.is_empty());

        // A required member set to null is still an entry error.
        let broken = McpSettings::from_file(
            br#"{"mcpServers":{
                "no-command": {"type": null, "command": null},
                "no-url": {"type": "http", "url": null},
                "no-transport": {"type": null}
            }}"#,
        )
        .unwrap();
        assert_eq!(broken.invalid_entries().len(), 3);
        assert_eq!(
            broken.invalid_entries()["no-command"].field.as_deref(),
            Some("command")
        );
        assert_eq!(
            broken.invalid_entries()["no-url"].field.as_deref(),
            Some("url")
        );
        assert_eq!(
            broken.invalid_entries()["no-transport"].field.as_deref(),
            Some("type")
        );
    }

    #[test]
    fn an_empty_entry_id_fails_the_document() {
        for source in [
            br#"{"mcpServers":{"": {"command": "server"}}}"#.as_slice(),
            br#"{"mcpServers":{"   ": {"command": "server"}}}"#.as_slice(),
        ] {
            let error = McpSettings::from_file(source).err().unwrap();
            assert_eq!(
                error.reason(),
                Some("MCP server entry id is empty"),
                "{error}"
            );
            let text = error.to_string();
            assert!(text.contains("entry id is empty"), "{text}");
            assert!(!text.contains("command") && !text.contains('{'), "{text}");
        }
    }

    fn user_expand(
        configuration: &McpServerSettings,
        environment: &dyn Fn(&str) -> Option<String>,
    ) -> Expanded {
        expand(configuration, Origin::User, environment, &BTreeSet::new())
    }

    fn remote(url: &str, headers: &[(&str, &str)], token: Option<&str>) -> McpServerSettings {
        McpServerSettings::Remote {
            kind: RemoteTransport::Http,
            url: url.to_owned(),
            headers: headers
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
            auth: token.map(|token| BearerAuth::Bearer {
                token: token.to_owned(),
            }),
            oauth: Some(Box::new(McpOAuthSettings {
                client_id: Some("${OAUTH_CLIENT}".into()),
                auth_server_metadata_url: Some("https://${OAUTH_HOST}/metadata".into()),
                ..Default::default()
            })),
            enabled: None,
            timeout: None,
            extra: BTreeMap::from([("note".into(), json!("${EXTRA}"))]),
        }
    }

    #[test]
    fn expansion_expands_a_name_and_a_default_only_when_unset() {
        let environment = |name: &str| match name {
            "SET" => Some("value".to_owned()),
            "EMPTY" => Some(String::new()),
            _ => None,
        };
        let expanded = user_expand(
            &remote(
                "https://${SET}.test/${MISSING}",
                &[("X-Value", "${EMPTY}${DEFAULTED:-fallback}")],
                None,
            ),
            &environment,
        );
        let McpServerSettings::Remote { url, headers, .. } = &expanded.configuration else {
            panic!("a remote entry stays remote");
        };
        assert_eq!(url, "https://value.test/${MISSING}");
        assert_eq!(headers["X-Value"].as_str(), "fallback");
        assert_eq!(
            expanded.referenced,
            BTreeSet::from([
                "DEFAULTED".to_owned(),
                "EMPTY".to_owned(),
                "MISSING".to_owned(),
                "SET".to_owned(),
            ])
        );
        assert_eq!(expanded.unset, BTreeSet::from(["MISSING".to_owned()]));
    }

    #[test]
    fn expansion_leaves_other_dollar_text_literal() {
        for text in ["$A", "$$", "${1BAD}", "${A-B}", "${", "a${B"] {
            let expanded = user_expand(&remote(text, &[], None), &|_| {
                panic!("no reference is parsed from {text}")
            });
            let McpServerSettings::Remote { url, .. } = &expanded.configuration else {
                panic!("a remote entry stays remote");
            };
            assert_eq!(url, text, "{text}");
            assert!(expanded.referenced.is_empty(), "{text}");
            assert!(expanded.unset.is_empty(), "{text}");
        }

        // There is no nesting: a default ends at the first `}`.
        let expanded = user_expand(&remote("${A:-${B}}", &[], None), &|_| None);
        let McpServerSettings::Remote { url, .. } = &expanded.configuration else {
            panic!("a remote entry stays remote");
        };
        assert_eq!(url, "${B}");
        assert_eq!(expanded.referenced, BTreeSet::from(["A".to_owned()]));
        assert!(expanded.unset.is_empty());
    }

    #[test]
    fn expansion_walks_exactly_the_listed_fields() {
        let stdio = McpServerSettings::Stdio {
            kind: Some(Stdio::Stdio),
            command: "${CMD}".into(),
            args: vec!["--flag=${FLAG}".into(), "${PLAIN}".into()],
            env: BTreeMap::from([("ENV_NAME".into(), "${VALUE}".into())]),
            cwd: Some("${DIR}".into()),
            enabled: Some(true),
            timeout: Some(1_000),
            extra: BTreeMap::from([("note".into(), json!("${EXTRA}"))]),
        };
        let environment = |name: &str| match name {
            "CMD" => Some("server".to_owned()),
            "FLAG" => Some("on".to_owned()),
            "VALUE" => Some("value".to_owned()),
            "DIR" => Some("/tmp/work".to_owned()),
            "URL" => Some("https://example.test/mcp".to_owned()),
            "HEADER" => Some("header-value".to_owned()),
            "TOKEN" => Some("token-value".to_owned()),
            _ => None,
        };
        let expanded = user_expand(&stdio, &environment);
        let McpServerSettings::Stdio {
            kind,
            command,
            args,
            env,
            cwd,
            enabled,
            timeout,
            extra,
        } = &expanded.configuration
        else {
            panic!("a stdio entry stays stdio");
        };
        assert!(matches!(kind, Some(Stdio::Stdio)));
        assert_eq!(command, "server");
        assert_eq!(args, &["--flag=on".to_owned(), "${PLAIN}".to_owned()]);
        assert_eq!(env["ENV_NAME"].as_str(), "value");
        assert_eq!(cwd.as_deref(), Some("/tmp/work"));
        assert_eq!(*enabled, Some(true));
        assert_eq!(*timeout, Some(1_000));
        assert_eq!(extra["note"], json!("${EXTRA}"));
        assert_eq!(
            expanded.referenced,
            BTreeSet::from([
                "CMD".to_owned(),
                "DIR".to_owned(),
                "FLAG".to_owned(),
                "PLAIN".to_owned(),
                "VALUE".to_owned(),
            ])
        );

        let remote = remote(
            "${URL}",
            &[("X-Header", "${HEADER}")],
            Some("Bearer ${TOKEN}"),
        );
        let expanded = user_expand(&remote, &environment);
        let McpServerSettings::Remote {
            url,
            headers,
            auth,
            oauth,
            extra,
            ..
        } = &expanded.configuration
        else {
            panic!("a remote entry stays remote");
        };
        assert_eq!(url, "https://example.test/mcp");
        assert_eq!(headers["X-Header"].as_str(), "header-value");
        assert!(matches!(
            auth,
            Some(BearerAuth::Bearer { token }) if token == "Bearer token-value"
        ));
        let oauth = oauth.as_deref().unwrap();
        assert_eq!(oauth.client_id.as_deref(), Some("${OAUTH_CLIENT}"));
        assert_eq!(
            oauth.auth_server_metadata_url.as_deref(),
            Some("https://${OAUTH_HOST}/metadata")
        );
        assert_eq!(extra["note"], json!("${EXTRA}"));
        assert_eq!(
            expanded.referenced,
            BTreeSet::from(["HEADER".to_owned(), "TOKEN".to_owned(), "URL".to_owned()])
        );
    }

    #[test]
    fn expansion_reports_referenced_and_unset_names_once() {
        let expanded = user_expand(
            &remote("${B}/${A}/${B}/${D:-fallback}/${A}", &[], None),
            &|name: &str| (name == "A").then(|| "a".to_owned()),
        );
        assert_eq!(
            expanded.referenced,
            BTreeSet::from(["A".to_owned(), "B".to_owned(), "D".to_owned()])
        );
        assert_eq!(expanded.unset, BTreeSet::from(["B".to_owned()]));
    }

    #[test]
    fn expansion_does_not_change_a_stored_document() {
        let source = json!({
            "mcpServers": {
                "docs": {
                    "type": "http",
                    "url": "https://${MCP_HOST}/mcp",
                    "headers": {"Authorization": "Bearer ${MCP_TOKEN}"}
                }
            }
        });
        let document: McpSettings = serde_json::from_value(source.clone()).unwrap();
        let before = serde_json::to_value(&document).unwrap();
        let configuration = document.get("docs").unwrap();
        let expanded = user_expand(configuration, &|_| Some("expanded".to_owned()));
        assert!(
            serde_json::to_value(&expanded.configuration).unwrap() != before["mcpServers"]["docs"]
        );
        assert_eq!(serde_json::to_value(&document).unwrap(), before);
        assert_eq!(before, source);
    }

    #[test]
    fn guarded_fields_empty_a_protected_name_and_ignore_its_default() {
        let names = BTreeSet::from(["DYNAMIC_KEY".to_owned()]);
        let environment = |_: &str| Some("secret".to_owned());
        let entry = remote(
            "${GITHUB_TOKEN:-fallback}",
            &[("X-Key", "${HTTP_PROXY}"), ("X-Dynamic", "${dynamic_key}")],
            Some("Bearer ${DIM_TOKEN}"),
        );
        for origin in [Origin::Plugin, Origin::Workspace { overlay: None }] {
            let expanded = expand(&entry, origin, &environment, &names);
            let McpServerSettings::Remote {
                url, headers, auth, ..
            } = &expanded.configuration
            else {
                panic!("a remote entry stays remote");
            };
            assert_eq!(url, "");
            assert_eq!(headers["X-Key"].as_str(), "");
            assert_eq!(headers["X-Dynamic"].as_str(), "");
            assert!(matches!(
                auth,
                Some(BearerAuth::Bearer { token }) if token == "Bearer "
            ));
            assert_eq!(
                expanded.referenced,
                BTreeSet::from([
                    "DIM_TOKEN".to_owned(),
                    "GITHUB_TOKEN".to_owned(),
                    "HTTP_PROXY".to_owned(),
                    "dynamic_key".to_owned(),
                ])
            );
            assert!(expanded.unset.is_empty());
        }

        // The user's own text expands the same names.
        let expanded = expand(&entry, Origin::User, &environment, &names);
        let McpServerSettings::Remote {
            url, headers, auth, ..
        } = &expanded.configuration
        else {
            panic!("a remote entry stays remote");
        };
        assert_eq!(url, "secret");
        assert_eq!(headers["X-Key"].as_str(), "secret");
        assert_eq!(headers["X-Dynamic"].as_str(), "secret");
        assert!(matches!(
            auth,
            Some(BearerAuth::Bearer { token }) if token == "Bearer secret"
        ));
    }

    #[test]
    fn overlay_members_are_not_guarded() {
        let environment = |name: &str| Some(format!("{name}-value"));
        let overlay = McpSecretEntry {
            url: Some("https://${GITHUB_TOKEN}/overlay".into()),
            headers: Some(BTreeMap::from([(
                "X-Overlay".to_owned(),
                "${GITHUB_TOKEN}".to_owned(),
            )])),
            auth: Some(BearerAuth::Bearer {
                token: "${GITHUB_TOKEN}".into(),
            }),
            ..Default::default()
        };
        let entry = remote(
            "https://${GITHUB_TOKEN}/overlay",
            &[
                ("X-Overlay", "${GITHUB_TOKEN}"),
                ("X-File", "${GITHUB_TOKEN}"),
            ],
            Some("Bearer ${GITHUB_TOKEN}"),
        );
        let expanded = expand(
            &entry,
            Origin::Workspace {
                overlay: Some(&overlay),
            },
            &environment,
            &BTreeSet::new(),
        );
        let McpServerSettings::Remote {
            url, headers, auth, ..
        } = &expanded.configuration
        else {
            panic!("a remote entry stays remote");
        };
        assert_eq!(url, "https://GITHUB_TOKEN-value/overlay");
        assert_eq!(headers["X-Overlay"].as_str(), "GITHUB_TOKEN-value");
        assert_eq!(headers["X-File"].as_str(), "");
        assert!(matches!(
            auth,
            Some(BearerAuth::Bearer { token }) if token == "Bearer GITHUB_TOKEN-value"
        ));
    }

    #[test]
    fn plugin_entries_expand_stdio_and_keep_cwd_literal() {
        let environment = |_: &str| Some("value".to_owned());
        let entry = McpServerSettings::Stdio {
            kind: None,
            command: "${GITHUB_TOKEN}".into(),
            args: vec!["--key=${GITHUB_TOKEN}".into()],
            env: BTreeMap::from([("KEY".into(), "${GITHUB_TOKEN}".into())]),
            cwd: Some("${GITHUB_TOKEN}".into()),
            enabled: None,
            timeout: None,
            extra: BTreeMap::new(),
        };
        let expanded = expand(&entry, Origin::Plugin, &environment, &BTreeSet::new());
        let McpServerSettings::Stdio {
            command,
            args,
            env,
            cwd,
            ..
        } = &expanded.configuration
        else {
            panic!("a stdio entry stays stdio");
        };
        assert_eq!(command, "value");
        assert_eq!(args, &["--key=value".to_owned()]);
        assert_eq!(env["KEY"].as_str(), "value");
        assert_eq!(cwd.as_deref(), Some("${GITHUB_TOKEN}"));
    }

    #[test]
    fn public_url_keeps_a_reference_as_written() {
        // The authority keeps the reference's case; path, query and userinfo go.
        assert_eq!(public_url("https://${MCP_HOST}/mcp"), "https://${MCP_HOST}");
        assert_eq!(
            public_url("https://${MCP_HOST}:8443/mcp?x=1#f"),
            "https://${MCP_HOST}:8443"
        );
        assert_eq!(
            public_url("https://user:made-cz-qvjbfw-9k2@${MCP_HOST}/mcp"),
            "https://${MCP_HOST}"
        );
        // No authority: the text before any query or fragment stays.
        assert_eq!(public_url("${MCP_URL}/mcp"), "${MCP_URL}/mcp");
        assert_eq!(public_url("${MCP_URL}/mcp?x#y"), "${MCP_URL}/mcp");
        // A `://` after a `/`, `?` or `#` is part of the withheld text, not the
        // value's scheme, so the query never becomes public.
        assert_eq!(
            public_url("${BASE}/mcp?token=demo-key&next=https://callback.example/done"),
            "${BASE}/mcp"
        );
        assert_eq!(public_url("${BASE}/mcp#frag?x=https://y"), "${BASE}/mcp");
        // The scheme may be a reference itself; userinfo, path and query go.
        assert_eq!(
            public_url("${SCHEME}://user:secret@host/mcp?token=t"),
            "${SCHEME}://host"
        );
        assert_eq!(
            public_url("https://${HOST}/a?next=https://b"),
            "https://${HOST}"
        );

        // Without a reference the previous rule is unchanged.
        assert_eq!(
            public_url("https://example.test/mcp"),
            "https://example.test"
        );
        assert_eq!(
            public_url("https://user:made-cz-qvjbfw-9k2@example.test/mcp?x=1"),
            "https://example.test"
        );
        assert_eq!(public_url("not a url"), "not a url");
    }

    #[test]
    fn a_timeout_is_an_integer_number_of_milliseconds_from_1000_to_86400000() {
        let document = McpSettings::from_file(
            br#"{"mcpServers":{
                "stdio": {"command":"server","timeout":1000},
                "remote": {"type":"http","url":"https://example.test/mcp","timeout":86400000}
            }}"#,
        )
        .unwrap();
        assert!(document.invalid_entries().is_empty());
        assert_eq!(document.entries()["stdio"].timeout(), Some(1_000));
        assert_eq!(document.entries()["remote"].timeout(), Some(86_400_000));
        assert!(document.validate().is_ok());
        assert_eq!(
            serde_json::to_value(&document.entries()["stdio"]).unwrap(),
            json!({"command": "server", "timeout": 1000})
        );

        for value in [
            json!(999),
            json!(86_400_001),
            json!(1500.5),
            json!("1000"),
            json!(-1),
            json!(null),
        ] {
            let document = McpSettings::from_value(json!({"mcpServers":{
                "broken": {"command":"server","timeout": value},
                "valid": {"command":"server","timeout":1000}
            }}))
            .unwrap();
            let error = document.invalid_error("broken").unwrap();
            assert_eq!(error.id, "broken");
            assert_eq!(error.field.as_deref(), Some("timeout"), "{value}");
            assert!(
                error.expected.contains("1000") && error.expected.contains("86400000"),
                "{}",
                error.expected
            );
            assert!(!error.to_string().contains(&value.to_string()), "{value}");
            assert_eq!(document.entries()["valid"].timeout(), Some(1_000));
            assert!(!document.contains("broken"));
        }
    }

    #[test]
    fn a_timeout_belongs_to_the_public_skeleton_not_the_secret_overlay() {
        let entry = |timeout| McpServerSettings::Remote {
            kind: RemoteTransport::Http,
            url: "https://example.test".into(),
            headers: BTreeMap::new(),
            auth: None,
            oauth: None,
            enabled: None,
            timeout,
            extra: BTreeMap::new(),
        };
        let (skeleton, secret) = split_entry(&entry(Some(1_000)));
        assert_eq!(skeleton.timeout(), Some(1_000));
        assert!(secret.is_none());

        // An overlay `timeout` is an unknown member: it changes neither a
        // declared value nor an undeclared one.
        let overlay = McpSecretEntry {
            extra: BTreeMap::from([("timeout".into(), json!(5_000))]),
            ..Default::default()
        };
        let mut merged = skeleton;
        merge_secret(&mut merged, &overlay).unwrap();
        assert_eq!(merged.timeout(), Some(1_000));
        let mut undeclared = entry(None);
        merge_secret(&mut undeclared, &overlay).unwrap();
        assert_eq!(undeclared.timeout(), None);
        // A write drops the overlay member, so it never becomes the entry's
        // own field.
        strip_secret(&mut undeclared, &overlay);
        let document = McpSettings::from_value(
            json!({"mcpServers":{"id": serde_json::to_value(&undeclared).unwrap()}}),
        )
        .unwrap();
        assert_eq!(document.entries()["id"].timeout(), None);
    }

    #[test]
    fn the_fixed_protected_list_covers_provider_cloud_proxy_registry_and_dim_names() {
        let none = BTreeSet::new();
        for name in PROTECTED_NAMES {
            assert!(protected(name, &none), "{name}");
        }
        assert!(protected("github_token", &none));
        assert!(protected("DIM", &none));
        assert!(protected("DIMCODE", &none));
        assert!(protected("DIM_X", &none));
        assert!(protected("dimcode_x", &none));
        assert!(!protected("DIMENSION_X", &none));
        assert!(!protected("MCP_HOST", &none));
        let names = BTreeSet::from(["TEST_PROVIDER_KEY".to_owned()]);
        assert!(protected("test_provider_key", &names));
        assert!(!protected("OTHER_KEY", &names));
    }
}
