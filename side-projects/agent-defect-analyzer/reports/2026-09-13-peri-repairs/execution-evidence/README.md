# 执行证据：从真实命令到任务评价

对应 `PERI-20260913-VERIFICATION-EVIDENCE`，原 issue 位于基线 commit `73df17f7`。当前实现进入最终独立复审，关闭和提交信息由总报告更新。

## 已观察到的链路结果

真实 Bash 命令产生 20KB 输出，末尾包含 `TAIL_FAILURE` 并以 7 退出。经过生产 `dispatch_tools` 与 canonical transcript 序列化后，正文受输出预算限制，但 `execution.status=failed`、`exit_code=7`、`output_truncated=true` 和完整输出引用仍保留。协调者读取原始临时文件，确认当时可读、共 20,028 字节，末尾包含失败标记与退出码。

`rust-bash-failure-capture.json` 保存真实 Rust 消息的脱敏副本；仅替换临时路径及正文中的同一引用。`rust-capture-provenance.json` 保存原始与脱敏 hash、引用内容 hash、字节数和核验范围。后一次完整测试导出与冻结样本在规范化生成的消息 ID、临时路径后相等。分析器真实 `normalizeMessage` 读取该样本得到 failed / 7，`parseIssues=[]`。

这是一份合成命令的真实执行产物，既不是手写同形 JSON，也不是生产用户任务。其他状态由 Rust 场景测试和 TypeScript 状态矩阵分别覆盖；不能称全部状态都已有历史生产 capture。

## 事实与解释

- legacy `invoke` / `dispatch` 字符串结果没有 execution metadata 时保留未知。正文中仿造的成功标记不能生成 typed 事实。
- 后台启动与前台等待超时后转入后台分别保留 `running`、`running_after_timeout`；工具调用已返回不等于后台任务已结束。
- live、模型和回放通过受预算限制的文本投影看到执行摘要；canonical 字段不依赖正文尾部是否保留。PTC 继续使用字符串接口，不伪造独立嵌套 transcript。
- 非法字段、状态矛盾和双写冲突保留质量告警，执行状态降为 typed unknown；不能进入已知成功/失败计数。
- packet 与 evidence 默认只给引用/任务 ID 是否存在，按需内容导出才包含原值，分析器不会自动打开历史路径。

新增 Bash typed 结果会将非零退出映射为工具错误。历史 `is_error` 原样保留，不从退出码文字回填。因此跨生产者版本的工具错误率变化还包含观测契约变化，不能直接解释为 Agent 质量变差或改善。执行状态覆盖率、工具显式错误率、实际运行的测试数和任务验收应分别核对。

## 验证层次与兼容性

`legacy-quality-summary.json` 是只读全库兼容检查的安全摘要；覆盖对象是所有项目和日期，告警并未隐藏。它没有测量新执行字段的生产覆盖率，也不能给出修复效果。

分析器最终 `bun run typecheck` 与 `bun test` 均以 0 退出，57 个测试通过，0 失败，265 个断言；记录在 `analyzer-validation.json` 与对应原始测试输出。任务事实包和 review schema 升为 2；旧包及绑定它的评审应按当前 README/TASK-EVALUATION 重新导出并评审，不能只替换版本号或复用旧 hash。默认 metadata 包允许引用存在标记，但禁止原始引用和 task ID；缺正文的评审只允许未知结论。

退出码 0 证明命令成功退出，尚需核对目标用例是否实际运行。完整输出引用当时可读不承诺以后仍存在；这轮数据也不证明历史缺失终态的任务通过或失败。
