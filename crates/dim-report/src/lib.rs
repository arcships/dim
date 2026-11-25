//! Connection-bound reports for the App Server's product profiles.

mod conversation;
mod draft;
mod evidence;
pub mod redaction;
mod render;

use dim_agent_capabilities::{methods::SessionMethods, timeline_tree::TreeCommands};
use dim_agent_runtime::{AgentBuilder, Names, PartRegistry, PartScope, SessionLifecycle};
use dim_app_server::methods::ManagementMethods;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};

/// Installs the process and Session report methods.
pub fn install(builder: &mut AgentBuilder) {
    builder.plugin(|names| {
        Ok(Box::new(Report {
            methods: names.key()?,
            parts: names.key()?,
        }))
    });
}

struct Report {
    methods: Key<ManagementMethods>,
    parts: Key<PartRegistry>,
}

impl DynPlugin for Report {
    fn name(&self) -> &str {
        "Report"
    }

    fn injects(&self) -> Vec<TypeKey> {
        vec![self.methods.erase(), self.parts.erase()]
    }

    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let methods = ctx.require_at(&self.methods)?;
            let parts = ctx.require_at(&self.parts)?;
            draft::register(ctx, &methods, "report", None)?;
            parts.register(ctx, PartScope::Session, "ReportSession", |mount| {
                Ok(Some(Box::new(ReportSession::new(mount.names)?)))
            })?;
            Ok(Effect::Done)
        })
    }
}

struct ReportSession {
    identity: Key<dim_agent_runtime::SessionIdentity>,
    platform: Key<dim_agent_capabilities::host_platform::HostPlatform>,
    methods: Key<SessionMethods>,
    lifecycle: Key<SessionLifecycle>,
    tree: Key<TreeCommands>,
}

impl ReportSession {
    fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            methods: names.key()?,
            lifecycle: names.key()?,
            identity: names.key()?,
            platform: names.key()?,
            tree: names.key()?,
        })
    }
}

impl DynPlugin for ReportSession {
    fn name(&self) -> &str {
        "ReportSession"
    }

    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.methods.erase(),
            self.lifecycle.erase(),
            self.tree.erase(),
            self.identity.erase(),
            self.platform.erase(),
        ]
    }

    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let methods = ctx.require_at(&self.methods)?;
            let session = draft::Session {
                lifecycle: ctx.require_at(&self.lifecycle)?,
                identity: ctx.require_at(&self.identity)?,
                home: ctx.require_at(&self.platform)?.home.clone(),
                tree: ctx.require_at(&self.tree)?,
            };
            draft::register(ctx, &methods, "session.report", Some(session))?;
            Ok(Effect::Done)
        })
    }
}
