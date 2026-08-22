# 审批/提问职责拆分：PermissionMiddleware 与 HITL 归位（AskUserQuestion 纳入关闭面）

**状态**：Implemented（2026-08-15；文档同步 2026-08-22）
**优先级**：中
**类型**：架构变更
**创建日期**：2026-08-15
**来源**：用户访谈（2026-08-15）——"AskUserQuestion 在 meta harness 中关闭不掉"排查后提出的职责归位决策

## 背景

现状 `HumanInTheLoopMiddleware` 名不副实：其实际职责是**工具审批**（`before_tool` / `before_tools_batch` 钩子 + `default_requires_approval` + `PermissionMode` 决策 + `AutoClassifier`），名为"HITL"但 Bypass 模式 / disabled 时并无人在环。真正的"人在环"交互工具 `AskUserQuestion` 反而游离在 middleware 体系之外：作为宿主级 `shared_tools` 唯一条目（`assembly.rs:537-543` 无条件 insert）、不在 `MIDDLEWARE_TOOL_NAMES` 剔除面，导致 meta harness 关闭 `HumanInTheLoopMiddleware` 后该工具仍出现在 LLM 视图——"关闭不掉"。

设计文档 `docs/design/meta-harness-design.md` §2.5 / §3.4 将"AskUserQuestion 不在关闭面"记为**有意设计 + 未来项**（"若要纳入关闭面，需将其移入某个 middleware 的 collect_tools 或显式建模为可关闭能力"）。本 issue 落地该未来项。

## 决策内容（用户拍板 2026-08-15）

1. **改名**：现审批中间件改名 `PermissionMiddleware`；`HumanInTheLoopMiddleware` 旧名释放，由新"提问"中间件接管（含 `AskUserQuestion` 工具 + 新提问纪律段落）。配置键采取**纯破坏性改名**：新键 `PermissionMiddleware` 生效；旧键 `"HumanInTheLoopMiddleware"` 不再表示"关审批"——该键名由新 HITL 接管，旧配置（`"HumanInTheLoopMiddleware": false`）语义**静默漂移**为"关提问"，接受代价，发布说明/文档警告。
2. **关闭语义**：新 HITL 可独立关闭，关闭即从 LLM 工具视图消失（AskUserQuestion 随 middleware disabled 不再装配，`collect_tools` 无它 → `build_session_tool_view` 本地视图不含）。PermissionMiddleware（审批）与新 HITL（提问）各自独立开关。
3. **执行路径**：AskUserQuestion 彻底移入新 HITL 的 `collect_tools`，宿主级 `shared_tools` 不再注册任何工具（生产路径写入点归零，`MIDDLEWARE_TOOL_NAMES` 剔除面回归纯防御——事实核查结论更新）。`ExecuteExtraTool` / `ToolSearchMiddleware` 持每 turn 本地视图（`stage_builder.rs` 组件装配时传入），AskUserQuestion 为 `is_direct=true` 无需代理，执行路径自洽。
4. **段落归属**：`10_hitl`（审批机制 + sensitive 列表）归 `PermissionMiddleware`；新 HITL 新持提问纪律段落（新增段落 ID）。

## 涉及文件（初步影响面，实施时以编译为准）

- `peri-acp-types/src/meta_harness.rs` — `MIDDLEWARE_NAMES` / `SECTION_HOLDER_MIDDLEWARE` / `SECTION_IDS` / `MIDDLEWARE_TOOL_NAMES` 四常量 + 注释
- `peri-acp-types/src/interaction.rs` — doc 示例引用
- `peri-middlewares/src/hitl/`（→ `permission/`）+ 新建 `hitl/`（新 middleware）
- `peri-middlewares/src/assembly.rs` — 顶层链双槽位 / workflow 链 / 删 shared_tools insert / parent_tools
- `peri-middlewares/src/lib.rs`、`tests/canonical_tool_invocation_contract.rs`
- `peri-middlewares/src/assembly_test.rs`（`meta_harness_ask_user_tool_always_registered` **反转**）、`hitl/mod_test.rs`
- `peri-agent/src/session/factory.rs`（`ChainSlot::Hitl` → `Permission` + 新增 AskUser 槽位，ARC-MIDDLEWARE-001 契约面）、`stage_builder.rs`、`executor.rs` 注释
- `peri-acp/src/prompt/mod.rs`、`host/workflow_agent.rs`（10_hitl 过滤改指 PermissionMiddleware）、相关测试
- E2E `e2e/tests/scenarios/ask-user-question.test.ts` 检查（不涉及 middleware 名则不动）

## 实施要点

- **蓝本**：`production_blueprint` 第五组 `[Permission, AskUser, SubAgent]`；`assembly.rs` 槽位一一对应（ARC-MIDDLEWARE-001）。
- **workflow 链**：~~现状 workflow agent 可调 AskUserQuestion（经宿主级 shared_tools）；移出后为保持现状行为，workflow 链装配新 HITL（含 AskUserQuestion），`PermissionMiddleware::disabled()` 保持现状（workflow 无审批）~~ —— **实施修订（advisor 裁决 B 延伸，2026-08-15）**：workflow 生产路径 `broker: None`（`workflow_agent.rs` 装配面），新 HITL 仅 `if let Some(broker)` 时装配 → workflow 链**不装配**新 HITL，AskUserQuestion 随之从 workflow agent 消失。这是修复宿主级 shared_tools 泄漏的合理副作用（workflow 无 broker，提问无承载通道），与裁决 B"workflow 无 HITL"精神一致；`workflow_build_middlewares_filters_disabled` 测试锁定"不含 HumanInTheLoopMiddleware"。
- **新段落 ID**：`12_ask_user`；编号采用**顺插方案**（12=5，位于 11_subagent=4 之后）——13_skills 顺延 5→6、15_channel 6→7、language 7→8 一次性迁移（段内编号为渲染顺序事实，顺读 11→12→13 优先于避免重排；C1 D2 编号事实的有意识修订）；内容为提问纪律（何时提问/批量合并纪律，与 `AskUserTool` 的 `prompt_declaration` 同源）。
- **注释事实核查更新**：`meta_harness.rs` / `stage_builder.rs` 中"shared_tools 唯一写入点是 AskUserQuestion""当前永不命中"表述失效，已重写为"拆分后生产路径写入点归零"。

## 设计文档修订建议（下次修订时补入，按纪律不改正文）

- §2.5"AskUserQuestion 不在关闭面"条目删除/改写为"随新 HITL middleware 可关闭"。
- §3.4"为何不可关闭"段落改写为落地方案说明。
- 事实核查结论（2026-08-14 决策记录）中"唯一写入点"表述随本变更失效，`spec/issues/2026-08-14-meta-harness-tool-view-exclusion.md` 备注关联。

## 非目标

- 不改 `AskUserTool` 工具本体行为（broker、is_direct、namespace、批量纪律）。
- 不改 `UserInteractionBroker` 契约与 TUI 面板。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-15 | — | Open | agent | 用户访谈拍板 6 项决策后建档 |
| 2026-08-15 | Open | Implemented | agent | 代码 + 测试 + 验证全绿（见落地记录） |

## 落地记录（2026-08-15）

**代码变更**（全仓库，按受影响 crate）：

- `peri-acp-types`：`meta_harness.rs`——`MIDDLEWARE_NAMES` 新增 `PermissionMiddleware`（旧名保留、语义变为提问中间件）；`MIDDLEWARE_TOOL_NAMES` 新增 `AskUserQuestion`（剔除面回归纯防御）；`SECTION_IDS` 插入 `12_ask_user`；`SECTION_HOLDER_MIDDLEWARE` 增 `("10_hitl","PermissionMiddleware")` + `("12_ask_user","HumanInTheLoopMiddleware")`。`interaction.rs` doc 示例引用改 `PermissionMiddleware::with_shared_mode`。
- `peri-middlewares`：
  - `hitl/` 目录 git mv → `permission/`（审批中间件改名 `PermissionMiddleware`，内容不动：`with_shared_mode` / `AutoClassifier` / `10_hitl` 段落 / before_tool(s)_batch 钩子）；
  - 新建 `hitl/mod.rs`——新提问中间件 `HumanInTheLoopMiddleware`：仅持 broker + `AskUserQuestion`（`collect_tools`）+ `12_ask_user` 段落，无任何钩子；
  - `assembly.rs`——删 `shared_tools.insert("AskUserQuestion", ...)`（生产路径写入点归零）与 `AskUserTool` 直接构造；顶层链 Hitl 槽位 → `PermissionMiddleware` + 新增 AskUser 槽位（disabled 集合含 `HumanInTheLoopMiddleware` 时不装配）；workflow 链 `PermissionMiddleware::disabled()` 无条件 + 新 HITL 仅 broker 存在时装配；
  - `lib.rs` 导出更新；`skills/mod.rs` 13_skills 段内序号 5→6；`assembly_test.rs` 用例更新（`meta_harness_ask_user_tool_always_registered` 反转：关闭新 HITL 后工具消失）；`hooks/` 引用改 `permission::`；
  - `tests/canonical_tool_invocation_contract.rs` import 与 `with_shared_mode` 改指 `permission::`。
- `peri-agent`：`session/factory.rs`——`ChainSlot::Hitl` → `Permission` + 新增 `ChainSlot::AskUser`（production_blueprint 第五组 `[Permission, AskUser, SubAgent]`）；`stage_builder.rs` 注释事实核查更新（写入点归零）；`executor.rs` 10_hitl 持有者注释；`middleware/chain_test.rs` 测试语义更新。
- `peri-acp`：`prompt/mod.rs` `GATED_SECTIONS` 15_channel 序号 6→7；`session/mod.rs` `build_collected_sections` 增 `PermissionMiddleware::sections()` 分支（C2 对拍驱动）；`prompt/prompt_test.rs` / `host/executor_flow_test.rs` / `host/workflow_agent.rs`（10_hitl 过滤目标语义改 PermissionMiddleware，workflow 渲染过滤不变）注释与 parity 用例更新；`host/requests_test.rs` / `stdio/session/create_test.rs` `shared_mode` 引用路径更新。
- 新段落文件 `peri-acp/prompts/sections/12_ask_user.md`（16 行提问纪律）。

**验证**：4 个受影响 crate lib 测试全绿（peri-acp-types 139 / peri-middlewares 1350 / peri-agent 667 / peri-acp 411）+ `canonical_tool_invocation_contract` 通过；`cargo clippy --workspace --all-targets -- -D warnings` 0 错误。

**交付范围外（按用户拍板）**：`docs/` 与 `peri-cool/` 未更新（设计文档修订建议见上节，下次修订时补入）；`spec/global/domains/middlewares.md` 的 HITL 行与 `peri-middlewares/src/hitl/` 稳定入口引用保持旧语义（该文件整体已过时——仍引用已迁移的 `builder.rs`，待单独维护批次处理）。

**与本次变更无关的既有失败**（未处理，非本 issue 引入）：`concurrent_bg_agent_test::test_bg_event_pump_receives_all_completions` 与用户分支既有未提交 `event_sink.rs` 工作冲突；`agm` 某测试 stash 后偶发失败（环境相关）。
