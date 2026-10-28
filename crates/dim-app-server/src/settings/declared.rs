//! Declared sources (settings-service §3–§4): JSON documents checked against
//! the owner's JSON Schema, the `settings` service owners register and use,
//! and the client's `settings.*` management methods.
use super::*;
use rutis_bridge::session::{Error, HostDispatch, Reply, Value as RpcValue, host_key};
use serde_json::{Value, json};

/// A JSON Schema source an owner registers (§3.1).
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceDeclaration {
    pub source: String,
    pub user_file: String,
    /// Present when the source also has project scope.
    #[serde(default)]
    pub project_file: Option<String>,
    /// JSON Schema (draft 2020-12) whose root is `type: object`.
    pub schema: Value,
    /// JSON Pointers to members reachable through `properties`.
    #[serde(default)]
    pub user_only: Vec<String>,
}

/// The two write paths: an owner write may not change a user-only field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Writer {
    Owner,
    Client,
}

pub(super) struct Declared {
    declaration: SourceDeclaration,
    validator: jsonschema::Validator,
}
impl Declared {
    /// Checks a declaration once, where it enters the host.
    pub(super) fn compile(declaration: SourceDeclaration) -> std::result::Result<Self, String> {
        let error = |error: SettingsError| error.to_string();
        validate_namespace(&declaration.source).map_err(error)?;
        validate_file(&declaration.user_file).map_err(error)?;
        if let Some(file) = &declaration.project_file {
            validate_file(file).map_err(error)?;
        }
        if declaration.schema.get("type") != Some(&json!("object")) {
            return Err("schema root must be `type: object`".into());
        }
        let validator = jsonschema::draft202012::new(&declaration.schema)
            .map_err(|error| format!("schema does not compile: {}", error.masked()))?;
        for pointer in &declaration.user_only {
            if !reachable(&declaration.schema, pointer) {
                return Err(format!(
                    "user-only pointer {pointer} does not name a declared member"
                ));
            }
        }
        let declared = Self {
            declaration,
            validator,
        };
        declared
            .prepare(json!({}))
            .map_err(|error| format!("default document is invalid: {error}"))?;
        Ok(declared)
    }
    pub(super) fn declaration(&self) -> &SourceDeclaration {
        &self.declaration
    }
    pub(super) fn files(&self) -> (String, Option<String>) {
        (
            self.declaration.user_file.clone(),
            self.declaration.project_file.clone(),
        )
    }
    /// Defaults, then the schema (§3.2).
    fn prepare(&self, mut document: Value) -> std::result::Result<Value, String> {
        if !document.is_object() {
            return Err("document must be an object".into());
        }
        fill(&self.declaration.schema, &mut document);
        if let Some(error) = self.validator.iter_errors(&document).next() {
            return Err(format!("{}: {}", error.instance_path(), error.masked()));
        }
        Ok(document)
    }
    /// An owner write fails at the first user-only member it changes.
    fn user_only(&self, current: &Value, next: &Value) -> Result<()> {
        match self
            .declaration
            .user_only
            .iter()
            .find(|pointer| current.pointer(pointer) != next.pointer(pointer))
        {
            Some(pointer) => Err(SettingsError::UserOnlyField(pointer.clone())),
            None => Ok(()),
        }
    }
}
impl Format for Declared {
    type Document = Value;
    fn source(&self) -> &str {
        &self.declaration.source
    }
    fn decode(&self, bytes: Option<&[u8]>) -> Result<Value> {
        let document = match bytes {
            Some(bytes) => serde_json::from_slice(bytes)
                .map_err(|_| SettingsError::Invalid("invalid settings document"))?,
            None => json!({}),
        };
        self.prepare(document).map_err(|_| {
            SettingsError::Invalid("persisted settings document does not match its schema")
        })
    }
}
/// The document an agent may read (§1.2): every member reachable through
/// `properties` whose schema has `writeOnly: true` is removed, and the JSON
/// Pointers of those that held a value are returned in schema order.
pub fn without_write_only(schema: &Value, mut document: Value) -> (Value, Vec<String>) {
    fn hide(schema: &Value, value: &mut Value, at: &str, set: &mut Vec<String>) {
        let (Some(properties), Value::Object(members)) =
            (schema.get("properties").and_then(Value::as_object), value)
        else {
            return;
        };
        for (name, property) in properties {
            let path = format!("{at}/{}", name.replace('~', "~0").replace('/', "~1"));
            if property.get("writeOnly") == Some(&Value::Bool(true)) {
                if members.remove(name).is_some() {
                    set.push(path);
                }
            } else if let Some(member) = members.get_mut(name) {
                hide(property, member, &path, set);
            }
        }
    }
    let mut set = Vec::new();
    hide(schema, &mut document, "", &mut set);
    (document, set)
}

/// Whether `schema`, or a member reachable from it through `properties`, has
/// `writeOnly: true`: an agent may not write a value there (§1.2).
pub fn holds_write_only(schema: &Value) -> bool {
    schema.get("writeOnly") == Some(&Value::Bool(true))
        || schema
            .get("properties")
            .and_then(Value::as_object)
            .is_some_and(|properties| properties.values().any(holds_write_only))
}

/// Fills missing members that have a `default`, through `properties` only.
fn fill(schema: &Value, value: &mut Value) {
    let (Some(properties), Value::Object(members)) =
        (schema.get("properties").and_then(Value::as_object), value)
    else {
        return;
    };
    for (name, property) in properties {
        if let Some(default) = property.get("default")
            && !members.contains_key(name)
        {
            members.insert(name.clone(), default.clone());
        }
        if let Some(member) = members.get_mut(name) {
            fill(property, member);
        }
    }
}
/// Whether an RFC 6901 pointer names an object member through `properties`.
fn reachable(schema: &Value, pointer: &str) -> bool {
    let Some(tokens) = pointer.strip_prefix('/') else {
        return false;
    };
    tokens
        .split('/')
        .try_fold(schema, |schema, token| {
            schema
                .get("properties")?
                .get(token.replace("~1", "/").replace("~0", "~"))
        })
        .is_some()
}

impl SettingsService {
    fn declared(&self, source: &str, scope: SettingsScope) -> Result<(Arc<Declared>, FilePath)> {
        let declared = self
            .sources
            .get(source)
            .and_then(|registered| match &registered.value.kind {
                Kind::Declared(declared) => Some(declared.clone()),
                Kind::Rust(_) => None,
            })
            .ok_or(SettingsError::UnknownSource)?;
        if let SettingsScope::Project(root) = scope {
            if declared.declaration.project_file.is_none() {
                return Err(SettingsError::InvalidScope("source has no project scope"));
            }
            if self.project_path(root).is_err() {
                return Err(SettingsError::InvalidScope("unauthorized project"));
            }
        }
        let path = self.locate(
            &declared.declaration.user_file,
            declared.declaration.project_file.as_deref(),
            scope,
        )?;
        Ok((declared, path))
    }
    /// Reads a declared source; a missing file is the default document.
    pub async fn read_declared(
        &self,
        source: &str,
        scope: SettingsScope,
        cancel: &CancellationToken,
    ) -> Result<SettingsSnapshot<Value>> {
        self.check_open(cancel)?;
        let (declared, path) = self.declared(source, scope)?;
        self.read_source(declared, path, scope, cancel).await
    }
    /// Replaces a declared source if `expected` is its current revision, and
    /// returns the stored document (with defaults).
    pub async fn update_declared(
        self: &Arc<Self>,
        source: &str,
        scope: SettingsScope,
        expected: &str,
        document: Value,
        writer: Writer,
        cancel: &CancellationToken,
    ) -> Result<SettingsSnapshot<Value>> {
        self.check_open(cancel)?;
        let (declared, path) = self.declared(source, scope)?;
        let document = declared
            .prepare(document)
            .map_err(SettingsError::Document)?;
        let rules = declared.clone();
        self.replace(
            declared,
            path,
            scope,
            expected,
            document,
            move |current, next| match writer {
                Writer::Client => Ok(()),
                Writer::Owner => rules.user_only(current, next),
            },
            cancel,
        )
        .await
    }
}

/// `read` requests of both paths.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Read {
    pub(crate) source: String,
    pub(crate) scope: SettingsScope,
}
/// `update` requests of both paths.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Update {
    pub(crate) source: String,
    pub(crate) scope: SettingsScope,
    pub(crate) expected_revision: String,
    pub(crate) document: Value,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Projects {}

/// The service error codes (§6).
fn code(error: &SettingsError) -> &'static str {
    match error {
        SettingsError::UnknownSource => "unknown_source",
        SettingsError::InvalidScope(_) => "invalid_scope",
        SettingsError::Document(_) | SettingsError::Invalid(_) => "invalid_document",
        SettingsError::UserOnlyField(_) => "user_only_field",
        SettingsError::Conflict => "conflict",
        SettingsError::Storage(FileStorageError::Unsynced(_)) => "unsynced",
        SettingsError::Closed => "closed",
        SettingsError::Cancelled => "cancelled",
        SettingsError::Storage(_) | SettingsError::Persistence | SettingsError::Worker(_) => {
            "failed"
        }
    }
}
/// `{ok: true, value}` or `{ok: false, error: {code, message}}`, as `sessions`.
fn envelope(result: Result<impl Serialize>) -> RpcValue {
    match result {
        Ok(value) => json!({"ok": true, "value": value}),
        Err(error) => {
            json!({"ok": false, "error": {"code": code(&error), "message": error.to_string()}})
        }
    }
    .into()
}
fn request<T: DeserializeOwned>(args: RpcValue) -> std::result::Result<T, Error> {
    let invalid = || Error::Value("expected one settings request object".into());
    let Value::Array(args) = args.json()? else {
        return Err(invalid());
    };
    let [request] = <[Value; 1]>::try_from(args).map_err(|_| invalid())?;
    serde_json::from_value(request).map_err(|_| invalid())
}

/// The owner path: `settings` by name for every language (§4.2). Only
/// `register` uses the caller's identity, to withdraw its sources when the
/// calling row unloads; each source has one owner (AGENTS.md rule 11).
struct Dispatch(Arc<SettingsService>);
impl HostDispatch for Dispatch {
    fn methods(&self) -> Option<Value> {
        Some(json!({"register": "async", "read": "async", "update": "async", "projects": "sync"}))
    }
    fn invoke(&self, method: &str, args: RpcValue) -> Reply {
        let service = self.0.clone();
        let cancel = CancellationToken::new();
        match method {
            "register" => {
                let caller = rutis_bridge::session::caller_row()
                    .map(|row| row.ctx().clone())
                    .ok_or_else(|| {
                        Error::Value(
                            "register needs the calling row's context: only a row of a language runtime can call it"
                                .into(),
                        )
                    })?;
                let registered = request(args).and_then(|declaration| {
                    service
                        .register_declared(&caller, declaration)
                        .map(|()| RpcValue::Undefined)
                        .map_err(|error| Error::Value(error.to_string()))
                });
                Ok(RpcValue::future(async move { registered }))
            }
            "projects" => {
                let Projects {} = request(args)?;
                let projects: Vec<_> = service
                    .project_sources()
                    .into_iter()
                    .map(|(root, cwd)| ProjectAuthorized {
                        name: project_authorizations(),
                        root,
                        cwd,
                    })
                    .collect();
                Ok(envelope(Ok(json!({ "projects": projects }))))
            }
            "read" => {
                let Read { source, scope } = request(args)?;
                Ok(RpcValue::future(async move {
                    let _cancel = cancel.clone().drop_guard();
                    Ok(envelope(
                        service.read_declared(&source, scope, &cancel).await,
                    ))
                }))
            }
            "update" => {
                let update: Update = request(args)?;
                Ok(RpcValue::future(async move {
                    let _cancel = cancel.clone().drop_guard();
                    Ok(envelope(
                        service
                            .update_declared(
                                &update.source,
                                update.scope,
                                &update.expected_revision,
                                update.document,
                                Writer::Owner,
                                &cancel,
                            )
                            .await,
                    ))
                }))
            }
            _ => Err(Error::Value(format!("unknown settings method {method}"))),
        }
    }
}

/// Provides the owner service `settings`.
pub(super) fn provide(ctx: &Ctx, service: &Arc<SettingsService>) -> rutis::Result<()> {
    ctx.provide_at(
        host_key("settings"),
        Arc::new(Dispatch(service.clone())) as Arc<dyn HostDispatch>,
    )?;
    Ok(())
}
