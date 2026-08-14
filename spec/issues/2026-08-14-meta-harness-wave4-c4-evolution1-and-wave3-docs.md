# MetaHarness 波 4 C4——演进 1（permission_mode_notice 删除）+ 波 3 文档落位实施决策记录

**状态**：Closed（C4 已实施）
**优先级**：高（波 4 演进批次收尾）
**类型**：决策记录
**创建日期**：2026-08-14
**来源**：`docs/design/meta-harness-design.md` §3.5 演进 1（含影响面与语义代价注释）；
批 C4 任务书；C3 决策记录遗留问题 2（10_hitl "controlled runtime notification" 句联动）；
wave3 交付物 `spec/issues/2026-08-14-meta-harness-wave3-examples-and-safety.md`

## 背景

C1-C3 已落地段落持有基础设施、基础段/gated 段迁移与 gate 原子迁移。
本批执行演进 1：整体删除 Executor 直接注入消息流的 permission_mode 运行时
通知（`permission_mode_notice_if_changed` 及其全部配套），使"完全纯净"
路径闭环（演进后运行时注入通道与 meta_harness 关闭面齐平）；并将波 3
交付物（示例文档 + 安全段落覆盖风险提示）落位至文档站 peri-cool。

## 实施决策

### D1：演进 1 删除面（设计 §3.5 影响面逐项落地）

| 项 | 位置 | 动作 |
| --- | --- | --- |
| 注入函数 | `executor.rs` `permission_mode_notice_if_changed` + `permission_mode_name`/`permission_mode_semantics`（语义文本与 10_hitl 重复，一并删除） | 删除 |
| 哨兵 | `PERMISSION_MODE_NEVER_NOTIFIED` 常量（`executor.rs`，经 `peri-acp/src/session/executor.rs` re-export） | 删除 |
| 注入组装 | `executor.rs` run_session_loop 的 `mode_notice_booking` 生成 + agent_input `<system-reminder>` 拼接（原 recall 与通知共用容器） | 简化回仅 incoming_recalls 容器 |
| 载体 | `ModeNoticeBooking`（`executor_helpers.rs`）+ `V2ExecuteRequest.mode_notice_booking` 字段 | 删除 |
| 记账 | `mark_permission_mode_notified` + Phase 6 入队点调用 | 删除 |
| 状态 | `AcpSession.last_notified_permission_mode` 字段 + 两处初始化 + `SessionAccessPort::last_notified_permission_mode` 方法 + impl | 删除 |
| 测试 | `executor_test.rs` 5 个纯函数测试（unchanged/on change/once/retry/initial disclose）+ `mod_test.rs` 哨兵初始化测试 | 删除 |

删除后 `AtomicU8` / `PermissionMode` import 相应清理（executor.rs 的
`PermissionMode` import 删除，SharedPermissionMode 路径引用保留）。

### D2：10_hitl 段落联动（C3 D5 预述句）

`10_hitl.md` "controlled runtime notification" 句重写为无通知语义的机制
描述：mode 是会话状态、可中途变化；每次工具调用观察到的审批结果反映
**评估时刻**的 mode；模型不应假设先前看到的 mode 仍然有效（无运行时通知，
以最近一次审批结果为准）。旧句（"informed via a controlled runtime
notification…check for such notifications"）全仓零残留（测试断言无依赖）。

**语义代价确认**（设计 §3.5）：删除后模型失去权限模式感知通道——Default/
Plan 模式下 10_hitl 段落（C3 后 gate = 持有者装配，Bypass 下也渲染）仍描述
审批机制与四模式语义，但 mode 切换不再注入通知。审批边界认知由段落机制
说明承担；模型需从实际审批交互推断当前 mode。接受该代价（设计定案）。

### D3：波 3 文档落点（任务书 A1/A2）

- **A1（落位）**：正文搬运至 `peri-cool/src/content/docs/docs/features/meta-harness.mdx`
  （标题「系统提示词自定义（meta_harness）」），接入 `astro.config.mjs`
  「Agent 能力」分组导航（permissions 之后）。内容按 peri-cool 风格重排：
  配置示例（嵌套 `config.meta_harness` 形态，与 settings.json 实际结构一致，
  修正 wave3 草稿的顶层写法）→ 段落 ID 清单（**按 C3 后现状同步为 13 项**，
  含 persona/language，去 16_workflow/07_env/14_system_reminder）→
  middleware 名清单 → 覆盖文件形态 → 安全覆盖风险提示（Aside caution）。
  `check:site` 通过（85 pages）。
- **A2（spec 保留）**：wave3 issue 保持落盘为正文来源记录，状态 Open → Closed，
  状态变更记录注明搬运完成与 ID 清单同步。

## 验证

| 命令 | 结果 |
| --- | --- |
| `cargo build --workspace` | ✅ |
| `cargo clippy --workspace --all-targets -- -D warnings` | ✅ 零警告 |
| `cargo test -p peri-acp-types -p peri-acp -p peri-middlewares -p peri-agent --lib` | ✅ 139 + 385 + 1328（3 ignored 既有）+ 667 全绿（C4 净减 6 测试：executor_test -5、mod_test -1，与基线精确对齐） |
| `cargo test --workspace --doc` | ✅ 15 crate 全绿 |
| `bun run check:site`（peri-cool） | ✅ 85 pages，链接/索引/导航可达 |

残留检查：`__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__` 仅剩测试否定断言（C2 回归
锁定）；`permission_mode_notice` / `ModeNoticeBooking` / `mark_permission_mode` /
`last_notified` / `PERMISSION_MODE_NEVER` / `workflow_enabled` /
`FeatureGate::Workflow` 代码零残留；`16_workflow` 仅剩"已删除"说明性注释与
回归断言；e2e 无权限模式通知依赖。

## 偏差记录

1. wave3 草稿的 settings.json 示例为顶层 `meta_harness`，实际配置嵌套于
   `config` 内（`{"config": {"meta_harness": {...}}}`）——落位时按代码事实
   修正（任务书要求"正文已就绪，待落位"，落位点属实施判断）。
2. `permission_mode_semantics` / `permission_mode_name` 虽不在设计列出的
   影响面清单内，但仅为通知文本服务（语义与 10_hitl.md 重复），一并删除。
3. 工作区存在并发未提交改动（acp-hub 等），非本批范围，未触碰。

## 遗留问题

1. `FrozenSessionData::subagent_system_prompt` 遗留字段完整移除（C2 遗留，
   与 16_workflow 删除配套；当前回退 `system_prompt()` 字节相同，已由测试
   锁定，属清理项非功能项）。
2. `project_enabled_sections` 显式投影未接入生产渲染面（收集机制天然承担
   gate 判定；投影保留为契约 3 显式视图与一致性测试载体，C3 遗留）。
3. 10_hitl 段落现不再描述任何 mode 通知通道——若未来恢复"模式感知"
   需求，需在段落或注入面重新设计（超出波 4 范围）。

## 涉及文件

- `peri-agent/src/session/exec/executor.rs` / `executor_helpers.rs` / `executor_test.rs` — 演进 1 核心删除
- `peri-acp-types/src/session.rs` — `SessionAccessPort` 方法删除
- `peri-acp/src/session/mod.rs` / `mod_test.rs` / `session/executor.rs` — 状态字段、初始化、re-export、测试删除
- `peri-acp/prompts/sections/10_hitl.md` — 通知句重写
- `peri-cool/src/content/docs/docs/features/meta-harness.mdx`（新建）+ `peri-cool/astro.config.mjs` — 波 3 落位
- `spec/issues/2026-08-14-meta-harness-wave3-examples-and-safety.md` — 状态变更记录（Closed）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-14 | — | Closed | agent | C4 实施完成：演进 1 全删除面落地（注入/哨兵/载体/记账/状态/测试）+ 10_hitl 联动 + 波 3 文档落位 peri-cool + 三命令验证全绿 |
