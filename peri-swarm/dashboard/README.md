# peri-swarm-dashboard

Peri Swarm 网关的终端管理客户端。crate 与二进制名均为 `peri-swarm-dashboard`，是一个
纯 HTTP 客户端：不依赖 `peri-swarm` 或任何业务 crate，只按网关公开的
`/v1/agents/sessions` 契约工作。界面文案全部为英文，集中在
[`src/ui/text.rs`](src/ui/text.rs)；鼠标是主输入方式，键盘提供等价路径。

权威设计见 [Peri Swarm Dashboard 设计](../../docs/design/peri-swarm-dashboard.md)。

## 运行

本仓库的 `Cargo.lock` 固定了受补丁的 `rmcp` 源，必须通过仓库脚本启动，避免锁文件被
普通 Cargo 重写：

```bash
# 等价的 crate 入口；裸命令 `cargo run -p peri-swarm-dashboard` 在本仓库会要求重写
# Cargo.lock，所以必须由脚本注入补丁后执行。
./scripts/cargo-rmcp-patched.sh run -p peri-swarm-dashboard
```

已构建时也可直接执行 `./target/debug/peri-swarm-dashboard`。终端至少需要 50x12；更小时
显示 “Terminal too small”并允许退出，不会 panic。首次运行需要能访问网关的
`GET /healthz` 和一次带 Bearer 的会话列表请求。

## 首次运行与配置

- 配置路径：`~/.config/peri-swarm-dashboard/config.toml`（由 `dirs::home_dir()` 推导）。
- 目录权限 `0700`，文件权限 `0600`；写入使用同目录临时文件 + 原子 rename。
- 顶层 TOML 键为 `base_url` 与 `api_key`；可选的 `[observability]` 表保存 OpenObserve
  连接（`base_url`、`org`、`user`、`password`）。旧配置文件没有该表时照常加载。
- 首次运行在 Onboarding 输入 Base URL（默认 `http://127.0.0.1:8080`）和 API Key。
  `Connect` 先请求 `GET /healthz`，再用 Bearer 请求 `GET /v1/agents/sessions?limit=1`；
  两步都成功才保存。失败停留在 Onboarding 显示具体错误，不写文件。
- Base URL 会去空白、去尾斜杠、去掉可选的 `/v1`，只接受 `http`/`https`；拒绝 userinfo、
  query、fragment 以及其他路径。非 loopback 的明文 `http://` 会显示
  “API key will be sent in cleartext”警告，但不阻止连接。
- 已有合法配置时直接进入 Session log；配置损坏时进入 Onboarding 并显示读取错误，
  不会自动覆盖。加载时若发现文件权限比 `0600` 宽，只警告，不静默 chmod。
- 401 会暂停认证轮询并显示持久错误横幅；不会自动删除配置。可在 Settings 重新认证。

## Observability 配置

Settings 的 **Observability** 区域显示 OpenObserve 连接状态（`not configured` /
`configured`、Base URL、Organization、User），`Configure observability` 打开配置子页：

- 字段：Base URL、Organization、User、Password；`Test` 请求 `GET /api/{org}/streams`
  验证连接，`Save` 通过同一份原子配置存储写盘，`Cancel` 返回 Settings。
- Base URL 接受 `http`/`https` 和可选路径前缀，但拒绝 userinfo、query、fragment；
  尾斜杠会被去掉。Organization/User 必须非空；Password 必须非空。
- Password 输入始终遮蔽。已保存的密码不会回显；编辑时留空表示沿用已保存的密码。
- 保存后 Settings 只显示状态、URL、org 与 user，从不显示密码。
- 旧配置文件没有 `[observability]` 时行为不变；带未知字段的文件仍按损坏配置处理。

**直连风险（重要）**：dashboard 用**专用 admin 账号**直接连接 OpenObserve。
OpenObserve OSS v1.2.0 没有 viewer/editor 角色，只有 admin/service account 可用，因此
该账号本身可以写入。客户端只发送 `SELECT`，但这是防误操作，**不是安全边界**。
安全性依赖：OpenObserve 只在内网/loopback 可达、使用专用账号（不要用 root）、
`config.toml` 的 `0600` 权限，以及凭据轮换。

配置示例（密码不要提交到仓库）：

```toml
base_url = "http://127.0.0.1:8080"
api_key = "…"

[observability]
base_url = "http://127.0.0.1:5080"
org = "default"
user = "peri-swarm-dashboard"
password = "…"
```

本阶段实现配置、连接测试、Core metrics、Agent metrics 与 Request log 页面；Sandbox
metrics 保持诚实的空页（网关没有资源级指标），Query 留给后续阶段。

## Metrics 与 Request log 数据源

- **Core metrics**：直连 OpenObserve，只使用 PromQL（该版本对 metrics 流不接受
  `_search` SQL）。查询 `peri_swarm_http_server_requests` 与
  `peri_swarm_http_server_duration_bucket`，时间窗 15m/1h/24h；页面可见时每 15 秒轮询。
  指标为累计计数器，而 v1.2.0 不支持 `rate()`/`increase()`（返回空矩阵），因此客户端读取
  少量原始样本并将正向增量相加（进程重启不会产生负流量）。错误率只统计 5xx；p95 由
  `le` 桶增量做直方图插值，落在 `+Inf` 桶时显示 `—`，不虚构有限值。页面展示实际样本
  覆盖范围；刷新失败保留上一次成功数据并标记 `stale`。
- **Agent metrics**：来自网关 `GET /v1/agents/metrics`，不依赖 OpenObserve。固定请求
  窗口内最多 500 个会话（`limit=500`），客户端每页 10 条；总计行使用响应的 `totals`。
  `truncated=true` 时显示 “Window truncated; totals cover the newest 500 sessions.”。
  缺失的 token 字段显示 `—`；Cache 列展示 `cache_read_tokens + cache_creation_tokens`。
- **Request log**：直连 OpenObserve，SQL 查询 `peri_swarm_logs`。SQL 固定以下形状：
  `WHERE _timestamp >= ? AND _timestamp < ? ORDER BY _timestamp DESC LIMIT 11 OFFSET ?`；
  返回 11 行是为了判断是否存在下一页，界面每页显示 10 行。时间、method、route、
  status、duration、session 任一缺失时显示 `—`。OpenObserve 的 `_search.total` 在带
  OFFSET 时不是全局总数，因此页面只显示 “Page N”，不声称总数。
- Core metrics 与 Request log 未配置 `[observability]` 时显示
  “Observability is not configured.” 并提供跳转 Settings 的按钮；Agent metrics 不受影响。
- 所有请求带 generation；切换窗口、换页或手动刷新会使旧响应失效，不会覆盖新数据。

## 安全说明

- API Key 只在内存与 `0600` 配置文件中存在；不打印、不回显、不进入错误消息或
  `Debug` 输出。Settings 只显示 `(set)`。
- OpenObserve 密码使用同一套 `0600`/`0700` 原子存储；`Debug` 与错误消息都会脱敏，
  密码输入遮蔽，保存后不再回显。OpenObserve 客户端同样禁用重定向并设置 10 秒超时，
  错误只含状态码与经过脱敏的短消息；响应体里的密码和 `Authorization` 头值都会被替换为
  `<redacted>`。
- dashboard 直连 OpenObserve 的专用 admin 账号：OSS 没有 viewer 角色，日志查询只发
  `SELECT`、指标只发 PromQL 查询，但客户端校验不是安全边界；必须内网部署、专用账号、
  保护并轮换密码。
- HTTP 客户端禁用自动重定向，`Authorization` 只发往配置的 origin；`/healthz` 不发
  Bearer。
- 启用 rustls 证书校验，没有“忽略证书”选项。
- 请求超时 10 秒；写操作使用 `Idempotency-Key`（UUID v7），不自动重试。轮询失败按
  3→6→12→24→30 秒退避并保留最后一份数据标记为 stale。
- 退出登录只有在配置文件确实删除后才报告成功；删除失败保留当前状态并显示错误。
- 仓库、测试和文档中不含密钥；`tests/live_gateway.rs` 只从环境变量读取测试凭据且从不
  打印它们。二进制没有 demo/mock 模式，fake 只存在于测试。

## 页面状态

侧栏共 13 项（Monitor 6 / Resources 4 / Platform 3），全部可见、可点击；没有 Overview。

| 分组 | 页面 | 状态 |
| --- | --- | --- |
| Monitor | Core metrics / Agent metrics / Request log | 已实现（OpenObserve / 网关数据） |
| Monitor | Sandbox metrics | 空页，说明网关没有资源级指标 |
| Monitor | Query | 空页，本构建未实现 |
| Monitor | Session log | 已实现（默认页） |
| Resources | Agents / Artifacts / Vaults / MCP | 空页，说明能力边界或“本构建未实现” |
| Platform | Automations / Nodes | 空页，说明能力边界或“本构建未实现” |
| Platform | Settings | 已实现 |

已实现功能：

- Session log：首屏加载 50 条，表下分页器每页显示 10 条（页码 + `‹ Prev`/`Next ›`，
  `PgUp`/`PgDn` 同效）；页码控件按最大页码的位数占固定列数，页码带按最宽窗口预留空间，
  翻页时 `Next ›` 不移动；在末页继续 `Next ›` 时再取 50 条，最多 500 条。列表在本地按
  最近活跃排序（`last_active_at` 降序，其次 `created_at` 降序；时间完全相同保持网关返回
  顺序）。状态 chips 与客户端搜索只作用于已加载集合；点击行进入详情。
- Session detail：Timeline / Turns / Info 三个标签；审批 Approve/Deny；发送消息；取消
  turn；编辑 model/metadata/prompt cache；删除。删除、取消和退出登录都有确认框。
- New session：Logical ID、Model、CWD、Instructions、Metadata 行与可折叠的
  tools/environment/prompt_cache_options JSON。成功后打开新会话详情。
- Settings：连接信息、`Test connection`、`Edit credentials`、`Configure observability`、
  `Sign out`、`Quit`。
- Core metrics：窗口 chips、Refresh、requests/min、5xx 错误率、p95、总请求数与实际数据
  范围；15 秒轮询，窗口 chips 可用鼠标或 `1`/`2`/`3`，`r` 刷新。
- Agent metrics：窗口 chips、Refresh、总计行、8 列表格、每页 10 条的 Prev/Next；
  `PgUp`/`PgDn` 同效。
- Request log：窗口 chips、Refresh、时间/method/route/status/duration/session 表格、
  每页 10 条的服务端分页；`PgUp`/`PgDn` 同效。
- 空页会给出“为什么为空”和“网关实际支持什么”，从不编造数据。

## 限制

- **只轮询，没有 SSE**。列表、详情和健康检查按固定间隔拉取；没有流式消费入口。
- **排序在客户端完成**：网关按 `logical_id desc` 返回，但 `logicalId` 是调用方任意字符串，
  不保证时间顺序。UI 在收到响应后按 `last_active_at` 降序、同值按 `created_at` 降序做稳定
  排序，时间完全相同时保留网关返回顺序；页脚标注
  `Newest activity first · N loaded`。搜索和计数仍只覆盖已加载集合。
- 单网关、单凭据；没有多 profile、Overview、指标聚合或跨节点视图。
- OpenObserve 的 PromQL 方言有限：`rate()`、`increase()` 与 `delta()` 返回空矩阵，
  因此 Core metrics 在客户端按累计计数器的正向增量计算窗口值；采样起点之前的首个增量
  无法归因，页面以实际样本范围说明覆盖区间。
- Core metrics 的 p95 依赖 OTel histogram 桶；落在 `+Inf` 桶或没有观测时显示 `—`。
  OTel SDK 默认的秒级桶较粗，p95 是桶内插值近似值。
- Request log 不提供总数（OpenObserve 在 OFFSET 下返回的 `total` 只是本页行数），
  只显示当前页码；访问日志不含 query string、headers 或 body。
- Agent metrics 只请求窗口内最多 500 个会话；响应 `truncated=true` 时总计覆盖扫描到的
  最新行，不是整个窗口的完整总计。
- Sandbox metrics、Query、Agents、Artifacts、Vaults、MCP、Automations、Nodes 在本构建中
  仍为空页，文案说明为什么为空。
- 编辑不发送 `instructions`：网关更新端点只接受 `agent.model`、`metadata`、
  `prompt_cache_options`（`deny_unknown_fields`），表单中 instructions 只读。创建表单也不
  发送首条消息。
- 不做 SSE、demo 模式、i18n 切换、不安全 TLS、剪贴板或指标持久化。

## 测试

```bash
./scripts/cargo-rmcp-patched.sh check --locked -p peri-swarm-dashboard --all-targets
./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm-dashboard
./scripts/cargo-rmcp-patched.sh clippy --locked -p peri-swarm-dashboard --all-targets -- -D warnings
```

测试包含配置/API/view-model/poll/app 单测，以及 80x24、100x30、140x40 的渲染与鼠标命中
测试。`tests/live_gateway.rs` 默认 `#[ignore]`，只有显式提供
`PERI_SWARM_DASHBOARD_IT_BASE_URL` 与 `PERI_SWARM_DASHBOARD_IT_API_KEY` 时才连接真实网关：

```bash
PERI_SWARM_DASHBOARD_IT_BASE_URL=http://127.0.0.1:8080 \
PERI_SWARM_DASHBOARD_IT_API_KEY=... \
  ./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm-dashboard \
  --test live_gateway -- --ignored
```

真实 TUI 交互（首启 onboarding、点击、发送、审批、编辑、删除）仍是人工验收步骤，不由
单元测试替代。
