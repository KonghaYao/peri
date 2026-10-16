//! 后台启动回执的身份可复用性回归。
//!
//! 历史样本（冻结 cohort `resume-id-kind-confusion`，P2/observed）：4 次 Agent 调用把启动
//! 回执里 task id（`bg-{uuid}`）**去掉前缀**后的 UUID 当作 `resume_thread_id`，全部以
//! `resume_subagent: thread not found: <uuid>` 失败。task 身份与 child thread 身份是两个
//! 领域身份（114/114 不同本身正常，不是缺陷）；这里锁定两件可观察行为：
//!
//! 1. 启动回执必须与完成通知使用同一规范句，直接给出可复制的 `resume_thread_id`，
//!    且不再给出无指引的裸线程标注（`(thread: …)`）——两个 v7 UUID 前缀相同，
//!    并列出现时没有标签就没有可判别的身份语义；
//! 2. 错误身份（task UUID 去掉 `bg-`）仍然显式失败，不自动转义为该任务的
//!    child_thread_id，也不绕开 UUID 形态与存在性校验。
//!
//! 注意：本文件是回执契约与边界的锁定；文本断言不构成「误用率下降」的因果证明。

use super::*;

/// 从启动回执中取出 task 身份（`bg-` 前缀）与线程身份（规范句标注）。
fn receipt_identities(receipt: &str) -> (String, String) {
    let task_id = receipt
        .split("Background task ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("回执应携带 task 身份: {receipt}"))
        .to_string();
    let child_thread_id = receipt
        .split("child_thread_id: ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("启动回执应直接给出 child_thread_id: {receipt}"))
        .to_string();
    (task_id, child_thread_id)
}

async fn start_background_agent(
    dir: &tempfile::TempDir,
    host: &HostFixture,
    label: &str,
) -> String {
    write_test_agent(dir);
    let tool = host.bind(with_agent_face(make_subagent_tool(vec![]), dir.path()).await);
    let receipt = tool
        .invoke(
            serde_json::json!({
                "subagent_type": "test-agent",
                "prompt": label,
                "run_in_background": true,
                "cwd": host.cwd.clone(),
            }),
            host.context(&[]),
        )
        .await
        .expect("bg agent 应启动并返回启动回执");
    receipt
}

/// 启动回执必须直接给出可复用的 `resume_thread_id`：规范句与完成通知逐字同形，
/// 且其中的 id 是真实存在的线程身份；裸线程标注 `(thread: …)` 不再出现。
#[tokio::test]
async fn background_launch_receipt_names_reusable_child_thread_id() {
    let dir = tempdir().unwrap();
    let host = HostFixture::open_in(dir.path(), "fixture-resume-identity").await;
    let receipt = start_background_agent(&dir, &host, "identity probe").await;

    let (task_id, child_thread_id) = receipt_identities(&receipt);
    assert!(
        task_id.starts_with("bg-"),
        "task 身份带 bg- 前缀: {receipt}"
    );

    // 规范句（与 bg 完成通知同一形状）：唯一可直接复制的 resume 参数来源。
    assert!(
        receipt.contains(&format!(
            "child_thread_id: {child_thread_id} \
             (resume with Agent(resume_thread_id: {child_thread_id}))"
        )),
        "启动回执必须携带规范句: {receipt}"
    );
    assert!(receipt.contains("While active, include a non-empty prompt"));
    assert!(receipt.contains("omit prompt only when resuming a non-active thread"));
    assert!(
        !receipt.contains("(thread: "),
        "启动回执不得保留无指引的裸线程标注: {receipt}"
    );

    // 两个领域身份：task 身份去掉前缀后仍不是线程身份（不是同值改写）。
    assert_ne!(
        task_id.strip_prefix("bg-").expect("task 身份带 bg- 前缀"),
        child_thread_id,
        "task UUID 与 child_thread_id 是两个身份: {receipt}"
    );

    // 「可直接复用」不是空话：该 id 是真实落库的线程身份。
    host.fixture
        .load_meta(&child_thread_id)
        .await
        .unwrap_or_else(|error| panic!("回执中的 child_thread_id 必须是存在的线程: {error}"));
}

/// 边界锁定：把 task id 去掉 `bg-` 前缀当作 `resume_thread_id`（历史误用形态）必须显式
/// 失败——形态校验放行合法 UUID 后，存在性校验判无此线程，错误保留收到的身份，
/// 且不回退/转义为该任务真实的 child_thread_id。
#[tokio::test]
async fn task_uuid_is_rejected_without_translation_to_child_thread() {
    let dir = tempdir().unwrap();
    let host = HostFixture::open_in(dir.path(), "fixture-resume-task-uuid").await;
    let receipt = start_background_agent(&dir, &host, "task uuid probe").await;
    let (task_id, child_thread_id) = receipt_identities(&receipt);
    let task_uuid = task_id
        .strip_prefix("bg-")
        .expect("task 身份带 bg- 前缀")
        .to_string();
    assert_ne!(task_uuid, child_thread_id);

    let tool = host.bind(with_agent_face(make_subagent_tool(vec![]), dir.path()).await);
    let error = tool
        .invoke(
            serde_json::json!({
                "resume_thread_id": task_uuid.clone(),
                "prompt": "continue",
            }),
            host.context(&[]),
        )
        .await
        .err()
        .unwrap_or_else(|| {
            panic!("task UUID 不是线程身份，必须显式失败（child_thread_id 是 {child_thread_id}）")
        });
    let rejection = error
        .downcast_ref::<peri_agent::tools::EffectiveToolError>()
        .expect("应为 typed preflight rejection");
    assert_eq!(
        rejection.code,
        peri_agent::tools::EffectiveToolErrorCode::InvalidInput
    );
    assert!(
        rejection.message.contains("thread not found"),
        "错误须说明线程不存在: {}",
        rejection.message
    );
    assert!(
        rejection.message.contains(&task_uuid),
        "错误须保留收到的身份: {}",
        rejection.message
    );
}
