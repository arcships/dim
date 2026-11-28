//! The model stacks' consumption of the persisted `compaction` settings
//! source (#2235 PR 1b): a host port resolvers read on every preparation,
//! the late-binding cell that connects it to the resolvers the host process
//! wires before the settings source exists, and the overlay that injects the
//! shaped window into a request the same way an adopted startup window
//! carries it.
//!
//! The port is a closed Rust port (#2235 has no cross-language reader of
//! this document; clients use `settings.read`/`settings.update`). The
//! shapes, selection order, soft cap and shaping floor live in
//! [`dim_agent_types::compaction_settings`], which both sides of the wire
//! share.

use crate::host::{ContextModel, HostFuture, ResolveContextModel};
use dim_agent_types::ModelRequest;
use dim_agent_types::compaction_settings::{
    CompactionSettings, ShapedWindow, shape_compaction_window,
};
use futures_util::future::BoxFuture;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

/// A failure of the compaction settings port: `cancelled` when the caller's
/// preparation stopped, `storage` for everything the backing settings
/// service reports.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CompactionSettingsError {
    #[error("cancelled")]
    Cancelled,
    #[error("the compaction settings could not be read")]
    Storage,
}

/// The process's reader of the persisted compaction document. The resolver
/// calls it on every preparation, so a write takes effect on the next
/// request without a restart (#2235 PR 1b).
#[derive(Clone)]
pub struct CompactionSettingsHost(
    pub  Arc<
        dyn Fn(
                CancellationToken,
            )
                -> BoxFuture<'static, Result<CompactionSettings, CompactionSettingsError>>
            + Send
            + Sync,
    >,
);

/// The late binding between the resolvers, wired while the host process
/// assembles its model stacks, and the host the `compaction` settings row
/// publishes once its source exists. A process without the row keeps an
/// empty cell and its model stacks behave as before.
#[derive(Clone, Default)]
pub struct CompactionSettingsCell {
    host: Arc<Mutex<Option<CompactionSettingsHost>>>,
}

impl CompactionSettingsCell {
    pub fn set(&self, host: CompactionSettingsHost) {
        *self.host.lock().unwrap() = Some(host);
    }

    /// One cell serves one row; unloading the row takes the port back with
    /// it.
    pub fn clear(&self) {
        *self.host.lock().unwrap() = None;
    }

    pub fn host(&self) -> Option<CompactionSettingsHost> {
        self.host.lock().unwrap().clone()
    }
}

impl std::fmt::Debug for CompactionSettingsCell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CompactionSettingsCell")
    }
}

/// The persisted window the resolver applies to `request`, or `None` to let
/// the request resolve as before. An adopted startup window — a value, or
/// the explicit `auto` fact — always wins over the persisted setting (#2235
/// PR 1b): it is the one layer a user names per session. A read failure of
/// the backing store degrades to no overlay, since a corrupt persisted
/// configuration must not take every session down.
pub async fn overlay_window(
    host: &CompactionSettingsHost,
    request: &ModelRequest,
    context_window: u64,
    cancel: CancellationToken,
) -> Option<(u64, Option<u64>)> {
    if let Some(startup) = request
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.startup.as_ref())
        && (startup.auto_compact_window.is_some() || startup.auto_compact_window_explicit)
    {
        return None;
    }
    let document = match crate::port_faces::read(
        cancel,
        || CompactionSettingsError::Cancelled,
        |read| (host.0)(read),
    )
    .await
    {
        Ok(document) => document,
        Err(CompactionSettingsError::Cancelled) => return None,
        Err(error) => {
            tracing::warn!(%error, "compaction settings read failed; preparing without the overlay");
            return None;
        }
    };
    let spec = document.spec_for(&request.provider_id, &request.model_id);
    match shape_compaction_window(spec, context_window) {
        ShapedWindow::ModelDerived => None,
        ShapedWindow::Shaped {
            tokens,
            raised_from,
        } => Some((tokens, raised_from)),
    }
}

/// The request the persisted overlay resolves through: a copy whose startup
/// metadata carries `tokens` as the compaction window, so the model's own
/// window facts apply the same shaping an adopted startup window gets.
pub fn inject_window(request: &ModelRequest, tokens: u64) -> ModelRequest {
    let mut injected = request.clone();
    injected
        .metadata
        .get_or_insert_with(Default::default)
        .startup
        .get_or_insert_with(Default::default)
        .auto_compact_window = Some(tokens);
    injected
}

/// The product model stack's compaction overlay (#2235 PR 1b): the persisted
/// setting shapes the window unless the session adopted an explicit startup
/// window. Anything else — no host, no hit, `auto`, a soft-capped token
/// value — resolves untouched.
pub struct CompactionContextModel {
    inner: Arc<dyn ResolveContextModel>,
    cell: CompactionSettingsCell,
}

impl CompactionContextModel {
    pub fn new(
        inner: Arc<dyn ResolveContextModel>,
        cell: CompactionSettingsCell,
    ) -> CompactionContextModel {
        CompactionContextModel { inner, cell }
    }
}

impl ResolveContextModel for CompactionContextModel {
    fn resolve(
        &self,
        request: Arc<ModelRequest>,
        cancel: CancellationToken,
    ) -> HostFuture<'_, Arc<dyn ContextModel>> {
        Box::pin(async move {
            let host = self.cell.host();
            let context_window = request
                .capability_snapshot
                .as_ref()
                .map(|snapshot| snapshot.capabilities.context_window);
            if let (Some(host), Some(context_window)) = (host, context_window)
                && let Some((tokens, raised_from)) =
                    overlay_window(&host, &request, context_window, cancel.clone()).await
            {
                if let Some(configured) = raised_from {
                    tracing::warn!(
                        provider = %request.provider_id,
                        model = %request.model_id,
                        configured,
                        tokens,
                        "persisted compaction window below the shaping floor; raised for this preparation"
                    );
                }
                return self
                    .inner
                    .resolve(Arc::new(inject_window(&request, tokens)), cancel)
                    .await;
            }
            self.inner.resolve(request, cancel).await
        })
    }
}
