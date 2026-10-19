//! `session/load` 的模式分流与历史页出口（REQ §5.1~§5.3、REQ-H01~H03）。
//!
//! `session/load` 保持**唯一**入口：没有 `cursor`/`history` = 初始 load（现有会话装配
//! 路径，行为不变），有合法非空 `cursor` = 旧历史页（只读一页），有 `history` =
//! `peri.historyWindowV1` 的 open/page/seek（只读窗口，不装配、不广播、不取执行锁）。
//! 模式在进入生命周期路由**之前**解析：锁策略与响应后的 setup 都由模式决定，只看方法名
//! 无法区分三者（REQ §5.1）。旧分页与窗口能力独立协商、互不推导。
//!
//! ## 参数规则（唯一，且全部失败关闭）
//!
//! | 请求 | 判定 |
//! | --- | --- |
//! | 无 `cursor` 字段 | 初始 load；带 `limit` 即拒绝（`limit` 只属于历史页） |
//! | `cursor` 合法非空字符串 | 历史页 |
//! | `cursor` 为 null / 空串 / 非字符串 / 解不开 / 属于别的会话 / 已失效 | 拒绝，**不降级为初始 load** |
//! | `limit` 非整数或不在 `1..=100` | 拒绝；缺省 = [`DEFAULT_PAGE_LIMIT`] |
//! | 历史页携带 setup 参数（`cwd` / `mcpServers`） | 拒绝：分页请求不重新装配或改绑 |
//! | 初始 load 的 `loadGeneration` 非空字符串 | 采用；缺省 = 服务端生成（见 [`crate::dispatch::LoadGeneration`]） |
//! | 历史页携带 `loadGeneration` | 拒绝：代次只属于初始 load |
//! | 本连接未协商分页能力却带 `cursor`/`limit` | 拒绝（REQ §5.3） |
//!
//! 游标的**结构版本**在 [`handle_page`] 里、委派存储之前校验（[`HistoryCursor`] 内层
//! 由分类阶段解码）：rewind / 精确删除提交成功后版本前进，旧游标与「锚点被删」得到
//! 同一个可区分错误（-32012）。追加消息与 Full/Smart compact 不推进版本，因此在途浏览
//! 不会被它们打断（REQ-H02）。
//!
//! 能力判定按**当前连接**的协商结果（[`ConnectionContext::caps`]），不读会话级
//! `caps_registry`：重连后能力变化必须重判，会话内保存的旧值不作数。
//!
//! ## 响应形状
//!
//! 历史页在**响应**里返回（不是回放通知）：
//!
//! ```json
//! {
//!   "sessionId": "…",
//!   "snapshotToken": "…",
//!   "nextCursor": "…" | null,
//!   "items": [
//!     { "entryId": "<threadId>:<messageId>", "position": 42, "oversize": false,
//!       "subItems": [ { "position": 0, "update": { "sessionUpdate": "agent_message_chunk", … } } ] }
//!   ]
//! }
//! ```
//!
//! - 子项就是回放路径的同一份投影（[`dispatch::project_payload_updates`]），
//!   一个条目 = 一个持久 payload 的完整展示封装，可含 reasoning / text / tool 多个子项。
//! - `position`：条目的**存储序键**（不透明整数，同一次浏览内可比较），子项的 `position`
//!   是它在该封装投影序列中的下标。合并契约见下。
//!
//! ## 客户端可以依赖的合并契约（REQ-H01-3、REQ-H01-6、验收 A20）
//!
//! 首屏回放、历史页与游标链携带**同一套条目身份**，客户端据此合并稀疏首屏与后续页：
//!
//! 1. `entryId` = `{会话 id}:{条目 id}`：同一次浏览里一个持久 payload 只有这一个身份，
//!    去重按它做（子项身份另由子项位置区分，不能只用 messageId）。
//! 2. `position` = 该条目在**存储序**中的位置（`rowid` 原样给出的不透明整数）：
//!    - 只在同一个 `snapshotToken` 内可比较（页之间、页与首屏之间都可以），
//!      跨 `snapshotToken` **不可比**：rewind / 精确删除会删尾部行，新插入的行复用 rowid。
//!    - 客户端**不得**解释它（不是时间戳、不是行号语义），只用于排序与区间合并；
//!      序号不连续是正常的（归档区与有效区之间本来就跳号）。
//!    - 继承区（冻结的祖先前缀）条目在本线程没有行，因此没有 `position`：它们先于全部
//!      自有条目，页永远不会返回它们（页的范围在自有历史之内）。
//! 3. 页与页、页与首屏**不重叠**：游标从首屏最旧的**自有有效**条目之前开始向更早推进，
//!    每页给的是上一页锚点之前的一段；页内按历史正序（`position` 递增）。
//!    REQ-H01-6 的例子因此可合并为 `[继承 I]，[H…]，[摘要 S]`。
//! 4. 页的顺序只由 `nextCursor` 链给出：`null` 表示更早方向确实没有可展示内容（空页 +
//!    前进的游标是合法结果）。
//!
//! 这些字段只对**当前连接**协商了分页能力时出现（REQ §5.3）：未协商的连接既拿不到
//! `nextCursor` / `snapshotToken`，回放里也不会出现 `periEntryId` / `periPosition`。
//! - `entryId` 区分来源会话与 payload；子项不能只用共享 messageId 去重。
//! - `nextCursor` 为 `null` 只表示更早方向确实没有可展示内容：空页 + 前进的游标是
//!   合法结果（一整段不可展示条目）。
//!
//! 错误码映射见 [`crate::dispatch::session_load`] 的模块文档。

use serde_json::{json, Value};

use peri_acp_types::history_page::{
    HistoryCursorEnvelope, HistoryCursorError, HistoryPageItem, RangeToken,
};
use peri_acp_types::history_window::HistoryWindowRequest;
use peri_acp_types::thread::ThreadId;
use peri_acp_types::PeriCaps;

use crate::dispatch::{self, LoadGeneration, ProjectedUpdate};
use crate::transport::types::AcpError;

use super::history_window::window_error;
use super::{AcpServerConfig, SharedSessions};

/// 单页默认条数（REQ-H03：每页 50 条、最大 100 条，按完整展示封装计）。
pub(crate) const DEFAULT_PAGE_LIMIT: usize = 50;
/// 单页条数上限（REQ-H03）。
pub(crate) const MAX_PAGE_LIMIT: usize = 100;
/// 单页响应字节目标（REQ-H03 默认 512 KiB；本片取固定值，不引入新配置通道）。
pub(crate) const PAGE_BYTE_BUDGET: usize = 512 * 1024;
/// 浏览快照身份的**展示口径版本**：投影规则或页预算的语义一变就 +1。
///
/// 存储契约看不见投影与预算，所以这半截身份只能由 ACP 铸（[`browsing_snapshot_token`]）。
const HISTORY_SNAPSHOT_VERSION: u16 = 1;

/// `session/load` 的模式（REQ §5.1）。
#[derive(Clone, Debug)]
pub(crate) enum SessionLoadMode {
    /// 无 `cursor` 字段：恢复会话及当前有效 Transcript 的展示。
    InitialLoad,
    /// 有合法非空 `cursor`：只读获取更早历史。
    HistoryPage(HistoryPageRequest),
    /// `peri.historyWindowV1` 的 open/page/seek：只读窗口请求。
    HistoryWindow {
        session_id: String,
        request: HistoryWindowRequest,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HistoryPageRequest {
    pub(crate) session_id: String,
    pub(crate) cursor: HistoryCursorEnvelope,
    pub(crate) limit: usize,
}

/// 初始 load 的参数里会改绑定或装配资源的字段：历史页请求不得携带。
const SETUP_PARAMS: [&str; 2] = ["cwd", "mcpServers"];

/// 解析 `session/load` 的模式。
///
/// `pagination` / `window_v1` 是**当前连接**分别协商出的旧分页与 `historyWindowV1`
/// 能力。未协商却带对应字段是明确拒绝，不降级为初始 load（REQ §5.2/§5.3 与验收 A25）。
pub(crate) fn classify(
    params: &Value,
    pagination: bool,
    window_v1: bool,
) -> Result<SessionLoadMode, AcpError> {
    // `history` 存在即进入 V1 分类：格式错误/未知 kind/未知字段直接拒绝，绝不按
    // 「没有 history」处理。它与旧顶层 cursor/limit/setup 字段互斥。
    if let Some(raw) = params.get("history") {
        if !window_v1 {
            return Err(window_error(
                peri_acp_types::history_window::HistoryWindowError::NotNegotiated,
            ));
        }
        for key in ["cursor", "limit", "loadGeneration", "cwd", "mcpServers"] {
            if params.get(key).is_some() {
                return Err(window_error(
                    peri_acp_types::history_window::HistoryWindowError::invalid_request(format!(
                        "{key} is mutually exclusive with history"
                    )),
                ));
            }
        }
        let session_id = session_id(params)?;
        let request = HistoryWindowRequest::decode(raw).map_err(window_error)?;
        return Ok(SessionLoadMode::HistoryWindow {
            session_id,
            request,
        });
    }
    match params.get("cursor") {
        None => {
            if params.get("limit").is_some() {
                return Err(reject(
                    "limit_without_cursor",
                    "limit is only valid on a history page request (cursor field required)",
                ));
            }
            // 形状在进入生命周期路由之前校验（取值在 handle_load；同一 helper，无第二份规则）。
            initial_load_generation(params)?;
            Ok(SessionLoadMode::InitialLoad)
        }
        Some(raw) => {
            if !pagination {
                return Err(AcpError::new(
                    -32601,
                    "peri history pagination capability not negotiated for this connection",
                )
                .with_data(json!({ "reason": "pagination_not_negotiated" })));
            }
            let raw = raw.as_str().ok_or_else(|| {
                reject(
                    "invalid_cursor",
                    "cursor must be a non-empty string produced by a previous session/load",
                )
            })?;
            if raw.is_empty() {
                return Err(reject(
                    "invalid_cursor",
                    "cursor must be a non-empty string produced by a previous session/load",
                ));
            }
            reject_setup_params(params)?;
            if params.get("loadGeneration").is_some() {
                // 代次只属于初始 load 的回放标记；历史页不重放任何东西，带上它是坏请求
                // （不静默忽略：否则客户端会拿一个永远等不到标记的暂存代次）。
                return Err(reject(
                    "load_generation_on_history_page",
                    "loadGeneration is only valid on an initial session/load",
                ));
            }
            let session_id = session_id(params)?;
            let cursor = HistoryCursorEnvelope::decode(raw, &ThreadId::from(session_id.clone()))
                .map_err(cursor_error)?;
            let limit = parse_limit(params.get("limit"))?;
            Ok(SessionLoadMode::HistoryPage(HistoryPageRequest {
                session_id,
                cursor,
                limit,
            }))
        }
    }
}

/// 只读回答一页历史：**不取执行生命周期锁**，不做任务绑定 / MCP attach / 命令广播 /
/// continuation 调度；存储 I/O 在全局 sessions 锁之外（验收 A21）。
///
/// 调用方已按 [`classify`] 校验参数与能力；本函数补两件事：调用方当前是否有权读这条
/// 会话（会话已加载且未关闭），以及游标的**结构版本**是否仍是当前版本——版本校验在短锁内
/// 完成、**先于**存储委派，旧游标不再换取一次往返（rewind / 精确删除已使浏览作废）。
/// 锚点与范围身份仍由存储回答（可能晚于 ACP 的版本快照失效）。
pub(crate) async fn handle_page(
    cfg: &AcpServerConfig,
    sessions: &SharedSessions,
    caps: &PeriCaps,
    request: &HistoryPageRequest,
) -> Result<Value, AcpError> {
    // 短锁 prepare：会话存在性、关闭状态与结构版本，不做任何装配。
    {
        let sessions = sessions.lock().await;
        let state = sessions
            .get(&request.session_id)
            .ok_or_else(|| AcpError::new(-32602, "session not found"))?;
        if state.closing {
            return Err(AcpError::new(-32010, "Session is closing"));
        }
        request
            .cursor
            .verify_structure_version(state.structure_version)
            .map_err(cursor_error)?;
    }
    // 锁外工作：分页是只读浏览，与正在执行的 turn 并行。
    let page = dispatch::load_history_page(
        cfg.controller.as_ref(),
        &request.session_id,
        &request.cursor.cursor,
        request.limit,
        PAGE_BYTE_BUDGET,
    )
    .await?;
    let items: Vec<Value> = page
        .items
        .iter()
        .filter_map(|item| page_item(item, &request.session_id, caps))
        .collect();
    Ok(json!({
        "sessionId": &request.session_id,
        // 浏览身份原样回传：它就是选路时定下的那一份（投影口径 + 范围身份）。
        "snapshotToken": &request.cursor.snapshot_token,
        "nextCursor": page.next_cursor.as_ref().map(|cursor| {
            HistoryCursorEnvelope::new(
                request.cursor.structure_version,
                request.cursor.snapshot_token.clone(),
                cursor.clone(),
            )
            .encode()
        }),
        "items": items,
    }))
}

/// 一次浏览的展示身份（REQ-H02 的 `snapshotToken`）：范围身份 + 本版固定的展示口径。
///
/// 投影规则版本与页预算在 ACP 层是编译期常量，存储契约看不见它们——这正是这半截身份
/// 只能铸在这里的原因。同一次浏览内它保持不变（客户端只当不透明值回传）。
fn browsing_snapshot_token(range_token: &str) -> RangeToken {
    format!("{HISTORY_SNAPSHOT_VERSION}:{PAGE_BYTE_BUDGET}:{range_token}")
}

/// 初始 load 的可选代次参数（REQ §5.2 `loadGeneration`）。
///
/// 缺省（字段缺席）= `None`，由调用方生成；带了就必须是**非空字符串**——类型错误是坏请求，
/// 不能当成「没带」而被静默换成服务端新代次（那会让客户端暂存的旧代次永远等不到响应标记）。
pub(crate) fn initial_load_generation(params: &Value) -> Result<Option<LoadGeneration>, AcpError> {
    match params.get("loadGeneration") {
        None => Ok(None),
        Some(raw) => raw
            .as_str()
            .filter(|value| !value.is_empty())
            .map(|value| Some(LoadGeneration::from_client(value)))
            .ok_or_else(|| {
                reject(
                    "invalid_load_generation",
                    "loadGeneration must be a non-empty string",
                )
            }),
    }
}

/// 初始 load 的响应补充分页身份（只在当前连接协商了分页能力时；REQ §5.3）。
///
/// 只改响应体、不改变 load 的成败：读不到边界事实时**失败**——给协商过的客户端回一个
/// 缺字段的成功响应等于谎称「没有更早历史」，那正是本需求要避免的静默降级。
///
/// 首个游标是**版本化封装**：结构版本取自刚发布的会话状态，浏览身份由
/// [`browsing_snapshot_token`] 铸造（两者都是存储契约看不见的）。
pub(crate) async fn attach_initial_cursor(
    cfg: &AcpServerConfig,
    sessions: &SharedSessions,
    session_id: &str,
    response: &mut Value,
) -> Result<(), AcpError> {
    let initial = dispatch::initial_history_cursor(cfg.controller.as_ref(), session_id).await?;
    // 短锁读版本，锁外不持有；会话必须已登记（初始 load 成功即已发布状态）。
    let structure_version = {
        let sessions = sessions.lock().await;
        sessions
            .get(session_id)
            .map(|state| state.structure_version)
            .ok_or_else(|| {
                AcpError::new(
                    -32603,
                    "session/load produced no live session state for the initial cursor",
                )
            })?
    };
    let snapshot_token = browsing_snapshot_token(&initial.snapshot_token);
    let Some(object) = response.as_object_mut() else {
        return Err(AcpError::new(
            -32603,
            "session/load response is not a JSON object",
        ));
    };
    object.insert(
        "nextCursor".to_string(),
        match &initial.next_cursor {
            Some(cursor) => Value::String(
                HistoryCursorEnvelope::new(
                    structure_version,
                    snapshot_token.clone(),
                    cursor.clone(),
                )
                .encode(),
            ),
            None => Value::Null,
        },
    );
    object.insert("snapshotToken".to_string(), Value::String(snapshot_token));
    Ok(())
}

/// 一条持久化条目的展示封装；没有客户端可见子项时不返回该条目。
///
/// `position` 取存储序键（**不透明整数**，不是页内下标）：同一次浏览内可比较，客户端才能把
/// 这一页并进稀疏首屏的正确区间（合并契约见模块文档）。子项的 `position` 仍是它在本条
/// payload 投影序列中的下标——两者不是同一把尺子，不能混用。
///
/// `entryId` 走 [`dispatch::entry_id`]（回放出口的同一份格式）：历史页与回放必须给出
/// **同一个**身份字符串，去重与区间合并才成立。`marks` 传 `None`：历史页响应不属于任何一次
/// load 代次，也不重复打回放戳（身份由本函数的 `entryId` 字段给出）。
pub(crate) fn page_item(item: &HistoryPageItem, thread_id: &str, caps: &PeriCaps) -> Option<Value> {
    let sub_items: Vec<Value> = dispatch::project_payload_updates(&item.payload, caps, None)
        .into_iter()
        .enumerate()
        .filter_map(|(position, update)| sub_item(update, position))
        .collect();
    if sub_items.is_empty() {
        return None;
    }
    Some(json!({
        "entryId": dispatch::entry_id(thread_id, item.id),
        "position": item.position,
        "oversize": item.oversized,
        "subItems": sub_items,
    }))
}

/// 一个投影子项：`update` 与回放路径同源；结构化 reminder 沿用同一受众判据。
fn sub_item(update: ProjectedUpdate, position: usize) -> Option<Value> {
    match update {
        ProjectedUpdate::Session(update) => {
            let update = serde_json::to_value(update).ok()?;
            Some(json!({ "position": position, "update": update }))
        }
        ProjectedUpdate::SystemReminder(reminder) => {
            if !dispatch::reminder_visible_to_client(&reminder) {
                return None;
            }
            let reminder = serde_json::to_value(&reminder).ok()?;
            Some(json!({ "position": position, "systemReminder": reminder }))
        }
    }
}

fn session_id(params: &Value) -> Result<String, AcpError> {
    params
        .get("sessionId")
        .or_else(|| params.get("session_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))
}

fn reject_setup_params(params: &Value) -> Result<(), AcpError> {
    for key in SETUP_PARAMS {
        if params.get(key).is_some() {
            return Err(reject(
                "setup_param_on_history_page",
                &format!("{key} is not accepted on a history page request"),
            ));
        }
    }
    Ok(())
}

fn parse_limit(raw: Option<&Value>) -> Result<usize, AcpError> {
    let Some(raw) = raw else {
        return Ok(DEFAULT_PAGE_LIMIT);
    };
    let value = raw
        .as_u64()
        .ok_or_else(|| reject("invalid_limit", "limit must be an integer within 1..=100"))?;
    if !(1..=MAX_PAGE_LIMIT as u64).contains(&value) {
        return Err(reject(
            "invalid_limit",
            "limit must be an integer within 1..=100",
        ));
    }
    Ok(value as usize)
}

/// 游标解码错误 → ACP 错误（与存储侧同一套码，见 `dispatch::session_load` 模块文档）。
fn cursor_error(error: HistoryCursorError) -> AcpError {
    dispatch::history_page_error(error.into())
}

fn reject(reason: &str, message: &str) -> AcpError {
    AcpError::new(-32602, message).with_data(json!({ "reason": reason }))
}

#[cfg(test)]
#[path = "history_page_test.rs"]
mod tests;
