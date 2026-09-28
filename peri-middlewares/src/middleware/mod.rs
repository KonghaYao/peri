pub mod image;
pub mod terminal;
pub mod todo;
pub(crate) mod web_common;
pub(crate) mod web_fetch;
pub(crate) mod web_search;

pub use image::ImageMiddleware;
pub use todo::TodoMiddleware;

// `pub mod filesystem` / `FilesystemMiddleware` 与 `TerminalMiddleware` 已随
// v4-part-4 W3-C1 从模块树删除：7 个文件/终端工具（Read / Write / Edit / Glob /
// Grep / folder_operations / Bash）迁移为 builtin `workspace` 实例提供（模型面
// 名字为原始工具名），middleware 提供面不再存在，因此
// `PERI_MCP_BUILTIN=off` 的退回态没有文件/终端能力（与 Web/Artifact 的 A2
// 运维语义同构）。工具实现本身保留并复用：6 个文件工具在 `crate::tools::filesystem`，
// `BashTool` 在 `terminal.rs`，两处都不再经 middleware 暴露。
//
// `pub mod web` / `WebMiddleware` 已随 v4-part-2 W3（I-03）从模块树删除：Web 能力
// 迁移为 builtin MCP 实例（WebSearch / WebFetch 原名），middleware 提供面不再存在，因此
// `PERI_MCP_BUILTIN=off` 的退回态没有 Web/Artifact 能力（A2 的运维语义）。
// `web_fetch` / `web_search` 的内部实现仍被 builtin handler 复用（I-01）。
// 原「S-02 未落地前 lib 测试会报 E0432」的告警已失效并删除：`tool_search/declaration_test.rs`
// 已改走声明表 `peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES`，全仓无
// `crate::middleware::WebMiddleware` 引用。
