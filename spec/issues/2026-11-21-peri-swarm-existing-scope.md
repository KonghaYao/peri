# peri-swarm 对既有 Peri 范围的改动

状态：实现记录（不构成运行时契约）；基线 `c972ddd6d`；审查后 HEAD `86cba6349`。
GitHub issue：[#515](https://github.com/KonghaYao/peri/issues/515)。
PR：[#513](https://github.com/KonghaYao/peri/pull/513)。

本单只回答：`peri-swarm` 本地开发在既有 Peri 中改了哪些边界、为什么必须改、哪些没有改。

## 改动范围（审查后）

| 位置 | 改动 | 默认行为 |
| --- | --- | --- |
| `peri-acp` | `reserved_root_session_id` 与 `lazy_workspace` 两个 opt-in；`session/new` 可选 `_meta.peri.rootSessionId`；UUIDv7 校验/一次消费/冲突映射 | 默认 false；未提供时仍自动生成 UUIDv7；普通远端 workspace 失败仍拒绝初连 |
| `peri-middlewares` | **已删除**未使用的 `HostWorkspaceBinding`/`set_host_workspace_binding`/bare 绑定分支与专属测试；保留 `lazy_workspace` 池侧判定与 ACP `mcpServers` 注入路径 | 未 opt-in 时保持既有 TUI/print/stdio 行为 |
| `peri-tui` | `cli_print.rs` / `launch.rs` 显式关闭预留 ID 与 lazy workspace | 无行为变化 |
| `peri-sandbox` | 新增 `src/swarm/*` 本地 Bun 服务、`dev:swarm`/`test:swarm`；settings 字段错误 | 既有单实例 `startSandbox`/HTTP 桥语义不变 |
| `peri-sandbox` | 公共 `policyFor()` **不再**默认放行 `dirname(periPath)`；新增显式 `allowPeriBinaryDirectory` opt-in，仅 Swarm manager 传 true | 普通调用方与公共策略保持原范围 |
| `peri-swarm` | `create_session` 先写 `creating` 行（`logicalId → rootSessionId`），再 `session/new`；重试按原 ID attach/重建 | 不重新生成 root ID，不重放旧输入 |
| `peri-resources` | S6a 单库 schema 16 已完整回退 | **净零**；Peri schema 15 / contract v9，无 `swarm_*` |
| workspace 根 | 新增 `peri-swarm` member、`tower` workspace 依赖、`Cargo.lock`、`.gitignore` 例外 | 无既有 crate 行为变化 |
| `docs` | 新增 design/verification/quickstart 与 10 个 `F-SWARM-*` | 文档索引更新 |

规模：既有 Peri（`peri-acp` + `peri-middlewares` + `peri-tui`）25 个文件、约 +1295/−24（含测试）。
`peri-resources` 为 0；`peri-sandbox` 为新增服务与两个既有文件的小改；主交付是新增 `peri-swarm`。

## 为什么必须改

- 根会话 ID 必须由宿主预留，才能让 API 会话、Rivet actor key、Peri 根线程一一对应；
  否则只能在 Peri 之外另建并行身份与调度。
- 冷启动延迟必须由宿主显式声明；普通远端 workspace 的探测/失败语义不能被 Swarm 的懒启动需求改写。
- 沙箱服务复用既有 `startSandbox`、workspace 锁与 HTTP MCP 桥；公共 SRT 策略不扩权，
  Swarm 只在自己的调用点显式放行 Peri 二进制目录。
- 创建中断后的身份一致性由 Swarm 自有状态库解决：先落 ID，再调 ACP。
- 存储没有改 Peri schema：双库拆分后 Swarm 自己管理 `swarm_*`，Peri 库保持 15。

## 验证

- PR HEAD `86cba6349`；`cargo test -p peri-swarm` 216 passed / 0 failed / 5 ignored；
  Bun 包 74+38 测试通过；fmt/clippy/pre-commit 通过。
- 真实 Loom 双库、Rivet 3.1.1、真实 SRT、官方 `openai@7.62.0`；dogfood R1/R2/R3 与聚焦修复。
- 未验证：真实第三方模型、后台任务引用、1000 Agent、生产 TLS、Linux SRT、真实第三方 MCP。

## 复核命令

```bash
git grep -n "HostWorkspaceBinding\|set_host_workspace_binding" -- ':!*.md'   # 应无结果
git diff c972ddd6d..HEAD -- peri-resources   # 应为空
git diff --stat c972ddd6d..HEAD -- peri-acp peri-middlewares peri-tui
git diff --stat c972ddd6d..HEAD -- npm-packages/peri-sandbox
```
