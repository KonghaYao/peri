# 从 peri-acp 提取 TUI 关注点泄漏

**状态**：Fixed
**优先级**：中
**类型**：架构改进
**创建日期**：2026-07-20
**来源**：`/tmp/architecture-review-peri-acp-20260720.html` 候选 #5（improve-codebase-architecture 审查 peri-acp）

## Problem Statement

`peri-acp`（服务层）中存在多处 TUI 渲染层关注点泄漏：

| 位置 | 内容 | 问题 |
|------|------|------|
| `provider/config.rs:AppConfig` | `theme`、`daily_color`、`daily_color_date`、`diff_enabled`、`streaming_mode`、`scroll_fps` | 6+ 字段严格属于 UI 渲染配置，却定义在服务层 |
| `provider/mod.rs:ThinkingConfig` | `next_effort()` / `prev_effort()` 方法 | UI 交互逻辑（轮播切换）放在数据类中 |
| `event/truncate.rs` | `truncate_text`、`summarize_input`、`summarize_output` | TUI 渲染格式化逻辑在服务层，且只在 TUI 使用 |

**影响**：
- 违反分层原则：服务层不应关心 theme / scroll_fps / 文本截断格式
- `AppConfig` 被 TUI 和服务层共享，但字段属于不同关注点——修改 TUI 配置需要动 `peri-acp`
- `truncate.rs` 中的格式化逻辑如果未来有非 TUI 前端，会成为耦合点
- `next_effort` / `prev_effort` 的循环边界（`LOW→MEDIUM→HIGH→LOW`）是 UI 交互设计，不属于领域逻辑

## 建议方案

### 配置拆分
```rust
// peri-acp 保留纯服务配置
pub struct PeriConfig {
    pub env: HashMap<String, String>,
    pub paths: PathConfig,
    pub provider: ProviderConfig,
    pub proxy: Option<ProxyConfig>,
    // ... 仅服务层需要的字段
}

// peri-tui 新增独立 UI 配置
pub struct TuiConfig {
    pub theme: String,
    pub daily_color: bool,
    pub daily_color_date: Option<NaiveDate>,
    pub diff_enabled: bool,
    pub streaming_mode: StreamingMode,
    pub scroll_fps: u32,
}
```

从同一个 settings.json 加载，但在两种配置中拆分为两个 section 或两个 struct。

### truncate.rs 迁移
将整个 `event/truncate.rs` 文件迁移到 `peri-tui` crate（TUI 渲染模块），`peri-acp` 不再持有截断逻辑。

### ThinkingConfig 清理
移除 `next_effort()` / `prev_effort()`，改为在 TUI 面板中调用纯函数 `cycle_effort(current: ThinkingEffort) -> ThinkingEffort`。

## 涉及文件

| 文件 | 操作 |
|------|------|
| `provider/config.rs` | 拆分 `AppConfig` 为服务配置 + TUI 配置 |
| `provider/store.rs` | 调整加载逻辑以支持双配置 struct |
| `provider/mod.rs` | 移除 `next_effort` / `prev_effort` |
| `event/truncate.rs` | 迁移到 `peri-tui` |
| `event/mod.rs` | 移除 `mod truncate` |
| `peri-tui/src/` | 新增 `tui_config.rs` 和 `truncate.rs` |

## 收益

- **locality**：TUI 配置修改只需改 `peri-tui`，不再触及 `peri-acp`
- **adaptability**：未来非 TUI 前端（如 HTTP API）不需要加载 theme/scroll_fps 等无关配置
- settings.json 的 schema 可独立进化，不耦合服务层结构

## 风险

- 配置拆分可能影响 settings.json 的兼容性（旧配置文件缺少 TUI section）
- `truncate.rs` 如果被 peri-acp 的其他模块（非 TUI）使用，需要确认后保留部分
- 需要同步修改 TUI 所有读取 `AppConfig` 字段的位置

## 修复记录

### 修复 #1（2026-07-20）
- **操作人**：agent
- **commit**：`6aa12127 refactor(acp): 提取 TUI 关注点到 peri-tui，消除 peri-acp 泄漏`
- **修复内容**：拆分 AppConfig，迁移 truncate.rs 到 peri-tui，移除 next_effort/prev_effort
