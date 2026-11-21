# peri-swarm 对既有 Peri 范围的改动

状态：实现记录（不构成运行时契约）；基线 `c972ddd6d`。
GitHub issue：[#515](https://github.com/KonghaYao/peri/issues/515)。
PR：[#513](https://github.com/KonghaYao/peri/pull/513)。
设计与验收：[docs/design/peri-swarm.md](../../docs/design/peri-swarm.md)、
[docs/verification/peri-swarm-local-dev.md](../../docs/verification/peri-swarm-local-dev.md)。

本单只回答一个问题：`peri-swarm` 本地开发在既有 Peri 中改了哪些边界、为什么必须改、
哪些没有改。

## 改动范围

| 位置 | 改动 | 默认行为 |
| --- | --- | --- |
| `peri-acp` | `reserved_root_session_id` opt-in；`session/new` 可选 `_meta.peri.rootSessionId`；UUIDv7 校验/一次消费/冲突映射 | 默认 `false`；未提供时仍自动生成 UUIDv7 |
| `peri-middlewares` | `HostWorkspaceBinding::{Remote, Absent}` 与宿主绑定窗口；MCP pool/initialize/config/builtin/workspace_io/watch 接线 | 未注入时保持既有 TUI/print/stdio 行为；builtin 关闭不产生第二套 workspace 面 |
| `peri-tui` | `cli_print.rs` / `launch.rs` 显式 `reserved_root_session_id: false` | 无行为变化 |
| `peri-sandbox` | 新增 `src/swarm/*` 本地 Bun 服务、`dev:swarm`/`test:swarm` | 既有单实例 `startSandbox`/HTTP 桥语义不变 |
| `peri-sandbox` | `instance/options.ts` 字段级 settings 错误；`runtime/policy.ts` SRT `allowRead` 加入 `dirname(periPath)` | 显式 `runtimeReadPaths` 保留；只扩大目录级读权限 |
| `peri-resources` | S6a 的单库 schema 16 已完整回退 | **净零**；Peri schema 15 / contract v9，无 `swarm_*` |
| workspace 根 | 新增 `peri-swarm` member、`tower` workspace 依赖、`Cargo.lock`、`.gitignore` 例外 | 无既有 crate 行为变化 |
| `docs` | 新增 design/verification/quickstart 与 10 个 `F-SWARM-*` | 文档索引更新 |

规模：既有 Peri crate 32 个文件、约 +2277/−63 行（含测试）；`peri-resources` 为 0。
新增 `peri-swarm` 是主交付；既有 crates 只承载身份、MCP 绑定、沙箱入口与默认路径修复。

## 为什么必须改

- 根会话 ID 必须由宿主预留，才能让 API 会话、Rivet actor key、Peri 根线程一一对应；
  否则只能在 Peri 之外另建并行身份与调度。
- workspace MCP 必须经既有 bare 初始化路径注入；另建第二套 MCP 装配会绕过 Peri 的发现、
  冻结、关闭与能力关闭语义。
- 沙箱服务复用既有 `startSandbox`、workspace 锁与 HTTP MCP 桥；只新增入口与 SRT 默认路径修复。
- 存储没有改 Peri schema：双库拆分后 Swarm 自己管理 `swarm_*`，Peri 库保持 15。

## 验证

- CI `44825942058`：Classify、Build（ubuntu/macOS/Windows）、E2E replay、CI result 全绿。
- 真实 Loom 双库、Rivet 3.1.1、真实 SRT、官方 `openai@7.62.0`；dogfood R1/R2/R3 与聚焦修复。
- 未验证：真实第三方模型、后台任务引用、1000 Agent、生产 TLS、Linux SRT、真实第三方 MCP。

## 复核命令

```bash
git diff c972ddd6d..HEAD -- peri-resources   # 应为空
git diff --stat c972ddd6d..HEAD -- peri-acp peri-middlewares peri-tui
git diff --stat c972ddd6d..HEAD -- npm-packages/peri-sandbox
```
