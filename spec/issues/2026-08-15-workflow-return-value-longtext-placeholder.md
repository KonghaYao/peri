# Workflow return_value 长文本被 `${label}` 占位符掏空：state.json 结果不可直接读

**状态**：Won't fix（2026-08-15 确认：设计行为，非缺陷；脚本用法问题）
**优先级**：低（已确认为设计行为，仅需正确用法）
**类型**：使用问题
**创建日期**：2026-08-15
**来源**：deferred-tool 修复回归验证（自编 workflow run `01a0045e-b31c-7ec3-8deb-c33977f33cef`）

## 结论（2026-08-15 定案）

- **设计行为，非缺陷**：`extract_long_texts`（threshold=200）将 return_value 中超阈值字符串外置到 `outputs/<label>.txt` 并原位替换 `${label}`，为既有设计（journal.rs doc comment 明确）；数据完整落盘，未丢失。
- **使用问题**：脚本不应将 agent 全文直接放入 return_value 顶层字段。正确用法：`return_value` 返回摘要/结构化短数据；长文本按需截断（`r1.slice(0, 200)`）或经 `log()` 入 journal 查看。短结果（<200B）不触发提取，`state.json` 直接可读（对照 run `01a003c6` 实证）。
- **不修改占位符协议**：维持 `${label}` 格式与现有机制，不做协议/消费端扩展（经 advisor 咨询后由用户定案）。
- 已知提示（不改动，记录在案）：`${label}` 与 JS 模板字符串插值同形，首次查看易误判；含 `/` 或 `..` 的 key 兜底 `unnamed.txt`，多字段碰撞会覆盖。

## 背景：本次回归验证结论（前置 issue `2026-08-15-workflow-deferred-tool-missing.md` 已修复）

自编 workflow `regression-verify-toolview`（并行 3 agent 只读核查修复代码）验证结果：

1. `SearchExtraTools select:Workflow` 命中（score 1.0，schema 完整）——原"发现面"恢复；
2. `ExecuteExtraTool(Workflow)` 派发成功，异步返回 run_id——"执行面"恢复；
3. 3 个 agent 正常执行（journal.jsonl 三条 `kind: ok` 记录完整，含代码行号引用）；
4. `state.json` 落盘完整（status=completed，started/finished 时间齐全）；
5. **异常**：`state.json` 的 `return_value` 为 `{r1: "${r1}", r2: "${r2}", r3: "${r3}"}`。

## 症状

- 脚本 `return { r1, r2, r3 }`（shorthand，语法正确，`state.json` 的 `script` 字段可证）执行后，`return_value` 中每个 agent 输出（827–1856 字节）被替换为 `${r1}` / `${r2}` / `${r3}` 字符串字面量。
- `${label}` 与 JS 模板字符串插值语法**完全相同**，直接读者无法区分"占位符"与"真实模板字符串值"；首次查看即被误判为脚本变量插值失败（本次排查误判过程可证）。
- `state.json` 无任何指向 `outputs/` 目录的提示字段。
- 对照：12:55 修复验证 run `01a003c6`（echo-hello-workflow-test，输出 <200 字节）`return_value` 完整保留，未触发提取——机制是长度阈值触发，非偶发。

## 影响面

- 任何直接读取 `state.json` 的消费端（用户手查、TUI `/workflows` 面板、CLI read、上层工具）看到的是占位符集合而非工作流结果；若消费端不读取 `outputs/` 目录，结果实际不可见。
- 阈值 200 字节（runner.rs:598 硬编码）过低：任何有实质内容的 agent 输出（>200B）都会触发提取，`return_value` 几乎总是只剩占位符，形同虚设。
- 语义欺骗：占位符格式与模板字符串/未求值变量相同，误导排查方向（本次误判即为实例）。

## 根因（已定位）

`peri-workflow/src/journal.rs:185` `extract_long_texts`（runner.rs:591-599 在 RunDone 后调用，threshold=200）：

- 设计意图：将 return_value 中超阈值字符串写入 `<run_dir>/outputs/<label>.txt`，原位替换 `${label}`（doc comment 明确此为设计行为）。
- **数据未丢失**：`outputs/r1.txt` / `r2.txt` / `r3.txt` 均完整落盘（827–1856 字节全文）。
- 机制说明（非缺陷，记录在案）：全仓（Rust + 本地 bundle 0.1.1）无 `${label}` → outputs 内容还原逻辑，消费端需自行读取 `outputs/`；`${}` 格式与 JS 模板插值同形，首次查看易误判。
- 已知边界：label 为对象路径（`r1`、`a.b`、`[0]`）；含 `/` 或 `..` 的 key 全部兜底写入 `unnamed.txt`（journal.rs:172-176），多字段碰撞覆盖（定案：不改动）。

## 曾评估的修复方向（advisor 咨询后定案：不实施）

- 曾考虑：占位符改自描述格式 + state.json 增加 outputs 元数据 + 阈值治理（~4KB/32KB）+ CLI read 展示层还原 + label 唯一化（advisor 建议的方案 D）。
- **定案：不实施**。机制为设计行为，正确解法在使用侧（脚本返回摘要），而非协议侧改动。

## 复现

任意脚本 `return { a: ">200B 文本" }` 跑一次 workflow：`state.json` 的 return_value 出现 `${a}`，全文在 `outputs/a.txt`。此为预期行为。
