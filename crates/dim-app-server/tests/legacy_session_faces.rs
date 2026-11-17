use dim_app_server::collaboration::{CollaborationConfig, CollaborationConfigReply};
use rutis::Ctx;
use rutis::host::CtxHost;
use rutis_bridge::session::call_service;
use std::sync::Arc;

async fn wire<T: serde::de::DeserializeOwned + serde::Serialize>(
    root: &Ctx,
    name: &str,
    method: &str,
    args: impl serde::Serialize,
) -> T {
    call_service(root, name, method, args)
        .await
        .unwrap_or_else(|error| panic!("{name}.{method}: {error}"))
}

#[tokio::test]
async fn the_collaboration_configuration_answers_the_same_through_the_binding_and_the_face() {
    let root = Ctx::root().unwrap();
    root.provide(CollaborationConfig {
        leader_description: Some(Arc::from("lead this")),
        member_description: None,
    })
    .unwrap();
    let binding = root.get::<CollaborationConfig>().unwrap();
    let face: CollaborationConfigReply = wire(&root, "collaborationConfig", "get", ()).await;
    assert_eq!(binding.get(), face);
    assert_eq!(
        face,
        CollaborationConfigReply {
            leader_description: Some("lead this".into()),
            member_description: None,
        },
        "a text the assembly did not configure stays absent"
    );
}
