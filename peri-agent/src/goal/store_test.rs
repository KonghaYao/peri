use peri_acp_types::goal::ThreadGoal;

use super::*;

#[tokio::test]
async fn test_in_memory_store_save_and_load() {
    let store = InMemoryGoalStore::new();
    let goal = ThreadGoal::new("测试目标".to_string(), Some(100_000));

    store.save("thread-1", goal.clone()).await.unwrap();

    let loaded = store.load("thread-1").await.unwrap();
    assert_eq!(loaded.unwrap().objective, "测试目标");
}

#[tokio::test]
async fn test_in_memory_store_load_missing_returns_none() {
    let store = InMemoryGoalStore::new();
    let result = store.load("missing-thread").await.unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn test_in_memory_store_overwrite_on_save() {
    let store = InMemoryGoalStore::new();
    let goal1 = ThreadGoal::new("目标 1".to_string(), None);
    let goal2 = ThreadGoal::new("目标 2".to_string(), None);

    store.save("thread-1", goal1).await.unwrap();
    store.save("thread-1", goal2).await.unwrap();

    let loaded = store.load("thread-1").await.unwrap().unwrap();
    assert_eq!(loaded.objective, "目标 2");
}

#[tokio::test]
async fn test_in_memory_store_delete() {
    let store = InMemoryGoalStore::new();
    let goal = ThreadGoal::new("待删除".to_string(), None);
    store.save("thread-1", goal).await.unwrap();

    store.delete("thread-1").await.unwrap();
    let result = store.load("thread-1").await.unwrap();
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
            let goal = ThreadGoal::new(format!("目标 {}", i), None);
            s.save(&format!("thread-{}", i), goal).await.unwrap();
        }));
    }

    for h in handles {
        h.await.unwrap();
    }

    for i in 0..10 {
        assert!(store
            .load(&format!("thread-{}", i))
            .await
            .unwrap()
            .is_some());
    }
}
