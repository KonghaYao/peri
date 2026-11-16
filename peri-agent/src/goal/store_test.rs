use peri_acp_types::goal::GoalStore as _;
use peri_acp_types::goal::ThreadGoal;

use peri_acp_types::goal::InMemoryGoalStore;

#[tokio::test]
async fn test_in_memory_store_save_and_load() {
    let store = InMemoryGoalStore::new();
    let goal = ThreadGoal::new_at("测试目标".to_string(), Some(100_000), peri_time::now_wall());

    store
        .save("thread-1", goal.clone())
        .await
        .expect("the save fixture result is required for this assertion");

    let loaded = store
        .load("thread-1")
        .await
        .expect("the load fixture result is required for this assertion");
    assert_eq!(
        loaded
            .expect("the loaded fixture result is required for this assertion")
            .objective,
        "测试目标"
    );
}

#[tokio::test]
async fn test_in_memory_store_load_missing_returns_none() {
    let store = InMemoryGoalStore::new();
    let result = store
        .load("missing-thread")
        .await
        .expect("the load fixture result is required for this assertion");
    assert!(result.is_none());
}

#[tokio::test]
async fn test_in_memory_store_overwrite_on_save() {
    let store = InMemoryGoalStore::new();
    let goal1 = ThreadGoal::new_at("目标 1".to_string(), None, peri_time::now_wall());
    let goal2 = ThreadGoal::new_at("目标 2".to_string(), None, peri_time::now_wall());

    store
        .save("thread-1", goal1)
        .await
        .expect("the save fixture result is required for this assertion");
    store
        .save("thread-1", goal2)
        .await
        .expect("the save fixture result is required for this assertion");

    let loaded = store.load("thread-1").await.expect("the load fixture result is required for this assertion").expect("in memory store overwrite on save: the completed fixture operation must provide its result payload");
    assert_eq!(loaded.objective, "目标 2");
}

#[tokio::test]
async fn test_in_memory_store_delete() {
    let store = InMemoryGoalStore::new();
    let goal = ThreadGoal::new_at("待删除".to_string(), None, peri_time::now_wall());
    store
        .save("thread-1", goal)
        .await
        .expect("the save fixture result is required for this assertion");

    store
        .delete("thread-1")
        .await
        .expect("the delete fixture result is required for this assertion");
    let result = store
        .load("thread-1")
        .await
        .expect("the load fixture result is required for this assertion");
    assert!(result.is_none());
}

#[tokio::test]
async fn test_in_memory_store_concurrent_access() {
    use std::sync::Arc;
    let store = Arc::new(InMemoryGoalStore::new());
    let mut handles = Vec::new();

    for i in 0..10 {
        let s = Arc::clone(&store);
        handles.push(tokio::spawn(async move {
            let goal = ThreadGoal::new_at(format!("目标 {}", i), None, peri_time::now_wall());
            s.save(&format!("thread-{}", i), goal)
                .await
                .expect("the save fixture result is required for this assertion");
        }));
    }

    for h in handles {
        h.await
            .expect("the h fixture result is required for this assertion");
    }

    for i in 0..10 {
        assert!(store
            .load(&format!("thread-{}", i))
            .await
            .expect("the load fixture result is required for this assertion")
            .is_some());
    }
}
