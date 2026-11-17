mod support;

use dim_agent_types::*;
use dim_app_server::storage::{SqliteStorage, SqliteTimelineStore};
use support::parts::*;

#[tokio::test(flavor = "current_thread")]
async fn native_goal_saves_business_changes_and_serves_live_get_without_sql() {
    let path = std::env::temp_dir().join(format!("dim-goal-state-{}.sqlite", uuid::Uuid::new_v4()));
    let db = SqliteStorage::open(path.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    let env = Env::start_store(store.clone()).await;
    env.set("ship it").await;
    env.emit("main", user("a new instruction", InputMode::Queue));
    env.goal("get", serde_json::json!({})).await.unwrap();
    let business_count = |events: Vec<std::sync::Arc<AgentEvent>>| {
        events.iter().filter(|event|matches!(&event.kind,EventKind::Extension(data) if data.key()=="goal/state")).count()
    };
    assert_eq!(
        business_count(env.all_events("main").await),
        1,
        "an operator input changes only execution memory"
    );
    env.goal("pause", serde_json::json!({})).await.unwrap();
    env.goal("get", serde_json::json!({})).await.unwrap();
    assert_eq!(
        business_count(env.all_events("main").await),
        2,
        "pause saves one complete business state"
    );
    let env = env.restore().await;
    let value = env.goal("get", serde_json::json!({})).await.unwrap();
    assert_eq!(value["goal"]["objective"], "ship it");
    assert_eq!(value["goal"]["status"]["kind"], "paused");
    assert_eq!(
        business_count(env.all_events("main").await),
        2,
        "reload does not checkpoint execution progress"
    );
    store.record_query_statistics();
    env.goal("get", serde_json::json!({})).await.unwrap();
    assert!(
        store.take_query_statistics().is_empty(),
        "a ready Goal getter reads memory"
    );
    env.close().await;
    store.close().await.unwrap();
    db.close().await.unwrap();
    drop(store);
    drop(db);
    let _ = std::fs::remove_file(path);
}
