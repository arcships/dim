//! The App Server's legacy host ports whose content is data: the addresses the
//! transport bound. The owner provides the port and the face is the same
//! operation; this test pins that the Rust binding and the face answer the
//! same ([service faces §4.8](../../docs/design/service-faces.md#48-旧主机端口credentialvariableshost-等六个)).
use dim_app_server::{BoundAddresses, ListenAddresses};
use rutis::Ctx;
use rutis::host::CtxHost;
use rutis_bridge::session::call_service;
use std::path::PathBuf;

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
async fn the_bound_addresses_answer_the_same_through_the_binding_and_the_face() {
    let root = Ctx::root().unwrap();
    root.provide(ListenAddresses {
        unix_path: Some(PathBuf::from("/tmp/dim.sock")),
        pipe_name: Some("dim-pipe".into()),
        websocket_address: Some("127.0.0.1:4100".parse().unwrap()),
    })
    .unwrap();
    let binding = root.get::<ListenAddresses>().unwrap().read().unwrap();
    let face: BoundAddresses = wire(&root, "listenAddresses", "read", ()).await;
    assert_eq!(binding, face);
    assert_eq!(
        face,
        BoundAddresses {
            unix_path: Some("/tmp/dim.sock".into()),
            pipe_name: Some("dim-pipe".into()),
            websocket_address: Some("127.0.0.1:4100".into()),
        }
    );
}
