//! 工作区观测缓存的有界保留与目录回收（issue #473）。

use std::path::PathBuf;

use peri_acp_types::workspace::{ProjectId, ResolvedWorkspace, WorkspaceId};

use super::{ObservationCache, MAX_WORKSPACE_OBSERVATIONS};

fn resolved(index: usize) -> ResolvedWorkspace {
    let cwd = PathBuf::from(format!("/observed/{index}"));
    ResolvedWorkspace {
        project_id: ProjectId::new(),
        workspace_id: WorkspaceId::new(),
        cwd: cwd.clone(),
        root: cwd.clone(),
        relative_cwd: PathBuf::new(),
        discovery_snapshot: Some("{}".to_owned()),
    }
}

fn key(observation: &ResolvedWorkspace) -> (WorkspaceId, PathBuf) {
    (observation.workspace_id, observation.relative_cwd.clone())
}

#[test]
fn cache_evicts_the_least_recently_used_observation() {
    let mut cache = ObservationCache::default();
    let observations: Vec<ResolvedWorkspace> =
        (0..MAX_WORKSPACE_OBSERVATIONS).map(resolved).collect();
    for observation in &observations {
        cache.record(key(observation), observation.clone());
    }
    assert_eq!(cache.len(), MAX_WORKSPACE_OBSERVATIONS);

    // 命中把最旧的一条移到队尾；淘汰必须落在下一条最久未使用的记录上。
    assert!(cache.get(&key(&observations[0])).is_some());
    let extra = resolved(MAX_WORKSPACE_OBSERVATIONS);
    cache.record(key(&extra), extra);

    assert_eq!(cache.len(), MAX_WORKSPACE_OBSERVATIONS);
    assert!(
        cache.get(&key(&observations[0])).is_some(),
        "最近命中过的观测不能被淘汰"
    );
    assert!(
        cache.get(&key(&observations[1])).is_none(),
        "最久未使用的观测应先被淘汰"
    );
}

#[test]
fn forgetting_one_directory_keeps_other_observations() {
    let mut cache = ObservationCache::default();
    let first = resolved(0);
    let second = resolved(1);
    cache.record(key(&first), first.clone());
    cache.record(key(&second), second.clone());

    cache.forget_cwds(std::slice::from_ref(&first.cwd));

    assert!(cache.get(&key(&first)).is_none());
    assert!(cache.get(&key(&second)).is_some());
}

#[test]
fn cache_does_not_grow_when_the_limit_is_reached() {
    let mut cache = ObservationCache::default();
    for index in 0..(MAX_WORKSPACE_OBSERVATIONS * 2) {
        let observation = resolved(index);
        cache.record(key(&observation), observation);
    }
    assert_eq!(cache.len(), MAX_WORKSPACE_OBSERVATIONS);
}
