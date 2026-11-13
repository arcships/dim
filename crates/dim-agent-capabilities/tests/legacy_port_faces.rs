//! The legacy host ports whose content is data: the credential variable names,
//! a Session's attached clients and the Session method reservation check. Each
//! owner binds its read as the port's operation; these tests pin that the Rust
//! binding and the face answer the same
//! ([service faces §4.9](../../docs/design/service-faces.md#49-旧主机端口credentialvariableshost-等六个)).
use dim_agent_capabilities::environment_clients::{
    ClientChannel, ClientPresence, SessionClientsHost, SessionPresenceRequest,
    SessionPresencesReply,
};
use dim_agent_capabilities::host::{CredentialVariablesHost, CredentialVariablesReply};
use dim_agent_capabilities::methods::{ReservedMethod, ReservedMethods, ReservedMethodsRequest};
use rutis::Ctx;
use rutis::host::CtxHost;
use rutis_bridge::session::call_service;
use std::{collections::BTreeSet, sync::Arc};

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
async fn credential_variable_names_answer_the_same_through_the_binding_and_the_face() {
    let root = Ctx::root().unwrap();
    let names: BTreeSet<String> = ["DIM_MODEL_KEY", "DIM_SECOND_KEY"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let owned = names.clone();
    root.provide(CredentialVariablesHost::new(Arc::new(move || {
        owned.clone()
    })))
    .unwrap();
    let binding = root.get::<CredentialVariablesHost>().unwrap();
    let face: CredentialVariablesReply =
        wire(&root, "credentialVariablesHost", "variables", ()).await;
    assert_eq!(binding.variables(), face);
    assert_eq!(
        face,
        CredentialVariablesReply { variables: names },
        "the names only, never a value"
    );
}

#[tokio::test]
async fn a_sessions_attached_clients_answer_the_same_through_the_binding_and_the_face() {
    let root = Ctx::root().unwrap();
    let presences = vec![
        ClientPresence {
            channel: ClientChannel::Tui,
            label: Some("eric@laptop".into()),
        },
        ClientPresence {
            channel: ClientChannel::Web,
            label: None,
        },
    ];
    let owned = presences.clone();
    root.provide(SessionClientsHost::new(Arc::new(move |_| owned.clone())))
        .unwrap();
    let binding = root.get::<SessionClientsHost>().unwrap();
    let request = SessionPresenceRequest {
        session_id: "session".into(),
    };
    let face: SessionPresencesReply =
        wire(&root, "sessionClientsHost", "presences", (request.clone(),)).await;
    assert_eq!(binding.presences(request), face);
    assert_eq!(
        face,
        SessionPresencesReply { presences },
        "the channel is its wire name and the label travels with it"
    );
}

/// A check that refuses the protocol's roots, as App Server's does.
fn reserved() -> ReservedMethods {
    ReservedMethods::new(Arc::new(|namespace, declarations| {
        if namespace == "hello"
            || declarations.iter().any(|declaration| {
                declaration.name == "reserved"
                    || declaration
                        .notifications
                        .iter()
                        .any(|name| name == "management.progress")
            })
        {
            return Err(rutis::Error::from(rutis::host::ErrorKind::PluginFailed(
                format!("invalid Session namespace {namespace}").into(),
            )));
        }
        Ok(())
    }))
}

fn reserved_request(namespace: &str, name: &str, notification: &str) -> ReservedMethodsRequest {
    ReservedMethodsRequest {
        namespace: namespace.into(),
        declarations: vec![ReservedMethod {
            name: name.into(),
            mutating: false,
            notifications: vec![notification.into()],
        }],
    }
}

#[tokio::test]
async fn the_reservation_check_answers_the_same_through_the_binding_and_the_face() {
    let root = Ctx::root().unwrap();
    root.provide(reserved()).unwrap();
    let binding = root.get::<ReservedMethods>().unwrap();
    let accepted = reserved_request("session.event", "session.event.read", "session.event.read");
    binding.check(accepted.clone()).unwrap();
    let face: () = wire(&root, "reservedMethods", "check", (accepted,)).await;

    let refused = reserved_request("session.event", "session.event.read", "management.progress");
    let error = binding.check(refused.clone()).unwrap_err();
    assert!(error.to_string().contains("invalid Session namespace"));
    let through_face = call_service::<()>(&root, "reservedMethods", "check", (refused,))
        .await
        .expect_err("the face refuses the same namespace");
    assert_eq!(through_face.code(), Some("reserved"));
    assert!(through_face.to_string().contains(&error.to_string()));
    assert_eq!(face, ());
}
