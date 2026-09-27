//! 显式云端 mutation 实验（默认 `#[ignore]`）：在已授权测试库上验证 §5.1 机制的最小事实。
//!
//! 覆盖（每条都断言可观察结果，不靠「连上了」推断）：
//!
//! | 实验 | 断言的事实 |
//! | --- | --- |
//! | 原子批回滚 | 批中途约束失败 → 零部分结果（资格行与效果行都不存在，新连接同样读不到） |
//! | 重复资格 | 同一 operation_id 第二次调用返回**原收据**、不写第二个效果 |
//! | 并发资格 | 两个连接竞争同一 operation_id → 至多一方提交，效果只出现一次 |
//! | 终态封闭 | 封闭先提交 → 迟到的原请求不可能再生效；封闭已生效操作 → 返回原收据 |
//! | 跨连接读 | 提交后的行对新连接可读；身份读取可用且初始化幂等（不覆盖） |
//!
//! 安全与清理：
//!
//! - 只操作**本轮 run 命名空间**下的对象（`op_ledger` 行按 `run.` 前缀）；结束时用一次
//!   正常 mutation 路径删除本轮**合成数据**（会话/消息），**不删收据**：`op_ledger` 的
//!   每一行都是封闭证据，删除它会让迟到的同 id 请求失去判据（P7）。不动任务 schema、
//!   不动其他轮次的数据，也不碰本机库里别的 store 的记录。
//! - 本轮**不写**业务表：效果语句落在 `op_ledger` 自身（唯一键表）上作见证。canonical 会话表
//!   在打开时按 `IF NOT EXISTS` 就位（清理语句删的是那几张表，表得先在），但本轮不写它们，
//!   也**不能**把本轮证据说成业务表行为已验证。
//! - 只输出安全状态/版本/计数/布尔事实，全部经 `SafeOut` 校验。
//!
//! ```text
//! PERI_CLOUD_URL_KEY=<url 变量名> PERI_CLOUD_TOKEN_KEY=<token 变量名> \
//!   cargo test -p peri-resources --lib -- --ignored --nocapture --test-threads=1 cloud_mutation_
//! ```

use std::future::Future;

use peri_acp_types::session_resources::{
    SessionResourceError, SessionResourceErrorKind, SessionResourceResult,
};

use super::cloud_tests::{remote_error_class, unique_run_label, CloudTarget};
use super::failure::RemoteFailureClass;
use super::ledger::{qualify_statement, OperationId, OperationIdentity, Receipt};
use super::mutation::{
    MutationOutcome, OperationResolution, QualifiedMutation, RemoteStore, StoreAccess,
};
use super::schema::{REMOTE_SCHEMA_VERSION, STORE_CONTRACT};

// 清理器**不删收据**：`peri_op_ledger` 里本轮的行（资格行与见证行）保持原样，它们正是
// 「这次操作发生过」的证据。清理只针对本轮合成会话/消息（本批实验不建业务表，因此这两条
// 语句在这里通常是空操作——它们的意义是「清理器只删合成数据」这条契约本身）。

fn check(condition: bool, message: &str) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(message.to_owned())
    }
}

fn witness(run: &str, label: &str) -> OperationIdentity {
    OperationIdentity::new(
        OperationId::scoped(run, label),
        "probe_witness",
        &[run, label],
    )
}

fn receipt_of(outcome: &MutationOutcome) -> Result<&Receipt, String> {
    match outcome {
        MutationOutcome::Applied { receipt, .. } => Ok(receipt),
        other => Err(format!("expected applied, got {other:?}")),
    }
}

fn mutation(identity: OperationIdentity, witnesses: &[&OperationIdentity]) -> QualifiedMutation {
    QualifiedMutation {
        identity,
        effects: witnesses
            .iter()
            .map(|identity| qualify_statement(identity, "witness"))
            .collect(),
    }
}

/// 跑实验体、无论成败都尝试清理本轮对象，最后一起判定。
async fn with_cleanup<F, Fut>(store: &RemoteStore, run: &str, body: F) -> Result<(), String>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    let outcome = body().await;
    let cleanup = cleanup_run(store, run).await;
    match (outcome, cleanup) {
        (Err(message), _) => Err(message),
        (Ok(()), Ok(MutationOutcome::Applied { .. })) => Ok(()),
        (Ok(()), Ok(other)) => Err(format!("cleanup did not apply: {other:?}")),
        (Ok(()), Err(error)) => Err(format!("cleanup failed: {}", remote_error_class(&error))),
    }
}

/// 用正常 mutation 路径清理本轮合成数据，然后复核**收据仍在**。
///
/// 与 `cloud_tests::cleanup_run` 同一条契约：清理器只删合成数据，收据（本轮每一次操作的
/// 账本行）**有意保留**——它们的空间成本由 P7 口径承担，删除它们才是缺陷。因此这里的判据
/// 不是「本轮行归零」，而是「清理自身的收据可读回、且它没有被自己的清理语句删掉」。
async fn cleanup_run(store: &RemoteStore, run: &str) -> SessionResourceResult<MutationOutcome> {
    let identity =
        OperationIdentity::new(OperationId::scoped(run, "cleanup"), "probe_cleanup", &[run]);
    let outcome = store
        .apply_qualified(&QualifiedMutation {
            identity: identity.clone(),
            effects: super::cloud_tests::cleanup_effects(run),
        })
        .await?;
    match store.resolve_operation(&identity.operation_id).await? {
        // 清理这一次操作自己的收据也必须留下：迟到请求据此不能重放。
        super::ledger::LedgerRow::Applied { .. } => Ok(outcome),
        other => Err(SessionResourceError::new(
            SessionResourceErrorKind::Internal {
                detail: format!("cleanup receipt was not retained: {other:?}"),
            },
        )),
    }
}

/// 初始化本任务 schema（授权范围内）并报告安全身份摘要。
async fn prepare(target: &CloudTarget) -> Result<(RemoteStore, String), String> {
    let store = target
        .store(StoreAccess::ReadWrite)
        .await
        .map_err(|error| format!("connect failed: {}", remote_error_class(&error)))?;
    let outcome = store
        .initialize_store()
        .await
        .map_err(|error| format!("initialize failed: {}", remote_error_class(&error)))?;
    // canonical 会话表就位（全部 `IF NOT EXISTS`，重复执行是空操作）：本轮不写它们，
    // 但清理语句删的正是这几张表，形状得先到——不依赖「别的用例已经建过表」。
    store
        .force_parent_checks_off()
        .await
        .map_err(|error| format!("foreign key reset failed: {}", remote_error_class(&error)))?;
    store
        .apply_schema(super::session_schema::initialization_plan())
        .await
        .map_err(|error| format!("session schema failed: {}", remote_error_class(&error)))?;
    let run = unique_run_label("peri-mut");
    let mut out = target.out();
    out.push(format!("run_prefix={}", &run[..14.min(run.len())]));
    out.push(format!("schema_version={REMOTE_SCHEMA_VERSION}"));
    out.push(format!("contract_len={}", STORE_CONTRACT.len()));
    out.push(format!(
        "store_id_len={}",
        outcome.store_id().as_str().len()
    ));
    // 已授权测试库早就有身份：本进程只是读回它（`Created` 只可能在空库上出现）。
    out.push(format!("initialize_verdict={}", initialize_name(&outcome)));
    out.flush();
    Ok((store, run))
}

/// 实验一：原子批中途失败 → 零部分结果。
#[tokio::test]
#[ignore = "显式 cloud 实验：需要已授权测试库的 .env，默认不跑"]
async fn cloud_mutation_atomic_batch_leaves_no_partial_results() {
    let target = CloudTarget::load();
    let (store, run) = prepare(&target).await.expect("cloud target unusable");
    let result = with_cleanup(&store, &run, || async {
        let identity = OperationIdentity::new(
            OperationId::scoped(&run, "atomic"),
            "probe_atomic",
            &[&run, "atomic"],
        );
        let first = witness(&run, "atomic_witness_1");
        let second = witness(&run, "atomic_witness_2");
        // 效果顺序：见证 1、见证 1 的重复插入（同主键 → 约束失败）、见证 2（永不执行）。
        let mutation = QualifiedMutation {
            identity: identity.clone(),
            effects: vec![
                qualify_statement(&first, "witness"),
                qualify_statement(&first, "witness"),
                qualify_statement(&second, "witness"),
            ],
        };
        let outcome = store
            .apply_qualified(&mutation)
            .await
            .map_err(|error| format!("apply failed: {}", remote_error_class(&error)))?;

        let mut out = target.out();
        out.push(format!("atomic_batch_outcome={}", outcome_name(&outcome)));
        match &outcome {
            MutationOutcome::NotApplied {
                class,
                rejected_statement,
            } => {
                out.push(format!("rejected_statement={rejected_statement:?}"));
                check(
                    *class == RemoteFailureClass::Constraint,
                    "expected a constraint rejection",
                )?;
                check(
                    *rejected_statement == Some(2),
                    "expected the duplicate effect to be rejected",
                )?;
            }
            other => return Err(format!("expected determinate rejection, got {other:?}")),
        }

        // 新连接复核：资格、见证 1、见证 2 都不存在。
        let fresh = target
            .store(StoreAccess::ReadWrite)
            .await
            .map_err(|error| format!("second connect failed: {}", remote_error_class(&error)))?;
        for (label, operation_id) in [
            ("qualification_absent", identity.operation_id.clone()),
            ("witness_1_absent", first.operation_id.clone()),
            ("witness_2_absent", second.operation_id.clone()),
        ] {
            match fresh.resolve_operation(&operation_id).await {
                Ok(super::ledger::LedgerRow::Absent) => out.push(format!("{label}=true")),
                Ok(other) => return Err(format!("{label} expected absent, got {other:?}")),
                Err(error) => {
                    return Err(format!(
                        "{label} read failed: {}",
                        remote_error_class(&error)
                    ));
                }
            }
        }
        let _ = fresh.close().await;
        out.flush();
        Ok(())
    })
    .await;
    let _ = store.close().await;
    result.unwrap_or_else(|message| panic!("{message}"));
}

/// 实验二：同一 operation_id 重复调用返回原收据，且不写第二个效果。
#[tokio::test]
#[ignore = "显式 cloud 实验：需要已授权测试库的 .env，默认不跑"]
async fn cloud_mutation_repeat_qualification_returns_the_original_receipt() {
    let target = CloudTarget::load();
    let (store, run) = prepare(&target).await.expect("cloud target unusable");
    let result = with_cleanup(&store, &run, || async {
        let identity = OperationIdentity::new(
            OperationId::scoped(&run, "repeat"),
            "probe_repeat",
            &[&run, "repeat"],
        );
        let first_effect = witness(&run, "repeat_witness_1");
        let first = store
            .apply_qualified(&mutation(identity.clone(), &[&first_effect]))
            .await
            .map_err(|error| format!("first apply failed: {}", remote_error_class(&error)))?;
        let original = receipt_of(&first)?.clone();

        // 第二次：同一 operation_id，不同效果（模拟重试时输入摘要不同）。
        let second_effect = witness(&run, "repeat_witness_2");
        let second = store
            .apply_qualified(&mutation(identity.clone(), &[&second_effect]))
            .await
            .map_err(|error| format!("repeat apply failed: {}", remote_error_class(&error)))?;

        let mut out = target.out();
        out.push(format!("first_outcome={}", outcome_name(&first)));
        out.push(format!("repeat_outcome={}", outcome_name(&second)));
        match &second {
            MutationOutcome::Applied { receipt, replayed } => {
                check(*replayed, "repeat must be marked as a replay")?;
                check(
                    receipt.as_str() == original.as_str(),
                    "repeat must return the stored original receipt",
                )?;
            }
            other => return Err(format!("expected replayed applied, got {other:?}")),
        }

        // 原效果在、第二个效果不在。
        let state = store
            .resolve_operation(&identity.operation_id)
            .await
            .map_err(|error| format!("resolve failed: {}", remote_error_class(&error)))?;
        match state {
            super::ledger::LedgerRow::Applied { receipt, .. } => check(
                receipt.as_str() == original.as_str(),
                "stored receipt differs from the first result",
            )?,
            other => return Err(format!("expected applied ledger row, got {other:?}")),
        }
        match store.resolve_operation(&second_effect.operation_id).await {
            Ok(super::ledger::LedgerRow::Absent) => {
                out.push("second_effect_absent=true".to_owned())
            }
            Ok(other) => return Err(format!("second effect should not exist, got {other:?}")),
            Err(error) => {
                return Err(format!(
                    "second effect read failed: {}",
                    remote_error_class(&error)
                ));
            }
        }
        match store.resolve_operation(&first_effect.operation_id).await {
            Ok(super::ledger::LedgerRow::Applied { .. }) => {
                out.push("first_effect_present=true".to_owned())
            }
            Ok(other) => return Err(format!("first effect missing, got {other:?}")),
            Err(error) => {
                return Err(format!(
                    "first effect read failed: {}",
                    remote_error_class(&error)
                ));
            }
        }
        out.flush();
        Ok(())
    })
    .await;
    let _ = store.close().await;
    result.unwrap_or_else(|message| panic!("{message}"));
}

/// 实验三：两个连接竞争同一 operation_id → 至多一方提交。
#[tokio::test]
#[ignore = "显式 cloud 实验：需要已授权测试库的 .env，默认不跑"]
async fn cloud_mutation_concurrent_qualification_applies_once() {
    let target = CloudTarget::load();
    let (store, run) = prepare(&target).await.expect("cloud target unusable");
    let second_store = target
        .store(StoreAccess::ReadWrite)
        .await
        .expect("second connection");
    let result = with_cleanup(&store, &run, || async {
        let identity = OperationIdentity::new(
            OperationId::scoped(&run, "race"),
            "probe_race",
            &[&run, "race"],
        );
        let effect = witness(&run, "race_witness");
        let mutation = mutation(identity.clone(), &[&effect]);

        let (left, right) = tokio::join!(
            store.apply_qualified(&mutation),
            second_store.apply_qualified(&mutation)
        );
        let left = left.map_err(|error| format!("left failed: {}", remote_error_class(&error)))?;
        let right =
            right.map_err(|error| format!("right failed: {}", remote_error_class(&error)))?;

        let mut out = target.out();
        out.push(format!("race_left={}", outcome_name(&left)));
        out.push(format!("race_right={}", outcome_name(&right)));
        let fresh_wins = matches!(
            &left,
            MutationOutcome::Applied {
                replayed: false,
                ..
            }
        ) as u8
            + matches!(
                &right,
                MutationOutcome::Applied {
                    replayed: false,
                    ..
                }
            ) as u8;
        check(fresh_wins == 1, "exactly one side must commit first")?;
        for outcome in [&left, &right] {
            match outcome {
                MutationOutcome::Applied { .. } => {}
                // 写忙/超时/网络都属于「未决」，不得被误判为业务失败。
                MutationOutcome::Unknown { class } => {
                    out.push(format!("race_loser_unknown={}", class.as_str()));
                }
                other => return Err(format!("unexpected loser outcome: {other:?}")),
            }
        }

        // 胜者一行：效果只出现一次，且终态不是 closed。
        match store.resolve_operation(&identity.operation_id).await {
            Ok(super::ledger::LedgerRow::Applied { .. }) => {
                out.push("race_qualification_applied=true".to_owned())
            }
            Ok(other) => return Err(format!("expected applied, got {other:?}")),
            Err(error) => return Err(format!("resolve failed: {}", remote_error_class(&error))),
        }
        match store.resolve_operation(&effect.operation_id).await {
            Ok(super::ledger::LedgerRow::Applied { .. }) => {
                out.push("race_effect_applied_once=true".to_owned())
            }
            Ok(other) => return Err(format!("expected effect applied, got {other:?}")),
            Err(error) => {
                return Err(format!(
                    "effect read failed: {}",
                    remote_error_class(&error)
                ));
            }
        }
        out.flush();
        Ok(())
    })
    .await;
    let _ = second_store.close().await;
    let _ = store.close().await;
    result.unwrap_or_else(|message| panic!("{message}"));
}

/// 实验四：终态封闭——封闭先提交则迟到的原请求不可能再生效；封闭已生效操作返回原收据。
#[tokio::test]
#[ignore = "显式 cloud 实验：需要已授权测试库的 .env，默认不跑"]
async fn cloud_mutation_closure_beats_late_original_and_replays_applied() {
    let target = CloudTarget::load();
    let (store, run) = prepare(&target).await.expect("cloud target unusable");
    let result = with_cleanup(&store, &run, || async {
        let mut out = target.out();

        // (1) 从未发送的操作被封闭：封闭胜出，迟到的原请求不能再产生数据变化。
        let ghost = OperationIdentity::new(
            OperationId::scoped(&run, "ghost"),
            "probe_ghost",
            &[&run, "ghost"],
        );
        let closure = store
            .close_operation(&ghost)
            .await
            .map_err(|error| format!("closure failed: {}", remote_error_class(&error)))?;
        out.push(format!("ghost_closure={}", resolution_name(&closure)));
        check(
            matches!(closure, OperationResolution::ClosedNeverApplied),
            "closure of an unqualified operation must prove it never applied",
        )?;

        let late_effect = witness(&run, "ghost_witness");
        let late =
            OperationIdentity::new(ghost.operation_id.clone(), "probe_ghost", &[&run, "ghost"]);
        let late_outcome = store
            .apply_qualified(&mutation(late, &[&late_effect]))
            .await
            .map_err(|error| format!("late apply failed: {}", remote_error_class(&error)))?;
        out.push(format!("late_original={}", outcome_name(&late_outcome)));
        check(
            !matches!(late_outcome, MutationOutcome::Applied { .. }),
            "a closed operation must never become applied",
        )?;
        match store.resolve_operation(&late_effect.operation_id).await {
            Ok(super::ledger::LedgerRow::Absent) => out.push("late_effect_absent=true".to_owned()),
            Ok(other) => return Err(format!("late effect must be absent, got {other:?}")),
            Err(error) => {
                return Err(format!(
                    "late effect read failed: {}",
                    remote_error_class(&error)
                ));
            }
        }

        // (2) 已生效的操作被封闭：返回原收据，数据不动。
        let applied = OperationIdentity::new(
            OperationId::scoped(&run, "applied"),
            "probe_applied",
            &[&run, "applied"],
        );
        let applied_effect = witness(&run, "applied_witness");
        let applied_outcome = store
            .apply_qualified(&mutation(applied.clone(), &[&applied_effect]))
            .await
            .map_err(|error| format!("apply failed: {}", remote_error_class(&error)))?;
        let original = receipt_of(&applied_outcome)?.clone();

        let replay = store
            .close_operation(&applied)
            .await
            .map_err(|error| format!("closure failed: {}", remote_error_class(&error)))?;
        out.push(format!("applied_closure={}", resolution_name(&replay)));
        match replay {
            OperationResolution::Applied { receipt } => check(
                receipt.as_str() == original.as_str(),
                "closure of an applied operation must return the original receipt",
            )?,
            other => return Err(format!("expected replayed receipt, got {other:?}")),
        }
        match store.resolve_operation(&applied_effect.operation_id).await {
            Ok(super::ledger::LedgerRow::Applied { .. }) => {
                out.push("applied_effect_intact=true".to_owned())
            }
            Ok(other) => return Err(format!("applied effect must stay, got {other:?}")),
            Err(error) => {
                return Err(format!(
                    "applied effect read failed: {}",
                    remote_error_class(&error)
                ));
            }
        }
        out.flush();
        Ok(())
    })
    .await;
    let _ = store.close().await;
    result.unwrap_or_else(|message| panic!("{message}"));
}

/// 实验五：提交后的行对新连接可读；身份读取与初始化幂等（不覆盖既有身份）。
#[tokio::test]
#[ignore = "显式 cloud 实验：需要已授权测试库的 .env，默认不跑"]
async fn cloud_mutation_committed_rows_are_visible_to_a_new_connection() {
    let target = CloudTarget::load();
    let (store, run) = prepare(&target).await.expect("cloud target unusable");
    let result = with_cleanup(&store, &run, || async {
        let mut out = target.out();
        let identity = OperationIdentity::new(
            OperationId::scoped(&run, "visible"),
            "probe_visible",
            &[&run, "visible"],
        );
        let effect = witness(&run, "visible_witness");
        let outcome = store
            .apply_qualified(&mutation(identity.clone(), &[&effect]))
            .await
            .map_err(|error| format!("apply failed: {}", remote_error_class(&error)))?;
        let original = receipt_of(&outcome)?.clone();

        // 新连接：读回同一条事实，并确认身份读取在真实引擎上可用。
        let fresh = target
            .store(StoreAccess::ReadWrite)
            .await
            .map_err(|error| format!("second connect failed: {}", remote_error_class(&error)))?;
        match fresh.resolve_operation(&identity.operation_id).await {
            Ok(super::ledger::LedgerRow::Applied { receipt, .. }) => check(
                receipt.as_str() == original.as_str(),
                "new connection must read the same receipt",
            )?,
            Ok(other) => return Err(format!("expected applied row, got {other:?}")),
            Err(error) => return Err(format!("resolve failed: {}", remote_error_class(&error))),
        }
        out.push("cross_connection_read_after_write=true".to_owned());

        let first_identity = store
            .read_identity()
            .await
            .map_err(|error| format!("identity read failed: {}", remote_error_class(&error)))?;
        let second_identity = fresh
            .read_identity()
            .await
            .map_err(|error| format!("identity read failed: {}", remote_error_class(&error)))?;
        match (first_identity, second_identity) {
            (
                super::schema::StoreIdentityRead::Present(left),
                super::schema::StoreIdentityRead::Present(right),
            ) => {
                check(left.matches_build(), "identity must match this build")?;
                check(
                    left.store_id == right.store_id,
                    "both connections must see one store identity",
                )?;
                out.push(format!("schema_version={}", left.schema_version));
            }
            other => return Err(format!("expected initialized identity, got {other:?}")),
        }

        // 再次初始化：读回同一身份、且结论是 `Existing`（本次没有建立任何东西）。
        let again = fresh
            .initialize_store()
            .await
            .map_err(|error| format!("re-initialize failed: {}", remote_error_class(&error)))?;
        check(
            initialize_name(&again) == "existing",
            "re-initialize must report an existing identity, not a created one",
        )?;
        match fresh.read_identity().await {
            Ok(super::schema::StoreIdentityRead::Present(snapshot)) => check(
                snapshot.store_id == *again.store_id(),
                "re-initialize must not mint a second identity",
            )?,
            Ok(other) => return Err(format!("expected present identity, got {other:?}")),
            Err(error) => {
                return Err(format!(
                    "identity read failed: {}",
                    remote_error_class(&error)
                ));
            }
        }
        out.push("initialize_is_idempotent=true".to_owned());
        let _ = fresh.close().await;
        out.flush();
        Ok(())
    })
    .await;
    let _ = store.close().await;
    result.unwrap_or_else(|message| panic!("{message}"));
}

/// 初始化结论的安全名字（只给类别，不给身份值）。
fn initialize_name(outcome: &super::schema::StoreIdentityOutcome) -> &'static str {
    match outcome {
        super::schema::StoreIdentityOutcome::Created(_) => "created",
        super::schema::StoreIdentityOutcome::Existing(_) => "existing",
    }
}

fn outcome_name(outcome: &MutationOutcome) -> &'static str {
    match outcome {
        MutationOutcome::Applied {
            replayed: false, ..
        } => "applied",
        MutationOutcome::Applied { replayed: true, .. } => "applied_replayed",
        MutationOutcome::ClosedNeverApplied => "closed_never_applied",
        MutationOutcome::NotApplied { .. } => "not_applied",
        MutationOutcome::Unknown { .. } => "unknown",
    }
}

fn resolution_name(resolution: &OperationResolution) -> &'static str {
    match resolution {
        OperationResolution::Applied { .. } => "applied",
        OperationResolution::ClosedNeverApplied => "closed_never_applied",
        OperationResolution::StillUnknown { .. } => "still_unknown",
    }
}
