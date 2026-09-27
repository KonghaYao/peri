//! 显式云端未决收敛回归（默认 `#[ignore]`）：**结果未知的一次写入**在真引擎上的最小事实。
//!
//! 故障是**注入到真实批上的**（`FaultPlan`），不是替代返回值：
//!
//! | 实验 | 抓的是什么 |
//! | --- | --- |
//! | 响应丢失（`drop_reply`）：批真的提交、调用方只看到未知 | 未知必须如实上报、远端效果真的在，收敛之后同一条会话可以继续写 |
//!
//! v10 之后本机不再持有远端操作日志（`session_remote_operations` 已删），跨进程的
//! 「按原 id 向远端账本求证终态」这条路径不存在：`recover_persistence` 只回答本机能回答的
//! 那部分（本机已没有可证明未结态的 durable 记录）。原先依赖那份本机记录的三组实验
//! （发出前消失后按同一唯一键封闭、未决写按远端父链阻塞整棵树、重启后向账本求证）随之
//! 删除：它们要注入与读回的**都是本机事实**，在新语义下没有对象。
//!
//! 安全与清理：只操作本轮 run 命名空间；只输出计数与布尔；结束用正常删除路径清掉本轮行
//! 并复核计数为 0；不打印 locator、token 或会话内容。
//!
//! ```text
//! PERI_CLOUD_URL_KEY=<url 变量名> PERI_CLOUD_TOKEN_KEY=<token 变量名> \
//!   cargo test -p peri-resources --lib -- --ignored --nocapture --test-threads=1 cloud_recovery_
//! ```

use peri_acp_types::messages::BaseMessage;
use peri_acp_types::session_resources::PersistenceRecovery;
use peri_acp_types::store::PersistedPayload;

use super::cloud_tests::{
    check, failure, session_input, synth_binding, synth_thread, unique_run_label, with_cleanup,
    CloudTarget,
};
use super::mutation::{FaultPlan, StoreAccess};
use crate::sessions::data::SessionDataPort;

/// 实验：响应在回程丢失 —— 远端已提交、本机只能看见未知。
#[tokio::test]
#[ignore = "显式 cloud 实验：需要已授权测试库的 .env，默认不跑"]
async fn cloud_lost_reply_converges_to_applied() {
    let target = CloudTarget::load();
    let run = unique_run_label("peri-rec-lost");
    let collected: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    let result = with_cleanup(&target, &run, || async {
        let lines = lost_reply_flow(&target, &run).await?;
        *collected.lock().unwrap() = lines;
        Ok(())
    })
    .await;
    match result {
        Ok(()) => {
            let mut out = target.out();
            for line in collected.lock().unwrap().iter() {
                out.push(line.clone());
            }
            out.push("recovery_lost_reply=ok".to_owned());
            out.flush();
        }
        Err(message) => panic!("{message}"),
    }
}

async fn lost_reply_flow(target: &CloudTarget, run: &str) -> Result<Vec<String>, String> {
    let data = target
        .session_data(StoreAccess::ReadWrite)
        .await
        .map_err(failure)?;
    let thread = synth_thread(&format!("{run}-lost"));
    let frozen = format!("{{\"synth\":\"{run}\"}}");
    data.save_new_session(&session_input(
        thread.as_str(),
        &chrono::Utc::now().to_rfc3339(),
        &synth_binding(),
        &frozen,
        None,
    ))
    .await
    .map_err(failure)?;

    // 注入「批照常提交、结果按未知上报」：真实批、真实账本，只有回程被吞掉。
    data.inject_faults(FaultPlan {
        drop_reply: Some("append_history".to_owned()),
        drop_before_send: None,
    })
    .await;
    let payload = PersistedPayload::Message(BaseMessage::human("lost reply turn"));
    let error = data
        .append_history(&thread, &[payload])
        .await
        .expect_err("a swallowed reply must not be reported as applied");
    check(
        error.is_persistence_uncertain(),
        "an unknown outcome must surface as persistence uncertainty",
    )?;

    // 看远端：效果真的提交了，恢复不能把它当成丢失。
    let reader = target
        .session_data(StoreAccess::ReadOnly)
        .await
        .map_err(failure)?;
    let history = reader
        .load_session_history(&thread)
        .await
        .map_err(failure)?;
    check(
        history.len() == 1,
        "the lost-reply write is really committed on the remote side",
    )?;

    // 恢复：本机已没有可证明未结态的 durable 记录，收敛结论是「可以继续」——它绝不能
    // 反过来被当成「那次写没发生」（上一步的远端计数就是反例）。
    let recovery = data.recover_persistence(&thread).await.map_err(failure)?;
    check(
        recovery == PersistenceRecovery::Recovered,
        "a committed-but-unacknowledged write must converge to Recovered",
    )?;

    // 放行后正常写入：同一条会话在恢复之后能继续写。
    data.append_history(
        &thread,
        &[PersistedPayload::Message(BaseMessage::human(
            "after recovery",
        ))],
    )
    .await
    .map_err(failure)?;
    let history = reader
        .load_session_history(&thread)
        .await
        .map_err(failure)?;
    check(history.len() == 2, "writes must resume after recovery")?;
    Ok(vec![
        "lost_reply=unknown_then_applied".to_owned(),
        format!("history_after_recovery={}", history.len()),
    ])
}
