use dim_agent_capabilities::host::ModelProtocol;
use dim_app_server::managed_provider::{
    AccountSnapshot, AccountView, CredentialLease, LeaseOwner, ManagedCatalog, ManagedError,
    ManagedFuture, ManagedProvider, ManagedProviderBinding, ManagedProviders,
    ManagedProvidersPlugin, ManagedSearch, ManagedSearchError, ProviderDescriptor, WebSearchHit,
    WebSearchRequest, WebSearchResponse,
    management::{
        BrowserLogin, BrowserLoginBegin, DeviceLogin, LoginBegin, LoginState,
        ManagedBrowserLoginComplete, ManagedLoginAttempt, Usage,
    },
    owner_key,
};
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, FiberState, TypeKey};
use rutis::{Ctx, Plugin};
use rutis_bridge::session::{
    Error as BridgeError, HostDispatch, Value as RpcValue, host_key, settle,
};
use serde_json::{Value as Json, json};
use std::sync::{
    Arc, Mutex, OnceLock, Weak,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct Leases {
    issued: Mutex<Vec<String>>,
    released: Mutex<Vec<String>>,
}
impl LeaseOwner for Leases {
    fn release(&self, lease_id: &str) {
        self.released.lock().unwrap().push(lease_id.into());
    }
}

#[derive(Default)]
struct Fixture {
    leases: Arc<Leases>,
    entered: AtomicUsize,
    resume: Notify,
    quiesced: AtomicBool,
    /// The lease field the next issued lease gets wrong, if any.
    malformed: Mutex<Option<&'static str>>,
    /// Recoveries and final rejections this provider was asked for.
    settled: AtomicUsize,
    /// The account recovery reports.
    recovered_generation: Mutex<u64>,
    searches: Mutex<Vec<WebSearchRequest>>,
    search_entered: Notify,
    search_cancelled: Notify,
    pause_status: AtomicBool,
    status_entered: Notify,
    status_resume: Notify,
    empty_catalog: AtomicBool,
}
fn account(generation: u64) -> AccountSnapshot {
    AccountSnapshot {
        credential_epoch: "epoch".into(),
        generation,
        account: Some(AccountView {
            issuer: "https://accounts.example".into(),
            subject: "subject".into(),
            email: None,
            name: None,
            granted_scopes: vec![],
        }),
    }
}
impl Fixture {
    fn lease(&self, generation: u64) -> CredentialLease {
        let mut issued = self.leases.issued.lock().unwrap();
        let id = format!("lease-{}", issued.len());
        issued.push(id.clone());
        let owner: Weak<dyn LeaseOwner> = Arc::downgrade(&self.leases) as Weak<Leases>;
        let malformed = *self.malformed.lock().unwrap();
        let mut account = account(generation);
        if malformed == Some("signed-out") {
            account.account = None;
        }
        let base_url = match malformed {
            Some("scheme") => "file:///private",
            Some("userinfo") => "http://user:secret@localhost/v1",
            Some("query") => "http://localhost/v1?token=x",
            _ => "http://localhost/v1",
        };
        let headers = match malformed {
            Some("header") => [("X-Inject".into(), "a\r\nHost: evil".into())].into(),
            _ => Default::default(),
        };
        let bearer = if malformed == Some("bearer") {
            ""
        } else {
            "private-fixture-token"
        };
        CredentialLease::new(
            account,
            ModelProtocol::OpenaiCompatible,
            base_url.into(),
            headers,
            bearer.into(),
            id,
            owner,
        )
    }
    /// Issues a lease from owned work that waits for `resume`, as a provider
    /// owns its accepted operations.
    fn issue(self: &Arc<Self>, generation: u64) -> ManagedFuture<CredentialLease> {
        let owner = self.clone();
        let task = tokio::spawn(async move {
            owner.entered.fetch_add(1, Ordering::SeqCst);
            owner.resume.notified().await;
            Ok(owner.lease(generation))
        });
        Box::pin(async move { task.await.unwrap() })
    }
}
struct Provider(Arc<Fixture>);
fn unused<T: Send + 'static>() -> ManagedFuture<T> {
    Box::pin(async { Err(ManagedError::InvalidInput) })
}
impl ManagedProvider for Provider {
    fn providers(&self) -> Vec<ProviderDescriptor> {
        vec![ProviderDescriptor {
            provider_id: "fixture".into(),
            display_name: "Fixture".into(),
            login_methods: vec!["device".into()],
            default_directory: None,
        }]
    }
    fn status(&self, _: &str) -> ManagedFuture<AccountSnapshot> {
        let fixture = self.0.clone();
        Box::pin(async move {
            if fixture.pause_status.swap(false, Ordering::SeqCst) {
                fixture.status_entered.notify_one();
                fixture.status_resume.notified().await;
            }
            Ok(account(1))
        })
    }
    fn recover(&self, _: &str) -> ManagedFuture<AccountSnapshot> {
        unused()
    }
    fn begin_browser(&self, _: BrowserLoginBegin) -> ManagedFuture<BrowserLogin> {
        unused()
    }
    fn complete_browser(&self, _: ManagedBrowserLoginComplete) -> ManagedFuture<AccountSnapshot> {
        unused()
    }
    fn begin_device(&self, _: LoginBegin) -> ManagedFuture<DeviceLogin> {
        unused()
    }
    fn attempt_status(&self, _: &ManagedLoginAttempt) -> Result<LoginState, ManagedError> {
        Err(ManagedError::AttemptNotFound)
    }
    fn cancel_attempt(&self, _: ManagedLoginAttempt) -> ManagedFuture<()> {
        unused()
    }
    fn logout(&self, _: &str) -> ManagedFuture<AccountSnapshot> {
        unused()
    }
    fn usage(&self, _: &str) -> ManagedFuture<Usage> {
        unused()
    }
    fn web_search(
        &self,
        request: WebSearchRequest,
        cancel: CancellationToken,
    ) -> ManagedFuture<WebSearchResponse> {
        self.0.searches.lock().unwrap().push(request.clone());
        let fixture = self.0.clone();
        Box::pin(async move {
            if request.query == "fail" {
                return Err(ManagedError::Transient);
            }
            if request.query == "pending" {
                let worker = fixture.clone();
                let task = tokio::spawn(async move {
                    cancel.cancelled().await;
                    worker.search_cancelled.notify_one();
                    Err(ManagedError::ServiceReloading)
                });
                fixture.search_entered.notify_one();
                return task.await.unwrap();
            }
            Ok(WebSearchResponse {
                results: vec![WebSearchHit {
                    title: "Dim".into(),
                    url: "https://example.com/dim".into(),
                    snippet: "a coding agent".into(),
                    published_at: Some("2026-01-02T03:04:05Z".into()),
                }],
            })
        })
    }
    fn catalog(&self, _: &str, _: CancellationToken) -> ManagedFuture<ManagedCatalog> {
        let empty = self.0.empty_catalog.load(Ordering::SeqCst);
        Box::pin(async move {
            let models = if empty {
                vec![]
            } else {
                vec![dim_app_server::managed_provider::ManagedModel {
                    entry: dim_app_server::model_catalog::BundledCatalog::load()
                        .unwrap()
                        .model("deepseek", "deepseek-v4-flash")
                        .unwrap(),
                    protocol: ModelProtocol::OpenaiCompatible,
                    unavailable_reason: None,
                    zdr: false,
                }]
            };
            Ok(ManagedCatalog {
                account: account(1),
                models,
            })
        })
    }
    fn credential(&self, _: &str, _: &str) -> ManagedFuture<CredentialLease> {
        self.0.issue(1)
    }
    fn recover_rejected(&self, _: &CredentialLease) -> ManagedFuture<CredentialLease> {
        self.0.settled.fetch_add(1, Ordering::SeqCst);
        self.0.issue(*self.0.recovered_generation.lock().unwrap())
    }
    fn finish_rejected(&self, _: &CredentialLease) -> ManagedFuture<()> {
        self.0.settled.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
    fn quiesce(&self) -> BoxFuture<'static, ()> {
        self.0.quiesced.store(true, Ordering::SeqCst);
        Box::pin(async {})
    }
}
struct FixturePlugin(Arc<Fixture>);
impl Plugin for FixturePlugin {
    type Deps = ();
    fn name(&self) -> &str {
        "managed-fixture"
    }
    async fn apply(&self, ctx: &Ctx, (): ()) -> rutis::Result {
        ctx.provide_as::<dyn ManagedProvider>(Arc::new(Provider(self.0.clone())))?;
        Ok(())
    }
}
struct Capture(Arc<OnceLock<Arc<ManagedProviders>>>);
impl DynPlugin for Capture {
    fn name(&self) -> &str {
        "capture-managed"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![owner_key().erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let owner = ctx.require_at(&owner_key())?;
            let _ = self.0.set(owner);
            Ok(Effect::Done)
        })
    }
}
async fn active(view: &rutis::host::FiberView) {
    let mut watch = view.watch();
    while watch.borrow().state != FiberState::Active {
        assert_ne!(
            watch.borrow().state,
            FiberState::Failed,
            "{:?}",
            *watch.borrow()
        );
        watch.changed().await.unwrap();
    }
}

async fn setup() -> (
    Ctx,
    Arc<Fixture>,
    Arc<ManagedProviders>,
    rutis::host::FiberView,
) {
    let root = Ctx::root().unwrap();
    let catalog = dim_app_server::catalog();
    root.plugin(dim_app_server::methods::ManagementMethodsPlugin::typed(&catalog).unwrap());
    let fixture = Arc::new(Fixture::default());
    *fixture.recovered_generation.lock().unwrap() = 1;
    let captured = Arc::new(OnceLock::new());
    let owner_plugin = root.plugin(ManagedProvidersPlugin::new(None));
    let fixture_plugin = root.plugin(FixturePlugin(fixture.clone()));
    let binding = root.plugin(ManagedProviderBinding);
    let capture = root.plugin(Capture(captured.clone()));
    for view in [&owner_plugin, &fixture_plugin, &binding, &capture] {
        active(view).await;
    }
    (root, fixture, captured.get().unwrap().clone(), binding)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn abandoned_credential_and_recovery_waiters_release_every_issued_lease() {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        for recovery in [false, true] {
            let (root, fixture, owner, _) = setup().await;
            fixture.resume.notify_one();
            let initial = owner
                .credential("fixture", "model", &CancellationToken::new())
                .await
                .unwrap();
            let waiter = tokio::spawn(async move {
                let cancel = CancellationToken::new();
                if recovery {
                    owner.recover(&initial, &cancel).await
                } else {
                    drop(initial);
                    owner.credential("fixture", "model", &cancel).await
                }
            });
            while fixture.entered.load(Ordering::SeqCst) < 2 {
                tokio::task::yield_now().await;
            }
            waiter.abort();
            assert!(matches!(waiter.await, Err(error) if error.is_cancelled()));
            fixture.resume.notify_one();
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while fixture.leases.released.lock().unwrap().len() < 2 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            root.shutdown().await.unwrap();
            let mut released = fixture.leases.released.lock().unwrap().clone();
            released.sort();
            assert_eq!(
                released,
                *fixture.leases.issued.lock().unwrap(),
                "recovery={recovery}"
            );
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recovery_for_another_account_is_refused_and_released() {
    let (root, fixture, owner, _) = setup().await;
    fixture.resume.notify_one();
    let initial = owner
        .credential("fixture", "model", &CancellationToken::new())
        .await
        .unwrap();
    *fixture.recovered_generation.lock().unwrap() = 2;
    fixture.resume.notify_one();
    let changed = owner
        .recover(&initial, &CancellationToken::new())
        .await
        .err()
        .unwrap();
    assert_eq!(changed.code, "account_changed");
    assert_eq!(*fixture.leases.released.lock().unwrap(), ["lease-1"]);
    drop(initial);
    root.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unbinding_withdraws_the_provider_after_quiescing_it() {
    let (root, fixture, owner, binding) = setup().await;
    let cancel = CancellationToken::new();
    assert_eq!(owner.providers(&cancel).await.unwrap().len(), 1);
    binding.shutdown().await.unwrap();
    assert!(fixture.quiesced.load(Ordering::SeqCst));
    assert!(owner.providers(&cancel).await.unwrap().is_empty());
    let unavailable = owner
        .credential("fixture", "model", &cancel)
        .await
        .err()
        .unwrap();
    assert_eq!(unavailable.code, "account_provider_unavailable");
    root.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lease_of_a_replaced_provider_never_reaches_its_replacement() {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let root = Ctx::root().unwrap();
        let fixture = || {
            let fixture = Arc::new(Fixture::default());
            *fixture.recovered_generation.lock().unwrap() = 1;
            fixture
        };
        let (first, second) = (fixture(), fixture());
        let captured = Arc::new(OnceLock::new());
        let catalog = dim_app_server::catalog();
        root.plugin(dim_app_server::methods::ManagementMethodsPlugin::typed(&catalog).unwrap());
        let owner_plugin = root.plugin(ManagedProvidersPlugin::new(None));
        let first_plugin = root.plugin(FixturePlugin(first.clone()));
        let binding = root.plugin(ManagedProviderBinding);
        let capture = root.plugin(Capture(captured.clone()));
        for view in [&owner_plugin, &first_plugin, &binding, &capture] {
            active(view).await;
        }
        let owner = captured.get().unwrap().clone();
        let cancel = CancellationToken::new();
        first.resume.notify_one();
        let lease = owner.credential("fixture", "model", &cancel).await.unwrap();
        first.resume.notify_one();
        drop(owner.recover(&lease, &cancel).await.unwrap());
        owner.finish_rejected(&lease, &cancel).await.unwrap();
        assert_eq!(first.settled.load(Ordering::SeqCst), 2);

        first_plugin.shutdown().await.unwrap();
        let second_plugin = root.plugin(FixturePlugin(second.clone()));
        active(&second_plugin).await;
        active(&binding).await;
        second.resume.notify_one();
        let current = owner.credential("fixture", "model", &cancel).await.unwrap();
        assert_eq!(
            (current.id(), &current.account),
            (lease.id(), &lease.account)
        );
        for refused in [
            owner.recover(&lease, &cancel).await.err().unwrap(),
            owner.finish_rejected(&lease, &cancel).await.err().unwrap(),
        ] {
            assert_eq!(refused.code, "service_reloading");
        }
        assert_eq!(second.settled.load(Ordering::SeqCst), 0);
        second.resume.notify_one();
        drop(owner.recover(&current, &cancel).await.unwrap());
        assert_eq!(second.settled.load(Ordering::SeqCst), 1);
        drop((lease, current));
        root.shutdown().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn malformed_leases_are_refused_at_the_host_and_released() {
    let (root, fixture, owner, _) = setup().await;
    let cancel = CancellationToken::new();
    let cases = [
        "scheme",
        "userinfo",
        "query",
        "header",
        "bearer",
        "signed-out",
    ];
    for case in cases {
        *fixture.malformed.lock().unwrap() = Some(case);
        fixture.resume.notify_one();
        let refused = owner
            .credential("fixture", "model", &cancel)
            .await
            .err()
            .unwrap();
        assert_eq!(refused.code, "account_invalid_response", "{case}");
    }
    *fixture.malformed.lock().unwrap() = None;
    fixture.resume.notify_one();
    let lease = owner.credential("fixture", "model", &cancel).await.unwrap();
    *fixture.malformed.lock().unwrap() = Some("header");
    fixture.resume.notify_one();
    let refused = owner.recover(&lease, &cancel).await.err().unwrap();
    assert_eq!(refused.code, "account_invalid_response");
    drop(lease);
    let mut released = fixture.leases.released.lock().unwrap().clone();
    released.sort();
    let mut issued = fixture.leases.issued.lock().unwrap().clone();
    issued.sort();
    assert_eq!(issued.len(), cases.len() + 2);
    assert_eq!(released, issued);
    root.shutdown().await.unwrap();
}

async fn face_call(
    dispatch: &Arc<dyn HostDispatch>,
    method: &str,
    request: Json,
) -> Result<Json, BridgeError> {
    let reply = dispatch.invoke(method, RpcValue::List(vec![request.into()]))?;
    settle(reply).await?.json()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_managed_search_face_answers_with_the_owner_hits() {
    let (root, fixture, _owner, _binding) = setup().await;
    let face = root
        .get_at::<dyn HostDispatch>(&host_key("managedSearch"))
        .expect("the managedSearch data face is provided");

    let reply = face_call(&face, "search", json!({"query": "dim agent", "count": 3}))
        .await
        .unwrap();
    assert_eq!(
        reply,
        json!({"results": [{
            "title": "Dim",
            "url": "https://example.com/dim",
            "snippet": "a coding agent",
            "publishedAt": "2026-01-02T03:04:05Z",
        }]})
    );
    assert_eq!(
        fixture.searches.lock().unwrap().as_slice(),
        [WebSearchRequest {
            query: "dim agent".into(),
            count: 3,
        }]
    );
    use futures_util::FutureExt;
    let native = root.get::<ManagedSearch>().unwrap();
    let parent = CancellationToken::new();
    let typed = native
        .search(
            WebSearchRequest {
                query: "dim agent".into(),
                count: 3,
            },
            &parent,
        )
        .now_or_never()
        .expect("a ready native operation needs no bridge task")
        .unwrap();
    assert_eq!(serde_json::to_value(typed).unwrap(), reply);
    assert!(!parent.is_cancelled());
    assert_eq!(
        native
            .search(
                WebSearchRequest {
                    query: "fail".into(),
                    count: 1
                },
                &parent
            )
            .await
            .unwrap_err(),
        ManagedSearchError::SearchFailed
    );
    let failure = face_call(&face, "search", json!({"query":"fail","count":1}))
        .await
        .unwrap_err();
    assert_eq!(failure.code(), Some("search_failed"));
    assert_eq!(
        native
            .search(
                WebSearchRequest {
                    query: "x".into(),
                    count: 1
                },
                &{
                    let c = CancellationToken::new();
                    c.cancel();
                    c
                }
            )
            .await
            .unwrap_err(),
        ManagedSearchError::Unavailable
    );
    for request in [
        json!({"query":"x"}),
        json!({"query":"x","count":1,"extra":true}),
    ] {
        assert!(face_call(&face, "search", request).await.is_err());
    }
    let unknown = face_call(&face, "providers", json!({})).await.unwrap_err();
    assert!(
        unknown.to_string().contains("unknown managedSearch method"),
        "{unknown}"
    );
    root.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_managed_search_face_reports_an_unbound_provider() {
    let root = Ctx::root().unwrap();
    let view = root.plugin(ManagedProvidersPlugin::new(None));
    active(&view).await;
    let face = root
        .get_at::<dyn HostDispatch>(&host_key("managedSearch"))
        .expect("the managedSearch data face is provided");
    let error = face_call(&face, "search", json!({"query": "x", "count": 1}))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Some("unavailable"), "{error}");
    root.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_or_abandoned_search_settles_provider_work() {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let (root, fixture, _, _) = setup().await;
        let native = root.get::<ManagedSearch>().unwrap();
        let face = root
            .get_at::<dyn HostDispatch>(&host_key("managedSearch"))
            .unwrap();
        for mode in ["native_drop", "wire_drop", "native_cancel"] {
            let parent = CancellationToken::new();
            let task = {
                let native = native.clone();
                let face = face.clone();
                let parent = parent.clone();
                tokio::spawn(async move {
                    if mode == "wire_drop" {
                        let _ =
                            face_call(&face, "search", json!({"query":"pending","count":1})).await;
                    } else {
                        let result = native
                            .search(
                                WebSearchRequest {
                                    query: "pending".into(),
                                    count: 1,
                                },
                                &parent,
                            )
                            .await;
                        assert_eq!(result.unwrap_err(), ManagedSearchError::SearchFailed);
                    }
                })
            };
            fixture.search_entered.notified().await;
            if mode == "native_cancel" {
                parent.cancel();
                task.await.unwrap();
            } else {
                task.abort();
                assert!(task.await.unwrap_err().is_cancelled());
                assert!(!parent.is_cancelled());
            }
            fixture.search_cancelled.notified().await;
        }
        let parent = CancellationToken::new();
        root.shutdown().await.unwrap();
        assert_eq!(
            native
                .search(
                    WebSearchRequest {
                        query: "closed".into(),
                        count: 1
                    },
                    &parent
                )
                .await
                .unwrap_err(),
            ManagedSearchError::Unavailable
        );
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn model_pages_survive_refresh_removal_and_an_unbound_empty_account() {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        use dim_app_server::provider::{ModelQuery, ProviderPlugin, ProviderService};
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().canonicalize().unwrap();
        let (root, fixture, _, binding) = setup().await;
        let catalog = dim_app_server::catalog();
        let build = rutis_loader::resolve::Build::default();
        let names = dim_agent_runtime::Names::copy(&catalog, &build);
        let dependencies = [
            root.plugin(
                dim_agent_capabilities::file_storage::FileStoragePlugin::new(vec![home.clone()]),
            ),
            root.plugin(dim_app_server::storage::SqliteStoragePlugin::new(
                home.join("native.sqlite"),
            )),
            root.plugin(
                dim_app_server::codex_oauth::OpenAICodexOAuthPlugin::new(&names, 0).unwrap(),
            ),
            root.plugin(dim_app_server::grok_oauth::GrokOAuthPlugin::new(&names, 0).unwrap()),
            root.plugin(dim_agent_aimux::AimuxPlugin::default()),
        ];
        for dependency in &dependencies {
            active(dependency).await;
        }
        active(&root.plugin(ProviderPlugin::new(&names).unwrap())).await;
        let provider = root.get::<ProviderService>().unwrap();
        let cancel = CancellationToken::new();
        let initial = provider.refresh("fixture".into(), &cancel).await.unwrap();
        assert_eq!(initial.models.len(), 1);
        let target = initial.models[0].id.clone();
        let version = provider
            .set_model_enabled(
                "fixture".into(),
                initial.version,
                target.clone(),
                false,
                &cancel,
            )
            .await
            .unwrap();
        let disabled = provider
            .management("fixture".into(), Some(target.clone()), &cancel)
            .await
            .unwrap();
        assert!(!disabled.providers[0].models.as_ref().unwrap()[0].enabled);
        provider
            .set_model_enabled("fixture".into(), version, target, true, &cancel)
            .await
            .unwrap();
        fixture.pause_status.store(true, Ordering::SeqCst);
        let reader = provider.clone();
        let query = tokio::spawn(async move {
            reader
                .query_models(
                    ModelQuery {
                        provider_id: Some("fixture".into()),
                        ..Default::default()
                    },
                    &CancellationToken::new(),
                )
                .await
        });
        fixture.status_entered.notified().await;
        fixture.empty_catalog.store(true, Ordering::SeqCst);
        let refreshed = provider.refresh("fixture".into(), &cancel).await.unwrap();
        assert!(refreshed.models.is_empty());
        fixture.status_resume.notify_one();
        let page = query.await.unwrap().unwrap();
        assert!(page.models.is_empty());
        assert!(page.next.is_none());
        binding.dispose().await.unwrap();
        let page = provider
            .query_models(
                ModelQuery {
                    provider_id: Some("fixture".into()),
                    ..Default::default()
                },
                &cancel,
            )
            .await
            .unwrap();
        assert!(page.models.is_empty());
        let value = serde_json::to_value(page).unwrap();
        assert_eq!(value["empty"][0]["reason"]["kind"], "unreadable");
        root.shutdown().await.unwrap();
    })
    .await
    .expect("model page operations settle across refresh and unbind");
}
