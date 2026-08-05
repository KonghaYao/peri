use serde::{Deserialize, Serialize};

/// WebSocket 消息协议类型
///
/// 使用 serde 内部标签模式（tag = "type"），JSON 中 "type" 字段决定变体。
/// `DataChunk` 和 `TransferComplete` 为双向共用变体。
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WsMessage {
    // ── Client → Server ──
    /// sender 请求生成配对码
    #[serde(rename = "request_pair")]
    RequestPair,
    /// receiver 输入配对码加入
    #[serde(rename = "join_pair")]
    JoinPair { pair_code: String },
    /// receiver 告知 sender 要同步的项
    #[serde(rename = "sync_config")]
    SyncConfig { items: SyncItems },

    // ── 双向 ──
    /// 加密数据块
    #[serde(rename = "data_chunk")]
    DataChunk { seq: u32, data: Vec<u8> },
    /// 传输完成
    #[serde(rename = "transfer_complete")]
    TransferComplete { checksum: String },

    // ── Server → Client ──
    /// 返回配对码给 sender
    #[serde(rename = "pair_created")]
    PairCreated { pair_code: String },
    /// 通知双方配对成功
    #[serde(rename = "pair_joined")]
    PairJoined {
        #[serde(default)]
        peer_info: Option<PeerInfo>,
    },
    /// 错误消息
    #[serde(rename = "error")]
    Error { code: String, message: String },
}

/// 对端信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfo {
    /// 客户端版本
    pub version: String,
    /// 操作系统
    pub os: String,
    /// 主机名
    pub hostname: String,
}

/// 同步数据包（打包后加密传输）
#[derive(Debug, Serialize, Deserialize)]
pub struct SyncPackage {
    /// 数据格式版本，当前固定为 1
    pub version: u32,
    /// Unix 时间戳（秒）
    pub timestamp: u64,
    /// 同步项
    #[serde(flatten)]
    pub items: SyncItems,
}

/// 同步项集合，接收方可按需选择
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SyncItems {
    /// settings.json 内容
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<SettingsItem>,
    /// skills 目录文件列表
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<FilesItem>,
    /// MCP 配置
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp: Option<McpItem>,
    /// 插件文件列表
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugins: Option<FilesItem>,
}

/// 单个配置文件内容
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SettingsItem {
    /// settings.json 在 .peri/ 下的原文
    pub content: String,
    /// .claude/settings.json 的内容（可选）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_content: Option<String>,
}

/// 文件集合（用于 skills、plugins 等）
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct FilesItem {
    #[serde(default)]
    pub files: Vec<FileEntry>,
}

/// 单个文件条目
#[derive(Debug, Serialize, Deserialize)]
pub struct FileEntry {
    /// 相对路径，如 "skills/my-skill/SKILL.md"
    pub path: String,
    /// 文件二进制内容
    pub content: Vec<u8>,
}

/// MCP 配置
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct McpItem {
    /// ~/.mcp.json 内容
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global: Option<String>,
    /// 项目级 .mcp.json 内容（如有）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}
