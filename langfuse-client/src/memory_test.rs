//! 内存预算与工作区租约的端口契约测试（契约 §3.4、§4、§8 用例 5）。
//!
//! 覆盖：预约/返还恰好一次、retained 饱和后进度工作区仍可推进、工作区租约排他且 Drop 回池、
//! `limit <= P` 与非零校验、`used` 含已隔离 P 且不重复计费、槽位上限公式与溢出拒绝。

use std::sync::Arc;

use crate::client::ExportConfig;
use crate::config::BatcherConfig;
use crate::error::LangfuseError;
use crate::memory::{slot_capacity, MemoryBudget};

fn deployment(config: &BatcherConfig) -> Arc<MemoryBudget> {
    MemoryBudget::for_batcher(config, &ExportConfig::default()).expect("deployment budget")
}

/// 8 MiB 部署预算：足以隔离默认 P，同时留有可观测的 retained 分区。
fn config() -> BatcherConfig {
    BatcherConfig {
        telemetry: crate::config::TelemetryConfig {
            memory_budget_bytes: 8 * 1024 * 1024,
            ..Default::default()
        },
        ..Default::default()
    }
}

#[test]
fn reserve_before_retain_and_release_once() {
    let budget = MemoryBudget::new(1024).expect("budget");
    let permit = budget.try_reserve(600).expect("first reservation fits");
    assert_eq!(permit.bytes(), 600);
    assert_eq!(budget.used(), 600);

    // 不足立即失败：不等待、不驱逐已受保护对象。
    let error = match budget.try_reserve(600) {
        Ok(_) => panic!("second reservation must be rejected"),
        Err(error) => error,
    };
    match error {
        LangfuseError::MemoryBudgetExceeded {
            requested,
            available,
        } => {
            assert_eq!(requested, 600);
            assert_eq!(available, 424);
        }
        other => panic!("unexpected error: {other}"),
    }

    drop(permit);
    assert_eq!(budget.used(), 0);
    let full = budget.try_reserve(1024).expect("full usable budget");
    assert_eq!(budget.used(), 1024);
    drop(full);
    assert_eq!(budget.used(), 0);
}

#[test]
fn retained_saturation_preserves_progress_workspace() {
    let config = config();
    let budget = deployment(&config);
    let limit = config.telemetry.memory_budget_bytes;
    let progress = budget.used();
    assert!(progress > 0 && progress < limit);

    // retained 占满可用分区：新源立即拒收。
    let retained = budget
        .try_reserve(limit - progress)
        .expect("retained partition saturates");
    assert_eq!(budget.used(), limit);
    assert!(budget.try_reserve(1).is_err());

    // 进度工作区未被挪用：编码/HTTP 仍可取得租约，Drop 回池后可再次取得。
    let prepare = budget.try_prepare_workspace().expect("prepare workspace");
    let http = budget.try_http_workspace().expect("http workspace");
    assert!(prepare.bytes() > 0 && http.bytes() > 0);
    drop(prepare);
    drop(http);
    budget
        .try_prepare_workspace()
        .expect("prepare returns to pool");

    drop(retained);
    assert_eq!(budget.used(), progress);
}

#[test]
fn workspace_pools_are_exclusive_and_return_leases() {
    let config = BatcherConfig {
        max_in_flight: 1,
        telemetry: crate::config::TelemetryConfig {
            prepare_max_in_flight: 1,
            memory_budget_bytes: 8 * 1024 * 1024,
            ..Default::default()
        },
        ..Default::default()
    };
    let budget = deployment(&config);

    let prepare = budget.try_prepare_workspace().expect("first lease");
    let error = match budget.try_prepare_workspace() {
        Ok(_) => panic!("no second prepare lease"),
        Err(error) => error,
    };
    match error {
        LangfuseError::MemoryBudgetExceeded { available, .. } => assert_eq!(available, 0),
        other => panic!("unexpected error: {other}"),
    }
    drop(prepare);
    let prepare = budget.try_prepare_workspace().expect("released lease");

    let http = budget.try_http_workspace().expect("first http lease");
    assert!(budget.try_http_workspace().is_err());
    drop(http);
    budget.try_http_workspace().expect("released http lease");
    drop(prepare);
}

#[test]
fn zero_and_below_progress_budgets_are_config_errors() {
    assert!(matches!(
        MemoryBudget::new(0),
        Err(LangfuseError::Config(_))
    ));

    let budget = deployment(&config());
    // 构造后 `used` 恰为已隔离的进度分区 P：`limit == P` 必须拒收，不能向上取整扩大配置。
    let progress = budget.used() as usize;
    let tight = BatcherConfig {
        telemetry: crate::config::TelemetryConfig {
            memory_budget_bytes: progress,
            ..config().telemetry
        },
        ..config()
    };
    assert!(matches!(
        MemoryBudget::for_batcher(&tight, &ExportConfig::default()),
        Err(LangfuseError::Config(_))
    ));
}

#[test]
fn used_includes_isolated_progress_partition_exactly_once() {
    let config = config();
    let budget = deployment(&config);
    let progress = budget.used();
    let permit = budget.try_reserve(4096).expect("retained reservation");
    assert_eq!(budget.used(), progress + 4096);
    drop(permit);
    assert_eq!(budget.used(), progress);
}

#[test]
fn slot_capacity_follows_contract_formula_and_rejects_overflow() {
    let config = BatcherConfig::default();
    assert_eq!(
        slot_capacity(&config).expect("capacity"),
        3 * config.queue_capacity
            + config.telemetry.prepare_max_in_flight
            + config.max_in_flight
            + 64
    );

    let overflowing = BatcherConfig {
        queue_capacity: usize::MAX,
        ..BatcherConfig::default()
    };
    assert!(matches!(
        slot_capacity(&overflowing),
        Err(LangfuseError::Config(_))
    ));
}
