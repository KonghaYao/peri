# [P3] workspace 工具提示与错误消息的细节缺陷汇总（7 项）

**状态**：已修复，待用户验收
**优先级**：P3（均为提示/措辞/渲染层面的细节，不阻塞使用；建议随相关文件的下一次改动顺带处理）
**类型**：提示与错误消息细节 / 文档表达
**创建日期**：2026-09-28
**来源**：2026-09-28 workspace MCP 工具深度测试（Peri 内置 `workspace` 实例，HEAD `4a4307bb`，macOS 26.5.1 arm64）
**最后核查**：2026-09-28（逐项实测；涉及实现位置的已标注，未复核处如实标出）

## 本轮修复与验收状态

7 项均已处理：提前停止搜索的计数标为 collected / total unknown；单层建目录准确说明父目录与 recursive；Edit 片段不存在不再触发路径纠错；补充 Unix 原子替换可能覆盖只读文件；说明 Glob 根 symlink 解析；修复目录树连接符与链接标记；list 保持目录优先且组内按名称排序。

同一计数链路还修复 list 显示配额不满却声称 500、deep_scan 实际显示 501 却声称 500 的问题。目录枚举不完整时不再宣称完整总数。

回归先红后绿：folder 19 项、path_suggester 9 项通过。最终文件系统相关 216 项、错误建议 40 项通过。两组 `./dev.sh -p` 共 10 个验收场景通过，实际工具输出与磁盘结果已核对；覆盖树连接、链接标记、缺父目录提示、无关建议消失及 1000 / 74 / 500 条内联展示计数。

以下问题描述及证据保留为修复前验收输入，最终接受与否待用户复验。

## 汇总清单

### 1. 截断提示中的计数是"已收集量"，易被误读为总数

- Glob：1200 个文件的目录返回 `[Output truncated: 1001 files total (collection stopped at the result limit), showing first 1000]`——真实总量 1200，"1001"只是收集停止点。
- Grep：300 行匹配返回 `[Output truncated: 88 lines total, 20230 bytes; showing first 86 — exceeds 20000 byte limit]`——"88"是截断时已构建的输出行数，非匹配总数。
- `folder_operations list`：`[Output truncated: 1200 total entries, showing first 500]` 计数正确，但三种工具的措辞不一致。
- 建议：统一为"已收集/已输出 N，总量未知或 M"的口径，或明确写 `≥1000`。

### 2. `folder_operations create recursive=false` 对父目录缺失的报错语义不准

- 实测：`create("/tmp/peri-ws-test/single-level/deep", recursive=false)`（父目录不存在）→ `File not found. Verify the requested path.`
- 实际原因是没有递归创建父目录，错误文本却指向"路径不存在"（且未提示 `recursive`），用户可能误以为路径拼写错误。
- 位置：`peri-middlewares/src/tools/filesystem/folder.rs`（未逐行复核具体分支）。

### 3. Edit 的 `old_string not found` 错误附带路径建议，语义不搭

- 实测错误尾部：`Did you mean one of these path? • dup.txt`。
- 该候选机制来自 `peri-agent/src/error_suggest`（`format_test.rs` 有断言），面向"路径拼写纠错"场景；Edit 的失败对象是**文本片段**，给出文件路径候选会让模型困惑。
- 建议：Edit 场景抑制该建议，或替换为 `build_not_found_hint`（`edit.rs:25-83`）已提供的行号级提示。

### 4. Write 可覆盖只读文件（chmod 444）

- 实测：对 `chmod 444` 的文件执行 Write 覆盖 → `Wrote 1 line` 成功。
- 原因：原子写（`write_sandbox.rs` 与 `write.rs` 均走 temp + rename 语义），rename 只需父目录可写，目标文件权限位不构成保护。
- 这不是缺陷本身（与描述 "Uses atomic write" 一致），但描述未说明"只读文件同样可被覆盖"，涉及权限安全的用户可能误解。

### 5. Glob 搜索根为符号链接时会被解析后搜索

- 实测：Glob path=`/tmp/peri-ws-test/linkdir`（→ `subdir`）`*.rs` → 返回 `/private/tmp/peri-ws-test/subdir/a.rs`（真实路径）。
- 与描述 "Symbolic links are not followed during the walk: symlinked files and directories are skipped" 不矛盾（root 非 walk 中相遇），但 "globbing a project root won't pull in trees linked from outside the workspace" 的表述对有链接根的调用者可能误导。
- 建议：描述补一句"根路径若为符号链接会先 canonicalize"。

### 6. `deep_scan` 树形渲染两处瑕疵

- 实测 `/tmp/peri-ws-test`（max_depth=2）：`deep/` 分支插入后，兄弟节点连接符错位（`deep/nested/` 之后 `dup.txt` 等仍以 `├──` 平铺，视觉层级断裂）；符号链接 `linkdir` 显示为普通文件 `📄 linkdir (24 bytes)`（24 为链接目标字符串字节数），未标注为链接。
- 位置：`peri-middlewares/src/tools/filesystem/folder.rs`（渲染部分）。

### 7. `list` 输出顺序未定义

- 实测 1200 文件目录：既非字典序也非 mtime 序（近似目录读取顺序）。
- 描述未承诺排序，但同一次测试中 Glob 明确按 mtime 倒序，用户可能默认两者一致。建议在描述中注明顺序不保证，或统一排序口径。

## 验证与未验证项

- 已验证：上述 7 项均有本次会话的原始输出（沙箱 `/tmp/peri-ws-test` 保留至清理前可复核）。
- 未验证：第 2、6 项的具体实现分支未逐行复核，仅依据可观察输出归因到 `folder.rs`；各平台（Windows）表现未测。

## 涉及文件

- `peri-middlewares/src/tools/filesystem/folder.rs`（第 2、6、7 项）
- `peri-middlewares/src/tools/filesystem/descriptions/glob.md`（第 5 项）
- `peri-middlewares/src/tools/filesystem/descriptions/write.md`（第 4 项）
- `peri-agent/src/error_suggest/`（第 3 项，路径建议机制）
- 截断计数（第 1 项）：`glob.rs`、`grep.rs`、`folder.rs` 各自的三处提示文本

## 状态记录

| 日期 | 状态 | 说明 |
| --- | --- | --- |
| 2026-09-28 | Open | 由 workspace 工具深度测试创建；各项可独立处理，建议随相关文件改动顺带修复 |
| 2026-09-28 | 已修复，待用户验收 | 本轮修复、用户裁决与验证见文首；原 Open 记录为历史输入 |
