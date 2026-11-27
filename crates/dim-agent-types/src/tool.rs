use crate::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SchemaType {
    #[serde(rename = "object")]
    Object,
    #[serde(rename = "array")]
    Array,
    #[serde(rename = "string")]
    String,
    #[serde(rename = "number")]
    Number,
    #[serde(rename = "integer")]
    Integer,
    #[serde(rename = "boolean")]
    Boolean,
    #[serde(rename = "null")]
    Null,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolSource {
    #[serde(rename = "static")]
    Static,
    #[serde(rename = "builtin")]
    Builtin,
    #[serde(rename = "extension")]
    Extension,
    #[serde(rename = "dynamic")]
    Dynamic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolFilePathSelector {
    #[serde(rename = "path")]
    Path,
    #[serde(rename = "affectedPaths")]
    AffectedPaths,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WakePolicy {
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "manual")]
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlobKind {
    #[serde(rename = "text")]
    Text,
    #[serde(rename = "binary")]
    Binary,
    #[serde(rename = "file")]
    File,
    #[serde(rename = "stream")]
    Stream,
    #[serde(rename = "remote")]
    Remote,
}

/// A tool input schema.
///
/// Deserialization accepts the JSON Schema forms an MCP server may send and
/// stores the canonical shape the v22 format defines: a `type` array of one
/// name becomes that name, two or more names become `anyOf` branches (under
/// `allOf` when the node already has an `anyOf`), and a boolean subschema
/// becomes `{}` or `{"not": {}}`. Keywords the struct does not type, such as
/// `anyOf` branches or `$defs`, stay verbatim in `extensions` and are never
/// walked. A rejected node's error names its path under `inputSchema`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonSchema {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub schema_type: Option<SchemaType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<std::collections::BTreeMap<String, JsonSchema>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Box<JsonSchema>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<Vec<String>>,
    #[serde(rename = "enum", skip_serializing_if = "Option::is_none")]
    pub enum_values: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(flatten)]
    pub extensions: JsonObject,
}

impl<'de> Deserialize<'de> for JsonSchema {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        Self::from_value(value).map_err(serde::de::Error::custom)
    }
}

/// A rejected schema node: its path under the tool's `inputSchema`, and the
/// reason it was rejected.
struct SchemaError {
    path: String,
    reason: String,
}
impl SchemaError {
    fn new(path: &str, reason: impl Into<String>) -> Self {
        Self {
            path: path.to_owned(),
            reason: reason.into(),
        }
    }
}
impl std::fmt::Display for SchemaError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.path.is_empty() {
            write!(formatter, "inputSchema: {}", self.reason)
        } else {
            write!(formatter, "inputSchema.{}: {}", self.path, self.reason)
        }
    }
}

impl JsonSchema {
    fn empty() -> Self {
        Self {
            schema_type: None,
            properties: None,
            items: None,
            required: None,
            enum_values: None,
            description: None,
            extensions: JsonObject::new(),
        }
    }

    fn from_value(value: serde_json::Value) -> Result<Self, SchemaError> {
        match value {
            serde_json::Value::Object(object) => Self::from_object(object, ""),
            other => Err(SchemaError::new(
                "",
                format!(
                    "invalid type: {}, expected a JSON Schema object",
                    kind(&other)
                ),
            )),
        }
    }

    /// A node the struct types as a schema: an object, or the boolean
    /// subschema form `true` / `false`.
    fn from_subschema(value: serde_json::Value, path: &str) -> Result<Self, SchemaError> {
        match value {
            serde_json::Value::Bool(true) => Ok(Self::empty()),
            serde_json::Value::Bool(false) => {
                let mut schema = Self::empty();
                schema
                    .extensions
                    .insert("not".into(), serde_json::Value::Object(JsonObject::new()));
                Ok(schema)
            }
            serde_json::Value::Object(object) => Self::from_object(object, path),
            other => Err(SchemaError::new(
                path,
                format!(
                    "invalid type: {}, expected a schema object or a boolean",
                    kind(&other)
                ),
            )),
        }
    }

    fn from_object(object: JsonObject, path: &str) -> Result<Self, SchemaError> {
        let mut schema = Self::empty();
        let mut names = None;
        for (key, value) in object {
            let field = child(path, &key);
            match key.as_str() {
                "type" => names = Some(type_names(value, &field)?),
                "properties" => {
                    let serde_json::Value::Object(properties) = value else {
                        return Err(SchemaError::new(
                            &field,
                            format!("invalid type: {}, expected a map", kind(&value)),
                        ));
                    };
                    let mut schemas = std::collections::BTreeMap::new();
                    for (name, value) in properties {
                        schemas.insert(
                            name.clone(),
                            Self::from_subschema(value, &child(&field, &name))?,
                        );
                    }
                    schema.properties = Some(schemas);
                }
                "items" => schema.items = Some(Box::new(Self::from_subschema(value, &field)?)),
                "required" => schema.required = Some(typed(value, &field)?),
                "enum" => schema.enum_values = Some(typed(value, &field)?),
                "description" => schema.description = Some(typed(value, &field)?),
                _ => {
                    schema.extensions.insert(key, value);
                }
            }
        }
        if let Some(names) = names {
            schema.apply_type_names(names, path)?;
        }
        Ok(schema)
    }

    /// Drops `type` and encodes the names as `anyOf` branches, keeping every
    /// other keyword a sibling of that node.
    fn apply_type_names(&mut self, names: Vec<SchemaType>, path: &str) -> Result<(), SchemaError> {
        if names.len() == 1 {
            self.schema_type = Some(names[0]);
            return Ok(());
        }
        let branches = serde_json::Value::Array(
            names
                .into_iter()
                .map(|name| serde_json::json!({ "type": name }))
                .collect(),
        );
        if !self.extensions.contains_key("anyOf") {
            self.extensions.insert("anyOf".into(), branches);
            return Ok(());
        }
        let node = serde_json::json!({ "anyOf": branches });
        match self.extensions.get_mut("allOf") {
            None => {
                self.extensions
                    .insert("allOf".into(), serde_json::Value::Array(vec![node]));
                Ok(())
            }
            Some(serde_json::Value::Array(all_of)) => {
                all_of.push(node);
                Ok(())
            }
            Some(_) => Err(SchemaError::new(
                &child(path, "allOf"),
                "invalid type: expected an array of schemas",
            )),
        }
    }
}

fn child(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_owned()
    } else {
        format!("{path}.{key}")
    }
}

fn kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "sequence",
        serde_json::Value::Object(_) => "map",
    }
}

fn typed<T: serde::de::DeserializeOwned>(
    value: serde_json::Value,
    path: &str,
) -> Result<T, SchemaError> {
    serde_json::from_value(value).map_err(|error| SchemaError::new(path, error.to_string()))
}

/// `type` as a name or an array of names, rejecting an empty array, a
/// repeated name and a name outside the seven JSON Schema types.
fn type_names(value: serde_json::Value, path: &str) -> Result<Vec<SchemaType>, SchemaError> {
    let names = match value {
        serde_json::Value::String(name) => vec![type_name(name, path)?],
        serde_json::Value::Array(values) => {
            let mut names = Vec::with_capacity(values.len());
            for (index, value) in values.into_iter().enumerate() {
                let item = format!("{path}[{index}]");
                let serde_json::Value::String(name) = value else {
                    return Err(SchemaError::new(
                        &item,
                        format!("invalid type: {}, expected a type name", kind(&value)),
                    ));
                };
                names.push(type_name(name, &item)?);
            }
            names
        }
        other => {
            return Err(SchemaError::new(
                path,
                format!(
                    "invalid type: {}, expected a type name or an array of type names",
                    kind(&other)
                ),
            ));
        }
    };
    if names.is_empty() {
        return Err(SchemaError::new(path, "a type array must name a type"));
    }
    if let Some(index) = (1..names.len()).find(|index| names[..*index].contains(&names[*index])) {
        return Err(SchemaError::new(
            &format!("{path}[{index}]"),
            format!("duplicate type name `{}`", type_label(names[index])),
        ));
    }
    Ok(names)
}

fn type_name(name: String, path: &str) -> Result<SchemaType, SchemaError> {
    serde_json::from_value(serde_json::Value::String(name))
        .map_err(|error| SchemaError::new(path, error.to_string()))
}

fn type_label(schema_type: SchemaType) -> &'static str {
    match schema_type {
        SchemaType::Object => "object",
        SchemaType::Array => "array",
        SchemaType::String => "string",
        SchemaType::Number => "number",
        SchemaType::Integer => "integer",
        SchemaType::Boolean => "boolean",
        SchemaType::Null => "null",
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ToolMetadata {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub version: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub author: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub docs_url: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub source_plugin_id: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub mcp_server_id: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub mcp_original_tool_name: Option<String>,
    #[serde(flatten)]
    pub extensions: JsonObject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolPreExecutePaths {
    pub selector: ToolFilePathSelector,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolFileChangeDeclaration {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub pre_execute_paths: Option<ToolPreExecutePaths>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub post_execute_paths: Option<ToolFilePathSelector>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: JsonSchema,
    pub source: ToolSource,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub risk: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub metadata: Option<ToolMetadata>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub file_changes: Option<ToolFileChangeDeclaration>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: JsonObject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackgroundTaskInfo {
    pub task_id: String,
    pub wake_policy: WakePolicy,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub output_path: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BlobRef {
    pub blob_id: String,
    pub session_id: String,
    pub kind: BlobKind,
    pub path: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub r#ref: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub mime_type: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    #[serde(serialize_with = "crate::finite_number::serialize_optional")]
    pub size_bytes: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolResult {
    pub content: ToolContent,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub structured_content: Option<JsonObject>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub is_error: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub blob_refs: Option<Vec<BlobRef>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub background_task: Option<BackgroundTaskInfo>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub context_messages: Option<Vec<Message>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub effects: Option<Vec<ToolEffect>>,
}

/// What a call did, declared by the tool on its own result
/// (session boundary A.6). The vocabulary is closed; anything else needs a
/// requirements change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum ToolEffect {
    /// The call wrote the file at `path`, the absolute resolved path the
    /// result reports.
    FileWritten { path: String },
    /// The call ran `command`. At most one of `exit_code` and `signal` is
    /// present; both absent mean the tool observed no exit status.
    CommandRun {
        command: String,
        #[serde(
            rename = "exitCode",
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional_non_null"
        )]
        exit_code: Option<i32>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional_non_null"
        )]
        signal: Option<String>,
    },
}

impl ToolResult {
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            content: ToolContent::Text(content.into()),
            structured_content: None,
            is_error: None,
            blob_refs: None,
            background_task: None,
            context_messages: None,
            effects: None,
        }
    }
    pub fn error(content: impl Into<String>) -> Self {
        Self {
            is_error: Some(true),
            ..Self::text(content)
        }
    }
}

impl Default for ToolResult {
    fn default() -> Self {
        Self::text("")
    }
}

impl ToolResult {
    /// The effects the narrative projection tells for this result: each declared effect
    /// shortened by [`ToolEffect::shortened`], and `None` when the result
    /// declared none.
    pub fn shortened_effects(&self) -> Option<Vec<ToolEffect>> {
        self.effects
            .as_ref()
            .filter(|effects| !effects.is_empty())
            .map(|effects| effects.iter().map(ToolEffect::shortened).collect())
    }
}

/// Characters of a command a read page or the compaction digest keeps before
/// the cut is marked (ContextCompaction §5.10).
const SHORTENED_COMMAND_CHARS: usize = 200;

impl ToolEffect {
    /// The effect as a reader sees it: a command keeps its first
    /// [`SHORTENED_COMMAND_CHARS`] characters, counted before escaping, and a
    /// longer one is cut there and marked with a trailing `…`; a path stays
    /// verbatim, an exact identifier. The cut is ContextCompaction §5.10's
    /// rule for the digest and the timeline branches design's rule for a read
    /// page. The digest and the branch read page call it directly, and the
    /// narrative projection calls it through [`ToolResult::shortened_effects`],
    /// so no reader can drift.
    pub fn shortened(&self) -> ToolEffect {
        match self {
            ToolEffect::CommandRun {
                command,
                exit_code,
                signal,
            } => {
                let command = match command.char_indices().nth(SHORTENED_COMMAND_CHARS) {
                    Some((index, _)) => format!("{}…", &command[..index]),
                    None => command.clone(),
                };
                ToolEffect::CommandRun {
                    command,
                    exit_code: *exit_code,
                    signal: signal.clone(),
                }
            }
            other => other.clone(),
        }
    }
}
