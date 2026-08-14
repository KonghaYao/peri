# MetaHarness 工具视图剔除机制——设计文档外决策记录

**状态**：Open
**优先级**：低（文档与决策记录，无行为变更）
**类型**：决策记录
**创建日期**：2026-08-14
**来源**：MetaHarness 实施质量审查（P1-2）+ 设计文档 `docs/design/meta-harness-design.md` §2.5

## 背景

MetaHarness 设计 §2.5 定义 middleware 关闭语义：关闭的 middleware 不进链、
工具不进入 LLM 视图。设计文档未定义"共享工具 registry 中残留工具"的处理
机制。实施期自研了 `MIDDLEWARE_TOOL_NAMES` 剔除面（见下），该决策超出设计
文档正文，按纪律不改设计文档，记录于此供文档下次修订时补入 §2.5。

## 决策内容

**机制**：`peri-agent/src/session/exec/stage_builder.rs` 的
`build_session_tool_view` 每 turn 构造本地工具视图时，从宿主级共享
registry（`shared_tools`）复制并剔除"`MIDDLEWARE_TOOL_NAMES`（
`peri-acp-types/src/meta_harness.rs`）命中且不在当前链 `collect_tools`
结果中的工具名"条目，再 merge 当前链工具。

- 常量：`peri-acp-types/src/meta_harness.rs:80` `MIDDLEWARE_TOOL_NAMES`
  （全部 middleware 静态工具名并集）。
- 剔除条件：`!清单命中 || live_names 含`——disabled 视图隔离 + enabled
  视图不受影响。
- 一致性锁定：`peri-middlewares/src/assembly_test.rs`
  `middleware_tool_names_match_static_tool_sets`（工具名漂移即剔除面失真）。

**维护约束**：新增 middleware 工具名必须同步
`MIDDLEWARE_TOOL_NAMES`（否则工具名漂移时测试失败）。

## 事实核查（2026-08-14，review P1-1 反驳依据）

review 曾质疑"MCP 动态工具（`mcp__{server}__{tool}`）不在剔除面，禁用
McpMiddleware 后跨 session 残留泄漏"。核查结论：**该场景在当前代码事实下
不成立**，剔除面为纯防御性机制：

1. `shared_tools`（宿主级共享 registry）全局唯一写入点是
   `peri-middlewares/src/assembly.rs` 装配期 `AskUserQuestion` insert——
   middleware 工具（含静态工具与 MCP 动态工具）**从不写入**共享表。
2. middleware 工具只经 `chain.collect_tools()` 进入 `build_session_tool_view`
   产出的**每 turn 重建本地视图**（stage_builder.rs），不写回共享表。
3. disabled session 当前链无 McpMiddleware → `collect_tools` 无 MCP 工具 →
   本地视图天然不含 → LLM 视图不可见；`tool_dispatch` 只从本 turn 视图
   读取执行（`peri-agent/src/agent/stages/tool_dispatch.rs`），`ExecuteExtraTool`
   从 shared_tools 读取但 MCP 工具不在其中 → 不可调用。

因此 MCP 动态工具无需也无法静态枚举进清单；`DiscoverMCP` /
`mcp_read_resource` 保留在清单中作静态部分（与测试锁定并集一致）。

**风险登记**：若将来注册面变化（middleware 工具开始写入 shared_tools），
须同步扩展清单或引入前缀规则（如 `mcp__`），并触发本决策复核。

## 相关修正

- `peri-acp-types/src/meta_harness.rs` 注释重写：去除不存在的 "Q8" 引用，
  陈述事实核查结论与维护约束。
- `peri-agent/src/session/exec/stage_builder.rs` 两处注释重写：剔除语义、
  MCP 工具无需剔除的原因、同名误伤语义（P2-3：非 middleware 来源同名
  工具在对应 middleware 关闭时被剔出本地视图——保守方向，`live_names`
  保护当前链同名工具）。

## 非目标

- 不改剔除逻辑本身（无 bug；机制为防御性）。
- 不改设计文档（按纪律；修订建议：下次修订时补入 §2.5 共享表残留防御面
  一段，含维护约束）。

## 涉及文件

- `peri-acp-types/src/meta_harness.rs` — 常量与注释（事实核查）。
- `peri-agent/src/session/exec/stage_builder.rs` — `build_session_tool_view`
  注释（剔除语义 + 误伤语义）。
- `peri-middlewares/src/assembly_test.rs` — 一致性测试注释（维护约束）。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-14 | — | Open | agent | review P1-2 决策记录；P1-1 经事实核查判定误报，证据入本文 |
