//! 命令注册表本体（Phase 2；设计 `docs/design/command-system.md`「Routing 层」§61-67）。
//!
//! 扁平 HashMap + alias 索引的运行时注册表（设计 §63）：
//! `HashMap<fullname_lowercase, RouteEntry>` 与
//! `HashMap<alias_lowercase, fullname>`——严格输入下查找永远精确（全名 /
//! 第一等级域内裸名 / alias），域枚举用前缀过滤（决策 1：`demo:`，Mcp server
//! 名即词法首段域）。
//! 支持运行时 register / unregister / unregister_namespace、冲突裁决
//! （内置优先，纯拒绝：不覆盖、不静默，设计 §64）与 `on_change` → 投影重建
//! 回调（`McpSkillRegistry` 先例泛化，设计 §65）。
//!
//! 来源生命周期状态机（Phase 6 A2，设计「Routing 层」：发现完成才注册、
//! 断连按 namespace 前缀批量注销、重连 = 注销 → 重发现 → 重注册天然无 ABA）：
//! `sources` 表登记第二等级来源（MCP server / 插件）的 [`SourceDiscoveryState`]
//! （`Started → Discovered`），发现管线经 [`CommandRegistry::project_sources`] /
//! [`mark_source_started`](CommandRegistry::mark_source_started) /
//! [`mark_source_completed`](CommandRegistry::mark_source_completed) /
//! [`clear_source_started`](CommandRegistry::clear_source_started) 驱动；
//! `handle` 用 [`HandleToken`]（type-erased Arc + `Arc::ptr_eq`，registry 持强
//! 引用使旧 handle 分配保持存活，新 handle 不可能复用同一地址——防 ABA）。
//! 条目级注册、前缀级注销（决策 1：`demo:` 形态——Mcp server 名即词法首段
//! 域），语义逐条对齐 `mcp_skills.rs` 的 `McpSkillRegistry` 先例。
//!
//! 模块依赖约束（设计 §72）：只 import peri-acp-types 契约 + std + parking_lot，
//! **不 import 任何 handler 实现**——注册表只持 `Arc<dyn CommandHandler>` +
//! 元数据，新增命令 = 新模块在组合根注册。
//!
//! 裸名展开唯一实现 = 本模块的 alias_index 登记（第一等级裸名 → fullname 单键
//! 映射，跨域同名互斥注册）；`CommandName::bare_level1_keys()`（裸名 → core/ui
//! 两键展开）仅供词法层推导使用，勿在路由/解析路径调用（解析唯一实现不变式 3）。

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::Arc;

use parking_lot::RwLock;

use crate::command::command_name::CommandName;
use crate::command::command_route::{CommandSource, RouteEntry};
// pub re-export（plan A2 关键代码形态 :70）：`command_registry::HandleToken`
// 路径直达，消费方（A3 接线）无需经 `mcp_skills::HandleToken` 引入。
pub use crate::mcp_skills::HandleToken;

/// 注册表解析结果（P1-6 定案：查找唯一出口的返回值，跨层传递）。
///
/// 词法切分（命令名 / 参数分离）由注册表 [`CommandRegistry::resolve`] 统一完成
/// （设计不变式 3：解析唯一实现），拦截层消费 `args` 与 `entry.args_schema`。
#[derive(Clone)]
pub struct ResolvedCommand {
    pub entry: Arc<RouteEntry>,
    /// 命令名之后的参数文本（词法切分由注册表统一完成，不变式 3）。
    pub args: String,
}

// RouteEntry 含 `Arc<dyn CommandHandler>` 无法 derive Debug，手工输出可读字段。
impl fmt::Debug for ResolvedCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolvedCommand")
            .field("fullname", &self.entry.fullname)
            .field("args", &self.args)
            .finish()
    }
}

/// 注册错误（设计 §64 终态：低优先级重名 → 拒绝 + 警告，不覆盖、不静默；
/// 优先级由装配顺序实现——内置先注册，后注册者同键一律拒绝，无替换语义）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterError {
    /// 同键（全名 / alias / 第一等级裸名）二次注册：后注册者拒绝 + `tracing::warn!`。
    Conflict { key: String },
    /// fullname 首段域 != provenance.source.domain()（namespace 首段不可伪造，设计 §58）。
    ProvenanceMismatch,
    /// 词法非法：`mcp__` / 层数超限 / 第二等级单层（`mcp:hello`）/ 第一等级双层。
    MalformedName,
}

/// 第二等级来源（MCP server / 插件）发现状态（Phase 6 A2；对齐
/// `mcp_skills.rs` 的 [`crate::mcp_skills::ServerDiscoveryState`]）。
///
/// 发现完成才注册条目（`Started → Discovered` 不占位注册，设计「Routing 层」）；
/// 条目已入主表，本状态仅登记生命周期 + handle（防 ABA），不存条目本身。
#[derive(Debug, Clone)]
pub enum SourceDiscoveryState {
    /// 发现任务已 spawn（防每轮重复 spawn）。
    Started { handle: HandleToken },
    /// 已发现（条目已逐条注册进主表）。
    Discovered { handle: HandleToken },
}

impl SourceDiscoveryState {
    fn handle(&self) -> &HandleToken {
        match self {
            SourceDiscoveryState::Started { handle }
            | SourceDiscoveryState::Discovered { handle } => handle,
        }
    }
}

/// `project_sources` 的结果：需 spawn 发现的 (prefix, handle) 列表 +
/// 本轮是否发生了断连清理（`removed_any == true` 时已锁外触发 on_change 恰一次）。
#[derive(Debug, Default)]
pub struct SourceProjection {
    /// 需发现的来源（域前缀如决策 1 的 `demo`——Mcp server 名）+ 连接 handle。
    pub to_discover: Vec<(String, HandleToken)>,
    /// 本轮是否有来源被移除（断连按前缀批量注销）。
    pub removed_any: bool,
}

/// 命令注册表（设计 §63：**扁平 map，不做树**）。
///
/// - `entries`：fullname 小写键 → 条目（唯一键 = 全名小写，设计 §57/§86）。
/// - `alias_index`：alias 小写 / **第一等级**（core/ui）条目裸名小写 → fullname
///   小写键（第二等级条目不登记裸名——`mcp:hello` 形态非法，设计 §54）。
/// - `sources`：第二等级来源发现状态（Phase 6 A2；决策 1 后 Mcp server 名
///   即词法首段域，形态键如 `demo`）。
/// - `on_change`：内容变化（register Ok / unregister 命中 / unregister_namespace
///   n>0 / 生命周期撤旧）锁内取回调克隆、锁外调用（防死锁；`McpSkillRegistry`
///   先例）。锁序固定：sources → entries → alias_index → on_change（生命周期
///   方法在单次写锁内完成状态 + 主表变更，原子可见）。
pub struct CommandRegistry {
    entries: RwLock<HashMap<String, Arc<RouteEntry>>>,
    alias_index: RwLock<HashMap<String, String>>,
    sources: RwLock<BTreeMap<String, SourceDiscoveryState>>,
    on_change: RwLock<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            alias_index: RwLock::new(HashMap::new()),
            sources: RwLock::new(BTreeMap::new()),
            on_change: RwLock::new(None),
        }
    }

    /// 注册条目。
    ///
    /// 校验顺序（register 锁内完成）：词法校验（fullname 与全部 alias 均过
    /// [`CommandName::parse`]，alias 还须为 Bare 形态；失败 →
    /// [`RegisterError::MalformedName`]，设计 §59/§78「解析即失败」裁决不可经
    /// alias 旁路）→ 域校验（词法首段域 == `provenance.source.domain()`，且
    /// Level2 的 namespace 段 == 来源域内标识，否则
    /// [`RegisterError::ProvenanceMismatch`]，设计 §58 namespace 首段不可伪造）
    /// → 同键冲突（entries 已占 / alias 已占 / 第一等级裸名被占 →
    /// [`RegisterError::Conflict`] + `tracing::warn!`）。
    /// 全部通过才写入；**任何 Err 下注册表内容保持不变**（纯拒绝，无替换分支，
    /// 设计 §64——优先级由装配顺序实现，内置先注册）。
    ///
    /// 注册成功触发 `on_change`（投影重建推送）。
    pub fn register(&self, entry: RouteEntry) -> Result<(), RegisterError> {
        // 锁外预校验（词法 / 域 / namespace / alias 词法，纯函数）。
        let prepared = prepare_entry(&entry)?;
        // 冲突检查 + 写入原子完成（写锁内，防并发竞态）；任何 Err 提前返回，
        // guard 析构释放锁，注册表内容保持不变（纯拒绝，无替换分支）。
        let cb = {
            let mut entries = self.entries.write();
            let mut alias_index = self.alias_index.write();
            check_conflicts(&prepared, &entry, &entries, &alias_index)?;
            insert_entry(&mut entries, &mut alias_index, &prepared, entry);
            self.on_change.read().clone()
        };
        if let Some(cb) = cb {
            cb();
        }
        Ok(())
    }

    /// 批量注册（Phase 6 A2；对齐 [`reconcile`](Self::reconcile) 的锁结构与
    /// 纯拒绝语义）：锁外逐条预校验（词法 / 域 / namespace / alias 词法），锁内
    /// 逐条冲突检查——失败条目 `tracing::warn!` + 记入返回的错误列表后跳过
    /// （先出现者保留，不覆盖、不静默、不整体回滚），成功条目写入。
    ///
    /// 返回 `(成功数, 失败错误列表)`（**顺序与输入一致**，成功条目不占位）；
    /// 成功数 > 0 才锁外触发 `on_change` 恰一次（内容无变化不触发）。
    pub fn register_all(&self, entries: Vec<RouteEntry>) -> (usize, Vec<RegisterError>) {
        // 锁外逐条预校验（纯函数；失败 → warn，错误统一在锁内循环按输入
        // 顺序收集——预校验失败与冲突失败保持输入相对顺序）。
        let mut pending: Vec<(Option<PreparedEntry>, RouteEntry, Option<RegisterError>)> =
            Vec::with_capacity(entries.len());
        for entry in entries {
            match prepare_entry(&entry) {
                Ok(p) => pending.push((Some(p), entry, None)),
                Err(e) => {
                    tracing::warn!(fullname = %entry.fullname, error = ?e, "register_all: 条目预校验失败（拒绝，不覆盖）");
                    pending.push((None, entry, Some(e)));
                }
            }
        }
        // 锁内逐条冲突检查 + 写入（原子完成，防并发竞态）。
        let (added, errors, cb) = {
            let mut entries = self.entries.write();
            let mut alias_index = self.alias_index.write();
            let mut errors: Vec<RegisterError> = Vec::new();
            let mut added = 0;
            for (prepared, entry, precheck_error) in pending {
                match prepared {
                    Some(p) => {
                        if let Err(e) = check_conflicts(&p, &entry, &entries, &alias_index) {
                            errors.push(e);
                            continue;
                        }
                        insert_entry(&mut entries, &mut alias_index, &p, entry);
                        added += 1;
                    }
                    None => {
                        if let Some(e) = precheck_error {
                            errors.push(e);
                        }
                    }
                }
            }
            let cb = if added > 0 {
                self.on_change.read().clone()
            } else {
                None
            };
            (added, errors, cb)
        };
        if let Some(cb) = cb {
            cb();
        }
        (added, errors)
    }
}

impl CommandRegistry {
    /// 小写化精确键删除（无词法校验：直接小写化查键）。
    ///
    /// 命中 → 同步移除该条目的 alias_index 登记项（aliases + 第一等级裸名）+
    /// 触发 `on_change`；未命中 → `false`，不触发。
    pub fn unregister(&self, fullname: &str) -> bool {
        let cb = {
            let mut entries = self.entries.write();
            let mut alias_index = self.alias_index.write();
            let key = fullname.to_lowercase();
            let Some(removed) = entries.remove(&key) else {
                return false;
            };
            remove_index_entries(&mut alias_index, &removed, &key);
            self.on_change.read().clone()
        };
        if let Some(cb) = cb {
            cb();
        }
        true
    }

    /// `domain:namespace:` 前缀批量注销（断连清理，设计 §65）；返回移除数。
    /// 移除 n > 0 触发 `on_change`；未命中返回 0，不触发。
    ///
    /// 决策 1 扩展：`namespace` 传空串表示「无 namespace 段的来源」（Mcp
    /// server 名即词法首段域），传参前缀 = `{domain}`（无冒号，由
    /// `unregister_prefix_locked` 内部补 `:`，与 [`mcp_source_key`] 形态
    /// 同构）；非空 namespace 传 `{domain}:{namespace}`（plugin/user，
    /// 内部同样补冒号）。
    pub fn unregister_namespace(&self, domain: &str, namespace: &str) -> usize {
        let prefix = if namespace.is_empty() {
            domain.to_lowercase()
        } else {
            format!("{}:{}", domain.to_lowercase(), namespace.to_lowercase())
        };
        let (n, cb) = {
            let mut entries = self.entries.write();
            let mut alias_index = self.alias_index.write();
            let n = unregister_prefix_locked(&mut entries, &mut alias_index, &prefix);
            let cb = if n > 0 {
                self.on_change.read().clone()
            } else {
                None
            };
            (n, cb)
        };
        if let Some(cb) = cb {
            cb();
        }
        n
    }

    /// 原子对账（批量变更 API，Phase 3 P2-2）：先按小写全名键批量注销 `stale`
    /// 中命中项，再批量注册 `new` 条目（逐条纯拒绝：预校验失败 / 同键冲突 →
    /// `tracing::warn!` + 跳过，不覆盖、不静默，设计 §64）。
    ///
    /// **任一内容变化（注销 n>0 或注册成功 m>0）仅触发一次 `on_change`**——
    /// 「注销 + 注册」合并为单次变更事件，避免逐条 register/unregister 各触发
    /// 一次投影重建重发（O(n) 次全量推送的通知风暴）；内容无变化（注销 0 且
    /// 注册 0）不触发。返回 `(removed, added)`。
    ///
    /// 注意：注销与注册在单次写锁内完成，故 `new` 中与 `stale` 同键的条目会
    /// 先被移除再重新注册（收缩 + 增长原子完成，中间态不可见）。
    pub fn reconcile(&self, stale: &[String], new: Vec<RouteEntry>) -> (usize, usize) {
        // 锁外逐条预校验（词法 / 域 / namespace / alias 词法；失败 → warn +
        // 跳过，纯拒绝语义）。
        let prepared: Vec<(PreparedEntry, RouteEntry)> = new
            .into_iter()
            .filter_map(|entry| match prepare_entry(&entry) {
                Ok(p) => Some((p, entry)),
                Err(e) => {
                    tracing::warn!(fullname = %entry.fullname, error = ?e, "reconcile: 条目预校验失败（拒绝，不覆盖）");
                    None
                }
            })
            .collect();
        let (removed, added, cb) = {
            let mut entries = self.entries.write();
            let mut alias_index = self.alias_index.write();

            // 1) 批量注销（小写化精确键，未命中静默跳过）。
            let mut removed = 0;
            for fullname in stale {
                let key = fullname.to_lowercase();
                if let Some(removed_entry) = entries.remove(&key) {
                    remove_index_entries(&mut alias_index, &removed_entry, &key);
                    removed += 1;
                }
            }
            // 2) 批量注册（冲突逐条拒绝，先出现者保留）。
            let mut added = 0;
            for (p, entry) in prepared {
                if check_conflicts(&p, &entry, &entries, &alias_index).is_err() {
                    continue;
                }
                insert_entry(&mut entries, &mut alias_index, &p, entry);
                added += 1;
            }

            let cb = if removed + added > 0 {
                self.on_change.read().clone()
            } else {
                None
            };
            (removed, added, cb)
        };
        if let Some(cb) = cb {
            cb();
        }
        (removed, added)
    }

    /// 严格精确查找（无前缀匹配，设计 §55：`/rew` 不解析为 `/rewind`，
    /// 模糊只留 UI 搜索层）。
    ///
    /// 三段：① `trim_start_matches('/')` + `split_once(' ')` 词法切分（对齐现状
    /// `mod.rs` find 先例，args 首尾 trim）→ ② entries 全名键（小写）→
    /// ③ alias_index（小写）。任何失败（词法非法 / `mcp__` 形态 / lookup 未命中）
    /// → `None`——**全部 fall through 裁决**（设计 §78：未解析 slash 文本进管线，
    /// 不报错；仅 execute-command RPC 路径显式报错）。
    pub fn resolve(&self, input: &str) -> Option<ResolvedCommand> {
        let text = input.trim_start_matches('/');
        let (name, args) = match text.split_once(' ') {
            Some((n, a)) => (n.trim(), a.trim()),
            None => (text.trim(), ""),
        };
        if name.is_empty() {
            return None;
        }
        let name = name.to_lowercase();

        let entries = self.entries.read();
        let entry = entries.get(&name).cloned().or_else(|| {
            let alias_index = self.alias_index.read();
            alias_index
                .get(&name)
                .and_then(|fullname| entries.get(fullname).cloned())
        });
        entry.map(|entry| ResolvedCommand {
            entry,
            args: args.to_string(),
        })
    }

    /// 投影数据源（替代旧 `list()`；按 fullname 排序保证确定性输出，
    /// 投影推送与测试可预测）。
    pub fn snapshot(&self) -> Vec<Arc<RouteEntry>> {
        let mut entries: Vec<Arc<RouteEntry>> = self.entries.read().values().cloned().collect();
        entries.sort_by(|a, b| a.fullname.cmp(&b.fullname));
        entries
    }

    /// 注册/注销回调（`McpSkillRegistry` 先例）：内容变化时锁外同步调用，
    /// 供投影重建推送（`available_commands_update`）。
    pub fn set_on_change(&self, cb: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self.on_change.write() = cb;
    }

    /// 来源对账投影（对齐 `McpSkillRegistry::project_connected`，
    /// `mcp_skills.rs:70-107`）：比对 connected 列表（prefix, handle）。
    ///
    /// **prefix 形态契约**：小写 `domain:namespace`（plugin/user 如 `plugin:ecc`；
    /// 决策 1 后 Mcp 即 server 名 `demo`，无 namespace）、无尾冒号；`sources`
    /// 键大小写敏感、不归一，与 mcp_skills 先例 server 键同契约——调用方须
    /// 保证 connected 与生命周期 API 传入的 prefix 大小写一致，否则同一来源
    /// 会分叉为两个键，且本方法会把大小写不一致的来源误判为断连并批量注销
    /// （`unregister_prefix_locked` 对 entries 键小写归一，但 sources 键不归一）。
    ///
    /// - sources 中不在 connected 的来源被移除，逐个按 `{prefix}:` 前缀批量
    ///   注销（断连清理）；有移除（`removed_any`）才锁外触发 `on_change`
    ///   恰一次。
    /// - connected 中无状态或 handle 变化（`!Arc::ptr_eq`，重连）进入
    ///   `to_discover`，由调用方 spawn 发现任务。
    ///
    /// 锁序：sources 写锁 → entries/alias_index 写锁 → on_change 克隆。
    pub fn project_sources(&self, connected: &[(String, HandleToken)]) -> SourceProjection {
        let mut projection = SourceProjection::default();

        let cb = {
            let mut sources = self.sources.write();
            let connected_names: std::collections::HashSet<&str> =
                connected.iter().map(|(name, _)| name.as_str()).collect();

            // 1) 移除已消失的来源（任何移除都算 removed_any）；被移除来源
            //    逐个按前缀批量注销（断连清理，设计 §65）。
            let removed: Vec<String> = sources
                .keys()
                .filter(|k| !connected_names.contains(k.as_str()))
                .cloned()
                .collect();
            projection.removed_any = !removed.is_empty();
            if projection.removed_any {
                let mut entries = self.entries.write();
                let mut alias_index = self.alias_index.write();
                for prefix in &removed {
                    sources.remove(prefix);
                    unregister_prefix_locked(&mut entries, &mut alias_index, prefix);
                }
            }

            // 2) 新来源 / 重连（handle 指针变化）→ to_discover。
            for (name, handle) in connected {
                let needs_discovery = match sources.get(name) {
                    None => true,
                    Some(state) => !Arc::ptr_eq(state.handle(), handle),
                };
                if needs_discovery {
                    projection.to_discover.push((name.clone(), handle.clone()));
                }
            }

            // 锁内取回调克隆，锁外调用（防死锁）。
            if projection.removed_any {
                self.on_change.read().clone()
            } else {
                None
            }
        };

        if projection.removed_any {
            if let Some(cb) = cb {
                cb();
            }
        }

        projection
    }

    /// 发现任务 spawn 前同步置位（插入/覆盖为 Started；对齐
    /// `McpSkillRegistry::mark_discovery_started`，`mcp_skills.rs:115-134`）。
    ///
    /// **prefix 形态契约**：小写 `domain:namespace`（plugin/user 如 `plugin:ecc`；
    /// 决策 1 后 Mcp 即 server 名 `demo`，无 namespace）、无尾冒号；`sources`
    /// 键大小写敏感、不归一（见 [`project_sources`](Self::project_sources)
    /// 的契约说明），与 mcp_skills 先例 server 键同契约，风险由调用方一致性
    /// 保证。
    ///
    /// on_change 语义：一般插入/覆盖为 Started 不触发；**例外**——覆盖前为
    /// `Discovered` 且该前缀下有已注册条目（重连撤旧）→ 先按前缀批量注销 +
    /// 锁外触发 `on_change` 一次（陈旧条目及时撤下；随后完成回调按需再触发）。
    /// 回调在锁内取克隆、锁外调用。
    pub fn mark_source_started(&self, prefix: &str, handle: HandleToken) {
        let cb = {
            let mut sources = self.sources.write();
            let mut fire = false;
            if matches!(
                sources.get(prefix),
                Some(SourceDiscoveryState::Discovered { .. })
            ) {
                // 撤旧：该前缀下已注册条目批量注销（重连 = 注销 → 重发现 →
                // 重注册，天然无 ABA）。
                let mut entries = self.entries.write();
                let mut alias_index = self.alias_index.write();
                let n = unregister_prefix_locked(&mut entries, &mut alias_index, prefix);
                fire = n > 0;
            }
            sources.insert(prefix.to_string(), SourceDiscoveryState::Started { handle });
            if fire {
                self.on_change.read().clone()
            } else {
                None
            }
        };
        if let Some(cb) = cb {
            cb();
        }
    }

    /// 发现任务完成回写（对齐 `McpSkillRegistry::mark_discovery_completed`，
    /// `mcp_skills.rs:138-174`）：状态存在且 `Arc::ptr_eq(handle)` 才应用
    /// （旧任务回写丢弃，防 ABA）→ 按前缀批量注销清旧（重连后发现结果可能
    /// 变化）→ 逐条 `register`（冲突 / 越权条目跳过 + 告警，不整体回滚）→
    /// 有实际变化（注销 + 注册计数 > 0）才锁外触发 `on_change` **恰一次**。
    ///
    /// **触发条件与先例的差异**：先例 `mcp_skills.rs:153-168` 以内容集比较
    /// （`old_names != new_names`）决定是否触发；本模块 `sources` 状态不存
    /// 条目本身（A2 设计取舍，见模块 doc「sources 状态不存条目本身」），内容
    /// 比较不可行，`removed + added > 0` 是唯一可行近似（plan 语义矩阵 :111
    /// 「有实际变化」）。语义漂移场景：同 handle、同内容重复完成回写（周期性
    /// 重扫）时清旧 + 重注册计数均 > 0 → 假触发一次投影推送（先例不触发，
    /// 正确性无影响）。**A3 接线应避免同 handle 重复完成**（发现任务只跑一次
    /// / 完成即移出 to_discover）；若无法避免，接受一次多余投影推送。
    ///
    /// 返回实际注册成功数；旧任务回写（ptr 不一致）或来源无状态 → 0，
    /// 不触发 `on_change`。
    pub fn mark_source_completed(
        &self,
        prefix: &str,
        handle: HandleToken,
        entries: Vec<RouteEntry>,
    ) -> usize {
        // 快速路径（read 锁）：先验「状态存在 + handle 匹配（ptr_eq）」——
        // 旧任务回写（重连竞态的常态路径，见 test
        // `mark_source_completed_old_handle_writeback_discarded`）或无状态时
        // 直接返回 0，避免白做锁外逐条预校验（纯性能优化）。read 检查后状态
        // 可能并发变化，最终以写锁内二次 `ptr_eq` 为准——快速路径失败只会
        // 提前返回，写锁内失败语义不变，无 TOCTOU 危害。
        {
            let sources = self.sources.read();
            let Some(state) = sources.get(prefix) else {
                return 0;
            };
            if !Arc::ptr_eq(state.handle(), &handle) {
                return 0;
            }
        }
        // 锁外逐条预校验（词法 / 域 / namespace / alias 词法；失败 → warn +
        // 跳过，纯拒绝语义，不整体回滚）。
        let prepared: Vec<(PreparedEntry, RouteEntry)> = entries
            .into_iter()
            .filter_map(|entry| match prepare_entry(&entry) {
                Ok(p) => Some((p, entry)),
                Err(e) => {
                    tracing::warn!(fullname = %entry.fullname, error = ?e, "mark_source_completed: 条目预校验失败（拒绝，不覆盖）");
                    None
                }
            })
            .collect();
        let (added, cb) = {
            let mut sources = self.sources.write();
            let Some(state) = sources.get(prefix) else {
                return 0;
            };
            if !Arc::ptr_eq(state.handle(), &handle) {
                // 旧任务回写（来源已重连重扫）→ 丢弃，防 ABA。
                return 0;
            }
            sources.insert(
                prefix.to_string(),
                SourceDiscoveryState::Discovered { handle },
            );
            let mut entries = self.entries.write();
            let mut alias_index = self.alias_index.write();
            // 清旧：该前缀下已注册条目批量注销（发现结果可能变化）。
            let removed = unregister_prefix_locked(&mut entries, &mut alias_index, prefix);
            // 逐条注册（冲突 / 越权跳过 + 告警，不整体回滚）。
            let mut added = 0;
            for (p, entry) in prepared {
                if check_conflicts(&p, &entry, &entries, &alias_index).is_err() {
                    continue;
                }
                insert_entry(&mut entries, &mut alias_index, &p, entry);
                added += 1;
            }
            let cb = if removed + added > 0 {
                self.on_change.read().clone()
            } else {
                None
            };
            (added, cb)
        };
        if let Some(cb) = cb {
            cb();
        }
        added
    }

    /// 发现任务取消时回退 Started 状态（对齐 `McpSkillRegistry::clear_discovery_started`，
    /// `mcp_skills.rs:225-234`）：状态为 Started 且 ptr_eq 才移除（**不触发
    /// on_change**）——cancel 回退后下轮可重试。
    pub fn clear_source_started(&self, prefix: &str, handle: HandleToken) {
        let mut sources = self.sources.write();
        let matches = matches!(
            sources.get(prefix),
            Some(SourceDiscoveryState::Started { handle: h })
                if Arc::ptr_eq(h, &handle)
        );
        if matches {
            sources.remove(prefix);
        }
    }
}

impl Default for CommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// 第一等级（core/ui）条目的裸名（name 段小写）——alias_index 登记/清理依据
/// （设计 §63：第二等级条目不登记裸名，`mcp:hello` 形态非法）。
fn level1_bare_name(entry: &RouteEntry) -> Option<String> {
    match CommandName::parse(&entry.fullname) {
        Ok(CommandName::Level1 { name, .. }) => Some(name.to_lowercase()),
        _ => None,
    }
}

/// 注册条目的锁外预校验结果（`register` / `reconcile` 共用；设计 §59/§78）。
struct PreparedEntry {
    /// 登记键（小写规范全名）。
    key: String,
    /// 第一等级裸名（alias_index 登记项之一；第二等级为 None）。
    bare_name: Option<String>,
    /// 校验通过的 alias（Bare 形态小写）。
    aliases: Vec<String>,
}

/// 注册条目锁外预校验（纯函数）：词法（fullname 与全部 alias 均过
/// [`CommandName::parse`]，alias 还须为 Bare 形态；失败 →
/// [`RegisterError::MalformedName`]）→ 域校验（词法首段域 ==
/// `provenance.source.domain()`，Bare 无域禁止注册；Level2 namespace 段 ==
/// 来源域内标识，否则 [`RegisterError::ProvenanceMismatch`]，设计 §58）——
/// 通过后返回登记键与 alias_index 登记项；任何 Err 下注册表内容保持不变。
fn prepare_entry(entry: &RouteEntry) -> Result<PreparedEntry, RegisterError> {
    // 词法校验（纯函数，锁外完成）。
    let parsed = CommandName::parse(&entry.fullname).map_err(|_| RegisterError::MalformedName)?;
    // 域校验：词法首段域必须与 provenance 声明域一致（Bare 无域，注册键
    // 禁止裸名——设计 §86 裸名不是独立键，注册路径要求完整全名）。
    // 防线 2（审查 B1）：Mcp 源强制 Level2Short 形态——server 名即词法
    // 首段域；server 名恰为保留域（core/ui/plugin/user/mcp）时 fullname
    // 会被解析为 Level1/Level2，此处置 ProvenanceMismatch 拒绝（命令面
    // 源头跳过见 `mcp_namespace_reserved`，双防线）。
    let domain_ok = match (&parsed, &entry.provenance.source) {
        (_, CommandSource::Mcp { .. }) => matches!(
            &parsed,
            CommandName::Level2Short { domain, .. }
                if domain.as_str() == entry.provenance.source.domain()
        ),
        (CommandName::Bare { .. }, _) => false,
        (
            CommandName::Level1 { domain, .. }
            | CommandName::Level2 { domain, .. }
            | CommandName::Level2Short { domain, .. },
            _,
        ) => domain.as_str() == entry.provenance.source.domain(),
    };
    if !domain_ok {
        return Err(RegisterError::ProvenanceMismatch);
    }
    // namespace 段与 provenance 来源域内标识一致（设计 §58 不可伪造：
    // `Plugin { name: "ecc" }` 只能注册 `plugin:ecc:*`；决定 1 后 Mcp 走
    // Level2Short（无 namespace 段），server 名即词法首段域，已由上方域校验
    // 把关）。Level2Short 不命中本分支（无 namespace 段）。
    if let CommandName::Level2 { namespace, .. } = &parsed {
        if entry.provenance.source.namespace() != Some(namespace.as_str()) {
            return Err(RegisterError::ProvenanceMismatch);
        }
    }
    let key = parsed.full_name();
    let bare_name = level1_bare_name(entry);
    // alias 词法校验：必须为 Bare 形态（无冒号 / 无 `__` / 无空白），
    // 失败 → MalformedName（设计 §59/§78：register 路径严格校验，「解析
    // 即失败」的裁决不可经 alias 旁路）。`CommandName::parse` 已小写归一，
    // 空 alias（Empty）一并并入 MalformedName。
    let aliases: Vec<String> = entry
        .aliases
        .iter()
        .map(|a| {
            CommandName::parse(a)
                .map_err(|_| RegisterError::MalformedName)
                .and_then(|parsed| match parsed {
                    CommandName::Bare { name } => Ok(name),
                    _ => Err(RegisterError::MalformedName),
                })
        })
        .collect::<Result<_, _>>()?;
    Ok(PreparedEntry {
        key,
        bare_name,
        aliases,
    })
}

/// 锁内同键冲突检查（全名键 / alias / 第一等级裸名）；命中 → `tracing::warn!`
/// 与 `Err(Conflict)`（纯拒绝，不覆盖、不静默，设计 §64）。`register` 与
/// [`reconcile`](Self::reconcile) 共用（对 Err 跳过该条目继续，不中断批量）。
fn check_conflicts(
    prepared: &PreparedEntry,
    entry: &RouteEntry,
    entries: &HashMap<String, Arc<RouteEntry>>,
    alias_index: &HashMap<String, String>,
) -> Result<(), RegisterError> {
    // 1) 全名键已占。
    if entries.contains_key(&prepared.key) {
        tracing::warn!(fullname = %entry.fullname, "命令注册冲突：全名键已存在（拒绝，不覆盖）");
        return Err(RegisterError::Conflict {
            key: prepared.key.clone(),
        });
    }
    // 2) alias 已占。
    for a in &prepared.aliases {
        if alias_index.contains_key(a) {
            tracing::warn!(fullname = %entry.fullname, alias = %a, "命令注册冲突：别名已存在（拒绝，不覆盖）");
            return Err(RegisterError::Conflict { key: a.clone() });
        }
    }
    // 3) 第一等级裸名已占。
    if let Some(bare) = &prepared.bare_name {
        if alias_index.contains_key(bare) {
            tracing::warn!(fullname = %entry.fullname, bare = %bare, "命令注册冲突：第一等级裸名已存在（拒绝，不覆盖）");
            return Err(RegisterError::Conflict { key: bare.clone() });
        }
    }
    Ok(())
}

/// 锁内写入（entries 键 + alias_index 登记）；调用方保证冲突已检查、
/// 全部登记项唯一映射到本键。
fn insert_entry(
    entries: &mut HashMap<String, Arc<RouteEntry>>,
    alias_index: &mut HashMap<String, String>,
    prepared: &PreparedEntry,
    entry: RouteEntry,
) {
    entries.insert(prepared.key.clone(), Arc::new(entry));
    for a in &prepared.aliases {
        alias_index.insert(a.clone(), prepared.key.clone());
    }
    if let Some(bare) = &prepared.bare_name {
        alias_index.insert(bare.clone(), prepared.key.clone());
    }
}

/// 移除条目在 alias_index 中的登记项（aliases + 第一等级裸名）。
/// value 需等于被删 entries 键（小写规范全名）才移除——纯拒绝语义下
/// alias 唯一映射，此处防御性比较防误删。
fn remove_index_entries(index: &mut HashMap<String, String>, entry: &RouteEntry, key: &str) {
    for alias in &entry.aliases {
        let a = alias.to_lowercase();
        if !a.is_empty() && index.get(&a).map(String::as_str) == Some(key) {
            index.remove(&a);
        }
    }
    if let Some(bare) = level1_bare_name(entry) {
        if index.get(&bare).map(String::as_str) == Some(key) {
            index.remove(&bare);
        }
    }
}

/// 锁内按 `{prefix}:` 前缀批量注销（`prefix` 为小写 `domain:namespace`（plugin/
/// user）或决策 1 的 Mcp server 名 `demo`，键匹配前小写归一——entries 键一律
/// 小写）；返回移除数。生命周期方法（`project_sources` /
/// `mark_source_started` / `mark_source_completed`）与 [`CommandRegistry::unregister_namespace`]
/// 共用；调用方持有 entries / alias_index 写锁并自行决定 on_change 触发时机。
fn unregister_prefix_locked(
    entries: &mut HashMap<String, Arc<RouteEntry>>,
    alias_index: &mut HashMap<String, String>,
    prefix: &str,
) -> usize {
    let prefix = format!("{}:", prefix.to_lowercase());
    let keys: Vec<String> = entries
        .keys()
        .filter(|k| k.starts_with(&prefix))
        .cloned()
        .collect();
    for key in &keys {
        if let Some(removed) = entries.remove(key) {
            remove_index_entries(alias_index, &removed, key);
        }
    }
    keys.len()
}

#[cfg(test)]
#[path = "command_registry_test.rs"]
mod tests;
