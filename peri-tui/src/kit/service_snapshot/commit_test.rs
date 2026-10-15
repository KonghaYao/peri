//! 发布职责、无写短路和整批失效门的行为测试。

use super::*;
use crate::kit::atoms::McpInitPhase;
use serial_test::serial;

fn reset() -> ProjectionIdentity {
    crate::kit::atoms::init_atoms();
    ACTIVE_EXECUTION_CWD.set(None);
    ACTIVE_SESSION_ID.set(String::new());
    THREAD_BROWSER_SCOPE.set(ThreadBrowserScope::Project);
    THREAD_BROWSER_ARCHIVED.set(false);
    THREAD_LIST_PAGE_COUNT.set(1);
    ProjectionIdentity {
        active_cwd: None,
        session_id: String::new(),
        session_generation: None,
        scope: ThreadBrowserScope::Project,
        archived: false,
        page_count: 1,
    }
}

#[test]
#[serial]
fn partial_patches_preserve_other_owners_and_equal_values_do_not_write() {
    let identity = reset();
    let original = ServiceSnapshot {
        cwd: "workspace".into(),
        provider_name: "provider".into(),
        model_alias: "alias".into(),
        model_name: "model".into(),
        effort: "high".into(),
        permission_mode: "default".into(),
        mcp: McpStatusSnapshot {
            init_phase: McpInitPhase::Ready,
            total: 2,
            connected: 1,
        },
        cron_total: 3,
        cron_enabled: 2,
        ..Default::default()
    };
    SERVICE_SNAPSHOT.set(original.clone());
    let patches = [SnapshotPatch::Process(ProcessSnapshotPatch {
        memory_mb: 9,
        cpu_percent: 4.2,
    })];
    assert!(commit_projection(
        None,
        &identity,
        &patches,
        ProjectionAtoms::default()
    ));
    let mut expected = original;
    expected.memory_mb = 9;
    expected.cpu_percent = 4.0;
    assert_eq!(*SERVICE_SNAPSHOT.state().read(), expected);

    // 持有读借用：若重复值尝试获取写句柄，AtomState 会直接 panic。
    // 因而本断言不仅比较值，还证明不存在写入和更新通知。
    let handle = SERVICE_SNAPSHOT.state();
    let guard = handle.read();
    assert!(commit_projection(
        None,
        &identity,
        &patches,
        ProjectionAtoms::default()
    ));
    assert_eq!(*guard, expected);
    drop(guard);

    let patches = [
        SnapshotPatch::Cwd("next".into()),
        SnapshotPatch::Session(SessionSnapshotPatch {
            provider_name: "next-provider".into(),
            model_alias: "next-alias".into(),
            model_name: "next-model".into(),
            effort: "low".into(),
            permission_mode: "bypass".into(),
        }),
        SnapshotPatch::Services(ServiceSnapshotPatch {
            mcp: McpStatusSnapshot::default(),
            cron_total: 0,
            cron_enabled: 0,
        }),
    ];
    assert!(commit_projection(
        None,
        &identity,
        &patches,
        ProjectionAtoms::default()
    ));
    let guard = handle.read();
    assert_eq!(guard.memory_mb, 9);
    assert_eq!(guard.cpu_percent, 4.0);
    assert!(commit_projection(
        None,
        &identity,
        &patches,
        ProjectionAtoms::default()
    ));
}

#[tokio::test]
#[serial]
async fn every_identity_dimension_rejects_the_entire_batch_without_any_atom_write() {
    use peri_acp::transport::mpsc::mpsc_transport_pair;
    let (transport, _server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    client.force_stable_for_test("session", false);
    for dimension in 0..7 {
        let mut identity = reset();
        ACTIVE_SESSION_ID.set("session".into());
        identity.session_id = "session".into();
        identity.session_generation = client.stable_session_identity();
        match dimension {
            0 => ACTIVE_EXECUTION_CWD.set(Some("new-cwd".into())),
            1 => ACTIVE_SESSION_ID.set("other".into()),
            2 => THREAD_BROWSER_SCOPE.set(ThreadBrowserScope::Workspace),
            3 => THREAD_BROWSER_ARCHIVED.set(true),
            4 => THREAD_LIST_PAGE_COUNT.set(2),
            5 => identity.session_generation.as_mut().unwrap().1 += 1,
            6 => identity.session_generation = None,
            _ => unreachable!(),
        }
        // 所有输出都持有读借用，任何越过失效门的写入都会导致测试失败。
        macro_rules! hold {
            ($handle:ident, $guard:ident, $atom:ident) => {
                let $handle = $atom.state();
                let $guard = $handle.read();
            };
        }
        hold!(snapshot, _snapshot, SERVICE_SNAPSHOT);
        hold!(title, _title, CURRENT_SESSION_TITLE);
        hold!(error, _error, SERVICE_PROJECTION_ERROR);
        hold!(thread_error, _thread_error, THREAD_LIST_ERROR);
        hold!(more, _more, THREAD_LIST_HAS_MORE);
        hold!(threads, _threads, THREAD_LIST);
        hold!(cron, _cron, CRON_JOBS);
        hold!(files, _files, FILE_LIST);
        hold!(hooks, _hooks, HOOK_LIST);
        hold!(plugins, _plugins, PLUGIN_LIST);
        hold!(providers, _providers, PROVIDER_LIST);
        hold!(mcp, _mcp, MCP_SERVERS);
        hold!(memory, _memory, MEMORY_LIST);
        assert!(
            !commit_projection(
                Some(&client),
                &identity,
                &[SnapshotPatch::Cwd("stale".into())],
                ProjectionAtoms::default()
            ),
            "身份维度 {dimension} 变化后必须拒绝整批发布"
        );
    }
    reset();
    client.close();
}
