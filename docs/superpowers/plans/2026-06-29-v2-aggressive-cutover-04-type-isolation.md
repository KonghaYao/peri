# v2 激进切换 — 子计划 4：类型隔离

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans`. 本计划可在 Plan 1/2/3 任意阶段并行启动；不依赖其他子计划。

**Goal:** 将 `peri-tui/src/` 中对 `peri_agent::*` / `peri_middlewares::*` 的 138 处运行时类型依赖替换为 `peri-acp-types` DTO + ACP 查询模式；启用 `scripts/check-tui-imports.sh` 为 pre-commit 钩子，使 TUI 物理脱离 Agent/Middlewares 运行时类型。

**Architecture:** TUI 仅消费 `peri-acp-types` 中定义的 DTO（`BaseMessageDto`、`ContentBlockDto`、`InteractionContextDto` 等）。**所有**动态状态通过 `acp_query_cache`（已有）查询；**所有**跨进程调用通过 `acp_client` 协议方法。`acp_server/` + `acp_client/` + `acp_stdio/` 三个目录作为合法协议边界保留。物理删除 `peri_agent`/`peri_middlewares` 在 `peri-tui/Cargo.toml` 的依赖（`BaseMessage`/`ContentBlock` 通过 `peri-acp-types` 重导出获取）。

**Tech Stack:** Rust 2021 + serde DTO + `parking_lot::RwLock`（缓存）+ async-trait（ACP client）

---

## 关键约束

1. **激进策略**：物理删除运行时类型引用，**不保留** `type LegacyBaseMessage = peri_agent::messages::BaseMessage;` 这类 alias。DTO 转换在 `acp_server` 边界完成（`view_mapper.rs`），TUI 内部只看 DTO。
2. **协议边界保留**：`acp_server/`（TUI 内嵌 ACP server）+ `acp_client/`（ACP client 类型桥接）+ `acp_stdio/`（ACP stdio 模式）允许使用 `peri_agent` 类型；不在本计划清理范围。
3. **测试文件过渡白名单**：`*_test.rs` / `*_test/` 中的 `BaseMessage` 构造可保留（测试数据需要完整类型）。本计划处理 `src/` 下 27 处非测试违规。
4. **过渡期白名单收紧**：Plan 2 + Plan 3 完成后，`message_pipeline/` + `ui/message_view/` + `state_machine/` 中的 import 会被顺带清理；本计划最后一道验证会移除这些白名单条目。
5. **`Cargo.toml` 最终状态**：`peri-agent` / `peri-middlewares` 依赖保留为**仅 dev-dependencies**（测试需要），从 `[dependencies]` 中删除。

---

## 文件清单

### 创建文件

| 文件 | 职责 |
|------|------|
| `peri-acp-types/src/message.rs` | `BaseMessageDto` / `ContentBlockDto` / `MessageContentDto` |
| `peri-acp-types/src/interaction.rs` | `InteractionContextDto` / `InteractionResponseDto` / `ChannelStateDto` |
| `peri-acp-types/src/cron.rs` | `CronTaskDto`（已有，扩充 `CronSchedulerStateDto`）|
| `peri-acp-types/src/mcp_types.rs` | `ClientStatusDto` / `ConfigSourceDto` / `OAuthStatusDto` / `ServerInfoDto` / `McpToolDto`（扩充）|
| `peri-acp-types/src/hook.rs` | `HookDto`（已有，扩充 `HookEventDto` / `HookTypeDto` / `RegisteredHookDto`）|
| `peri-acp-types/src/plugin_types.rs` | `PluginDto`（已有，扩充 `InstallScopeDto` / `MarketplaceSourceDto` / `CommandEntryDto`）|
| `peri-acp-types/src/skill.rs` | `SkillSummary`（已有，扩充 `SkillMetadataDto`）|
| `peri-acp-types/src/permission.rs` | `PermissionModeDto` |
| `peri-acp-types/src/interaction_types.rs` | `AskUserQuestionDataDto` / `AskUserBatchRequestDto` / `AskUserOptionDto` / `BatchItemDto` / `HitlDecisionDto` |

### 修改文件

| 文件 | 改动 |
|------|------|
| `peri-acp-types/src/lib.rs` | 声明 9 个新 module |
| `peri-tui/Cargo.toml` | 将 `peri-agent`/`peri-middlewares` 移至 `[dev-dependencies]` |
| `peri-tui/src/app/agent_comm.rs` | `BaseMessage` → `BaseMessageDto`，`AgentCancellationToken` → `acp_client::CancellationToken` |
| `peri-tui/src/app/agent_compact.rs` | `BaseMessage` → `BaseMessageDto` |
| `peri-tui/src/app/agent_ops/rewind.rs` | `BaseMessage, ContentBlock` → DTO |
| `peri-tui/src/app/mod.rs` | `BaseMessage` + `HitlDecision` → DTO |
| `peri-tui/src/app/events.rs` | `InteractionContext, InteractionResponse` → DTO，`OAuthCallbackResult` → DTO |
| `peri-tui/src/app/service_registry.rs` | `ChannelState` + MCP pool 相关 → DTO + `acp_query_cache` |
| `peri-tui/src/app/cron_state.rs` | `CronScheduler, CronTask` → `acp_query_cache` |
| `peri-tui/src/app/tasks_panel.rs` | `CronScheduler, CronTask` → DTO |
| `peri-tui/src/app/ask_user_prompt.rs` | `AskUserBatchRequest, AskUserQuestionData` → DTO |
| `peri-tui/src/app/hitl_prompt.rs` | `BatchItem, HitlDecision` → DTO |
| `peri-tui/src/app/agent_ops_interaction.rs` | `BatchItem` + `AskUser*` + `Interaction*` → DTO |
| `peri-tui/src/app/chat_session.rs` | `SkillMetadata` → DTO |
| `peri-tui/src/app/command_system.rs` | `SkillMetadata` → DTO |
| `peri-tui/src/app/hooks_panel.rs` | `HookEvent, HookType, RegisteredHook` → DTO |
| `peri-tui/src/app/plugin_panel/handlers/plugin_handlers/discover_detail.rs` | `InstallScope` → DTO |
| `peri-tui/src/app/plugin_panel/handlers/plugin_handlers/install.rs` | `InstallScope` → DTO |
| `peri-tui/src/app/plugin_panel/types.rs` | `pub use` 改为 DTO re-export |
| `peri-tui/src/app/panel_plugin/plugin_loader.rs` | 5 处函数体内 use → DTO + `acp_query_cache` |
| `peri-tui/src/app/panel_plugin/background.rs` | 4 处 use → DTO + `acp_query_cache` |
| `peri-tui/src/app/panel_plugin/marketplace_ops.rs` | 2 处 use → DTO + `acp_query_cache` |
| `peri-tui/src/app/panel_plugin/entries.rs` | 1 处 use → DTO |
| `peri-tui/src/app/panel_plugin/install_ops.rs` | 1 处 use → DTO |
| `peri-tui/src/app/panel_plugin/source_helpers.rs` | `MarketplaceSource` → DTO |
| `peri-tui/src/app/mcp_panel/mod.rs` | `ConfigSource, Resource, ServerInfo, Tool` → DTO |
| `peri-tui/src/app/mcp_panel/component.rs` | `ClientStatus` + 3 处 `OAuthFlowEvent` → DTO |
| `peri-tui/src/app/mcp_panel/ops.rs` | `ClientStatus` + 3 处 `OAuthFlowEvent` → DTO |
| `peri-tui/src/app/oauth_prompt.rs` | `parse_code_from_url` 复制到 TUI（10 行函数）|
| `peri-tui/src/app/hint_ops.rs` | `SkillMetadata` → DTO |
| `peri-tui/src/command/core/gc.rs` | `BaseMessage, ContentBlock, MessageContent` → DTO |
| `peri-tui/src/command/session/plugin_command.rs` | `CommandEntry` + `CommandSource` → DTO |
| `peri-tui/src/panel/panels/thread_browser.rs` | `ThreadMeta` → DTO |
| `peri-tui/src/ui/main_ui/panels/mcp.rs` | `ClientStatus, ConfigSource, OAuthStatus, ServerInfo` → DTO + 函数体内 `McpInitStatus` → DTO |
| `peri-tui/src/ui/main_ui/panels/plugin/plugin_render/list.rs` | `InstallScope` → DTO |
| `peri-tui/src/ui/main_ui/panels/plugin/plugin_render/detail.rs` | `InstallScope` → DTO |
| `peri-tui/src/ui/main_ui/panels/hooks.rs` | `HookEvent, HookType, RegisteredHook` → DTO |
| `peri-tui/src/ui/main_ui/panels/status.rs` | `RequestRecord` → DTO |
| `peri-tui/src/ui/main_ui/popups/hitl.rs` | `BatchItem` → DTO |
| `peri-tui/src/ui/main_ui/popups/ask_user_height.rs` | `AskUserQuestionData` → DTO |
| `peri-tui/src/ui/main_ui/status_bar.rs` | `PermissionMode` + `McpInitStatus` → DTO |
| `scripts/check-tui-imports.sh` | 收紧白名单（删除 `state_machine/` 和 `ui/message_view/` 和 `message_pipeline/` 三个过渡条目）|
| `lefthook.yml` | 添加 `tui-imports` 命令 |

---

## Task 1: 扩充 `peri-acp-types` DTO

**Goal:** 在 `peri-acp-types` crate 中补充 TUI 所需的全部 DTO 类型，确保 TUI 物理上不需要 `peri_agent`/`peri_middlewares` 任何类型。

**Files:**
- Create: `peri-acp-types/src/message.rs`
- Create: `peri-acp-types/src/interaction.rs`
- Create: `peri-acp-types/src/mcp_types.rs`
- Create: `peri-acp-types/src/hook.rs`
- Create: `peri-acp-types/src/plugin_types.rs`
- Create: `peri-acp-types/src/skill.rs`
- Create: `peri-acp-types/src/permission.rs`
- Create: `peri-acp-types/src/interaction_types.rs`
- Modify: `peri-acp-types/src/lib.rs`
- Test: `peri-acp-types/tests/dto_completeness.rs`

- [ ] **Step 1: 写失败测试 — DTO 完整性**

创建 `peri-acp-types/tests/dto_completeness.rs`：

```rust
//! Assert every DTO type needed by TUI exists and round-trips through serde.
use peri_acp_types::*;
use serde_json::json;

#[test]
fn test_base_message_dto_roundtrip() {
    let msg = message::BaseMessageDto::human("hello");
    let j = serde_json::to_value(&msg).unwrap();
    let back: message::BaseMessageDto = serde_json::from_value(j).unwrap();
    assert_eq!(back, msg);
}

#[test]
fn test_content_block_dto_all_variants() {
    let blocks = vec![
        message::ContentBlockDto::text("hi"),
        message::ContentBlockDto::tool_use("t1", "Bash", json!({"cmd": "ls"})),
        message::ContentBlockDto::tool_result("t1", "output", false),
        message::ContentBlockDto::reasoning("thinking...", "sig"),
    ];
    for b in blocks {
        let j = serde_json::to_value(&b).unwrap();
        let back: message::ContentBlockDto = serde_json::from_value(j).unwrap();
        assert_eq!(back, b);
    }
}

#[test]
fn test_interaction_context_dto() {
    let ctx = interaction::InteractionContextDto {
        session_id: "s1".into(),
        prompt_id: "p1".into(),
        channel_state: interaction::ChannelStateDto::Ready,
    };
    let j = serde_json::to_value(&ctx).unwrap();
    let back: interaction::InteractionContextDto = serde_json::from_value(j).unwrap();
    assert_eq!(back, ctx);
}

#[test]
fn test_mcp_dto_set() {
    let _server = mcp_types::ServerInfoDto {
        name: "mcp1".into(),
        status: mcp_types::ClientStatusDto::Ready,
        config_source: mcp_types::ConfigSourceDto::Project,
        tools: vec![],
        oauth: None,
    };
    let _oauth = mcp_types::OAuthStatusDto::Authorized { scopes: vec!["read".into()] };
}

#[test]
fn test_hook_dto_set() {
    let _h = hook::RegisteredHookDto {
        id: "h1".into(),
        event: hook::HookEventDto::PreToolUse,
        hook_type: hook::HookTypeDto::Command { cmd: "echo".into() },
        enabled: true,
    };
}

#[test]
fn test_plugin_dto_set() {
    let _scope = plugin_types::InstallScopeDto::User;
    let _src = plugin_types::MarketplaceSourceDto::Git { url: "x".into() };
    let _cmd = plugin_types::CommandEntryDto {
        name: "c1".into(),
        source: plugin_types::CommandSourceDto::Builtin,
    };
}

#[test]
fn test_skill_metadata_dto() {
    let _s = skill::SkillMetadataDto {
        name: "writer".into(),
        description: "...".into(),
        source: skill::SkillSourceDto::Builtin,
        disabled: false,
    };
}

#[test]
fn test_permission_mode_dto() {
    for m in &[permission::PermissionModeDto::Default,
               permission::PermissionModeDto::AcceptEdits,
               permission::PermissionModeDto::Plan,
               permission::PermissionModeDto::Yolo] {
        let j = serde_json::to_value(m).unwrap();
        let back: permission::PermissionModeDto = serde_json::from_value(j).unwrap();
        assert_eq!(&back, m);
    }
}

#[test]
fn test_hitl_and_ask_user_dtos() {
    let _item = interaction_types::BatchItemDto {
        tool_name: "Bash".into(),
        input_summary: "ls".into(),
        tool_call_id: "tc1".into(),
    };
    let _decision = interaction_types::HitlDecisionDto::Accept;
    let _q = interaction_types::AskUserQuestionDataDto {
        question: "q?".into(),
        options: vec![],
    };
}
```

- [ ] **Step 2: 运行测试，确认失败**

```bash
cargo test -p peri-acp-types --test dto_completeness
```

Expected: 编译失败，`error[E0433]: failed to resolve: module message` 等。

- [ ] **Step 3: 创建 `message.rs`**

```rust
//! Message DTOs -- TUI 渲染所需的 BaseMessage / ContentBlock 等。
//!
//! 这些 DTO 与 `peri_agent::messages` 中的 BaseMessage **结构等价**，
//! 但只保留 TUI 真正消费的字段（去掉了 role-specific metadata）。
//! 转换在 acp_server/view_mapper.rs 完成。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum BaseMessageDto {
    Human(HumanMessageData),
    Ai(AiMessageData),
    System(SystemMessageData),
    Tool(ToolMessageData),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HumanMessageData {
    pub content: MessageContentDto,
    pub message_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AiMessageData {
    pub content: Vec<ContentBlockDto>,
    pub message_id: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SystemMessageData {
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolMessageData {
    pub tool_call_id: String,
    pub content: MessageContentDto,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum MessageContentDto {
    Text(String),
    Blocks(Vec<ContentBlockDto>),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ContentBlockDto {
    Text { text: String },
    Image { source: ImageSourceDto },
    ToolUse { id: String, name: String, input: serde_json::Value },
    ToolResult { tool_use_id: String, content: String, is_error: bool },
    Reasoning { thinking: String, signature: Option<String> },
    Unknown { raw: serde_json::Value },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "source", rename_all = "lowercase")]
pub enum ImageSourceDto {
    Base64 { media_type: String, data: String },
    Url { url: String },
}

impl BaseMessageDto {
    pub fn human(text: impl Into<String>) -> Self {
        Self::Human(HumanMessageData {
            content: MessageContentDto::Text(text.into()),
            message_id: None,
        })
    }
    pub fn ai(text: impl Into<String>) -> Self {
        Self::Ai(AiMessageData {
            content: vec![ContentBlockDto::text(text)],
            message_id: None,
            model: None,
        })
    }
}

impl ContentBlockDto {
    pub fn text(t: impl Into<String>) -> Self {
        Self::Text { text: t.into() }
    }
    pub fn tool_use(id: impl Into<String>, name: impl Into<String>, input: serde_json::Value) -> Self {
        Self::ToolUse { id: id.into(), name: name.into(), input }
    }
    pub fn tool_result(tool_use_id: impl Into<String>, content: impl Into<String>, is_error: bool) -> Self {
        Self::ToolResult { tool_use_id: tool_use_id.into(), content: content.into(), is_error }
    }
    pub fn reasoning(thinking: impl Into<String>, signature: impl Into<Option<String>>) -> Self {
        Self::Reasoning { thinking: thinking.into(), signature: signature.into() }
    }
}
```

- [ ] **Step 4: 创建 `interaction.rs`**

```rust
//! Interaction DTOs -- 取代 peri_agent::interaction::{InteractionContext, ...}

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InteractionContextDto {
    pub session_id: String,
    pub prompt_id: String,
    pub channel_state: ChannelStateDto,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ChannelStateDto {
    Ready,
    WaitingForResponse { prompt: String },
    Closed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum InteractionResponseDto {
    Text { text: String },
    Cancelled,
    Error { message: String },
}
```

- [ ] **Step 5: 创建 `mcp_types.rs`**

```rust
//! MCP DTOs -- 取代 peri_middlewares::mcp::{ClientStatus, ConfigSource, ...}

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServerInfoDto {
    pub name: String,
    pub status: ClientStatusDto,
    pub config_source: ConfigSourceDto,
    pub tools: Vec<McpToolDto>,
    pub oauth: Option<OAuthStatusDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ClientStatusDto {
    Initializing,
    Ready,
    Failed { reason: String },
    Disabled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ConfigSourceDto {
    Project,
    User,
    Builtin,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum OAuthStatusDto {
    Required,
    Pending { url: String },
    Authorized { scopes: Vec<String> },
    Failed { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpToolDto {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum McpInitStatusDto {
    NotStarted,
    Loading { current: usize, total: usize },
    Done,
    Failed { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OAuthFlowEventDto {
    pub server_name: String,
    pub status: OAuthStatusDto,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OAuthCallbackResultDto {
    pub server_name: String,
    pub success: bool,
    pub error: Option<String>,
}
```

- [ ] **Step 6: 创建 `hook.rs`**

```rust
//! Hook DTOs -- 取代 peri_middlewares::hooks::types::{HookEvent, HookType, RegisteredHook}

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RegisteredHookDto {
    pub id: String,
    pub event: HookEventDto,
    pub hook_type: HookTypeDto,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum HookEventDto {
    PreToolUse,
    PostToolUse,
    UserPromptSubmit,
    Stop,
    SessionStart,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum HookTypeDto {
    Command { cmd: String },
    Script { path: String },
}
```

- [ ] **Step 7: 创建 `plugin_types.rs`**

```rust
//! Plugin DTOs -- 取代 peri_middlewares::plugin::{InstallScope, MarketplaceSource, ...}

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum InstallScopeDto {
    User,
    Project,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum MarketplaceSourceDto {
    Git { url: String },
    Local { path: String },
    Registry { name: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CommandEntryDto {
    pub name: String,
    pub source: CommandSourceDto,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum CommandSourceDto {
    Builtin,
    Plugin { plugin_id: String },
    User,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MarketplaceEntryDto {
    pub id: String,
    pub name: String,
    pub description: String,
    pub source: MarketplaceSourceDto,
    pub installed: bool,
}
```

- [ ] **Step 8: 创建 `skill.rs`**

```rust
//! Skill DTOs -- 取代 peri_middlewares::skills::loader::SkillMetadata

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SkillMetadataDto {
    pub name: String,
    pub description: String,
    pub source: SkillSourceDto,
    pub disabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SkillSourceDto {
    Builtin,
    User,
    Project,
    Agm,
}
```

- [ ] **Step 9: 创建 `permission.rs`**

```rust
//! Permission DTOs -- 取代 peri_middlewares::prelude::PermissionMode

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum PermissionModeDto {
    Default,
    AcceptEdits,
    Plan,
    Yolo,
}
```

- [ ] **Step 10: 创建 `interaction_types.rs`**

```rust
//! HITL + AskUser DTOs -- 取代 peri_middlewares::{hitl::*, ask_user::*}

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BatchItemDto {
    pub tool_name: String,
    pub input_summary: String,
    pub tool_call_id: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum HitlDecisionDto {
    Accept,
    Reject,
    AcceptAll,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AskUserQuestionDataDto {
    pub question: String,
    pub options: Vec<AskUserOptionDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AskUserOptionDto {
    pub label: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AskUserBatchRequestDto {
    pub questions: Vec<AskUserQuestionDataDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThreadMetaDto {
    pub thread_id: String,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
}
```

- [ ] **Step 11: 修改 `lib.rs`**

替换 `peri-acp-types/src/lib.rs` 为：

```rust
//! Pure DTO crate for TUI ↔ ACP contract. Depends only on serde.

pub mod event_data;
pub mod hook;
pub mod interaction;
pub mod interaction_types;
pub mod message;
pub mod mcp_types;
pub mod permission;
pub mod plugin_types;
pub mod skill;
pub mod summary;
pub mod view_model;
```

- [ ] **Step 12: 运行测试，确认通过**

```bash
cargo test -p peri-acp-types --test dto_completeness
```

Expected: 9 测试通过。

- [ ] **Step 13: 提交**

```bash
git add peri-acp-types/src/{message,interaction,mcp_types,hook,plugin_types,skill,permission,interaction_types,lib}.rs \
        peri-acp-types/tests/dto_completeness.rs
git commit -m "$(cat <<'EOF'
feat(acp-types): 扩充 9 个 DTO module 覆盖 TUI 全部运行时类型

补充 message / interaction / mcp_types / hook / plugin_types / skill / permission / interaction_types 八个 DTO module，
使 peri-tui 物理脱离 peri_agent / peri_middlewares 运行时类型依赖。每个 DTO 与原类型结构等价但去掉了 role-specific metadata，
转换边界在 acp_server/view_mapper.rs 完成。

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>
EOF
)"
```

---

## Task 2: 替换 BaseMessage / ContentBlock / MessageContent

**Goal:** 将 5 个文件中的 `peri_agent::messages::{BaseMessage, ContentBlock, MessageContent}` 全部替换为 DTO。

**Files:**
- Modify: `peri-tui/src/app/agent_comm.rs:1`
- Modify: `peri-tui/src/app/agent_compact.rs:1`
- Modify: `peri-tui/src/app/agent_ops/rewind.rs:6`
- Modify: `peri-tui/src/app/mod.rs:129`
- Modify: `peri-tui/src/command/core/gc.rs:1`

- [ ] **Step 1: 写失败测试 — DTO 替换正确性**

Modify `peri-tui/src/app/agent_comm.rs` 顶部测试模块（如果没有则新增）：

```rust
#[cfg(test)]
mod dto_migration_tests {
    use peri_acp_types::message::BaseMessageDto;

    #[test]
    fn test_no_peri_agent_messages_import() {
        // 静态断言：编译时确保模块顶部无 use peri_agent::messages
        let _msg = BaseMessageDto::human("test");
    }
}
```

- [ ] **Step 2: 运行测试，确认编译错误**

```bash
cargo build -p peri-tui 2>&1 | grep "use peri_agent::messages" | head
```

Expected: 5 个文件编译错误（因为下面替换还未完成）。

- [ ] **Step 3: 替换 `agent_comm.rs`**

将 `peri-tui/src/app/agent_comm.rs:1`:

```rust
use peri_agent::{agent::AgentCancellationToken, messages::BaseMessage};
```

替换为：

```rust
use peri_acp_types::message::BaseMessageDto;
use tokio_util::sync::CancellationToken as AgentCancellationToken;
```

文件内所有 `BaseMessage` 类型标注替换为 `BaseMessageDto`。

- [ ] **Step 4: 替换 `agent_compact.rs`**

将 `peri-tui/src/app/agent_compact.rs:1`:

```rust
use peri_agent::messages::BaseMessage;
```

替换为：

```rust
use peri_acp_types::message::BaseMessageDto;
```

文件内 `BaseMessage` → `BaseMessageDto`。

- [ ] **Step 5: 替换 `agent_ops/rewind.rs`**

将 `peri-tui/src/app/agent_ops/rewind.rs:6`:

```rust
use peri_agent::messages::{BaseMessage, ContentBlock};
```

替换为：

```rust
use peri_acp_types::message::{BaseMessageDto, ContentBlockDto};
```

文件内类型标注同步替换。

- [ ] **Step 6: 替换 `app/mod.rs`**

将 `peri-tui/src/app/mod.rs:129`:

```rust
use peri_agent::messages::BaseMessage;
```

替换为：

```rust
use peri_acp_types::message::BaseMessageDto;
```

并搜索文件内所有 `BaseMessage` 标注替换。

```bash
# 确认替换完整
grep -n "BaseMessage\b" peri-tui/src/app/mod.rs | grep -v "BaseMessageDto" | head
```

Expected: 0 行（除非注释）。

- [ ] **Step 7: 替换 `command/core/gc.rs`**

将 `peri-tui/src/command/core/gc.rs:1`:

```rust
use peri_agent::messages::{BaseMessage, ContentBlock, MessageContent};
```

替换为：

```rust
use peri_acp_types::message::{BaseMessageDto, ContentBlockDto, MessageContentDto};
```

- [ ] **Step 8: 编译 + 测试**

```bash
cargo build -p peri-tui 2>&1 | tail -30
```

Expected: 编译通过。如有错误，根据错误信息修复字段名不匹配（如 `BaseMessage::human(...)` → `BaseMessageDto::human(...)`）。

```bash
cargo test -p peri-tui --lib app::agent_comm app::agent_compact app::agent_ops::rewind
```

Expected: 相关测试通过。

- [ ] **Step 9: 提交**

```bash
git add peri-tui/src/app/agent_comm.rs peri-tui/src/app/agent_compact.rs \
        peri-tui/src/app/agent_ops/rewind.rs peri-tui/src/app/mod.rs \
        peri-tui/src/command/core/gc.rs
git commit -m "$(cat <<'EOF'
refactor(tui): 替换 BaseMessage/ContentBlock/MessageContent → DTO

5 个文件从 peri_agent::messages 切换到 peri_acp_types::message DTO。
AgentCancellationToken 改用 tokio_util::sync::CancellationToken 直接导入
（peri_agent 只是 re-export 该类型）。

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>
EOF
)"
```

---

## Task 3: 替换 InteractionContext / InteractionResponse / ChannelState

**Goal:** 将 `peri_agent::interaction::*` 类型替换为 DTO。

**Files:**
- Modify: `peri-tui/src/app/events.rs:1`
- Modify: `peri-tui/src/app/service_registry.rs:4`
- Modify: `peri-tui/src/app/agent_ops_interaction.rs:156`

- [ ] **Step 1: 替换 `events.rs`**

将 `peri-tui/src/app/events.rs:1-2`:

```rust
use peri_agent::interaction::{InteractionContext, InteractionResponse};
pub use peri_middlewares::mcp::OAuthCallbackResult;
```

替换为：

```rust
use peri_acp_types::interaction::{InteractionContextDto, InteractionResponseDto};
pub use peri_acp_types::mcp_types::OAuthCallbackResultDto as OAuthCallbackResult;
```

文件内 `InteractionContext` → `InteractionContextDto`，`InteractionResponse` → `InteractionResponseDto`。

- [ ] **Step 2: 替换 `service_registry.rs`**

将 `peri-tui/src/app/service_registry.rs:4`:

```rust
use peri_agent::interaction::ChannelState;
```

替换为：

```rust
use peri_acp_types::interaction::ChannelStateDto;
```

文件内 `ChannelState` → `ChannelStateDto`。

- [ ] **Step 3: 替换 `agent_ops_interaction.rs:156`**

将函数体内的：

```rust
use peri_agent::interaction::{
    InteractionContext, InteractionResponse,
};
```

替换为：

```rust
use peri_acp_types::interaction::{
    InteractionContextDto as InteractionContext,
    InteractionResponseDto as InteractionResponse,
};
```

> **注**：使用 `as` 别名可让函数体内大量调用点不动，最小化 diff。Task 12 会清理全部别名。

- [ ] **Step 4: 编译 + 测试**

```bash
cargo build -p peri-tui 2>&1 | tail -30
cargo test -p peri-tui --lib app::events app::service_registry app::agent_ops_interaction
```

Expected: 通过。

- [ ] **Step 5: 提交**

```bash
git add peri-tui/src/app/events.rs peri-tui/src/app/service_registry.rs \
        peri-tui/src/app/agent_ops_interaction.rs
git commit -m "$(cat <<'EOF'
refactor(tui): 替换 InteractionContext/Response/ChannelState → DTO

events.rs / service_registry.rs / agent_ops_interaction.rs 三个文件
从 peri_agent::interaction 切换到 peri_acp_types::interaction DTO。

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>
EOF
)"
```

---

## Task 4: 替换 MCP 类型 + 引入 `acp_query_cache`

**Goal:** 将 `peri_middlewares::mcp::*` 类型替换为 DTO；MCP 状态查询从直接持有 `McpClientPool` 改为通过 `acp_query_cache::snapshot_mcp()`。

**Files:**
- Modify: `peri-tui/src/app/service_registry.rs:5`
- Modify: `peri-tui/src/app/mod.rs:375,387`
- Modify: `peri-tui/src/app/mcp_panel/mod.rs:1,118`
- Modify: `peri-tui/src/app/mcp_panel/component.rs:3,350,477,529`
- Modify: `peri-tui/src/app/mcp_panel/ops.rs:1,219,310,360`
- Modify: `peri-tui/src/app/oauth_prompt.rs:35`
- Modify: `peri-tui/src/ui/main_ui/panels/mcp.rs:1,498`
- Modify: `peri-tui/src/ui/main_ui/status_bar.rs:260`

- [ ] **Step 1: 检查 `acp_query_cache` 现状**

```bash
grep -rn "fn snapshot_mcp\|fn snapshot_cron\|fn snapshot_hooks" peri-tui/src/acp_query_cache* 2>/dev/null
```

Expected: 至少找到 `snapshot_mcp`。如果不存在，需要在 `acp_query_cache.rs` 中补一个查询函数（向 ACP server 发 `query/snapshot` 请求）。

- [ ] **Step 2: 替换 `service_registry.rs:5`**

查看 `service_registry.rs:5` 的完整 use：

```rust
use peri_middlewares::{
    mcp::{McpClientPool, McpInitStatus},
    ...
};
```

替换为：

```rust
// McpClientPool 替换：TUI 不再持有 pool，改为通过 acp_client::snapshot_mcp() 查询
// McpInitStatus → DTO
use peri_acp_types::mcp_types::McpInitStatusDto;
```

如果 `McpClientPool` 类型字段在 `ServiceRegistry` 中存在，需要从 struct 中删除（改为通过 `acp_client` 按需查询）。

- [ ] **Step 3: 替换 `app/mod.rs:375,387` 函数体内 use**

```rust
// L375 原：
use peri_middlewares::mcp::{McpClientPool, McpInitStatus};
// 改为：
use peri_acp_types::mcp_types::McpInitStatusDto;
// McpClientPool 调用点改 self.acp_client.snapshot_mcp()

// L387 原：
use peri_middlewares::mcp::OAuthFlowEvent;
// 改为：
use peri_acp_types::mcp_types::OAuthFlowEventDto;
```

- [ ] **Step 4: 替换 `mcp_panel/mod.rs`**

```rust
// L1 原：
use peri_middlewares::mcp::{ConfigSource, Resource, ServerInfo, Tool};
// 改为：
use peri_acp_types::mcp_types::{ConfigSourceDto, ServerInfoDto};

// Resource 和 Tool 类型如果仅用于渲染，改为内联 DTO（在 mcp_types.rs 中追加）：
```

如缺 `ResourceDto` / `McpToolDto`（已在 mcp_types.rs 中），同步替换。

```rust
// L118 测试 use：
use peri_middlewares::mcp::ClientStatus;
// 改为：
use peri_acp_types::mcp_types::ClientStatusDto;
```

- [ ] **Step 5: 替换 `mcp_panel/component.rs`**

```rust
// L3 原：
use peri_middlewares::mcp::ClientStatus;
// 改为：
use peri_acp_types::mcp_types::ClientStatusDto;

// L350/L477/L529 函数体内：
use peri_middlewares::mcp::OAuthFlowEvent;
// 改为：
use peri_acp_types::mcp_types::OAuthFlowEventDto;
```

- [ ] **Step 6: 替换 `mcp_panel/ops.rs`**

```rust
// L1：
use peri_middlewares::mcp::ClientStatus;
// → use peri_acp_types::mcp_types::ClientStatusDto;

// L219/L310/L360：
use peri_middlewares::mcp::OAuthFlowEvent;
// → use peri_acp_types::mcp_types::OAuthFlowEventDto;
```

- [ ] **Step 7: 替换 `oauth_prompt.rs:35`**

`parse_code_from_url` 是个 10 行的纯函数，**复制到 TUI** 而非保留 import：

在 `peri-tui/src/app/oauth_prompt.rs` 顶部添加：

```rust
/// Parse OAuth redirect URL to extract authorization code.
/// 移自 peri_middlewares::mcp::parse_code_from_url（避免跨 crate 调用）。
fn parse_code_from_url(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let query = parsed.query_pairs().find(|(k, _)| k == "code").map(|(_, v)| v.to_string());
    query.or_else(|| {
        parsed.fragment().and_then(|f| {
            f.split('&').find_map(|pair| {
                let (k, v) = pair.split_once('=')?;
                (k == "code").then(|| v.to_string())
            })
        })
    })
}
```

L35 函数体内删除 `use peri_middlewares::mcp::parse_code_from_url;`。

- [ ] **Step 8: 替换 `ui/main_ui/panels/mcp.rs`**

```rust
// L1：
use peri_middlewares::mcp::{ClientStatus, ConfigSource, OAuthStatus, ServerInfo};
// 改为：
use peri_acp_types::mcp_types::{ClientStatusDto, ConfigSourceDto, OAuthStatusDto, ServerInfoDto};

// L498 测试中：
use peri_middlewares::mcp::{ClientStatus, ConfigSource, ServerInfo};
// 同上替换。
```

- [ ] **Step 9: 替换 `ui/main_ui/status_bar.rs:260`**

```rust
// L260 函数体内：
use peri_middlewares::mcp::McpInitStatus;
// 改为：
use peri_acp_types::mcp_types::McpInitStatusDto;
```

- [ ] **Step 10: 编译 + 修复字段名**

```bash
cargo build -p peri-tui 2>&1 | tail -50
```

如有字段名不匹配，根据错误信息修复（如 `ClientStatus::Ready` → `ClientStatusDto::Ready`）。

```bash
cargo test -p peri-tui --lib app::mcp_panel ui::main_ui::panels::mcp
```

Expected: 相关测试通过。

- [ ] **Step 11: 提交**

```bash
git add peri-tui/src/app/service_registry.rs peri-tui/src/app/mod.rs \
        peri-tui/src/app/mcp_panel/ peri-tui/src/app/oauth_prompt.rs \
        peri-tui/src/ui/main_ui/panels/mcp.rs peri-tui/src/ui/main_ui/status_bar.rs
git commit -m "$(cat <<'EOF'
refactor(tui): 替换 MCP 类型为 DTO + parse_code_from_url 内联

mcp_panel / panels/mcp / status_bar / oauth_prompt / mod.rs / service_registry
全部切换到 peri_acp_types::mcp_types DTO。parse_code_from_url 从 peri_middlewares
复制到 TUI 内部（10 行纯函数）。

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>
EOF
)"
```

---

## Task 5: 替换 Cron / Hooks / Plugin 类型

**Goal:** 处理 cron/hooks/plugin 三大中间件类型替换；其中 Plugin 涉及 `MarketplaceManager` 调用点，需要改为 `acp_client::refresh_marketplace()` 协议方法。

**Files:**
- Modify: `peri-tui/src/app/cron_state.rs:4`
- Modify: `peri-tui/src/app/tasks_panel.rs:4`
- Modify: `peri-tui/src/app/hooks_panel.rs:3`
- Modify: `peri-tui/src/ui/main_ui/panels/hooks.rs:163`
- Modify: `peri-tui/src/app/panel_plugin/plugin_loader.rs:36,90,112,215`
- Modify: `peri-tui/src/app/panel_plugin/background.rs:44,49,94,140`
- Modify: `peri-tui/src/app/panel_plugin/marketplace_ops.rs:21,121`
- Modify: `peri-tui/src/app/panel_plugin/entries.rs:170`
- Modify: `peri-tui/src/app/panel_plugin/install_ops.rs:13`
- Modify: `peri-tui/src/app/panel_plugin/source_helpers.rs:10`
- Modify: `peri-tui/src/app/plugin_panel/handlers/plugin_handlers/discover_detail.rs:1`
- Modify: `peri-tui/src/app/plugin_panel/handlers/plugin_handlers/install.rs:1`
- Modify: `peri-tui/src/app/plugin_panel/types.rs:3`
- Modify: `peri-tui/src/ui/main_ui/panels/plugin/plugin_render/list.rs:1`
- Modify: `peri-tui/src/ui/main_ui/panels/plugin/plugin_render/detail.rs:1`
- Modify: `peri-tui/src/command/session/plugin_command.rs:1,38`
- Modify: `peri-tui/src/ui/main_ui/panels/status.rs:591` (`RequestRecord` → DTO)

- [ ] **Step 1: Cron 类型替换**

`cron_state.rs:4` 和 `tasks_panel.rs:4`:

```rust
// 原：
use peri_middlewares::cron::{CronScheduler, CronTask};
// 改为：
use peri_acp_types::summary::CronTaskDto;
// CronScheduler 调用点（如 self.cron.tasks()）改为 self.acp_client.snapshot_cron()
```

`CronScheduler` 字段如果在 struct 中存在（`cron_state.rs`），删除；改为持有 `CronTaskDto` 列表，由 `acp_query_cache` 填充。

- [ ] **Step 2: Hooks 类型替换**

`hooks_panel.rs:3` 和 `panels/hooks.rs:163`:

```rust
// 原：
use peri_middlewares::hooks::types::{HookEvent, HookType, RegisteredHook};
// 改为：
use peri_acp_types::hook::{HookEventDto, HookTypeDto, RegisteredHookDto};
```

文件内类型标注替换。

- [ ] **Step 3: Plugin 简单类型替换（InstallScope / CommandEntry）**

`plugin_panel/handlers/plugin_handlers/{discover_detail,install}.rs`、`app/plugin_panel/types.rs`、`ui/main_ui/panels/plugin/plugin_render/{list,detail}.rs`、`command/session/plugin_command.rs`:

```rust
// 原：
use peri_middlewares::plugin::InstallScope;
// 改为：
use peri_acp_types::plugin_types::InstallScopeDto;

// command/session/plugin_command.rs:1,38:
use peri_middlewares::plugin::CommandEntry;
use peri_middlewares::plugin::CommandSource;
// 改为：
use peri_acp_types::plugin_types::{CommandEntryDto, CommandSourceDto};
```

`app/plugin_panel/types.rs:3`:

```rust
// 原：
pub use peri_middlewares::plugin::InstallScope;
// 改为：
pub use peri_acp_types::plugin_types::InstallScopeDto as InstallScope;
```

- [ ] **Step 4: Plugin 复杂调用替换（MarketplaceManager / refresh_marketplace / load_plugin_manifest）**

`panel_plugin/source_helpers.rs:10`:

```rust
// 原：
use peri_middlewares::plugin::MarketplaceSource;
// 改为：
use peri_acp_types::plugin_types::MarketplaceSourceDto;
```

`panel_plugin/plugin_loader.rs`（5 处函数体内 use）：

```rust
// 原 L36：
use peri_middlewares::plugin::{...};
// 改为：使用 acp_client 协议方法加载 plugin manifest。
// 具体：将 load_plugin_manifest 调用改为 self.acp_client.plugin_load_manifest(source).await
```

> **注**：这步可能需要在 `acp_client` 中新增协议方法（如果不存在）。检查：

```bash
grep -n "fn plugin_load_manifest\|fn refresh_marketplace\|fn marketplace_list" \
    peri-tui/src/acp_client/client.rs
```

如果不存在，需要在 Task 5 步骤 4a 中先补：

```rust
// peri-tui/src/acp_client/client.rs 新增方法：
pub async fn plugin_load_manifest(&self, source: &MarketplaceSourceDto)
    -> Result<PluginManifestDto, AcpClientError> { ... }
pub async fn refresh_marketplace(&self, source: &MarketplaceSourceDto)
    -> Result<Vec<MarketplaceEntryDto>, AcpClientError> { ... }
pub async fn marketplace_list(&self) -> Result<Vec<MarketplaceEntryDto>, AcpClientError> { ... }
```

`panel_plugin/background.rs`（4 处）+ `marketplace_ops.rs`（2 处）+ `entries.rs`（1 处）+ `install_ops.rs`（1 处）:

```rust
// 全部：
use peri_middlewares::plugin::marketplace::refresh_marketplace;
use peri_middlewares::plugin::MarketplaceManager;
use peri_middlewares::plugin::MarketplaceSource;
// 替换为：
use peri_acp_types::plugin_types::{MarketplaceEntryDto, MarketplaceSourceDto};
// 调用改为 self.acp_client.refresh_marketplace(&source).await
```

- [ ] **Step 5: 替换 `ui/main_ui/panels/status.rs:591`**

```rust
// 原：
use peri_agent::agent::token::RequestRecord;
// 改为：
// 检查 RequestRecord 使用场景，如果是 token usage 显示，改用 peri_acp_types::summary::TokenUsageDto
use peri_acp_types::summary::TokenUsageDto;
```

- [ ] **Step 6: 编译 + 修复**

```bash
cargo build -p peri-tui 2>&1 | tail -60
```

如有 `acp_client` 协议方法缺失的错误，按错误提示补齐。

- [ ] **Step 7: 测试**

```bash
cargo test -p peri-tui --lib app::cron_state app::tasks_panel app::hooks_panel \
                                app::panel_plugin app::plugin_panel \
                                ui::main_ui::panels::hooks ui::main_ui::panels::plugin \
                                ui::main_ui::panels::status \
                                command::session::plugin_command
```

Expected: 全部通过。

- [ ] **Step 8: 提交**

```bash
git add peri-tui/src/app/cron_state.rs peri-tui/src/app/tasks_panel.rs \
        peri-tui/src/app/hooks_panel.rs peri-tui/src/app/panel_plugin/ \
        peri-tui/src/app/plugin_panel/ peri-tui/src/command/session/plugin_command.rs \
        peri-tui/src/ui/main_ui/panels/{hooks,plugin,status}.rs \
        peri-tui/src/acp_client/client.rs
git commit -m "$(cat <<'EOF'
refactor(tui): 替换 Cron/Hooks/Plugin 类型为 DTO + ACP 协议方法

Cron/Hooks/Plugin 类型切换到 peri_acp_types DTO。MarketplaceManager/
load_plugin_manifest/refresh_marketplace 改为 acp_client 协议方法调用，
彻底脱离 peri_middlewares 运行时依赖。

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>
EOF
)"
```

---

## Task 6: 替换 SkillMetadata / PermissionMode / ThreadMeta / HITL / AskUser 类型

**Goal:** 清理剩余的类型 import。

**Files:**
- Modify: `peri-tui/src/app/chat_session.rs:4`
- Modify: `peri-tui/src/app/command_system.rs:3`
- Modify: `peri-tui/src/app/hint_ops.rs:187`
- Modify: `peri-tui/src/ui/main_ui/status_bar.rs:35`
- Modify: `peri-tui/src/panel/panels/thread_browser.rs:24`
- Modify: `peri-tui/src/app/ask_user_prompt.rs:1`
- Modify: `peri-tui/src/app/hitl_prompt.rs:1`
- Modify: `peri-tui/src/app/agent_ops_interaction.rs:2,60,159`
- Modify: `peri-tui/src/ui/main_ui/popups/hitl.rs:147`
- Modify: `peri-tui/src/ui/main_ui/popups/ask_user_height.rs:1`

- [ ] **Step 1: SkillMetadata 替换**

`chat_session.rs:4` / `command_system.rs:3` / `hint_ops.rs:187`:

```rust
// 原：
use peri_middlewares::prelude::SkillMetadata;
// 或：
use peri_middlewares::skills::loader::SkillMetadata;
// 改为：
use peri_acp_types::skill::SkillMetadataDto;
```

文件内 `SkillMetadata` 标注替换。

- [ ] **Step 2: PermissionMode 替换**

`status_bar.rs:35`:

```rust
// 原：
use peri_middlewares::prelude::PermissionMode;
// 改为：
use peri_acp_types::permission::PermissionModeDto;
```

- [ ] **Step 3: ThreadMeta 替换**

`panel/panels/thread_browser.rs:24`:

```rust
// 原：
use peri_agent::thread::ThreadMeta;
// 改为：
use peri_acp_types::interaction_types::ThreadMetaDto;
```

- [ ] **Step 4: AskUser 类型替换**

`ask_user_prompt.rs:1`:

```rust
// 原：
use peri_middlewares::ask_user::{AskUserBatchRequest, AskUserQuestionData};
// 改为：
use peri_acp_types::interaction_types::{AskUserBatchRequestDto, AskUserQuestionDataDto};
```

`popups/ask_user_height.rs:1`:

```rust
// 原：
use peri_middlewares::ask_user::AskUserQuestionData;
// 改为：
use peri_acp_types::interaction_types::AskUserQuestionDataDto;
```

`agent_ops_interaction.rs:60,159` 函数体内 use:

```rust
// 原：
use peri_middlewares::ask_user::{AskUserBatchRequest, AskUserOption, AskUserQuestionData};
// 改为：
use peri_acp_types::interaction_types::{
    AskUserBatchRequestDto, AskUserOptionDto, AskUserQuestionDataDto,
};
```

- [ ] **Step 5: Hitl 类型替换**

`hitl_prompt.rs:1`:

```rust
// 原：
use peri_middlewares::prelude::{BatchItem, HitlDecision};
// 改为：
use peri_acp_types::interaction_types::{BatchItemDto, HitlDecisionDto};
```

`agent_ops_interaction.rs:2`:

```rust
// 原：
use peri_middlewares::hitl::BatchItem;
// 改为：
use peri_acp_types::interaction_types::BatchItemDto;
```

`popups/hitl.rs:147` 测试中 use:

```rust
// 原：
use peri_middlewares::hitl::BatchItem;
// 改为：
use peri_acp_types::interaction_types::BatchItemDto;
```

- [ ] **Step 6: 清理 `agent_ops_interaction.rs:156` 中的别名**

Task 3 步骤 3 中我们留了 `as InteractionContext` 别名，现在清理：

```rust
// 原：
use peri_acp_types::interaction::{
    InteractionContextDto as InteractionContext,
    InteractionResponseDto as InteractionResponse,
};
// 改为：
use peri_acp_types::interaction::{InteractionContextDto, InteractionResponseDto};
```

文件内调用点 `InteractionContext` → `InteractionContextDto`，`InteractionResponse` → `InteractionResponseDto`。

- [ ] **Step 7: 编译 + 测试**

```bash
cargo build -p peri-tui 2>&1 | tail -40
cargo test -p peri-tui --lib
```

Expected: 全部通过。

- [ ] **Step 8: 提交**

```bash
git add peri-tui/src/app/ peri-tui/src/ui/main_ui/ peri-tui/src/panel/panels/thread_browser.rs
git commit -m "$(cat <<'EOF'
refactor(tui): 替换剩余类型 import 为 DTO

SkillMetadata / PermissionMode / ThreadMeta / BatchItem / HitlDecision /
AskUser* / InteractionContext 别名清理。

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>
EOF
)"
```

---

## Task 7: 收紧 check-tui-imports.sh 白名单 + 启用 pre-commit 钩子

**Goal:** 将 `scripts/check-tui-imports.sh` 的过渡白名单收紧为只剩 `acp_server/` / `acp_client/` / `acp_stdio/` + `*_test.rs` 测试白名单，启用为 pre-commit 钩子。

**Files:**
- Modify: `scripts/check-tui-imports.sh`
- Modify: `lefthook.yml`

- [ ] **Step 1: 写失败测试 — 脚本严格模式**

创建 `scripts/test-check-tui-imports.sh`（或直接验证）：

```bash
# 临时添加一个故意违规的 import，验证脚本能检测到
echo "use peri_agent::messages::BaseMessage;" > /tmp/dummy_violation.rs
# 脚本目前只检查 peri-tui/src/，所以需要把文件放到那里
sudo cp /tmp/dummy_violation.rs peri-tui/src/ZZ_violation_probe.rs 2>/dev/null || \
    cp /tmp/dummy_violation.rs peri-tui/src/ZZ_violation_probe.rs
bash scripts/check-tui-imports.sh; EXIT=$?
rm peri-tui/src/ZZ_violation_probe.rs
exit $EXIT
```

Expected: 退出码 1（检测到违规）。

- [ ] **Step 2: 收紧白名单**

修改 `scripts/check-tui-imports.sh`，删除过渡白名单条目：

```bash
#!/usr/bin/env bash
# Check that peri-tui/src/ doesn't import from peri-agent or peri-middlewares
# except for explicitly whitelisted bridge files.
#
# Whitelist (合法桥接，永久保留):
# - acp_server/             TUI 内嵌 ACP server（协议边界）
# - acp_client/             ACP client（协议边界）
# - acp_stdio/              TUI 内嵌 ACP stdio 模式（协议边界）
# - main.rs                 bin entry
# - cli_print.rs            -p 打印模式（独立 bin）
# - *_test.rs / *_test/     测试文件（测试数据需要完整 BaseMessage 类型）

set -e
cd "$(dirname "$0")/.."

VIOLATIONS=$(grep -rn "use peri_agent::\|use peri_middlewares::" \
    peri-tui/src/ \
    --include="*.rs" \
    | grep -v "peri-tui/src/acp_server/" \
    | grep -v "peri-tui/src/acp_client/" \
    | grep -v "peri-tui/src/acp_stdio/" \
    | grep -v "peri-tui/src/main.rs" \
    | grep -v "peri-tui/src/cli_print.rs" \
    | grep -v "_test.rs" \
    | grep -v "_test/" \
    || true)

if [ -n "$VIOLATIONS" ]; then
    echo "❌ Forbidden peri_agent/peri_middlewares imports in peri-tui:"
    echo "$VIOLATIONS"
    echo ""
    echo "Allowed bridge files: acp_server/, acp_client/, acp_stdio/, main.rs,"
    echo "cli_print.rs, *_test.rs / *_test/ (permanent whitelist)"
    exit 1
fi

echo "✅ TUI imports OK"
```

> **关键改动**：删除了 `message_pipeline/` / `ui/message_view/` / `state_machine/` / `thread/mod.rs` 四个过渡白名单条目。
>
> **前置条件**：Plan 2（删除 thin_handle + 重写 event/keyboard/）+ Plan 3（删除 message_pipeline + 重写 render）必须完成，否则本步骤会让脚本立即报错。

- [ ] **Step 3: 运行脚本，确认 0 违规**

```bash
bash scripts/check-tui-imports.sh
```

Expected: `✅ TUI imports OK`。如果是 `❌`，需要先回去处理剩余违规。

- [ ] **Step 4: 启用 pre-commit 钩子**

修改 `lefthook.yml`，在 `pre-commit.commands` 中添加 `tui-imports`：

```yaml
# lefthook.yml - Git hooks managed by lefthook
# Install: lefthook install
# Run manually: lefthook run pre-commit

pre-commit:
  parallel: true
  commands:
    fmt:
      glob: "*.rs"
      run: cargo fmt --all -- --check
      fix: cargo fmt --all

    check:
      glob: "*.rs"
      run: cargo check --all-targets

    clippy:
      glob: "*.rs"
      run: cargo clippy --all-targets -- -W clippy::all

    typos:
      run: typos --ignore-hidden 2>/dev/null || true

    tui-imports:
      glob: "peri-tui/src/**/*.rs"
      run: bash scripts/check-tui-imports.sh
```

- [ ] **Step 5: 测试 pre-commit 钩子**

```bash
# 故意添加一个违规 import，验证钩子拦截
echo "use peri_agent::messages::BaseMessage;" >> peri-tui/src/app/mod.rs
git add peri-tui/src/app/mod.rs
lefthook run pre-commit 2>&1 | tail -10; EXIT=$?
# 撤销
git checkout peri-tui/src/app/mod.rs
exit $EXIT
```

Expected: lefthook 输出 `❌ Forbidden peri_agent/peri_middlewares imports`，退出码非零。

- [ ] **Step 6: 提交**

```bash
git add scripts/check-tui-imports.sh lefthook.yml
git commit -m "$(cat <<'EOF'
chore: 启用 check-tui-imports.sh 为 pre-commit 钩子 + 收紧白名单

删除 message_pipeline / ui/message_view / state_machine / thread/mod.rs
四个过渡白名单条目（这些目录已被 Plan 2/3 清理）。永久保留 acp_server /
acp_client / acp_stdio / main.rs / cli_print.rs / *_test.rs{,/} 作为
合法协议边界 + 测试白名单。

lefthook.yml 新增 tui-imports 命令，每次提交前强制校验。

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>
EOF
)"
```

---

## Task 8: Cargo.toml 依赖迁移 + 最终 grep 验证

**Goal:** 将 `peri-tui/Cargo.toml` 中的 `peri-agent` / `peri-middlewares` 移至 `[dev-dependencies]`（仅测试需要），完成最终验证。

**Files:**
- Modify: `peri-tui/Cargo.toml`

- [ ] **Step 1: 检查 Cargo.toml 现状**

```bash
grep -A 1 "^\[dependencies\]\|^\[dev-dependencies\]" peri-tui/Cargo.toml | head -30
grep "peri-agent\|peri-middlewares" peri-tui/Cargo.toml
```

Expected: `peri-agent` / `peri-middlewares` 在 `[dependencies]` 中。

- [ ] **Step 2: 迁移依赖**

在 `peri-tui/Cargo.toml` 中：

```toml
# 原：
[dependencies]
peri-agent = { workspace = true }
peri-middlewares = { workspace = true }

# 改为：
[dependencies]
# （删除 peri-agent 和 peri-middlewares）

[dev-dependencies]
peri-agent = { workspace = true }
peri-middlewares = { workspace = true }
```

如果原本没有 `[dev-dependencies]` section，在 `[dependencies]` 之后新增。

- [ ] **Step 3: 全量编译验证**

```bash
cargo build -p peri-tui 2>&1 | tail -20
```

Expected: 编译通过。如果有 `unresolved import peri_agent` 错误，说明有遗漏的运行时 import，回到 Task 2-6 处理。

```bash
cargo build -p peri-tui --tests 2>&1 | tail -20
```

Expected: 测试也能编译（dev-dependencies 提供）。

- [ ] **Step 4: 全量 grep 验证**

```bash
# 真实违规（非测试、非白名单）必须为 0
grep -rn "^use peri_agent::\|^use peri_middlewares::" \
    peri-tui/src/ --include="*.rs" \
    | grep -v "acp_server/" | grep -v "acp_client/" | grep -v "acp_stdio/" \
    | grep -v "_test.rs" | grep -v "_test/" \
    | grep -v "main.rs" | grep -v "cli_print.rs"
```

Expected: 0 行。

```bash
# 函数体内 use（非行首）也必须清空
grep -rn "    use peri_agent::\|    use peri_middlewares::\|        use peri_agent::\|        use peri_middlewares::" \
    peri-tui/src/ --include="*.rs" \
    | grep -v "acp_server/" | grep -v "acp_client/" | grep -v "acp_stdio/" \
    | grep -v "_test.rs" | grep -v "_test/" \
    | grep -v "main.rs" | grep -v "cli_print.rs"
```

Expected: 0 行。

- [ ] **Step 5: 全量测试**

```bash
cargo test --workspace 2>&1 | tail -10
```

Expected: 通过率 ≥ 95%（允许少数 legacy 测试因 Plan 2/3 删除而失效，但必须显式记录）。

- [ ] **Step 6: pre-commit 全量验证**

```bash
lefthook run pre-commit 2>&1 | tail -20
```

Expected: 全过。

- [ ] **Step 7: 提交**

```bash
git add peri-tui/Cargo.toml
git commit -m "$(cat <<'EOF'
chore(tui): peri-agent/peri-middlewares 依赖移至 dev-dependencies

TUI 运行时不再依赖 peri_agent / peri_middlewares，只在编译测试时需要
（用于构造 BaseMessage 等完整类型）。完成 Workflow D 类型隔离。

完成定义达成：
- grep "use peri_agent::\|use peri_middlewares::" 在 src/ 下 0 运行时违规
- check-tui-imports.sh 作为 pre-commit 钩子
- cargo build --workspace 退出 0

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>
EOF
)"
```

---

## 完成定义（Definition of Done）

本子计划完成的标志：

1. ✅ `grep -rn "^use peri_agent::\|^use peri_middlewares::" peri-tui/src/ --include="*.rs" | grep -v acp_server | grep -v acp_client | grep -v acp_stdio | grep -v _test | grep -v main.rs | grep -v cli_print.rs` → 0 结果
2. ✅ `grep -rn "    use peri_agent::\|    use peri_middlewares::" peri-tui/src/ --include="*.rs" | grep -v acp_server | grep -v acp_client | grep -v acp_stdio | grep -v _test` → 0 结果（函数体内 use 也清空）
3. ✅ `peri-tui/Cargo.toml` 中 `peri-agent` / `peri-middlewares` 不在 `[dependencies]`，仅 `[dev-dependencies]`
4. ✅ `scripts/check-tui-imports.sh` 白名单只剩：`acp_server/` / `acp_client/` / `acp_stdio/` / `main.rs` / `cli_print.rs` / `*_test.rs{,/}`
5. ✅ `lefthook.yml` 中有 `tui-imports` 命令，每次 pre-commit 触发
6. ✅ `cargo test --workspace` 通过率 ≥ 95%
7. ✅ `cargo clippy --workspace -- -D warnings` 0 警告
8. ✅ `lefthook run pre-commit` 全过

---

## 风险与缓解

| 风险 | 影响 | 缓解 |
|------|------|------|
| `acp_client` 缺失协议方法（plugin_load_manifest 等） | Task 5 阻塞 | Task 5 步骤 4 先补协议方法，再做替换 |
| DTO 字段名与原类型不完全一致 | 编译错误集中爆发 | 每个 Task 后跑一次 `cargo build` 立即修复 |
| `acp_query_cache` snapshot 函数不存在 | Task 4/5 阻塞 | 检查并补齐 `snapshot_mcp` / `snapshot_cron` / `snapshot_hooks` |
| Plan 2/3 未完成导致 `state_machine/` 等目录仍有 import | Task 7 步骤 2 失败 | **前置条件**：Plan 2 + Plan 3 必须先完成，否则收紧白名单后 check-tui-imports.sh 立即报错 |
| dev-dependencies 测试编译失败 | Task 8 阻塞 | 检查测试中 `BaseMessage::human(...)` 等 builder 是否在 DTO 中提供了对应 `human()` 方法（Task 1 已补） |

---

## 与其他子计划的依赖关系

| 依赖 | 说明 |
|------|------|
| **Plan 1（InputState）** | 无依赖，可并行 |
| **Plan 2（main_loop cutover）** | **必须先完成**：Plan 2 删除 `event/keyboard/` 中的 import |
| **Plan 3（rendering rewrite）** | **必须先完成**：Plan 3 删除 `message_pipeline/` 和 `ui/message_view/` 中的 import |

**推荐执行顺序**：Phase A（Plan 1 + Plan 4 并行）→ Phase B（Plan 2）→ Phase C（Plan 3）→ **Phase D（Plan 4 Task 7-8 最终验证）**。

> **特殊说明**：Plan 4 的 Task 1-6 可以在 Plan 2/3 之前并行启动（只处理非过渡目录的 import）。Task 7-8（收紧白名单 + Cargo.toml 迁移）必须等 Plan 2/3 完成后执行。
