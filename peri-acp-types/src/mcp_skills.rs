//! Session 级 MCP skill 远端注册表（纯数据 + 回调，无 RPC 依赖）。
//!
//! 发现任务（peri-middlewares 侧）写入，Skills 侧读取合并；断连清理、
//! 重连重扫、发现完成经 `on_change` 回调通知（如重发 commands 更新）。
//! 全程无 tokio 依赖——并发安全由 `parking_lot::RwLock` 保证。

use std::collections::BTreeMap;

use crate::skills::SkillMetadata;

/// 连接身份 token（防重连竞态 + 防 ABA：registry 持强引用使旧 handle 分配保持
/// 存活，新 handle 不可能复用同一地址；比较用 `Arc::ptr_eq`。type-erased 使
/// peri-acp-types 不依赖 concrete 类型）。
pub type HandleToken = std::sync::Arc<dyn std::any::Any + Send + Sync>;

/// 单个 server 的发现状态。
#[derive(Debug, Clone)]
pub enum ServerDiscoveryState {
    /// 发现任务已 spawn（防每轮重复 spawn）
    Started { handle: HandleToken },
    /// 已发现（或已尝试失败——失败置空条目，不重试；重连才重扫）
    Discovered {
        handle: HandleToken,
        entries: Vec<SkillMetadata>,
    },
}

impl ServerDiscoveryState {
    fn handle(&self) -> &HandleToken {
        match self {
            ServerDiscoveryState::Started { handle }
            | ServerDiscoveryState::Discovered { handle, .. } => handle,
        }
    }
}

/// `project_connected` 的结果：需 spawn 发现的 (name, handle) 列表 +
/// 本轮是否发生了断连清理（`removed_any == true` 时已锁外触发 on_change）。
#[derive(Debug, Default)]
pub struct Projection {
    pub to_discover: Vec<(String, HandleToken)>,
    pub removed_any: bool,
}

struct RegistryInner {
    servers: BTreeMap<String, ServerDiscoveryState>,
    on_change: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
}

/// Session 级 MCP skill 远端注册表。
pub struct McpSkillRegistry {
    inner: parking_lot::RwLock<RegistryInner>,
}

impl McpSkillRegistry {
    pub fn new() -> Self {
        Self {
            inner: parking_lot::RwLock::new(RegistryInner {
                servers: BTreeMap::new(),
                on_change: None,
            }),
        }
    }

    /// before_agent 投影：比对 connected（name, handle）列表。
    ///
    /// - registry 中不在 connected 的 server 被移除；有移除才锁外触发 on_change。
    /// - connected 中无条目或 handle 变化（`!Arc::ptr_eq`，重连）的进入
    ///   `to_discover`，由调用方 spawn 发现任务。
    pub fn project_connected(&self, connected: &[(String, HandleToken)]) -> Projection {
        let mut projection = Projection::default();

        let cb = {
            let mut guard = self.inner.write();
            let connected_names: std::collections::HashSet<&str> =
                connected.iter().map(|(name, _)| name.as_str()).collect();

            // 1) 移除已消失的 server（任何条目被移除都算 removed_any）。
            let before = guard.servers.len();
            guard
                .servers
                .retain(|name, _| connected_names.contains(name.as_str()));
            projection.removed_any = guard.servers.len() < before;

            // 2) 新 server / 重连（handle 指针变化）→ to_discover。
            for (name, handle) in connected {
                let needs_discovery = match guard.servers.get(name) {
                    None => true,
                    Some(state) => !std::sync::Arc::ptr_eq(state.handle(), handle),
                };
                if needs_discovery {
                    projection.to_discover.push((name.clone(), handle.clone()));
                }
            }

            // 锁内取回调克隆，锁外调用（防死锁）。
            guard.on_change.clone()
        };

        if projection.removed_any {
            if let Some(cb) = cb {
                cb();
            }
        }

        projection
    }

    /// 发现任务 spawn 前同步置位（插入/覆盖为 Started）。
    ///
    /// on_change 语义（评审 LOW-2）：一般插入/覆盖为 Started 不触发；**例外**——
    /// 覆盖前为 `Discovered` 且 entries 非空时触发一次（旧条目从 `all_skills`
    /// 消失，commands 列表及时撤下陈旧条目；随后完成回调按需再触发）。
    /// 回调在锁内取克隆、锁外调用。
    pub fn mark_discovery_started(&self, server: &str, handle: HandleToken) {
        let cb = {
            let mut guard = self.inner.write();
            let fire = matches!(
                guard.servers.get(server),
                Some(ServerDiscoveryState::Discovered { entries, .. }) if !entries.is_empty()
            );
            guard
                .servers
                .insert(server.to_string(), ServerDiscoveryState::Started { handle });
            if fire {
                guard.on_change.clone()
            } else {
                None
            }
        };
        if let Some(cb) = cb {
            cb();
        }
    }

    /// 发现任务完成回写：条目存在且 `Arc::ptr_eq(handle)` 才应用（旧任务回写
    /// 丢弃）；新旧 entries 的 name 集（大小写不敏感）变化才锁外触发 on_change。
    pub fn mark_discovery_completed(
        &self,
        server: &str,
        handle: HandleToken,
        entries: Vec<SkillMetadata>,
    ) {
        let cb = {
            let mut guard = self.inner.write();
            let Some(state) = guard.servers.get(server) else {
                return;
            };
            if !std::sync::Arc::ptr_eq(state.handle(), &handle) {
                // 旧任务回写（server 已重连重扫）→ 丢弃。
                return;
            }
            let old_names: std::collections::BTreeSet<String> = match state {
                ServerDiscoveryState::Discovered { entries, .. } => {
                    entries.iter().map(|e| e.name.to_lowercase()).collect()
                }
                ServerDiscoveryState::Started { .. } => Default::default(),
            };
            let new_names: std::collections::BTreeSet<String> =
                entries.iter().map(|e| e.name.to_lowercase()).collect();
            guard.servers.insert(
                server.to_string(),
                ServerDiscoveryState::Discovered { handle, entries },
            );
            if old_names != new_names {
                guard.on_change.clone()
            } else {
                None
            }
        };
        if let Some(cb) = cb {
            cb();
        }
    }

    /// 读取面热更新回写（resource_tool 恢复成功后调用）：仅当该 server
    /// 当前状态为 Discovered 且 handle 指针一致（`Arc::ptr_eq`，防与重连/
    /// 新发现竞态——恢复 RPC 期间 server 可能已重连重扫）时替换 entries；
    /// 替换后锁外触发 on_change（旧 entries 非空或新 entries 非空——对齐
    /// `mark_discovery_started` 的覆盖语义：内容变化即通知 commands 侧）。
    /// 状态为 Started（发现任务进行中，不写——整体覆盖以发现完成为准）或
    /// handle 不一致 → 返回 false 不写入。
    pub fn refresh_entries(
        &self,
        server: &str,
        handle: &HandleToken,
        entries: Vec<SkillMetadata>,
    ) -> bool {
        let cb = {
            let mut guard = self.inner.write();
            let Some(ServerDiscoveryState::Discovered {
                handle: current,
                entries: old_entries,
            }) = guard.servers.get(server)
            else {
                // Started / 无状态：不写。
                return false;
            };
            if !std::sync::Arc::ptr_eq(current, handle) {
                // 恢复期间重连/新发现 → 旧 handle 回写丢弃。
                return false;
            }
            let fire = !old_entries.is_empty() || !entries.is_empty();
            guard.servers.insert(
                server.to_string(),
                ServerDiscoveryState::Discovered {
                    handle: handle.clone(),
                    entries,
                },
            );
            if fire {
                guard.on_change.clone()
            } else {
                None
            }
        };
        if let Some(cb) = cb {
            cb();
        }
        true
    }

    /// 发现任务取消时回退 Started 状态：条目为 Started 且 ptr_eq 才移除
    /// （不触发 on_change）——session/cancel 后下轮可重试。
    pub fn clear_discovery_started(&self, server: &str, handle: HandleToken) {
        let mut guard = self.inner.write();
        let matches = matches!(
            guard.servers.get(server),
            Some(ServerDiscoveryState::Started { handle: h }) if std::sync::Arc::ptr_eq(h, &handle)
        );
        if matches {
            guard.servers.remove(server);
        }
    }

    /// 全部已发现技能（BTreeMap 键序，每 server 按 entries 顺序拼合；跳过
    /// Started 状态的 server）。
    pub fn all_skills(&self) -> Vec<SkillMetadata> {
        let guard = self.inner.read();
        guard.servers.values().flat_map(entries_of).collect()
    }

    /// 单 server 的技能（条目顺序；未发现/Started → 空）。
    pub fn skills_of(&self, server: &str) -> Vec<SkillMetadata> {
        let guard = self.inner.read();
        guard
            .servers
            .get(server)
            .map(entries_of)
            .unwrap_or_default()
    }

    /// 按全名查找（小写精确匹配 `mcp__<server>__<skill>`）；未命中再试
    /// `<server>:<skill>` 别名（rsplit_once(':')，后缀非空才拼全名）。
    pub fn find(&self, name: &str) -> Option<SkillMetadata> {
        let guard = self.inner.read();
        let needle = name.to_lowercase();
        if let Some(entry) = find_exact(&guard.servers, &needle) {
            return Some(entry);
        }
        // 别名：<server>:<skill> → mcp__<server>__<skill>（复用拼名逻辑，与
        // SkillTool 别名分支同源，杜绝内联拼名漂移）。
        if let Some((prefix, suffix)) = name.rsplit_once(':') {
            if !suffix.is_empty() {
                let full = mcp_skill_name(prefix, suffix).to_lowercase();
                return find_exact(&guard.servers, &full);
            }
        }
        None
    }

    /// 仅含 Discovered 且 entries 非空的 server（BTreeMap 键序）。
    pub fn server_names(&self) -> Vec<String> {
        let guard = self.inner.read();
        guard
            .servers
            .iter()
            .filter(|(_, state)| !entries_of(state).is_empty())
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// 变更通知回调（session 级；None 清除）。回调在锁外同步调用。
    pub fn set_on_change(&self, cb: Option<std::sync::Arc<dyn Fn() + Send + Sync>>) {
        let mut guard = self.inner.write();
        guard.on_change = cb;
    }

    /// 单 server 当前发现状态（测试可观察）。
    pub fn discovery_state(&self, server: &str) -> Option<ServerDiscoveryState> {
        let guard = self.inner.read();
        guard.servers.get(server).cloned()
    }
}

impl Default for McpSkillRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Discovered 的条目引用（Started → 空）。
fn entries_of(state: &ServerDiscoveryState) -> Vec<SkillMetadata> {
    match state {
        ServerDiscoveryState::Discovered { entries, .. } => entries.clone(),
        ServerDiscoveryState::Started { .. } => Vec::new(),
    }
}

/// BTreeMap 键序 + 条目序的小写全名精确匹配。
fn find_exact(
    servers: &BTreeMap<String, ServerDiscoveryState>,
    needle: &str,
) -> Option<SkillMetadata> {
    for state in servers.values() {
        if let ServerDiscoveryState::Discovered { entries, .. } = state {
            for entry in entries {
                if entry.name.to_lowercase() == *needle {
                    return Some(entry.clone());
                }
            }
        }
    }
    None
}

/// `<server>:<skill>` → `mcp__<server>__<skill>`（registry.find 与 SkillTool
/// 别名分支共用）。
pub fn mcp_skill_name(server: &str, skill: &str) -> String {
    format!("mcp__{}__{}", server, skill)
}

#[cfg(test)]
#[path = "mcp_skills_test.rs"]
mod tests;
