# Peri SDK Chat 架构

> 状态：**现行设计**（2026-10-25）。实现与验收证据见
> [Peri SDK Chat 重构 issue](../../spec/issues/2026-10-24-peri-sdk-chat-refactor.md)；
> 代码入口以 [TS SDK 索引](../code-index/peri-ts-sdk.md) 为准。
> 本文只描述稳定结构与边界，不记录实施批次、测试计数或平台验收结果。

## 1. 目标与边界

SDK 只做 Chat 控制面、ACP 运行装配与只读展示投影；模型执行、调度、会话持久化与工具运行仍归 Peri。一个 Chat 句柄对应一次可替换的 Session 绑定，不把 Chat 句柄 ID 当作持久 Session 身份。

不新增 SDK 调度器、持久执行恢复、平行会话表或第二套 ACP 协议。ACP 连接只有宿主注入的一条装配路径；SDK 不启动 Workspace 进程，也不提供本地数据库后端。

## 2. 五个核心对象

| 对象 | 职责 | 生命周期 |
| --- | --- | --- |
| `ChatManager` | 共享依赖快照、句柄目录、Session claim 协调、整体释放；不接收普通执行动作 | 进程/宿主内可重建；`shutdown` 后封闭新准入 |
| `ChatService` | 单 Chat 的绑定状态机、动作分派、事实日志、载荷读取；不持有 Manager，不依赖 Yjs | 由 `createChat` 同步登记；绑定在首次动作时建立 |
| `ChatUserAction` | 八类纯数据用户意图，携带稳定 `requestId` | 单次请求；结果按 `disposition` 判断可重试性 |
| `PeriInstance` | 一次激活的 ACP Session/宿主装配，交互、队列、历史与载荷控制 | 由 `ChatService` 拥有；绑定换代或关闭时释放 |
| `ChatView` / `ChatReplica` | 服务端事实到 Yjs 的投影、逻辑同步连接与浏览器只读副本 | `ChatView` 由 Chat 所有者释放；`ChatReplica` 由浏览器释放 |

依赖方向固定为 `ChatManager → ChatService → PeriInstance → Session → Transport`。`ChatView` 只依赖 `ChatFactSource` 与 `ChatYjsChannel`；`ChatReplica` 只消费同步帧与只读 `SessionView`。

## 3. 动作与回执

八类动作：`create_new_session`、`change_session`、`send`、`force_send`、`take_back`、`load_earlier_history`、`cancel`、`answer`。

- `send` 同步返回 `SendReceipt`：`inputId` 与 `accepted` 表示入队被接受，`then`/投递等待表示 Peri 已交付输入；两者不能混为一谈。
- `cancel` 的 `requested` 只表示取消通知已写出，不是停止证明。停止必须由 ACP 关闭/结算证据确认。
- 动作失败携带 `disposition`：`rejected` 明确未生效；`unknown` 可能已写出/执行且不得自动重试；`not_sent` 只由调用端在提交前产生。
- CF HTTP 层只做边界校验、目标绑定核对与错误映射，不重写领域规则。

## 4. 绑定、claim 与关闭

- `createChat` 只同步登记句柄，不启动 ACP、不 claim Session、不读取历史。
- 首次绑定由 `create_new_session` 或 `change_session` 触发；新会话使用句柄/默认 path，加载历史使用 Store 中的持久 cwd。
- 同一 Session 同时至多一个执行所有者。`SessionClaims` 由外部注入原子 KV；`LocalSessionIndex` 覆盖 binding/cleanup 候选，冲突带 `ownerChatId`。
- 空闲才允许换绑；运行中、存在未处理输入、待决交互或未确认清理时拒绝换绑，不 cancel、不 close 旧绑定。
- 关闭是有界所有权事务：`session/close` 在预算内重放。超时返回 `ChatCloseIncompleteError` 并保留 claim、索引与资源；不产生 `closed` 事实，可重试。
- `shutdown` 先封闭新准入，再分别结算在途绑定/关闭，最后释放 WASM 宿主；失败聚合上报，可重试。

## 5. 事实与同步

`ChatService` 发布按绑定代次编号的 `ChatFactEnvelope`（seed、历史批次、通知、队列、任务、交互、运行态、关闭）。事实日志供晚挂 View 重放；View 不执行动作、不决定权限、不取消任务、不关闭宿主。

- 每个绑定代次拥有独立的一对 Y.Doc 与同步源；换代时成对替换，旧代次条目不能混入新代次。
- `ChatYjsChannel` 只是逻辑帧通道：不创建 WebSocket/HTTP，不携带凭据，鉴权与物理连接由应用完成。
- 同步帧为二进制 `HELLO/SNAPSHOT/UPDATE/ACK`；用户可恢复向量、全量/增量快照、交付信用与有界背压由同步层负责。
- `ChatReplica` 只读：`readiness` 明确区分 `not-ready`、`ready`、`unsupported`；版本不支持不是空历史。公开读取为 `read()`、`subscribe()` 与纯选择器，不提供可写文档或快照门面。
- 客户端断线状态由物理传输适配器报告；断线不停止执行，重连通过 HELLO resume 补齐。

## 6. 宿主、端口与工厂

`ChatHost` 只有两个分支：

1. `{ kind: "wasm" }`：Manager 为每次激活启动独立 WASM 宿主；可注入 `moduleUrl`、`moduleFactory`、`env`、`ports` 与生命周期/诊断回调。
2. `{ kind: "transport", factory }`：宿主按 path 创建 transport；`factory.close()` 成功即停止证明，失败保留原因供重试。

`PeriInstance` 是内部实现，不是发布门面。工厂把句柄的路径、Store、宿主与生命周期上下文转换为一次运行装配；`PeriWasmHostManager` 负责宿主租约与释放证据。共享 MCP server 地址不共享 MCP client、声明路由或审批状态。

## 7. 发布入口

| 入口 | 内容 |
| --- | --- |
| 主入口 | `ChatManager`、`ChatService`、`ChatUserAction`、`ChatView`、`ChatReplica`、公开类型/错误、Store 读取工具 |
| `/portable` | 同一 Chat 公共面 + 嵌入方驱动的执行准入核心；仍依赖 `node:crypto`，不是纯浏览器入口 |
| `/view` | 浏览器安全展示出口：副本、选择器、载荷读取、同步协议；不引入 Node 宿主 |
| `/wasm` | 平台无关的 Emscripten 宿主、loader、transport、scheduler 端口 |
| `/wasm-host` | `/wasm` + Node 网络/DNS 端口 |

## 8. 非目标

- 不恢复 stdio/Workspace 进程管理、本地数据库后端、JSONL dispatcher 或执行恢复账本。
- 不让浏览器直接持有模型/Store 凭据，也不把宿主执行搬进浏览器。
- 不声明跨实例 claim、共享内存 Manager 或多租户隔离已实现；这些由部署注入或另行设计。
- 不用兼容 shim 维持两套业务入口；删除的公开门面按 breaking change 处理。
