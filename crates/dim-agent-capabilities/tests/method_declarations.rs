//! Method declaration validation (methods contract A.1 and A.2): a declared
//! notification follows the name grammar, lies inside its namespace, is
//! unique across the declaration set and is never one of the provider's
//! method names.
use dim_agent_capabilities::methods::{Handler, Interruption, MethodDecl, MethodProvider};
use rutis::Ctx;
use rutis::host::CtxHost;
use serde_json::json;
use std::sync::Arc;

fn handler() -> Handler {
    Arc::new(|_, _, _| Box::pin(async { Ok(json!({})) }))
}

fn declared(namespace: &str, notifications: &[&str]) -> MethodDecl {
    MethodDecl {
        notifications: notifications.iter().map(|name| (*name).into()).collect(),
        ..MethodDecl::new(&format!("{namespace}.run"), false, Interruption::Drain)
    }
}

#[tokio::test]
async fn declared_notifications_follow_the_namespace_and_its_methods() {
    let root = Ctx::root().unwrap();
    for notifications in [
        &["other.changed"][..],
        &["notes.Changed"][..],
        &["notes.changed", "notes.changed"][..],
        &["notes.run"][..],
        &[".changed"][..],
    ] {
        let error = MethodProvider::new(
            &root,
            "notes",
            vec![declared("notes", notifications)],
            handler(),
            false,
        )
        .err()
        .unwrap_or_else(|| panic!("{notifications:?} must be refused"));
        assert!(
            error
                .to_string()
                .contains("invalid methods for namespace notes"),
            "{error}"
        );
    }
    MethodProvider::new(
        &root,
        "notes",
        vec![declared("notes", &["notes.changed"])],
        handler(),
        false,
    )
    .unwrap();
    root.shutdown().await.unwrap();
}
