# Compatibility and validation ledger

> 本文件只记录**实际执行过**的命令与结果。旧版 daemon/registry/control socket 的验收数字已作废，不再沿用。

## 2026-10-27：MCP 透明代理（capabilities / resources / skills / 通知）

环境：Apple Silicon macOS，Node `v24.18.0`，Bun `1.4.3`，`@anthropic-ai/sandbox-runtime` `0.0.81`，真实 Peri `~/.peri/peri`（4.4.1）。

| 命令 | 结果 |
| --- | --- |
| `bun install` | 成功；SDK patch 重新应用到 `SubscriptionFilterSchema.passthrough()` 与自定义方法缺省 `resultType` 容错 |
| `bun run format:check` | 通过 |
| `bun run check` | 通过 |
| `bun run test` | **61/61 passed，0 failed** |
| `npm run test:mac:programmatic` | 通过：start、同 id `conflict`、占用端口 `conflict`、重复 close、`deleteData` |
| `node dist/tests/integration/mac.js <tmp-settings>` | 通过：7 个工具；外层 discover 镜像 `resources`/`resources.subscribe`/skills/tasks 扩展；`skills/list` 看到项目技能；`resources/list\|read` 可读技能；宿主 `$HOME/.claude/skills` 的 host-only 技能不可见；宿主临时文件 `Read`/`Bash cat` 均被 SRT 拒绝；token 401、同 workspace 重启保留 |

真实 macOS 验收的负向补充：默认 profile 的 `runtimeReadPaths` 包含 `/etc`，因此 `/etc/passwd` **可读**（已断言为可读事实）；越界负向改用 `tmpdir()` 下的宿主专用文件。这与旧计划中「`/etc/passwd` 读取失败」的预期不同，未修改默认 profile。

### 宿主 Rust

| 命令 | 结果 |
| --- | --- |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-middlewares --lib` | 1839 passed；3 failed（`plugin::marketplace::tests::test_fetch_url_cache_fallback`、`subagent::tool::tests::invoke_test::test_skill_preload_registered`、`subagent::tool::tests::integration_v2_test::test_p0_2_background_defined_skill_preload_once_after_parent_cancel`）。三者已在**同 HEAD 的 stash 基线**复现，与本轮改动无关 |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib` | **1086 passed，0 failed，1 ignored** |
| `bash scripts/check-changed.sh` | 通过（changed-crate `cargo check --locked`） |
| `bash scripts/verify-async.sh start && read` | 通过：全量 `check --workspace --all-targets` + `clippy -D warnings`；最终回执见 `target/verify-receipts/latest.json` |

## 2026-10-27：单实例迁移基线

环境：Apple Silicon macOS，Node `v24.18.0`，Bun `1.4.3`，`@anthropic-ai/sandbox-runtime` `0.0.81`。

| 命令 | 结果 |
| --- | --- |
| `bun install` | 成功；`better-sqlite3` 已从依赖树移除 |
| `bun run format:check` | 通过 |
| `bun run check` | 通过 |
| `bun run build` | 通过 |
| `bun run test`（`node --test dist/tests/*.test.js`） | **43/43 passed，0 failed，0 skipped** |

单元测试覆盖：settings init/load/hash、workspace 目录与锁、token 内存/文件、deleteData 与 symlink 拒绝、policy/transient unit 属性、cgroup 探针、diagnostics 轮转、PeriBackend 限流/Tasks 映射、HTTP codec，以及用 fake provider/backend 的实例生命周期（start/close 幂等、deleteData、同 id conflict、backend 退出、启动超时、abort、端口冲突、`stop_incomplete` 重试、symlinked root canonicalization）。

### macOS 真实 SRT/MCP（已执行）

使用真实 Peri 4.3.2 + SRT 0.0.81，全部写入临时目录（不使用 `~/.peri-sandbox`）：

| 命令 | 结果 |
| --- | --- |
| `npm run test:mac:programmatic` | 通过：start、同 id `conflict`、占用端口 `conflict`、重复 close、`deleteData` |
| `node dist/src/cli.js init --settings <tmp>` + `node dist/tests/integration/mac.js <tmp>` | 通过：7 个工具、Write/Read/Bash、未授权 401、关闭后同 workspace 重启 |
| 自定义程序化脚本（临时 settings） | 通过：Bearer MCP、`Bash pwd` 位于 workspace、文件跨重启保留、新 runId/新 URL、`deleteData` 删除目录与 `/private/tmp/peri-*` |

补充验证：以未 realpath 的 `/var/folders/...` 作为 `sandboxesRoot` 时，实例路径被 canonicalize 为 `/private/var/...`，启动与 MCP 调用成功。运行后未发现残留 Peri/SRT 进程。

未验证：进程组逃逸、宿主 SIGKILL 后的 macOS 残留、真实 ChatManager 端到端会话。

### 未在本机执行

| 项目 | 原因 | 状态 |
| --- | --- | --- |
| `scripts/macos-acceptance.sh`（OrbStack Ubuntu 24.04 VM） | 本轮未启动 VM | **未验证**：Linux cgroup、`systemd-run`、真实 SRT/seccomp、SIGKILL 清理、resources 验收均需在 VM 内重跑 |
| npm pack / 独立消费者安装 | 未重跑 | **未验证** |

Linux 侧需要手工确认：`run` 前台启动后 Ctrl+C 无残留；`kill -9` 宿主后 `systemctl --user list-units 'peri-sbx-*'` 无 active unit 且 cgroup 为空；同 sandboxId 第二实例 `conflict`；不同 id 可并行；显式端口占用 `conflict`；`close({deleteData:true})` 后目录不存在。

## 保留的保证

| 能力 | 说明 |
| --- | --- |
| workspace/home 持久 | `close()` 保留；只有 `deleteData` 删除 |
| SRT 路径/网络策略 | 每次启动重新生成 `policy.json`；`denyRead: ["/"]`，allowRead 仅运行时路径 + workspace/home/tmp |
| 单 workspace 单写者 | `<sandboxRoot>/.lock`（O_EXCL + pid 存活校验） |
| HTTP 鉴权 | 每实例 token；`timingSafeEqual`；Host/Origin 校验；请求 1 MiB / 响应 8 MiB |
| Linux cgroup 限制 | transient unit 保留 `MemoryMax`/`TasksMax`/`CPUQuota`/`PrivateTmp` |
| PeriBackend Tasks 映射与限流 | 每个实例独立 runId；旧 task 在新实例中不可用 |
| 停止确认 | Linux 等 cgroup 空；macOS 等进程组消失 |

## 不再提供的保证

| 旧能力 | 现状 |
| --- | --- |
| 稳定 MCP URL | 默认 ephemeral port；重启后重新取 `mcpUrl` |
| 崩溃后自动恢复/对账 | 无 daemon、无 registry、无 tombstone |
| 请求幂等回执 | 无 requestId 去重；由宿主负责 |
| 全局 `list` / `maxSandboxes` | 无 API；由宿主控制并发 |
| token 文件共享 | 默认内存 token；`tokenFile` 必须位于 `<sandboxRoot>` |
