# Bug 报告：peri agent-v4.2.3 控制面事件通道静默丢弃事件（单题 10 分钟 13,271 条）

> 来源：GitLab issue #3 正文（`bugs/2026-10-21-peri-423-event-drop/README.md`，`main` 分支），逐字收录，未改动正文。

> 出具日期：2026-10-21
> 被测版本：peri `agent-v4.2.3`（`peri -p` / StreamJson 非交互模式）
> 复现环境：DeepSWE 113 题评测，单题独立 Docker，peri 走 OpenAI 兼容协议直连 LiteLLM → DeepSeek `deepseek-v4-flash`
> 报告人：评测侧（pier 0.3.1 + 本地 LiteLLM 网关）
> 证据目录：`evidence/`（日志已脱敏，密钥串扫描 0 命中）

---

## 0. 结论（TL;DR）

peri 内部的**有界事件通道在满负荷时直接丢弃事件**，丢弃时只打印一行 `outcome=Dropped`，**不记录被丢弃事件的类型**。

本次单题运行 **10 分 33 秒内丢弃 13,271 条事件**，峰值约 **111 条/秒并持续 102 秒**。这些告警占整个日志的 **99.70%**（13,271 / 13,311 行）。

收尾阶段另有一处可量化的损失：**3 个后台任务的回执等待满 120 秒后被放弃**，而它们实际在放弃后 **0.31 秒 / 1.90 秒** 才到达。

本次运行**没有被判为异常**：`exception_info = null`，进程 exit 0，`session_close_failures: 0`；题目本身失败属能力问题。因此**本 bug 不改变本次判分结果**，但它属于**静默丢消息**类缺陷——程序报告成功，内部丢了上万条事件，事后无法复盘丢了什么。

> 与 #2 的关系：本次同时确认 #2 描述的「ACP 收尾失败导致 exit 1」**已在 4.2.3 修复**。本 bug 是同一条事件通道上的**另一个问题**，不是 #2 的遗留。

---

## 1. 环境与配置

| 项 | 值 |
|---|---|
| peri 版本 | `agent-v4.2.3`（`agent_info.version` 与结果一致） |
| 运行方式 | `peri -p '<task>' --max-turns 50 --output-format stream-json --model deepseek-v4-flash` |
| 协议 | OpenAI 兼容（`PERI_PROVIDER_TYPE=openai`，`OPENAI_BASE_URL=http://<eval-host>/v1`） |
| 模型 | `deepseek-v4-flash`（经本地 LiteLLM，无协议翻译层） |
| 并发 | `n_concurrent_trials: 1` |
| 任务 | `kgateway-consistent-hash-policy`（Go，隐藏测试 f2p 2 条、p2p 214 条） |
| trial 目录 | `/home/hpx/deepswe-peri-423/jobs/peri-423-kgateway/kgateway-consistent-hash-policy__ijQD766` |

---

## 2. 现象

### 2.1 日志被单一告警淹没

| 指标 | 值 |
|---|---|
| 日志规模 | 13,311 行 / 2,183,437 字节 |
| WARN / ERROR | 13,293 / **0** |
| `控制面弹出队列事件未投递 … outcome=Dropped` | **13,271 条** |
| 占比 | **99.70%** |
| 第一条 | 原始第 27 行，`06:50:39.574` |
| 最后一条 | 原始第 13,301 行，`07:01:12.996` |

除这一条外，整份 11 分钟的日志只剩约 40 行有效信息。

### 2.2 丢弃不是偶发，是三段持续过载

| 区段 | 条数 | 起 ~ 止 | 持续 | 速率 |
|---|---|---|---|---|
| 1 | 11,319 | 06:50:39.574 ~ 06:52:21.335 | 102 秒 | **约 111 条/秒** |
| 2 | 732 | 06:52:21.336 ~ 06:54:52.112 | 151 秒 | 约 4.8 条/秒 |
| 3 | 1,217 | 06:54:52.113 ~ 06:59:12.987 | 261 秒 | 约 4.7 条/秒 |
| 4 | 1 | 07:01:12.994 | — | — |
| 5 | 2 | 07:01:12.995 ~ 07:01:12.996 | — | — |
| **合计** | **13,271** | 跨度 633 秒 | — | 均值约 21 条/秒 |

**打印并非持续均匀**：633 秒中只有 123 秒有输出（占空比 19%），单秒最多 380 条。静默间隔中最长的四次分别为 **122.85 / 120.10 / 120.04 / 120.01 秒**，与收尾时 `bounded wait expired … waited_ms=120000` 的超时值一致，提示存在**约 120 秒的周期边界**：每到边界就把积压事件集中弹出丢弃，随后进入下一轮静默。

### 2.3 丢弃时不记录事件类型

告警携带的**全部**字段只有两个：

```
session_id=01a15d39-7c84-7ca2-adac-95f6892ed45f  outcome=Dropped
```

没有事件类型、没有事件来源、没有队列容量/水位、没有丢弃序号。**事后无法判断丢掉的是 UI 刷新这类可丢事件，还是工具结果、生命周期、收尾信号这类不可丢事件。**

### 2.4 可量化的实际损失：3 个后台回执被放弃

```
07:01:12.991  WARN peri_agent::agent::async_tasks::handoff:
              bounded wait expired; pending task handoff recorded
              pending=3 waited_ms=120000
```

3 个后台任务的回执等了 120 秒仍未到，handoff 放弃等待。但它们随后就来了：

```
07:01:13.304  result_accepted  → 放弃后 0.31 秒
07:01:14.896  result_accepted  → 放弃后 1.90 秒
```

三个回执均标记 `agent_name=mcp success=false output_len=94`。

### 2.5 判分结果（未受本 bug 影响）

```json
"agent_info": {"name": "peri", "version": "agent-v4.2.3"},
"exception_info": null,
"verifier_result": {"rewards": {"reward": 0, "f2p": 0.0, "p2p": 1.0,
                                "f2p_total": 2, "f2p_passed": 0,
                                "p2p_total": 214, "p2p_passed": 214}}
```

同题 4.1.1 的结果是 `f2p 0/2`、`p2p 214/214` 并伴随异常；4.2.3 结果相同但**异常消失**。

---

## 3. 关键日志（原文）

### 3.1 告警本身

```
2026-10-21T06:50:39.574927Z  WARN peri_controller::controller:
  控制面弹出队列事件未投递 session_id=01a15d39-7c84-7ca2-adac-95f6892ed45f outcome=Dropped
```

### 3.2 触发前 21.6 秒：workspace MCP 订阅停止消费

```
06:50:10.993292Z  WARN rmcp::service: response error id=1
  error=ErrorData { code: ErrorCode(-32602), message: "missing task scope capability", data: None }
06:50:10.993442Z  WARN peri_middlewares::mcp::client::subscription:
  订阅流异常中断，退避后重新 listen server=workspace attempt=1 delay_secs=1
...
06:50:17.998858Z  WARN rmcp::service: response error id=17  (-32602 同上)
06:50:17.999028Z  WARN peri_middlewares::mcp::client::subscription:
  订阅流错误，重试耗尽，停止消费 server=workspace
  error=Mcp error: -32602: missing task scope capability
```

### 3.3 运行中：两次 workspace 读取失败，丢弃风暴在它们之间连续进行

```
06:52:21.336121Z  WARN rmcp::service: response error id=74
  error=ErrorData { code: ErrorCode(-32603), message: "workspace text read failed", data: None }
06:54:52.113216Z  WARN rmcp::service: response error id=117  (-32603 同上)
```

### 3.4 收尾：状态总线已关闭仍在投递

```
07:01:12.991390Z  WARN peri_agent::agent::async_tasks::handoff:
  bounded wait expired; pending task handoff recorded pending=3 waited_ms=120000
07:01:12.994846Z  INFO peri_acp::host::prompt:
  Adopting canonical transcript snapshot payloads=134 compact=false ok=true
07:01:13.086368Z  WARN peri_acp_types::event_v2::bus:
  状态层事件未投递 event="state_event_dropped" outcome=Closed
07:01:14.900318Z  WARN peri_acp_types::event_v2::bus:  （同上）
07:01:14.900410Z  WARN peri_acp_types::event_v2::bus:  （同上）
07:01:14.904389Z  INFO peri_acp::host::shutdown:
  ACP host terminal shutdown complete
  terminal_report=Complete { host: Complete, dynamic_mcp: Complete,
    mcp_pool: Complete { settled_services: 0, failed_services: 0 },
    session_close_failures: 0 }
```

---

## 4. 时间线（UTC）

```
06:49:09  trial 开始
06:50:10  peri print 模式启动（provider=OpenAI）
06:50:10  ⚠️ workspace MCP -32602 missing task scope capability
06:50:17  ⚠️ 订阅流重试耗尽，停止消费
   ↓ 21.6 秒
06:50:39  【第 1 条 Dropped】开始 102 秒、约 111 条/秒的丢弃风暴
06:52:21  ⚠️ workspace text read failed（-32603）
06:54:52  ⚠️ workspace text read failed（-32603）
06:59:12  最后一次大批量 Dropped
06:59:12  handoff 开始等待 3 个后台任务（由 120 秒超时倒推）
07:01:12  ⚠️ 等待超时，pending=3 被放弃
07:01:13  3 个回执中的 2 个到达（放弃后 0.31s）
07:01:14  第 3 个到达（放弃后 1.90s）
07:01:13~14  状态总线 Closed，再丢 3 条
07:01:14  ✅ ACP host terminal shutdown complete，session_close_failures: 0
07:10:39  trial 结束
```

---

## 5. 根因分析（事实 → 推断）

**事实**：

1. 13,271 条丢弃集中在 06:50:39 ~ 06:59:12，速率最高约 111 条/秒；
2. 第一条丢弃出现在 workspace MCP 订阅停止消费后 **21.6 秒**；
3. 丢弃告警**只有 session_id 与 outcome 两个字段**，无事件类型；
4. 收尾时 3 个后台回执等满 120 秒被判超时，随后 0.31s / 1.90s 到达；
5. 状态总线在 `outcome=Closed` 状态下仍收到投递，再丢 3 条；
6. 本次 `session_close_failures: 0`，收尾本身成功。

**推断（供开发定位）**：

- `peri_controller::controller` 的事件通道是**有界 + 满即丢弃（drop-on-full）**策略。生产侧速度超过消费侧时不自适应，直接丢。
- workspace MCP 的订阅消费者在启动后约 7 秒即退出（`missing task scope capability`），很可能使通道少了一个消费者，导致积压迅速转为丢弃。**时间上吻合，但未读源码，不能视为已证实的因果。**
- 收尾时回执在超时后 0.31s / 1.90s 才被接住，说明事件确实被压在通道里，而不是根本没产生。

**已排除的外部因素**（见 `evidence/cause-elimination.txt`）：

- **不是网络**：网关侧 `peri-423-verify` 的 52 次模型调用 **全部 success**，0 失败；丢事件最猛的 06:51 仍有 15 次调用正常成功。
- **不是模型**：输出流无 `is_error`，51 个 assistant 轮次正常结束。
- **不是工具结果丢失**：80 个工具调用与 80 个工具结果 **一一配对，0 缺失**（日志中 130 行 tool_use 是因为每次调用记录两行：`input=null` 与实际 `input`）。
- **与模型输出量强相关**：13,271 条丢弃 / 29,228 output tokens ≈ 0.45 条/token；丢弃最多的分钟（06:51，7,123 条）正是模型输出最多的分钟。
- **倾向解释**：被丢弃的多为跟随流式输出产生的增量/进度类事件，在 print（无 UI 客户端）模式下仍全速生产、无人消费。

**明确声明**：以上推断仅基于本次日志时序与相关性，未查阅 peri 源码；日志不记录事件类型，因此"被丢的是进度类事件"无法直接证实。

---

## 6. 影响面

1. **评测侧排障能力受损**：99.7% 的日志被同一条告警占满，真实故障信息被淹没。
2. **结果不确定性**：若被丢弃的命中「工具执行结果 / 任务完成 / 会话关闭 / 生命周期」类事件，可能复现 #2 类假失败，或造成 agent 认为工具仍在运行；本次没有发生，但**无法排除**，因为日志不记录事件类型。
3. **可确认的损失**：至少 3 个后台任务回执因超时被放弃（该 3 个任务本身 `success=false`）。
4. **与 4.1.1 的对照（全量扫描，非单题）**：
   - 4.1.1 全量 **115 份** peri 日志中，`peri_controller::controller` **0 行**，`控制面弹出队列事件未投递` **0 次**；
   - 但 4.1.1 **并非不丢事件**：状态总线层的 `state_event_dropped` 出现在 **114/115 份**日志中，合计 **342 次**，平均每题约 3 次；同题 kgateway 为 3 次；
   - 4.2.3 同题为 controller 层 13,271 次 + bus 层 3 次。

5. **差异不是负载造成的**：同题 4.1.1 的后台任务更多（`registry.complete` 12+ 次，且触发过 shell 5/5 并发上限），却没有 controller 层丢弃；4.2.3 后台任务只有 3 个，反而丢弃上万次。两版触发条件完全相同（`-32602` → 订阅流停止消费），4.1.1 停摆后 9 分钟内无任何 controller 丢弃，4.2.3 停摆后 **21.6 秒**开始丢弃并持续约 8.5 分钟。

6. **日志措辞显示事件投递层被改写过**：4.1.1 为 `状态层通道已满，事件丢弃`（无 `outcome` 字段）；4.2.3 为 `状态层事件未投递 … outcome=Closed` 与 `控制面弹出队列事件未投递 … outcome=Dropped`，并新增 `peri_controller::controller` 作为生产者。
   **倾向判断**：这是 4.2.3 改动引入的问题；但「丢弃行为新增」与「仅日志新增」无法从日志区分，需源码确认。**单题单次运行，仍不足以定性为回归。**
5. **本次判分不受影响**：`exception_info = null`、exit 0，题目失败为能力问题。

---

## 7. 建议修复方向

1. **P1｜生命周期类事件不得丢弃**
   区分事件优先级：工具结果、任务完成、session close、状态迁移等使用可靠通道或阻塞背压；只有 UI 刷新等纯展示事件允许丢弃。

2. **P1｜丢弃时必须可追溯**
   在告警中记录：事件类型/kind、生产者模块、队列容量与当前水位、该类型累计丢弃数。否则线上无法评估损失。

3. **P2｜告警限流**
   同一区段的重复丢弃应聚合为「首次打印 + 累计计数」或每秒至多一条，避免 13,271 行把日志写死。

4. **P2｜排查通道容量与消费者生命周期**
   重点确认：workspace MCP 订阅消费者退出后，其他事件是否仍往同一通道投递；容量是否按最坏情况配置。

5. **P3｜workspace MCP 初始化能力缺失**
   `-32602 missing task scope capability` 在 4.2.3 中仍然出现（重试 3 次后停止消费）。若确认它是本 bug 的触发条件，应优先修复。

---

## 8. 证据文件清单（`evidence/`）

| 文件 | 内容 |
|---|---|
| `peri-print.2026-10-21.log` | peri 内部日志（脱敏 + 折叠版）：连续重复的 Dropped 告警折叠为一行并注明条数与起止时间，其余逐字保留（53 行） |
| `warning-summary.txt` | 按告警类型/区段/分钟的完整统计，及告警字段说明 |
| `timeline.txt` | 本次时间线与 4.1.1 同题对照 |
| `result.json` | pier 记录的判分结果（异常为空） |
| `peri-result-line.txt` | 输出流末条 result 与 token 用量；4.1.1 同题对照 |
| `peri-print.2026-10-20.v411-same-task.log` | 4.1.1 同题完整日志（59 行），用于两版对照 |
| `arm-comparison-411-vs-423.txt` | 4.1.1 全量 115 份日志的检索统计 + 同题逐项对照 + 能/不能得出的结论 |
| `cause-elimination.txt` | 归因排除法：网关 52/52 成功、工具结果 80/80 配对、丢事件与 output tokens 的逐分钟相关性 |

> 原始完整日志（2.18 MB / 13,311 行）留存在评测机 trial 目录；因 99.7% 为同一行重复，仓库内只收录折叠版，未删改任何非重复内容。
> 所有证据来自同一次真实运行，未做人工修改；密钥类内容已脱敏（`sk-*` / `glpat-*` 扫描均为 0 命中）。
