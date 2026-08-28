# WASI Preview 2 能力边界与 policy Component 探针

> 状态：窄切片已验证；完整 Headless ACP Host 仍为 Exploratory
>
> 静态探查日期：2026-08-27；探针验证日期：2026-08-29
>
> 目标基线：Rust 1.96.1 `wasm32-wasip2`、Node.js 22.20.0、Headless ACP 服务
>
> 证据范围：完整 Host 结论来自仓库源码、Cargo manifests 与 lockfile 的静态核查；实际构建和 Node 运行证据仅覆盖内部 deterministic turn-policy Component probe。

## 1. 目的与边界

本文记录 Perihelion 面向 WASI Preview 2 的能力边界，并补充一个已实际构建和跨运行时执行的窄探针。完整 ACP Host 的分析仍用于未来讨论部署形态或依赖边界；探针的 active implementation 事实与验收进度见 `spec/issues/2026-08-28-wasi-p2-node-validation.md`。

探查目标是：在不包含 TUI 的前提下，评估 ACP session/event、Agent loop 与相关运行能力构建为 `wasm32-wasip2` Component 时可能遇到的问题。

本次实际实现仅把 native 已使用的两项确定性策略抽到 dependency-free `peri-turn-policy`：`MessageContent` 判空和 compact action 选择；`peri-wasi` 再通过 WIT 暴露其受约束子集。它是内部、`publish = false` 的 Component probe，不是 Agent loop、ACP protocol 或 Headless ACP Host 的 WASI port，也不改变下文对完整 native 闭包的判断。

本次不评估：

- 将 `peri-tui` 或终端 UI 移植到 WASI；
- `wasm32-wasip1` 的兼容性；
- 浏览器 target；
- 通过特定 WASI runtime 私有扩展复刻完整 Unix 环境；
- 完整 Agent/ACP 的实际性能、产物体积或 runtime 兼容性；
- 完整 Agent/ACP 的 WIT schema、版本计划或任务拆分。

## 2. 摘要结论

当前 Headless ACP 服务仍不能被视为可直接、完整地构建为标准 `wasm32-wasip2` 产物。主要原因不是单个 Rust API，而是 ACP Host 当前同时装配了 HTTP、SQLite、文件系统、子进程、插件、MCP、LSP、PTC、OAuth callback、系统指标和文件日志等 native host 能力。

已验证的结论更窄：Rust 1.96.1 能把不依赖这些 native capability 的 `peri-wasi` 直接链接为 WASI Preview 2 Component；Node.js 22.20.0 可经固定版本 Jco 和 Preview 2 shim 调用它的唯一业务 export。这个结果验证了工具链和纯策略 seam，不证明完整 Agent/ACP 构建闭包已经可移植。

较可行的远期形态是：

```text
WASI Agent/ACP Core Component
  + Host HTTP / secrets
  + Host ThreadStore
  + Host Tool Gateway
  + 可选 preopen filesystem
```

其中 ACP 数据模型、session/router、RCRA Agent loop、prompt/message projection、tool schema/dispatch、cancel/event mapping 大体属于可保留的逻辑核心；网络、持久化和进程型工具更适合作为 Component imports 或宿主服务，而不是继续由 Component 直接调用 native API。

这不意味着现在应继续拆分或修改完整 Host。当前结论仅表明：若未来立项，第一步应定义 WASI capability boundary，而不是先逐个替换 native 依赖并尝试原样编译完整 Host。

## 3. 当前 Headless ACP 构建闭包

现有 stdio 主路径为：

```text
peri-tui binary
  → peri_acp::host::stdio::run_acp_stdio
  → assemble_stdio_config
  → assemble_server_config
  → SessionManager
  → peri-agent session factory
  → peri-middlewares production assembly
  → run_react_loop
```

静态事实：

- stdio 服务入口位于 `peri-acp/src/host/stdio/mod.rs` 的 `run_acp_stdio`。
- stdio transport 位于 `peri-acp/src/transport/stdio.rs`，直接使用 Tokio stdin/stdout 和异步 channel。
- 完整 Host 装配位于 `peri-acp/src/host/assemble.rs` 的 `assemble_server_config`。
- Agent 阶段循环入口为 `peri-agent` 的 `run_react_loop`。
- `peri-acp` 当前只有 library target；实际 CLI/stdio 启动入口仍由 `peri-tui/src/main.rs` 调用。
- stdio 启动会加载本地配置、从配置或环境变量取得 provider，并打开 thread store；它不是只包含协议与内存状态的 bare server。
- `assemble_server_config` 会构造中间件、Cron、MCP、Skills、Plugin、Settings Hooks、Workflow 等部署能力。

因此，“Headless”目前只表示无 TUI 渲染，不表示无 native host capability。

## 4. 主要阻塞面

### 4.1 Tokio workspace feature 过宽

根 `Cargo.toml` 对 Tokio 使用 `features = ["full"]`。这会统一启用 native `net`、`process`、`signal`、`fs`、多线程 runtime 和 I/O driver 等能力，而 WASI Preview 2 不能被当作普通 Unix socket/process 平台。

未来若做构建基线，可能需要按 crate 和 target 收缩为 `rt`、`sync`、`time`、`macros`、`io-util` 等实际使用 feature，再让 native target 单独启用 `process`、`fs`、`net` 和 `signal`。这只能缩小编译闭包，并不会自动把 native API 映射为 Component imports。

### 4.2 LLM HTTP/SSE 强绑定 reqwest

`peri-model` 无条件依赖 `reqwest`，Anthropic/OpenAI provider 与生产 `HttpTransport` 直接持有 `reqwest::Client`，响应流使用 `bytes_stream()`。

Preview 2 网络应通过 runtime 提供的 capability 或 Component import 表达。即使某个 runtime 支持 sockets，DNS、TLS roots、proxy、SSE streaming 和 cancellation 仍会绑定其具体实现。

项目已有 `HttpTransport` 抽象，这是可利用的边界；但当前 provider struct 仍会无条件构造 native client。未来更稳妥的方向是：native target 保留 reqwest，WASI target 使用 Host HTTP transport，并考虑由 Host 注入认证 header 或 opaque secret handle，避免 API key 必须进入 Component 环境。

### 4.3 ThreadStore 启动时绑定 SQLite

stdio Host 会通过 `peri_agent::resources::open_thread_store_with` 打开 `peri-resources::Resources`，当前实例化路径固定进入 `SqliteThreadStore`。依赖闭包包含 `sqlx`、SQLite native binding、文件系统和锁语义。

即使能将 SQLite 编译到 WASM，仍需处理 preopen 路径、持久化、文件锁、WAL 和并发契约。由于现有上层已依赖 `Arc<dyn ThreadStore>`，更适合的远期选择是：

- 初始探针使用 in-memory store；或
- 通过 Host-imported `ThreadStore` 保留 SQLite 在宿主侧。

单纯把 `sqlx` 更换为另一个 SQLite crate 不会消除这些运行语义问题。

### 4.4 peri-middlewares 同时包含纯逻辑与 native capability

`peri-acp` 无条件依赖 `peri-middlewares`，后者又无条件引入 `peri-js-runtime`、RMCP child-process transport、reqwest、cacache、目录遍历、压缩与归档等依赖。

当前同一构建闭包同时包含：

- 可移植逻辑：prompt contribution、tool schema/dispatch、tool search、Todo/Goal、部分消息转换和 policy；
- 文件系统能力：Read/Write/Edit/Glob/Grep、Skills、CLAUDE/AGENTS loader、插件和配置扫描；
- 进程能力：Bash、Hooks command、MCP stdio、LSP、PTC、git attribution；
- 网络能力：MCP HTTP、WebFetch/WebSearch、Artifact、plugin marketplace、OAuth callback。

未来若立项，至少需要通过 features、crate boundary 或 host ports 区分 `core`、`host-fs`、`host-process` 和 `host-http`。标准 WASI Preview 2 不提供任意 shell/process 能力，MCP stdio、LSP、Bash、Hook command 与 PTC 不能仅靠依赖替换恢复。

### 4.5 stdio 与 Component Model 的接口形态不同

现有 ACP stdio transport 是进程 stdin/stdout 上的 newline-delimited JSON-RPC。它可以作为部署 adapter 保留，但不一定应成为 WASI Component 的唯一接口。

Component Model 更自然的形态是导出 session、prompt、event stream 和 cancel 等接口，并导入 HTTP、ThreadStore、tools、secrets、logging 等能力。实际 WIT schema 尚未设计，本文不固定具体接口。

### 4.6 并发和 Send/Sync 边界

项目广泛使用 `Arc<dyn Trait + Send + Sync>`、`tokio::spawn` 和后台 task owner。未来 Component import 返回的 resource handle 不一定天然满足现有线程语义，单线程 WASM runtime 也可能更适合 `spawn_local` 或 actor 串行访问。

需要在真实 runtime 选择后验证：

- Component 是否允许并发重入；
- imported resources 的线程与生命周期约束；
- streaming、cancel 和 shutdown 如何结算；
- 是否保持 `Send` future，还是引入单线程 executor adapter。

## 5. 能力可行性矩阵

| 能力 | 初步可行性 | 主要问题 | 候选边界 |
| --- | --- | --- | --- |
| ACP 数据模型与 JSON-RPC codec | 高 | 主要为 serde/纯逻辑 | Component 内保留 |
| ACP session/router/event mapping | 高 | channel 与 task runtime 需验证 | Component 内保留 |
| RCRA Agent loop | 高 | executor 和 imported resources 并发语义 | Component 内保留 |
| Prompt/message projection | 高 | 部分来源依赖文件扫描 | 逻辑保留，来源宿主化 |
| Tool schema/dispatch | 高 | 工具实现依赖外部能力 | 逻辑保留，执行走 Host |
| stdio transport | 中 | Tokio stdio 与 Preview 2 adapter | 部署 adapter 或 WIT export |
| LLM HTTP/SSE | 低 | reqwest、socket、TLS、stream | Host HTTP port |
| SQLite thread store | 低 | sqlx/native SQLite/锁/持久化 | Host ThreadStore 或内存实现 |
| Filesystem tools | 中 | 任意绝对路径与 capability root 不一致 | preopen filesystem adapter |
| Skills/CLAUDE loader | 中 | home/cwd/root 扫描语义 | Host 提供 roots/read capability |
| MCP HTTP | 低到中 | 网络、OAuth、secret | Host MCP gateway |
| MCP stdio | 不可直接保持 | 任意子进程能力缺失 | Host MCP gateway |
| LSP | 不可直接保持 | server 子进程和 stdio | Host LSP gateway |
| Bash/Hook command | 不可直接保持 | shell/process 能力缺失 | 禁用或 Host tools |
| PTC Node runtime | 不可直接保持 | Node/npm 子进程、cache 和锁 | Host tool/runtime |
| Plugin installer | 低 | 网络、归档、任意文件系统 | Host 管理 |
| OAuth callback | 低 | 本地 listener/browser 流程 | Host OAuth broker |
| RSS/system metrics | 低 | sysinfo 平台 API | Host metrics 或关闭 |
| 文件 tracing | 中 | preopen、轮转与 flush | stdout/Host logging |

“不可直接保持”表示不能依赖标准 WASI Preview 2 提供与 native process 相同的能力，不表示该功能永远无法通过宿主接口提供。

## 6. 依赖处置分类

### 6.1 大体可保留

纯逻辑路径中的以下依赖通常更接近 WASM/WASI 友好，但仍需以真实 target check 为准：

- `serde`、`serde_json`；
- `thiserror`、`anyhow`；
- `async-trait`、`futures`；
- `url`、`sha2`、`base64`、`regex`；
- `pulldown-cmark`、`lru`；
- `chrono`、`uuid`，但 clock、timezone 和 randomness 需要 runtime capability 验证。

### 6.2 更适合 feature-gate

- Tokio 的 native feature；
- `tracing-subscriber` / `tracing-appender`；
- `dirs-next`、`sysinfo`；
- `walkdir`、`ignore`；
- `cacache`、`tempfile`、`fs2`。

### 6.3 更适合移出 WASI 核心闭包

- `sqlx` 与 SQLite native binding；
- `peri-js-runtime`；
- RMCP `transport-child-process`；
- LSP process implementation；
- native file appender 和 system metrics。

### 6.4 更适合由 Host port 替代

| 当前能力 | 候选 WASI 边界 |
| --- | --- |
| reqwest HTTP/SSE | Host streaming HTTP |
| SQLite | Host ThreadStore |
| tokio/std process | Host Tool/MCP/LSP gateway |
| 任意 std::fs 路径 | capability filesystem adapter |
| env API key | Host-side auth 或 secret handle |
| sysinfo | Host metrics |
| file appender | Host logging |
| OAuth callback listener | Host OAuth broker |

## 7. 推荐的远期部署形态

如果未来重新评估，优先考虑以下边界：

```text
┌──────────────── WASI Component ────────────────┐
│ ACP session core                              │
│ RCRA loop                                     │
│ Prompt / message projection                   │
│ Tool selection / dispatch                     │
│ Cancellation / event mapping                  │
└───────────────┬────────────────────────────────┘
                │ Component imports
┌───────────────▼────────────────────────────────┐
│ Native Host                                   │
│ HTTP/SSE │ ThreadStore │ Filesystem │ Secrets  │
│ MCP      │ LSP         │ Bash       │ PTC      │
│ Plugins  │ Logging     │ OAuth      │ Clock     │
└────────────────────────────────────────────────┘
```

可选折中是让 Component 直接访问受控 preopen workspace，以保留文件工具和项目指引扫描，其余能力仍宿主化。此时路径必须相对 capability roots 表达，不应继续假设真实的 host home、任意绝对路径或完整 cwd 可见。

不建议优先尝试把 SQLite、reqwest/TLS、MCP child process、LSP、Node/npm、plugin installer 和 OAuth callback 全部搬入 Component。这样容易依赖特定 runtime 的私有扩展，且削弱采用 Preview 2 capability model 的价值。

## 8. 已验证的 deterministic policy Component probe

### 8.1 共享策略与 WIT 边界

`peri-turn-policy` 是 `#![no_std]`、无第三方依赖、`publish = false` 的共享 kernel。native 调用路径仍保持原入口，但把两项纯决策委托给该 crate：

- `peri-acp-types::MessageContent::is_empty` 将 `Text`、`Blocks`、`Raw` 投影为 `MessageContentShape`；纯空白 text 仍为非空。
- `peri-agent::agent::compact_v2::determine_compact_action` 把 budget、micro threshold 与 native Smart 开关交给 `select_compact_action`；原有 `CompactAction` 路径通过 re-export 保持不变。

`peri-wasi` 是 `cdylib`、`publish = false` 的边界 adapter。WIT package 为 `peri:turn-policy@0.1.0`，world 为 `turn-policy`，唯一业务 export 是命名 interface `policy`：

```wit
classify-content: func(content: content-shape) -> content-classification;
select-compact: func(
    budget: f64,
    micro-threshold: f64,
) -> result<compact-action, policy-error>;
```

Component 没有 imports。`classify-content` 只表达 content 判空，不包含 continuation-aware keepgoing；`select-compact` 只导出 `skip`/`micro`，不把 native legacy Smart 固化为跨运行时契约。边界在调用共享 selector 前拒绝非有限值和 `[0,1]` 外输入，错误优先级依次为 budget 非有限、budget 越界、threshold 非有限、threshold 越界。

### 8.2 构建与产物

Rust 1.96.1 的 `wasm32-wasip2` target 通过 `wasm-component-ld` 直接生成 Component，不需要 `cargo-component`。固定依赖与实际 lockfile 解析结果为：

| 项目 | 固定/解析版本 |
| --- | --- |
| Rust toolchain | 1.96.1 |
| target | `wasm32-wasip2` |
| `wit-bindgen` | 0.57.1 |
| `wit-parser`（`Cargo.lock`） | 0.247.0 |
| Node.js | 22.20.0 |
| npm | 10.9.3 |
| `@bytecodealliance/jco` | 1.32.1 |
| `@bytecodealliance/preview2-shim` | 0.22.0 |

release artifact 为 `target/wasm32-wasip2/release/peri_wasi.wasm`。验证过的文件头为 `00 61 73 6d 0d 00 01 00`：Wasm magic 后是 Component encoding/version，而不是 core Wasm 的 `01 00 00 00`。产物 WIT 检查确认 sole business export 为 `peri:turn-policy/policy@0.1.0`，imports 为空；Cargo target closure 的本地路径只有 `peri-wasi → peri-turn-policy`，没有 Agent/ACP、Tokio、reqwest、SQLx 或 process 能力闭包。

### 8.3 可复现 Node gate 与隔离

Node gate 把 dependency acquisition 与执行分开：

```bash
npm --prefix wasi-e2e run acquire:cargo
npm --prefix wasi-e2e test
```

`acquire:cargo` 只从已有 parent Cargo cache 获取源代码，在 `wasi-e2e/target` 下生成独立 acquisition workspace、versioned vendor tree 和 credential-free Cargo home；它先比较 standalone manifest 与根 `peri-wasi` 的 registry closure，再固定供应该闭包。当前验证输出为 `CARGO_VENDOR_REGISTRY_PACKAGES=34`。执行 gate 只使用这个隔离 Cargo home，启用 offline/frozen 构建，并校验 acquisition fingerprint、路径 containment、symlink、环境变量 allowlist、toolchain、Cargo tree、Component header、WIT/import 与 Jco 产物。

`npm --prefix wasi-e2e test` 当前包含 8 项 TAP 测试：缺失、空、core Wasm、损坏 Component、Jco 非零退出、acquisition fingerprint 篡改、vendor symlink，以及完整 build/inspect/transpile/execute 链路。隔离 Node child 对真实 WIT export 执行 28 个确定性断言，覆盖 content empty/non-empty、0/1/equality、NaN/±Infinity、两侧越界和多错误 precedence。已验证结果为 `tests 8 / pass 8 / fail 0 / skipped 0`。

`wasi-e2e/target`、Cargo `target`、`node_modules`、最终 wasm 和 Jco 转译文件都是 ignored 生成物，不进入版本控制。Node 自带的 `node:wasi` 是 Preview 1 路径；本探针使用 pinned Jco + Preview 2 shim，不把二者混同。

## 9. 若未来继续完整 Host 探查

窄探针已经完成工具链与纯策略 seam 的第一步。若未来继续完整 Agent/ACP 探查，建议按以下顺序推进：

1. 用最小 feature 对 `peri-acp-types`、`peri-model` 纯类型层和 `peri-agent` core 执行 `cargo check --target wasm32-wasip2`，记录真实首错。
2. 构造无外部工具、mock model、in-memory ThreadStore 的最小 Agent session，验证 loop、event 和 cancel。
3. 选择具体 Preview 2 runtime，确认 WIT async/stream/resource、stdio、clock、random 和 filesystem 支持边界。
4. 接入 Host HTTP 与 secret policy，验证流式响应、retry、timeout 和 cancellation。
5. 接入 Host ThreadStore，验证 append/load、compact lifecycle、rewind 和 session resume。
6. 最后逐项评估 filesystem、MCP、LSP、Bash 与 PTC，不应在 Agent 工具视图中展示未被 Host grant 的能力。

高概率首先暴露问题的依赖类别包括 Tokio native backend、`mio`/`socket2`、`sysinfo`、`sqlx`/SQLite binding、reqwest/TLS、`peri-js-runtime`、RMCP child-process transport 和 file appender。该顺序只是静态推断，必须由真实 target build 修正。

## 10. 当前决定

- 保留内部 deterministic turn-policy Component probe 与 Node 验证门禁；不把它发布为稳定产品接口。
- 不宣称完整 Headless ACP Host、Agent loop 或 ACP protocol 已完成 WASI port。
- 当前不为完整 Host 添加 CI job、feature flag 或 runtime 私有扩展。
- 当前不为了推测性兼容而修改 production middleware 顺序或工具行为。
- 完整 Host 未来若立项，应新建独立 active spec/issue，并以该闭包的实际构建日志和选定 runtime 契约替代本文中的静态推断；本次 probe 的通过不能作为该结论的替代证据。
