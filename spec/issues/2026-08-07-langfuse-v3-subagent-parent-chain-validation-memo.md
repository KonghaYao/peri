# Langfuse v3 subagent 父链异常：发布切点前的线上验证备忘

**状态**：Open（待版本切点确认）  
**优先级**：中  
**创建日期**：2026-08-07  
**审计时间点（UTC）**：2026-08-07T06:15:44Z 至 2026-08-07T06:16:02Z  
**关联 issue**：`2026-08-05-langfuse-batcher-drops-during-slow-flush.md`、`2026-08-05-langfuse-subagent-attribution-stack-lifetime.md`

## 目的

记录 v3 重构完成后的首次线上结构审计结果，并保留一个关键不确定性：查询窗口可能包含修复前进程或旧部署写入的 trace。现阶段不得仅凭该窗口将异常判为 v3 当前构建的回归。

## 审计范围与方法

- 数据源：项目 `.env` 配置的 Langfuse 实例；凭据仅加载到审计子进程，未显示、记录或传输。
- 查询窗口：审计时刻向前 24 小时。
- 样本：按时间倒序读取前 50 条 trace 及其 observation 元数据。
- 只投影：trace/observation ID、类型、名称、parent ID、时间戳、Generation 字段存在性、usage 完整性与固定错误级别。
- 禁止读取、输出或保存：prompt、模型 input/output、工具结果、原始错误、任何密钥。

## 观察结果

### Generation 写入健康度

- 994 个 Generation 都具有 ID、trace ID、parent observation ID、名称、起止时间、模型、output、metadata、usage 与 usage details。
- 所有 Generation 都具有 `usage.input`、`usage.output`、`usage.total`；未发现 usage 三要素缺失。
- API 返回 4 个 Error Generation；审计未读取原始错误内容。
- `sessionId`、`completionStartTime`、`promptName`、`promptVersion` 未在此样本中出现。
- API 返回的 `modelParameters`、`costDetails`、`environment` 字段存在，不能单独证明当前 Rust bridge 主动写入：本地 `GenerationBody` 构造器仍将这些字段设为 `None`。

### Subagent 观测结构异常

- 50 条 trace 中有 4 条包含 subagent，共 7 个 child `AGENT` observation。
- 7 个 child `AGENT` 都具有开始与结束时间；未发现 parent 图环；每个 child 都有后代 observation。
- 其中 5 个 child `AGENT` 的祖先链未能到达 `agent-run`，表现为：

  ```text
  child AGENT → stage-act span → 缺失的 parent
  ```

- 同一窗口中有 59 个 parent ID 未出现在所属 trace 的 observation 列表：57 个 `SPAN`、1 个 `AGENT`、1 个 `EVENT`。
- 两条含 subagent 的 trace 结构完整，child `AGENT` 可经 `stage-act` 到达 `agent-run`。

## 当前判定

**待确认，不作为 v3 当前构建回归结论。**

该窗口中的 Generation `version` 均为 `0.2.0`，不足以辨别修复前、修复后或混合部署产生的数据。已知历史问题与本次症状相关：

- batcher 慢 flush 期间的 `DropNew` 可丢弃后入队的 observation；
- 旧 subagent attribution 实现曾导致 stage/AGENT 父链错误；
- 当前本地 bridge 与 tracer 图测试均通过，但 `FakeLangfuseSession` 不模拟线上 backpressure/drop。

因此需要以明确的**新部署切点**和可识别的构建标记重新采样，才能判定是否仍在发生。

## 复验条件

1. 记录部署完成的 UTC 时间、构建 commit/版本，并确认运行进程已切换。
2. 从该切点之后启动全新的主 agent + 至少一个并行 subagent 场景。
3. 等待 trace 完整 flush 后，仅用安全元数据检查：
   - 每个 child `AGENT` 都可沿 parent 链到达 `agent-run`；
   - 每个 observation 的 parent ID 要么为 trace 根，要么存在于同 trace observation 集合；
   - child `AGENT` 的时间范围覆盖全部子内容；
   - 图无环；
   - Generation 必须有完整的 usage 三要素及 trace/parent/timing/model 链接。
4. 若新切点之后仍复现，再将本 issue 升级为确认的 v3 回归，并优先关联 batcher 丢弃遥测（按 trace ID、事件类型、drop reason）。

## 待办

- [ ] 记录 v3 当前部署的 commit、版本和 UTC 切点。
- [ ] 生成一个明确发生在切点之后的并行 subagent trace。
- [ ] 运行安全结构审计并附上聚合结果。
- [ ] 若父链仍断裂，补充 capacity/slow-flush 集成测试和 parent-existence 图断言。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|----|----|--------|------|
| 2026-08-07 | — | Open（待版本切点确认） | agent | 记录首次 v3 后线上审计；历史数据与当前部署无法由版本字段区分，暂不下回归结论。 |
