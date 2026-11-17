mod local_support;

use dim_agent_capabilities::{
    prompt_sources::BasePromptSettings, skills::SkillsInstructionsSettings,
};
use local_support::*;
use std::sync::Arc;

/// A host base-prompt override fixed at construction.
struct FixedBase(&'static str);
impl BasePromptSettings for FixedBase {
    fn base(
        &self,
        _: tokio_util::sync::CancellationToken,
    ) -> dim_agent_capabilities::host::HostFuture<'_, Option<Arc<str>>> {
        Box::pin(async move { Ok(Some(self.0.into())) })
    }
}

/// A host Skills-instructions override fixed at construction.
struct FixedInstructions(&'static str);
impl SkillsInstructionsSettings for FixedInstructions {
    fn instructions(
        &self,
        _: tokio_util::sync::CancellationToken,
    ) -> dim_agent_capabilities::host::HostFuture<'_, Option<Arc<str>>> {
        Box::pin(async move { Ok(Some(self.0.into())) })
    }
}

/// The host's user overrides replace the built-in base and Skills texts, and
/// the assembly's factory configuration still wins over them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_overrides_replace_the_policy_texts() {
    let dirs = Dirs::new();
    let cwd = dirs.dir("root");
    dirs.write(
        &dirs.home.join(".agents/skills/review/SKILL.md"),
        &skill_file("review", "Review a diff", "REVIEW"),
    );
    let env = env(
        &dirs,
        Options {
            base_prompt: Some(Arc::new(FixedBase("CUSTOM-BASE"))),
            skills_instructions: Some(Arc::new(FixedInstructions("CUSTOM-SKILLS"))),
            ..Options::default()
        },
    )
    .await;
    let session = env.session(&cwd, "SESSION-S").await.unwrap();
    run(&session, "hello").await;
    let sent = requests(&session).await;
    let system = system_text(sent.last().unwrap().system.as_deref().unwrap());
    assert!(system.contains("CUSTOM-BASE"), "{system}");
    assert!(!system.contains("You are DimAgent."), "{system}");
    assert!(system.contains("CUSTOM-SKILLS"), "{system}");
    assert!(!system.contains("<skills_instructions>"), "{system}");
    let last = sent.last().unwrap();
    assert!(
        last.tools
            .as_ref()
            .is_some_and(|tools| tools.iter().any(|tool| tool.name == "skill")),
        "the skill tool is still declared"
    );
    let text = request_text(last);
    assert!(text.contains("dim/skills/list"), "{text}");
    assert!(text.contains("review"), "{text}");
    env.agent.close().await.unwrap();
}

/// A registered but unprovided host is not a gate: the Session opens with the
/// built-in texts instead of waiting for the service.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_registered_but_missing_host_keeps_the_built_in_text() {
    let dirs = Dirs::new();
    let cwd = dirs.dir("root");
    let env = env(
        &dirs,
        Options {
            missing_settings_hosts: true,
            ..Options::default()
        },
    )
    .await;
    let session = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        env.session(&cwd, "SESSION-S"),
    )
    .await
    .expect("the Session opens without the host")
    .unwrap();
    run(&session, "hello").await;
    let sent = requests(&session).await;
    let system = system_text(sent.last().unwrap().system.as_deref().unwrap());
    assert!(system.contains("You are DimAgent."), "{system}");
    assert!(system.contains("<skills_instructions>"), "{system}");
    env.agent.close().await.unwrap();
}

/// The factory configuration stays authoritative: a candidate assembly keeps
/// its text even when the host provides an override.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn factory_texts_outrank_host_overrides() {
    let dirs = Dirs::new();
    let cwd = dirs.dir("root");
    let env = env(
        &dirs,
        Options {
            instructions: Some("FACTORY-SKILLS".into()),
            skills_instructions: Some(Arc::new(FixedInstructions("HOST-SKILLS"))),
            ..Options::default()
        },
    )
    .await;
    let session = env.session(&cwd, "SESSION-S").await.unwrap();
    run(&session, "hello").await;
    let sent = requests(&session).await;
    let system = system_text(sent.last().unwrap().system.as_deref().unwrap());
    assert!(system.contains("FACTORY-SKILLS"), "{system}");
    assert!(!system.contains("HOST-SKILLS"), "{system}");
    env.agent.close().await.unwrap();
}
