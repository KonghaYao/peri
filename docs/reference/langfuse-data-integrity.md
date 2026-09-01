# Langfuse 监控数据结构检查手册

> 用途：排查 Perihelion → Langfuse 监控数据异常（字段缺失、归属错乱、比例异常、token/缓存异常）。
> 本手册面向「数据长什么样、应该长什么样、怎么快速验证」，不含 UI 操作指南。
> 架构归属与跨层约束以 `docs/design/architecture.md` 和
> `docs/standards/architecture-contracts.md` 为准。

---

## 一、数据链路结构

```
peri-controller/src/langfuse/tracer/    ① 观测对象生命周期 + IngestionEvent 构造
        │  (generation.rs / stages.rs / registry.rs / tool_batch.rs / usage.rs / sampling.rs)
        ▼
langfuse-client/src/                    ② 事件批处理 + OTLP 转换 + 上报
        │  (batcher.rs 攒批 → types/conversion.rs 转 OTLP → client.rs POST)
        ▼
POST {LANGFUSE_BASE_URL}/api/public/otel/v1/traces
        │  header: x-langfuse-ingestion-version: 4
        ▼
peri-fuse（本地服务 localhost:23332）   ③ 接收端：Langfuse OTel attribute 映射 → SQLite
        │  npm 包 peri-fuse，数据目录 ~/.peri-fuse/
        ▼
~/.peri-fuse/telemetry.db              ④ 数据落库（observations / traces / scores）
~/.peri-fuse/langfuse.db               （api_keys 认证，~3GB 数据在 telemetry.db）
```

### 事实源路径

| 环节 | 位置 |
|------|------|
| 观测生命周期 | `peri-controller/src/langfuse/tracer/`（各 `*_test.rs` 为行为契约） |
| OTLP 转换 | `langfuse-client/src/types/conversion.rs`（`ingestion_events_to_otel`） |
| 上报端点 | `langfuse-client/src/client.rs`（`POST {base_url}/api/public/otel/v1/traces`） |
| 服务端 attribute 映射 | peri-fuse `dist/server.cjs` 内 `LangfuseOtelSpanAttributes`（与官方 Langfuse 一致） |
| 配置 | 项目根 `.env`：`LANGFUSE_BASE_URL`（**注意不是** `LANGFUSE_HOST`，两者同名键易混淆） |
| 查询工具 | `.claude/skills/langfuse/`（trace-search / daily-report / analyze / session-analyze 等脚本 + `bunx langfuse-cli`） |
| 历史问题 | `spec/archive-issues/langfuse/` |

---

## 二、观测类型字段契约

id 前缀即类型签名：`gen_*`=GENERATION、`span_*`=SPAN、`obs_*`=TOOL、`batch_*`=tool-batch SPAN、trace id（UUID）=TRACE。

| 类型 | 名称示例 | 必填字段 | 可选字段 | 设计要点 |
|------|----------|----------|----------|----------|
| TRACE | `turn <uuid>` | id、start_time、session_id | input、output、metadata、tags | 每个 turn 一条 |
| GENERATION | `step-N` | id(`gen_*`)、input、output、model、usage_details、parent | metadata（retry/request_id） | **input 必须为完整请求体**（messages+tools 或 raw body）；parent = 该 agent 活跃 stage span |
| SPAN(stage) | `stage-receive/reason/act/compact/end` | id(`span_*`)、parent | input | 仅 receive 阶段 input 携带 MQ 计数；其余阶段无 input 为**设计行为**，非缺陷 |
| SPAN(turn) | `turn <uuid>` | id、parent | input | turn 根 span |
| SPAN(tool-batch) | `batch_*` | id、input（批量摘要）、output | — | 工具批处理聚合 |
| SPAN(workflow) | `workflow-<id>` | id(`span_*`)、input（`{"plan": ...}`）、output | — | Act 阶段 Workflow 子 span（2026-08-15 已恢复 plan input） |
| SPAN(compact) | `compact` / `micro-compact` | id(`span_*`)、input、output | metadata | **name 即类型**：Micro 策略记录 `micro-compact`，Full/Smart/Skip 记录 `compact`（2026-08-15 起）；input=执行前状态（strategy/trigger/estimated_tokens_before/cache_hit_rate_before），output=执行结果（summary/files_count/skills_count/micro_cleared/duration_ms/estimated_tokens_saved/estimated_tokens_after/full_escalation_reason/outcome）；失败时 output=`{"error_class":"compact_failure","message":...}` |
| TOOL | 工具名（Bash/Read/Edit…） | id(`obs_*`)、input（工具入参）、output | level | 失败时 output 为 `{"error_class": "tool_failure"}` |
| AGENT | `agent-run` / `subagent-*` | id、output、input | — | input = on_turn_start 的对话输入（2026-08-15 已恢复上报） |
| EVENT | `cache-hit-rate-low` | id、input（告警指标）、level | output | 告警类 |

### 字段来源规则（OTLP attribute 映射）

- `input`/`output`：`langfuse.observation.input` / `.output`（JSON 字符串）
- `model`：`langfuse.observation.model.name`；参数：`...model.parameters`
- `usage`：`langfuse.observation.usage_details`（含 cache_read/cache_creation_input_tokens）
- `metadata`：`langfuse.observation.metadata`
- 上传侧构造点在 `peri-controller/src/langfuse/tracer/mod.rs` 的事件构造（`GenerationBody`/`SpanBody`/`ToolBody` 等）

---

## 三、结构检查命令集

### 3.1 落库完整性统计（telemetry.db 直查）

```bash
# 类型 × input 完整率（异常判据：GENERATION/TOOL/AGENT 完整率应接近 100%）
sqlite3 ~/.peri-fuse/telemetry.db \
  "SELECT type, COUNT(*) as total,
          SUM(CASE WHEN input IS NULL OR input='' THEN 0 ELSE 1 END) as has_input,
          ROUND(100.0 * SUM(CASE WHEN input IS NULL OR input='' THEN 0 ELSE 1 END) / COUNT(*), 1) || '%' as rate
   FROM observations GROUP BY type ORDER BY total DESC;"

# 按天趋势（找「某天起字段开始缺失」的断点，可对比代码提交时间）
sqlite3 ~/.peri-fuse/telemetry.db \
  "SELECT substr(start_time,1,10) as day, type, COUNT(*) as total,
          SUM(CASE WHEN input IS NULL THEN 0 ELSE 1 END) as has_input
   FROM observations WHERE start_time >= datetime('now','-14 days')
   GROUP BY day, type HAVING total > 0 ORDER BY day DESC, type;"

# 单条观测深查（input 是否真缺失）
sqlite3 ~/.peri-fuse/telemetry.db \
  "SELECT id, type, name, trace_id, parent_observation_id,
          (input IS NULL) as no_input, length(output) as out_len
   FROM observations WHERE id='gen_...';"

# 父链完整性：parent 指向不存在的观测。
# 注意：parent 可以是 trace id（设计上大量观测直接挂 trace），需排除；
# 真正的断裂特征是 parent 带 obs_/span_/gen_/batch_ 前缀但查无此 id。
sqlite3 ~/.peri-fuse/telemetry.db \
  "SELECT COUNT(*) FROM observations o
   WHERE o.parent_observation_id IS NOT NULL
     AND o.parent_observation_id NOT IN (SELECT id FROM observations)
     AND o.parent_observation_id NOT IN (SELECT id FROM traces)
     AND o.parent_observation_id NOT LIKE '01%';"

# usage 缺失 / 缓存 token 未记录
sqlite3 ~/.peri-fuse/telemetry.db \
  "SELECT COUNT(*) FROM observations WHERE type='GENERATION'
   AND (usage_details IS NULL OR usage_details='{}' OR usage_details LIKE '%cache_read_input_tokens%null%');"
```

### 3.2 服务与认证检查

```bash
# 服务存活（注意用 LANGFUSE_BASE_URL，不是 LANGFUSE_HOST）
curl -s http://localhost:23332/api/public/health        # 期望 {"status":"OK"}

# 认证 401 排查：key 是否在 peri-fuse 注册表里
sqlite3 ~/.peri-fuse/langfuse.db "SELECT id, public_key, project_id FROM api_keys;"
# 对比 .env 的 LANGFUSE_PUBLIC_KEY 是否存在；period 轮换后 .env 与注册表不同步即 401

# 服务端支持的路由白名单（探测 401 vs SPA fallback 区分「认证失败」和「未实现」）
#   /api/public/traces /observations /sessions /scores /users /dashboard /health /ready
#   /api/public/otel/v1/traces（上传）、/api/public/ingestion（废弃兼容）
#   未注册路径返回 SPA HTML，注册路径返回 JSON/401
```

### 3.3 通过 skill 脚本查

```bash
bun .claude/skills/langfuse/scripts/trace-search.ts --days 1 --json        # 最近 trace
bun .claude/skills/langfuse/scripts/trace-tokens.ts <traceId>              # token 流 + 缓存
bun .claude/skills/langfuse/scripts/trace-messages.ts <traceId>            # 消息组成
bun .claude/skills/langfuse/scripts/analyze.ts 20 --days 7 --report        # 全维度扫描
bunx langfuse-cli api traces list --limit 10 --json                        # CLI 直查
```

---

## 四、异常模式检查表

| # | 异常模式 | 检查命令 | 判据 | 历史 |
|---|----------|----------|------|------|
| 1 | **GENERATION 全部/部分无 input** | §3.1 按天趋势 | 某天起 input 完整率骤降为 0 | 曾因 tracer 事件构造 `input` 硬编码 `None` 引入（2026-08-08 重构，2026-08-15 修复）；**修复需重启 peri 进程生效** |
| 2 | **AGENT 无 input**（agent-run/subagent） | §3.1 类型统计 | AGENT 完整率低 | 同上重构删除 `agent_input` 存储逻辑；2026-08-15 已恢复（`on_turn_start` 暂存 → agent-run input） |
| 3 | **TOOL 无 input** | §3.1 类型统计 | TOOL 完整率低 | 已修复（2026-08-10）；若出现新缺口按天断点定位 |
| 4 | SPAN 数量爆炸（>> turn × 5） | `SELECT COUNT(*) FROM observations WHERE type='SPAN'` | 单 turn 理论 stage 数 5 + batch | stage span 未成对关闭会堆积；查 `spec/archive-issues/langfuse/2026-08-03-*.md`（顺序错乱） |
| 5 | 缓存命中率异常（0% 或缺失 cache_read） | `trace-tokens.ts` / usage 直查 | 同模型同 prompt 应命中缓存 | `2026-07-22-langfuse-cache-tokens-not-recorded.md` |
| 6 | 父链断裂（parent 不存在） | §3.1 父链 SQL | 带 `obs_`/`span_`/`gen_`/`batch_` 前缀的 parent 查无此 id 即断裂 | 2026-08-15 实测存在 5151 条：stage span 挂在缺失的 TOOL obs 下（TOOL 未落库但子 span 已上传，疑似采样/丢弃不一致，待查） |
| 7 | 并行 subagent 下 step 顺序乱/span 挂错 | `analyze.ts --tools` + trace 详情 | 并行 agent 的 generation 应各归其 AGENT obs | `2026-08-03-langfuse-trace-step-order-shuffled-with-parallel-subagents.md`（已按 agent 隔离修复） |
| 8 | token 计费异常（input cost 为负等） | `daily-report.ts --days 7` | cost/token 比例异常 | `2026-08-02-langfuse-lib-ts-negative-input-cost.md`（脚本侧已修） |
| 9 | 上报偶发丢失（batcher 慢 flush 丢弃） | 对拍 trace 数量：UI 数 vs telemetry.db 数 | 差异突增 | `2026-08-05-langfuse-batcher-drops-during-slow-flush.md` |
| 10 | 401 / 连接失败 | §3.2 | 区分「key 未注册」vs「服务未启动」vs「host 配错」 | `.env` 用 `LANGFUSE_BASE_URL`；`LANGFUSE_HOST` 为空会导致 curl/脚本拿空 host |
| 11 | **compact 结构异常** | 类型统计按 name 分组 | micro/full 应能通过 name 区分；input 应含 before 状态、output 应含 token 指标 | 2026-08-15 修复：此前 name 统一 `compact`（micro 不可区分）、input 缺失、estimated_tokens_*/cache_hit_rate_before 被 bridge 丢弃；需重启生效 |

### 排查流程（固定套路）

1. **先判数据是否落库**：telemetry.db 直查（§3.1）→ 区分「上传侧没发」还是「服务端没存」
2. **再判上传侧**：看 `langfuse-client/src/types/conversion.rs` 转换后 OTLP payload 是否含对应 attribute；必要时在 tracer 事件构造处打点
3. **最后对拍历史**：按天断点 + `git log --date=short -S '关键字段' -- <tracer 路径>` 定位引入/修复 commit
4. 修复后验证：新数据落库正常 + `cargo test -p peri-controller --lib langfuse` 契约测试通过

---

## 五、易混淆点备忘

- `.env` 键名：`LANGFUSE_BASE_URL`（生效）vs `LANGFUSE_HOST`（skill 文档示例键，脚本两者都读，curl 只认后者）
- `input: None` 不总是 bug：ErrorTurn 合成 span、非 receive 阶段 stage span 无 input 是设计；GENERATION/TOOL/AGENT 无 input 才是异常
- generation 的 `input_json` 在 `tracer/generation.rs` 计算（raw_body 优先，回退 messages_json），**必须**被事件构造消费，否则是 dead value（本次事故形态）
- 服务端数据在 `~/.peri-fuse/telemetry.db`（约 3GB），`langfuse.db` 只存认证 key；两者都别当临时文件删
- peri-fuse 是 npx 缓存里的 npm 包，重启方式：`npx peri-fuse` 相关命令（包内 `dist/index.js` CLI）
