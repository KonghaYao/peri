> 归档于 2026-08-11，原路径 spec/issues/2026-08-02-grep-glob-path-parameter-ignored.md

# Grep/Glob 的 path 参数被忽略，搜索静默回退到全仓库

**状态**：Fixed
**优先级**：高
**创建日期**：2026-08-02

## 问题描述

Grep/Glob 工具传入 `path` 参数时，搜索范围未受到限制，静默回退到整个 cwd，返回全仓库噪声。无效搜索结果污染上下文、误导决策（如 019fbc99 中表面像"项目里已在用 hit_test"），并连锁引发 30+ 次 Bash grep/rg 工具迁移与分段盲读。wander 报告（agent-defect-analyzer，8-01 后样本）5/5 会话复现；现场最小复现 3/3 必现。

## 症状详情

**wander 报告跨会话证据**：

| 会话 | 证据 | 特征 |
|------|------|------|
| 019fc1b0 | pattern 含 `\|`（多模式）Grep 13+ 次 path 全部失效，返回 Cargo.lock/spec 噪音；同轮单模式 Grep path 全部正确 | 多模式全错 / 单模式全对 |
| 019fbc99 | path=`~/.cargo/registry/...` 4 次全部回退项目内；同路径 Bash ls/grep/Read 全部成功 | 大目录/外部路径 |
| 019fbdbe | 间歇性失效 7+ 次（#17/#102/#104/#106/#110/#114/#116/#131），同会话有生效对照（#48/#49/#141） | 间歇性比稳定失效更坑 |
| 019fc204 | 3 次全仓库噪音；agent 在 #15 自己诊断出"path 参数似乎被忽略了" | 第一次带 path 就失效 |
| 019fbd5d | 7/10 次错乱（path 指向单文件返回其他文件）；agent 把工具 bug 合理化成自己的失误（"因为 -C 上下文带进来了"，但没传 -C） | agent 错失发现工具问题的机会 |

**现场最小复现（2026-08-02，3/3 必现）**：

- `Grep(pattern="tokio\|serde", path="peri-agent/src/lib.rs")` → 返回全仓库 ~200 文件（含 Cargo.lock、README.md、spec 文档）
- 单模式 `Grep(pattern="tokio", path=同文件)` → 同样全仓库噪声
- `Grep(pattern="path", path="peri-middlewares/src/tools/filesystem/grep_args.rs")` → 返回全仓库含 "path" 字样的文件（pool.rs、TUI-STYLE.md 等），目标文件的反向匹配结果完全丢失

目标文件均存在（lib.rs 2626 字节），排除"path 不存在"因素。

**影响**：无效搜索 + 全仓库噪音污染上下文 + 误导决策（#240 表面像"项目里已在用 hit_test"）；连锁引发 30+ 次 Bash grep/rg 工具迁移（019fbc99）、分段盲读（019fbd5d）。

## 复现条件

- **复现频率**：必现（现场 3/3，wander 报告 5/5 会话）
- **触发步骤**：
  1. 调用 `Grep(pattern="<任意>", path="<存在的单文件或目录>")` 或 `Glob(pattern=..., path=...)`
  2. 观察返回结果：文件列表不限于 path 指定的范围
- **环境**：peri agent 8-01 后会话，macOS

## 涉及文件

- `peri-agent/src/tools/invocation.rs` —— `normalize_params` 无条件把 `path` 重命名为 `file_path`（Read/Write/Edit 的别名归一化），Grep/Glob 的 schema 参数名为 `path` 且无 `file_path`，归一化后参数丢失
- `peri-middlewares/src/tools/filesystem/grep.rs` —— `invoke` 读 `input.get("path")`，归一化后为 None → 回退 cwd
- `peri-middlewares/src/tools/filesystem/glob.rs` —— `invoke` 读 `input["path"]`，归一化后为 Null → 回退 cwd
- `peri-middlewares/tests/canonical_tool_invocation_contract.rs` —— 应补充"Grep(path=单文件) 结果全部位于该 path 下"契约测试

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-02 | — | Open | agent | 创建 |
| 2026-08-02 | Open | Fixed | agent | 修复：normalize_params 改为 schema 感知，Grep/Glob 的 path 不再被重命名 |

## 修复记录

### 修复 #1（2026-08-02）

- **操作人**：agent
- **用户原意**：Grep/Glob 传入 path 参数时搜索范围必须受限于该 path，不再静默回退全仓库
- **修复内容**：
  - `peri-agent/src/tools/invocation.rs`：`normalize_params` 增加 `target: Option<&dyn BaseTool>` 参数，仅当目标工具 schema 声明 `file_path` 时才执行 `path`→`file_path` 重命名
  - `peri-agent/src/agent/stages/tool_dispatch.rs`：本地 `normalize_params` 副本同样 schema 感知（PARAM_ALIASES 仅对声明 real 参数的工具生效），执行链 input 保留 Grep/Glob 的 `path`
  - `peri-middlewares/src/tool_search/execute_tool.rs`：ExecuteExtraTool 两处调用传入 target
  - 测试：`peri-middlewares/tests/canonical_tool_invocation_contract.rs` 新增 `grep_path_parameter_is_preserved_not_renamed_to_file_path`（wrapper + 直接调用双路径）；`peri-agent/src/agent/stages/tool_dispatch_test.rs` 适配新签名并新增 path 保留单测；`execute_tool_test.rs` 补 Write file_path schema
- **涉及 commit**：未提交（用户未要求）
- **验证状态**：已验证（contract 测试 5/5、peri-agent lib 624、peri-middlewares lib 1068 全过，workspace 构建 + clippy 无警告）
