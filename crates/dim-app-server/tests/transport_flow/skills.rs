//! `skills.catalog` reports where each entry was found, so a client groups
//! entries without reading the id (Skills page, management methods).
use super::*;

fn skill(name: &str, description: &str) -> String {
    format!("---\nname: {name}\ndescription: {description}\n---\n\nBody of {name}.\n")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_catalog_reports_the_source_of_every_entry() {
    use std::os::unix::fs::PermissionsExt;
    let home = std::path::PathBuf::from("/tmp").join(format!("dm-skills-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(home.join("rust")).unwrap();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    let project = home.join("project");
    std::fs::create_dir_all(project.join(".agents/skills/deploy")).unwrap();
    std::fs::write(
        project.join(".agents/skills/deploy/SKILL.md"),
        skill("deploy", "Deploys the project"),
    )
    .unwrap();
    let (server, _) = launch_with_agent(
        Arc::new(MockTools),
        ServerConfig {
            product_home: Some(home.clone()),
            target_home: Some(home.join("rust")),
            ..Default::default()
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let reply = client
        .rpc("skills.catalog", json!({"cwd": project.to_string_lossy()}))
        .await;
    assert_ok(&reply);
    let entries = reply["result"]["entries"]
        .as_array()
        .unwrap_or_else(|| panic!("entries: {reply}"));
    let deploy = entries
        .iter()
        .find(|entry| entry["name"] == "deploy")
        .unwrap_or_else(|| panic!("deploy is listed: {reply}"));
    assert_eq!(deploy["source"], "project", "{reply}");
    assert!(
        deploy["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("workspace:")),
        "the id stays what the catalog reports: {reply}"
    );
    // The user root is the host's own home; whatever it holds, every entry
    // names the root it came from.
    for entry in entries {
        let source = entry["source"].as_str().unwrap_or_default();
        assert!(
            source == "user" || source == "project" || source.starts_with("plugin:"),
            "{entry}"
        );
    }
    server.shutdown();
    std::fs::remove_dir_all(&home).unwrap();
}
