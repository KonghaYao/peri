# CLAUDE.md 文档规则

### DOC-ROOT-001

- **Scope**：根 `CLAUDE.md`。
- **Rule**：根文件只做路由：仓库概览、模块入口、standards、design、active spec 与命令入口；不得复制工程细则。预算：不超过 6KB 且不超过 120 行。
- **Verify**：`wc -c -l CLAUDE.md`；人工检查内容是否仅为路由而非规则副本。

### DOC-MODULE-001

- **Scope**：模块 `CLAUDE.md`。
- **Rule**：模块文件说明 Scope、该模块的数据流、任务路由、稳定不变量、目标命令和按需引用；通常不超过 8KB。它不是父目录规则的自动继承层。
- **Verify**：`wc -c -l <module>/CLAUDE.md`；从该模块 cwd 启动时人工确认仅加载该 cwd 发现的首个指引文件。

### DOC-LOADER-001

- **Scope**：`AgentsMdMiddleware` 与模块文档。
- **Rule**：两条加载路径都不向父目录递归继承。主会话冻结入口 `read_frozen_content` 只检查 cwd 下的 `AGENTS.md`、`CLAUDE.md`、`.claude/AGENTS.md`；普通 middleware 查找还会追加用户级 `~/.claude/AGENTS.md` 和调用方提供的额外路径。两者均选取首个匹配；选中的 `CLAUDE.md` 支持受深度和循环检测约束的 `@import`。模块文件应按任务显式读取 `../docs/standards/`，不得用 import 把整套规范加入默认上下文。
- **Verify**：人工检查 `peri-middlewares/src/agents_md/mod.rs` 的 `read_frozen_content`、`candidate_paths`、`find_file` 与 `resolve_imports`；检查模块 `CLAUDE.md` 没有批量导入 standards。

### DOC-STABLE-001

- **Scope**：全部规范和 CLAUDE 文档。
- **Rule**：每条工程规则使用稳定 ID、Scope、Rule、Verify；不写动态数量、固定源码行号或事故叙事。用路径、符号、测试名或命令定位事实源。
- **Verify**：`rg -n ':[0-9]+|[0-9]+\s*个' docs/standards peri-agent/CLAUDE.md peri-acp/CLAUDE.md` 后人工确认命中不是命令、ID 或必要语义。

### DOC-UPDATE-001

- **Scope**：改动架构、规则、模块入口或文档 loader。
- **Rule**：实现变更同时检查是否影响对应 standards、模块 CLAUDE、测试 canonical 路由和命令；只更新受影响的单一事实源，避免平行副本。
- **Verify**：`git diff --check`；人工按本文件和 `docs/standards/index.md` 检查受影响路由与事实源。

### DOC-DESIGN-001

- **Scope**：`docs/design/`。
- **Rule**：只保存已批准的现行设计或目标设计，并在文首标明状态。调查报告、迁移批次、实施计划、完成清单、复盘、draft/proposal 与未采纳方案不得留在该目录；进度写 `spec/issues/`，历史由 archive issue 与 Git 保留。设计不得复制动态 inventory 或固定源码行号。
- **Verify**：检查 `docs/design/README.md` 索引与每份设计状态；`rg -n '状态：.*(draft|proposal)|分批执行计划|完成清单|本轮仅分析|未采纳' docs/design` 后人工确认无过程文档。

### DOC-REFERENCE-001

- **Scope**：`docs/reference/` 与面向使用者的 `docs/*.md`。
- **Rule**：生态背景、操作手册和可重复 checklist 可以保留，但必须声明不具权威性并链接对应 design/standard；不得保存某次执行的勾选状态或临时结果。
- **Verify**：检查 `docs/reference/README.md` 路由；checklist 不含已完成勾选，引用的权威文档存在。

### DOC-LINK-001

- **Scope**：文档移动、合并与删除。
- **Rule**：同一变更同步根/模块 `CLAUDE.md`、standards、code-index、active spec 和其他重要引用。已归档 issue 可保留历史语境，但不得留下指向已删除文件的当前导航。
- **Verify**：运行仓库 Markdown 本地链接检查与 `git diff --check`；用 `rg` 检查旧文件名无残留。
