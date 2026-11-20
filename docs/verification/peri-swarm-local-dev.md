# Peri Swarm 本地开发链路验收（汇总）

- 状态：验收记录（汇总 2026-11-20 的多次真实本地运行；不重写历史原始记录）
- 日期：2026-11-20
- 工作树：`/Users/konghayao/code/ai/peri/.worktrees/peri-swarm-local-dev`
- 分支基线：`c972ddd6d`（S1 起点）；功能完成树：`3dbab78c9`（S9a4b，reattach）
- 范围：单机 loopback 上的 `peri-swarm` 网关、Rivet actor、进程内 Peri ACP Host、
  两个真实 Loom 逻辑库、Bun 托管沙箱与真实 SRT；模型侧为本地确定性 stub。
- 细则证据：
  [S9a1 文本会话](peri-swarm-local-2026-11-20.md)、
  [S9a2–S9a4b 沙箱/共享/断连/回收/恢复](peri-swarm-hosted-sandbox-2026-11-20.md)。
  本文只做索引与边界声明，不复制逐条断言与运行产物。

## 1. 环境与版本

| 组件 | 版本/标识 |
| --- | --- |
| macOS | 26.5.1（Apple Silicon，非 root，`/usr/bin/sandbox-exec` 可用） |
| Bun / Node | 1.4.2 / v26.9.0 |
| Peri | `target/debug/peri-swarm` 与 `target/debug/peri` 均为 0.2.0，经 `./scripts/cargo-rmcp-patched.sh` 构建 |
| Peri Loom | `/Users/konghayao/code/ai/peri-loom` @ `12dd01a15985634396dcdc10a49212e38c16702b`，预构建 `peri-loom-v1`、`peri-platform` |
| Rivet engine | `~/.rivetkit/engine/v3/3.1.1/rivet-engine-aarch64-apple-darwin`（Rivet 3.1.1） |
| 官方 SDK | `openai@7.62.0`（逐次由 `package.json` 读取版本） |
| 模型 | 本地 stub（`stub-model`），不访问外网 |
| 端口 | Loom 9497、Rivet 6420、Bun 8081、模型 stub 9496、网关 18080（8080 被用户 OrbStack 占用，未触碰） |

构建入口：`./scripts/cargo-rmcp-patched.sh build --locked -p peri-swarm`；沙箱内的
Peri 二进制为 `target/debug/peri`。每次运行的二进制 SHA-256 记录在对应细则的
「组件与版本」一节。

## 2. 已验证场景

下表只列本地真实运行覆盖到的行为；每行的判定与原始输出在细则对应章节。

| 场景 | 结果 | 证据位置 |
| --- | --- | --- |
| 官方 SDK 五个方法（create / get / events POST / events SSE / items） | 通过；`events.create` 的高层形状被 `400 unknown_field` 拒绝并记录，同一官方 client 的低层 `post` 提交网关 body 得到 `202` | [S9a1](peri-swarm-local-2026-11-20.md) §3–§5 |
| 文本轮次、SSE 增量与 complete 折叠、items 分页/游标 | 通过（122 条断言，连续两次运行一致） | [S9a1](peri-swarm-local-2026-11-20.md) §1、§4–§5 |
| 两个 Loom 逻辑库隔离（会话库 v15 无 `swarm%`；状态库 v1 四表 + 索引） | 通过（真实 Loom，运行后只读 SQL 交叉核对；后续各阶段继续复用两个独立库） | [S9a1](peri-swarm-local-2026-11-20.md) §6 |
| 真实 SRT + 模型驱动 `Bash`、文件落盘 | 通过；provider 为 SRT 子进程，命令写入 `~/.peri-swarm/sandboxes/<sandboxId>/workspace/hello.txt` | [S9a2](peri-swarm-hosted-sandbox-2026-11-20.md) §5 |
| 冷态工具面：创建/握手/`tools/list` 不启动沙箱，首个 `tools/call` 才启动 | 通过；创建返回 `reserved`，6 s 静默窗口后仍为 0 个 SRT 进程组 | 同上 §10 |
| 启动后资源重发现 | 通过（15 条资源）；句柄快照不刷新，已记录为边界 | 同上 §10.4 |
| A/B/C 共享 `openai_hosted` 环境（独立沙箱、复用环境、文件可见性） | 通过；C 复用 A 的 `sandboxId`/`runId`，B 独立 | 同上 §11 |
| A/C 并发与取消隔离 | 通过；两个会话主调用真实重叠，取消订阅不终止执行、不影响对方 | 同上 §11.5–§11.6 |
| SSE 断流后继续执行、items 补齐、按 `Last-Event-ID`/`afterSeq` 续读 | 通过；断开期间无订阅，turn 仍完成，重连无重复无缺口 | 同上 §12 |
| 输入幂等：并发同 key、不同内容 409、完成后重放 | 通过；恰好一次执行，状态库只有一行 `swarm_inputs` | 同上 §12.8–§12.9 |
| Bun 空闲回收：在途请求阻断回收，回收后文件与绑定保留，重新 ensure 新 `runId` | 通过（阈值 2 s，真实 4 s 慢命令） | 同上 §13 |
| 服务不可用：Bun 停机、Loom 停机 | 通过；错误 `502 upstream_unavailable` 可见，持久 items 中 `tool_result` 为 `failed`，无本机 SQLite 接管 | 同上 §13.6–§13.7 |
| 仅杀 Rust 宿主后的 reattach | 通过；历史只读不触达 Bun，首次输入触发 `session/load`，旧文件哈希/mtime 不变，工具调用未重放 | 同上 §14 |

未达成的历史断言保留在原始记录中，不回填为通过：S9a2 首轮「创建阶段 start 计数为
0」在修复前未达成（§6），修复后的运行见同一文档 §10。

## 3. 证据来源：真实组件 vs 模型 stub

| 断言 | 来源 |
| --- | --- |
| HTTP 路由、Bearer/`OpenAI-Beta` 门禁、状态码与错误信封 | 真实 `peri-swarm` 进程（axum） |
| 会话行、item seq/内容、输入回执、两库 schema | 真实 Loom 逻辑库（Hrana） |
| actor 生命周期、SSE 广播、idle/running 状态、reattach 路由 | 真实 Rivet engine 3.1.1 + `SwarmActor` |
| 进程内 ACP 会话、`session/new`/`session/load`、流式通知、工具 wire | 真实 Peri ACP Host 与 middleware |
| SRT 隔离、沙箱进程组、文件写入 | 真实 `peri-sandbox` + 真实 SRT（macOS `sandbox-exec`） |
| 回答文本、delta 切分、模型调用次数 | 本地确定性 stub（`stub-echo:<sha1>#<n>`、`[[tool:...]]` 指令） |
| 工具选择与最终答复质量 | **未验证**：stub 固定返回单一工具调用与固定文本 |

## 4. unverified 与前置条件

以下项目没有运行证据，不能作为已验收能力引用：

1. **真实模型供应商**：所有文本、tool call 与 helper（title/prediction）都来自 stub；
   真实模型的多轮工具使用、重试与错误恢复需要有效供应商凭据与计费环境。
2. **后台任务引用**：Bun 转发层未识别 `resultType:"task"`/`tasks/get`，空闲回收不看
   任务台账；需要先实现任务引用采集，再做长任务回收验证。
3. **未完成 turn 与崩溃中断**：`session/resume` 未实现，`state=running` 时返回
   `502 session_resume_required`；需要崩溃时正在执行命令的注入手段与恢复证据。
4. **1000 Agent 规模**：没有吞吐、并发会话上限或单 engine 容量数据；需要多机/多进程
   压测环境与观测口径。
5. **生产 TLS**：网关 loopback + 单 Bearer；Loom 只能 loopback HTTP（Rust 存储 SDK
   无法注入 CA）；生产证书、轮换与外部客户端接入未验证。
6. **Linux SRT**：`peri-sandbox` 有 `systemd-run` 路径，但沙箱、回收与文件权限只在
   macOS 验收；需要 Linux 主机与 systemd user session。
7. **调用方第三方 MCP**：`agent.tools` 注入形状有集成测试；真实第三方 MCP server 的
   连接、鉴权头与工具结果回填未端到端运行。
8. **跨机调度 / 多实例接管**：不在本地设计范围，无任何运行证据。
9. **官方 Agents API 完整形状**：`sessions.stream`、`AgentSession.status`、
   `items.data`/`has_more` 未做兼容层；差异见细则 §8。
10. **冷态可见性**：hosted 会话冻结的 system prompt 不含远端 workspace 指令/覆盖；
    启动后资源发现结果不回写句柄快照。

## 5. 复现与门禁

操作步骤（环境变量、启动顺序、SDK 示例）见
[peri-swarm README](../../peri-swarm/README.md)；沙箱服务的独立用法见
[peri-sandbox README](../../npm-packages/peri-sandbox/README.md)。

本地可重复的自动化门禁（不依赖 Loom/Rivet/Bun）：

```bash
./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm
cd npm-packages/peri-sandbox && bun run build && node --test dist/tests/swarm/*.test.js
```

交付时的最后一次运行结果（2026-11-20，S9a4b 提交前）：`peri-swarm` 205 passed、
0 failed（含 3 个 `loom_store` ignored 的真实 Loom 测试）；Bun swarm 测试 37 pass、
0 fail。真实链路的场景证据以 §2 链接的细则为准，本次文档变更没有重跑这些场景。

运行产物、连接文件与日志在各阶段的 `/tmp` 或 `/private/tmp` 运行目录，不入库；
记录和驱动脚本不含 token：网关/Bun 使用一次性随机测试串，Loom 项目 key 只从
`connection.json` 读入进程环境。

## 6. 2026-11-21 dogfood 第一轮修复

第一轮「只读发布表面」dogfood 报告记录了默认路径阻塞（macOS SRT 执行 Peri 父目录、
settings 缺字段无可读错误、SDK 示例两实参丢 `after`、回放无 `turn` 帧、items 无
`id`、hosted 会话无 sandbox 投影）。本轮修复与自动化证据：

| 修复 | 证据 |
| --- | --- |
| SRT policy 自动放行 `periPath` 父目录 | `peri-sandbox` `provider-doctor.test.ts` 的 policy 测试；全量 72 pass |
| settings 校验错误带字段名 | `settings.test.ts` 的 `periSha256` 缺失/格式与最小合法形状测试 |
| items 同时给出 `id`（等于 `itemId`） | `peri-swarm` 单测与 `items_pagination_test` |
| hosted 会话投影 `sandbox`（sandboxId/workspacePath） | `openai_hosted_test` 创建与 GET 断言 |
| legacy stub 每个用户轮次一次工具调用 | 本地 stub 两轮请求实测（两次 `tool_calls`，两次 `stop`） |
| README/设计契约同步 | `peri-swarm/README.md`、`docs/design/peri-swarm.md` §4.2/§4.5、`peri-sandbox/README.md` |

门禁结果（2026-11-21，本工作树）：`cargo test -p peri-swarm` 205 passed / 0 failed
（3 个真实 Loom 测试仍 `ignored`）；`peri-sandbox` 全量 72 pass、swarm 子集 37 pass。
**本轮没有重跑 Loom/Rivet/Bun 的真实端到端链路**：SRT exec、hosted 工具调用与 SDK
三实参续读需要第二轮 dogfood 用真实环境复验，不能据此记录为已验收。
