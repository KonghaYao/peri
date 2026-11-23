# Peri Swarm 本地会话、沙箱与恢复验收

基线为 `3de541f77420d8edb9dc67c9f68b6ffc2410a08c`。持久化使用同一 Loom 部署中的两个逻辑库。全部 SDK 与故障注入测试使用本地确定性模型。

| 用户场景 | 验收观察 |
|---|---|
| 同一会话连续追问 | 官方 `events.create()` 以稳定幂等键提交。第二轮须读到第一轮才知道的随机文本。断开后用原事件游标补读，同键重发只增加一次模型调用和一个 turn。 |
| 共享环境还有一个使用者 | 分别先删除环境创建者和最后加入者。存活的会话可以继续将同一环境分享给新会话；最后一个引用删除后，过期的环境 ID 被拒绝。 |
| 用户离开后再次操作 | 冷态发现返回七个工具。正常空闲回收后，原 MCP 客户端用原 URL 访问原文件；并发唤醒只启动一个新实例。显式停止或删除保持停止。 |
| 客户端已断开，工具仍在运行 | 后台返回 task 后，无轮询、正常轮询、未读回执和回执送达前断线各单独模拟。命令最多执行一次，任务结束后沙箱可再次回收。 |
| 宿主在工具写入后中断 | 仅终止测试拥有的网关进程，保留 Loom、Rivet 和 Bun。用 SDK 重连、查 items 和 turns，核对原目录中的写入次数及模型调用次数；用新输入恢复会话并核对新旧 turn 的各自终态。 |

## 2026-11-23 实测

| 验证 | 本地观测 |
|---|---|
| 官方 Node SDK | `openai@7.60.0` 和 `openai@7.62.0` 各通过 319/319 项断言。同一套用例在两版 SDK 上分别运行，创建、追问、续读、取消探针的 SSE 事件序号均无缺口。 |
| 默认客户端写法 | `events.create()` 省略自定义请求选项，配合默认 `events.stream()`，第二轮答出第一轮生成的随机值。官方 `turns.list` 和 `turns.retrieve` 核对了两轮记录。 |
| 空闲与共享 | 独立 MCP 客户端模拟 17/17。原版冷态发现缺少 `resultType`、回收后旧客户端得到 404，已受理的后台任务也会在写文件前被终止。补丁通过原客户端唤醒、任务终态后回收、断线不重发、显式停止和删除。双库 Loom 上先删创建者、先删复用者两种顺序均通过。 |
| 原生后台命令 | 空闲阈值设为 500 毫秒，Peri 原生 Bash 在后台运行 4 秒后写入文件。保持连接和收到回执后断连两种情况下，客户端均未查询任务、命令各执行一次。任务完成后沙箱回收，再从原 URL 唤醒新的 run 可读回文件，`sandboxId` 和工作区均相同。 |
| 宿主中断 | 官方 SDK 与真实宿主共通过 45/45 项断言。Bash 写入文件后强杀 Rust 网关，三次重连没有触发新执行。下一条追问完成，旧 turn 记录为失败。工作区中的命令标记始终只有一行。 |
| 仓内回归 | Rust 默认套件 389 项通过，另在空白双库实跑 3 项 Loom 测试、在本地 engine 实跑 2 项 Rivet 测试，均通过。剩余 1 项为显式跳过的 ACP 测量。沙箱两组共 120 项通过，Linux 上跳过 1 项 macOS 专用测试。原生 Peri 0.2.0 的冷、热工具列表及七项 schema 一致。 |

本地服务使用 Rivet 3.1.1、Loom v1.1.24、同一实例中的两个逻辑库和确定性模型。宿主中断测试使用真实 Bun、Bubblewrap 与源码构建的 Peri MCP。独立的断线与回收模拟使用可控制回执时机的 MCP 子进程。Linux 执行环境无法使用 systemd 用户总线，且拒绝 SRT 创建其 Unix socket。取消探针收到终态 `completed/end_turn`，生成中的模型中断仍待验证。

## 复验入口

在可用的本地双库 Loom、Rivet、模型 stub 和网关上，将 `NODE_PATH` 指向已安装官方 `openai` 包的目录。驱动使用 `PERI_SWARM_BASE_URL` 和 `PERI_SWARM_API_KEY`。故障注入驱动的 `--help` 说明网关路径、监听地址和日志目录的要求。

```sh
node peri-swarm/scripts/sdk_sessions_e2e.mjs
node peri-swarm/scripts/sdk_shared_binding_delete_e2e.mjs
node peri-swarm/scripts/sdk_recovery_restart_driver.mjs --help
scripts/cargo-rmcp-patched.sh test -p peri-swarm --locked
```

在 `npm-packages/peri-sandbox` 运行沙箱测试，将 `PERI_SNAPSHOT_PERI_BIN` 指向由同一源码构建的 Peri CLI。

```sh
bun run test
bun run test:swarm
bun run snapshot:workspace-tools:check
```

Rust 的 `actor_runtime_test` 单独覆盖 SQLite 持久 MQ 中的 `EnsureProcessing` 结果。它检查 ACP 只消费一次原始投递，随后接纳独立的新用户 turn。跨进程工具用例覆盖同步命令的中断边界，两种证据分别保留。
