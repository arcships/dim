//! The legacy Session and process ports whose content is data: the Session's
//! reply language, the Skills factory configuration, the Agent's identity and
//! the worktree association write. Each owner binds its operation on the port
//! type; these tests pin that the Rust binding and the face answer the same
//! ([service faces §4.8](../../docs/design/service-faces.md#48-旧主机端口agentconfig-等六个)).
use dim_agent_capabilities::file_access::{
    WorktreeAssociation, WorktreeAssociationError, WorktreeAssociations, WorktreeDirectory,
};
use dim_agent_capabilities::prompt_sources::{SessionLanguage, SessionLanguageReply};
use dim_agent_capabilities::skills::{SkillsConfig, SkillsConfigReply};
use dim_agent_runtime::{AgentConfig, AgentConfigReply, ReplyLanguage};
use rutis::Ctx;
use rutis::host::CtxHost;
use rutis_bridge::session::call_service;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

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
async fn the_sessions_reply_language_answers_the_same_through_the_binding_and_the_face() {
    let root = Ctx::root().unwrap();
    root.provide(SessionLanguage::new(Some(ReplyLanguage::SimplifiedChinese)))
        .unwrap();
    let binding = root.get::<SessionLanguage>().unwrap();
    let face: SessionLanguageReply = wire(&root, "sessionLanguage", "get", ()).await;
    assert_eq!(binding.get(), face);
    assert_eq!(
        face,
        SessionLanguageReply {
            language: Some(ReplyLanguage::SimplifiedChinese),
        },
        "the wire carries the operator's code, not a name"
    );
}

#[tokio::test]
async fn the_skills_factory_configuration_answers_the_same_through_the_binding_and_the_face() {
    let root = Ctx::root().unwrap();
    root.provide(SkillsConfig {
        instructions: Some("use this text".into()),
    })
    .unwrap();
    let binding = root.get::<SkillsConfig>().unwrap();
    let face: SkillsConfigReply = wire(&root, "skillsConfig", "get", ()).await;
    assert_eq!(binding.get(), face);
    assert_eq!(
        face,
        SkillsConfigReply {
            instructions: Some("use this text".into()),
        }
    );
}

#[tokio::test]
async fn the_agents_configuration_answers_the_same_through_the_binding_and_the_face() {
    let root = Ctx::root().unwrap();
    let config = AgentConfig {
        name: "agent".into(),
        system_prompt: "be brief".into(),
        model: dim_agent_runtime::ModelId {
            provider_id: "mock".into(),
            model_id: "deterministic".into(),
        },
    };
    root.provide(config.clone()).unwrap();
    let binding = root.get::<AgentConfig>().unwrap();
    let face: AgentConfigReply = wire(&root, "agentConfig", "get", ()).await;
    assert_eq!(binding.get(), face);
    assert_eq!(
        face,
        AgentConfigReply {
            name: config.name,
            system_prompt: config.system_prompt,
            model: config.model,
        }
    );
}

#[tokio::test]
async fn the_worktree_association_answers_the_same_through_the_binding_and_the_face() {
    let root = Ctx::root().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    root.provide(WorktreeAssociations::new(Arc::new(move |request| {
        let counted = counted.clone();
        Box::pin(async move {
            assert_eq!(request.session_id, "session");
            assert_eq!(
                request.directory(),
                WorktreeDirectory {
                    repository: "/repo".into(),
                    root: "/repo/tree".into(),
                }
            );
            assert_eq!(request.subtree, "/repo/tree/sub");
            counted.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    })))
    .unwrap();
    let binding = root.get::<WorktreeAssociations>().unwrap();
    let request = WorktreeAssociation {
        session_id: "session".into(),
        repository: "/repo".into(),
        root: "/repo/tree".into(),
        subtree: "/repo/tree/sub".into(),
    };
    binding.associate(request.clone()).await.unwrap();
    let face: () = wire(&root, "worktreeAssociations", "associate", (request,)).await;
    assert_eq!(face, ());
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "the binding and the face each ran the owner's write"
    );
}

#[tokio::test]
async fn a_worktree_association_failure_crosses_as_a_code_and_a_message() {
    let root = Ctx::root().unwrap();
    root.provide(WorktreeAssociations::new(Arc::new(|_| {
        Box::pin(async {
            Err(WorktreeAssociationError::Failed {
                message: "session is closed".into(),
            })
        })
    })))
    .unwrap();
    let request = WorktreeAssociation {
        session_id: "session".into(),
        repository: "/repo".into(),
        root: "/repo/tree".into(),
        subtree: "/repo/tree/sub".into(),
    };
    let error = call_service::<()>(&root, "worktreeAssociations", "associate", (request,))
        .await
        .expect_err("the owner refused the write");
    assert_eq!(error.code(), Some("failed"));
    assert!(error.to_string().contains("session is closed"));
}
