#[path = "../../../xtask/src/allocation.rs"]
mod allocation;

#[global_allocator]
static ALLOCATOR: allocation::MeasuredAllocator = allocation::MeasuredAllocator;

use dim_agent_capabilities::file_storage::FileStoragePlugin;
use dim_agent_runtime::Names;
use dim_app_server::{
    managed_provider::ManagedProvidersPlugin,
    model_catalog::BundledCatalog,
    provider::{
        ManagementPageRequest, ProviderPlugin, ProviderService, UpdateReplyKind, read_work,
    },
    storage::SqliteStoragePlugin,
};
use rutis::Ctx;
use rutis::host::{CtxHost, FiberState};
use tokio_util::sync::CancellationToken;

async fn active(view: &rutis::host::FiberView) {
    let mut status = view.watch();
    while status.borrow().state != FiberState::Active {
        assert_ne!(
            status.borrow().state,
            FiberState::Failed,
            "{:?}",
            *status.borrow()
        );
        status.changed().await.unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "isolated process allocation measurement"]
async fn provider_activation_and_target_reads_release_their_catalogs() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().canonicalize().unwrap();
    let catalog = dim_app_server::catalog();
    let build = rutis_loader::resolve::Build::default();
    let names = Names::copy(&catalog, &build);
    let before = allocation::live();
    let bundled = BundledCatalog::load().unwrap();
    let retained = allocation::live().saturating_sub(before);
    allocation::reset_peak_now();
    for _ in 0..100 {
        assert!(bundled.model("deepseek", "deepseek-v4-flash").is_some());
    }
    let peak = allocation::peak().saturating_sub(before);
    assert!(
        retained < 64 * 1024,
        "bundled reader retained {retained} bytes"
    );
    assert!(
        allocation::live().saturating_sub(before) < 64 * 1024,
        "model reads retained their results"
    );
    println!("bundled retained={retained} target_peak={peak}");
    drop(bundled);
    for phase in ["first-install", "restart"] {
        let sqlite_before_root = unsafe { rusqlite::ffi::sqlite3_memory_used() };
        let root = Ctx::root().unwrap();
        let plugins = [
            root.plugin(dim_app_server::methods::ManagementMethodsPlugin::typed(&catalog).unwrap()),
            root.plugin(FileStoragePlugin::new(vec![home.clone()])),
            root.plugin(SqliteStoragePlugin::new(home.join("native.sqlite"))),
            root.plugin(ManagedProvidersPlugin::new(None)),
            root.plugin(
                dim_app_server::codex_oauth::OpenAICodexOAuthPlugin::new(&names, 0).unwrap(),
            ),
            root.plugin(dim_app_server::grok_oauth::GrokOAuthPlugin::new(&names, 0).unwrap()),
            root.plugin(dim_agent_aimux::AimuxPlugin::default()),
        ];
        for plugin in &plugins {
            active(plugin).await;
        }
        let before = allocation::live();
        let sqlite_before = unsafe { rusqlite::ffi::sqlite3_memory_used() };
        unsafe { rusqlite::ffi::sqlite3_memory_highwater(1) };
        allocation::reset_peak_now();
        let decoded_before = read_work::source_payloads();
        let provider = root.plugin(ProviderPlugin::new(&names).unwrap());
        active(&provider).await;
        assert_eq!(
            read_work::source_payloads(),
            decoded_before,
            "activation decoded builtin model payloads"
        );
        let db = rusqlite::Connection::open(home.join("native.sqlite")).unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM provider_models", [], |r| r
                .get::<_, u32>(0))
                .unwrap(),
            0
        );
        drop(db);
        let retained = allocation::live().saturating_sub(before);
        let peak = allocation::peak().saturating_sub(before);
        let sqlite_retained = unsafe { rusqlite::ffi::sqlite3_memory_used() } - sqlite_before;
        let sqlite_peak = unsafe { rusqlite::ffi::sqlite3_memory_highwater(0) } - sqlite_before;
        println!(
            "{phase} provider_retained={retained} activation_peak={peak} sqlite_retained={sqlite_retained} sqlite_peak={sqlite_peak} sqlite_total={}",
            sqlite_before + sqlite_retained
        );
        assert!(retained < 128 * 1024, "Provider retained {retained} bytes");
        let service = root.get::<ProviderService>().unwrap();
        let cancel = CancellationToken::new();
        if phase == "first-install" {
            let record = service
                .management_page(
                    ManagementPageRequest {
                        provider_id: Some("deepseek".into()),
                        ..Default::default()
                    },
                    &cancel,
                )
                .await
                .unwrap();
            let before_update = read_work::source_payloads();
            service.update_reply("deepseek".into(), record.providers[0].version.unwrap(),
                serde_json::from_value(serde_json::json!({"enabled":true,"credential":{"action":"set","value":"measurement-fixture"}})).unwrap(), UpdateReplyKind::Summary,&cancel).await.unwrap();
            assert_eq!(
                read_work::source_payloads(),
                before_update,
                "summary configuration decoded builtin model payloads"
            );
            service
                .select(
                    None,
                    dim_agent_types::ModelId {
                        provider_id: "deepseek".into(),
                        model_id: "deepseek-v4-flash".into(),
                    },
                    None,
                    &cancel,
                )
                .await
                .unwrap();
        }
        let before_read = allocation::live();
        let sqlite_before_read = unsafe { rusqlite::ffi::sqlite3_memory_used() };
        allocation::reset_peak_now();
        for _ in 0..10 {
            let reply = service
                .management(
                    "deepseek".into(),
                    Some("deepseek-v4-flash".into()),
                    &CancellationToken::new(),
                )
                .await
                .unwrap();
            assert_eq!(reply.providers.len(), 1);
            let selection = service
                .configuration(None, None, Default::default(), &cancel)
                .await
                .unwrap();
            assert_eq!(selection.provider_id, "deepseek");
            assert_eq!(selection.model_id, "deepseek-v4-flash");
        }
        println!(
            "{phase} read_retained={} read_peak={}",
            allocation::live().saturating_sub(before_read),
            allocation::peak().saturating_sub(before_read)
        );
        println!(
            "{phase} sqlite_read_retained={}",
            unsafe { rusqlite::ffi::sqlite3_memory_used() } - sqlite_before_read
        );
        assert!(
            allocation::live().saturating_sub(before_read) < 64 * 1024,
            "completed management results accumulated"
        );
        if phase == "restart" {
            let summary = service
                .management_page(
                    ManagementPageRequest {
                        provider_id: Some("deepseek".into()),
                        ..Default::default()
                    },
                    &cancel,
                )
                .await
                .unwrap();
            let before_delete = read_work::source_payloads();
            service
                .delete_static(
                    "deepseek".into(),
                    summary.providers[0].version.unwrap(),
                    &cancel,
                )
                .await
                .unwrap();
            assert_eq!(
                read_work::source_payloads(),
                before_delete,
                "disabling a builtin decoded unchanged models"
            );
        }
        drop(service);
        provider.dispose().await.unwrap();
        root.shutdown().await.unwrap();
        assert!(
            unsafe { rusqlite::ffi::sqlite3_memory_used() } <= sqlite_before_root,
            "closing the storage owner releases its SQLite allocations"
        );
    }
}
