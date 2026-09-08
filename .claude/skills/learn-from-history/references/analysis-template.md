# 分析单元报告模板

`run_history.py` 会为每个 unit 生成自包含 prompt；本文件只定义对应 Markdown 报告的最低结构。机器完成证据写入同名 `.json` sidecar，不靠本报告自报。

```markdown
# 历史分析单元 — unit-NNN

- **Snapshot run**：<run_id>
- **Unit 状态**：sidecar `status=analyzed`
- **日期**：<一个或多个 updated_at 日期>
- **Thread**：N
- **消息**：M
- **降级输入**：截断 N / 解析失败 M / 已复核 K

## Thread 结果

### <thread id> — <用户意图>

- **结果**：成功 / 部分完成 / 失败 / 取消 / blocked
- **关键证据**：`extracted/<day>/<thread>.txt :: <可定位摘录或事件>`
- **反证**：`extracted/<day>/<thread>.txt :: <成功对照或不支持结论的事件>`；没有则写“未发现”
- **成功模式**：真正促成收敛的策略
- **改进分类**：rule gap / active issue covered / skill gap / execution deviation / external blocker

## 跨 Thread Findings

### F-001 — <简短结论>

- **失败模式**：跨场景重复出现的可观察模式
- **根因**：为什么失败，而不只是发生了什么
- **证据**：`extracted/<day>/<thread>.txt :: <可定位摘录或事件>`
- **反证**：`extracted/<day>/<thread>.txt :: <成功对照或不支持结论的事件>`
- **频次**：N/M，必须带分母
- **影响**：high / medium / low
- **置信度**：high / medium / low
- **事实源**：建议修改或已覆盖的路径
- **目标面**：standard / module_guidance / active_issue / skill / tool_description / tool_implementation / middleware / subagent / memory / configuration / implementation / test / external / none
- **归属理由**：为何由这个最窄层负责，而不是把建议都塞进 prompt 或规则
- **预测修复**：下一轮应出现的可观察改善，至少一项
- **回归风险**：可能受损的既有成功模式；若未发现，写明搜索范围和理由
- **验收**：分别列出 `target` 检查与 `preserved_success` 检查，各至少一项且原文不重复

## Blocked

- <缺失输入、截断无法复核或外部阻塞；没有则写“无”>
```

不要把正常工具失败字样机械当成问题；区分预期负例、环境阻塞、真实回归和用户纠正。单次事件默认不升级为稳定规则。
