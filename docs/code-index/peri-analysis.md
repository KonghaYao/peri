# Peri 数据分析代码索引

Peri 运行数据的离线分析归 `side-projects/agent-defect-analyzer/`，本地浏览归
`side-projects/peri-db-viewer/`。两个项目独立于根 Cargo workspace。
使用状态、命令和迁移说明分别见 [分析器 README](../../side-projects/agent-defect-analyzer/README.md)
与 [查看器 README](../../side-projects/peri-db-viewer/README.md)。

## 数据与职责

- 数据库与消息格式的生产者位于 `peri-resources` 和 `peri-acp-types`，先按
  [resources 索引](peri-resources.md) 与 [协议类型索引](peri-acp-types.md) 核对当前契约。
- 分析器的 `src/data/` 是只读读取和消息格式归一化入口。分析不迁移或修复生产库。
- 工具调用事实与 provider 上下文视图分开处理；历史工具执行不能因当前 `excluded`
  标记被丢弃。继承消息与双写消息需防止重复统计。
- 分析方法与复核边界见 [ANALYSIS.md](../../side-projects/agent-defect-analyzer/ANALYSIS.md)。
  报告输出改进候选，确认缺陷需回到源消息、当前代码和回归测试。
- 可继承的研究方法由 [auto-data-researcher](../../.claude/skills/auto-data-researcher/SKILL.md)
  维护。该目录是仓库内的维护源；会话若加载 `.agents/skills/auto-data-researcher/` 副本，
  更新时同步对应文件并检查一致性，避免旧方法继续被加载。
- 任务级评价的口径与 subagent 复核流程见
  [任务有效性研究](../../side-projects/agent-defect-analyzer/TASK-EVALUATION.md)。
  评审标签独立于持久化事实，缺失证据与评审分歧须保留。
  编排与提示词实验方法由 [agent-task-evaluator](../../.claude/skills/agent-task-evaluator/SKILL.md)
  维护，活动 `.agents/skills/` 副本采用上面相同的同步规则。
- 持续研究方向 [任务关注点漂移与纠正吸收（RQ-FD-001）](../../side-projects/agent-defect-analyzer/FOCUS-DRIFT.md)
  维护用户纠正、返工、有效约束保留与后续验收的专题口径；运行证据保存在本地 `output/`。

## 入口

| 意图 | 主文件与符号 |
| --- | --- |
| 运行离线分析命令 | `agent-defect-analyzer/src/cli.ts`：`parseCliArgs`、`run` |
| 探测 SQLite 契约、读取并归一化消息 | `agent-defect-analyzer/src/data/loader.ts`：`DataLoader`、`normalizeMessage` |
| 检查数据质量与观测缺口 | `agent-defect-analyzer/src/reporting/quality.ts`：`inspectDatabase` |
| 工具配对、`is_error` 工具错误、执行证据覆盖/状态、重复调用和输出体积 | `agent-defect-analyzer/src/analysis/metrics.ts`：`analyzeDatabase`；执行证据由 `src/data/loader.ts` 归一化 |
| 生成统计与候选报告 | `agent-defect-analyzer/src/reporting/report.ts`：`reportDatabase` |
| 校验口径并比较两个报告 | `agent-defect-analyzer/src/reporting/compare.ts`：`compareReportFiles`、`compareReports` |
| 确定性抽样与有限上下文回查（含共享脱敏 execution projection） | `agent-defect-analyzer/src/research/evidence.ts`：`sampleThreads`、`evidenceForMessage` |
| 分层抽样、带正文指纹的任务事实包 schema 2（含独立 execution facts） | `agent-defect-analyzer/src/research/task-packets.ts`：`sampleTaskPackets`、`exportTaskPacket`、`computeTaskPacketHash` |
| 校验任务评审、保留分歧并按 case 汇总 | `agent-defect-analyzer/src/research/task-reviews.ts`：`reviewTaskFiles`、`writeTaskReviewReport` |
| 查看本地会话与工具记录 | `peri-db-viewer/src/server.ts`：`startServer`；`src/app.ts`：`createApp` |
| 查询 API 与共享解析适配 | `peri-db-viewer/src/routes/api.ts`：`registerApiRoutes`；`src/data_adapter.ts`：`ViewerDataAdapter` |

表中两个项目的相对路径均位于 `side-projects/`。新增分析先复用共享读取层，
不要另写解析生产消息格式的独立脚本。具体过滤器、输出结构与用法由各项目 README 维护。

## 相关 side project 的边界

| 项目 | 角色 | 与行为分析的关系 |
| --- | --- | --- |
| `agent-defect-analyzer` | 持久化会话的离线分析 | 统一统计与证据入口 |
| `peri-db-viewer` | 本地会话浏览与搜索 | 按证据 ID 查阅上下文 |
| `llm-gateway` | LLM 请求/响应记录 | 独立遥测来源；没有可验证关联键时不能直接合并为同一调用 |
| `git-stats` | Git 变更统计 | 辅助了解代码演进，不衡量 agent 任务成败 |
| `md-scan-matrix` | 文档静态扫描 | 工程质量辅助信息，不是会话行为事实 |

历史专题脚本、旧 skill 和旧报告可能包含过时路径、格式假设或固定样本。先运行当前入口的质量检查，
不得把历史数字、估算 token 或人工图表当作本次分析结果。测试命令以各项目 README 为准，
测试范围遵循 [testing.md](../standards/testing.md)。
