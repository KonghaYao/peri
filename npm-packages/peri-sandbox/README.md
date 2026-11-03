# peri-sandbox

在调用者进程内启动一个 SRT/Peri 沙箱，并在 `127.0.0.1` 的随机端口提供 HTTP MCP 端点。没有 daemon、control socket 或全局 registry：实例与宿主进程同生命周期，workspace 持久化在磁盘上。

`peri-sandbox` 同时是 `@peri-code/sdk/sandbox` 的官方 adapter 实现。`createPeriSandboxAdapter()` 返回结构化满足 `SandboxAdapter` 契约的对象，由宿主注入 `SandboxManager`；`peri-sandbox` 不 import SDK 类型，兼容性由 SDK 侧测试保证。adapter 是 SDK 与具体 sandbox 实现之间的交界：SDK 只定义契约与按 `sandboxId` 的资源注册表，进程、workspace 与 MCP descriptor 由本包产生。

> 开发预览。macOS 使用 SRT + 进程组；Linux 使用 `systemd-run` transient unit 保留 cgroup 的 CPU/内存/进程数限制。适用于本机开发隔离，不是生产级多租户安全边界。

## 适合什么场景

| 适合 | 不适合 |
| --- | --- |
| 在宿主进程内嵌入一个隔离的 Peri 执行环境 | 启动常驻 daemon 或控制远端主机 |
| 给 `@peri-code/sdk/sandbox` 的 `SandboxManager` 注入本地 adapter | 生产级多租户安全边界 |
| workspace 落盘、`close()` 后保留、按 id 删除 | 跨实例调度、租约或 fencing |
| 每次启动使用不同的 profile、端口或预算 | 恢复旧实例的在途请求与 Task 状态 |

## 要求

1. Node.js >= 22.12.0，macOS 或 Linux（非 root）。
2. Linux 需要可用的 systemd user session（`systemd-run --user`）；macOS 需要系统自带的 `sandbox-exec`。
3. 本机已安装 Peri（默认 `~/.peri/peri`，可用 `PERI_PATH` 指定）。
4. SRT 随 npm 依赖安装（`@anthropic-ai/sandbox-runtime` 0.0.81），无需单独安装。

## 安装

```bash
bun add peri-sandbox
# 或
npm install peri-sandbox
```

当前包仍是仓库内的 private 包；发布前请用本地路径或 workspace 链接安装。使用 SDK 集成时还需安装 `@peri-code/sdk`。

## 快速开始

```ts
import { startSandbox } from "peri-sandbox";

const sandbox = await startSandbox({
  sessionKey: "chat-session-id", // 或显式 sandboxId: "sbx_<32 hex>"
});

sandbox.id;            // sbx_...
sandbox.runId;         // run_...；每次启动不同
sandbox.state;         // "ready"
sandbox.mcpUrl;        // http://127.0.0.1:<port>/mcp
sandbox.workspacePath; // <sandboxesRoot>/<sandboxId>/workspace
sandbox.sandboxRoot;   // <sandboxesRoot>/<sandboxId>
sandbox.token;         // 只用于构造 Authorization: Bearer
sandbox.tokenFile;     // 未传 tokenFile 时为 null
sandbox.runtime;       // { provider, unit, pid }
sandbox.status();      // 当前快照，不含 token

await sandbox.close();                     // 停止实例，保留 workspace
await sandbox.close({ deleteData: true }); // 停止并删除 <sandboxRoot>
```

`startSandbox` 在返回前完成 workspace lock、HTTP 绑定、SRT/Peri 启动与 MCP 握手，因此返回时状态已是 `ready`。`SandboxInstance` 用 `workspacePath` 与 `mcpUrl` 表达 workspace 和 MCP 端点；adapter 实例才把它们投影成 `workspace` 与 `mcpServer`。

默认不写 token 文件：token 只在内存中存在。用 `mcpUrl` 和 `token` 组装 MCP 客户端配置，路径固定为 `/mcp`：

```json
{
  "mcpServers": {
    "workspace": {
      "type": "http",
      "url": "http://127.0.0.1:PORT/mcp",
      "headers": {
        "Authorization": "Bearer ${PERI_SANDBOX_TOKEN}"
      }
    }
  }
}
```

完整示例见 [`examples/peri-mcp-client.json`](examples/peri-mcp-client.json)。不要传 `0.0.0.0`：`host` 只接受 `127.0.0.1` 或 `::1`。

## 与 SDK 集成

`@peri-code/sdk/sandbox` 的 `SandboxManager` 必填 `adapter`，SDK 不提供默认实现，也不动态 import 任何具体 sandbox。manager 只做按 `sandboxId` 的注册表，不感知 session；调用方拿到 `SandboxHandle` 后自行决定绑定到哪个会话。

```ts
import { SandboxManager } from "@peri-code/sdk/sandbox";
import { createPeriSandboxAdapter } from "peri-sandbox";

const manager = new SandboxManager({
  // adapter 必填；省略 settingsPath 时使用默认 settings，
  // 默认文件不存在则回退内存默认值。
  adapter: createPeriSandboxAdapter(),
  // manager 默认值；单次 start 的 adapterOptions 整体替换它，不做字段合并。
  adapterOptions: { profileName: "workspace-default" },
  serverName: "workspace",
});

const handle = await manager.start({
  sandboxId: "sbx_0123456789abcdef0123456789abcdef",
  adapterOptions: {
    profileName: "workspace-default",
    minFreeBytes: 512 * 1024 * 1024,
  },
  startupTimeoutMs: 60_000,
  stopTimeoutMs: 10_000,
});

const mcp = await handle.mcpServer(); // { name, config }；token 只在 config.headers
const workspace = handle.workspace!;  // { id, hostPath, runtimePath }

await manager.closeAll({ deleteData: false });
```

需要固定 settings 路径或 workspace 根时，显式传给 adapter。该文件必须已存在，否则启动报 `invalid_options`。JS 不展开 `~`，用 `homedir()` + `join()` 拼接：

```ts
import { homedir } from "node:os";
import { join } from "node:path";

const adapter = createPeriSandboxAdapter({
  settingsPath: join(homedir(), ".peri-sandbox", "settings.json"),
  sandboxesRoot: join(homedir(), ".peri-sandbox", "sandboxes"),
});
```

### adapter 映射

| adapter 面 | 行为 |
| --- | --- |
| `start({ sandboxId, signal, options })` | 调用 `startSandbox({ sandboxId, signal, ...options })`；`signal` 只作用于本次启动 |
| `options` | 接受 `profileName`、`profile`、`port`、`bashCalls`、`minFreeBytes`、`startupTimeoutMs`、`stopTimeoutMs` |
| 返回 `workspace` | `{ id: sandboxId, hostPath: workspacePath, runtimePath: workspacePath }`；本地 sandbox 两者相同 |
| 返回 `mcpServer` | `{ type: "http", url: mcpUrl, headers: { Authorization: "Bearer <token>" } }` |
| 返回实例的 `close()` | 停止实例，保留 workspace；不删除持久化数据 |
| `deleteData(sandboxId)` | 调用 `deleteSandboxData`；需要显式删除数据时由调用方触发 |

adapter 不持久化 token，也不把它放进 `workspace` 或错误消息。`workspace.id` 与 `sandboxId` 相同；`runtimePath` 是 Agent/session 看到的根，本地 sandbox 与 `hostPath` 相同。

## 核心概念

### 单实例

每次 `startSandbox` 在调用者进程内创建一个实例。没有跨进程发现机制：宿主必须保留返回的实例、`sandboxId` 或稳定的 `sessionKey`，否则无法回到同一 workspace。同一 `sandboxId` 同时只能有一个实例，第二个启动返回 `conflict`；不同 `sandboxId` 可以在同一进程内并存。owner 进程已死的陈旧 lock 会在下次启动时清理。

### sandboxId 与 WorkspaceRef

1. `sandboxId` 必须是 `sbx_[a-f0-9]{32}`；传 `sessionKey` 时按 `sha256("peri-sandbox-session:" + key)` 确定性派生。
2. 两者都不传时随机生成一次。重启后要回到同一 workspace，必须自行保存 `sandboxId` 或重传相同的 `sessionKey`。
3. 目录为 `<sandboxesRoot>/<sandboxId>/{workspace,home}`，权限 0700，`close()` 后保留。
4. `policy.json`、`token`、`.lock`、`diagnostics/` 位于 `<sandboxRoot>`，不在 SRT 的 `allowRead` 中。

Workspace 是路径引用，不拥有 sandbox 或 session：

| 字段 | 含义 |
| --- | --- |
| `id` | 与 `sandboxId` 相同 |
| `hostPath` | 宿主文件系统路径；本地 sandbox 等于 `workspacePath` |
| `runtimePath` | Agent/session 看到的根；本地 sandbox 与 `hostPath` 相同 |

`startSandbox` 直接返回 `workspacePath`；SDK 的 `handle.workspace` 返回上述 `{ id, hostPath, runtimePath }`。远程 sandbox 可以省略 `hostPath`，但本包始终提供本地路径。

### MCP server 与 token

1. 端点固定为 `http://<host>:<port>/mcp`；默认 `host` 为 `127.0.0.1`，`port` 为 `0`（随机空闲端口）。
2. 显式端口被占用返回 `conflict`，不会静默换端口；因此每次启动的 URL 可能变化。
3. 默认每次启动随机生成 token（32 字节 hex），只在内存；传 `tokenFile` 时才写入 `<sandboxRoot>` 内的文件（0600，已存在则复用并校验权限）。
4. 运行时 API 中，token 只通过 `instance.token` 与 adapter 的 `mcpServer.headers.Authorization` 暴露；传 `tokenFile` 时同时写入该文件。`status()` 与错误消息都不包含 token。
5. 不要把 token 写入日志、提交到 Git 或交给不可信租户。

### Settings

settings 是可选文件，决定 `sandboxesRoot`、`periPath`、profile allowlist 等默认值。不传 `settingsPath` 且默认文件不存在时，使用内存默认配置直接启动，不需要先做任何初始化。完整规则见下文 `Settings（可选）`。

### 生命周期

```text
ready -> stopping -> closed
  \-> failed
```

1. `startSandbox` 返回时已完成 workspace lock、HTTP 绑定、SRT/Peri 启动与 MCP 握手。
2. `close()` 幂等；只有 provider 确认 unit/进程组停止后才释放 lock。
3. 停止未确认时状态为 `failed` 并抛 `stop_incomplete`，数据保留，可重试。
4. backend 非预期退出后实例转 `failed`，HTTP 返回 503；不会自动重启。
5. `close({ deleteData: true })` 在停止成功后删除 `<sandboxRoot>`；删除失败抛 `delete_incomplete`（校验类错误保留原 code），已停止的实例状态仍为 `closed`。
6. 不恢复旧执行：实例停止或宿主崩溃后，在途 MCP 请求、Task 映射和 Bash 预算全部丢失；workspace 文件保留。

`SandboxState` 联合还包含 `starting` 与 `stopped`；当前启动路径不会把未就绪的实例交给调用方。

## API

### 导出

| 导出 | 签名 | 返回 |
| --- | --- | --- |
| `startSandbox` | `(options?: StartSandboxOptions) => Promise<SandboxInstance>` | 已就绪的实例 |
| `deleteSandboxData` | `(options: { sandboxId: string; sandboxesRoot?: string; settingsPath?: string }) => Promise<void>` | `void` |
| `defaultSettingsPath` | `() => string` | `PERI_SANDBOX_SETTINGS` 或 `~/.peri-sandbox/settings.json` |
| `initSettings` | `(file?: string) => Promise<string>` | 已创建的 settings 路径 |
| `loadSettings` | `(file?: string) => SandboxSettings` | 解析并校验后的 settings |
| `createPeriSandboxAdapter` | `(options?: PeriSandboxAdapterOptions) => SandboxAdapter<PeriSandboxStartOptions>` | 结构化 adapter |
| `SandboxError` | `new SandboxError(code, message, options?)` | 带稳定 `code` 的错误 |

类型导出：`SandboxErrorCode`、`SandboxInstance`、`SandboxProfile`、`SandboxRuntimeInfo`、`SandboxSettings`、`SandboxState`、`SandboxStatus`、`StartSandboxOptions`、`PeriSandboxAdapterOptions`、`PeriSandboxStartOptions`。

### `startSandbox`

```ts
function startSandbox(options?: StartSandboxOptions): Promise<SandboxInstance>;
```

| 选项 | 默认 | 说明 |
| --- | --- | --- |
| `sandboxId` | 随机 `sbx_<32 hex>` | 必须匹配 `sbx_[a-f0-9]{32}` |
| `sessionKey` | — | 与 `sandboxId` 同时存在时以 `sandboxId` 为准；非空且不超过 1024 字节 |
| `sandboxesRoot` | settings 值 | 规范化绝对路径；覆盖 settings |
| `settingsPath` | `defaultSettingsPath()` | 显式传入时文件必须存在 |
| `profileName` | `settings.defaultProfile` 或首个 profile | 未知名称报 `invalid_options` |
| `profile` | — | 内联 profile，优先于 `profileName` |
| `token` | 随机生成 | 16–512 个无空白可打印字符；与 `tokenFile` 互斥 |
| `tokenFile` | — | 必须位于 `<sandboxRoot>` 内；新文件 0600 |
| `host` | `127.0.0.1` | 只接受 `127.0.0.1` 或 `::1` |
| `port` | `0` | 显式占用返回 `conflict` |
| `periPath` | settings 值 | 启动前校验存在且是普通文件 |
| `periSha256` | settings 值 | 与文件 SHA-256 不一致报 `unsupported_environment` |
| `bashCalls` | `10000` | 单实例 Bash 调用预算 |
| `startupTimeoutMs` | `45000` | settings 可提供默认值，per-start 覆盖 |
| `stopTimeoutMs` | `15000` | settings 可提供默认值，per-start 覆盖 |
| `minFreeBytes` | 1 GiB | 启动前检查 `sandboxesRoot` 剩余空间 |
| `signal` | — | 取消启动；触发 `start_cancelled` 并清理半成品 |

可能抛出的错误：`unsupported_environment`、`invalid_options`、`token_unreadable`、`conflict`、`start_failed`、`start_cancelled`、`stop_incomplete`。

### `deleteSandboxData`

```ts
function deleteSandboxData(options: {
  sandboxId: string;
  sandboxesRoot?: string;
  settingsPath?: string;
}): Promise<void>;
```

删除 `<sandboxesRoot>/<sandboxId>`；目录不存在是 no-op。不会跟随 symlink，也不会越出 `sandboxesRoot`。本函数不检查实例是否仍在运行，应在实例停止后调用。失败抛 `delete_incomplete`；路径或权限校验类错误保留 `conflict` 等原 code。

### `initSettings` / `loadSettings` / `defaultSettingsPath`

```ts
function defaultSettingsPath(): string;
function initSettings(file?: string): Promise<string>;
function loadSettings(file?: string): SandboxSettings;
```

1. `defaultSettingsPath()` 读取 `PERI_SANDBOX_SETTINGS`，否则返回 `~/.peri-sandbox/settings.json`（结果已 resolve）。
2. `initSettings()` 创建 `<settings 目录>/settings.json`（0600）与同目录的 `sandboxes/`（0700），写入 Peri 路径与 SHA-256。已存在时抛 `conflict`；平台或 Peri 不可用抛 `unsupported_environment`。
3. `loadSettings()` 不传参数且默认文件不存在时返回内存默认值；显式路径不存在、文件无法解析、`defaultProfile` 未知，或加载到的 `sandboxesRoot` 已存在但不是私有真实目录时抛 `invalid_options`。

### `createPeriSandboxAdapter`

```ts
function createPeriSandboxAdapter(options?: {
  settingsPath?: string;
  sandboxesRoot?: string;
}): SandboxAdapter<PeriSandboxStartOptions>;
```

| 选项 | 说明 |
| --- | --- |
| `settingsPath` | 使用的 settings 文件；可与 `sandboxesRoot` 同时提供 |
| `sandboxesRoot` | 部署级根目录，覆盖 settings 中的值 |

返回对象满足 `SandboxAdapter`：`start()` 启动一个实例；`deleteData()` 按 id 删除数据。adapter 构造不校验文件；错误在首次 `start()` / `deleteData()` 时以 `SandboxError` 抛出。`SandboxAdapter` 是 `@peri-code/sdk/sandbox` 的契约类型，本包不导出它；本包只导出 `PeriSandboxAdapterOptions` 与 `PeriSandboxStartOptions`。

### `SandboxInstance`

| 成员 | 类型 | 说明 |
| --- | --- | --- |
| `id` | `string` | `sandboxId` |
| `runId` | `string` | 每次启动生成的新 `run_...` |
| `state` | `SandboxState` | 当前生命周期状态 |
| `mcpUrl` | `string` | `http://<host>:<port>/mcp` |
| `token` | `string` | 敏感值；只用于构造 Authorization |
| `tokenFile` | `string \| null` | 未使用 token 文件时为 `null` |
| `workspacePath` | `string` | `<sandboxesRoot>/<sandboxId>/workspace` |
| `sandboxRoot` | `string` | `<sandboxesRoot>/<sandboxId>` |
| `runtime` | `SandboxRuntimeInfo` | `{ provider, unit, pid }`；macOS 的 `unit` 为 `null` |
| `status()` | `() => SandboxStatus` | 快照；含 `lastError`，不含 token |
| `close(options?)` | `(options?: { deleteData?: boolean }) => Promise<void>` | 幂等停止；可保留或删除数据 |

`SandboxStatus` 额外提供 `lastError: { code, message } | null`，用于在不抛异常时读取失败原因。`close()` 可能抛 `stop_incomplete`；`deleteData: true` 时还可能抛 `delete_incomplete` 或校验类 `conflict`。

### 错误码

| code | 触发场景 |
| --- | --- |
| `unsupported_environment` | 平台、Peri、SRT、systemd/cgroup 不可用，或 runtime 路径与受管目录重叠 |
| `invalid_options` | id/sessionKey/端口/超时/token 非法，显式 settings 缺失或不可解析，或加载到的 settings 中 `sandboxesRoot` 不是私有真实目录 |
| `token_unreadable` | `tokenFile` 不是普通文件、权限不是 0600、无法读取或内容非法 |
| `conflict` | 同 id 已有实例、残留 unit、显式端口被占用、`sandboxesRoot` 不是私有真实目录、`init` 目标已存在、删除路径不安全 |
| `start_failed` | backend 未在启动预算内就绪、剩余空间不足、缺少必要工具 |
| `start_cancelled` | `signal` 在启动完成前触发 |
| `stop_incomplete` | 停止未被 provider 确认，实例转 `failed` |
| `backend_exited` | backend 非预期退出；也作为 HTTP 请求被中断的内部原因 |
| `delete_incomplete` | 删除持久化数据失败 |
| `limit_exceeded` | stdio 缓冲、并发、Task 映射或 Bash 预算超限 |

`SandboxErrorCode` 联合还包含 `closed`；当前单实例 API 不主动抛出该 code。

## Settings（可选）

不传 `settingsPath` 时启动路径不依赖任何初始化，具体分三种情况：

1. 默认路径 `~/.peri-sandbox/settings.json`（可用 `PERI_SANDBOX_SETTINGS` 覆盖）不存在：回退到内存默认配置，不写磁盘，可直接启动。默认 `sandboxesRoot` 为 `~/.peri-sandbox/sandboxes`，`periPath` 取 `PERI_PATH` 或 `~/.peri/peri`，默认 profile 为 `workspace-default`。
2. 显式传入 `settingsPath` 但文件不存在：抛 `invalid_options`，不会用内存默认值掩盖路径错误。
3. 默认路径存在：加载该文件。文件无法解析，或其中的 `sandboxesRoot` 已存在但不是私有的真实目录（非 symlink、权限 0700）时，抛 `invalid_options`。

以上路径都要求 Peri 可被发现：缺失、不是普通文件，或与 settings 记录的 SHA-256 不一致时报 `unsupported_environment`。

`initSettings()` 只做环境固定：写入 `settings.json`（0600）、创建 `sandboxes/`（0700）、把 `periPath` 与 SHA-256 固化进文件、校验目录私有性。它不生成 token、端口或后台服务，也不覆盖已有文件（已存在时报 `conflict`）。需要固定路径/哈希或自定义配置时再使用：

```ts
import { homedir } from "node:os";
import { join } from "node:path";
import { initSettings } from "peri-sandbox";

const settingsPath = await initSettings(
  join(homedir(), ".peri-sandbox", "settings.json"),
);
```

settings 文件结构：

```json
{
  "sandboxesRoot": "/Users/you/.peri-sandbox/sandboxes",
  "periPath": "/Users/you/.peri/peri",
  "periSha256": "<64 hex>",
  "defaultProfile": "workspace-default",
  "unitPrefix": "peri-sbx-",
  "profiles": {
    "workspace-default": {
      "allowedDomains": [
        "github.com",
        "api.github.com",
        "registry.npmjs.org",
        "api.anthropic.com",
        "api.openai.com"
      ],
      "runtimeReadPaths": ["/usr", "/bin", "/opt/homebrew"],
      "memoryMax": "1G",
      "tasksMax": 128,
      "cpuQuota": "100%"
    }
  }
}
```

| 字段 | 说明 |
| --- | --- |
| `sandboxesRoot` | 受管根目录；规范化绝对路径 |
| `periPath` | Peri 可执行文件；规范化绝对路径 |
| `periSha256` | 启动前比对的 SHA-256 |
| `defaultProfile` | 默认 profile 名；必须存在于 `profiles` |
| `unitPrefix` | Linux transient unit 前缀，默认 `peri-sbx-` |
| `profiles` | 至少一个 profile；SDK/API 可用 `profileName` 或内联 `profile` 覆盖 |
| `startupTimeoutMs` / `stopTimeoutMs` | 可选默认预算；per-start 参数优先 |

`allowedDomains` 只填域名，不支持 URL 或 `*`。`runtimeReadPaths` 必须存在且不能覆盖 `sandboxesRoot`。`memoryMax`、`tasksMax`、`cpuQuota` 只在 Linux 生效。SRT policy 在每次启动时重新生成，因此修改 profile 后需重启实例；同一 workspace 会在新 policy 下启动。

`settings.json` 中的路径必须是规范化绝对路径（示例中的 `/Users/you/...` 是占位符）：加载时不展开 `~`，写 `"~/.peri-sandbox/sandboxes"` 会因不是绝对路径而报 `invalid_options`。

## 安全、隔离与平台支持

### 隔离边界

1. 文件系统：SRT policy 设置 `denyRead: ["/"]`，`allowRead` 仅包含 profile 的 `runtimeReadPaths`、SRT 包目录、Peri 可执行文件、workspace/home/tmp；workspace 与 home 可写。
2. `<sandboxRoot>` 下的 `policy.json`、`token`、`.lock`、`diagnostics/` 不在 `allowRead` 中；宿主进程本身仍能读取。
3. 网络：按域名严格 allowlist，`strictAllowlist: true`，不允许 URL 或通配符，Unix socket 默认拒绝。
4. Linux 用 cgroup 提供 CPU/内存/进程数硬限制；macOS 没有等价硬限制，double-fork/setsid 后代可能逃逸进程组。
5. token 默认只在内存；不要写入日志、提交到 Git 或交给不可信租户。
6. 本包提供本机开发隔离，不承诺生产级多租户安全边界；需要强隔离时使用独立 VM 或容器。

### 平台差异

| 平台 | 机制 | 已知限制 |
| --- | --- | --- |
| macOS | SRT + `sandbox-exec` + detached 进程组 | 没有 cgroup 等价的 CPU/内存/进程数硬限制；后代可能逃逸进程组 |
| Linux | `systemd-run --user` transient unit（`KillMode=control-group`、`PrivateTmp=yes`、`CollectMode=inactive-or-failed`），按 profile 设置 cgroup 限制 | 宿主被 SIGKILL 后 unit 依赖 stdio EOF 退出；若 unit 仍存活，下次启动返回 `conflict`，需人工 `systemctl --user stop` |

`doctor` 只报告残留 `peri-sbx-*` unit 与陈旧 `.lock`，不自动清理。Windows 不支持。

## CLI（附录）

CLI 是同一套 API 的薄封装；正常集成优先使用编程式 API。

| 命令 | 作用 |
| --- | --- |
| `peri-sandbox init [--settings /absolute/settings.json]` | 写入 settings（0600）与 `sandboxes/`（0700），固化 Peri 路径与 SHA-256 |
| `peri-sandbox doctor [--settings /absolute/settings.json]` | 检查平台、Peri hash、SRT/seccomp、cgroup 或 `sandbox-exec`；只报告，不清理 |
| `peri-sandbox run [--settings ...] [--sandbox-id sbx_... \| --session-key key] [--port n] [--profile name]` | 启动一个实例，写 `<sandboxRoot>/token`（0600），打印 `{ id, mcpUrl, workspacePath, tokenFile }`，前台等待 SIGTERM/SIGINT |

`run` 是唯一默认把 token 写入文件的入口；编程式 API 只有显式传 `tokenFile` 才落盘。

## 开发与验证

```bash
bun install
bun run format:check && bun run check && bun run build && bun run test
bash scripts/macos-acceptance.sh   # Apple Silicon + OrbStack，Linux VM 验收
```

`bun run test:mac -- /absolute/settings.json` 与 `bun run test:mac:programmatic` 需要真实的 Peri/SRT。已实测的版本与边界见 [`COMPATIBILITY.md`](COMPATIBILITY.md)，架构说明见 [`peri-sandbox-design.md`](peri-sandbox-design.md)。

## 参考

- [SDK 沙箱生命周期](../@peri-sdk/README.md)
- [设计说明](peri-sandbox-design.md)
- [兼容性与验收记录](COMPATIBILITY.md)
- [MCP 客户端示例](examples/peri-mcp-client.json)
