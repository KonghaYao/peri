# Git Watch 中间件

> **状态**：已批准目标设计（已实现于 `peri-middlewares/src/git_watch/`）
>
> **事实源（实现后）**：`peri-middlewares/src/git_watch/`、`peri-agent/src/session/factory.rs::production_blueprint`、`peri-middlewares/src/assembly.rs`
>
> **关联契约**：ARC-MIDDLEWARE-001、[middleware-system.md](middleware-system.md)、[message-transcript.md](message-transcript.md)

## 1. 问题与目标

### 1.1 背景

主 Agent 在 `07_runtime` 中仅冻结「是否在 git 仓库」与 cwd 快照；会话进行中**分支切换**或 **HEAD 前进**（新 commit）不会自动进入模型上下文。用户或外部进程在会话外 `checkout` / `commit` / `pull` 时，模型可能基于过时的分支或 commit 假设执行不可逆 git 操作。

`GitAttributionMiddleware` 负责 Write/Edit 贡献与 Co-Authored-By 的 `prompt_contribution`，**不**承担仓库拓扑监视。

### 1.2 目标（已对齐产品意向）

| 项 | 决策 |
| --- | --- |
| 监视维度 | **仅**当前分支名 + **HEAD commit**（完整 hash） |
| 不监视 | 工作区 / 暂存区 / untracked（避免 Agent 自身 Write/Edit 造成误报） |
| 中间件形态 | **独立** `GitWatchMiddleware`（MetaHarness 可单独关闭） |
| 提示语言 | **固定英文** Info 文案 |
| 注入方式 | `MessageKind::Info` + `MessageSource::SystemInjected`（transcript 层统一 `<system-reminder>`） |
| 非 git | 探测一次 → 本会话 `NotRepository`，后续零 git 调用 |

### 1.3 非目标

- 不跑 `git status` / porcelain；不报告 dirty files。
- 不 inotify、不后台线程轮询。
- 不向 SubAgent 链装配。
- 不修改 `GitAttributionMiddleware`。

## 2. 触发与节流（核心）

### 2.1 设计意图

- **每个工具执行完成后**都应有机会发现「外部导致的」分支或 commit 变化（例如在 `Bash` 里 `git pull`、`checkout`）。
- 若在每个 hook 点都真正调用 git，高频工具 回合会产生大量子进程；因此采用 **1 分钟节流**：同一 middleware 实例上，两次**实际 git 采样**之间至少间隔 **60 秒**（墙钟）。

### 2.2 触发入口（建议默认，见 §12）

| Hook | 行为 |
| --- | --- |
| `after_tool` | 每次工具**成功返回后**调用 `try_sample_and_notify(state)` |
| `before_agent` | 每轮 ReAct **开始前**同样调用（覆盖「本轮无工具、仅文本回复」时用户在外部改分支/commit 的场景） |

二者共享**同一会话实例**上的：

- `last_sample_at: Option<Instant>` — 上次**完成** git 采样的时刻
- `last_snapshot: Option<GitSnapshot>`

**节流算法**（`try_sample_and_notify`）：

1. 若 `RepoMode::NotRepository` → return。
2. 若 `last_sample_at` 存在且 `elapsed < 60s` → return（**不**采样、**不**通知）。
3. 否则执行采样（§4）；更新 `last_sample_at`。
4. 若尚无 `last_snapshot` → 仅存快照，**不** Info。
5. 若 `branch` 或 `head` 与 `last_snapshot` 不同 → 推送 Info（§5），再更新 `last_snapshot`。
6. 若相同 → 仅更新 `last_snapshot`（与当前一致，幂等）。

> **讨论点**：若你希望**严格**「只有 tool 后才检测」，可去掉 `before_agent` 入口；代价是纯文本回合与「仅用户发下一条消息」之间，外部 git 变化最多延迟到下一次 `after_tool` 或下一条用户消息对应的 `before_agent`。

### 2.3 与「每个 tool 触发」的关系

- **每个** `after_tool` 都会**进入**节流逻辑，但多数调用在 60s 窗口内**立即返回**，无子进程。
- 窗口外的第一次调用才真正执行 `git rev-parse`（§4）。

### 2.4 失败与超时

- 单次采样超时预算 **1s**；失败或超时：**不**更新 `last_snapshot`、**不**推送、**不**推进 `last_sample_at`（下次 hook 可重试；避免「空采样却锁住 60s」——**待 §12 Q2 确认**）。

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
        → 确认非 git   → NotRepository（会话内短路）
```

非 git：`git rev-parse --is-inside-work-tree` 不为 `true`（含超时/失败策略与 §4 一致）。

## 4. Git 采样（性能）

- 环境：`GIT_OPTIONAL_LOCKS=0`。
- **不**调用 `git status`。
- 推荐单次 spawn（示例）：

```bash
git rev-parse --is-inside-work-tree &&
git rev-parse HEAD &&
git rev-parse --abbrev-ref HEAD
```

- 非仓库：第一条失败即设 `NotRepository`，不再 spawn。
- 超时 **1s**；异步 `tokio::process::Command`，`current_dir = state.cwd()`。

## 5. Info 文案（固定英文）

纯文本，不含外层 `<system-reminder>`：

```text
[Git watch] Repository ref changed since the last sample:
- Branch: {prev} → {curr}     (omit line if unchanged)
- HEAD: {prev_short} → {curr_short}   (omit line if unchanged)

Sampled after a tool run or turn start. Run `git status` and `git log -1` before irreversible git operations.
```

- HEAD 展示 **7 字符**短 hash。
- 若仅一项变化，只列对应 bullet。

## 6. 链装配

- `ChainSlot::GitWatch` 位于 `GitAttribution` 与 `Terminal` 之间。
- `name()`：`GitWatchMiddleware`。
- Hooks：`before_agent` + `after_tool`（无 `prompt_contribution`）。
- Workflow agent 路径：与主链一致（与 `GitAttribution` 同路径装配）。

## 7. 安全

- 注入内容仅分支名与 commit hash；无 remote URL、无文件路径列表。

## 8. 测试策略

| 用例 | 期望 |
| --- | --- |
| 非 git cwd | 无 Info；第二次 hook 无 git 子进程 |
| 首采样 | 无 Info |
| `checkout` 另一分支 | 节流窗口外 → 一条 Info，含 Branch 行 |
| `commit` | HEAD 变 → Info 含 HEAD 行 |
| Agent 仅 Write 改文件 | **无** Info（不监视 working tree） |
| 60s 内多次 `after_tool` | 至多一次 git 采样 |
| `disabled: GitWatchMiddleware` | 链上无实例 |

## 9. 验收标准（实施后）

1. 默认主链含 `GitWatchMiddleware`，位于 `GitAttribution` 之后。
2. 分支或 HEAD 变化且在节流窗口外 → 恰好一条 Info，transcript 含 `[Git watch]`。
3. 仅工作区变化 → 无 Info。
4. 非 git → 无 Info、无持续 git 调用。
5. `cargo test -p peri-middlewares --lib git_watch` 与 blueprint 契约测试通过。

## 10. 已确认（讨论记录）

| 日期 | 结论 |
| --- | --- |
| 2026-09-02 | 独立 middleware，不合并 GitAttribution |
| 2026-09-02 | 只监视 branch + commit，**不**监视变更区域 / working tree |
| 2026-09-02 | 工具完成后参与检测，**1 分钟节流**限制实际 git 调用 |
| 2026-09-02 | 提示文案固定英文 |

## 11. 参考实现锚点

- Info 注入：`peri-middlewares/src/mcp/middleware.rs`（`push_status_changes`）
- `after_tool` 链：`peri-agent/src/middleware/chain.rs`
- Transcript：`peri-agent/src/agent/stages/mod.rs`（`append_messages_to_transcript`）

## 12. 待你确认（实现前）

| ID | 问题 | 建议 |
| --- | --- | --- |
| **Q1** | 除 `after_tool` 外，是否保留 **`before_agent`** 同一节流？（覆盖无工具回合） | **保留** |
| **Q2** | 采样**失败/超时**时是否推进 `last_sample_at`？ | **不推进**（尽快重试） |
| **Q3** | `after_tool` 是否仅在 **Ok** 结果时尝试？（工具报错仍采样） | **无论成败都尝试**（外部 git 可能与工具失败无关） |
| **Q4** | 节流 60s 从「上次采样**开始**」还是「**结束**」计时？ | **结束**时刻 |

你回复 Q1–Q4（或「全部采用建议」）后，可将本文状态改为「已批准目标设计」并开始实现。
