//! Swarm 自有事实的持久化端口（`peri-swarm` 设计 §4.3–§4.5 的 S6a 面）。
//!
//! 本模块是 `peri-resources` 对 Swarm 唯一开放的存储窄口：只有会话登记、追加式 item 流、
//! 输入收据与沙箱绑定，没有通用 SQL、没有跨会话查询，也没有事务或连接句柄。trait 对象可用
//! （[`SwarmStorePort`]），远端实现使用门面持有的**同一条**连接（`Resources::swarm_store()`）；
//! 本机 store 与不支持的部署返回显式 `Unsupported`，**不**回退本机 SQLite。
//!
//! ## 行类型的形状
//!
//! 行类型只描述落库事实，不引用 `peri-swarm` 的领域类型（依赖方向：存储实现细节不进协议
//! 类型 crate，`peri-swarm` 也不被 `peri-resources` 依赖）。因此：
//!
//! - 枚举值按 `peri-swarm` 的稳定字符串保存（`state` / `kind` / `input_id` 对应的
//!   `accepted|handed_off|failed` 等），本层不解释取值；
//! - JSON 事实按**文本**传递（`environment_json` / `source_json` / `payload_json`），与本机
//!   `thread_mq.payload_json` 同一先例：形状由 `peri-swarm` 拥有，本层只保存与读回字节。
//!
//! 三条映射约束（S6b 的 `SwarmStore` 实现按此逐字段搬移，不丢字段）：
//!
//! | S3 行字段 | 端口行字段 | 落库列 |
//! | --- | --- | --- |
//! | `instructions: Option<String>` | `instructions` | `swarm_sessions.instructions` |
//! | `environment: EnvironmentRef` | `environment_json`（serde JSON） | `swarm_sessions.environment_json` |
//! | `source: ItemSource` | `source_json`（serde JSON） | `swarm_items.source` |
//! | `payload: Value` | `payload_json`（serde JSON） | `swarm_items.payload_json` |
//! | `state` / `kind` / `InputState` | 同名 `String`（`as_str()`） | 同名 TEXT 列 |
//!
//! ## 错误与效果确定性
//!
//! 端口返回 [`SessionResourceResult`]：唯一键/主键冲突是 `Conflict`（不重试、不改号）；
//! 未找到是 `NotFound`；只读打开上的写是 `ReadOnlyStore`；本机或不支持的 store 是
//! `Unsupported`。远端批次无法证明是否生效时（丢响应、超时、回滚失败）一律
//! `PersistenceUncertain`（效果 `Unknown`），**不重试写**。`swarm_put_input` 只返回事实
//! （`Inserted` / `Existing(row)`）：同 `inputId` 不同 `fingerprint` 的冲突由上层判定。

use async_trait::async_trait;
use peri_acp_types::session_resources::{
    SessionResourceError, SessionResourceErrorKind, SessionResourceResult,
};

/// Swarm 会话登记行（对应 `swarm_sessions` 一行）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwarmSessionRow {
    /// 逻辑会话身份（`agent-` 前缀，由 `peri-swarm` 校验）。
    pub logical_id: String,
    /// 预留给 `session/new` 的 root 会话身份（唯一）。
    pub root_session_id: String,
    /// 承载该会话路由的 actor key。
    pub actor_key: String,
    /// 执行目录（绝对、规范化路径；由 `peri-swarm` 保证）。
    pub cwd: String,
    /// 持久状态：`idle|running|recovering|stopped|failed`。
    pub state: String,
    pub model: Option<String>,
    /// 内联 instructions（S3 行类型字段，原样保存）。
    pub instructions: Option<String>,
    /// environment 的 canonical JSON 文本（`{"type":...,"id":...}`）。
    pub environment_json: String,
    pub created_at: String,
    pub last_activity_at: String,
    /// 投影缓存；权威值仍是 `MAX(swarm_items.seq)`（design §4.4）。
    pub last_item_seq: u64,
}

/// 会话状态/活动时间的单向更新（S3 `SwarmSessionUpdate` 的落库面）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwarmSessionUpdate {
    pub logical_id: String,
    pub state: String,
    pub last_item_seq: u64,
    pub last_activity_at: String,
}

/// 一条追加式 item（对应 `swarm_items` 一行）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwarmItemRow {
    pub logical_id: String,
    /// 稳定顺序与恢复游标；`(logical_id, seq)` 唯一。
    pub seq: u64,
    /// 全库唯一：同一 item 的重放去重依据。
    pub item_id: String,
    /// `message|thought|tool_call|tool_update|tool_result|receipt`。
    pub kind: String,
    /// item 来源的 canonical JSON 文本（`{"agentId":"root"}` 等）。
    pub source_json: String,
    /// item 正文的 canonical JSON 文本（截断由 `peri-swarm` 决定，本层保存结果）。
    pub payload_json: String,
    pub created_at: String,
    pub truncated: bool,
}

/// 输入收据（对应 `swarm_inputs` 一行）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwarmInputRow {
    pub logical_id: String,
    pub input_id: String,
    /// 内容指纹；只保存事实，同 ID 不同指纹由上层判冲突。
    pub fingerprint: String,
    /// `accepted|handed_off|failed`。
    pub state: String,
    pub created_at: String,
}

/// 沙箱绑定（对应 `swarm_bindings` 一行）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwarmBindingRow {
    pub logical_env_id: String,
    /// 不唯一：允许诊断/灰度期间多个逻辑环境映射同一 Agent。
    pub logical_id: String,
    pub sandbox_id: String,
    pub workspace_path: String,
    pub sandbox_root: String,
    pub settings_profile: Option<String>,
    pub token_file: Option<String>,
    pub last_run_id: Option<String>,
    pub last_used_at: String,
}

/// `swarm_put_input` 的结论（`INSERT ... ON CONFLICT DO NOTHING` 语义）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SwarmInputPut {
    /// 本次插入了一行收据。
    Inserted,
    /// 同 `(logical_id, input_id)` 的收据已存在：原样返回既有行。
    Existing(SwarmInputRow),
}

/// Swarm 自有事实的持久化端口。
///
/// 每个方法都以 `logical_id` 为范围（绑定以 `logical_env_id` 为范围）；实现必须把冲突如实
/// 上报，不得改号、覆盖或把「不确定生效」报告成成功。
#[async_trait]
pub trait SwarmStorePort: Send + Sync {
    async fn swarm_upsert_session(&self, row: SwarmSessionRow) -> SessionResourceResult<()>;
    async fn swarm_get_session(
        &self,
        logical_id: &str,
    ) -> SessionResourceResult<Option<SwarmSessionRow>>;
    async fn swarm_update_session(&self, update: SwarmSessionUpdate) -> SessionResourceResult<()>;

    async fn swarm_append_item(&self, row: SwarmItemRow) -> SessionResourceResult<()>;
    /// 升序读取 `seq > after`，最多 `limit` 条。
    async fn swarm_items_after(
        &self,
        logical_id: &str,
        after: u64,
        limit: u32,
    ) -> SessionResourceResult<Vec<SwarmItemRow>>;
    /// 降序读取 `seq < before`（`None` = 最新一页），最多 `limit` 条。
    async fn swarm_items_before(
        &self,
        logical_id: &str,
        before: Option<u64>,
        limit: u32,
    ) -> SessionResourceResult<Vec<SwarmItemRow>>;
    async fn swarm_last_seq(&self, logical_id: &str) -> SessionResourceResult<u64>;

    async fn swarm_put_input(&self, row: SwarmInputRow) -> SessionResourceResult<SwarmInputPut>;
    async fn swarm_get_input(
        &self,
        logical_id: &str,
        input_id: &str,
    ) -> SessionResourceResult<Option<SwarmInputRow>>;

    async fn swarm_put_binding(&self, row: SwarmBindingRow) -> SessionResourceResult<()>;
    async fn swarm_get_binding(
        &self,
        logical_env_id: &str,
    ) -> SessionResourceResult<Option<SwarmBindingRow>>;
}

/// 明确不支持的端口：本机 SQLite store 与没有该能力的部署返回它，**不**降级到本机库。
///
/// 每个方法都返回 `Unsupported`（不是空结果）：调用方必须能看到「这个部署没有 Swarm 持久
/// 化」，而不是把缺能力读成没有数据。
pub(crate) struct UnsupportedSwarmStore;

fn unsupported<T>() -> SessionResourceResult<T> {
    Err(SessionResourceError::new(
        SessionResourceErrorKind::Unsupported,
    ))
}

#[async_trait]
impl SwarmStorePort for UnsupportedSwarmStore {
    async fn swarm_upsert_session(&self, _row: SwarmSessionRow) -> SessionResourceResult<()> {
        unsupported()
    }

    async fn swarm_get_session(
        &self,
        _logical_id: &str,
    ) -> SessionResourceResult<Option<SwarmSessionRow>> {
        unsupported()
    }

    async fn swarm_update_session(&self, _update: SwarmSessionUpdate) -> SessionResourceResult<()> {
        unsupported()
    }

    async fn swarm_append_item(&self, _row: SwarmItemRow) -> SessionResourceResult<()> {
        unsupported()
    }

    async fn swarm_items_after(
        &self,
        _logical_id: &str,
        _after: u64,
        _limit: u32,
    ) -> SessionResourceResult<Vec<SwarmItemRow>> {
        unsupported()
    }

    async fn swarm_items_before(
        &self,
        _logical_id: &str,
        _before: Option<u64>,
        _limit: u32,
    ) -> SessionResourceResult<Vec<SwarmItemRow>> {
        unsupported()
    }

    async fn swarm_last_seq(&self, _logical_id: &str) -> SessionResourceResult<u64> {
        unsupported()
    }

    async fn swarm_put_input(&self, _row: SwarmInputRow) -> SessionResourceResult<SwarmInputPut> {
        unsupported()
    }

    async fn swarm_get_input(
        &self,
        _logical_id: &str,
        _input_id: &str,
    ) -> SessionResourceResult<Option<SwarmInputRow>> {
        unsupported()
    }

    async fn swarm_put_binding(&self, _row: SwarmBindingRow) -> SessionResourceResult<()> {
        unsupported()
    }

    async fn swarm_get_binding(
        &self,
        _logical_env_id: &str,
    ) -> SessionResourceResult<Option<SwarmBindingRow>> {
        unsupported()
    }
}
