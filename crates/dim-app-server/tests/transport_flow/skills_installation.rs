//! A Skill installation is not a plugin: the `skills.*` methods preview a
//! local directory or Git repository, install a selection into the user
//! root, update it and remove it
//! ([docs/design/plugins/Skills.md](../../docs/design/plugins/Skills.md)).
use super::plugin_manager::{config, git, home, product_builder, ssh_stand_in};
use super::session_typescript::{start, when_served};
use super::*;
use std::path::{Path, PathBuf};

/// A server whose platform home, and so its user Skill root, is inside the
/// disposable home.
fn skills_config(home: &Path) -> ServerConfig {
    ServerConfig {
        unix_path: Some(home.join("ipc/server.sock")),
        platform_home: Some(home.join("user")),
        ..config(home)
    }
}

fn user_root(home: &Path) -> PathBuf {
    home.join("user/.agents/skills")
}

fn app_error(reply: &Value) -> &str {
    reply["error"]["data"]["appError"]
        .as_str()
        .unwrap_or_default()
}

fn skill_doc(name: &str, description: &str, body: &str) -> String {
    format!("---\nname: {name}\ndescription: {description}\n---\n{body}\n")
}

/// A source tree with two Skills and no manifest.
fn source_tree(root: &Path) {
    std::fs::create_dir_all(root.join("writing/review")).unwrap();
    std::fs::write(
        root.join("writing/review/SKILL.md"),
        skill_doc("review", "reviews a change", "REVIEW-1"),
    )
    .unwrap();
    std::fs::write(root.join("writing/review/checklist.md"), "CHECK-1").unwrap();
    std::fs::create_dir_all(root.join("tooling/lint")).unwrap();
    std::fs::write(
        root.join("tooling/lint/SKILL.md"),
        skill_doc("lint", "lints a change", "LINT-1"),
    )
    .unwrap();
}

fn paths(list: &Value) -> Vec<String> {
    list["result"]["skills"]
        .as_array()
        .unwrap_or_else(|| panic!("{list}"))
        .iter()
        .map(|skill| skill["path"].as_str().unwrap().to_owned())
        .collect()
}

fn selected(list: &Value, path: &str) -> Value {
    let skill = list["result"]["skills"]
        .as_array()
        .unwrap()
        .iter()
        .find(|skill| skill["path"] == path)
        .unwrap_or_else(|| panic!("{path} in {list}"));
    json!({"path": skill["path"], "revision": skill["revision"]})
}

fn catalog_ids(catalog: &Value) -> Vec<String> {
    catalog["result"]["entries"]
        .as_array()
        .unwrap_or_else(|| panic!("{catalog}"))
        .iter()
        .map(|entry| entry["id"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn skills_preview_install_update_and_remove_a_repository() {
    ssh_stand_in();
    let home = home();
    let (server, _, _) = start(skills_config(&home)).await;
    let mut client = Client::new_version(&server, false, 3).await;

    let repo = home.join("src/acme-skills.git");
    source_tree(&repo);
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "one"]);
    let url = format!("ssh://fixture{}", repo.display());
    let source = json!({"git": url, "ref": "main"});

    let preview = when_served(&mut client, "skills.preview", json!({"source": source})).await;
    assert_ok(&preview);
    assert_eq!(paths(&preview), ["tooling/lint", "writing/review"]);
    assert_eq!(preview["result"]["source"]["ref"], "main");
    assert!(
        preview["result"]["source"]["commit"]
            .as_str()
            .unwrap()
            .len()
            >= 7
    );
    assert!(!user_root(&home).join("acme-skills").exists());

    let installed = when_served(
        &mut client,
        "skills.install",
        json!({"source": source, "skills": [selected(&preview, "writing/review")]}),
    )
    .await;
    assert_ok(&installed);
    let installation = &installed["result"]["installation"];
    assert_eq!(installation["id"], "acme-skills");
    assert_eq!(installation["source"]["git"], json!(url));
    assert!(installed["result"].get("previous").is_none(), "{installed}");
    assert!(
        user_root(&home)
            .join("acme-skills/writing/review/checklist.md")
            .is_file()
    );
    assert!(!user_root(&home).join("acme-skills/tooling").exists());
    let catalog = when_served(&mut client, "skills.catalog", json!({})).await;
    assert!(
        catalog_ids(&catalog).contains(&"user:acme-skills/writing/review".to_owned()),
        "{catalog}"
    );
    let installations = when_served(&mut client, "skills.installations", json!({})).await;
    assert_eq!(
        installations["result"]["installations"][0]["skills"],
        json!([{"path": "writing/review", "id": "user:acme-skills/writing/review"}])
    );

    std::fs::write(
        repo.join("writing/review/SKILL.md"),
        skill_doc("review", "reviews a change", "REVIEW-2"),
    )
    .unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "two"]);
    let updated = when_served(&mut client, "skills.update", json!({"id": "acme-skills"})).await;
    assert_ok(&updated);
    let previous = &updated["result"]["previous"];
    assert_eq!(previous["revision"], installation["revision"]);
    assert_eq!(
        previous["source"]["commit"],
        installation["source"]["commit"]
    );
    assert_ne!(
        updated["result"]["installation"]["revision"],
        previous["revision"]
    );
    assert_ne!(
        updated["result"]["installation"]["source"]["commit"],
        previous["source"]["commit"]
    );
    assert!(
        std::fs::read_to_string(user_root(&home).join("acme-skills/writing/review/SKILL.md"))
            .unwrap()
            .contains("REVIEW-2")
    );

    let removed = when_served(&mut client, "skills.remove", json!({"id": "acme-skills"})).await;
    assert_ok(&removed);
    assert!(removed["result"]["version"].is_u64(), "{removed}");
    assert!(!user_root(&home).join("acme-skills").exists());
    let catalog = when_served(&mut client, "skills.catalog", json!({})).await;
    assert!(
        !catalog_ids(&catalog).contains(&"user:acme-skills/writing/review".to_owned()),
        "{catalog}"
    );
    let unknown = when_served(&mut client, "skills.update", json!({"id": "acme-skills"})).await;
    assert_eq!(app_error(&unknown), "skills_install_unknown", "{unknown}");

    let preview = when_served(&mut client, "skills.preview", json!({"source": source})).await;
    assert_ok(&preview);
    let reinstalled = when_served(
        &mut client,
        "skills.install",
        json!({"source": source, "skills": [selected(&preview, "writing/review")]}),
    )
    .await;
    assert_ok(&reinstalled);
    let conflict = when_served(
        &mut client,
        "skills.install",
        json!({"source": source, "skills": [selected(&preview, "tooling/lint")]}),
    )
    .await;
    assert_eq!(app_error(&conflict), "skills_install_exists", "{conflict}");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    let server = serve(product_builder(), skills_config(&home))
        .await
        .unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    let installations = when_served(&mut client, "skills.installations", json!({})).await;
    assert_eq!(
        installations["result"]["installations"][0]["id"], "acme-skills",
        "{installations}"
    );
    let catalog = when_served(&mut client, "skills.catalog", json!({})).await;
    assert!(
        catalog_ids(&catalog).contains(&"user:acme-skills/writing/review".to_owned()),
        "{catalog}"
    );
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn skills_replies_and_the_user_document_stay_safe() {
    let home = home();
    let (server, _, _) = start(skills_config(&home)).await;
    let mut client = Client::new_version(&server, false, 3).await;

    let installations = when_served(&mut client, "skills.installations", json!({})).await;
    assert_ok(&installations);
    assert_eq!(installations["result"]["installations"], json!([]));

    for source in [
        json!({"path": "relative/skills"}),
        json!({"path": home.join("missing")}),
        json!({"git": "file:///tmp/skills.git"}),
    ] {
        let refused = when_served(&mut client, "skills.preview", json!({ "source": source })).await;
        assert_eq!(
            app_error(&refused),
            "skills_source_invalid",
            "{source}: {refused}"
        );
    }
    let missing = when_served(
        &mut client,
        "skills.preview",
        json!({"source": {"path": home.join("missing")}}),
    )
    .await;
    assert!(
        !missing
            .to_string()
            .contains(&home.to_string_lossy().into_owned()),
        "{missing}"
    );

    let source = home.join("src/notes");
    source_tree(&source);
    let preview = when_served(
        &mut client,
        "skills.preview",
        json!({"source": {"path": source}, "id": "notes"}),
    )
    .await;
    assert_ok(&preview);
    let installed = when_served(
        &mut client,
        "skills.install",
        json!({"source": {"path": source}, "id": "notes",
            "skills": [selected(&preview, "tooling/lint")]}),
    )
    .await;
    assert_ok(&installed);
    assert!(
        user_root(&home)
            .join("notes/tooling/lint/SKILL.md")
            .is_file()
    );
    let installations = when_served(&mut client, "skills.installations", json!({})).await;
    assert_eq!(
        installations["result"]["installations"][0]["source"]["path"],
        json!(
            source
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        )
    );

    let repo = home.join("src/private.git");
    source_tree(&repo);
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "one"]);
    let url = format!(
        "ssh://user-fixture:password-fixture@fixture{}",
        repo.display()
    );
    let preview = when_served(
        &mut client,
        "skills.preview",
        json!({"source": {"git": url, "ref": "main"}, "id": "private-git"}),
    )
    .await;
    assert_ok(&preview);
    let installed = when_served(
        &mut client,
        "skills.install",
        json!({"source": {"git": url, "ref": "main"}, "id": "private-git",
            "skills": [selected(&preview, "writing/review")]}),
    )
    .await;
    assert_ok(&installed);
    assert_eq!(
        installed["result"]["installation"]["source"]["git"],
        json!(format!("ssh://fixture{}", repo.display()))
    );
    let installations = when_served(&mut client, "skills.installations", json!({})).await;
    let listed = installations.to_string();
    for secret in ["user-fixture", "password-fixture"] {
        assert!(!listed.contains(secret), "{secret} in {listed}");
    }
    assert!(
        !installed.to_string().contains("password-fixture"),
        "{installed}"
    );
    let catalog = when_served(&mut client, "skills.catalog", json!({})).await;
    assert!(
        catalog_ids(&catalog).contains(&"user:private-git/writing/review".to_owned()),
        "{catalog}"
    );

    assert_ok(&when_served(&mut client, "skills.remove", json!({"id": "notes"})).await);
    let unknown = when_served(&mut client, "skills.remove", json!({"id": "notes"})).await;
    assert_eq!(app_error(&unknown), "skills_install_unknown", "{unknown}");
    assert!(user_root(&home).join("private-git/writing/review").is_dir());
    assert!(!user_root(&home).join("notes").exists());
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}
