# Session 异步任务

- 状态：**执行恢复已剥离，当前任务生命周期保留**；验证范围见剥离计划。
- 消息与运行生命周期服从 [RCRA 权威](rcra-message-activation.md)；实施状态见[剥离计划](../../spec/issues/2026-10-07-remove-execution-recovery-plan.md)。

## 1. 归属与当前运行

每个会话拥有当前进程的任务目录、队列和取消关系。共享 MCP 连接不共享含混的收件人回调；结果投递给直接发起会话，父子关系仅表达委托与显式取消策略。

模型 tool-call ID 用于工具卡及子 Agent 展示归属；当前 invocation/task ID 用于当前调用关联。当前运行可保留可信绑定和去重，但不持久化 task binding、owner declaration、恢复 locator 或 terminal delivery ACK 账本。

## 2. 操作与结果

保留当前任务的启动、查询、等待、进度、结果和取消。TaskManager、MCP task subscription 与当前队列是运行期实现，不提供跨进程恢复保证。

完成结果按可信来源进入当前会话队列或既有 canonical reminder 路径，保留消息持久化与工具/任务错误可见性。展示成功不代表工具成功，取消请求已发出不代表外部资源已停止。Workspace 结果文件和产物引用保持原有访问和有界投影语义。

## 3. 移除冷恢复

不在 session/load/resume、新输入或进程重启时扫描旧 task scope、重建旧 invocation、接管 owner 或自动续跑 child。缺少旧 callback/任务身份不写 Work quarantine，不阻塞新的普通执行。

历史状态可显示，但不是当前 running。重启后不保证外部后台结果再次送达；外部任务可能继续存在，显式工具查询或人工处理不等于恢复旧 Agent 执行。

## 4. 取消与关闭

当前进程保留 Cascade/Independent 策略、取消 token、MCP owner 关闭顺序和有界排空。清理未结清明确报错，不假称已停止。

显式会话关闭意图可以独立保存；它不保存旧 Agent 阶段或恢复任务目录。关闭历史会话不自动发现/接管其旧外部资源。

## 5. 验收

覆盖当前任务结果/进度/取消、父子消息路由与展示身份、连接关闭失败和新输入处理。覆盖历史加载不触发任务发现或旧副作用；不再验收冷恢复、永久 terminal ACK 和跨进程处理义务。
