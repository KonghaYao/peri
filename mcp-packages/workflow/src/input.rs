use std::sync::{Arc, OnceLock};

use peri_workflow::runtime::WorkflowRuntime;

/// 会话装配注入 workflow 实例的上下文输入（对标 `WorkspaceInstanceInput` 的角色）。
///
/// **槽位语义**：会话环境装配期只创建空槽（[`Self::slot`]）并交给 builtin `workflow`
/// 实例；真正的 `WorkflowRuntime` 由会话旧链在 session/new / restore / fork 构造
/// （其执行依赖来自该时点才冻结的会话数据，早于池装配不可得），随后经
/// [`Self::bind`] 绑定**同一份** `Arc`——MCP 面与旧链共享同一状态（进度 / 任务表 /
/// 事件日志同源，`Arc::ptr_eq` 可观察）。
///
/// 绑定**首次为准**：首次绑定生效，重复绑定不替换既有运行时；同一实例重复绑定
/// 幂等，不同实例重复绑定是可观察的装配冲突（见 [`BindOutcome`]）。实例面每次调用
/// 按槽位解析；未绑定由 [`Self::runtime`] 返回 `None`，调用方以明确错误收敛，
/// 不伪装成实例缺失。
#[derive(Clone)]
pub struct WorkflowInstanceInput {
    runtime: Arc<OnceLock<Arc<WorkflowRuntime>>>,
}

/// 槽位绑定结果：让调用方区分「首次 / 幂等重复 / 实例冲突」，而不是共用 `false`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindOutcome {
    /// 首次绑定成功。
    Bound,
    /// 同一运行时实例重复绑定（`Arc::ptr_eq`）：幂等成功，不替换既有值。
    AlreadyBound,
    /// 槽位已绑定到**不同**运行时实例：装配冲突，不替换既有值。
    Conflict,
}

impl WorkflowInstanceInput {
    /// 空槽：等待 [`Self::bind`] 绑定运行时（会话装配期的唯一生产形态）。
    pub fn slot() -> Self {
        Self {
            runtime: Arc::new(OnceLock::new()),
        }
    }

    /// 立即绑定的输入（便利构造：测试夹具与已就绪路径，跳过槽位等待）。
    pub fn new(runtime: Arc<WorkflowRuntime>) -> Self {
        let input = Self::slot();
        let outcome = input.bind(runtime);
        debug_assert_eq!(
            outcome,
            BindOutcome::Bound,
            "fresh slot accepts the first binding"
        );
        input
    }

    /// 绑定运行时（首次为准；结果区分首次 / 幂等重复 / 不同实例冲突，均不替换既有值）。
    pub fn bind(&self, runtime: Arc<WorkflowRuntime>) -> BindOutcome {
        match self.runtime.set(runtime) {
            Ok(()) => BindOutcome::Bound,
            Err(rejected) => match self.runtime.get() {
                Some(current) if Arc::ptr_eq(current, &rejected) => BindOutcome::AlreadyBound,
                // `Err` 分支下槽位必然已初始化，`None` 不可达；保守按冲突收敛。
                _ => BindOutcome::Conflict,
            },
        }
    }

    /// 解析当前运行时；`None` = 槽位尚未绑定（实例先于会话就绪初始化）。
    pub fn runtime(&self) -> Option<&Arc<WorkflowRuntime>> {
        self.runtime.get()
    }
}
