# [P1] workspace Write/Edit 未实现 read-before-write 保护，与工具描述的硬承诺不符

> 2026-09-28 范围更新：`local-mcp-server` 已按用户裁决退役，代码与独立构建入口已删除。本文涉及旧项目的路径、命令、比较和后续复用建议仅作为历史记录，不再作为实施或验收要求；当前 Workspace MCP 入口与验证见 [主项目代码索引](../../docs/code-index/peri-middlewares.md)。

**状态**：已修复，待用户验收
**优先级**：P1（本次测试建议级，待用户定级。描述对模型构成明确的行为契约，缺失保护使模型可能在未读取最新内容时覆盖他人修改）
**类型**：提示契约修正；已按用户裁决保留未读先写能力
**创建日期**：2026-09-28
**来源**：2026-09-28 workspace MCP 工具深度测试（Peri 内置 `workspace` 实例，HEAD `4a4307bb`，macOS 26.5.1 arm64）
**最后核查**：2026-09-28（实测行为 + 静态代码核查，两层证据均已复核）

## 用户裁决与本轮修复

用户已明确裁决：**允许未读先写，不新增限制；从提示词和相关文档移除强制先读条目。** 下文“补实现”建议不再是待选方向，也不作为验收要求。

已删除主 Read/Write/Edit 描述中的强制预读、未读会失败承诺与提前预读建议，并同步独立 `side-projects/local-mcp-server` 生产 catalog 实际消费的三份 schema fixture。保留 Edit 匹配失败后建议读取最新内容的恢复提示。未增加 read registry、guard 或新的写入限制。

验证：Edit 17 项、Write 40 项通过；独立项目 catalog 7 项通过。`./dev.sh -p` 真实执行未预读直接覆盖既有文件与修改既有文件，均成功并回读核实；模型实际收到的描述不再含预读硬要求。只读文件的原子替换场景也通过。

以下描述与测试证据仅保留为修复前验收输入，最终接受与否待用户复验。

## 问题描述

工具描述向模型承诺"编辑/覆盖已有文件前必须先 Read，否则调用会失败"：

- `peri-middlewares/src/tools/filesystem/descriptions/edit.md:4`：
  "You must use your mcp__workspace__Read tool at least once in the conversation before editing. **This tool will fail if you attempt an edit without reading the file**"
- `peri-middlewares/src/tools/filesystem/descriptions/write.md:5`：
  "If this is an existing file, you MUST use the mcp__workspace__Read tool first to read the file's contents. **This tool will fail if you did not read the file first**"

实测四个场景全部成功写入，无一被拒绝；静态核查在实现中未找到任何 read-tracking 状态或前置校验。

## 证据

### 实测（四个独立场景，均可复现）

| 场景 | 操作 | 预期（按描述） | 实际 |
| --- | --- | --- | --- |
| 1 | Write 覆盖已存在、从未 Read 的 `/tmp/peri-ws-test/pattern.txt` | 调用失败 | `Wrote 1 line` 成功 |
| 2 | Edit 从未 Read 的 `/tmp/peri-ws-test/subdir/a.rs`（`fn a() {}` → `fn a() { /* edited */ }`） | 调用失败 | `Replaced 1 line` 成功 |
| 3 | Edit 由 Bash 创建、工具层从未接触的 `repo/.tmp/read-guard-probe.txt` | 调用失败 | `Replaced 1 line` 成功 |
| 4 | 对同一仓库内文件二次覆盖（`ws-overwrite-probe.txt`，写后未重读） | 调用失败 | `Wrote 1 line` 成功 |

最小复现：

```bash
printf 'guard me\n' > /tmp/read-guard-demo.txt   # 用 Bash 制造"工具层从未读过"的文件
```

随后直接 `Edit`（`old_string: "guard me"`）或 `Write`（覆盖 `/tmp/read-guard-demo.txt`）该路径，均成功。

### 实现核查（已复核）

- `edit.rs`：`invoke` 路径为 `read_pre`（读当前内容）→ 匹配/替换 → `guard_and_commit`，无"该文件是否曾被 Read"的校验；`build_not_found_hint`（`edit.rs:25-83`）只在 `old_string` 匹配失败后输出 "Please Read this file to get the latest content before retrying."，是**事后提示**，不是前置约束。
- `write.rs`：`invoke`（`:200` 起）→ `direct_write` → `commit` → `guard_and_commit`，无 read 校验。
- 全目录检索 `read_state|ReadState|has_read|have_read|read_files|read_before_write|file_has_been_read` 在 `peri-middlewares/src/tools/filesystem` 无任何命中。
- `write_sandbox.rs:1-5` 的 WriteSandbox 是 readonly subagent 的**目录白名单**通道（限制可写位置），与 read-tracking 无关，不能充当该保护。

## 影响

- 描述承诺"会失败"是模型形成安全假设的依据：模型可据此假定"能写成功 = 内容已在上下文中"，但实际并非如此，存在覆盖未读内容的静默风险。
- `peri-middlewares/src/tools/filesystem/write_test.rs`、`edit_test.rs` 中未见覆盖该承诺的用例（测试空白与本缺陷一致）。

## 修复方向（二选一，需裁决）

1. **补实现（建议）**：引入会话级 read registry（记录被 Read 的 canonical 路径），Write/Edit 对已存在文件在未登记时返回显式错误（与描述一致）；已在 edit.md / write.md 承诺，属"实现向契约收敛"。
2. **修描述**：若该保护被有意取消，删除两处 "This tool will fail" 承诺，并记录取消理由（否则描述持续误导）。

按 `STD-INDEX-002`，选择取决于是否存在"已批准契约"支持该保护；本 issue 只澄清分歧，不单方面判定。

## 验证与未验证项

- 已验证：上述四场景行为、实现内无 read-tracking、`write_sandbox.rs` 用途。
- 未验证：`peri-agent` 层是否存在会话级 read 记录（本次仅核查 workspace 实例与 `crate::tools::filesystem` 范围；即便存在，也未被这两个工具消费）。
- 未验证：`side-projects/local-mcp-server`（独立实现）是否已有该保护——若它有而内置实现没有，两者行为将分叉。

## 涉及文件

- `peri-middlewares/src/tools/filesystem/descriptions/write.md`（承诺文本，`:5`）
- `peri-middlewares/src/tools/filesystem/descriptions/edit.md`（承诺文本，`:4`）
- `peri-middlewares/src/tools/filesystem/write.rs`（实现，无校验）
- `peri-middlewares/src/tools/filesystem/edit.rs`（实现，无校验）
- `peri-middlewares/src/mcp/builtin/workspace.rs`（内置实例装配，7 工具复用来源）

## 状态记录

| 日期 | 状态 | 说明 |
| --- | --- | --- |
| 2026-09-28 | Open | 由 workspace 工具深度测试创建；优先级为建议值，待用户定级 |
| 2026-09-28 | 已修复，待用户验收 | 本轮修复、用户裁决与验证见文首；原 Open 记录为历史输入 |
