//! The `prompts` Settings source: the three texts it accepts, the schema that
//! refuses an empty one, and the host ports each capability reads.
use dim_agent_capabilities::{
    prompt_sources::BasePromptSettingsHost, skills::SkillsInstructionsSettingsHost,
    summary_strategy::SummaryPromptSettingsHost,
};
use dim_agent_runtime::{AgentBuilder, AgentConfig};
use dim_app_server::{
    ServerConfig, serve,
    settings::{SettingsError, SettingsScope, SettingsService, Writer},
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn home() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("dim-prompts-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&path).unwrap();
    path.canonicalize().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_prompts_source_serves_the_three_texts() {
    let home = home();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    dim_agent_capabilities::defaults::mock_hosts(&mut builder);
    dim_agent_capabilities::defaults::install(&mut builder);
    let server = serve(
        builder,
        ServerConfig {
            product_home: Some(home.clone()),
            target_home: Some(home.join("runtime")),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let settings = server.service::<SettingsService>().unwrap();
    let cancel = CancellationToken::new();
    let initial = settings
        .read_declared("prompts", SettingsScope::User, &cancel)
        .await
        .unwrap();
    assert_eq!(initial.document, json!({}));
    let updated = settings
        .update_declared(
            "prompts",
            SettingsScope::User,
            &initial.revision,
            json!({"base": "B", "summary": "S", "skills": "K"}),
            Writer::Client,
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(updated.document["base"], "B");
    let refused = settings
        .update_declared(
            "prompts",
            SettingsScope::User,
            &updated.revision,
            json!({"base": ""}),
            Writer::Client,
            &cancel,
        )
        .await
        .unwrap_err();
    assert!(matches!(refused, SettingsError::Document(_)), "{refused}");
    for document in [json!({"unknown": "x"}), json!({"base": "   "})] {
        let refused = settings
            .update_declared(
                "prompts",
                SettingsScope::User,
                &updated.revision,
                document,
                Writer::Client,
                &cancel,
            )
            .await
            .unwrap_err();
        assert!(matches!(refused, SettingsError::Document(_)), "{refused}");
    }

    let base = server.service::<BasePromptSettingsHost>().unwrap();
    assert_eq!(
        base.0.base(cancel.child_token()).await.unwrap().as_deref(),
        Some("B")
    );
    let instructions = server.service::<SkillsInstructionsSettingsHost>().unwrap();
    assert_eq!(
        instructions
            .0
            .instructions(cancel.child_token())
            .await
            .unwrap()
            .as_deref(),
        Some("K")
    );
    let summary = server.service::<SummaryPromptSettingsHost>().unwrap();
    assert_eq!(
        summary
            .0
            .system_prompt(cancel.child_token())
            .await
            .unwrap()
            .as_deref(),
        Some("S")
    );

    let cancelled = CancellationToken::new();
    cancelled.cancel();
    for error in [
        base.0.base(cancelled.clone()).await.unwrap_err(),
        instructions
            .0
            .instructions(cancelled.clone())
            .await
            .unwrap_err(),
        summary.0.system_prompt(cancelled).await.unwrap_err(),
    ] {
        assert!(
            matches!(
                error,
                dim_agent_capabilities::host::HostError::Failed { .. }
            ),
            "the product owner preserves its settings error mapping: {error}"
        );
    }

    let owner = settings
        .update_declared(
            "prompts",
            SettingsScope::User,
            &updated.revision,
            json!({"base": "OWNER", "summary": "S", "skills": "K"}),
            Writer::Owner,
            &cancel,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(owner, SettingsError::UserOnlyField(ref pointer) if pointer == "/base"),
        "{owner}"
    );

    server.shutdown();
}
