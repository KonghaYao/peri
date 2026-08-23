# @peri-code/ptc

Perihelion Programmatic Tool Calling（PTC）的 Node adapter、wire types 与 CLI entry。当前发布版本为 `0.2.2`，package identity 为 `@peri-code/ptc@0.2.2`。

## 运行时契约

- Rust host 在缺失时使用隔离环境执行 `npm install --ignore-scripts --no-audit --no-fund --no-update-notifier --prefix <staging> @peri-code/ptc@0.2.2`，安装到 `~/.peri/ptc/0.2.2`，完整校验后原子 rename。
- 默认安装仅支持公共 registry：进程清空继承环境，只保留 `PATH`，并设置受控临时 `HOME`、npm cache 与公共 registry；不会继承 npm token、cloud 凭据或 `NODE_OPTIONS`。私有 registry 应预装缓存，或由调用方提供显式、最小且安全的配置路径，不得继承整个宿主环境。
- 缓存更新使用跨进程 lockfile；损坏目录只会在锁内原子 rename 到 quarantine，不会直接删除可能正在使用的 target。rename 冲突会重新验证并发 winner。
- adapter 直接以 `node <validated-entry>` 启动，不从仓库 `dist` 运行，也不在 Cargo 构建期间要求 Bun。
- Node 必须在接收 source 前完成 `ptc/start` handshake，并校验 protocol version 与 build identity；不匹配时 fail closed。
- package version、`periBuildId`、`periProtocolVersion`、Rust 常量与 tracked `dist` 必须同步。

## npm fallback 与供应链边界

默认运行路径会在固定版本缓存缺失或无效时执行固定版本 npm 安装。仅当安装失败且调用方显式设置：

```bash
PERI_PTC_ALLOW_NPX_FALLBACK=1
```

host 才可在固定版本 npm 安装失败后使用精确版本 `@peri-code/ptc@0.2.2` 的 `npx` fallback。fallback 同样使用 private `HOME`/cache、公共 registry、最小环境和禁用 lifecycle script 的参数；错误不会包含 token、registry source 或 npm stderr。私有 registry 默认不受支持，应预装 artifact 或使用显式安全配置，不得改为继承全环境。该 fallback 仍会引入 registry 可用性、包解析和下载链路的供应链风险，不要改为浮动版本。

## 协议与执行模型

adapter 使用 stdin/stdout 上的 NDJSON JSON-RPC。`ptc/start` protocol/build handshake 必须先于 source；随后 JavaScript 可通过 `tools.<ToolName>(input)` 向 host 发起异步工具调用。

执行环境是 ESM-only。Node module 使用动态 import：

```js
const crypto = await import('node:crypto');
```

static `import` 与 CommonJS `require` 不可用。该进程不是 sandbox；Node 原生文件系统、进程、环境变量和网络 API 不受 `tools.*` 的 Permission/HITL 约束。不要在 source、input、日志、返回值或异常中包含 secret。

## 本地验证

```bash
bun run typecheck
bun test
bun run check:dist
bun run pack:check
bun run pack:smoke
```

## 发布

在本目录确认版本、build ID、protocol version、Rust 常量与 tracked `dist` 同步后，严格按顺序执行：

```bash
bun run prepublishOnly
npm publish
```

只有 `bun run prepublishOnly` 全部成功后才能运行 `npm publish`。
