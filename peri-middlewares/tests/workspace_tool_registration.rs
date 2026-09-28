//! workspace 实例的**工具注册面**外部验收（公开 API）。
//!
//! 本文件取代原 `middleware_tool_registration.rs`（v4-part-4 W3-C1）：那个文件的主语
//! 是 `FilesystemMiddleware` / `TerminalMiddleware` 两个类型与其 `tool_names()`，两者
//! 已随 7 个本地工具迁入 builtin `workspace` 实例而被删除。**注册这件事本身依然存在**，
//! 只是权威从「middleware 的 `tool_names()`」变成了契约层注册表
//! `peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES`；本文件守住的是同一意图在
//! 迁移后的形态：**注册表声明 ↔ 工具实现 的逐项一致**。
//!
//! 两侧都从公开面取证，缺任意一侧断言都退化为空转：
//!
//! - 注册表侧（`peri-acp-types`）：`workspace` 实例的工具表非空；
//! - 实现侧（本 crate 公开路径 `peri_middlewares::tools` /
//!   `peri_middlewares::middleware::terminal`）：逐项构造，断言
//!   `BaseTool::name()` / `is_direct()` / `prompt_declaration()` 与注册表声明**逐字相等**。
//!
//! 三项各自的失效方式不同，因此分开断言：
//! - `name()`：桥侧把裸名映射为 effective name（`mcp__workspace__Read`），映射表就是
//!   `original_name`。实现改名而注册表未改 ⇒ 桥会去找一个不存在的线名，模型面工具**静默消失**；
//! - `is_direct()`：决定这 7 项是否进首个 LLM 请求的直连表（AW3-03：迁移前后都必须在）；
//!   实现在此恒真、注册表也声明 `direct: true`，两侧不一致即能力面静默变化；
//! - `prompt_declaration()`：注册表里的模板是**逐字搬运**实现文本的（模板渲染仍归
//!   `tool_search/declaration.rs`）。任一侧被改写都会让模型面指引漂移，而 `tools/list`
//!   不承载该文本（声明段另有来源），只有本断言能发现。

use peri_acp_types::builtin_mcp::find as find_builtin_instance;
use peri_agent::tools::BaseTool;
use peri_middlewares::middleware::terminal::BashTool;
use peri_middlewares::tools::{
    EditFileTool, FolderOperationsTool, GlobFilesTool, GrepTool, ReadFileTool, WriteFileTool,
};

/// 按注册表**原始工具名**从公开路径构造实现。
///
/// `other => panic!` 是刻意的：注册表新增工具时，本测试必须以「未覆盖」失败，
/// 强迫新工具在公开构造面被显式登记——否则新工具会在无人守护的情况下进注册表。
fn build_tool(original_name: &str) -> Box<dyn BaseTool> {
    // 构造期不读文件系统（工具只在 `invoke` 时解析路径），cwd 取当前目录即可。
    let cwd = ".";
    match original_name {
        "Read" => Box::new(ReadFileTool::new(cwd)),
        "Write" => Box::new(WriteFileTool::new(cwd)),
        "Edit" => Box::new(EditFileTool::new(cwd)),
        "Glob" => Box::new(GlobFilesTool::new(cwd)),
        "Grep" => Box::new(GrepTool::new(cwd)),
        "folder_operations" => Box::new(FolderOperationsTool::new(cwd)),
        "Bash" => Box::new(BashTool::new(cwd)),
        other => panic!(
            "workspace 实例声明了本测试未覆盖的工具 `{other}`：注册表新增工具时必须同步 \
             `build_tool`（否则该工具的公开构造面无人守护）"
        ),
    }
}

/// 7 个本地工具迁入 builtin `workspace` 实例后，注册表声明与工具实现逐项一致。
///
/// 遍历顺序取自注册表本身（不做第二份名单）：注册表是「有哪些工具」的唯一权威。
#[test]
fn workspace_registry_and_tool_implementations_agree() {
    let workspace =
        find_builtin_instance("workspace").expect("workspace 必须是已实现的 builtin 实例");
    assert!(
        !workspace.tools.is_empty(),
        "workspace 实例的工具表不得为空（空表会让下面的循环退化为空转）"
    );

    for declared in workspace.tools {
        let tool = build_tool(declared.original_name);

        assert_eq!(
            tool.name(),
            declared.original_name,
            "工具实现的 `name()` 必须逐字等于注册表声明的原始名（桥侧以它为映射源）"
        );
        assert_eq!(
            tool.is_direct(),
            declared.direct,
            "工具实现的 `is_direct()` 必须与注册表声明一致（决定是否进首个 LLM 请求的直连表）"
        );
        assert_eq!(
            tool.prompt_declaration().as_deref(),
            declared.prompt_declaration,
            "注册表的 prompt 声明模板必须逐字等于 `BaseTool::prompt_declaration()` \
             （模板是搬运而非改写；任一侧改动都会让模型面指引静默漂移）"
        );
    }
}
