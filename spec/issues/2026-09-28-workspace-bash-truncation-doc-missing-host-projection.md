# [P2] Bash 截断描述只登记 2000 行/65000 字节，未覆盖 10000 字符宿主投影层

> 2026-09-28 范围更新：`local-mcp-server` 已按用户裁决退役，代码与独立构建入口已删除。本文涉及旧项目的路径、命令、比较和后续复用建议仅作为历史记录，不再作为实施或验收要求；当前 Workspace MCP 入口与验证见 [主项目代码索引](../../docs/code-index/peri-middlewares.md)。

**状态**：已修复，待用户验收
**优先级**：P2（文档不完整，行为本身是有意设计；代价是模型与用户对 "head + tail preserved" 的预期在常见区间不成立）
**类型**：文档缺陷（工具描述与实现不一致）
**创建日期**：2026-09-28
**来源**：2026-09-28 workspace MCP 工具深度测试（Peri 内置 `workspace` 实例，HEAD `4a4307bb`，macOS 26.5.1 arm64）
**最后核查**：2026-09-28（实测 + 实现注释核对，均已复核）

## 本轮修复与验收状态

已同步实际 Bash 描述中的模型输出预算、head-only 分支、较短输出的 head/tail 分支，以及完整捕获输出的落盘读取方式；后台完成通知明确交付文件引用。未修改截断实现。

`./dev.sh -p` 真实调用 3000 行输出：内联出现 10000 chars 截断提示，随后通过 Read 读取完整输出文件，确认 `line 3000` 保留。该组 5 项检查全部通过，进程 exit 0。随后短行补验发现第三层 MCP 2000 行投影仍会裁掉内联尾标记，模型明确报 FAIL；已再次修正文案，说明 head/tail 仅属内部格式化，外层落盘可能是中间结果。最终再次通过 `./dev.sh -p` 实际沿两级文件引用读取，确认原始第 3000 行 `TAIL_MARKER` 保留，描述与恢复检查均 PASS，进程 exit 0。截断行为保持不变。

以下问题描述及证据保留为修复前验收输入，最终接受与否待用户复验。

## 问题描述

`peri-middlewares/src/middleware/descriptions/bash.md:31-33` 的 "Output handling" 宣称：

```
- Output exceeding 2000 lines is truncated (head + tail preserved)
- Output exceeding 65000 bytes is truncated
```

实测 3000 行输出（约 24 KB，**未超** 65000 字节）被截断为 **head-only**，标记为 `[Output truncated at 10000 chars]`，没有任何 tail。

## 证据

### 实测（可复现）

```bash
seq 1 3000 | sed 's/^/line /'
```

结果：输出在 `line 1037` 附近停止，随后是

```
[Output truncated at 10000 chars]
[Full output saved to /var/folders/.../peri-tool-output-*.txt ...]
```

无 tail 段，未出现 2000 行 head/tail 保留形态。

### 实现核查（已复核）

- `peri-middlewares/src/middleware/terminal.rs:173-188` 的 `bounded_bash_output`：输出超过 `BASH_OUTPUT_LIMIT - EXECUTION_SUMMARY_BUDGET`（10000 - 512）字符时，直接走 10000 字符 head-only 截断（`truncate_bytes` + `[Output truncated at {BASH_OUTPUT_LIMIT} chars]` 标记），**跳过** 2000 行 head/tail 与 65000 字节路径；`terminal.rs:174` 常量 `BASH_OUTPUT_LIMIT: usize = 10_000`，`terminal.rs:358-360` 的 `output_char_limit() -> Some(10000)`。
- 设计旁证：`side-projects/local-mcp-server/src/tools/bash/limits.rs:1-11` 明确登记"两层限额"：工具内部为 2000 行/65000 字节（超出时落盘全量 + head/tail 各 1000 行），宿主投影层为 10000 字符（追加 `[Output truncated at 10000 chars]`）。**该 10000 字符层是有意设计**，缺陷在描述未登记它。

## 影响

- 在 9488 ~ 65000 字符这一常见区间（例如 3000 行日志），描述承诺的 "head + tail preserved" 不成立，用户与模型看到的只有头部；排查"为什么没有 tail"时无据可依。
- 描述未提示该区间会落盘完整输出到临时文件（实测提示中有该信息，描述中缺失）。

## 修复方向（建议）

更新 `descriptions/bash.md` 的 "Output handling"，登记三层限额的实际语义，例如：

- 输出超过 10000 字符（宿主投影层）：head-only 截断，追加 `[Output truncated at 10000 chars]` 并落盘完整输出；
- 未触发前一层且超过 2000 行：head + tail 各 1000 行并落盘；
- 超过 65000 字节：字节上限截断。

实现已按上述分层运作（`bounded_bash_output`），本 issue 只涉及描述同步，不改行为。

## 验证与未验证项

- 已验证：3000 行场景的实际截断形态与标记文本；`bounded_bash_output` 分支逻辑；limits.rs 两层限额注释。
- 未验证：65000 字节与 2000 行 head/tail 分支在字符数 ≤ 9488 条件下的实际表现（本次未构造该区间的样本）；后台任务取回输出的截断形态是否一致。

## 涉及文件

- `peri-middlewares/src/middleware/descriptions/bash.md`（待更新，`:31-33`）
- `peri-middlewares/src/middleware/terminal.rs`（行为源：`bounded_bash_output` `:173-188`、`output_char_limit` `:358-360`）
- `side-projects/local-mcp-server/src/tools/bash/limits.rs`（两层限额的设计登记，仅作旁证）

## 状态记录

| 日期 | 状态 | 说明 |
| --- | --- | --- |
| 2026-09-28 | Open | 由 workspace 工具深度测试创建；修复内容为描述同步（不改行为） |
| 2026-09-28 | 已修复，待用户验收 | 本轮修复、用户裁决与验证见文首；原 Open 记录为历史输入 |
