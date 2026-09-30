# Langfuse 高性能监控：协议保真、吞吐隔离与资源预算

状态：七项修复及本地回归完成；真实服务端和生产负载验收待完成，保持 active。
跟踪位置为仓库本地 `spec/issues/`。

## 目标与边界

观测上报不得阻塞业务等待网络，发送结果必须区分完整接受、部分拒收及发送失败；
排队、聚合和并发发送都有独立容量及字节预算。保留既有 flush 前缀保护、失败确认、
shutdown 取消重试和唯一 worker/join owner 契约。不改动并发进行中的 OAuth/Resources 迁移。

以下七项覆盖本轮审查全部发现；性能推导不视作实测指标。未约定业务吞吐/P99 SLO，
本轮以确定性慢端点、并发、字节预算和生命周期测试验收，真实远端及负载验收仍须单独记录。

## F1 / P1：标准 OTLP 身份

### What to build

领域 ID 与 OTLP trace/span ID 分离；转换是确定的，父引用使用同一映射。
有效标准 ID 保持稳定，UUID/带前缀 ID 映射到非零的标准长度十六进制 ID。

### Acceptance criteria

- [x] 根及子节点 traceId 为 32 位、spanId/parentSpanId 为 16 位合法非零 hex。
- [x] 不同领域 ID 不因简单截断 UUID 时间前缀而系统性碰撞。
- [x] 实际 HTTP 载荷验证父子引用一致；缺失身份不发送非法 span。

Blocked by：无。执行组：OTLP 转换。

## F2 / P1：完整接受与部分拒收

### What to build

解析 OTLP 成功响应，拒收数量非零时作为可观察发送失败进入 flush/shutdown；
部分成功不可整批重发，错误摘要不包含服务端 payload。

### Acceptance criteria

- [x] HTTP 200 + rejectedSpans 非零不能报告成功，不重复提交已接受部分。
- [x] 拒收为零的警告与完整成功不混淆；无效/截断响应不能静默成功。
- [x] flush 的失败确认和 shutdown 累计报告保持原有契约。

Blocked by：无。执行组：HTTP 客户端。

## F3 / P1：v4 完整不可变观测

### What to build

Workflow 在内存保留开始时间、输入与父关系，结束后仅导出一个完整 span；
不依赖重发同 ID 的 span 更新服务端。无有效 trace/span 身份的 Session/SDK 记录
不能伪装成 OTLP span；Score 使用专用入口或显式拒绝，不能假装已上传评分。

### Acceptance criteria

- [x] Workflow 和 Subagent 完成后仅有一次导出，包含完整输入、输出、开始及结束时间。
- [x] 外部 Update 事件不会生成不可验证的重复观测；失败显式且可诊断。
- [x] 正常 turn 不发送无身份的 Session span，session 关联通过观测属性承载。

Blocked by：F1 的 wire 身份验收。执行组：生产观测 + OTLP 转换。

## F4 / P1：有界并发发送与独立队列

### What to build

命令队列容量与单批上限分别配置；聚合与有限并发 HTTP 分离，慢批次不能立即停止消费。
flush 只等待自身前缀，不能错误等待后续批次；shutdown 排空并 join 全部已接受发送。

### Acceptance criteria

- [x] 门控第一批 HTTP 时第二批可发送，并发不超过配置上限。
- [x] 乱序完成、失败、取消、panic、flush 前缀保护及 shutdown 重试均有回归。
- [x] 生产配置可设置队列容量及发送并发，不用增大 batch 代替缓冲。

Blocked by：F2 的结果语义。执行组：批处理核心。

## F5 / P1：限流与重试预算

### What to build

429/502/503/504 和可恢复网络错误按有上限的指数退避及 jitter 重试；
尊重 Retry-After，控制累计重试时间，永久错误不重复提交。

### Acceptance criteria

- [x] 429 后成功、Retry-After、永久 4xx/5xx、网络错误有确定性测试。
- [x] 次数、退避、请求和累计发送期限均有界，无移位溢出。
- [x] 响应与日志不暴露原始服务端正文或事件内容。

Blocked by：无。执行组：HTTP 客户端。

## F6 / P2：减少同步复制与拥塞日志

### What to build

生产 Generation 避免重复复制 messages/tools，不在已有 raw body 时再构造不使用的 JSON；
拥塞诊断以周期汇总/计数为主，不为每个丢弃事件写两条日志。

### Acceptance criteria

- [x] raw body 与 fallback input 行为保持完整，不静默截断用户输入。
- [x] 相邻生命周期、retry、fallback 与采样测试不退化。
- [x] 重试复用同一已编码 HTTP body；逐事件拥塞日志被聚合诊断替代。

Blocked by：无。执行组：生产观测 + HTTP + 批处理核心。

## F7 / P2：字节预算与监控验收

### What to build

准入、单事件、单批 HTTP 与响应分别有字节上限；过大事件明确拒绝，不静默裁剪。
提供安全的累计接受/拒绝/完成/失败及在途计数，便于核对确定性压力场景。

### Acceptance criteria

- [x] 大上下文、超大响应、重试编码及并发发送的内存责任边界明确。
- [x] 配置边界、字节释放、拒绝、关闭及取消有回归测试。
- [x] 记录实际执行命令、通过数量和局限，不把功能测试冒充生产性能基准。

Blocked by：F4 的在途所有权。执行组：批处理核心 + HTTP 客户端。

## 验证基线

审查时 `cargo test -p langfuse-client --lib` 实跑 97 项通过；三个已清理的临时
characterization 测试复现了非法 ID、429 不重试和部分拒收仍成功。它们证明故障，
不是修复验收。最终证据在本 issue 更新；未完成的验收保持 active。

## 实施结果与证据

用户授权后由三个 subagent 分别负责 HTTP、OTLP 转换、生产观测，主线负责批处理、
配置接线及整体验收。HTTP body 编码一次；部分拒收提供结构化错误与累计拒收计数。
并发发送仍归唯一 worker/join owner 管理，flush 屏障不消耗后缀事件；取消关闭等待
不丢失在途发送，worker 被取消时释放在途计数。

默认预算为队列 1024、单批 50、并发 2、单事件 512 KiB、单批/请求 4 MiB、
队列 JSON 字节 16 MiB、响应 64 KiB。Controller 支持 settings 和五个独立环境变量；
参数入口与稳定行为见 `docs/code-index/langfuse-client.md`。

| 实跑命令 | 结果与范围 |
| --- | --- |
| `cargo test -p langfuse-client --lib` | 153 项通过，覆盖转换、HTTP、准入、并发、失败确认与关闭 |
| `cargo test -p peri-controller --lib -- langfuse` | 153 项通过，16 项无关测试过滤 |
| `cargo test -p peri-controller --test langfuse_e2e` | 5 项通过，使用 fake session，不是真实远端 E2E |
| `cargo test -p langfuse-client --test otel_contract` | 1 项通过，实际 HTTP mock 检查认证、载荷及父关系 |
| `cargo test -p peri-acp --lib -- host::stdio::run_server_integration_tests::langfuse_shutdown_tests` | 6 项通过，覆盖完整关闭、重试及外部 session 所有权 |
| `cargo clippy -p langfuse-client --all-targets -- -D warnings` | 通过 |
| `cargo clippy -p peri-controller --all-targets -- -D warnings` | 通过 |
| `cargo test -p langfuse-client -p peri-controller --doc` | 成功运行，两 crate 均无 doc test；不计入测试通过数量 |

ACP 测试链接阶段出现 macOS `__eh_frame section too large` 警告，不影响这六项通过。
全仓 `bash scripts/check-file-size.sh --top 8` 报告 25 个无关既存超限文件；本轮修改
及新增 Rust 文件均不超过 1000 行。未运行全 workspace 测试，不声明全仓无回归。

## 未完成的远端与负载验收

- [ ] 在配置好的真实 Langfuse 服务验证收到的根、Workflow、Subagent、Generation、
  工具观测及 session 关联；核对实际服务版本的 immutable/partialSuccess 行为。
- [ ] 与业务方确认事件吞吐、输入尺寸分布、允许丢弃率、同步准入 P99、
  flush/shutdown 耗时以及内存预算，再使用代表性负载记录实测数据。
- [ ] 持续限流、慢端点及大上下文压力下，记录 accepted/rejected/evicted、
  completed/failed/partial-rejected/in-flight 计数、RSS 与业务延迟，确认预算可用。

JSON 字节预算不等于 allocator/RSS 上限；队列预算不包括尚未准入的调用方事件，
转换后的 HTTP 编码还会独立检查上限。超限明确拒绝而不截断，需按真实上下文调整。
Update 和 Score 当前显式不支持，不新增兼容更新或评分专用接口。父 ID 稳定不保证
父观测先于子观测到达。以上本地证据只能支持高性能监控的必要机制已补齐，不能
证明生产吞吐/P99 SLO；待未完成项验收后再按文档生命周期规则结束本 issue。
