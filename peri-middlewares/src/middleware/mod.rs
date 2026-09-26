pub mod filesystem;
pub mod image;
pub mod terminal;
pub mod todo;
pub(crate) mod web_common;
pub(crate) mod web_fetch;
pub(crate) mod web_search;

pub use filesystem::FilesystemMiddleware;
pub use image::ImageMiddleware;
pub use terminal::TerminalMiddleware;
pub use todo::TodoMiddleware;

// `pub mod web` / `WebMiddleware` 已随 v4-part-2 W3（I-03）从模块树删除：Web 能力
// 迁移为 builtin MCP 实例（`mcp__web__*`），middleware 提供面不再存在，因此
// `PERI_MCP_BUILTIN=off` 的退回态没有 Web/Artifact 能力（A2 的运维语义）。
// `web_fetch` / `web_search` 的内部实现仍被 builtin handler 复用（I-01）。
//
// ⚠ 已知跨 task 依赖（S-02，W3）：`tool_search/declaration_test.rs` 仍引用
// `crate::middleware::WebMiddleware`（§4 归 S-02；§6 S-02 行要求它改走声明表）。
// 在 S-02 落地前，**crate 内 lib 测试目标**会因该文件报 E0432；生产 lib 不受影响。
