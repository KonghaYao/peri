# 执行恢复机制剥离计划

- 状态：**权威文档已修正；计划完成，准备并行实施**。
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
- [ ] 文档基线提交。
- [ ] A/B/C/D并行完成。
- [ ] review、整合、路由与验证更新。
- [ ] 整合提交及提交后轻量确认。

实际结果、失败和提交OID在完成时补充，不预填通过。
