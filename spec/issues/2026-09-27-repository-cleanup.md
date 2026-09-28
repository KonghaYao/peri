# Repository cleanup scope and acceptance tracker

**状态**：Active — 全仓清理仍在进行；此文件仅作为当前任务台账。
**日期**：2026-09-27
**范围**：整个仓库，显式排除名为 `celld` 的目录/包及其内容；生成物不纳入扫描。

## 起始状态与边界

- 仓库：`/Users/konghayao/code/ai/peri-v4p3`，分支 `feat/mcp-adaptation-v4-part-3`。
- 开始扫描时 `git status --short` 为空；工作树和暂存区均无既有改动。并行工作开始后出现的代码改动不属于本台账作者。
- 起始版本索引有 2,073 个路径。按路径段排除 `target`、`node_modules`、`dist`、`build`、`out`、缓存目录和 `celld` 后统计。二进制、非文本资产的行数不用于规模估算。
- `git ls-files` 与普通文件扫描都没有 `celld` 路径命中。此名称仍作为显式排除边界；若后续发现被忽略或未跟踪的 `celld`，不读取其内部。
- `.gitmodules` 记录两个子仓。`peri-cool` 已按父仓 gitlink 初始化到 `7ca88a230eb094cc6bb3de0b9690c43cab85fc0b`，未更新远端分支；子仓有 154 个跟踪文件，检出后状态干净。`e2e/tui-tester` 已初始化（`.git` gitfile → `peri/.git/worktrees/peri-v4p3/modules/e2e/tui-tester`），当前 HEAD = 父仓固定 gitlink `1d73f75510232aea4ec589e7dc563f9fcb23ee04`，内部已装依赖与构建产物（`node_modules/`、`dist/`）；内部有一处未提交改动（`yarn.lock`），父仓因此记为 dirty 子模块，其内部未纳入本次扫描。

## 互斥工作包

每个目录只归入一行。Rust 规模是扫描初期的跟踪文件数和 Rust 源行数估值；并行改动可能改变行数，不影响路径边界。

| 包 | 覆盖路径 | 规模 / 风险 | 主要验收门禁 |
| --- | --- | --- | --- |
| WP-01 Agent | `peri-agent/` | 210 files / 59.3k Rust lines；高，loop、session、compact、异步生命周期 | `peri-agent/CLAUDE.md`；目标 `cargo test -p peri-agent --lib <filter>`；改 doc comment 跑对应 doc tests |
| WP-02 Middleware | `peri-middlewares/` | 360 / 115.1k；高，工具、MCP、权限、链顺序与子 Agent | `peri-middlewares/CLAUDE.md`；目标 lib tests、相关 contract tests；按改动运行 ACP host contract |
| WP-03 ACP protocol | `peri-acp/`、`peri-acp-types/` | 248 / 80.1k；高，wire、session lifecycle、event、caps | `peri-acp/CLAUDE.md`、architecture contracts；各 crate 目标测试与 doc tests；协议变更核对两端投影 |
| WP-04 TUI / theme | `peri-tui/`、`peri-theme/` | 390 / 115.0k；高，状态投影、输入、交互和主题；根 manifest 显式列 15 个成员，`cargo metadata` 将 path dependency `peri-theme` 计入 workspace | `peri-tui/CLAUDE.md`、`docs/standards/tui.md`；TUI 与 theme 目标测试；必要时 `e2e/` 目标场景 |
| WP-05 Controller / runtime / Langfuse | `peri-controller/`、`peri-runtime/`、`langfuse-client/` | 86 / 18.4k；高，取消身份、执行路由、观测旁路 | 相关 code-index + architecture contracts；目标 crate lib tests；Langfuse 改动按 Controller 指引验证 bridge/e2e |
| WP-06 Workflow / JS runtime | `peri-workflow/`、`peri-js-runtime/`、`npm-packages/@peri-workflow/` | 75 tracked files，约 10.9k Rust lines；高，跨进程 RPC、取消/收敛与 wire DTO | `docs/code-index/peri-workflow.md`、`peri-js-runtime` 索引、`docs/design/workflow.md`；Rust 目标测试和 npm package scripts；必要时 middleware workflow lifecycle test |
| WP-07 Session support | `peri-resources/`、`peri-process/` | 30 / 11.0k；高，持久身份、OS owner 和进程回收 | `ARC-WORKSPACE-001`；crate 目标测试；OS 承诺需在声称支持的平台运行，交叉编译不替代运行验收 |
| WP-08 Other workspace crates | `peri-model/`、`peri-lsp/`、`peri-web-pty/` | 88 / 18.4k；中到高，provider、LSP、PTY platform lifecycle | 对应 `docs/code-index/`；各 crate 目标 lib/integration/doc tests；PTY 按平台边界做运行验收 |
| WP-09 E2E | `e2e/`（含子仓路径 `e2e/tui-tester`，已初始化但内部未扫描） | 53 个父仓跟踪文件；中到高，真实 TUI/tmux、部分 Judge 使用外部 API | `e2e/CLAUDE.md`：目标文件 `npm run e2e -- --file ... --serial --retry 0`；日常 `e2e:l0`，合并 `e2e:l1`，发版 `e2e:release`。确认 tester 子仓是否需要初始化后再运行 |
| WP-10 文档站 | `peri-cool/` 子仓 | 固定 gitlink；子仓 154 files | 子仓本地说明与 package scripts；父仓 gitlink 保持固定 commit |
| WP-11 PTC npm package | `npm-packages/@peri-ptc/` | 14 files；中，独立 package | `package.json` 中的本地 scripts、类型/构建入口 |
| WP-12 Example | `example/`（含 `minimal/`） | 22 files；低到中，独立 Bun example | `example/package.json` 和 `minimal/` 指引中的构建/运行命令 |
| WP-13 Side projects | `side-projects/agent-defect-analyzer/`、`daytona/`、`git-stats/`、`image-spike/`、`llm-gateway/`、`mcp-apps/`、`md-scan-matrix/`、`peri-db-viewer/`、`peri-sync/` | 各项目分别有 3–100 个跟踪文件；中，互不构成根 workspace | 各项目 manifest/README 中的本地命令；Rust 项目用对应 manifest 运行目标测试。不得因不在 workspace 就标为无测试 |
| WP-14 Scripts | `scripts/` | 11 files；中，安装、跨平台构建和验证入口 | 只运行与改动脚本对应的 shell/PowerShell、容器或平台验证；检查脚本目标与其文档一致 |
| WP-15 Root config and hidden state | 根级配置/说明/资产、`.cargo/`、`.claude/`、`.peri/`、`.github/`；根文件含 `AGENTS.md`、`CLAUDE.md`、`Cargo.toml`、`Cargo.lock`、`Cross.toml`、`lefthook.yml`、`.mcp.json` 等 | `.claude` 64、`.peri` 20、`.cargo` 1、其他根路径 23；高，规则注入、权限、工具配置、构建与 hooks | 根任务路由和对应 standards；配置/脚本改动按具体入口验证；必要时 `lefthook run pre-commit` |
| WP-16 Docs | `docs/`（standards、design、reference、code-index 与 HTML） | 65 files / 10.8k Markdown lines；中，事实源与导航 | `docs/standards/documentation.md`、`git.md`；本地链接/路由目标存在性、`git diff --check` |
| WP-17 Active spec | `spec/` | 78 files / 16.3k Markdown lines；中，实施/验收状态可能滞后 | `DOC-HISTORY-001`；只更正现行路由和有证据的当前状态，不重写历史，不把启动命令视作通过 |
| WP-18 Prompt fixtures | `prompts/` | 3 files / 49 Markdown lines；低，手工验证提示样例 | 与当前功能路由一致性检查；它们是 prompt fixtures，不当作架构规则 |

## 验收约定

- Rust 改动按目标 crate 和受影响链路运行测试；不要用 workspace 全套测试代替目标失败回归，也不要把 `cargo check` 当运行生命周期证据。
- 外部进程、transport、持久化恢复、跨平台行为按 `docs/standards/testing.md` 覆盖完整生命周期；不能运行的平台标为 blocked/unsupported 并说明原因。
- 独立 package 和 side project 通过各自 manifest/项目指引选择命令；根 workspace 门禁不覆盖它们。
- 文档改动核对事实源、路由、有效本地链接，并运行 `git diff --check`。历史 issue 只在仍需实现或验收时留在 `spec/issues/`。
- 本台账只记录可核对的命令终态与实际结果；started/completed 通知不是通过证据。

## 当前任务记录

| 工作包 | 覆盖目录与主要子域 | 发现与处置 | 修改文件 | 验证命令与真实结果 | 未验证及原因 |
| --- | --- | --- | --- | --- | --- |
| Inventory + WP-16/17/18 | 全部互斥路径已按上表盘点；重点检查 standards、code-index、design/reference、spec 路由与 prompts | `spec/global/problems.md` 原 Workflow 路由遗漏 `peri-workflow`/`peri-js-runtime` 索引，Langfuse 路由遗漏独立 `langfuse-client` 索引；已补当前路由。Prompt 文件是手工测试样例；active spec 状态均未凭文件标题推断关闭，历史叙述未重写。Cargo metadata 确认有效 workspace 包含 `peri-theme`。 | `spec/global/problems.md`；本台账 | `git diff --check -- spec/global/problems.md spec/issues/2026-09-27-repository-cleanup.md`：exit 0；14 个新增路由目标存在性检查：14/14，exit 0；`cargo metadata --no-deps --format-version 1`：exit 0，16 workspace members。 | 全仓其余包由并行工作包扫描；`e2e/tui-tester` 子仓已初始化、内部未扫描（其工作区 `yarn.lock` 有一处未提交改动） |
