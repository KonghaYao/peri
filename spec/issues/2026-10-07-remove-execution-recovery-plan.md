# 执行恢复机制剥离计划

- 状态：**四个 worker 已完成，代码已整合提交，提交后轻量复验通过**。
- 授权：2026-10-07 用户要求文档 → 充分计划 → 多 subagent → 完成/提交/整合 → 轻量验证；全部在独立 worktree。
- 工作树：`/Users/konghayao/code/ai/peri-remove-execution-recovery-20261007`。
- 分支：`refactor/remove-execution-recovery-20261007`；基线 `01c0efecff74427ca563a77a57de6188be256646`。
- 不包含原工作树并行 WIP，不修改真实用户库，不推送、不合并回原脏工作树。
- 结构基线：[表结构审核](2026-10-07-remove-execution-recovery-schema-review.md)。

## 1. 交付契约

删除新增持久执行恢复及其正常执行依赖，而非只禁用 load 自动恢复。
目标路径为进程内 MQ → Receive → Compact → Reason → Act → MQ；canonical history 正常落库。
保留 history 列表/加载/切换/重放、frozen/inherited、fork、正常续聊、当前工具/子 Agent 进度和取消、普通输入队列交互。
不恢复旧阶段、模型请求、工具调用、cold child、owner binding 或旧待发队列，不由旧 Work Unknown 冻结新输入。
不引入替代恢复表、兼容 shim、dual-write、Store 租约或假成功。
外部副作用不保证被停止或再次交付，不能从历史缺终态推断 live。

保留九张共享业务表、五条显式业务索引，Turso 保留两张一般机制表。
删除四张 session_work_* 和两张 session_control_* 表。保留旧库 goal/扩展对象与 messages.rowid。
schema 向前升级（基线17，目标18），不回退到 main10，不恢复 execution_runs。
session_close_intents 保留；普通关闭/重开不依赖撤销的持久控制状态。
SDK alpha 源码不修改；旧恢复协调模式需后续调整，不能宣称不变兼容。
普通本地/ACP prompt 不要求 SDK reverse admission capability。

## 2. 权威面与实施次序

先修正根 CLAUDE、architecture ARC-RCRA/子 Agent 身份、rcra-message-activation、session-async-tasks、user-input-queue、ACP protocol及设计索引；旧恢复目标被撤销，不把目标当已实现。
再完成本计划与结构基线，提交文档基线后派发以下四个互斥任务。
旧历史记录不改写；受影响 active issue 的 superseded 注记在交付阶段补齐。

## 3. 并行 write set 与集成约定

worker在指定独立worktree直接编辑，使用apply_patch；不切分支、不操作原树、不提交。
不同worker目录互斥；跨目录需求先反馈协调者，不越权改文件。
先读取自身CLAUDE、standards和本计划；参考main的普通行为，不整crate回退。
共享编译错误由协调者整合修复，不保留被删除能力的mock或fallback。

### A：领域类型与存储

Write set：`peri-acp-types/`、`peri-resources/`。

- 移除work/control模块、execution_admission协议及对应SessionResources/DataPort方法与adapter/gate实现。
- 保留普通SessionResources、访问/事务/关闭/历史能力；gate只剥离旧执行账本门禁。
- ToolContext移除invocation_intent/work_target/session_lifecycle；保留session_id、invocation_id、cancel、session_resources。新增可选`tool_call_id`及`with_tool_call_id`，默认None，供当前模型卡片归属。
- 纯运行期ControlAttempt等若仍有实际用途迁入运行领域，不保留持久reducer/journal。
- schema18仅删除六表与版本推进；新库/所有旧版升级终点不创建六表。
- 17升级不重建九表；保护goal、扩展对象与索引/trigger、历史rowid。Turso复用托管事务与store identity/version guard。
- 更新自身测试，覆盖历史保留、六表不存在、正常history写入、迁移失败回滚与旧版本直升。

### B：Agent

Write set：`peri-agent/`。

- 删除stages/work_*、恢复测试和注册，重接Receive/Reason/Act/dispatch普通数据流，保留模型/工具结果持久化。
- 移除TurnContext admission、cold factory binding/settlement、durable mailbox与journal依赖；保留普通child手动历史续聊。
- 输入队列保留enqueue/dispatch/takeback/steer/stop及可信当前run身份，删除跨重启发布/ACK/SDK结算。
- 工具上下文分别设置invocation_id和tool_call_id；子工厂取当前模型tool-call发事件，不读intent账本。
- 保留近期non-recovery逻辑、模型接口、当前取消策略；不能整crate回退main。
- 更新普通run、消息落库、子Agent身份、取消、历史续聊不恢复执行回归。

### C：ACP

Write set：`peri-acp/`，必要的`peri-controller/`、`peri-runtime/`。

- 删除execution admission/finish/cold execution/work recovery、scheduled admission和对应路由/测试。
- 移除stage builder、stdio及prompt对SDK reverse admission要求；prompt/input直接驱动正常Agent。
- 输入使用普通内存mailbox；取消旧durable control协议，正常控制交互按运行态重接，不保留成功shim。
- history restore仅装配metadata/payload/frozen/environment，删除owner quarantine与Work读写；保留replay、scope打开及cleanup真实错误。
- 普通关闭直接用一般关闭接口，不调用Work control；保留session list/load/resume/fork、system MCP startup和stdio关闭。
- 同步公开caps/wire与测试，撤销方法明确unsupported。

### D：MCP Middleware

Write set：`peri-middlewares/`；`mcp-packages/`仅必要的当前metadata适配。

- 删除invocation/owner cold recovery、持久Work binding与历史task discovery调用链。
- 保留当前MCP metadata、授权/HITL/cancel、活跃task subscription/通知、pool关闭和工具搜索。
- 从可信ToolContext构造session/invocation/tool-call关联，不依赖WorkState。
- 当前task注册和结果投递不能因旧durable身份缺失失败；保留scope隔离和错误可见。
- mcp-packages改动先列精确路径通知协调者，不削减工具自己的任务能力。
- 更新当前调用/结果/取消回归，删除仅证明冷恢复的测试，不屏蔽一般失败。

### 协调者

Write set：根、`docs/`、`spec/`；必要的跨层集成及TUI最小编译适配。

先完成权威文档/计划及文档提交，再并行派发。审阅diff并整合共享接口，移除残留调用，不搬回恢复复杂度。
TUI不重写UI，仅适配删除类型/协议的编译断点，history交互不删除。
更新code-index、active spec、验证记录，核对共享表DDL与结构审核一致。
worker完成后协调者统一暂存/提交，避免并发操作git index。

## 4. 风险与处理

- 普通循环与账本已耦合：必须重接消息/模型/工具结果写入，不只删checkpoint。
- 身份：model tool_call_id与invocation_id区分，测不同值/同名并发。
- 外部后台任务：不自动接管旧任务，当前结果/取消保留；停止追踪不声称资源停止。
- history owner依赖：只删Work装配，不删payload/frozen/environment准入。
- 普通close/存储Unknown：保留真实错误和有界排空，不以旧execution账本冻结新会话。
- 旧库cleanup会删goal：修正直升路径保护未知对象，schema18不建替代恢复表。
- SDK：记录旧恢复协议不兼容，不伪造ticket/proof，不修改alpha源码。
- 多worker：目录独占，共享接口按本计划冻结，协调者统一git操作。

## 5. 完成、提交与轻量验证

逐模块review并整合，做轻量定向检查，接口闭合后提交。再对提交快照轻量确认；本任务失败修复后补提交，不把启动命令当通过。

1. diff空白检查，本次源码≤1000行，文档本地链接检查。
2. formatter仅覆盖本次改动，检查Rust语法。
3. patched Cargo、locked/offline检查受影响crate及peri-tui，不跑workspace全量test/clippy。
4. 定向schema/history保留、普通Receive/prompt、MCP当前调用/取消测试。
5. 扫描production不再依赖WorkState/journal/reverse admission/cold recovery；migration删表语句和历史文档可留名称。
6. 构建阻碍如实记录，不宣称完整验收；真实Turso、WASM、UI E2E不在轻量门禁。

代码/文档只提交到新分支，不push，不纳入原WIP，不自动merge回原树。

## 6. 进度与证据

- [x] 独立worktree、固定基线、无upstream。
- [x] 权威目标修正、表结构基线与充分计划。
- [x] 文档基线提交：`0dbfb386`。
- [x] A/B/C/D并行完成，均已关闭。
- [x] review、整合、路由与验证更新。
- [x] 整合提交及提交后轻量确认：代码提交 `b9d09430`；下列命令均在该提交后通过。

### 实施与整合

- A 删除领域与存储 work/control，落地 schema18，保护 history rowid、九表与普通扩展；详细证据见 `peri-resources/execution-recovery-removal-report.md`。
- B 重接进程内 MQ、RCRA、消息持久化和当前父子任务；模型 tool-call ID 与独立 invocation ID 分离，当前后台结果接受不声称持久交付完成。
- C 删除 ACP 恢复协议、SDK reverse admission 与 cold owner 装配；旧方法返回 -32601，history/fork/compact/普通续聊保留。
- D 删除 middleware 与 Workspace 的持久 invocation/owner 恢复，包括后端 v1 内存 owner ledger，不保留 v2 绕过分支；当前 scope、task subscription、结果与取消保留。
- 协调者修正跨层 parent_tool_call_id 归属、TUI 普通 cancel 通知与 pump、移除 sidecar 启动和失效回执夹具。history UI 未重写，SDK alpha 源码未改。
- 编译整合暴露并修正：子任务配置字段、TUI 生产 run accessor、过时回执字段、middleware 测试夹具 context 参数。没有屏蔽错误或恢复旧兼容类型。
- 原工作树 WIP 未混入，没有访问或迁移真实用户库，没有 push 或 merge 回原分支。

### 已完成定向验证

所有 Cargo 命令使用 `./scripts/cargo-rmcp-patched.sh`、`--locked --offline`；未运行 workspace 全量测试或 clippy。

| 范围 | 结果 |
| --- | --- |
| A：types lib | 526 passed |
| A：resources schema / history / migration / contract | 83 / 41 / 3 / 6 passed；history 1 ignored |
| A：types doc | 1 passed，2 ignored |
| B：agent 定向 lib / doc / tests check | 289 / 10 passed；check 通过 |
| C：ACP 输入、取消、history、fork、compact、cron、关闭等定向测试 | 104 passed |
| D：Workspace 当前 invocation wire、取消与 scope barrier | 5 passed |
| 协调者：TUI cancel、load reservation、history load、旧 admission 忽略、stop wire、cancel drain | 9 passed |
| 协调者：peri-tui 入口 check | 通过，包含受影响生产依赖编译 |
| 协调者：middleware tests check / 当前 invocation 身份测试 | check 通过；3 passed |

ignored 不算通过。日志保存在隔离工作树 `target/removal-*.log` 和 A 的 `/tmp/peri-a-*-test.log`，不提交构建产物。
schema18 远端验证采用 SQLite-backed RemoteTransport，不是实际 Turso 网络验证；未执行真实用户库、WASM、UI E2E、跨进程故障注入或性能收益 benchmark。

### 提交快照复验

2026-10-07 对 `b9d09430` 执行，统一命令前缀仍为 patched Cargo；以下均为实际完成结果。

| 命令参数 | 结果 |
| --- | --- |
| `check --locked --offline -p peri-tui` | 通过，无新警告 |
| `test --locked --offline -p peri-resources --lib schema_v18 -- --test-threads=1` | 10 passed |
| `test --locked --offline -p peri-tui --lib requests::cancel_tests -- --test-threads=1` | 2 passed |
| `test --locked --offline -p peri-tui --lib load_by_id_has_no_recovery_popup_or_reset_request -- --test-threads=1` | 1 passed |

日志：`target/removal-postcommit-{entry-check,schema18-test,cancel-test,history-test}.log`。
修改源码≤1000行、根 CLAUDE 5892字节/70行、修改 Markdown 本地链接及 `git diff --check` 均通过。
恢复符号残留仅为迁移识别数据、历史文档及明确验证旧协议已撤销的测试；生产依赖已移除。
本记录提交仅改文档，不改变上述复验代码快照；工作树交付保持干净，无 upstream/push/原树合并。
