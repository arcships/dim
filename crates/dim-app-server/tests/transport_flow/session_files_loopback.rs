//! `sessionFiles` through a Node Session part: the face's request, reply and
//! data errors cross the real runtime channel (session files §9.1).
use super::session_typescript::{fixture, setup, when_served};
use super::*;
use dim_app_server::session_typescript::SessionTypeScriptConfig;

fn parts() -> SessionTypeScriptConfig {
    let mut parts = fixture();
    let root = parts.anchor.parent().unwrap().join("fixtures");
    parts.rows = vec![json!({
        "id": "session-files",
        "name": root.join("session-files.ts"),
        "provides": {},
        "config": {},
    })];
    parts
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_session_part_puts_through_the_session_files_face() {
    tokio::time::timeout(Duration::from_secs(180), async {
        let home = std::env::temp_dir().join(format!(
            "dm-session-files-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&home).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let (builder, _requests, _release) = setup();
        let server = serve(
            builder,
            ServerConfig {
                unix_path: Some(home.join("server.sock")),
                database_path: Some(home.join("dimcode.sqlite")),
                session_typescript: Some(parts()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let mut client = Client::new_version(&server, false, 3).await;
        let created = client
            .rpc("session.create", json!({"cwd": home, "tools": []}))
            .await;
        assert_ok(&created);
        let id = created["result"]["sessionId"].as_str().unwrap().to_owned();

        let reply = when_served(&mut client, "session.files.probe", json!({"sessionId": id})).await;
        assert_ok(&reply);
        let value = &reply["result"];
        assert_eq!(value["stored"]["bytes"], 5, "{reply}");
        assert_eq!(value["owned"], true, "{reply}");
        assert_eq!(value["large"]["code"], "too_large", "{reply}");
        assert_eq!(value["missing"]["code"], "not_found", "{reply}");

        // The over-limit call crossed the channel and the row kept serving.
        let again = when_served(&mut client, "session.files.probe", json!({"sessionId": id})).await;
        assert_ok(&again);
        assert_eq!(again["result"]["stored"]["bytes"], 5, "{again}");

        server.shutdown();
        server.join().await.unwrap();
        std::fs::remove_dir_all(home).unwrap();
    })
    .await
    .unwrap();
}
