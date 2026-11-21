use std::{collections::HashSet, sync::Arc};

pub type Result<T> = std::result::Result<T, AgentError>;

#[derive(Debug, Clone, thiserror::Error)]
pub enum AgentError {
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("{0} is closed")]
    Closed(String),
    #[error("unknown branch: {0}")]
    UnknownBranch(String),
    /// The line has ended: it accepts no input and no new operation.
    #[error("branch has ended: {0}")]
    BranchEnded(String),
    /// The tree refused an operation on a line for the named reason; the
    /// refusal's code is what the protocol answers.
    #[error("{message}")]
    BranchRefused {
        refusal: BranchRefusal,
        message: String,
    },
    #[error("storage: {0}")]
    Storage(String),
    /// A write was accepted, but its publication or required outcome was not
    /// confirmed. Callers must reconcile committed state before retrying.
    #[error("{0}")]
    PublicationUnknown(String),
    #[error("initialization failed: {0}")]
    Build(String),
    #[error("operation cancelled: {0}")]
    Cancelled(String),
    /// A capability the operation needs is reloading; the operation did not run.
    #[error("{0} is reloading")]
    Reloading(String),
    /// More than one input on the branch has this submission id, written
    /// before branches accepted each id once; nothing was written.
    #[error("more than one input has submission id {0}")]
    AmbiguousSubmission(String),
    /// A batch's submission ids are partly committed on the branch and partly
    /// not; nothing was written. The message lists the uncommitted ids.
    #[error("some inputs of the batch have committed on the branch and some have not: {0}")]
    PartialSubmission(String),
    #[error("{component}: {message}")]
    Fault { component: String, message: String },
    #[error("framework: {0}")]
    Framework(#[source] Arc<rutis::host::ErrorKind>),
    #[error("multiple failures: {0:?}")]
    Multiple(Vec<AgentError>),
}

/// A failure reported to a Session. Framework failures carry their diagnostic text.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionFault {
    InvalidInput(String),
    Closed(String),
    UnknownBranch(String),
    BranchEnded(String),
    BranchRefused {
        refusal: BranchRefusal,
        message: String,
    },
    Storage(String),
    PublicationUnknown(String),
    Build(String),
    Cancelled(String),
    Reloading(String),
    AmbiguousSubmission(String),
    PartialSubmission(String),
    Fault {
        component: String,
        message: String,
    },
    Framework(String),
    Multiple(Vec<SessionFault>),
}
impl From<AgentError> for SessionFault {
    fn from(error: AgentError) -> Self {
        match error {
            AgentError::InvalidInput(value) => Self::InvalidInput(value),
            AgentError::Closed(value) => Self::Closed(value),
            AgentError::UnknownBranch(value) => Self::UnknownBranch(value),
            AgentError::BranchEnded(value) => Self::BranchEnded(value),
            AgentError::BranchRefused { refusal, message } => {
                Self::BranchRefused { refusal, message }
            }
            AgentError::Storage(value) => Self::Storage(value),
            AgentError::PublicationUnknown(value) => Self::PublicationUnknown(value),
            AgentError::Build(value) => Self::Build(value),
            AgentError::Cancelled(value) => Self::Cancelled(value),
            AgentError::Reloading(value) => Self::Reloading(value),
            AgentError::AmbiguousSubmission(value) => Self::AmbiguousSubmission(value),
            AgentError::PartialSubmission(value) => Self::PartialSubmission(value),
            AgentError::Fault { component, message } => Self::Fault { component, message },
            AgentError::Framework(value) => Self::Framework(value.to_string()),
            AgentError::Multiple(value) => {
                Self::Multiple(value.into_iter().map(Into::into).collect())
            }
        }
    }
}
impl From<SessionFault> for AgentError {
    fn from(error: SessionFault) -> Self {
        match error {
            SessionFault::InvalidInput(value) => Self::InvalidInput(value),
            SessionFault::Closed(value) => Self::Closed(value),
            SessionFault::UnknownBranch(value) => Self::UnknownBranch(value),
            SessionFault::BranchEnded(value) => Self::BranchEnded(value),
            SessionFault::BranchRefused { refusal, message } => {
                Self::BranchRefused { refusal, message }
            }
            SessionFault::Storage(value) => Self::Storage(value),
            SessionFault::PublicationUnknown(value) => Self::PublicationUnknown(value),
            SessionFault::Build(value) => Self::Build(value),
            SessionFault::Cancelled(value) => Self::Cancelled(value),
            SessionFault::Reloading(value) => Self::Reloading(value),
            SessionFault::AmbiguousSubmission(value) => Self::AmbiguousSubmission(value),
            SessionFault::PartialSubmission(value) => Self::PartialSubmission(value),
            SessionFault::Fault { component, message } => Self::Fault { component, message },
            SessionFault::Framework(value) => {
                Self::Framework(Arc::new(rutis::host::ErrorKind::PluginFailed(value.into())))
            }
            SessionFault::Multiple(value) => {
                Self::Multiple(value.into_iter().map(Into::into).collect())
            }
        }
    }
}

/// Why the tree refused an operation on a line, named the way the protocol's
/// `appError` names it ([timeline branches §14](../../../docs/design/timeline-branches.md#14-clients)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchRefusal {
    /// The target is already the Session's default branch.
    AlreadyDefault,
    /// The target cannot be the default branch: a plugin line, or a line a
    /// client opened before `FORMAT` 53.
    NotSelectable,
    /// The target or an ancestor of it is ending or ended, so an end already
    /// on its way to it wins.
    Protected,
    /// The target cannot end: the current default branch, or an ancestor of
    /// the default branch. The root's own refusal is the operation's input
    /// error, which its callers already answer.
    CannotEnd,
}

impl BranchRefusal {
    /// The code the protocol answers this refusal with.
    pub fn code(self) -> &'static str {
        match self {
            Self::AlreadyDefault => "branch_already_default",
            Self::NotSelectable => "branch_not_selectable",
            Self::Protected => "branch_target_protected",
            Self::CannotEnd => "branch_cannot_end",
        }
    }
}

impl AgentError {
    /// The tree's refusal of an operation on a line.
    pub fn refused(refusal: BranchRefusal, message: impl Into<String>) -> Self {
        Self::BranchRefused {
            refusal,
            message: message.into(),
        }
    }
    pub fn fault(component: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Fault {
            component: component.into(),
            message: message.into(),
        }
    }
    pub fn aggregate(errors: Vec<Self>) -> Result<()> {
        // A cached rutis failure can be observed through both construction and
        // parent cleanup. Merge those observations by Arc identity, never text
        // (lifecycle.md §3); collect only clones existing Arcs and must not
        // allocate new error Arcs that could reuse an address of an already
        // expanded wrapper.
        fn collect(
            error: AgentError,
            seen: &mut HashSet<*const rutis::host::ErrorKind>,
            out: &mut Vec<AgentError>,
        ) {
            match error {
                AgentError::Multiple(errors) if !errors.is_empty() => {
                    for error in errors {
                        collect(error, seen, out);
                    }
                }
                AgentError::Framework(error) => {
                    if !seen.insert(Arc::as_ptr(&error)) {
                        return;
                    }
                    match error.as_ref() {
                        rutis::host::ErrorKind::Aggregate { errors }
                        | rutis::host::ErrorKind::ListenersFailed { errors, .. }
                            if !errors.is_empty() =>
                        {
                            for error in errors {
                                collect(AgentError::Framework(error.clone()), seen, out);
                            }
                        }
                        rutis::host::ErrorKind::PluginFailed(source) => {
                            match source.downcast_ref::<AgentError>() {
                                Some(
                                    inner @ (AgentError::Multiple(_) | AgentError::Framework(_)),
                                ) => collect(inner.clone(), seen, out),
                                _ => out.push(AgentError::Framework(error)),
                            }
                        }
                        _ => out.push(AgentError::Framework(error)),
                    }
                }
                error => out.push(error),
            }
        }
        let mut unique = Vec::new();
        let mut seen = HashSet::new();
        for error in errors {
            collect(error, &mut seen, &mut unique);
        }
        let errors = unique;
        match errors.len() {
            0 => Ok(()),
            1 => Err(errors.into_iter().next().expect("one error")),
            _ => Err(Self::Multiple(errors)),
        }
    }
    pub fn with_cleanup(self, cleanup: Self) -> Self {
        Self::aggregate(vec![self, cleanup]).expect_err("construction and cleanup failed")
    }
    /// Drops the causes a close answers itself, a cancelled or closed scope,
    /// and keeps what failed besides them: a build the close interrupted
    /// reports its cleanup failures.
    pub(crate) fn without_cancellation(self) -> Option<Self> {
        match self {
            Self::Cancelled(_) | Self::Closed(_) => None,
            Self::Multiple(errors) => {
                let mut rest: Vec<_> = errors
                    .into_iter()
                    .filter_map(Self::without_cancellation)
                    .collect();
                match rest.len() {
                    0 => None,
                    1 => rest.pop(),
                    _ => Some(Self::Multiple(rest)),
                }
            }
            error => Some(error),
        }
    }
}

/// Renders a caught panic payload into a diagnostic message.
pub fn panic_message(value: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = value.downcast_ref::<String>() {
        message.clone()
    } else if let Some(message) = value.downcast_ref::<&str>() {
        (*message).into()
    } else {
        "panic in managed operation".into()
    }
}
impl From<rutis::Error> for AgentError {
    fn from(error: rutis::Error) -> Self {
        Self::Framework(Arc::new(error.into_kind()))
    }
}
impl From<rutis::host::CallError> for AgentError {
    fn from(error: rutis::host::CallError) -> Self {
        Self::from(rutis::Error::from(error))
    }
}
impl From<Arc<rutis::host::ErrorKind>> for AgentError {
    fn from(error: Arc<rutis::host::ErrorKind>) -> Self {
        Self::Framework(error)
    }
}
