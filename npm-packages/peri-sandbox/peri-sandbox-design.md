# peri-sandbox 设计（单实例）

> 适用版本：`feat/sandbox-manager` 单实例迁移之后。旧的多 sandbox daemon、control socket、SQLite registry、request receipt、崩溃 recovery、tombstone 与全局 `list` 均已删除，不再出现在本设计中。

## 1. 目标与非目标

**目标**

1. `startSandbox(options)` 在调用者进程内启动一个 SRT/Peri backend，返回一个 `SandboxInstance`。
2. 每个实例提供 `127.0.0.1` 随机端口的 `/mcp` HTTP MCP 端点；token 每实例独立，默认只在内存。
3. workspace 固定为 `<sandboxesRoot>/<sandboxId>/{workspace,home}`，`close()` 后保留，`close({deleteData:true})` 才删除。
4. Linux 保留 `systemd-run` transient unit（cgroup 隔离）；macOS 保留 SRT + detached 进程组。
5. 与 Agent 的唯一交界是 `ChatSessionInit.mcpServers`（HTTP descriptor + Bearer）；不改 ACP/Rust。

**非目标**

- 不做多实例协调、崩溃后自动拉起、执行状态恢复、请求去重/回执、全局清单。
- 不保证稳定 URL；默认 ephemeral port。
- 不提供多租户隔离；HTTP 端点与宿主同信任边界。

## 2. 架构与数据流

```
Host process (SDK / CLI / 测试)
  SandboxManager (one per chat session)
    └─ startSandbox()
         ├─ settings/profile → random token → workspace lock
         ├─ Provider.start() ──> SRT CLI ──> peri mcp-start workspace --stdio
         ├─ PeriBackend (MCP stdio client, Tasks/limits, runId)
         └─ HTTP MCP server 127.0.0.1:<ephemeral>/mcp (Bearer, Host/Origin)
              └─ ChatSessionInit.mcpServers
                   └─ session/new|load { cwd, mcpServers }
```

1. HTTP server 运行在宿主进程内；sandbox 内只有 SRT 包裹的 Peri backend。
2. 每实例独立端口，`mcpUrl = http://127.0.0.1:<port>/mcp`，不带 sandboxId 路径。
3. `ChatSessionInit.path` 必须是宿主可见的 `<sandboxRoot>/workspace`，否则 Agent 的绝对路径会落到 SRT 拒绝的区域。
4. 一个 `SandboxManager` 对应一个会话；多个会话由宿主创建多个 manager，SDK 不维护全局表。

## 3. 模块职责

| 模块 | 职责 |
| --- | --- |
| `src/instance/types.ts` | `SandboxInstance`/`SandboxError`/options/status 类型；`McpBackendPort` |
| `src/instance/options.ts` | zod settings/profile schema、sandboxId 派生、option 归一化 |
| `src/instance/settings.ts` | settings 加载/初始化、默认 profile、Peri SHA-256 校验 |
| `src/instance/workspace.ts` | 目录、`.lock`、tokenFile、`deleteSandboxData`、free-space 检查 |
| `src/instance/instance.ts` | 状态机、`status()`、`close()`/`deleteData` |
| `src/instance/start.ts` | 启动编排、失败回卷、测试缝 `startSandboxWith` |
| `src/server/http.ts` | 单实例 HTTP server、鉴权、大小/超时限制 |
| `src/server/mcp-handler.ts` | MCP `Server`、`tools/list`、`tools/call`、`tasks/*`，pinned `2026-07-28` |
| `src/runtime/provider.ts` | Provider 接口 + `providerFor(platform)` |
| `src/runtime/systemd-provider.ts` | Linux transient unit 的 start/stop/inspect |
| `src/runtime/mac-provider.ts` | macOS 进程组 start/stop/leader 校验 |
| `src/runtime/policy.ts` | SRT `policy.json` 生成、路径/symlink 校验 |
| `src/runtime/doctor.ts` | cgroup/seccomp/SRT 探针、transient unit 属性 |
| `src/runtime/backend.ts` | MCP stdio transport、Tasks 映射、Bash 预算/并发限制 |
| `src/runtime/diagnostics.ts` | `<sandboxRoot>/diagnostics` 有界轮转日志 |
| `src/cli.ts` | `init`/`doctor`/`run` |

Provider 只负责**单个 unit/handle** 的 start/stop/inspect，不持有 registry、不枚举/认领旧进程。`doctor` 的残留 unit 列表只用于报告。

## 4. 公开 API 与 CLI

```ts
startSandbox(options?: StartSandboxOptions): Promise<SandboxInstance>;
deleteSandboxData(options: { sandboxId: string; sandboxesRoot?: string; settingsPath?: string }): Promise<void>;
initSettings(file?: string): Promise<string>;
loadSettings(file?: string): SandboxSettings;
defaultSettingsPath(): string;
```

`SandboxInstance`：`id`、`runId`、`state`、`mcpUrl`、`token`、`tokenFile`、`workspacePath`、`sandboxRoot`、`runtime`、`status()`、`close(options?)`。`status()`/错误/日志不含 token。

CLI：`init`、`doctor`、`run`。`run` 打印 `{id, mcpUrl, workspacePath, tokenFile}`，等待 SIGTERM/SIGINT 后 `close()`，不自动删除数据。旧 `up/serve/create/start/stop/destroy/inspect/list` 已删除。

## 5. 生命周期

```
starting -> ready -> stopping -> closed
     \-> failed (startup/backend exit) -> stopping -> closed
```

1. 解析 settings/options，校验 id/profile/路径/Peri hash。
2. 创建/校验 `<sandboxesRoot>/<sandboxId>/{workspace,home}`（0700），检查 symlink 与 root containment，检查剩余空间。
3. 获取 `<sandboxRoot>/.lock`（`wx`，0600）；写入 owner pid，backend spawn 后补写 backend pid。任一 pid 仍存活 → `conflict`；都确认死亡 → 清理陈旧 lock 后重试。
4. 解析 token：默认 32 字节随机 hex（仅内存）；`tokenFile` 必须在 `<sandboxRoot>` 内，`O_EXCL` 创建、0600、拒绝 symlink。
5. 写 `<sandboxRoot>/policy.json`（0600）。
6. 绑定 HTTP server（`port: 0` 默认），此阶段请求返回 503。
7. `Provider.start()` → SRT → Peri backend；backend 在 `startupTimeoutMs` 内完成 MCP 握手。
8. 返回 `ready` 实例。

失败回卷：HTTP close → backend close（stdin EOF）→ provider stop → 释放 lock。主错误与清理错误同时出现时抛 `stop_incomplete`，`cause` 为 `AggregateError`。

`close()` 语义：

| 场景 | 行为 |
| --- | --- |
| 正常 close | 关闭 HTTP/backend，provider stop 并确认退出，释放 lock，保留数据 |
| 重复 close | 直接 resolve；带 `deleteData` 时补删数据 |
| `deleteData:true` | 停止确认后删除 `<sandboxesRoot>/<sandboxId>`；失败 `delete_incomplete` |
| backend 已异常退出 | 跳过重复 kill，仍清理 HTTP/lock 并确认 provider 停止 |
| 停止未确认 | 状态 `failed`，抛 `stop_incomplete`，不删数据、不释放 lock，可重试 |
| `signal` abort | 与失败回卷相同，抛 `start_cancelled` |

## 6. 路径、锁与 token

```
<sandboxesRoot>/
  <sandboxId>/
    .lock               # ownerPid/backendPid，0600
    policy.json         # 0600；sandbox 不可读
    token               # 可选 tokenFile，0600
    diagnostics/        # 轮转日志，0700
    workspace/          # 持久工作目录，0700
    home/               # 持久 HOME/XDG，0700
```

- `sandboxId`：显式 `sbx_[a-f0-9]{32}`；或 `sessionKey` 经 `sha256("peri-sandbox-session:" + key)` 取前 32 hex；否则随机生成一次。
- macOS 临时目录 `/private/tmp/peri-<id前16位>`；Linux 由 `PrivateTmp=yes` 提供。
- lock 不写在 workspace 内，避免 Agent 误删；删除数据时在停止确认后、持锁状态下进行。

## 7. HTTP MCP

- 只监听 `127.0.0.1` 或 `::1`；Host 必须匹配绑定端口，Origin 为空或本机来源。
- 401（token 不匹配）、403（Host/Origin）、404（非 `/mcp`）、405（方法不允许）、413（请求 >1 MiB）、503（starting/stopping/failed）、500（内部）。
- 响应上限 8 MiB；`maxConnections=32`；请求/头部/连接超时 10–15 秒。
- token 比较使用 `timingSafeEqual`；401 只返回 `{error:"unauthorized"}`。
- MCP handler 固定协议 `2026-07-28`，`legacy: "reject"`；`Bash` 与 `tasks/*` 要求客户端声明 Tasks capability。

## 8. Provider

### Linux（`systemd-user-unit`）

- `systemd-run --user --pipe --wait --quiet --unit=<prefix><id>-<runId>.service`。
- Properties：`Type=exec`、`Restart=no`、`KillMode=control-group`、`OOMPolicy=continue`、`PrivateTmp=yes`、`ExecStartPre=/usr/bin/mkdir -p /tmp/claude`、`CollectMode=inactive-or-failed`、`MemoryMax`、`TasksMax`、`CPUQuota`。
- 不再有 `BindsTo=/After=` API unit；没有 daemon 可绑定。
- `stop`：`systemctl --user stop` → 等 unit inactive + cgroup `populated 0` → 超时后 `kill --signal=SIGKILL` → 再等；失败 `stop_incomplete`。
- `findActiveUnit` 只用 `systemctl list-units` 做同 id 冲突/doctor 报告，不自动 stop。

### macOS（`macos-process-group`）

- `spawn(node, [srt.cli, '--settings', policy, '--', peri, 'mcp-start', ...], { detached: true })`。
- `stop`：`ps` 校验 leader 命令行含 SRT CLI 与 policy 路径、启动时间一致后，对进程组 SIGTERM/SIGKILL，等待组消失；随后清理 `/private/tmp/peri-*`。
- 不写 process record 文件，不做 `listOwned`/`pruneStopped`。

## 9. 崩溃与父进程退出

1. 正常 `close()`：见 §5。
2. SIGINT/SIGTERM：CLI `run` 捕获并 `close()`；库本身不劫持全局信号。
3. SIGKILL/宿主崩溃：
   - macOS：stdin 管道关闭 → Peri EOF 退出 → SRT wrapper 退出；逃逸进程组可能残留（旧限制）。
   - Linux：`systemd-run --pipe` 连接 stdio；宿主死亡 → Peri EOF → unit 主进程退出 → `--collect` 卸载。若 unit 仍活，没有 registry 可对账。
4. 下次启动：陈旧 lock 按 pid 清理；Linux 发现同 id active unit → `conflict`，由人工处理；`doctor` 只报告。

不提供：`recover()`、receipt 重放、tombstone、PID 自动认领。

## 10. 风险与限制

| 风险 | 缓解 |
| --- | --- |
| 端口冲突 | 默认 port 0；显式端口 EADDRINUSE → `conflict`，不 fallback |
| 同 workspace 双写 | `.lock`（O_EXCL + pid 校验）；同 id 第二实例 conflict |
| token 泄露 | 内存/0600；日志、status、错误不含 token；Host/Origin 校验 |
| symlink/越界删除 | id 正则、realpath containment、逐级 symlink 检查、只删除受管路径 |
| backend 静默死亡 | transport close + process exit → `backend_exited`，HTTP 503；不自动重启 |
| macOS 进程组逃逸 | 文档标注为未验证，不承诺生产隔离 |
| Linux unit 残留 | `CollectMode`、`KillMode=control-group`、停后 cgroup 空确认、doctor 报告 |

## 11. 文件布局

```
src/
  cli.ts
  index.ts
  instance/{types,options,settings,workspace,instance,start}.ts
  server/{http,mcp-handler}.ts
  runtime/{provider,systemd-provider,mac-provider,policy,doctor,diagnostics,backend,srt}.ts
  util/timeout.ts
tests/
  *.test.ts               # 不启动真实 SRT 的单元测试
  integration/{mac,programmatic-mac,linux,resources}.ts  # 真实环境验收
```
