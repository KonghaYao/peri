use crate::kit::acp_types::{AcpEventData, BgTaskSnapshotSource};

#[test]
fn test_bg_task_completed_decodes_output_preview() {
    let data = serde_json::json!({
        "task_id": "t1",
        "success": true,
        "duration_ms": 100,
        "output_preview": "hello preview"
    });
    let ev = AcpEventData::decode("bg-task-completed", data);
    match ev {
        AcpEventData::BgTaskCompleted {
            output_preview,
            task_id,
            ..
        } => {
            assert_eq!(task_id, "t1");
            assert_eq!(output_preview.as_deref(), Some("hello preview"));
        }
        other => panic!("unexpected variant {other:?}"),
    }
}

#[test]
fn test_bg_task_completed_missing_preview_still_ok() {
    let data = serde_json::json!({
        "task_id": "t2",
        "success": false,
        "duration_ms": 50
    });
    let ev = AcpEventData::decode("bg-task-completed", data);
    match ev {
        AcpEventData::BgTaskCompleted { output_preview, .. } => {
            assert!(output_preview.is_none());
        }
        other => panic!("unexpected variant {other:?}"),
    }
}

#[test]
fn test_bg_task_snapshot_accepts_session_envelope() {
    let ev = AcpEventData::decode(
        "bg-task-snapshot",
        serde_json::json!({
            "revision": 7,
            "tasks": [{
                "task_id": "t3",
                "kind": "shell",
                "summary": "sleep 1",
                "started_at": "2026-10-03T00:00:00Z",
                "pid": null
            }]
        }),
    );
    match ev {
        AcpEventData::BgTaskSnapshot {
            tasks,
            revision,
            source,
        } => {
            assert_eq!(tasks.len(), 1);
            assert_eq!(tasks[0].task_id, "t3");
            assert_eq!(revision, Some(7));
            assert_eq!(
                source,
                BgTaskSnapshotSource::Live,
                "旧 wire 无 source 字段时按当前 live 快照兼容解码"
            );
        }
        other => panic!("unexpected variant {other:?}"),
    }
}

#[test]
fn test_bg_task_snapshot_decodes_explicit_historical_source() {
    let ev = AcpEventData::decode(
        "bg-task-snapshot",
        serde_json::json!({
            "revision": 8,
            "source": "historical",
            "tasks": [],
        }),
    );
    match ev {
        AcpEventData::BgTaskSnapshot { source, .. } => {
            assert_eq!(source, BgTaskSnapshotSource::Historical);
        }
        other => panic!("unexpected variant {other:?}"),
    }
}

#[test]
fn test_bg_task_snapshot_unknown_source_stays_read_only() {
    let ev = AcpEventData::decode(
        "bg-task-snapshot",
        serde_json::json!({
            "revision": 9,
            "source": "future-source",
            "tasks": [],
        }),
    );
    match ev {
        AcpEventData::BgTaskSnapshot { source, .. } => {
            assert_eq!(source, BgTaskSnapshotSource::Historical);
        }
        other => panic!("unexpected variant {other:?}"),
    }
}
