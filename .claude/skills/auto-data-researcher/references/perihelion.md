# Peri 会话研究入口

仅在研究 Peri 会话与工具行为时读取。本文件是路由；字段和命令以当前代码、测试及项目文档为准，不以本 skill 冻结 schema、数字或工具名称。

## 开始位置

先定位 Perihelion 仓库根目录，读取以下文件；路径不存在时查当前 code-index 和 manifest，不能为迎合旧文档而补造入口。

- `docs/code-index/peri-analysis.md`：分析项目、消息生产契约与代码入口。
- `side-projects/agent-defect-analyzer/README.md`：可运行命令、范围参数、输出和迁移说明。
- `side-projects/agent-defect-analyzer/ANALYSIS.md`：观测含义、不可推导的指标及复核边界。

已有读取器是 `agent-defect-analyzer/src/data/loader.ts`，行为指标位于 `src/analysis/metrics.ts`。修改统计前核对生产者与契约测试，不另写一份 legacy/V1 消息解析器。

## 选择命令

在 `side-projects/agent-defect-analyzer` 下运行，参数和上限先核对 README 与 `src/cli.ts`。数据库位置由用户或本机配置确定；默认位置可用不等于数据完整或符合研究范围。

| 任务 | 入口 | 使用时核对 |
| --- | --- | --- |
| 数据体检 | `inspect --db DB --out output/quality` | 结构、可选字段、格式覆盖、双写、继承、解析/配对质量；命令成功不等于所有 check 通过 |
| 计算行为候选 | `report --db DB --out output/behavior` | 主/子会话、hidden、创建窗口；默认可见主会话；工具错误以已配对且状态已知的结果为分母 |
| 确定性抽样 | `sample --db DB --out output/sample --seed SEED --size N` | 候选集合、最小消息数、seed 与实际阅读数量；只输出样本不等于完成案例复核 |
| 回查邻近证据 | `evidence --db DB --out output/evidence --thread THREAD_ID --message MESSAGE_ID` | 自有消息的完整 ID、radius、截断与 omissions；正文需 `--include-content` |
| 比较研究结果 | `compare --baseline BASELINE_JSON --candidate CANDIDATE_JSON --out output/comparison` | 先生成兼容报告；零事件规则、缺席工具、两侧分母与版本；差异仅为描述性证据 |

这些是 `bun run` 的子命令。例如 `bun run inspect --db "$PERI_DB_PATH" --out output/quality`；运行前给 `PERI_DB_PATH` 设置已确认的本机路径，勿直接使用占位值。

命令结束后核对退出状态并读取对应 JSON 中的质量、来源与范围。当前 `report` 用会话创建时间筛选，再包含该会话全部持久化历史，不能表述为“这几天发生的工具事件”。主会话与子会话、隐藏与非隐藏是不同维度；子会话通常需要显式包含 hidden。

`report` 指纹覆盖分析输入；`sample` 与 `evidence` 的指纹分别覆盖声明的元数据集合，不能要求不同 scope 的指纹相等。对相同范围与算法的重复产物，应检查来源、指标与分母一致；若不同，先解释数据或口径变化。

## 阅读上下文与延伸调查

需要浏览详情时使用 `side-projects/peri-db-viewer`，按其 README 启动。查看器总览统计全库自有消息，详情可展示继承上下文；离线报告还会排除有冲突的配对，不能直接将两边总数对比。搜索或最近错误列表用于定位，长会话需继续分页读取。

错误先按证据区分目录/名称不匹配、确定性输入诊断、运行或模型流中断、明确取消等；无法确定就保留未知，不能把某个工具名永久绑定到一种根因。检查后续同类操作与原任务反馈，普通后续成功活动只能说明活动继续了。

需要成本、耗时、完成率或 provider 根因时，先检查是否有直接观测字段和可验证的关联键。当前缺失就列出所需遥测，不能用字符数、会话更新时间或模糊文本分类代替。Langfuse、网关日志与 Git 数据可作旁证，但关联契约需要独立核验。

算法变更按该项目的 `bun run typecheck`、`bun test` 验证，fixture 不读取生产库。真实数据报告另行只读取数，默认产物放本地忽略的 `output/`；选择提交的研究成果仅含统计、定位信息和安全摘要。
