# peri-sandbox

在 **macOS** 上为 Peri 创建独立的 Workspace，并提供本机 MCP 地址。停止服务不会删除 Workspace 文件；下次启动继续使用原来的 Workspace 和地址。

> 当前为开发预览。macOS 版使用 SRT 和本地进程组管理，不具备 Linux cgroup 的 CPU、内存和进程数硬限制；不要将它当成生产级多租户安全隔离。

## 安装

正式发布到 npm 后，在 macOS 的 Node 项目中运行 `npm install peri-sandbox`（当前尚未发布，可从本仓库用 `npm pack` 生成 tarball 试用）。

## 启动

需要 Node.js >= 22.12.0 和本机已安装的 Peri；SRT 随 `peri-sandbox` 的 npm 依赖自动安装，不需要单独安装或配置。如果 Peri 已在 `~/.peri/peri`，无需再处理；否则启动前设置 `PERI_PATH` 为它的绝对路径。

```sh
npx peri-sandbox up
```

首次运行会在 `~/.peri-sandbox/` 自动创建私有配置、访问令牌和 Workspace，启动一个默认 Sandbox，并打印 `mcpUrl` 与 `tokenFile`。**保持这个终端运行**；按 `Ctrl+C` 停止服务，下次再运行同一命令即可恢复文件；若原端口仍可用，MCP 地址也保持不变。

将打印的 MCP 地址和令牌填入支持远程 HTTP MCP 的客户端，例如：

```json
{
  "mcpServers": {
    "workspace": {
      "url": "http://127.0.0.1:端口/sandboxes/sbx_你的ID/mcp",
      "headers": { "Authorization": "Bearer 你的令牌" }
    }
  }
}
```

将 `url` 换成输出的 `mcpUrl`，令牌内容从输出的 `tokenFile` 读取；**不要把令牌提交到 Git**。不同客户端的配置格式可能不同，示例见 `examples/peri-mcp-client.json`；Peri 客户端的自动发现流程尚未端到端验证。默认只监听本机 `127.0.0.1`，不对局域网开放。

Workspace MCP 透传 Peri 后端公布的工具；当前版本提供 `Read`、`Write`、`Edit`、`Glob`、`Grep`、`folder_operations` 和 `Bash`。

## 调整默认权限

首次运行生成的配置位于 `~/.peri-sandbox/control/config.json`（自己传入 `configPath` 时以该路径为准）。macOS 的 `profiles.workspace-default` 默认可读取系统运行目录、`/opt/homebrew`、`/usr/local` 和当前 Sandbox 的 Workspace/Home，可使用这些目录中的命令；出站网络默认允许 `github.com`、`api.github.com`、`registry.npmjs.org`、`api.anthropic.com`、`api.openai.com`。这是方便本机开发的配置，不等于允许访问整个磁盘或互联网；写入仍限于该 Sandbox 的 Workspace/Home/临时目录。

停止服务后打开配置文件，编辑 `profiles.workspace-default` 下的两个数组。下面演示追加一个域名和一个只读目录；原有运行时路径（尤其是 Peri 的实际路径）需要保留：

```json
{
  "allowedDomains": ["github.com", "api.github.com", "registry.npmjs.org", "api.anthropic.com", "api.openai.com", "example.com"],
  "runtimeReadPaths": ["/usr", "/bin", "/sbin", "/opt/homebrew", "/usr/local", "/System", "/Library", "/private/etc", "/你的/工具/绝对路径", "/原有的/peri/绝对路径"]
}
```

上面只是 profile 的字段示例，**不要用它替换整个配置文件**，并将示例中的 `/你的/工具/绝对路径` 换为真实存在的路径，保留配置中原有的 Peri 路径。`allowedDomains` 填域名，不填 URL 或 `*`；删掉某个域名可收紧其网络权限。添加新工具目录后可以在 Bash 中用绝对路径执行命令；如需直接使用命令名，还要让目录位于沙箱的 `PATH` 中（默认包含 Homebrew、`/usr/local`、`/usr` 和 `/bin` 的 `bin`/`sbin` 目录）。不要将令牌或其他私有目录加入只读列表。

保存后重新运行 `npx peri-sandbox up`。**profile 的权限在创建 Sandbox 时就已固定**：改配置并重启不会更改现有 Sandbox 的权限；运行 `npx peri-sandbox create --name updated --profile workspace-default` 再 `npx peri-sandbox start sbx_新ID`，新实例才使用新配置。旧 Workspace 保留，不会自动搬迁或删除；如需继续用旧 Workspace，请勿误删它。

## 在程序中启动

无需运行 `npx peri-sandbox up`，macOS 的 Node.js 程序可直接启动并管理服务：

```ts
import { startPeriSandbox } from 'peri-sandbox';

const session = await startPeriSandbox();
console.log(session.mcpUrl, session.tokenFile);

await session.close();
```

首次调用同样自动创建配置和默认 Sandbox；需要隔离不同项目时传入 `{ configPath: '/绝对路径/control/config.json' }`。`session.id` 是默认 Sandbox ID；`session.close()` 停止此配置下的所有 Sandbox，并释放服务端口，再次启动可恢复原来的 Workspace；MCP 地址在原端口可用时保持不变。请在程序退出前调用 `close()`；程序异常退出时下次启动也会尝试恢复。

首次初始化会选择本机空闲端口并写入配置；之后优先复用该端口。如果它被其他程序占用，服务会直接监听系统分配的新端口，`session.mcpUrl` 会相应变化；释放占用后再次启动会优先恢复原端口。不要缓存旧地址，请始终使用本次启动返回的 `mcpUrl`。同一配置已在运行时，重复启动会报 `conflict`，不会影响现有服务。

## 管理多个 Workspace

`up` 已经创建并启动默认 Workspace。需要更多 Workspace 时，在**另一个终端**中运行：

```sh
npx peri-sandbox create --name project-b
npx peri-sandbox start sbx_从create输出取得的ID
npx peri-sandbox list
npx peri-sandbox stop sbx_你的ID
```

只有确认不再需要文件时，才运行 `npx peri-sandbox destroy sbx_你的ID --delete-data`。

TypeScript 项目也能从包入口调用本机管理 socket：

```ts
import { homedir } from 'node:os';
import { join } from 'node:path';
import { connectSandboxControl, newRequestId } from 'peri-sandbox';

const control = connectSandboxControl({ socketPath: join(homedir(), '.peri-sandbox/control/control.sock') });
const sandbox = await control.create({ name: 'project-b', profile: 'workspace-default' }, { requestId: newRequestId() });
console.log((await control.start(sandbox.id, { requestId: newRequestId() })).mcpUrl);
```

Linux 版设计与各平台已验证的功能边界见 `peri-sandbox-design.md`、`COMPATIBILITY.md`。开发使用 Bun 安装依赖（`bun install`）；可运行 `bun run format` 自动格式化。完整验证可运行 `bun run format:check && bun run check && bun run build && bun run test`；运行 `bun run test:mac:programmatic` 验证程序启动，或启动本机服务后运行 `bun run test:mac` 验证实际 MCP 调用。
