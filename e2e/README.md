# Peri E2E

完整测试命令以 [测试规范](../docs/standards/testing.md#82-peri-tui-e2e分层门禁) 为准。
持久模型录放的业务范围见 [场景目录](fixtures/replay-v2/README.md)。

## 离线业务回放

在仓库根目录构建真实 Peri，再进入 `e2e/` 安装依赖。需要 Node、Python 3 和 Unix PTY；
终端画面由 `@xterm/headless` 解析。`PERI_E2E_BINARY` 可指定已构建的二进制。

```bash
bash scripts/cargo-rmcp-patched.sh build --locked -p peri-tui --bin peri
cd e2e
npm ci
npm run e2e:replay:check
npm run e2e:replay:unit
npm run e2e:replay
```

默认只读取封存 fixture。缺录制、配置签名变化、请求漂移、额外/遗漏调用或错误终态都会失败。
回放不会启动 fixture 上游，也不会调用线上模型。实际 Agent、工具、MCP、ACP、存储和 TUI 继续运行。
每次使用新 HOME/work；通过真实 ACP 初始化新的会话，并用已声明的因果身份绑定动态 ID。
默认回放以 3 个 worker 并行执行测试文件，文件内场景串行；显式录制保持单 worker。
排查顺序问题可执行 `npm run e2e:replay -- --maxWorkers 1 --no-file-parallelism`。

更新录制必须显式执行：

```bash
npm run e2e:replay:record
# 调试一个用例：
PERI_E2E_REPLAY_MODE=record npx vitest run --config vitest.replay-v2.config.ts --testNamePattern S01
```

`CI` 环境拒绝 record。场景使用本地合成模型 fixture；通过这些测试不代表某个线上模型的质量。
每个录制目录同时保存初始文件 seed、HTTP 原字节 cassette、环境绑定声明和工具参数派生计划。
更新先写入独立临时目录，通过场景和封存校验后才替换旧 fixture。

## 场景接口

业务用例调用 `helpers/llm-replay-suite.ts` 的 `runReplayScenario`：

```ts
await runReplayScenario({
  id: "T01",
  title: "真实文件工具结果可见",
  sessionNames: ["root"],
  configure: async (world) => { /* 初始配置、工作文件；只在显式 record 执行 */ },
  upstream: async (world) => startLocalFixture(world), // 只在 record 启动
  runOptions: (ctx) => ({ gates: [/* 按明确 owner/execution/call 控制字节前缀 */] }),
  run: async (ctx) => {
    ctx.identity.arm(ctx.sessionIds.root!, "turn-1");
    const tui = await ctx.startTui("root");
    await tui.prompt("fixture scenario input");
    // 对真实屏幕、文件和 ACP 完成事实断言；代理发送完不是应用已消费。
    ctx.observe("user-visible-result", { /* 实际观测证据 */ });
  },
});
```

`ctx.requests` 保留当前模式的原始请求，`ctx.checked(predicate)` 在代理错误时尽早失败。
`runOptions.onRequest` 与共享绑定、证据采集串联，不能替换共享行为。
`ctx.bindings` 注册显式 typed 值；重放只使用录制时冻结的精确字段模板，保留其余全文严格匹配。
工具参数中的路径引用按白名单派生到当前 world，原始 cassette 不变。任意 shell 文本、签名和不透明状态不自动改写。

`configure` 的源码及默认配置有签名；变化后必须重录。闭包值或外部配置变化还应修改 `configVersion`。
seed 中不得写入录制 world 的绝对路径或会话数据库。相对工作目录配置能直接复用，模型工具的绝对参数由 typed 路径绑定处理。
record 通过后保留最终工作文件证据并删除旧 HOME/work，随后独立 replay 必须只依赖自己的新目录。

## 结果与时延

每场景结果写入 `results/replay-v2/<invocation>/<id>/report.json`，同时保留屏幕 `.txt`、原始 `.pty`、
请求 JSON 和 `final-work/`。失败也写报告，保留明确错误。`observe` 记录证据，不代替断言。

| 字段 | 含义 |
| --- | --- |
| `preparationMs` | 读取/验证 seed、创建隔离环境和真实 ACP 初始化；不含 proxy 启动 |
| `proxyBootMs` | 打开及校验 cassette、启动本地 HTTP 代理 |
| `driverMs` | 场景键盘/ACP 动作、等待和断言 |
| `scenarioMs` | 驱动及完成核验、证据与录制封存；不含最终清理 |
| `cleanupMs` | 关闭进程/代理/fixture、清理录制世界 |
| `totalMs` | 整个 case 的墙钟时延 |

`driverMs` 和 `scenarioMs` 有包含关系，不能相加。真实重试 backoff、受控 gate 和应用刷新包含在场景时延中，
不能把这些时间称为纯模型回放 CPU 开销。

可用 `PERI_E2E_REPLAY_FIXTURES`、`PERI_E2E_REPLAY_RESULTS`、`PERI_E2E_REPLAY_INVOCATION` 指定 fixture 与报告位置。
代理协议和管理 CLI 另见 [录放核心](llm-replay/README.md)。
