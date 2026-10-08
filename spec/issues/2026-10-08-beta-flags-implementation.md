# Beta Flag 系统实施

- **状态**：**已实施**（本分支：注册表、`config.betas` 配置面、装配期消费注入、ConfigPanel 管理区块；`BetasPanel` 已退役）。逐项验收证据见下文「验收结果」，尚未验证的项与跟进项见文末——本文不声称这些缺口已闭合。
- **日期**：2026-10-08。
- **设计权威**：`docs/design/beta-flags.md`。目标行为以设计为准；实施与验收中的取舍若偏离设计，先更新设计再实施。
- **来源**：用户要求引入 flag 系统配合 settings 配置控制面，用于开启 beta 业务能力；首批能力为 `full-async-tools`（Bash 与 Agent 缺省后台），普通用户不配置时行为不变。

## 交付契约

实现设计定义的注册表、`config.betas` 配置面投影、装配期消费注入与 ConfigPanel 管理面，并完成首个 flag `full-async-tools`。

不引入：远程下发/灰度/AB、会话内热生效、环境变量覆盖、独立 ACP `SessionConfigOption`、BetasPanel 保留。未配置用户的 Bash/Agent 行为与现状一致。

## 实施拆分

### 1. 注册表（`peri-acp-types`）

- 新增注册表模块：条目字段 id（kebab-case）与 canonical description；`BETA_FLAGS` 清单、id 查找与常量引用（消费方不写字面量）。
- 首个条目 `full-async-tools`。
- 测试：id 唯一且 kebab-case、清单顺序稳定、查找命中/未命中。

### 2. 配置面（`peri-config`）

- `BetasConfig` 由空占位扩展为 bool 稀疏覆盖表，位置 `config.betas`。
- 解析接入：global 与 workspace 逐 key 覆盖（workspace 胜出，显式 false 可关闭 global true）；差异提取与 MetaHarness 同款。
- 校验：未知键解析后从内存剔除并 warn（`validate_meta_harness` 同款接入点）；非 bool 值使该来源解析失败。
- 投影：`BetaFlags` 与 `ConfigurationSnapshot::flags()`（`resources()` 同级，由合并后 settings 派生）；`ConfigurationField::Betas` 与 DOMAINS 声明（Global + Workspace）。
- 保存沿用 `ConfigSource::save`，不新增写路径。
- 测试：合并、未知键剔除、类型错误、投影默认 false、explain contributors、CAS 保存往返与冲突拒绝。

### 3. 消费注入（工具装配）

- Bash：装配输入新增有效缺省字段；执行路径缺省决策与工具 schema `default`/描述同步生效。
- Agent：subagent 工具装配参数接收同一缺省；`resume_thread_id` 调用与任务管理器缺失场景维持现状语义。
- 注入值来自会话装配时的快照投影，会话内冻结、SubAgent 共享，执行路径不重读配置。
- 测试：flag off 时行为与现状逐项一致；flag on 时未传参走后台、显式 `false` 走前台、schema 与执行缺省一致。

### 4. TUI 管理面（`peri-tui`）

- ConfigPanel 行模型扩展为基础行 + 注册表驱动 flag 区块（Toggle），滚动沿用既有能力。
- 切换写当前生效层 `config.betas[id]`，经 `save_effective` CAS；失败提示且不改内存视图。
- 区块标注「新会话生效」；描述优先 i18n key `beta-desc-<id>`，缺失回退 canonical 文本。
- 退役 BetasPanel：删除面板实现、`PanelKind::Betas` 注册（Ctrl+B 与 `/betas`）及相关 mock/i18n。
- 测试：行渲染与切换写入、保存失败路径、退役后无残留引用。

## 验收

- 用户可观察行为：未配置时 Bash/Agent 行为与现状一致；`config.betas.full-async-tools=true` 的新会话中，未显式传参的 Bash/Agent 走后台，显式 `false` 仍前台。
- 配置面：未知键被忽略且不影响其他配置；workspace 覆盖 global；保存经 CAS，冲突或校验失败不发布。
- 面板：切换持久化到生效层并提示新会话生效；当前会话行为不变。
- 生命周期：新会话使用新值，既有会话保持冻结值。
- 回归：既有 Bash/Agent 前后台测试、配置权威面测试不回归。
- 逐项按 `testing.md` 先验证用户可观察行为，再补回归与失败路径。

## 验收结果（实施后记录）

实现与修复提交见本分支 `git log`（主体、索引收尾、新会话投影修复、审查修复）。对应测试命令与结果：

| 验收项 | 证据（命令 → 结果） |
| --- | --- |
| 未配置行为不变 / flag 开启走后台 / 显式 false 前台 / schema 与执行缺省一致 | `cargo test -p peri-mcp-workspace --lib -- default_run_in_background`（5 passed）；`cargo test -p peri-middlewares --lib -- beta_default`（5 passed）；`cargo test -p peri-middlewares --lib -- workspace_dispatch_propagates_bash_default_run_in_background`（1 passed） |
| 配置面合并、未知键、类型错误、投影、explain、CAS | `cargo test -p peri-config --lib -- betas`（15 passed） |
| 新会话使用新值（配置面 → 冻结值 → builtin Bash / Agent 工具 schema） | `cargo test -p peri-acp --lib -- beta_flag`（4 passed：Bash 差分对照 + Agent 工具 schema + 恢复分支） |
| 既有会话保持冻结值 | 同上 `restored_session_keeps_frozen_beta_flag`；`cargo test -p peri-acp --lib -- frozen`（33 passed，blob 往返含 `beta_flags`） |
| TUI 区块渲染/切换/失败路径 | `cargo test -p peri-tui --lib -- kit::panels::config`（16 passed）；`-- kit::panel_registry`（28 passed） |
| 回归 | `peri-config` 149 passed；`peri-acp-types` 537 passed；`peri-mcp-workspace` 442 passed；`peri-middlewares -- subagent::tool` 120 passed；`peri-acp -- mcp_v4 / host::requests` 32 / 155 passed |

未验证项（不声称已闭合）：TUI 视觉布局未眼测；进程重启后的恢复链路未做真实重启实测
（覆盖为 blob 往返 + 恢复路径装配用例）；Agent 侧未做真实模型调用的端到端后台执行实测。

## 跟进项（跨仓/后续）

- `peri-cool` 文档站（submodule）仍列 `/betas` 与 `"betas": {}`：`features/cli-reference.mdx`、
  `reference/keyboard-shortcuts.mdx`、`reference/settings.mdx` 需随 `BetasPanel` 退役更新。
- `peri-middlewares --lib -- mcp::builtin_runtime_tests` 并行运行存在既有 HOME 重定向与
  machine-identity 发布的交互失败（串行运行通过），与本 flag 无关，属既有测试卫生缺口。

## 边界

与设计文档「边界外」一致；本 issue 不涉及数据库表结构变更。
