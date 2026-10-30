# Git Watch（git ref 变化提醒）

> **状态**：**现行设计**——能力已下放：原宿主 `GitWatchMiddleware` 已删除，
> 由 builtin `workspace` MCP 实例的 `workspace://git/ref` 资源 + MCP 2026-07-28 订阅回传。
>
> **事实源**：`mcp-packages/workspace/src/git_watch.rs`（采样 / 状态机 / 资源
> 正文）、`mcp-packages/workspace/src/workspace.rs`（`call_tool` 触发点、resource 与
> subscription 面）、`peri-middlewares/src/mcp/client/subscription.rs`（宿主消费：回读
> 资源 → canonical reminder）、`peri-middlewares/src/mcp/builtin/workspace_subscription.rs`
> （默认订阅注入）。
>
> **关联契约**：ARC-MIDDLEWARE-001、ARC-CAPABILITY-CLOSURE-001、
> [middleware-system.md](middleware-system.md)、[system-reminder.md](system-reminder.md)、
> [../reference/mcp-ecosystem.md](../reference/mcp-ecosystem.md)（2026-07-28 订阅口径）

## 0. 实现态

| 面 | 事实 |
| --- | --- |
| 生产者 | builtin `workspace` 实例（`WorkspaceMcpServer`）；**不在链上**，无 middleware 槽位 |
| 触发点 | 服务端 `tools/call` **成功**返回后（`is_error != Some(true)`）；无 `before_agent` 等价入口 |
| 传输 | `notifications/resources/updated`（`subscriptions/listen` 长流，同进程 duplex） |
| 订阅建立 | 宿主默认注入 `subscriptions.resources = ["workspace://git/ref"]`；用户显式配置优先 |
| 会话送达 | Workspace server 在资源更新通知 `_meta["peri/messageKind"]` 声明 `info`；宿主回读资源正文 → canonical SystemReminder（`MessageKind::Info`，不唤醒）。缺字段时仍保持此资源的旧默认。 |
| 关闭 | `WorkspaceMiddleware: false`（关实例 ⇒ 不建立订阅，零 git 调用）或实例 `subscriptions` 覆盖（含空配置 ⇒ 不订阅） |
| 测试落点 | `cargo test -p peri-mcp-workspace --lib -- git_watch`；`cargo test -p peri-middlewares --lib -- mcp::builtin_subscription_wire`；`cargo test -p peri-middlewares --lib -- mcp::client::subscription` |

### 0.1 与旧 middleware 实现的差异

旧宿主 `GitWatchMiddleware` 已删除；触发点从宿主 `after_tool` 收窄为服务端 `call_tool`（非 workspace 工具改动 git 时，延迟到下一次 workspace 工具调用才被发现），送达从 `MessageQueue` 改为订阅通知 → 宿主回读 → `MessageKind::Info` + `MessageSource::McpNotification`。节流窗口、单飞、短路语义与 notice 文案逐字保留；采样新增 `kill_on_drop(true)`；提醒映射内置在宿主消费侧，不新增配置面。

## 1. 问题与目标

### 1.1 背景

主 Agent 在 `07_runtime` 中仅冻结「是否在 git 仓库」与 cwd 快照；会话进行中**分支切换**或 **HEAD 前进**（新 commit）不会自动进入模型上下文。用户或外部进程在会话外 `checkout` / `commit` / `pull` 时，模型可能基于过时的分支或 commit 假设执行不可逆 git 操作。

`GitAttributionMiddleware` 负责 Write/Edit 贡献与 Co-Authored-By 的 `prompt_contribution`，**不**承担仓库拓扑监视。

### 1.2 目标（已对齐产品意向）

| 项 | 决策 |
| --- | --- |
| 监视维度 | **仅**当前分支名 + **HEAD commit**（完整 hash） |
| 不监视 | 工作区 / 暂存区 / untracked（避免 Agent 自身 Write/Edit 造成误报） |
| 形态 | builtin `workspace` 实例的资源 + 订阅（**不再有**独立 middleware） |
| 提示语言 | **固定英文** 正文 |
| 注入方式 | `MessageKind::Info` + `MessageSource::McpNotification`（transcript 层统一 `<system-reminder>`） |
| 非 git | 探测一次 → 本实例 `NotRepository`，后续零 git 调用 |

### 1.3 非目标

- 不跑 `git status` / porcelain；不报告 dirty files。
- 不 inotify、不后台线程轮询（**采样只在成功的 workspace 工具调用后发生**）。
- 不向 SubAgent 链装配（实例面天然不在链上）。
- 不修改 `GitAttributionMiddleware`。

## 2. 触发与节流（核心）

### 2.1 设计意图

- **每次 workspace 工具执行成功返回后**都应有机会发现「外部导致的」分支或 commit 变化（例如在 `Bash` 里 `git pull`、`checkout`）。
- 若每个触发点都真正调用 git，高频工具回合会产生大量子进程；因此采用 **1 分钟节流**：同一实例上，两次**实际 git 采样**之间至少间隔 **60 秒**（墙钟）。

### 2.2 触发入口（实现态）

| 入口 | 行为 |
| --- | --- |
| `WorkspaceMcpServer::call_tool` | 成功返回（`is_error != Some(true)`）后 `spawn` 采样；**不阻塞响应** |
| 订阅 sink 表为空 | **直接返回**（无订阅者 ⇒ 零 git 调用） |

**语义收窄（唯一且已知）**：非 workspace 工具（SubAgent / Workflow / 外部 MCP）改动 git 时，延迟到下一次 workspace 工具调用才被发现。若需闭合该延迟，须另立「仅订阅活跃时的 60s tick」并同步改本文非目标与 e2e「git 安静」前提。

触发点唯一 = 服务端 `call_tool`；链上不存在对应 hook。

节流 / 单飞 / 短路状态机（`GitWatchState`）：

1. 若 `RepoMode::NotRepository` → return（**短路后不再 spawn**）。
2. 若采样在途（`in_flight`）→ return（单飞）。
3. 若上次**完成**时刻距今 `< 60s` → return（不采样、不通知）。
4. CAS 抢占后由调用方 spawn 采样。
5. 采样收口：`Repository` → 更新快照与资源正文（有变化 ⇒ 通知）；`NotRepository` → 置短路；`Failed` / 超时 → `None` 且**不推进节流**。

### 2.3 与「每个 tool 触发」的关系

- **每个**成功的 workspace 工具调用都会进入节流判定，但多数调用在 60s 窗口内**立即返回**，无子进程。
- 窗口外的第一次调用才真正执行 `git rev-parse`（§4）。

### 2.4 失败与超时

- 单次采样超时预算 **1s**；失败或超时：**不**更新快照、**不**推送、**不**推进节流（下次触发可立即重试）。
- 超时后子进程由 `kill_on_drop(true)` 收口，不留下孤儿进程。

## 3. 状态模型

### 3.1 `GitSnapshot`

| 字段 | 来源 |
| --- | --- |
| `branch` | `git rev-parse --abbrev-ref HEAD` |
| `head` | `git rev-parse HEAD` |

**变化判定**：`branch` 或 `head` 任一变化即通知（`dirty` 不参与）。

### 3.2 `RepoMode`

```text
Unknown → 首次采样成功 → Repository
        → 确认非 git   → NotRepository（实例内短路）
```

非 git 的判定以 `git rev-parse --is-inside-work-tree` 输出为准；**普通非仓库目录**下 git 以非零退出，按 `Failed` 收口（不置短路、下次仍可重试）；`NotRepository` 短路分支由解析层与状态机用例锁定（`git_watch_test.rs`）。

### 3.3 资源正文

| 状态 | 正文 |
| --- | --- |
| 未采样 | 固定文本 `[Git watch] Repository ref has not been sampled yet.` |
| 采样完成、无变化 | 快照文本（`Repository ref snapshot:` + 当前 Branch/HEAD） |
| 采样完成、有变化 | `notice_text`（**逐字**固定文案，见 §5） |

**提醒正文 = 资源正文逐字**（超长仅在宿主侧按 8 KiB 上限截断，见 §7）。

## 4. Git 采样（性能）

- 环境：`GIT_OPTIONAL_LOCKS=0`。
- **不**调用 `git status`。
- 单次 spawn（实现形态）：

```bash
git rev-parse --is-inside-work-tree HEAD --abbrev-ref HEAD
```

- 超时 **1s**；异步 `tokio::process::Command`，`current_dir = 实例冻结的 host cwd`，
  `kill_on_drop(true)`。

## 5. 正文文案（固定英文）

变化 notice（**逐字**，不含外层 `<system-reminder>`）：

```text
[Git watch] Repository ref changed since the last sample:
- Branch: {prev} → {curr}     (omit line if unchanged)
- HEAD: {prev_short} → {curr_short}   (omit line if unchanged)

Sampled after a tool run or turn start. Run `git status` and `git log -1` before irreversible git operations.
```

- HEAD 展示 **7 字符**短 hash。
- 若仅一项变化，只列对应 bullet。

快照文本（无变化；新增）：

```text
[Git watch] Repository ref snapshot:
- Branch: {curr}
- HEAD: {curr_short}

Sampled after a tool run. Run `git status` and `git log -1` before irreversible git operations.
```

## 6. 链装配

链上**无** GitWatch 槽位；能力由 builtin `workspace` 实例的 `workspace://git/ref` 资源 + 订阅提供。链序事实源是 `peri-agent/src/session/factory.rs::production_blueprint`（ARC-MIDDLEWARE-001）；`GitWatchMiddleware` 配置键是**未知键**（解析期 warn + 忽略，判例同其他已删除 middleware 键）。

## 7. 安全与信任边界

- 注入内容仅分支名与 commit hash；无 remote URL、无文件路径列表。
- **回读正文按不可信 payload 处理**：宿主侧限长 8 KiB（UTF-8 边界截断 + 截断标记），不
  进入控制状态；读取失败 / 超时回退既有通用订阅提醒（事件不丢）。

## 8. 测试策略（实现态落点）

| 用例 | 落点 |
| --- | --- |
| `parse_sample_stdout` 三态 / notice 文案 / 快照与未采样正文 | `cargo test -p peri-mcp-workspace --lib -- git_watch`（T1） |
| 节流窗口 / 单飞 / `NotRepository` 短路 / 失败不推进节流 | 同上（T2，`with_timing` 注入） |
| 真实仓库：基线 → commit → notice | 同上（T3） |
| 能力位 / `resources/list` / `read` 命中与未命中 / filter 交集 | `cargo test -p peri-middlewares --lib -- mcp::builtin_subscription_wire`（T4） |
| 工具调用 → commit → 通知 → 回读 notice 正文 | 同上（T5） |
| 会话送达（Info 不唤醒 + 字段逐一致）/ 关闭集不建立订阅 | 同上（T6） |
| 无订阅者 ⇒ 零 git 调用（正文保持未采样） | 同上（T6b） |
| 映射纯函数（字段 / 不唤醒 / 截断 / 绑定面） | `cargo test -p peri-middlewares --lib -- mcp::client::subscription`（T7） |
| 回读失败 ⇒ 通用提醒回退 | `mcp::builtin_subscription_wire`（T7b） |
| 链序无 GitWatch / 关闭面矩阵 | `cargo test -p peri-middlewares --lib -- assembly::tests`（T8） |

## 9. 验收标准

1. 链上**无** GitWatch 槽位。
2. 分支或 HEAD 变化、且在节流窗口外、且订阅活跃 → 恰好一条 `Info` 提醒，正文含
   `[Git watch]` 与 `HEAD:` 行。
3. 仅工作区变化 → 无通知。
4. 非 git 仓库 → 无通知、无持续 git 调用。
5. 无订阅者（未建立 / 被关闭）→ **零** git 调用。
6. `cargo test -p peri-mcp-workspace --lib`、`cargo test -p peri-middlewares --lib`、
   `cargo test -p peri-acp-types --lib -- meta_harness::tests` 全绿。

## 10. 设计约束

- 独立能力，不合并进 `GitAttributionMiddleware`。
- 只监视 branch + HEAD commit，**不**监视变更区域 / working tree。
- 仅在成功的 workspace 工具调用后采样，60s 节流限制实际 git 调用。
- 提示文案固定英文。

## 11. 参考实现锚点

- 采样 / 状态机 / 正文：`mcp-packages/workspace/src/git_watch.rs`
- 触发点与资源/订阅面：`mcp-packages/workspace/src/workspace.rs`
- 默认订阅注入：`peri-middlewares/src/mcp/builtin/workspace_subscription.rs`
- 宿主消费（回读 + reminder 映射）：`peri-middlewares/src/mcp/client/subscription.rs`
- 订阅建立门（关闭集）：`peri-middlewares/src/mcp/client.rs::subscription_allowed`
- Transcript：`peri-agent/src/agent/stages/mod.rs`（`append_messages_to_transcript`）
