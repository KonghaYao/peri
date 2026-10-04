# Oxidizer `fetch` 上游核查

状态：参考资料，不构成 Peri 的设计或工程规则。Peri 的现状与边界以代码、`docs/standards/` 和 `docs/design/` 为准。核查基于 Microsoft/oxidizer `main` 的提交 [`fa0cfd9`](https://github.com/microsoft/oxidizer/commit/fa0cfd9258f8469bafd1dcf423b2acff8ed3c4c0)（2026-10-04 读取）；发布版本与后续提交需重新核对。

## 可确认的能力

- `fetch` 是 [monorepo 中的 `crates/fetch`](https://github.com/microsoft/oxidizer/tree/fa0cfd9258f8469bafd1dcf423b2acff8ed3c4c0/crates/fetch)，该提交的 [`Cargo.toml`](https://github.com/microsoft/oxidizer/blob/fa0cfd9258f8469bafd1dcf423b2acff8ed3c4c0/crates/fetch/Cargo.toml) 标为 `0.17.1`、MIT。`default = []`；`tokio` 是显式 feature，拉入 `fetch_hyper`、`hyper-util` 和 Tokio。HTTPS 的 `rustls`、`native-tls` 亦须显式启用。文档中“Tokio by default”指推荐的现成构造路径，不能理解为 Cargo 默认 feature 启用 Tokio。
- [`fetch::RequestHandler`](https://github.com/microsoft/oxidizer/blob/fa0cfd9258f8469bafd1dcf423b2acff8ed3c4c0/crates/fetch/src/lib.rs) 重新导出自 [`http_extensions`](https://github.com/microsoft/oxidizer/blob/fa0cfd9258f8469bafd1dcf423b2acff8ed3c4c0/crates/http_extensions/src/request_handler.rs)。它是 `Service<HttpRequest, Out = Result<HttpResponse>>` 的 blanket trait；`HttpRequest` / `HttpResponse` 分别是 `http::Request<HttpBody>` / `http::Response<HttpBody>`，不是抽象的任意请求状态。
- [`custom::create_builder`](https://github.com/microsoft/oxidizer/blob/fa0cfd9258f8469bafd1dcf423b2acff8ed3c4c0/crates/fetch/src/custom.rs) 接收 transport factory、`Isolation::{Shared, Isolated}` 和 `CustomDeps`（`Clock`、`GlobalPool`、可选 extras），返回 `HttpClientBuilder`。factory 按 pool slot 懒创建 `RequestHandler`；调用者负责自定义 transport 的 TLS。上层可继续使用 [`HttpClient` 的请求 API](https://github.com/microsoft/oxidizer/blob/fa0cfd9258f8469bafd1dcf423b2acff8ed3c4c0/crates/fetch/src/client.rs) 与 pipeline。因而“可插入 Hyper / 自定义 transport”有源码依据；reqwest 适配属于可实现的示例方向，非上游提供的现成后端。
- 默认 builder 使用含 resilience、logging、metrics 的标准 pipeline，也可选 minimal/custom pipeline；见 [`client_builder.rs`](https://github.com/microsoft/oxidizer/blob/fa0cfd9258f8469bafd1dcf423b2acff8ed3c4c0/crates/fetch/src/client_builder.rs)。因此接入范围超过替换一个 HTTP socket 实现，还会引入请求体、错误、时钟、内存池、可观测性等上游类型与策略。

## WebAssembly 边界

- 该提交的 [`Cargo.toml`](https://github.com/microsoft/oxidizer/blob/fa0cfd9258f8469bafd1dcf423b2acff8ed3c4c0/crates/fetch/Cargo.toml) 没有 browser、WASI、`wasm32` feature 或目标依赖；[`crates/fetch/src`](https://github.com/microsoft/oxidizer/tree/fa0cfd9258f8469bafd1dcf423b2acff8ed3c4c0/crates/fetch/src) 中的现成网络 transport 是 [`tokio.rs`](https://github.com/microsoft/oxidizer/blob/fa0cfd9258f8469bafd1dcf423b2acff8ed3c4c0/crates/fetch/src/tokio.rs) 调用 [`fetch_hyper`](https://github.com/microsoft/oxidizer/tree/fa0cfd9258f8469bafd1dcf423b2acff8ed3c4c0/crates/fetch_hyper/src)；另有测试用 [`fake.rs`](https://github.com/microsoft/oxidizer/blob/fa0cfd9258f8469bafd1dcf423b2acff8ed3c4c0/crates/fetch/src/fake.rs)。没有第一方 Browser Fetch 或 WASI HTTP handler。`default = []` 只证明可不启用该 transport，**不证明**整个依赖树可在 `wasm32-unknown-unknown` 或 `wasm32-wasip2` 编译。未见上游声明或 CI 契约保证这两个目标。
- [`layered::Service`](https://github.com/microsoft/oxidizer/blob/fa0cfd9258f8469bafd1dcf423b2acff8ed3c4c0/crates/layered/src/service.rs) 要求实现者 `Send + Sync` 且 `execute` 返回的 future 为 `Send`。浏览器 Fetch 常用的 [`wasm_bindgen_futures::JsFuture`](https://docs.rs/wasm-bindgen-futures/latest/wasm_bindgen_futures/struct.JsFuture.html) 为 `!Send`、`!Sync`。所以把 `JsFuture` 直接持有并 `await` 于 `RequestHandler::execute` 中，通常不能满足接口；需先验证本地任务桥接、消息传递或其他适配能否给出真正的 `Send` future。不能仅凭可替换 transport 就承诺浏览器后端可用。
- WASI HTTP 是不同的宿主接口：[`wasip2::http::outgoing_handler`](https://docs.rs/wasip2/latest/wasip2/http/outgoing_handler/index.html) 提供 `handle` 和 `future-incoming-response`。上游没有提供这个适配，仍需编写 HTTP 头与 body 转换、错误映射、超时与取消处理，并验证目标运行时是否提供 `wasi:http/outgoing-handler`。`Service` 的 `Send` 约束本身**不能证明 WASI Preview 2 不兼容**：例如 [`wstd::http::Client`](https://docs.rs/wstd/latest/wstd/http/struct.Client.html) 标为 `Send + Sync`，但具体 adapter 的 future、body 和整棵依赖树仍须实际编译验证。
- 上游 crate 文档明确列出 cookies、redirects、forms 等缺口，见 [`lib.rs`](https://github.com/microsoft/oxidizer/blob/fa0cfd9258f8469bafd1dcf423b2acff8ed3c4c0/crates/fetch/src/lib.rs)。若 Peri 当前请求路径依赖这些行为，须逐项做契约对照；不能以 API 外形相近推断等价。

## Peri 当前请求边界

这里至少有三种不同的「请求」，不宜共用一套状态枚举：

| 请求 | 当前事实源 | 状态与所有者 |
| --- | --- | --- |
| 模型 HTTP/SSE | [`peri-model` transport](../../peri-model/src/transport/http.rs)、[流式 runtime](../../peri-model/src/runtime/stream.rs)、[retry](../../peri-model/src/runtime/retry.rs) | HTTP 连接、分块 body、取消及首次可见增量前的重试由 `peri-model` 持有；provider 的 `Completed`/`Interrupted` 是模型流语义。现有 `HttpTransport` 可注入假实现，但 `HttpRequest` 直接包 `reqwest::Request`，两个 provider 构造请求时还持有 `reqwest::Client`，因此尚未隔离编译依赖。 |
| ACP 请求/响应 | [`RequestRouter`](../../peri-acp/src/transport/router.rs)、[ACP 契约](../design/peri-acp-protocol.md) | pending、响应匹配、调用者 drop 和 transport close 是连接内状态；router 通过 owner token 防止旧句柄删除复用 ID 的新请求，不负责 Agent 执行结果。 |
| Agent/turn 执行 | [`AgentStatus`](../../peri-acp-types/src/thread/types.rs)、[`TurnStatus`](../../peri-acp-types/src/event.rs)、[执行身份设计](../design/serverless-execution-identity.md) | 活跃、完成、取消、错误及执行接管由 Agent、Store 与协议层各自持有；HTTP 200 或 SSE EOF 不构成执行完成证据。 |

其他 HTTP 消费方也没有一套同构语义：远程 MCP 在 [`rmcp` transport](../../peri-middlewares/src/mcp/client/transport.rs) 上显式使用 `reqwest::Client` 和 OAuth 包装；[Web 工具](../../mcp-packages/web/src/web_fetch.rs)、[artifact](../../mcp-packages/artifact/src/client.rs) 与 [Langfuse](../../langfuse-client/src/client.rs) 各有请求协议。远程 Store 生产路径使用 [`turso_serverless`](../../peri-resources/Cargo.toml)；该 crate 的 `reqwest` 仅为云端探测测试的 dev dependency，不能靠替换 Peri 自己的 HTTP client 覆盖其内部传输。

当前工作树是 `pre-release/main`，没有 `peri-wasm` crate；[Serverless 执行环境身份](../design/serverless-execution-identity.md) 描述的是 `refactor/wasm` 分支的 `wasm32-unknown-emscripten` + Node/Bun/workerd 宿主路径，并明确普通浏览器不直接使用这条 Node socket 路线。故 Browser Fetch 与 WASI Preview 2 属于潜在后续目标，不是这条已批准目标路径的直接替换后端。该分支的构建结果和依赖闭包需在其工作树单独核对，本文没有替它宣称编译通过。

## 判断与建议

**现在不宜只为「统一请求状态」建立 `peri-request`。** `fetch` 的类型和 pipeline 表达 HTTP 请求，不表达 ACP pending、模型流完成、Agent 终态或执行恢复；把这些状态装进一个包会跨越现有所有权边界。把所有 HTTP 消费方一次迁到 `fetch` 也不能解决 `rmcp` 与 `turso_serverless` 的具体传输依赖，反而会把上游的时钟、pool、pipeline 和错误策略引进模型核心。

若近期目标是让模型 HTTP/SSE 在另一个 Wasm 宿主运行，先在 `peri-model` 现有 seam 上做一个最小垂直切片：把请求 DTO 改为与 `reqwest` 无关的 method、URL、headers、body，并保持凭据只在 transport/adapter 边界可见；注入目标后端，验证真正的流式响应、取消、错误映射、重试后重建请求、SSE 完成与中断。随后按真实第二个生产消费方（例如远程 MCP）评估是否提取仅承载 HTTP request/response stream 的独立 crate；若提取，业务状态仍归各自模块。浏览器目标须先解决 `Send` future；WASI 目标须先验证宿主提供的 HTTP 能力与依赖树。`fetch` 可作为此切片的候选实现，现有证据不足以直接选为 Peri 的统一底座。
