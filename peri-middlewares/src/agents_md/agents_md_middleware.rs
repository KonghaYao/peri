use super::{async_trait, hook_state, AgentResult, Arc, Middleware, RwLock};

/// AgentsMdMiddleware - 注入项目指引文件（AGENTS.md / CLAUDE.md）
///
/// 内容来源是会话冻结的项目指令快照（见模块文档）；本类型不执行文件系统 I/O。
#[derive(Debug)]
pub struct AgentsMdMiddleware {
    /// 贡献文本（构造时由冻结内容合成；只读使用）。
    pub(super) cached_contribution: Arc<RwLock<Option<String>>>,
}

impl AgentsMdMiddleware {
    #[must_use]
    pub fn new() -> Self {
        Self {
            cached_contribution: Arc::new(RwLock::new(None)),
        }
    }

    /// 注入冻结的指令正文（main 已含 `@import` 展开；local 为 `CLAUDE.local.md`）。
    ///
    /// M4：main 与 local 是**独立输入**，任一非空都应贡献——`None`（该来源
    /// 不可得）与 `Some("")`（显式空快照）语义不同但都不贡献，且**不授权重新
    /// 扫描**（本中间件无任何文件系统回落）。合成口径：非空白部分按
    /// main → local 顺序以空行连接；全为空白时整体不贡献。
    #[must_use]
    pub fn with_frozen_parts(self, main: Option<String>, local: Option<String>) -> Self {
        let mut parts: Vec<String> = Vec::new();
        {
            let frozen_parts = [main, local].into_iter().flatten();
            for part in frozen_parts {
                if !part.trim().is_empty() {
                    parts.push(part);
                }
            }
        }
        if !parts.is_empty() {
            *self
                .cached_contribution
                .write()
                .expect("frozen prompt contribution must not expose an interrupted cache update") =
                Some(parts.join("\n\n"));
        }
        self
    }
}

impl Default for AgentsMdMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Middleware for AgentsMdMiddleware {
    fn name(&self) -> &str {
        peri_acp_types::meta_harness::ChainSlot::AgentsMd.name()
    }

    fn prompt_contribution(&self) -> Option<String> {
        self.cached_contribution
            .read()
            .expect("frozen prompt contribution must not expose an interrupted cache update")
            .clone()
    }

    async fn before_agent(&self, _state: &mut dyn hook_state::BeforeAgentState) -> AgentResult<()> {
        // W5：本中间件已无读盘/搜索/import 行为——贡献在构造时由冻结内容定格，
        // 会话内不再变化（ARC-FROZEN-001）。保留空 hook 以维持链槽位与
        // `name()` 判定（MetaHarness 关闭键）不变。
        Ok(())
    }
}
