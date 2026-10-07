# Peri WASM 生态接入调查

> 状态：非权威生态探索，未批准设计；不代表集成已实现或部署已验收。
> 官方资料核查日期：2026-10-05。基础设施能力与 Peri 实现分别列出，候选顺序不构成实施承诺。
> 现状入口：[peri-wasm 代码索引](../code-index/peri-wasm.md)；权威契约：
> [Session 异步任务](../design/session-async-tasks.md)、[架构边界](../standards/architecture-contracts.md)。

## 范围与已有基础

- 在外部 MCP、Langfuse、Turso 之外，考察协调、认证、实时视图、对象存储、推理网关和任务唤醒；不替换 RCRA、ACP、Session Store 或任务 owner。
- 本文讨论 `wasm32-unknown-emscripten` 与 JS/TS host，不是原生 WASI 支持。代码索引记录本地 `workerd` probe；Hosted Workers 尚未部署验收，不能称为生产就绪 Workers Peri。
- 已有路径是 TS SDK → ACP 原始帧桥 → Rust Agent/模型/Store；工具能力由 MCP 消费。Workers binding 属于 TS host，不能因 binding 可用就推断 Rust 已接入。
- [SDK 依赖](../../npm-packages/@peri-sdk/package.json)已有 Hono/Yjs；[SessionDocSync](../../npm-packages/@peri-sdk/src/sync/session-doc-sync.ts)已有只读复制。新增候选是认证/传输 adapter，不是引入替代框架。
- 下列“建议”均为推断；官方文档证明服务能力，不证明 Peri 的协调、授权、R2、模型或 Queues adapter 已实现。

## 1. Redis：可移植的宿主协调租约

官方：[分布式锁与原子租约](https://redis.io/docs/latest/develop/clients/patterns/distributed-locks/)。

- **可用能力**：`SET key token NX PX ttl` 原子抢占；唯一 token 的条件删除与条件续期避免误释放他人租约。旧版本可用 Lua 比较并删除，不可无条件 `DEL`。
- **建议层**：TS host 的调度/owner 路由 adapter；Redis 客户端与网络接入由宿主部署决定，不塞入 Rust core，也不把控制面抢占暴露成模型可调用 MCP 工具。
- **失效边界**：TTL 到期不证明旧执行已停止；暂停、断网、续期失败后须拒绝继续提交。异步复制 failover、Redis 重启丢锁及墙钟变化均影响安全，Redlock 也有明确假设，不是无条件强一致保证。
- **Peri 待接入**：外部租约不能另立 Session 写入资格权威；必须与既有 Store epoch/同事务写入栅栏契约衔接。随机 token 用于租约身份，不等于单调 fencing token；写入端须拒绝过期代际，副作用仍需稳定幂等键。

## 2. Durable Objects：Cloudflare 内的协调控制面

官方：[私有事务存储](https://developers.cloudflare.com/durable-objects/best-practices/access-durable-objects-storage/)、
[生命周期](https://developers.cloudflare.com/durable-objects/concepts/durable-object-lifecycle/)、
[Alarms](https://developers.cloudflare.com/durable-objects/api/alarms/)。

- **可用能力**：每个 DO 有私有、事务性、强一致存储；alarm 可唤醒对象，适合按可信 session scope 路由、保存调度意图与对账游标。
- **建议层**：TS host/sidecar 通过 binding 调用 DO。DO 存储不自动成为 Rust Session Store；远端工具执行仍由 MCP owner 负责。Redis 与 DO 是协调候选，不建议双重租约权威。
- **失效边界**：休眠、驱逐、部署/运行时重启可丢内存；无可靠 shutdown hook，应增量持久化并重建宿主/WASM。DO 的唯一寻址不证明外部旧 Agent/MCP 已停止，也不提供跨 DO/Turso 的原子事务。
- **Peri 待接入**：取得新代际、旧 owner 停止证据、写入栅栏与恢复对账仍须实现 adapter。Alarm 至少执行一次、异常重试有上限且每 DO 同时只能设一个 alarm；持久计划、幂等结算与重排下一次唤醒不能省略。

## 3. Hono JWK/JWKS：宿主认证入口

官方：[JWK Auth Middleware](https://hono.dev/docs/middleware/builtin/jwk)。

- **可用能力**：验证 token 签名与默认时间声明检查（声明存在时检查 `nbf`/`exp`/`iat`），可配置 `iss`/`aud`；应用授权需另写 middleware，验签不等于拥有会话访问权。
- **建议层**：复用已有 Hono，在 TS host 的 Session list/load/send 与 WASM 创建之前做认证/授权；将可信 principal 映射到 session、scope、凭证及订阅权限。不是 Rust core 或 MCP 的认证替代品。
- **失效边界**：明确算法白名单、issuer/audience、有效期要求及 JWKS 轮换/不可达策略；WS 重连与宿主重启重新授权，长连接另定过期/撤权策略。创建前的准入检查不能替代逐操作授权。
- **Peri 待接入**：依赖存在不代表此 auth adapter 已完成。不得通过切换模块级 `ENV`/共享环境变量实现用户隔离；重试命令还需按已授权身份与 session 检查幂等，不把 JWT 当执行租约。

## 4. Yjs：只读实时会话投影

官方：[y-websocket](https://docs.yjs.dev/ecosystem/connection-provider/y-websocket)。

- **可用能力**：客户端/服务器间分发 document updates 与 awareness；通用 provider 会同步客户端修改，不等于 Peri 的只读复制协议。
- **已有实现**：`SessionDocSync` 明确只允许已授权的只读复制，命令边界仍是 ACP，peer 不回写；协议 `2` 带 generation、sequence 和 chat/session state vectors。generation 匹配才用 delta，否则发 snapshot。
- **建议层**：TS host 的二进制 WS adapter 保留上述协议及授权、背压/字节预算；不宣称 `y-websocket` 即插即用，不替换 Rust transcript/Store，不走 MCP 工具面。
- **失效边界/待接入**：断线后同代际按 state vectors 补齐；宿主重建换 generation 后重取 snapshot。限制入站帧，不能接受客户端 document update；awareness 不是任务结果或 owner 证据，传输重连也不保证 ACP 命令恰好执行一次。新增 WS adapter 不因 Yjs 依赖已存在而算落地。

## 5. R2：成果与附件 blob

官方：[一致性](https://developers.cloudflare.com/r2/reference/consistency/)、
[Workers API 与条件写入](https://developers.cloudflare.com/r2/api/workers/workers-api-reference/)。

- **可用能力**：对象读写、删除及列举强一致；Workers `put` 支持 `onlyIf` 条件写入。启用缓存的自定义域名可能返回旧内容，不能把缓存读用于 owner/CAS 判断。
- **建议层**：TS host 管理上传/授权下载；若 Agent 需要消费文件或成果能力，封装为 MCP。Rust core 保持对象引用/领域语义，不直接依赖 R2 binding；不将 R2 当 Session Store 或完整 POSIX workspace。
- **失效边界/待接入**：用稳定 artifact ID/内容摘要和条件写入约束重复上传；重启后按已持久化引用找回，处理孤儿 blob、访问权限与删除生命周期。R2 上传成功与 Turso transcript 提交不是同一事务；须设计失败补偿/对账，不能把 blob 存在当作任务已结算。

## 6. Workers AI / AI Gateway：推理与网络治理

官方：[Workers AI OpenAI-compatible endpoints](https://developers.cloudflare.com/workers-ai/configuration/open-ai-compatibility/)、
[AI Gateway 请求处理](https://developers.cloudflare.com/ai-gateway/configuration/request-handling/)。

- **可用能力**：Workers AI 提供 Chat Completions/embeddings 兼容 HTTP 入口；AI Gateway 提供请求超时与失败重试。基础设施 API 可调用不证明具体模型满足 Peri 的 tool calling、SSE、usage 或取消契约。
- **建议层**：优先经现有 Rust 模型 HTTP/provider 配置消费兼容入口；TS host 管理认证、endpoint/模型配置或代理 binding 到 HTTP。协议差异才在 Rust provider adapter 处理，不借 MCP 绕过模型边界。
- **失效边界/待接入**：联合限制 Rust 与网关重试，避免放大推理调用/费用；流开始后中断不意味着可无损续跑，fallback 也可能改变模型语义。Gateway timeout 针对响应首段，不是完整流的总时限，宿主仍需取消/总预算。
- **边界**：现行 Workers AI 文档将 Responses API 限于 GPT-OSS 且不支持 streaming；不能把“OpenAI-compatible”写成全部 API/模型兼容。推理服务与网关不托管 Peri 的执行 owner、租约或恢复状态，也不取代 Langfuse 的 Agent 生命周期语义。

## 7. Queues：持久唤醒与外部处理

官方：[投递保证](https://developers.cloudflare.com/queues/reference/delivery-guarantees/)、
[顺序与消费模型](https://developers.cloudflare.com/queues/reference/how-queues-works/)、
[ack、重试与 DLQ](https://developers.cloudflare.com/queues/configuration/batching-retries/)。

- **可用能力**：默认至少投递一次，不保证顺序；Worker push 或 HTTP pull consumer 可异步处理，批次失败可能重投已处理消息，可逐条 ack/retry。
- **建议层**：TS host/sidecar 保存待唤醒意图并消费通知，经 ACP/可信控制接口驱动会话；Rust 保留 canonical transcript、任务语义与去重，MCP owner 保留外部任务事实。不能替换内存 MessageQueue 后就宣称跨进程任务恢复完成。
- **失效边界/待接入**：使用稳定 session/task/terminal-delivery ID，重投时检查代际与取消状态；canonical 提交或可信持久交接后才 ack。提交后未 ack 的崩溃必须允许安全重投；外部副作用也须幂等。DLQ/重试耗尽须可观测并可对账，不得静默视作完成。
- **跨服务窗口**：Store 写入与 Queues send 不原子，需评估持久 outbox/扫描补发；“已入队”仅是唤醒，不是结果已送达。租约超时不等于任务取消，consumer 重启也不能凭通知自行抢占执行 owner。

## 探索结论与验收门槛

以下成本为集成复杂度判断，不是工时估算；均未计生产部署验收。

| 候选 | 预期价值 | 定性成本 | 尚缺的关键验证 |
| --- | --- | --- | --- |
| Redis | 跨宿主调度租约，较少绑定云平台 | 高：需与现有 epoch/fencing 单一权威衔接 | failover/重启丢锁、续期失败、旧代际写入与副作用 |
| Durable Objects | 按 scope 协调、持久调度与唤醒 | 高：跨 Store 边界与宿主重建 | 驱逐/部署重启、alarm 重投、旧 owner 停止证明；Hosted Workers |
| Hono JWK/JWKS | Session 操作与实例创建前的身份准入 | 中：库已依赖，授权模型仍需接入 | 跨租户拒绝、JWKS 轮换/故障、逐操作与 WS 撤权 |
| Yjs 二进制 WS adapter | 复用只读投影，实现重连增量同步 | 中：保留现有协议，而非替换 provider | protocol 2、generation 变化、state vectors、恶意回写与背压 |
| R2 | 成果/附件脱离计算实例驻留 | 中：对象接入简单，跨 Store 结算另有成本 | 条件上传、孤儿清理、引用提交失败与租户权限 |
| Workers AI / AI Gateway | 增加推理来源与请求治理 | 低至中：配置匹配时较低，协议差异需 adapter | 实际模型工具调用、SSE/usage/取消、双重重试与费用 |
| Queues | 跨实例持久唤醒与异步处理 | 高：outbox、去重、对账不能省略 | 重复/乱序、提交后未 ack、取消代际竞争、DLQ 与补发 |

优先补宿主协调与权限，再补只读实时传输和 blob；模型网关与 Queues 是可独立评估的网络 adapter。
这些都是“围绕 WASM 的基础设施”，不是将 Peri core 改造成 Cloudflare 专用实现。

任何获批接入都应先验证：宿主/协调服务重启、租约过期后的旧写入、重复/乱序通知、
canonical 提交与 ack/上传间崩溃、跨租户拒绝、WS 断线/换 generation，以及推理流取消。
可恢复的持久事实、旧 owner 停止证据和结果幂等提交缺一不可；基础设施在线不能替代这些 Peri 验收。
