export const meta = {
  name: 'full-code-review',
  description: '多维度深度代码审查：正确性/安全性/架构/性能/测试/回归，带对抗式验证',
  whenToUse: '当用户要求全面代码审查、review 最近变更、或需要多维度质量检查时使用',
  phases: [
    { title: 'Scan', detail: '扫描指定范围的变更文件' },
    { title: 'Review', detail: '多维度并行深度审查' },
    { title: 'Verify', detail: '对抗式验证每个发现' },
    { title: 'Synthesize', detail: '汇总确认的发现并分级' },
  ],
}

// ============================================================
// 参数说明（通过 args 传入）
//
// args.range        — git 范围，默认 'HEAD~10'
// args.dimensions   — 审查维度数组，默认全部 6 个
//                     可选: correctness, security, architecture, performance, tests, regression
// args.focusFiles   — 额外重点关注文件路径数组（追加到每个维度的默认重点）
// args.verifyLevel  — 验证最低级别，默认 'Medium'（验证 Medium+）
//                     可选: 'Critical', 'High', 'Medium', 'Low'
// args.baseRef      — 如果提供，用 baseRef..HEAD 代替 range..HEAD
// ============================================================

const range = args?.baseRef ? `${args.baseRef}..HEAD` : (args?.range || 'HEAD~10')
const requestedDims = args?.dimensions || ['correctness', 'security', 'architecture', 'performance', 'tests', 'regression']
const verifyMinLevel = args?.verifyLevel || 'Medium'
const extraFocus = args?.focusFiles || []

// 用于在 prompt 中拼接额外重点文件
const extraFocusBlock = extraFocus.length > 0
  ? '\n\n额外重点文件（用户指定）：\n' + extraFocus.map(f => `- ${f}`).join('\n')
  : ''

// 严重级别排序（用于 verifyLevel 过滤）
const SEVERITY_ORDER = { Critical: 0, High: 1, Medium: 2, Low: 3 }
const verifyThreshold = SEVERITY_ORDER[verifyMinLevel] ?? 2

// ── Phase 1: Scan ──────────────────────────────────────────
phase('Scan')
const diffStat = await agent(
  `运行 git diff --stat ${range} 获取变更文件列表，然后运行 git diff ${range} 获取完整 diff。输出所有变更文件的路径列表，按 crate 分组。不要省略任何文件。`,
  { label: 'scan-diff', phase: 'Scan' }
)

// ── Phase 2: Review ────────────────────────────────────────
phase('Review')

// 维度定义：每个维度有 key, 审查 prompt
const ALL_DIMENSIONS = [
  {
    key: 'correctness',
    prompt: `你是一个严格的正确性审查员。审查代码变更（${range}），聚焦：

1. **逻辑错误**：条件判断错误、边界条件遗漏、空指针/unwrap 风险
2. **并发安全**：数据竞争、死锁、Send/Sync 违规、channel 溢出
3. **错误处理**：错误吞没、不完整的错误传播、panic 风险
4. **API 契约**：函数签名变更导致的破坏性变更、trait 实现不完整
5. **数据一致性**：state 读写不一致、消息丢失/重复、ID 冲突

重点关注这些高风险区域：
- peri-acp/src/event/mapper.rs（事件映射重构）
- peri-acp/src/session/executor.rs（executor 重构）
- peri-acp/src/session/event_sink.rs（事件 sink 变更）
- peri-agent/src/llm/sse.rs（SSE 解析修复）
- peri-agent/src/interaction/（新增 channel 系统）
- peri-tui/src/app/agent_ops/acp_bridge.rs（ACP bridge 重构）
- peri-agent/src/agent/executor/tool_dispatch.rs（工具调度变更）${extraFocusBlock}

对每个发现，报告：
- 文件路径和行号范围
- 严重级别（Critical/High/Medium/Low）
- 具体描述
- 建议修复方案

用中文输出。`,
  },
  {
    key: 'security',
    prompt: `你是一个安全性审查员。审查代码变更（${range}），聚焦：

1. **输入验证**：外部输入是否充分校验（特别是 SSE 数据、MCP 消息、用户输入）
2. **路径穿越**：文件操作是否有路径穿越防护
3. **注入风险**：命令注入、SQL 注入、格式字符串漏洞
4. **敏感数据**：API key/secret 泄漏、日志中的敏感信息
5. **SSRF**：网络请求是否有 SSRF 防护
6. **权限提升**：权限检查是否可绕过

重点关注：
- peri-agent/src/llm/sse.rs（SSE 解析 - 外部数据）
- peri-middlewares/src/hooks/（hooks 执行 - 命令注入）
- peri-middlewares/src/mcp/（MCP 通信 - 外部数据）
- peri-middlewares/src/hooks/ssrf_guard.rs（SSRF 防护）${extraFocusBlock}

对每个发现，报告文件路径、行号、严重级别、具体描述、建议修复。用中文输出。`,
  },
  {
    key: 'architecture',
    prompt: `你是一个架构审查员。审查代码变更（${range}），聚焦：

1. **依赖方向**：是否违反 workspace 层级（下层不应依赖上层）
2. **职责分离**：模块职责是否清晰，是否有 god object / 职责泄漏
3. **API 设计**：公开 API 是否合理（过于宽泛/过于狭窄）
4. **抽象层次**：是否在正确的层次做事情（TUI 不应直连 agent，ACP 应是唯一通道）
5. **重复代码**：是否有大量重复逻辑可提取
6. **死代码**：是否有未使用的代码/导入/变量
7. **命名一致性**：命名是否符合项目惯例

重点关注：
- peri-acp/src/（新增 session/command 模块）
- peri-agent/src/interaction/（新增模块）
- peri-tui/src/app/agent_ops/acp_bridge.rs（桥接逻辑）
- peri-middlewares/src/subagent/（SubAgent 重构）
- peri-tui/src/acp_server/（ACP server 变更）${extraFocusBlock}

对每个发现，报告文件路径、行号、严重级别、具体描述、建议修复。用中文输出。`,
  },
  {
    key: 'performance',
    prompt: `你是一个性能审查员。审查代码变更（${range}），聚焦：

1. **内存分配**：不必要的 clone、大对象频繁分配、String 分配热点
2. **异步性能**：不必要的 .await、阻塞操作在 async 中、channel 容量不合理
3. **算法效率**：O(n²) 可优化为 O(n)、不必要的线性搜索
4. **锁竞争**：RwLock/Mutex 持有时间过长、读写锁选择不当
5. **I/O 效率**：小包频繁写入、不必要的 flush、缓冲区大小
6. **构建开销**：每轮 agent 构建是否有可复用部分

重点关注：
- peri-agent/src/llm/sse.rs（SSE 解析性能）
- peri-acp/src/session/executor.rs（每轮执行路径）
- peri-tui/src/ui/render_thread.rs（渲染性能）
- peri-agent/src/interaction/（channel 系统）
- peri-acp/src/event/mapper.rs（事件映射开销）${extraFocusBlock}

对每个发现，报告文件路径、行号、严重级别、具体描述、预估影响、建议优化。用中文输出。`,
  },
  {
    key: 'tests',
    prompt: `你是一个测试质量审查员。审查代码变更（${range}），聚焦：

1. **新增代码测试覆盖**：新增的 public 函数/方法是否有对应测试
2. **测试迁移质量**：测试迁移是否保持了覆盖度
3. **边界条件测试**：是否有边界条件遗漏（空输入、极端值、并发场景）
4. **Mock 质量**：mock 是否真实模拟了行为，是否有假阳性风险
5. **测试隔离**：测试是否互相独立，是否依赖全局状态
6. **断言质量**：断言是否足够具体（避免模糊的 assert!(result.is_ok())）

重点关注：
- 所有 *_test.rs 文件的变更
- 新增模块是否有对应测试文件
- 测试被删除/迁移的地方${extraFocusBlock}

列出缺失的测试场景和建议新增的测试。用中文输出。`,
  },
  {
    key: 'regression',
    prompt: `你是一个回归风险审查员。审查代码变更（${range}），聚焦：

1. **破坏性变更**：公开 API 签名变更、trait 方法增减、配置格式变更
2. **行为变更**：逻辑分支变化、默认值变化、错误处理路径变化
3. **数据格式变更**：序列化格式变化、事件类型增减、消息结构变更
4. **环境依赖**：新引入的外部依赖、环境变量变更、文件路径约定
5. **平台兼容**：Windows/macOS/Linux 兼容性、终端兼容性
6. **升级风险**：用户从旧版本升级时的迁移路径

重点关注：
- peri-acp/src/event/mapper.rs（事件映射 - 影响所有下游消费者）
- peri-acp/src/session/executor.rs（执行路径 - 影响所有 prompt）
- peri-agent/src/llm/（LLM 层变更 - 影响所有模型调用）
- peri-acp/src/session/command/（新增命令系统）
- peri-tui/src/acp_stdio.rs（stdio 传输变更）${extraFocusBlock}

对每个发现，报告：影响范围、回归概率、回归表现、建议的验证方法。用中文输出。`,
  },
]

// 过滤出用户请求的维度
const DIMENSIONS = ALL_DIMENSIONS.filter((d) => requestedDims.includes(d.key))

if (DIMENSIONS.length === 0) {
  log('错误：没有匹配的审查维度。可用维度：correctness, security, architecture, performance, tests, regression')
}

// Schema 定义（所有维度共用）
const REVIEW_SCHEMA = {
  type: 'object',
  properties: {
    dimension: { type: 'string' },
    findings: {
      type: 'array',
      items: {
        type: 'object',
        properties: {
          file: { type: 'string' },
          lines: { type: 'string' },
          severity: { type: 'string', enum: ['Critical', 'High', 'Medium', 'Low'] },
          title: { type: 'string' },
          description: { type: 'string' },
          suggestion: { type: 'string' },
        },
        required: ['file', 'severity', 'title', 'description'],
      },
    },
    summary: { type: 'string' },
  },
  required: ['dimension', 'findings', 'summary'],
}

const reviews = await parallel(
  DIMENSIONS.map((d) => () =>
    agent(d.prompt, {
      label: `review:${d.key}`,
      phase: 'Review',
      schema: REVIEW_SCHEMA,
    })
  )
)

log(`收到 ${reviews.filter(Boolean).length}/${DIMENSIONS.length} 个维度的审查结果`)

// ── Phase 3: Verify ────────────────────────────────────────
phase('Verify')

const allFindings = reviews
  .filter(Boolean)
  .flatMap((r) => r.findings.map((f) => ({ ...f, dimension: r.dimension })))
  .filter((f) => (SEVERITY_ORDER[f.severity] ?? 3) <= verifyThreshold)

log(`共 ${allFindings.length} 个 ${verifyMinLevel}+ 级别发现需要验证`)

const VERIFY_SCHEMA = {
  type: 'object',
  properties: {
    confirmed: { type: 'boolean' },
    adjusted_severity: { type: 'string', enum: ['Critical', 'High', 'Medium', 'Low'] },
    reason: { type: 'string' },
    additional_context: { type: 'string' },
  },
  required: ['confirmed', 'adjusted_severity', 'reason'],
}

const verified = await pipeline(
  allFindings,
  (finding) =>
    agent(`验证这个代码审查发现是否为真问题，不要放过任何可疑点。

文件：${finding.file}
行号：${finding.lines || '未知'}
维度：${finding.dimension}
严重级别：${finding.severity}
标题：${finding.title}
描述：${finding.description}

请读取相关文件的实际代码，验证：
1. 该问题是否真实存在（不是误报）
2. 严重级别是否准确
3. 是否有遗漏的相关代码路径

如果你认为这是误报，请说明原因。如果确认，请补充更多上下文。`,
      {
        label: `verify:${finding.file.split('/').pop()}`,
        phase: 'Verify',
        schema: VERIFY_SCHEMA,
      }
    ),
  (verdict, finding) => ({ ...finding, verdict })
)

const confirmed = verified.filter((v) => v && v.verdict && v.verdict.confirmed)
const falsePositives = verified.filter((v) => v && v.verdict && !v.verdict.confirmed)

log(`验证完成：${confirmed.length} 确认 / ${falsePositives.length} 误报 / ${allFindings.length} 总计`)

// ── Phase 4: Synthesize ────────────────────────────────────
phase('Synthesize')

const SYNTHESIS_SCHEMA = {
  type: 'object',
  properties: {
    critical_count: { type: 'number' },
    high_count: { type: 'number' },
    medium_count: { type: 'number' },
    low_count: { type: 'number' },
    false_positive_count: { type: 'number' },
    overall_assessment: { type: 'string' },
    priority_fixes: {
      type: 'array',
      items: {
        type: 'object',
        properties: {
          file: { type: 'string' },
          title: { type: 'string' },
          severity: { type: 'string' },
          description: { type: 'string' },
        },
      },
    },
    recommendations: {
      type: 'array',
      items: { type: 'string' },
    },
  },
  required: ['critical_count', 'high_count', 'medium_count', 'low_count', 'false_positive_count', 'overall_assessment', 'priority_fixes', 'recommendations'],
}

const synthesis = await agent(
  `你是首席审查员。汇总以下经过对抗式验证的代码审查发现，生成最终报告。

确认的发现（${confirmed.length} 个）：
${JSON.stringify(confirmed, null, 2)}

误报（${falsePositives.length} 个）：
${JSON.stringify(falsePositives.map(f => ({ file: f.file, title: f.title, reason: f.verdict.reason })), null, 2)}

请：
1. 按严重级别分组（Critical > High > Medium > Low）
2. 每组按模块/文件聚合
3. 标注需要立即修复的 Critical/High 级别问题
4. 给出整体代码质量评估
5. 建议优先修复顺序

用中文输出结构化报告。`,
  {
    label: 'synthesize',
    phase: 'Synthesize',
    schema: SYNTHESIS_SCHEMA,
  }
)

return { reviews, confirmed, falsePositives, synthesis }
