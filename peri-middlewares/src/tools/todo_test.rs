use tokio::sync::{mpsc, Mutex};

use super::*;

/// 构造带独立共享状态的 TodoWriteTool（测试辅助）
fn make_tool() -> (TodoWriteTool, Arc<Mutex<TodoState>>) {
    let (tx, _rx) = mpsc::channel(8);
    let state = Arc::new(Mutex::new(TodoState::default()));
    (TodoWriteTool::new(tx, Arc::clone(&state)), state)
}

#[test]
fn test_description_extended() {
    let (tool, _) = make_tool();
    let desc = tool.description();
    assert!(
        desc.contains("full replacement") || desc.contains("fully replaces"),
        "description 应提及全量替换语义"
    );
    assert!(
        desc.contains("pending") && desc.contains("in_progress") && desc.contains("completed"),
        "description 应提及三种状态值"
    );
    assert!(desc.len() > 200, "description 应为扩展后的多段落文本");
}

#[test]
#[allow(non_snake_case)]
fn test_tool_name_is_TodoWrite() {
    let (tool, _) = make_tool();
    assert_eq!(tool.name(), "TodoWrite");
}

#[test]
fn test_parameters_contains_require_completion() {
    let (tool, _) = make_tool();
    let params = tool.parameters();
    assert_eq!(
        params["properties"]["requireCompletion"]["type"], "boolean",
        "parameters 应声明顶层 requireCompletion 布尔参数"
    );
    assert_eq!(
        params["properties"]["requireCompletion"]["default"], false,
        "requireCompletion 默认应为 false"
    );
}

#[test]
fn test_todo_item_no_id() {
    let item: TodoItem = serde_json::from_value(serde_json::json!({
        "content": "test",
        "status": "pending"
    }))
    .unwrap();
    assert_eq!(item.content, "test");
}

#[test]
fn test_todo_item_active_form() {
    let item: TodoItem = serde_json::from_value(serde_json::json!({
        "content": "test",
        "activeForm": "Running tests",
        "status": "in_progress"
    }))
    .unwrap();
    assert_eq!(item.active_form, Some("Running tests".to_string()));
}

#[test]
fn test_summarize_changes_by_index() {
    let old = vec![
        TodoItem {
            content: "A".into(),
            active_form: None,
            status: TodoStatus::Pending,
        },
        TodoItem {
            content: "B".into(),
            active_form: None,
            status: TodoStatus::Pending,
        },
    ];
    let new = vec![
        TodoItem {
            content: "A".into(),
            active_form: None,
            status: TodoStatus::InProgress,
        },
        TodoItem {
            content: "B".into(),
            active_form: None,
            status: TodoStatus::Pending,
        },
        TodoItem {
            content: "C".into(),
            active_form: None,
            status: TodoStatus::Pending,
        },
    ];
    let summary = summarize_changes(&old, &new);
    assert!(
        summary.contains("[0]→in_progress"),
        "should detect status change at [0]: {summary}"
    );
    assert!(
        summary.contains("+[2]"),
        "should detect addition at [2]: {summary}"
    );
}

#[test]
fn test_summarize_changes_empty() {
    let old = vec![TodoItem {
        content: "A".into(),
        active_form: None,
        status: TodoStatus::Pending,
    }];
    let new = vec![TodoItem {
        content: "A".into(),
        active_form: None,
        status: TodoStatus::Pending,
    }];
    let summary = summarize_changes(&old, &new);
    assert_eq!(summary, "saved");
}

// ─── requireCompletion 解析测试 ───────────────────────────────────────────────

fn pending_items() -> serde_json::Value {
    serde_json::json!([
        { "content": "A", "status": "pending" },
        { "content": "B", "status": "pending" }
    ])
}

#[tokio::test]
async fn test_invoke_require_completion_true_开启标记() {
    let (tool, state) = make_tool();
    let result = tool
        .invoke(
            serde_json::json!({
                "requireCompletion": true,
                "todos": pending_items()
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_ok(), "合法输入应成功");
    let guard = state.lock().await;
    assert!(
        guard.require_completion,
        "requireCompletion=true 应开启标记"
    );
    assert_eq!(guard.items.len(), 2);
}

#[tokio::test]
async fn test_invoke_require_completion缺省_保留已有标记() {
    let (tool, state) = make_tool();
    tool.invoke(
        serde_json::json!({
            "requireCompletion": true,
            "todos": pending_items()
        }),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();

    // 中途更新列表（未携带参数）→ 标记保留
    tool.invoke(
        serde_json::json!({
            "todos": pending_items()
        }),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();
    let guard = state.lock().await;
    assert!(guard.require_completion, "缺省时不应丢失已有标记");
}

#[tokio::test]
async fn test_invoke_require_completion_false_解除标记() {
    let (tool, state) = make_tool();
    tool.invoke(
        serde_json::json!({
            "requireCompletion": true,
            "todos": pending_items()
        }),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();

    tool.invoke(
        serde_json::json!({
            "requireCompletion": false,
            "todos": pending_items()
        }),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();
    let guard = state.lock().await;
    assert!(!guard.require_completion, "显式 false 应解除标记");
}

#[tokio::test]
async fn test_invoke_all_completed_自动解除标记() {
    let (tool, state) = make_tool();
    tool.invoke(
        serde_json::json!({
            "requireCompletion": true,
            "todos": pending_items()
        }),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();

    // 全部标记 completed → 自动解除（未携带参数）
    tool.invoke(
        serde_json::json!({
            "todos": [
                { "content": "A", "status": "completed" },
                { "content": "B", "status": "completed" }
            ]
        }),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();
    let guard = state.lock().await;
    assert!(!guard.require_completion, "全部 completed 后标记应自动解除");
}

#[test]
fn test_render_todo_status_包含状态与内容() {
    let rendered = render_todo_status(&[
        TodoItem {
            content: "写测试".into(),
            active_form: None,
            status: TodoStatus::InProgress,
        },
        TodoItem {
            content: "跑 clippy".into(),
            active_form: None,
            status: TodoStatus::Pending,
        },
    ]);
    assert!(rendered.contains("[0] [in_progress] 写测试"));
    assert!(rendered.contains("[1] [pending] 跑 clippy"));
}
