//! The client method `storage.report` over the native store
//! (native-storage-format.md). The store stays persistence infrastructure and
//! provides its bytes as a service; this row owns the management namespace
//! over it against the method table, as SettingsMethods does for Settings.
use crate::{methods::ManagementMethods, protocol::Empty, storage::SqliteStorage};
use dim_agent_capabilities::methods::{
    Interruption, MethodCall, MethodDecl, RpcError, params, reply,
};
use dim_agent_runtime::Names;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use std::sync::Arc;

pub(crate) struct StorageMethodsPlugin {
    store: Key<SqliteStorage>,
    methods: Key<ManagementMethods>,
}

impl StorageMethodsPlugin {
    pub(crate) fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            store: names.key()?,
            methods: names.key()?,
        })
    }
}

impl DynPlugin for StorageMethodsPlugin {
    fn name(&self) -> &str {
        "StorageMethods"
    }

    fn injects(&self) -> Vec<TypeKey> {
        vec![self.store.erase(), self.methods.erase()]
    }

    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let store = ctx.require_at(&self.store)?;
            let methods = ctx.require_at(&self.methods)?;
            methods.register(
                ctx,
                "storage",
                vec![MethodDecl::new(
                    "storage.report",
                    false,
                    Interruption::Report,
                )],
                Arc::new(move |call, _stopping, _| {
                    let MethodCall {
                        method,
                        params: value,
                        ..
                    } = call;
                    let store = store.clone();
                    Box::pin(async move {
                        match method.as_str() {
                            "storage.report" => {
                                let _: Empty = params(value)?;
                                reply(store.storage_report().await.map_err(|error| {
                                    RpcError::service("storage_unavailable", error.to_string())
                                })?)
                            }
                            _ => unreachable!("registered storage management method"),
                        }
                    })
                }),
            )?;
            Ok(Effect::Done)
        })
    }
}
