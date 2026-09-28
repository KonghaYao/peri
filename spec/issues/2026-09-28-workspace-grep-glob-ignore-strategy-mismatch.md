# [P2] Grep 遵守 gitignore/跳过隐藏文件，Glob 不遵守，两者差异未登记在工具描述

> 2026-09-28 范围更新：`local-mcp-server` 已按用户裁决退役，代码与独立构建入口已删除。本文涉及旧项目的路径、命令、比较和后续复用建议仅作为历史记录，不再作为实施或验收要求；当前 Workspace MCP 入口与验证见 [主项目代码索引](../../docs/code-index/peri-middlewares.md)。

**状态**：已修复，待用户验收
**优先级**：P2（同一路径下两个工具的结论互相矛盾，Grep 的 `No matches found.` 会被误读为"不存在"；两者均为有意实现，缺的是描述）
**类型**：文档缺陷（行为差异未登记）
**创建日期**：2026-09-28
**来源**：2026-09-28 workspace MCP 工具深度测试（Peri 内置 `workspace` 实例，HEAD `4a4307bb`，macOS 26.5.1 arm64）
**最后核查**：2026-09-28（双向对照实测 + 实现核查，均已复核）

## 本轮修复与验收状态

已在模型实际消费的 Grep/Glob 描述中说明隐藏文件、`.gitignore` / `.ignore`、显式文件路径与符号链接根路径的差异，保持两工具原有搜索策略。补充 `.ignore` 在非 Git 目录仍生效、Git ignore 规则依赖 Git 仓库的准确范围。

新增搜索行为回归 8 项通过；Grep 相关 49 项、Glob 46 项通过。`./dev.sh -p` 实际验证隐藏/ignored 目录负例、指定文件正例、Glob 可见性及符号链接根解析，通过且 exit 0。

以下问题描述及证据保留为修复前验收输入，最终接受与否待用户复验。

## 问题描述

同一路径下，Grep 与 Glob 对"哪些文件可见"采用不同策略：

| 行为 | Grep | Glob |
| --- | --- | --- |
| 跳过隐藏（`.` 开头）文件/目录 | 跳过 | 不跳过 |
| 遵守 `.gitignore`（及 `.ignore`） | 遵守 | 不遵守 |
| 跳过的目录名黑名单 | 无（走 ignore 规则） | 有（18 个目录名，见下） |

`descriptions/grep.md` 全文（35 行）未提及任何跳过规则；`descriptions/glob.md` 只登记了目录名黑名单与符号链接，未提"不遵守 .gitignore"。用户与模型无从预知这一分叉。

## 证据

### 实测（对照组，均可复现）

| 场景 | 命令 | 结果 |
| --- | --- | --- |
| 隐藏目录 | Grep `HIDDENNEEDLE` in `/tmp/peri-ws-test`（命中项位于 `.hiddendir/x.txt`） | `No matches found.` |
| 隐藏目录对照 | 同批次 Grep `VISIBLENEEDLE`（命中项位于普通文件 `visiblen.txt`） | 正常命中 |
| 显式路径不受隐藏规则影响 | Grep path=`/tmp/peri-ws-test/.hiddendir/x.txt` | 正常命中 |
| gitignore（真仓库） | Grep `REPOIGNOREDNEEDLE` in 仓库（`logs/wsmcp-probe.txt`，被 `.gitignore:2` 的 `logs` 规则忽略） | `No matches found.` |
| gitignore 对照 | Glob `*.txt` in 仓库 `logs/` | 命中 `logs/wsmcp-probe.txt` |
| 隐藏 + gitignore 目录 | Glob `*.md` in 仓库 `.tmp/` | 命中 `.tmp/mcp-tool-smoke-test.md` |

另有一次会话内真实踩中：Grep `联通性测试` 在仓库根返回 `No matches found.`，而该文本确实存在于 `.tmp/mcp-tool-smoke-test.md`（隐藏目录 + gitignored）。

### 实现核查（已复核）

- Grep：`peri-middlewares/src/tools/filesystem/grep.rs:109-115` 使用 `ignore::WalkBuilder` 并显式配置 `.hidden(true).git_ignore(true).ignore(true)`——跳过隐藏文件、遵守 gitignore/.ignore，**有意为之**。
- Glob：`glob.rs` 为自研遍历（`glob.rs:138/147/174` 调用 `should_skip_dir`），黑名单定义于 `mod.rs:52-73`（`node_modules`/`.git`/`dist`/`build`/`target`/`coverage` 等 18 项），**不读取** gitignore，也不跳过隐藏目录——同样是**有意为之**。

## 影响

- 用 Grep 搜索 gitignored 产物目录（如仓库内 `logs/`、`.tmp/`、`data/`）会得到"无匹配"，与 Glob 能列出同名文件直接矛盾，容易被误判为文件不存在或工具故障。
- 模型据此可能重复搜索、改用 Bash `rg` 绕行（与描述中 "Prefer mcp__workspace__Grep over shell commands like grep or rg" 的指引冲突）。

## 修复方向（建议：只补描述，不改行为）

- `descriptions/grep.md` 增补："跳过隐藏文件/目录；遵守 `.gitignore` 与 `.ignore`（在 git 仓库内）；显式 path 指向具体文件时不受影响。"
- `descriptions/glob.md` 增补："不遵守 `.gitignore`；仅按目录名黑名单跳过（列表见描述）；隐藏目录参与匹配。"
- 若产品希望两者统一，需另行裁决（本 issue 不预设该决定）。

## 验证与未验证项

- 已验证：上述六组对照实测；Grep 的 `WalkBuilder` 配置；Glob 的黑名单函数调用点。
- 未验证：`.ignore` 文件规则（仅核对了 gitignore）；Glob 黑名单在**搜索根自身**命中时的跳过行为已有一次观察（`target` 作为根被跳过），未系统枚举 18 项。
- 未验证：`side-projects/local-mcp-server`（独立实现）是否保持同样差异。

## 涉及文件

- `peri-middlewares/src/tools/filesystem/descriptions/grep.md`（待增补）
- `peri-middlewares/src/tools/filesystem/descriptions/glob.md`（待增补）
- `peri-middlewares/src/tools/filesystem/grep.rs`（行为源，`:109-115`）
- `peri-middlewares/src/tools/filesystem/glob.rs`、`mod.rs`（行为源，`should_skip_dir` `:52-73`）

## 状态记录

| 日期 | 状态 | 说明 |
| --- | --- | --- |
| 2026-09-28 | Open | 由 workspace 工具深度测试创建；两处描述待补，行为是否统一待裁决 |
| 2026-09-28 | 已修复，待用户验收 | 本轮修复、用户裁决与验证见文首；原 Open 记录为历史输入 |
