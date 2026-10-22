//! MCP 面板的服务投影类型。

/// MCP 管理能力来自服务端投影，不能按名称或 transport 推断。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpControlSummary {
    pub restart: bool,
    pub stop: bool,
    pub unavailable_reason: Option<String>,
}

/// `mcp/list` 的单个工具投影（详情表格数据）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpToolSummary {
    pub name: String,
    pub description: Option<String>,
}

/// `mcp/list` 的单个资源投影（详情表格数据；agent 代理从此派生）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpResourceSummary {
    pub uri: String,
    pub name: Option<String>,
    pub description: Option<String>,
}

/// `mcp/list` 的订阅项投影。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpSubscriptionsSummary {
    pub resources: Vec<String>,
    pub tools_list_changed: bool,
    pub prompts_list_changed: bool,
    pub resources_list_changed: bool,
}

impl McpSubscriptionsSummary {
    /// 无资源订阅且全部列表变化标志关闭 = 空订阅（详情显示空节）。
    pub fn is_empty(&self) -> bool {
        self.resources.is_empty()
            && !self.tools_list_changed
            && !self.prompts_list_changed
            && !self.resources_list_changed
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpServerSummary {
    /// 服务端确认的管理能力；缺失时关闭所有动作。
    pub control: McpControlSummary,
    pub name: String,
    pub version: Option<String>,
    /// 当前连接建立时刻（RFC3339 UTC；`mcp/list` 的 connectedAt）；
    /// 从未成功连接为 None。
    pub connected_at: Option<String>,
    /// 与 server 协商出的 MCP 协议版本（`mcp/list` 的 protocolVersion，如
    /// `2026-07-28`）；未协商出为 None。
    pub protocol_version: Option<String>,
    pub status: String,
    /// 仅用于 MCP 面板的安全、单行失败摘要；完整诊断写入 tracing 日志。
    pub error_summary: Option<String>,
    pub transport: String,
    pub tools_count: usize,
    /// 工具的有界投影（`mcp/list` 的 tools，至多 64 条）；全量计数见
    /// [`Self::tools_count`]。
    pub tools: Vec<McpToolSummary>,
    /// 资源的有界投影（`mcp/list` 的 resources，至多 64 条）；全量计数见
    /// [`Self::resources_count`]。
    pub resources: Vec<McpResourceSummary>,
    /// 资源全量计数（`mcp/list` 的 resourcesCount）。
    pub resources_count: usize,
    /// 配置声明的订阅项（`mcp/list` 的 subscriptions）。
    pub subscriptions: Option<McpSubscriptionsSummary>,
    /// OAuth 待授权标记（oauth_status == NeedsAuthorization）：面板据此
    /// 显示"授权"按钮（Enter 触发 mcp/oauth_start）。
    pub needs_auth: bool,
    /// 服务器 URL（HTTP 传输），详情视图展示用。
    pub url: Option<String>,
    /// 最近一次持久化 MCP cache 结果：hit/live_fetch；None 表示尚无记录。
    pub cache_status: Option<String>,
}
