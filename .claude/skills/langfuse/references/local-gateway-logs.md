# 本地网关日志分析

这是 [Langfuse skill](../SKILL.md) 的本地数据分析方式：分析 llm-gateway 产生的请求/响应日志，不需要 Langfuse 凭据或网络。

以下命令从仓库根目录执行；建议用 `--dir <data-dir>` 显式指定用户授权分析的日志目录。从其他目录调用时，将脚本路径替换为绝对路径。仅 `llm-log-query.mjs` 支持默认目录查找；`context-growth.mjs` 必须提供 `--dir` 和 `--session`。

原始日志不保证已脱敏，禁止输出凭据（包括部分前缀）。不要使用 `show --headers` 展示原始 headers；`--body`、`--messages`、`--stream`、`--full`、diff、缓存深诊及上下文轨迹可能输出消息或工具参数，只能对已确认脱敏的数据使用。先看摘要，不把完整日志复制到报告或外部服务。

## 日志结构

每个请求对应一个目录，命名格式 `YYYY-MM-DD_HH-MM-SS-mmm_NNNN`：

### 新格式（当前）

```
data/
└── 2026-05-20_03-56-28-921_0014/
    ├── request.json      # { headers: {...}, body: {...} }
    └── stream.log         # SSE 流式响应原文（含 usage 数据）
```

### 旧格式（兼容）

```
data/
└── 2026-05-14_10-30-15-123_0003/
    ├── request.json      # { headers?: {...}, body?: {...} } 或裸 body
    ├── response.json      # JSON 响应（非流式）
    ├── stream.log         # SSE 流式响应原文
    └── log.txt            # 终端格式的人类可读日志
```

**request.json 格式**：`{ "headers": {...}, "body": {...} }`。body 包含以下字段：
- `model`：模型名称（如 `deepseek-v4-pro`）
- `system`：system prompt 数组（Anthropic 格式），每项含 `text` + 可选 `cache_control`
- `messages`：消息数组
- `tools`：工具定义数组（Anthropic 格式用 `name`，OpenAI 格式用 `function.name`）
- `thinking`：推理配置（如 `{"type": "enabled", "budget_tokens": 8000}`）
- `output_config`：输出配置（如 `{"effort": "high"}`）
- `stream`：是否流式（`true`）
- `max_tokens`：最大输出 token

`headers` 中的 `x-session-id` 可按 session 追踪同一 agent 的多次请求。`host` 头用于推断 API 路由（如 `api.deepseek.com` → `deepseek`）。

**stream.log 格式**：SSE 事件流，包含：
- `message_start`：初始 usage（`input_tokens`、`cache_read_input_tokens`、`cache_creation_input_tokens`、`output_tokens=0`）
- `content_block_start/delta/stop`：thinking、text、tool_use 内容块
- `message_delta`：最终 usage（含实际 `output_tokens`）+ `stop_reason`
- `message_stop`：流结束标记

工具自动从 `stream.log` 的 `message_delta` 事件提取 token usage 数据，无需 `response.json`。

## 分析工具

`scripts/llm-log-query.mjs` 提供以下子命令，用 `bun .claude/skills/langfuse/scripts/llm-log-query.mjs <command>` 运行：

### list — 列出请求摘要

```bash
bun .claude/skills/langfuse/scripts/llm-log-query.mjs list [--dir ./data] [--limit 20] [--model NAME] [--session ID] [--route openai|anthropic|deepseek] [--after YYYY-MM-DD] [--before YYYY-MM-DD] [--errors]
```

输出表格：序号 | 请求ID | 时间 | 路由 | 模型 | Session | 消息数 | 状态 | 延迟

### show — 查看单个请求详情

```bash
bun .claude/skills/langfuse/scripts/llm-log-query.mjs show <request-id> [--dir ./data] [--body] [--messages] [--tools] [--stream]
```

- 默认显示摘要（模型、Session、状态、延迟、token 用量、thinking 配置、output_config），不输出 headers
- `--body` 显示完整请求体
- `--messages` 显示 system blocks + 消息列表（role + 内容前 100 字），system blocks 标注 `[cached]`
- `--tools` 显示工具定义列表，标注 `[cached]` 的 cache_control 状态
- `--stream` 解析 stream.log 中的 SSE 事件
- 无 `response.json` 时，自动从 `stream.log` 提取响应（stop_reason、thinking、content、tool_calls）

### session — 追踪一个 session 的完整请求链

```bash
bun .claude/skills/langfuse/scripts/llm-log-query.mjs session <session-id> [--dir ./data] [--full]
```

按时间序列展示同一 session 的所有请求，显示每轮的角色和工具调用。`--full` 输出完整消息内容。

### diff — 对比两个请求的差异

```bash
bun .claude/skills/langfuse/scripts/llm-log-query.mjs session <session-id> diff <round1> <round2> [--dir ./data]
```

对比同一 session 中第 N 轮和第 M 轮请求的 messages 差异，高亮新增/删除/修改的消息块。用于观察 agent 如何逐步构建上下文。

也可以直接对比两个请求 ID：

```bash
bun .claude/skills/langfuse/scripts/llm-log-query.mjs diff <request-id-1> <request-id-2> [--dir ./data]
```

### stats — 统计汇总

```bash
bun .claude/skills/langfuse/scripts/llm-log-query.mjs stats [--dir ./data] [--by model|session|route|hour]
```

输出汇总：总请求数、按维度分组（模型/session/路由/小时）的请求数、错误率。

### cache — 缓存率深度分析（重要）

```bash
bun .claude/skills/langfuse/scripts/llm-log-query.mjs cache [--dir ./data] [--session <id>] [--by-session] [--after YYYY-MM-DD] [--before YYYY-MM-DD]
```

**这是最常用的诊断命令之一。** Prompt Cache 命中率直接影响 API 成本和延迟，每次分析日志时都应主动运行此命令，即使没有明确要求。

输出内容：

- **全局缓存率**：缓存命中 token / 总输入 token，以及缓存写入率、冷 miss 率
- **缓存健康度**：按阈值（无缓存 / < 30% / >= 30%）分级统计
- **Session 缓存趋势**（`--by-session` 或 `--session`）：逐轮展示缓存命中率变化，判断前缀是否稳定
- **自动诊断**：
    - 所有请求无缓存 → 检查 prompt caching 是否启用
    - 部分请求无缓存 → 区分冷启动（正常）vs 前缀不稳定（异常）
    - 缓存写入 > 缓存读取 → 缓存投入未回收，前缀在请求间变化
    - 逐轮下降 → messages 前缀被 prepend 打乱、tools 顺序变化、system prompt 动态段过大

### cache-debug — 缓存诊断深度分析

```bash
bun .claude/skills/langfuse/scripts/llm-log-query.mjs cache-debug <request-id-1> [request-id-2] ... [--dir ./data]
```

对指定请求进行深度缓存诊断：自动查找同 session 的前一轮请求，对比缓存变化类型（冷启动/缓存失效/缓存稀释/正常），分析 cache_control 断点地图，定位缓存失效的具体原因（system prompt 变化、tools 数组变化、消息前缀变化等）。

### cache-control — 断点地图与问题检测

```bash
bun .claude/skills/langfuse/scripts/llm-log-query.mjs cache-control <request-id> [--dir ./data]
```

展示指定请求的 cache_control 断点地图：system blocks、tools、messages 中的缓存标记位置，累积 token 估算，以及断点覆盖问题检测。

#### 缓存率分析检查清单

分析日志时，务必关注以下指标：

| 指标       | 正常范围                 | 异常信号                 |
| ---------- | ------------------------ | ------------------------ |
| 缓存命中率 | > 50%（ReAct 第 2 轮起） | < 30% 持续出现           |
| 缓存写入率 | 首轮高、后续低           | 每轮都很高（前缀总在变） |
| 冷 miss 率 | 仅首轮                   | 多轮后仍有冷 miss        |
| 逐轮趋势   | 稳定或微升               | 持续下降                 |

#### 常见缓存失效原因排查顺序

1. **system prompt 变化**：用 `session <id> diff 1 2` 对比前两轮的 system 消息，检查是否有日期、cwd 等动态占位符在边界标记之前
2. **tools 数组顺序**：用 `session <id> diff 1 2` 检查 tools 列表是否一致（HashMap 迭代顺序不确定会导致序列化不稳定）
3. **消息前缀被 prepend 打乱**：用 `session <id> diff 1 2` 检查消息序列开头是否被插入了新消息
4. **cache_control 断点丢失**：用 `cache-control <id>` 或 `show <id> --body` 检查请求体中 system/tools/messages 的 cache_control 标记是否存在

### context-growth — 上下文膨胀轨迹分析

```bash
bun .claude/skills/langfuse/scripts/context-growth.mjs --dir <data-dir> --session <id>
```

**这是诊断"上下文为什么消耗这么快"的核心工具。** 输出 session 内每轮的消息数、估算 token、消息组成占比（system/thinking/tool_calls/tool_results/compacted）、新增消息摘要、compact 事件、LLM 实际调用 usage。

输出内容：

- **关键轮次表格**：每轮的消息数、Δ变化、估算 tokens（K）、组成占比（SYS/THK/CALL/TOOL/CMP）、新增消息类型摘要、工具调用、thinking 文本、compact 事件标记
- **汇总**：首末轮消息数/tokens、平均每轮增长、LLM 实际调用次数、full compact 次数
- **组成贡献**：末轮的 system/工具结果/已压缩/思考/工具调用/对话占比

诊断要点：

| 指标 | 正常 | 异常信号 |
|------|------|---------|
| 每轮消息增长 | +2（1 assistant + 1 tool_result） | +10 以上（回注 compressed 消息） |
| 工具结果占比 | < 50% | > 70%（大文件读取/cargo test 输出堆积） |
| Micro-compact 首次触发 | 消息数 < 50 | 消息数 > 100（太晚，上下文已膨胀） |
| Full compact 后缓存 | 逐步恢复 | 暴跌至 < 20%（前缀全丢） |
| 估算 token 增长率 | < 1K/轮 | > 5K/轮（大工具输出未截断） |

典型异常模式：

1. **工具结果肿胀**：`cargo test`/`cargo build` 输出逐轮累积，单次 +10-15K tokens，无自动截断
2. **Micro-compact 延迟**：到 100+ 轮才首次压缩，此时上下文已膨胀到 100K+ tokens
3. **Compact 回注风暴**：compacted 消息一次性恢复（单轮 +100+ 条消息），而非渐进回注
4. **Full compact 缓存惩罚**：压缩后前缀结构全变，缓存命中率从 ~99% 暴跌至 ~15%，每轮浪费数万 tokens

## 分析流程

面对用户的日志分析需求，按以下步骤工作：

1. **定位范围**：先用 `list` 了解日志总量和时间范围，必要时用 `--after`/`--before` 缩小范围
2. **缓存率检查**（每次分析都应执行）：运行 `cache` 命令，检查缓存命中率和健康度。如果发现问题，用 `--by-session` 查看逐轮趋势，再用 `session <id> diff` 定位前缀不稳定的具体原因
3. **上下文膨胀分析**（长时间 session 必须执行）：运行 `context-growth` 命令，分析消息增长轨迹、工具结果大小、compact 时机和效果。找出上下文消耗的根本原因（是大工具输出、compact 太晚、还是回注风暴）
4. **按需下钻**：
    - 查某个 session 的完整交互 → `session`
    - 查某个具体请求 → `show`
    - 深度缓存诊断 → `cache-debug`
    - 断点地图 → `cache-control`
    - 对比两轮请求差异 → `session ... diff` 或 `diff`
    - 统计全局概况 → `stats`
5. **解读结果**：将工具输出翻译为用户能理解的结论（如"这个 session 共 5 轮 LLM 调用，缓存命中率 72%，第 3 轮因 tools 列表变化导致缓存失效"）

## 注意事项

- 工具已随 Langfuse skill 提供：`../scripts/llm-log-query.mjs`、`../scripts/context-growth.mjs`，不依赖其他 skill 目录。
- `llm-log-query.mjs` 默认依次查找 `side-projects/llm-gateway/data/`、`data/`；显式 `--dir` 可避免分析错误的数据源。
- 不假定 `request.json` 的 headers 或正文已脱敏；认证字段即使只剩部分前缀也不应展示。
- stream.log 是原始 SSE 文本，内容可能很大，展示时注意截断
- 新格式不再有 `response.json` 和 `log.txt`，路由从 `headers.host` 推导，状态从 stream 内容判断，usage 从 `message_delta` 事件提取
- 脚本同时兼容旧格式（有 `response.json`/`log.txt`）和新格式（仅 `request.json` + `stream.log`）
- `context-growth.mjs` 依赖封装格式的 `request.json`（`headers.x-session-id` 与 `body.messages`），按 session 前缀匹配；不能假定支持裸 body 或旧格式的全部能力。它按 chars/3.5 估算消息 token，不包括独立的顶层 system/tools；usage 来自 SSE 中包含 usage 的事件。估算与消息分类仅用于趋势线索，没有已验证的误差界限。
- 文中的阈值及脚本自动诊断是排查启发，不是产品契约或因果证明；需结合模型、provider、请求内容和真实 usage 核实。
