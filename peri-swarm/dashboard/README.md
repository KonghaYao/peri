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
- 文件内容只有 `base_url` 与 `api_key` 两个 TOML 键。
- 首次运行在 Onboarding 输入 Base URL（默认 `http://127.0.0.1:8080`）和 API Key。
  `Connect` 先请求 `GET /healthz`，再用 Bearer 请求 `GET /v1/agents/sessions?limit=1`；
  两步都成功才保存。失败停留在 Onboarding 显示具体错误，不写文件。
- Base URL 会去空白、去尾斜杠、去掉可选的 `/v1`，只接受 `http`/`https`；拒绝 userinfo、
  query、fragment 以及其他路径。非 loopback 的明文 `http://` 会显示
  “API key will be sent in cleartext”警告，但不阻止连接。
- 已有合法配置时直接进入 Session log；配置损坏时进入 Onboarding 并显示读取错误，
  不会自动覆盖。加载时若发现文件权限比 `0600` 宽，只警告，不静默 chmod。
- 401 会暂停认证轮询并显示持久错误横幅；不会自动删除配置。可在 Settings 重新认证。

## 安全说明

- API Key 只在内存与 `0600` 配置文件中存在；不打印、不回显、不进入错误消息或
  `Debug` 输出。Settings 只显示 `(set)`。
- HTTP 客户端禁用自动重定向，`Authorization` 只发往配置的 origin；`/healthz` 不发
  Bearer。
- 启用 rustls 证书校验，没有“忽略证书”选项。
- 请求超时 10 秒；写操作使用 `Idempotency-Key`（UUID v7），不自动重试。轮询失败按
  3→6→12→24→30 秒退避并保留最后一份数据标记为 stale。
- 退出登录只有在配置文件确实删除后才报告成功；删除失败保留当前状态并显示错误。
- 仓库、测试和文档中不含密钥；`tests/live_gateway.rs` 只从环境变量读取测试凭据且从不
  打印它们。二进制没有 demo/mock 模式，fake 只存在于测试。

## 页面状态

侧栏共 13 项（Monitor 5 / Resources 5 / Platform 3），全部可见、可点击；没有 Overview。

| 分组 | 页面 | 状态 |
| --- | --- | --- |
| Monitor | Core metrics / Agent metrics / Sandbox metrics / Request log | 空页，说明网关能力边界 |
| Monitor | Session log | 已实现（默认页） |
| Resources | Agents / Environment templates / Skills / Files / Vaults | 空页，说明网关能力边界 |
| Platform | Projects and keys / Nodes | 空页，说明网关能力边界 |
| Platform | Settings | 已实现 |

已实现功能：

- Session log：加载 50 条，`Load more` 每次 +50，最多 500；状态 chips 与客户端搜索只作用于
  已加载集合；点击行进入详情。
- Session detail：Timeline / Turns / Info 三个标签；审批 Approve/Deny；发送消息；取消
  turn；编辑 model/metadata/prompt cache；删除。删除、取消和退出登录都有确认框。
- New session：Logical ID、Model、CWD、Instructions、Metadata 行与可折叠的
  tools/environment/prompt_cache_options JSON。成功后打开新会话详情。
- Settings：连接信息、`Test connection`、`Edit credentials`、`Sign out`、`Quit`。
- 空页会给出“为什么为空”和“网关实际支持什么”，从不编造数据。

## 限制

- **只轮询，没有 SSE**。列表、详情和健康检查按固定间隔拉取；没有流式消费入口。
- **加载顺序是网关的 `logical_id desc`**，UI 只标注
  `Gateway order (logical id desc) · N loaded`，不宣称完整历史或时间顺序。搜索和计数都只
  覆盖已加载集合。
- 单网关、单凭据；没有多 profile、Overview、指标聚合或跨节点视图。
- Request log 无数据来源（网关不提供服务器请求日志）；Agents、Environment templates、
  Skills、Files、Vaults、Projects and keys、Nodes 均未实现。
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
