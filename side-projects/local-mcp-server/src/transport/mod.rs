//! 传输层：两条传输，同一个 MCP core（WP-005 / WP-006）。
//!
//! ## 共享契约（不满足即视为实现偏离）
//!
//! 1. **只有一种协议实现**：协议行为（七工具、legacy/modern 生命周期、结果与错误映射、
//!    任务资源）全部在 [`crate::mcp`] 与 [`crate::protocol`]。传输只做选帧、暴露面与关闭，
//!    不得复制任何协议判定；两条传输因此服务同一个 handler 类型
//!    （[`crate::mcp::server::SandboxServer`]）。
//! 2. **每条可信连接一个 handler 实例**：身份（principal / client_instance）在连接建立时
//!    生成并冻结进实例，工具执行只拿到该身份，跨连接不共享状态。
//! 3. **传输差异只允许出现在帧与暴露面**：
//!    - [`stdio`]：一行一个 JSON-RPC 消息（NDJSON，无 `Content-Length`）；stdout 只承载协议
//!      消息，诊断只走 stderr；stdin EOF 即优雅结束（规范 `basic/transports/stdio`）。
//!    - [`http`]：Streamable HTTP；modern 请求必须携带 `MCP-Protocol-Version`、`Mcp-Method`
//!      等标准头且与 body 一致，默认只绑回环（规范 `basic/transports/streamable-http`）。
//! 4. **关闭语义**：stdin EOF 结束 stdio 服务；HTTP 由 [`http::HttpServer::shutdown`] 取消
//!    会话并等待连接收尾。两条路径都必须由调用方（`main.rs` 装配）驱动，传输自身不决定
//!    进程退出码。
//! 5. **执行面的生命周期不归传输**：七工具在进程内执行，任务注册表由 `main.rs` 的装配
//!    持有并在关闭时统一回收；传输关闭只负责停止接单与收尾连接，
//!    不得留下在跑的后台进程或悬挂请求。
//!
//! 子模块所有权（声明由 WP-001 冻结，实现互不交叉）：
//! - [`stdio`]：WP-005。
//! - [`http`]：WP-006。

pub mod http;
pub mod stdio;

#[cfg(test)]
mod shared_core_test {
    //! 共享契约的编译期见证：两条传输都必须能驱动**同一个** handler 类型。
    //!
    //! 这不是"再测一遍协议"，而是结构断言：若有人让某条传输改用自己的一套 handler
    //! （例如绕开 [`crate::mcp::server::SandboxServer`] 另写协议层），本模块立刻无法编译，
    //! R-006 的"两种传输共用同一 MCP core"随之失效。

    use std::sync::Arc;

    use crate::mcp::server::SandboxServer;
    use crate::wire::{BoxFuture, ToolExecutor, ToolRequest, ToolResponse};

    /// 最小执行器：本断言只关心类型，不关心执行语义。
    struct NoopExecutor;

    impl ToolExecutor for NoopExecutor {
        fn execute<'a>(
            &'a self,
            request: ToolRequest,
        ) -> BoxFuture<'a, Result<ToolResponse, crate::error::ToolError>> {
            Box::pin(async move {
                Ok(ToolResponse::ok(
                    "noop",
                    crate::wire::StructuredOutput::ok(request.name),
                ))
            })
        }
    }

    /// 与 [`crate::transport::http::serve_http`] 的 `S`/`F` 约束逐字一致。
    fn assert_http_factory_accepts<S, F>(_factory: F)
    where
        S: rmcp::ServerHandler + Send + 'static,
        F: Fn() -> Result<S, std::io::Error> + Send + Sync + 'static,
    {
    }

    /// 共享 core 的实例（无任务来源，故不声明 resources 能力）。
    fn server() -> SandboxServer {
        SandboxServer::for_stdio(Arc::new(NoopExecutor) as Arc<dyn ToolExecutor>, None)
    }

    #[test]
    fn test_both_transports_serve_the_same_handler_type() {
        fn assert_server_handler<S: rmcp::ServerHandler>() {}

        // 1) 共享 core 本身是合法的 SDK handler。
        assert_server_handler::<SandboxServer>();

        // 2) stdio 入口直接消费该类型（签名不接受别的类型）。
        let stdio_entry = super::stdio::serve_stdio;
        drop(stdio_entry(server()));

        // 3) HTTP 工厂产出同一个类型，并被 serve_http 的约束接受。
        assert_http_factory_accepts(|| Ok::<_, std::io::Error>(server()));
    }
}
