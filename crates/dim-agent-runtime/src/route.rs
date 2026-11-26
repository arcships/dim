//! Calls by id to a live owner's data face
//! ([ADR 027](../../../docs/adr/027-cross-scope-access-by-id.md) D1–D4, [#2127](https://github.com/arcships/dim/issues/2127)).
//!
//! A caller outside a scope holds no instance key, so it names what it wants,
//! and the owner of the target holds the index and does the lookup:
//!
//! - a Session's [`SessionScope`] resolves one of its own lines and its own
//!   Session faces, for the Session's parts and their runtimes; it holds no
//!   Session id, so a caller cannot name another Session, and the scope itself
//!   is not one of the faces it resolves;
//! - Agents' `sessions` resolves a Session, and a line of it, for the process
//!   (`dim-app-server`): it finds the Session — loading it for the call when it
//!   is not loaded (D5) — and then calls [`SessionScope::line_call`] or
//!   [`SessionScope::call`] in that instance.
//!
//! Three data operations carry every routed call, with the same arguments in
//! every language:
//!
//! - [`SessionScope::line_call`] — `{lineId, service, method, args}`;
//! - [`SessionScope::call`] — `{service, method, args}`, for the Session's own
//!   faces;
//! - `sessions.call` — `{sessionId, lineId?, service, method, args}`, which
//!   takes the same two forms for a process caller.
//!
//! Each call looks the target up again, so a caller never keeps a live target
//! (D4). Rust calls the same two operations: it builds `args` from the target
//! operation's request type and reads the reply as its reply type.
//!
//! What a call resolves is the target scope's data face in the catalog. A name
//! the assembly marks `internal(reason)`, a mechanism registration point
//! (whose entries belong to the registering generation) and a scope lifecycle
//! service are refused as [`RouteError::NotDataFace`]; a data face of the
//! other scope is [`RouteError::WrongScope`]. Everything else about the call
//! belongs to the owner: the route passes the method and its arguments through
//! and returns the owner's reply, or the owner's own failure as it wrote it.
//!
//! Only the process path can find a Session that has no live instance: Agents
//! loads it for the call (D5), so [`RouteError::NoSession`] means the Session
//! does not exist, or is closed and cannot load again. A Session that is
//! closing answers [`RouteError::SessionClosing`], and a line target answers it
//! too, because closing the Session closes its lines. A line that is closing or
//! closed, or that belongs to another Session, answers [`RouteError::NoLine`];
//! that is final for the line: routing never retries it.

use crate::*;
use rutis::Ctx;
use rutis::host::CtxHost;
use rutis_bridge::session::{
    Error as BridgeError, HostDispatch, Value as BridgeValue, decode_value, host_key_in, settle,
};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Weak};

/// What a route answers: the target's reply, or why it did not reach one.
pub type Routed<T> = std::result::Result<T, RouteError>;

/// Why a routed call did not reach a reply.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, thiserror::Error)]
#[serde(rename_all = "camelCase")]
pub enum RouteError {
    /// The Session is not there, has no owner, or is closed and cannot load
    /// again. Only a process caller receives it; Agents loads and holds a
    /// Session that is merely not loaded (ADR 027 D5), so this is final.
    #[error("no live Session {0}")]
    NoSession(String),
    /// The Session instance is closing, and so is a call to one of its lines.
    /// A Session-scoped caller ends its work, and a process caller reads the
    /// same answer as `session_closing` and ends the work it was doing.
    #[error("Session {0} is closing")]
    SessionClosing(String),
    /// Agents is reloading and cannot take calls yet. A process caller retries
    /// later.
    #[error("Agents is reloading: {0}")]
    SessionReloading(String),
    /// The Session exists but cannot be loaded now: its restore failed, or the
    /// Agent that owns it is unavailable. The caller reports the reason and
    /// does not retry.
    #[error("Session {session} is unavailable: {reason}")]
    SessionUnavailable { session: String, reason: String },
    /// The Session is live but the line is closing, closed, or not one of its
    /// lines. Final for that line: no hold and no retry.
    #[error("no line {line} in Session {session}")]
    NoLine { session: String, line: String },
    /// The name is not a data face a route resolves there: the assembly marks
    /// it internal, it is a mechanism registration point, or it is a scope
    /// lifecycle service. A permanent answer: the request is wrong.
    #[error("{service} is not a routable data face")]
    NotDataFace { service: String },
    /// The service is a data face a route resolves, but this operation of it
    /// is not: its call needs a cancellation or a cleanup the routed call
    /// cannot carry, so only a direct call keeps its lifetime behaviour.
    #[error("{service}.{method} is not a routable operation")]
    MethodNotRoutable { service: String, method: String },
    /// The name is a data face, but not of the scope the route reaches.
    #[error("{service} is not a {group} service")]
    WrongScope { service: String, group: String },
    /// The owner's own failure, with the name, the code it reported and the
    /// message it wrote; routing does not restate them. `reloading` is that
    /// code's "try again later", so a caller retries exactly what the owner
    /// says it may retry.
    #[error("{message}")]
    Owner {
        name: String,
        code: Option<String>,
        message: String,
        /// The owner's failure as data, when its error type carries a serde
        /// value: a caller can act on the same structure a direct call would
        /// read, instead of parsing the message.
        data: Option<Box<serde_json::Value>>,
        reloading: bool,
    },
}

impl RouteError {
    /// The code a caller matches on, as the `sessions` envelope carries it.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NoSession(_) => "no_session",
            Self::SessionClosing(_) => "session_closing",
            Self::SessionReloading(_) => "session_reloading",
            Self::SessionUnavailable { .. } => "session_unavailable",
            Self::NoLine { .. } => "no_line",
            Self::NotDataFace { .. } => "not_data_face",
            Self::MethodNotRoutable { .. } => "method_not_routable",
            Self::WrongScope { .. } => "wrong_scope",
            Self::Owner { .. } => "owner",
        }
    }
}

/// A call to one of the caller's own Session's lines.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LineCall {
    pub line_id: String,
    pub service: String,
    pub method: String,
    /// The method's arguments, in order.
    #[serde(default)]
    pub args: Vec<serde_json::Value>,
}

impl LineCall {
    /// The call to `service.method(args...)` in the line `line_id`.
    pub fn new(
        line_id: impl Into<String>,
        service: impl Into<String>,
        method: impl Into<String>,
        args: Vec<serde_json::Value>,
    ) -> Self {
        Self {
            line_id: line_id.into(),
            service: service.into(),
            method: method.into(),
            args,
        }
    }
}

/// A call to one of the caller's own Session's data faces.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionCall {
    pub service: String,
    pub method: String,
    /// The method's arguments, in order.
    #[serde(default)]
    pub args: Vec<serde_json::Value>,
}

impl SessionCall {
    /// The call to `service.method(args...)` in this Session.
    pub fn new(
        service: impl Into<String>,
        method: impl Into<String>,
        args: Vec<serde_json::Value>,
    ) -> Self {
        Self {
            service: service.into(),
            method: method.into(),
            args,
        }
    }
}

/// Names routing does not resolve: the mechanism's registration points, whose
/// entries belong to the part that registered them and are reachable only
/// inside that generation
/// ([ADR 027](../../../docs/adr/027-cross-scope-access-by-id.md) D3), and the
/// scope lifecycle services the runtime provides.
/// The code a marked Rust service reports for `AgentError::Reloading`: the
/// owner's own word for "try again later".
const RELOADING: &str = "Reloading";

/// Operations a route does not resolve even though their service is a data
/// face: each takes a function, or hands the caller a subscription or a
/// cancellation to settle, and a routed call carries data only. The list is
/// maintained by hand; the generated authoring requests read this same
/// constant, so the runtime and the declarations cannot disagree.
pub const LIFECYCLE_METHODS: [(&str, &str); 8] = [
    ("dispatcher", "consume"),
    ("dispatcher", "listen"),
    ("permission", "check"),
    ("toolHost", "execute"),
    ("treeCommands", "create"),
    ("uiResources", "publishFile"),
    ("uiResources", "withdraw"),
    ("wait", "wait"),
];

/// The Session's own scope resolves a line and the Session's faces; it is not
/// one of the faces a routed call resolves, so `sessionScope.call` cannot reach
/// `sessionScope.lineCall`. Session parts inject this Session's services
/// directly instead.
const NOT_CALLABLE: [&str; 1] = ["sessionScope"];

/// The scope lifecycle services: the runtime provides them, and a route does
/// not hand them to a caller.
const LIFECYCLE_SERVICES: [&str; 2] = ["sessionLifecycle", "branchLifecycle"];

const REGISTRATION_POINTS: [&str; 5] = [
    "sessionMethods",
    "toolRegistry",
    "promptRegistry",
    "guideEntries",
    "submitChecks",
];

/// One Session's own lines and data faces, provided to the Session's parts and
/// their runtimes as the Session name `sessionScope`.
///
/// It holds its Session and no other: a part of a Session reaches its own
/// Session's lines only. `describe` joins it with base-3.
pub struct SessionScope {
    session: Weak<crate::assembly::SessionAssembly>,
    session_id: String,
}

impl SessionScope {
    /// The scope of the Session `session` is publishing.
    pub(crate) fn new(session: &Arc<crate::assembly::SessionAssembly>) -> Self {
        Self {
            session: Arc::downgrade(session),
            session_id: session.id.clone(),
        }
    }

    fn live(&self) -> Routed<Arc<crate::assembly::SessionAssembly>> {
        let closing = || RouteError::SessionClosing(self.session_id.clone());
        let session = self.session.upgrade().ok_or_else(closing)?;
        if session.closing.is_cancelled() {
            return Err(closing());
        }
        Ok(session)
    }

    /// The live line `line_id` of this Session. A Session that started closing
    /// while the line was being resolved answers `SessionClosing`, not
    /// `NoLine`: closing the Session closes its lines.
    fn line_context(&self, line_id: &str) -> Routed<Arc<TimelineContext>> {
        let session = self.live()?;
        let line = session.line(line_id);
        // A Session that started closing while the line was being selected —
        // or whose loaded scope went with that close — answers `SessionClosing`:
        // its lines close with it.
        match self.live() {
            Ok(_) => {}
            Err(closing) => return Err(closing),
        }
        line.ok_or_else(|| RouteError::NoLine {
            session: self.session_id.clone(),
            line: line_id.to_owned(),
        })
        .and_then(|line| match self.live() {
            Err(closing @ RouteError::SessionClosing(_)) => Err(closing),
            _ => Ok(line),
        })
    }
}

/// A Session with the router its owning instance provides: what Agents routes
/// through. Only the runtime builds one, when a Session is loaded, so a crate
/// that merely holds a [`Session`] has no path to the router (ADR 027 appendix
/// F). A part of another language reaches the same router as the
/// `sessionScope` data face.
///
/// A bare [`Session`] cannot reach the router in any of these ways:
///
/// ```compile_fail
/// use dim_agent_runtime::Session;
/// fn read(session: &Session) {
///     let _ = session.routed();
/// }
/// ```
///
/// ```compile_fail
/// use dim_agent_runtime::{routed_scope, Session};
/// fn read(session: &Session) {
///     let _ = routed_scope(session);
/// }
/// ```
///
/// ```compile_fail
/// use dim_agent_runtime::RoutedSession;
/// fn build() {
///     let _ = RoutedSession::new(todo!(), todo!());
/// }
/// ```
#[derive(Clone)]
pub struct RoutedSession {
    session: Session,
    scope: Arc<SessionScope>,
}

impl RoutedSession {
    pub(crate) fn new(session: Session, scope: Arc<SessionScope>) -> Self {
        Self { session, scope }
    }
    pub fn session(&self) -> &Session {
        &self.session
    }
    pub fn scope(&self) -> &Arc<SessionScope> {
        &self.scope
    }
}

#[rutis::service]
impl SessionScope {
    /// Calls one of this Session's own data faces.
    pub async fn call(&self, call: SessionCall) -> Routed<Option<serde_json::Value>> {
        let session = self.live()?;
        let (catalog, faces) = session.reach();
        face(catalog, faces, &call.service, &call.method, SESSION_GROUP)?;
        let loaded = session.loaded().map_err(|error| {
            // The loaded scope goes away when the Session closes, so that
            // answer wins over the owner's wrapper.
            if session.closing.is_cancelled() {
                RouteError::SessionClosing(self.session_id.clone())
            } else {
                owner(error)
            }
        })?;
        call_face(
            &loaded.ctx,
            loaded.scope.instance,
            &call.service,
            &call.method,
            &call.args,
        )
        .await
    }

    /// Calls one of this Session's lines' data faces.
    pub async fn line_call(&self, call: LineCall) -> Routed<Option<serde_json::Value>> {
        let line = self.line_context(&call.line_id)?;
        face(
            line.catalog(),
            line.faces(),
            &call.service,
            &call.method,
            BRANCH_GROUP,
        )?;
        call_face(
            line.ctx(),
            line.scope_instance(),
            &call.service,
            &call.method,
            &call.args,
        )
        .await
    }
}

/// Whether a route resolves `service` as a data face of `group` (module docs).
/// The generated authoring types and the runtime's own answer must agree, and
/// this is the predicate a check compares them with.
pub fn resolves(
    catalog: &names::ServiceCatalog,
    faces: &Faces,
    service: &str,
    group: &str,
) -> bool {
    face(catalog, faces, service, "", group).is_ok()
}

/// Fails unless a route may resolve `service` as a data face of `group`
/// (module docs).
pub(crate) fn face(
    catalog: &names::ServiceCatalog,
    faces: &Faces,
    service: &str,
    method: &str,
    group: &str,
) -> Routed<()> {
    if LIFECYCLE_METHODS
        .iter()
        .any(|(refused_service, refused_method)| {
            *refused_service == service && *refused_method == method
        })
    {
        return Err(RouteError::MethodNotRoutable {
            service: service.into(),
            method: method.into(),
        });
    }
    // Only a name shared across languages is a data face; a native-only
    // registration stays inside its scope.
    if !catalog.is_shared(service) {
        return Err(RouteError::NotDataFace {
            service: service.into(),
        });
    }
    if REGISTRATION_POINTS.contains(&service)
        || LIFECYCLE_SERVICES.contains(&service)
        || (group == SESSION_GROUP && NOT_CALLABLE.contains(&service))
        || faces.reason(service).is_some()
    {
        return Err(RouteError::NotDataFace {
            service: service.into(),
        });
    }
    names::named_in_group(catalog, service, group).map_err(|_| RouteError::WrongScope {
        service: service.into(),
        group: group.into(),
    })
}

/// Calls `method` of the data face `service` in `instance`, as another
/// language would: the arguments travel as data and the reply comes back as
/// data.
pub(crate) async fn call_face(
    ctx: &Ctx,
    instance: rutis::host::InstanceId,
    service: &str,
    method: &str,
    args: &[serde_json::Value],
) -> Routed<Option<serde_json::Value>> {
    let dispatch = ctx
        .get_at::<dyn HostDispatch>(&host_key_in(service, instance))
        .ok_or_else(|| RouteError::Owner {
            name: service.into(),
            code: None,
            message: format!("{service} is not provided now"),
            data: None,
            reloading: false,
        })?;
    let args = BridgeValue::List(
        args.iter()
            .map(|arg| BridgeValue::Data(arg.clone()))
            .collect(),
    );
    let reply = dispatch.invoke(method, args).map_err(owner)?;
    let value = settle(reply).await.map_err(owner)?;
    reply_value(value)
}

/// A method that answers nothing crosses as `undefined`; `null` is a value the
/// owner chose to reply with, and a caller reads the two differently.
fn reply_value(value: BridgeValue) -> Routed<Option<serde_json::Value>> {
    if matches!(value, BridgeValue::Undefined) {
        return Ok(None);
    }
    decode_value::<serde_json::Value>(value)
        .map(Some)
        .map_err(owner)
}

/// The owner's failure as the target wrote it, under the name it carries.
pub(crate) fn owner(error: impl Into<Failure>) -> RouteError {
    let failure = error.into();
    RouteError::Owner {
        name: failure.name,
        code: failure.code,
        message: failure.message,
        data: failure.data,
        reloading: failure.reloading,
    }
}

/// A failure's name and message, from either error surface a read answers
/// with.
pub(crate) struct Failure {
    name: String,
    code: Option<String>,
    message: String,
    data: Option<Box<serde_json::Value>>,
    reloading: bool,
}

impl From<AgentError> for Failure {
    fn from(error: AgentError) -> Self {
        Self {
            name: "AgentError".into(),
            code: Some(agent_code(&error).to_owned()),
            reloading: matches!(error, AgentError::Reloading(_)),
            message: error.to_string(),
            data: None,
        }
    }
}

/// The code a marked Rust service reports for an `AgentError`: its variant name,
/// spelled the way the generated declarations spell it.
fn agent_code(error: &AgentError) -> &'static str {
    match error {
        AgentError::InvalidInput(_) => "InvalidInput",
        AgentError::Closed(_) => "Closed",
        AgentError::UnknownBranch(_) => "UnknownBranch",
        AgentError::BranchEnded(_) => "BranchEnded",
        AgentError::BranchRefused { .. } => "BranchRefused",
        AgentError::Storage(_) => "Storage",
        AgentError::PublicationUnknown(_) => "PublicationUnknown",
        AgentError::Build(_) => "Build",
        AgentError::Cancelled(_) => "Cancelled",
        AgentError::Reloading(_) => "Reloading",
        AgentError::AmbiguousSubmission(_) => "AmbiguousSubmission",
        AgentError::PartialSubmission(_) => "PartialSubmission",
        AgentError::Fault { .. } => "Fault",
        AgentError::Framework { .. } => "Framework",
        AgentError::Multiple(_) => "Multiple",
    }
}

impl From<BridgeError> for Failure {
    fn from(error: BridgeError) -> Self {
        match error {
            error @ (BridgeError::Remote { .. } | BridgeError::Binding { .. }) => {
                // The owner's own code says whether it may be retried; a Rust
                // service reports a reload as `Reloading` (`AgentError`'s variant
                // code), and the wire keeps it in the error graph.
                let reloading = error.code() == Some(RELOADING);
                let code = error.code().map(str::to_owned);
                let data = error.data().map(Box::new);
                let name = match &error {
                    BridgeError::Remote { name, .. } => name.clone(),
                    BridgeError::Binding { code, .. } => code.unwrap_or("BindingError").into(),
                    other => other.to_string(),
                };
                match error {
                    BridgeError::Remote { message, .. } | BridgeError::Binding { message, .. } => {
                        Self {
                            code,
                            reloading,
                            data,
                            name,
                            message,
                        }
                    }
                    other => Self {
                        code,
                        reloading,
                        data,
                        name,
                        message: other.to_string(),
                    },
                }
            }
            other => Self {
                name: "BridgeError".into(),
                code: None,
                message: other.to_string(),
                data: None,
                reloading: false,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_void_reply_is_undefined_and_a_null_reply_is_a_value() {
        assert_eq!(reply_value(BridgeValue::Undefined).unwrap(), None);
        assert_eq!(
            reply_value(serde_json::Value::Null.into()).unwrap(),
            Some(serde_json::Value::Null)
        );
    }

    #[test]
    fn an_owner_failure_keeps_its_code() {
        let remote = |code: &str| BridgeError::Remote {
            name: "AgentError".into(),
            message: format!("{code}: absent"),
            graph: Some(serde_json::json!({
                "root": {"type": "reference", "value": 0},
                "nodes": [{"type": "error", "name": "AgentError", "code": code, "message": "absent"}],
            })),
        };
        let routed = owner(remote("UnknownBranch"));
        let RouteError::Owner {
            code, reloading, ..
        } = &routed
        else {
            panic!("the owner's failure crosses as Owner: {routed:?}");
        };
        assert_eq!(code.as_deref(), Some("UnknownBranch"), "{routed:?}");
        assert!(!reloading, "{routed:?}");
        assert_eq!(
            agent_code(&AgentError::UnknownBranch("absent".into())),
            "UnknownBranch"
        );
    }

    #[test]
    fn an_owner_failure_is_retryable_only_when_its_code_says_reloading() {
        let reloading = owner(AgentError::Reloading("timeline".into()));
        assert!(
            matches!(
                reloading,
                RouteError::Owner {
                    reloading: true,
                    ..
                }
            ),
            "{reloading:?}"
        );
        let permanent = owner(BridgeError::Binding {
            code: Some("not_reloadable"),
            message: "a permanent failure whose name mentions reload".into(),
            data: None,
        });
        assert!(
            matches!(
                permanent,
                RouteError::Owner {
                    reloading: false,
                    ..
                }
            ),
            "{permanent:?}"
        );
        // The production path of a Rust service's `Err`: a `Remote` error
        // whose graph carries the owner's own code.
        let remote = |code: &str, message: &str| BridgeError::Remote {
            name: "AgentError".into(),
            message: message.into(),
            graph: Some(serde_json::json!({
                "root": {"type": "reference", "value": 0},
                "nodes": [{"type": "error", "name": "AgentError", "code": code, "message": message}],
            })),
        };
        let reloading = owner(remote(RELOADING, "timeline is reloading"));
        assert!(
            matches!(
                reloading,
                RouteError::Owner {
                    reloading: true,
                    ..
                }
            ),
            "{reloading:?}"
        );
        let permanent = owner(remote(
            "not_reloadable",
            "a permanent failure whose name mentions reload",
        ));
        assert!(
            matches!(
                permanent,
                RouteError::Owner {
                    reloading: false,
                    ..
                }
            ),
            "{permanent:?}"
        );
    }
}
