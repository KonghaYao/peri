# peri-sandbox

在调用者进程内启动**一个** SRT/Peri 沙箱，并在 `127.0.0.1` 的随机端口提供 HTTP MCP 端点。没有 daemon、control socket 或全局 registry：实例与宿主进程同生命周期，workspace 持久化在磁盘上。

> 开发预览。macOS 使用 SRT + 进程组；Linux 使用 `systemd-run` transient unit 保留 cgroup 的 CPU/内存/进程数限制。适用于本机开发隔离，不是生产级多租户安全边界。

## 要求

- Node.js >= 22.12.0，macOS 或 Linux（非 root）。
- 本机已安装 Peri（默认 `~/.peri/peri`，可用 `PERI_PATH` 指定）。
- SRT 随 npm 依赖安装，无需单独安装。

## CLI

```sh
npx peri-sandbox init   [--settings /absolute/settings.json]
npx peri-sandbox doctor [--settings /absolute/settings.json]
npx peri-sandbox run    [--settings ...] [--sandbox-id sbx_... | --session-key key] [--port n] [--profile name]
```

1. `init` 创建 `~/.peri-sandbox/settings.json`（0600）与 `sandboxes/`（0700），写入 Peri 路径和 SHA-256。不会生成 token、端口或后台服务。
2. `doctor` 检查平台、Peri hash、SRT 0.0.81/seccomp、cgroup（Linux）或 `sandbox-exec`（macOS），并**只报告**残留 `peri-sbx-*` unit 与陈旧 `.lock`，不自动清理。
3. `run` 启动一个实例，打印握手 JSON 后前台等待 SIGTERM/SIGINT：

```json
{
  "id": "sbx_...",
  "mcpUrl": "http://127.0.0.1:PORT/mcp",
  "workspacePath": "/Users/you/.peri-sandbox/sandboxes/sbx_.../workspace",
  "tokenFile": "/Users/you/.peri-sandbox/sandboxes/sbx_.../token"
}
```

token 每实例随机生成，默认只在内存；`run` 会写 `<sandboxRoot>/token`（0600）供客户端读取。不要把 token 写入日志、提交到 Git 或交给不可信租户。

MCP 客户端示例见 `examples/peri-mcp-client.json`（路径固定为 `/mcp`，不同实例端口不同）。

## 程序内 API

```ts
import { startSandbox } from 'peri-sandbox';

const sandbox = await startSandbox({
  sessionKey: 'chat-session-id', // 或 sandboxId
  // settingsPath / sandboxesRoot / profileName / profile / port / tokenFile 可选
});

sandbox.mcpUrl;        // http://127.0.0.1:<port>/mcp
sandbox.token;         // 仅用于构造 Authorization: Bearer
sandbox.workspacePath; // <sandboxesRoot>/<sandboxId>/workspace
sandbox.status();      // 不含 token

await sandbox.close();                  // 停止实例，保留 workspace
await sandbox.close({ deleteData: true }); // 停止并删除 <sandboxRoot>
```

`deleteSandboxData({ sandboxId, sandboxesRoot })` 可单独删除一个已停止的 sandbox 目录；不会跟随 symlink，也不会越出 `sandboxesRoot`。

### 生命周期

```
starting -> ready -> stopping -> stopped/closed
     \-> failed
```

- `startSandbox` 在返回前完成 workspace lock、HTTP 绑定、SRT/Peri 启动与 MCP 握手。
- 同一 `sandboxId` 同时只能有一个实例：第二个启动返回 `conflict`；owner 进程已死的陈旧 lock 会被清理。
- 显式端口被占用返回 `conflict`，不会静默换端口；默认 `port: 0`，每次启动 URL 可能变化。
- `close()` 幂等；只有 provider 确认 unit/进程组停止后才释放 lock。清理未确认时状态为 `failed` 且抛 `stop_incomplete`，不删除数据，可重试。
- backend 非预期退出后实例转 `failed`，HTTP 返回 503；不会自动重启。
- `close({ deleteData: true })` 失败抛 `delete_incomplete`，数据保留。

### sandboxId 与 workspace

- `sandboxId` 必须是 `sbx_[a-f0-9]{32}`；也可传 `sessionKey`，经 `sha256("peri-sandbox-session:" + key)` 确定性派生。
- 两者都不传时在实例构造时随机生成一次；宿主需自行保存返回值或传入稳定的 `sessionKey`/`sandboxId`，否则重启后无法回到同一 workspace。
- 目录：`<sandboxesRoot>/<sandboxId>/{workspace,home}`，权限 0700，`close()` 后保留。
- `policy.json`、`token`、`.lock`、`diagnostics/` 位于 `<sandboxRoot>`，不在 SRT 的 allowRead 中。

## 设置与权限

`settings.json` 的 `profiles` 决定 allowlist 与运行时读取路径；`defaultProfile` 指定默认项。SDK/API 可传 `profileName` 或内联 `profile`（内联优先）。

```json
{
  "sandboxesRoot": "/Users/you/.peri-sandbox/sandboxes",
  "periPath": "/Users/you/.peri/peri",
  "periSha256": "<64 hex>",
  "defaultProfile": "workspace-default",
  "profiles": {
    "workspace-default": {
      "allowedDomains": ["github.com", "api.github.com", "registry.npmjs.org"],
      "runtimeReadPaths": ["/usr", "/bin", "/opt/homebrew"],
      "memoryMax": "1G",
      "tasksMax": 128,
      "cpuQuota": "100%"
    }
  }
}
```

`allowedDomains` 只填域名，不支持 URL 或 `*`。`runtimeReadPaths` 必须存在且不能覆盖 `sandboxesRoot`。SRT policy 在每次启动时重新生成，因此修改 profile 后需重启实例（同一 workspace 会在新 policy 下启动）。`memoryMax`/`tasksMax`/`cpuQuota` 只在 Linux 生效。

## 平台边界

- **macOS**：SRT + `sandbox-exec` + detached 进程组。没有 cgroup 等价的 CPU/内存/进程数硬限制；double-fork/setsid 后代可能逃逸进程组，不承诺生产隔离。
- **Linux**：`systemd-run --user` transient unit（`KillMode=control-group`、`PrivateTmp=yes`、`CollectMode=inactive-or-failed`），按 profile 设置 cgroup 限制。宿主被 SIGKILL 后 unit 依赖 stdio EOF 退出；若 unit 仍存活，下次启动返回 `conflict`，由人工 `systemctl --user stop`，`doctor` 只报告不清理。
- 不恢复旧执行：实例停止/宿主崩溃后，在途 MCP 请求、Task 映射和 Bash 预算全部丢失；workspace 文件保留。

## 验证

```sh
bun install
bun run format:check && bun run check && bun run build && bun run test
bash scripts/macos-acceptance.sh   # Apple Silicon + OrbStack，Linux VM 验收
```

`bun run test:mac -- /absolute/settings.json` 与 `bun run test:mac:programmatic` 需要真实的 Peri/SRT。已实测的版本与边界见 `COMPATIBILITY.md`，架构说明见 `peri-sandbox-design.md`。
