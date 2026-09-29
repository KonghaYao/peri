# Git Watch 下沉到 builtin workspace（MCP 2026-07-28 订阅回传）——实施计划与裁决记录

- 状态：已裁决，交实施 owner 执行（2026-09-29；基线 HEAD `c110089c`，分支 `feat/mcp-adaptation-v4-part-3`）
- 上游记录：
  - `docs/design/mcp-adaptation-v4-part-1.md:192`：设计目标「完全下放 → Workspace MCP；Agent 侧不再保留 GitWatch middleware」（未定回传机制、未实施）
  - `spec/issues/2026-09-27-mcp-adaptation-v4-part-4-plan.md:32`：「不在本波。下放它需要新机制（MCP 通知或宿主事件端口），是独立设计题，登记为 wave 4」（该登记漏进验收 backlog 表格——本次落地即补上）
- 协议口径：MCP 2026-07-28 标准 = `subscriptions/listen` + `notifications/resources/updated`（见 `docs/reference/mcp-ecosystem.md` §5，约 :267-294）
- 本文件同时是裁决记录；实施完成后在 §8 追加验证证据。

## 0. 可行性（已核实到源码/行号）

| 事实 | 证据 |
| --- | --- |
| rmcp 3.1.4 有完整服务端推送面：`accepted_subscription_filter` + `listen(SubscriptionContext)` + `SubscriptionSink::notify_resource_updated(uri)` | `~/.cargo/registry/src/*/rmcp-3.1.4/src/handler/server.rs:411-431`；`src/service/server.rs:139-160,177-260,312-320,328-401` |
| 客户端收通知面已完备，**无需改动路由层** | `rmcp-3.1.4/src/service/client.rs:1174,1194,1224,462-492`；`peri-middlewares/src/mcp/client/subscription.rs:96-113` |
| 服务端要收订阅，`get_info()` 必须声明 `resources.subscribe=true`（SDK 把 filter 与 capabilities 求交） | `rmcp-3.1.4/src/handler/server.rs:157-160`；`src/model.rs:1993-2019`（`supported_by`） |
| 仓库**零** builtin 通知生产者、零服务端推送测试 → 本变更即首个生产点 | `mcp-packages/*/src/server*.rs` 均只有 `enable_tools()`；`mcp-packages/common/src/helpers.rs:14-17`（`server_info()` 固定 tools-only，**不要**复用它） |
| 会话送达缝合 | `McpClientPool::session_inboxes`（`peri-middlewares/src/mcp/client.rs:133`）、`McpSubscriptionPort::{register,unregister}_inbox`（`:444-453`；注册点 `peri-acp/src/session/bridges.rs:77`）、`InboxHandle::push_system_reminder(kind, source, reminder)`（`peri-acp-types/src/session/inbox.rs:135-142`）、`MessageKind::Info` 不唤醒（`peri-acp-types/src/session/queue.rs:22-27`） |
| builtin 实例 per-session、watch root = 实例构造期冻结 cwd | `peri-acp/src/host/workspace.rs:59-126`；对齐 LSP 裁决 A22（root=宿主 cwd） |
| 旧关闭键退役有既有判例 | `FilesystemMiddleware`/`TerminalMiddleware` → 未知键 warn+忽略（`docs/meta-harness.md:84-86`；`peri-acp/src/provider/config.rs:415-422`） |

## 1. 裁决（实施约束）

- **D-1 触发语义**：服务端 `call_tool` 成功返回后采样；**不引入周期 tick**（保留既有非目标「不 inotify、不后台轮询」，`docs/design/git-watch-middleware.md:31`，及 e2e「git 安静」前提 `e2e/tests/scenarios/workspace-slow-git.test.ts:223`）。
  - 语义收窄（唯一且已知）：非 workspace 工具（SubAgent/PTC/Workflow/外部 MCP）改动 git 时，延迟到下一次 workspace 工具调用才被发现。
- **D-2 资源正文** = 「最近一次采样结论」：有变化时为旧 `info_message_if_changed` 文案（含 `prev → curr` 箭头）；无变化时为快照文本；未采样时为固定文本。**reminder body = 资源正文逐字**。
- **D-3** `git` 子进程 `kill_on_drop(true)`（旧实现超时后子进程成孤儿；一行修复）。
- **D-4** 队列来源标识用 `MessageSource::DynamicMcpNotification`（旧 `SystemInjected` 仅为宿主注入语义；全仓无按旧值过滤 git 提醒的代码）。
- **D-5（对设计建议的修订——不引入配置面）**：**不新增** `ResourceReminderProfile` / `McpSubscriptionsConfig.resource_reminders` 及相关校验。git 提醒映射内置于宿主（`peri-middlewares`），仅绑定 builtin workspace 实例名 + `GIT_REF_RESOURCE_URI`。理由：首个真实用例是内置实例（语义由我们控制），复用型配置抽象按仓库工程原则 1 等第二个真实用例再提取；对应既定口径「system mcp 直接注入，否则按现有机制」。
  - 粗粒度关闭 = `WorkspaceMiddleware: false`（关闭实例的同时**跳过订阅建立**，见 §3.C 关闭门，满足 ARC-CAPABILITY-CLOSURE-001）；
  - 细粒度关闭 = 既有实例 `subscriptions` 配置（用户显式写 `subscriptions` 即优先于默认注入；空配置 ⇒ 不订阅）。
- **逐字保持项**（以源文件为事实源 `peri-middlewares/src/git_watch/{mod.rs:97-107,171-195,208-242,246,250-260,snapshot.rs}`）：触发门（仅成功、`is_error != Some(true)`）、单飞、60s 节流（按采样完成计时；采样失败不推进）、`NotRepository` 首次短路后不再 spawn、1s 超时、`GIT_OPTIONAL_LOCKS=0`、notice 文案、以及 reminder 元数据逐字段一致（category=Diagnostic / source=`git_watch` / kind=`repository_ref_changed` / severity=Info / delivery=Configurable / audiences=[Model,Tui,Diagnostics] / summary / wake=false）。**搬迁时以源文件原文为准逐字搬运**。

## 2. 数据流

```
workspace 工具成功返回（server call_tool）
  └─ spawn（不阻塞响应）：节流 60s + 单飞 → git rev-parse（GIT_OPTIONAL_LOCKS=0，1s 超时，kill_on_drop）
       └─ 与上次快照比较 → 变化？→ 更新资源正文（notice 文案）
            └─ SubscriptionSink::notify_resource_updated("workspace://git/ref")
                 └─(同进程 duplex，_meta.subscriptionId 路由)─> 客户端 Subscription 流
                      └─ 客户端：失效资源缓存 → resources/read(uri)（带超时；失败/超时回退通用提醒）
                           └─ 组装 canonical SystemReminder（逐字段同旧）
                                └─ InboxHandle::push_system_reminder(Info, DynamicMcpNotification, r)（不唤醒）
                                     └─ 会话队列 → 下一 Receive → transcript <system-reminder>
```

无订阅者（sink 表空 / 实例关闭 / 用户覆盖为空）⇒ **不采样**（零 git 调用）。

## 3. 逐文件改动

### A. `mcp-packages/workspace`（通知生产者）

| 动作 | 文件 | 关键符号 |
| --- | --- | --- |
| 新建 | `src/git_watch.rs`（≈200 行） | `pub const GIT_REF_RESOURCE_URI: &str = "workspace://git/ref"`；`GitSnapshot{branch,head}`、`SampleOutcome`、`parse_sample_stdout`、`short_hash`、`notice_text(prev,current)->Option<String>`（旧 `info_message_if_changed` 逐字搬迁）、`snapshot_text`、`pub async fn run_git_sample(cwd)->SampleOutcome`（`GIT_OPTIONAL_LOCKS=0` + `kill_on_drop(true)`）、`GIT_SAMPLE_TIMEOUT=1s`/`GIT_THROTTLE=60s`、`pub struct GitWatchState`：`new()` / `with_timing(throttle,timeout)`（测试 seam）/ `begin_sample()->bool`（NotRepository 短路 + in_flight CAS + 完成时刻节流）/ `finish_sample(SampleOutcome)->Option<String>`（更新 repo_mode、快照、资源正文；返回需通知的 notice）/ `resource_text()->String` |
| 新建 | `src/git_watch_test.rs` | T1–T3（§4）；挂载 `#[cfg(test)] #[path = "git_watch_test.rs"] mod tests;` |
| 修改 | `src/workspace.rs` | 字段 `git: Arc<GitWatchState>`、`sinks: Arc<Mutex<BTreeMap<RequestId, SubscriptionSink>>>`（`parking_lot::Mutex`；`new()` 签名不变）；`get_info()` = `ServerCapabilities::builder().enable_tools().enable_resources().enable_resources_subscribe().build()`（**不复用** `peri_mcp_common::server_info()`）；`call_tool()` 在成功返回后 `spawn_git_sample(...)`；新增 `list_resources()`（单条 `Resource::new(GIT_REF_RESOURCE_URI,"git-ref").with_mime_type("text/plain")`）、`read_resource()`（命中→`ReadResourceResult::new(vec![ResourceContents::text(...)])`；未命中→`McpError::invalid_params`）、`accepted_subscription_filter()`（filter ∩ {GIT_REF_RESOURCE_URI}）、`listen()`（注册 sink → `ctx.cancelled().await` → 注销）；私有 `spawn_git_sample`（无 sink 直接返回；`tokio::spawn` + timeout + `finish_sample` + 逐 sink notify；`SubscriptionClosed` 时移除该 sink） |
| 修改 | `src/lib.rs` | `mod git_watch;` + `pub use git_watch::{GIT_REF_RESOURCE_URI, GitWatchState};`（其余符号 crate 内用） |

### B. `peri-acp-types`

| 动作 | 文件 | 改动 |
| --- | --- | --- |
| 修改 | `src/meta_harness.rs:111-134` | `MIDDLEWARE_NAMES` 删 `"GitWatchMiddleware"`（变为未知键 warn+忽略，同既有判例）；`BUILTIN_INSTANCE_POLICY_KEYS` 不动；补一行 v4 wave 4 说明 |

（**不新增**协议/配置类型——见 D-5。）

### C. `peri-middlewares`（宿主装配 + 客户端消费）

| 动作 | 文件 | 改动 |
| --- | --- | --- |
| 新建 | `src/mcp/builtin/workspace_subscription.rs`（≈50 行） | `pub(crate) fn workspace_default_subscriptions() -> McpSubscriptionsConfig`（`resources=[GIT_REF_RESOURCE_URI]`）；模块头注明「提醒元数据由宿主内置（D-5），不从 server 取」 |
| 修改 | `src/mcp/builtin/mod.rs` | `builtin_default_entry()` 为 workspace 填 `subscriptions`；规则 2 分支（条目已存在）在 `entry.subscriptions.is_none()` 时补默认（用户显式配置优先）；更新冻结规则模块文档 |
| 修改 | `src/mcp/builtin/dispatch.rs:50-90` | **必改**（否则 resources/订阅穿不过 builtin 链路）：`BuiltinServerHandler` 增四转发——`list_resources`（Workspace→转发，其余→`ListResourcesResult::default()`）、`read_resource`（Workspace→转发，其余→`method_not_found`）、`accepted_subscription_filter`（Workspace→转发，其余→`None`）、`listen`（Workspace→转发，其余→`ctx.cancelled().await; Ok(())`） |
| 修改 | `src/mcp/initialize.rs:429-433`、`src/mcp/reconnect.rs:252-260` | 关闭门：`closed` 命中该实例 ⇒ 跳过 `setup_subscription`（可抽 `pool.subscription_allowed(server)` 两处共用） |
| 修改 | `src/mcp/client/subscription.rs` | 收 `ResourceUpdatedNotification` 后：若 server = builtin workspace 且 uri = `GIT_REF_RESOURCE_URI` → 失效资源缓存 → 带超时 `resources/read` → 取文本 → 组装 canonical git_watch reminder（抽纯函数便于单测，如 `git_watch_reminder_from_resource(uri, body) -> (MessageKind, SystemReminder)`）→ `Info` 不唤醒；读取失败/超时 → 回退既有通用提醒（事件不丢）；其余 URI 走既有路径不变 |
| 修改 | `src/mcp/client_test.rs`（或新增 `src/mcp/client/subscription_test.rs`） | 映射纯函数单测（不唤醒、字段逐一致、回退、截断） |
| 新建 | `src/mcp/builtin_subscription_wire_test.rs` | 线路级 T4–T6（真 `spawn_builtin_transport_with_handler` + `serve_client_auto` + `peer.listen`，照 `builtin_spike_test.rs` 夹具） |
| 修改/删除 | `src/assembly.rs:42,271-273` 删 import 与 `ChainSlot::GitWatch` 两支，`:262-266` 注释同步；`src/attribution/mod.rs:40` 注释去 GitWatch 引用；`src/git_watch/{mod.rs,snapshot.rs,mod_test.rs}` 整目录删；`src/lib.rs:28,76` 删两行 | |

### D. `peri-agent`

| 动作 | 文件 | 改动 |
| --- | --- | --- |
| 修改 | `src/session/factory.rs:51-55,105` | 删 `ChainSlot::GitWatch` 变体与蓝本项；更新组注释（ARC-MIDDLEWARE-001：只经蓝本+装配实现） |

### E. 测试期望同步（同一变更内改期望值，不改断言强度）

| 文件 | 改动 |
| --- | --- |
| `peri-middlewares/src/assembly_test.rs:493` | 删 `slot_middleware_name` 的 GitWatch 臂 |
| `peri-middlewares/src/assembly_test_baseline.rs:5,36,69,101,543` | 删 `"GitWatch"`（blueprint 名序列、slot_name 臂、默认/全开链序列）；`:5` 槽位数改为实际值（本变更后 ChainSlot 变体 21 个） |
| `peri-middlewares/src/assembly_test_meta.rs` | 预期无需改（随常量自动收缩）；如红按事实修正 |

### F. 文档（DOC-UPDATE-001）

| 文件 | 改动 |
| --- | --- |
| `docs/design/git-watch-middleware.md` | 状态改「已下放（v4 wave 4）」；事实源指向 `mcp-packages/workspace/src/git_watch.rs` + 订阅链路；§2 触发语义改述（服务端 call_tool 后采样、无订阅不采样）；链装配章节删除并注明；§8/§9 测试落点更新；新增「与旧实现差异表」；顺带按代码事实修正 hooks 漂移（`:128` 声称 before_agent，实际只有 after_tool） |
| `docs/design/mcp-adaptation-v4-part-1.md:192` | 「目标：完全下放」→「已下放」 |
| `docs/design/middleware-system.md:107,125` | 槽位数、git_watch 行 |
| `docs/design/system-reminder.md:101,108,276` | producer 改述（MCP 订阅回传）；`:276` severity 按代码事实改 Info |
| `docs/meta-harness.md`（:84-86 旁） | `GitWatchMiddleware` 已非已知键；关闭办法 = `WorkspaceMiddleware: false` 或实例 `subscriptions` 覆盖 |
| `docs/code-index/peri-middlewares.md:28,114` | 队列能力表述（去 GitWatch）＋订阅消费说明 |
| `docs/code-index/mcp-packages.md` | Workspace 行补 `git_watch.rs` 与 `cargo test -p peri-mcp-workspace --lib -- git_watch` |
| `docs/code-index/peri-acp-types.md` | `MIDDLEWARE_NAMES` 变更说明（若该文件有清单） |
| `spec/issues/2026-09-27-mcp-adaptation-v4-part-4-plan.md:32` | 历史记录：加注「已由本文件落地」，不改历史结论 |
| `e2e/tests/scenarios/workspace-slow-git.test.ts:223` | 注释按新触发口径更新 |

未改：`peri-mcp-common`（保持 tools-only `server_info`）、`example/minimal/**`（无配置命中）。

## 4. 测试计划

**Step 0（先做，失败即停）** —— spike 验证 rmcp 推送面在 builtin duplex 链路可用（落点 `peri-middlewares/src/mcp/builtin_subscription_wire_test.rs`）：
① 未声明 `resources.subscribe` 时 `peer.listen` 必须失败（`method_not_found`）；② 声明后 `listen` 成功且 ack 带 subscriptionId；③ `sink.notify_resource_updated(uri)` 到达客户端 `Subscription::next()`；④ filter 外 URI 推送被 SDK 拒（`NotificationNotAccepted`）；⑤ 客户端 drop `Subscription` 后服务端 `cancelled()` 返回、`send` 返回 `SubscriptionClosed`。
**①–⑤ 任一不成立 ⇒ 停止施工并报告**（方案唯一未运行时验证的假设）。

| ID | 层 | 落点 | 断言要点 |
| --- | --- | --- | --- |
| T1 | 服务端纯逻辑 | `mcp-packages/workspace/src/git_watch_test.rs` | `parse_sample_stdout` 三态；`notice_text` 仅列变化项、短 hash 7 位；首采样/无变化 → `None` |
| T2 | 服务端状态机 | 同上（`with_timing` 注入） | 节流窗口内不重复采样；单飞；`NotRepository` 短路后不再 spawn；采样失败不推进节流 |
| T3 | 服务端真实 git | 同上（`git init -b main` + config user.*，锁 `peri_mcp_common::process_env::lock()`，照旧 `mod_test.rs:50-77` 写法） | 首采样基线与 commit 后第二次采样的 notice 文案 |
| T4 | 线路（server 侧） | `workspace_test.rs` 或 wire test | `get_info().capabilities.resources.subscribe == Some(true)`；`resources/list` 含该 URI；`read_resource` 命中/未命中（`-32602`）；`accepted_subscription_filter` 交集语义 |
| T5 | 线路（端到端） | wire test | `listen` → 临时仓库 commit → 触发一次 workspace 工具调用 → 客户端收到 `ResourceUpdatedNotification`（uri + subscriptionId）→ 随后 `resources/read` 返回 notice 正文 |
| T6 | 会话送达 + 隔离 | wire test | `register_inbox` 后：kind=Info、`wakes_up()==false`、reminder 逐字段 = 旧契约；`WorkspaceMiddleware:false`（closed 命中）⇒ 不建立订阅、零 git 调用；`subscriptions` 空覆盖 ⇒ 不建立订阅 |
| T7 | 客户端映射纯函数 | `mcp/client(subscription_)test.rs` | 字段逐一致、Info 不唤醒、读取失败回退通用提醒、body 截断 |
| T8 | 装配/关闭面 | `assembly::tests` 既有入口 | 链序无 GitWatch、槽位 21、两表不变式仍绿、`WorkspaceMiddleware:false` 四面矩阵不退化 |

**回归面**：`cargo test -p peri-mcp-workspace --lib`、`-p peri-acp-types --lib`、`-p peri-middlewares --lib`、`-p peri-agent --lib -- session::`；e2e `workspace-slow-git` / `workspace-no-git`（确认「git 安静」前提与关闭时零 git 调用）。

**可 E2E 验证项**：临时 git 仓库建会话 → 首次工具调用（建基线）→ `git commit` → 再调用一次工具 → transcript 出现含 `[Git watch]` + `HEAD:` 的 system-reminder；`WorkspaceMiddleware: false` 同场景无 reminder。

## 5. 验收命令

```bash
cargo test -p peri-mcp-workspace --lib -- git_watch
cargo test -p peri-mcp-workspace --lib
cargo test -p peri-acp-types --lib -- meta_harness::tests
cargo test -p peri-middlewares --lib -- mcp::builtin
cargo test -p peri-middlewares --lib -- mcp::client
cargo test -p peri-middlewares --lib -- assembly::tests
cargo test -p peri-middlewares --lib
cargo test -p peri-agent --lib
cargo test -p peri-acp --lib
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
bash scripts/check-file-size.sh
git diff --check
```

（过滤词先用 `-- --list` 确认真实命中名非 0。）

## 6. 风险

1. **rmcp 推送未运行时验证**（最高风险）：以 Step 0 前置闭合；降级路径查 `builtin/runtime.rs:24-29` duplex 背压/帧序。
2. **覆盖收窄**（D-1 固有代价）：如需闭合非 workspace 工具改动，再议「仅订阅活跃时的 60s tick」（须同步改文档非目标与 e2e 前提）。
3. **状态机搬迁易漏三条**：采样失败不推进节流 / `NotRepository` 短路后不再 spawn / 首采样不通知——T2 逐条锁定。
4. **关闭门遗漏**会让 ARC-CAPABILITY-CLOSURE-001 退化——T6 锁定。
5. **跨平台**：测试沿用旧写法；确认 `mcp-packages/workspace` 的 `tempfile`（dev-dep）与 tokio process feature。
6. **信任边界**：读回的正文按不可信 payload 处理（限长、不进控制状态）。

## 7. 与未提交 resources plan 的 rebase 点

`spec/issues/2026-09-29-workspace-mcp-resources-plan.md`（评审中、未提交）与本变更共享缝合点；**本变更先落地**，该 plan 后 rebase：
`mcp-packages/workspace/src/workspace.rs`（get_info 能力位 / list_resources / read_resource 分派）、`peri-middlewares/src/mcp/builtin/dispatch.rs`（四转发 Workspace arm）、`peri-middlewares/src/mcp/builtin/mod.rs`（默认注入）、`peri-middlewares/src/mcp/builtin/context.rs`（他们加 input 字段，本变更不加）、`peri-middlewares/src/mcp/client/subscription.rs`（他们改 cache/失效，本变更改通知→提醒分支）、`peri-acp-types/src/plugin.rs`。

## 8. 实施记录（实施完成后追加：改动文件、验证证据、未验证项）

### 8.1 Step 0（先证伪）结论

落点 `peri-middlewares/src/mcp/builtin_subscription_wire_test.rs`，5 项探针全绿：

| 探针 | 结果 | 证据 |
| --- | --- | --- |
| ① 未实现 `accepted_subscription_filter`（rmcp 默认 `None`）⇒ `peer.listen` 失败 `-32601` | ✅ 成立 | `step0_1_unimplemented_filter_listen_is_method_not_found` |
| ①′ handler 返回 filter 但未声明 `resources.subscribe` ⇒ SDK 把 filter 求交为空 | ✅ 成立（**计划 ① 措辞需修正**：此形态下 `listen` 仍成功，只是 ack 的 `resource_subscriptions` 为空 ⇒ 后续推送会被 `NotificationNotAccepted` 拒） | `step0_1b_missing_subscribe_capability_narrows_filter_to_empty` |
| ② 声明后 `listen` 成功、ack 带 subscriptionId 与请求 URI | ✅ 成立 | `step0_2_3_...`（`sink.id() == subscription.id()`） |
| ③ `sink.notify_resource_updated(uri)` 到达客户端 `Subscription::next()` | ✅ 成立 | 同上；**注意订阅 id 在通知的 `extensions` 层**（`get_meta().subscription_id()`），不是 `params.meta`（入站恒 `None`） |
| ④ filter 外 URI 推送被 SDK 拒（`NotificationNotAccepted`） | ✅ 成立 | `step0_4_out_of_filter_push_is_rejected` |
| ⑤ 客户端 drop `Subscription` ⇒ 服务端 `cancelled()` 返回、`send` 返回 `SubscriptionClosed` | ✅ 成立 | `step0_5_drop_cancels_server_listen_and_closes_sink` |

### 8.2 改动文件清单

**新建**

| 文件 | 内容 |
| --- | --- |
| `mcp-packages/workspace/src/git_watch.rs` | `workspace://git/ref` 常量、`GitSnapshot`/`SampleOutcome`、`notice_text`（旧 `info_message_if_changed` 逐字）、`snapshot_text`、`run_git_sample`（`GIT_OPTIONAL_LOCKS=0` + `kill_on_drop`）、`GitWatchState`（节流/单飞/短路/正文） |
| `mcp-packages/workspace/src/git_watch_test.rs` | T1–T3 |
| `peri-middlewares/src/mcp/builtin/workspace_subscription.rs` | `workspace` 默认订阅（唯一实例名字面量）与规则 7 的映射 |
| `peri-middlewares/src/mcp/builtin_subscription_wire_test.rs` | Step 0 探针 ①–⑤ + 探针夹具 |
| `peri-middlewares/src/mcp/builtin_subscription_workspace_wire_test.rs` | T4–T7b（真实 handler 线路证据；拆子模块以满足 STD-SIZE-001） |
| `peri-middlewares/src/mcp/client/subscription_test.rs` | T7 映射/判定纯函数单测 |

**修改**

| 文件 | 改动 |
| --- | --- |
| `mcp-packages/workspace/src/workspace.rs` | `git`/`sinks` 字段；`get_info` 声明 resources+subscribe（不复用 tools-only `server_info`）；`call_tool` 成功返回后 `spawn_git_sample`；`list_resources`/`read_resource`/`accepted_subscription_filter`/`listen`；`with_git_throttle`（测试 seam） |
| `mcp-packages/workspace/src/lib.rs` | `mod git_watch;` + 再导出 `GIT_REF_RESOURCE_URI`/`GitWatchState` |
| `peri-acp-types/src/meta_harness.rs` | `MIDDLEWARE_NAMES` 摘 `GitWatchMiddleware`（+ v4 wave 4 说明） |
| `peri-middlewares/src/mcp/builtin/mod.rs` | 挂 `workspace_subscription`、规则 7（默认条目 + 规则 2 补默认）、冻结规则文档 |
| `peri-middlewares/src/mcp/builtin/dispatch.rs` | `BuiltinServerHandler` 四转发（list_resources / read_resource / accepted_subscription_filter / listen） |
| `peri-middlewares/src/mcp/client.rs` | `McpClientPool::subscription_allowed`（A24 关闭集 ⇒ 不建立订阅） |
| `peri-middlewares/src/mcp/initialize.rs`、`reconnect.rs` | 订阅建立门（两处同一判定） |
| `peri-middlewares/src/mcp/client/subscription.rs` | git ref 通知分支：回读资源 → 宿主内置 reminder；回读失败回退通用提醒；`is_git_watch_resource` / `git_watch_reminder_from_resource` / `truncate_body` / `first_text_body` |
| `peri-middlewares/src/assembly.rs`、`attribution/mod.rs`、`lib.rs` | 摘除 GitWatch 引用 |
| `peri-middlewares/src/assembly_test.rs`、`assembly_test_baseline.rs` | 期望值同步（蓝本序列、`slot_name` 臂、默认/全开链、Permission/AskUser 位置 13/12 → 12/11、新增槽位数 21 断言） |
| `peri-middlewares/src/mcp/builtin/builtin_test.rs`、`builtin_apply_test.rs` | 规则 7 期望值 + 两个新过层用例（默认注入、用户显式覆盖优先） |
| `peri-middlewares/src/mcp/mod.rs` | 挂载两个线路测试模块 |
| `peri-agent/src/session/factory.rs` | 删 `ChainSlot::GitWatch` 与蓝本项 |
| `peri-acp/src/host/assemble.rs`、`workspace.rs`、`stdio/mod.rs`；`peri-tui/src/{launch,cli_print}.rs`；`peri-acp/src/host/{workspace_seam_test,requests_cron_test,mcp_v4_wave2_test}.rs` | A24 关闭集注入（`HostAssemblyInput::builtin_closed` → `BuiltinInstanceContext::with_closed`）：会话装配从 frozen 派生，顶层路径传空集 |
| 文档 | `docs/design/git-watch-middleware.md`（重写为已下放态 + 差异表）、`mcp-adaptation-v4-part-1.md`、`middleware-system.md`、`system-reminder.md`、`docs/meta-harness.md`、`docs/code-index/{peri-middlewares,mcp-packages,peri-acp-types}.md`、`spec/issues/2026-09-27-...-part-4-plan.md`（历史加注）、`e2e/tests/scenarios/workspace-slow-git.test.ts`（注释） |

**删除**：`peri-middlewares/src/git_watch/{mod.rs,snapshot.rs,mod_test.rs}`（整目录）、`ChainSlot::GitWatch` 与两支装配臂、`lib.rs` 两行导出。

### 8.3 验证证据

基线：HEAD `c110089c`（分支 `feat/mcp-adaptation-v4-part-3`），macOS，未 commit（工作区状态）。
命令均按 §5 原样执行；下表为 EXIT 与关键输出行（`/tmp/peri-acceptance.log`、`/tmp/peri-acceptance2.log`）。

| # | 命令 | EXIT | 关键输出行 |
| --- | --- | --- | --- |
| 1 | `cargo test -p peri-mcp-workspace --lib -- git_watch` | 0 | `test result: ok. 11 passed; 0 failed; 286 filtered out`（T1–T3） |
| 2 | `cargo test -p peri-mcp-workspace --lib` | 0 | `test result: ok. 296 passed; 0 failed; 1 ignored` |
| 3 | `cargo test -p peri-acp-types --lib -- meta_harness::tests` | 0 | `test result: ok. 8 passed; 0 failed` |
| 4 | `cargo test -p peri-acp-types --lib` | 0 | `test result: ok. 488 passed; 0 failed` |
| 5 | `cargo test -p peri-middlewares --lib -- mcp::builtin` | 0 | `test result: ok. 112 passed; 0 failed` |
| 6 | `cargo test -p peri-middlewares --lib -- mcp::client` | 0 | `test result: ok. 70 passed; 0 failed` |
| 7 | `cargo test -p peri-middlewares --lib -- assembly::tests` | 0 | `test result: ok. 38 passed; 0 failed` |
| 7b | `cargo test -p peri-middlewares --lib -- mcp::builtin_subscription_wire` | 0 | `test result: ok. 10 passed; 0 failed`（Step 0 ×5 + T4/T5/T6/T6b/T7b） |
| 8 | `cargo test -p peri-middlewares --lib` | 0 | `test result: ok. 1635 passed; 0 failed; 4 ignored`（夹具收紧后复跑同为全绿） |
| 9 | `cargo test -p peri-agent --lib` | 0 | `test result: ok. 900 passed; 0 failed` |
| 10 | `cargo test -p peri-acp --lib` | 0 | `test result: ok. 767 passed; 0 failed` |
| 11 | `cargo check --workspace --all-targets` | 0 | `Finished dev profile ... in 19.48s` |
| 12 | `cargo clippy --workspace --all-targets -- -D warnings` | 0 | `Finished dev profile ... in 30.32s`（零 warning） |
| 13 | `cargo fmt --all --check` | 0 | 无输出（已 `cargo fmt --all` 格式化） |
| 14 | `bash scripts/check-file-size.sh` | 1 | `共扫描 1756 个文件，超阈值 33 个（源码 4 / 测试 29）`；**33 个均为基线既有文件，与本变更零交集**（本次新增 `builtin_subscription_wire_test.rs` 432 行 / `builtin_subscription_workspace_wire_test.rs` 629 行，均 < 1000） |
| 15 | `git diff --check` | 0 | 无输出 |

补充（非 §5 命令）：

| 命令 | EXIT | 关键输出行 |
| --- | --- | --- |
| `cargo test -p peri-middlewares --lib -- mcp::builtin_apply` | 0 | `19 passed`（含规则 7 的默认注入 / 用户显式覆盖两个新用例） |
| `cargo test -p peri-middlewares --lib -- mcp::client::subscription::tests` | 0 | `3 passed`（T7：字段逐一致 / 不唤醒 / 截断 / 绑定面） |

### 8.4 与 §3 的偏差项

| # | 偏差 | 原因 |
| --- | --- | --- |
| 1 | `WorkspaceMcpServer::with_git_throttle`（新增公开方法） | 计划未列；T5/T6 需在同一链路内观察「基线 → commit → 二次采样」，60s 真实节流不可等待 |
| 2 | `sinks` 用 `HashMap`（计划写 `BTreeMap`） | `RequestId = NumberOrString` 只派生 `Clone/Eq/Hash`，无 `Ord` |
| 3 | **关闭集生产注入**（peri-acp 3 文件 + peri-tui 2 文件 + 3 测试夹具） | 计划 §3.C 只列 initialize/reconnect 两处 gate，但 `BuiltinInstanceContext.closed` 生产上从未被填充（`assemble.rs` 原注释「本字段在装配面不被消费」），不补注入则 `subscription_allowed` 恒真、T6 关闭面只在单测成立 |
| 4 | 新增文案：未采样正文、快照正文 | 计划只规定「固定文本」「快照文本」，未给字面量 |
| 5 | 映射函数返回 `TrustedSystemReminder`（计划示例写 `SystemReminder`） | `InboxHandle::push_system_reminder` 收 trusted 类型（`peri-acp-types/src/session/inbox.rs:135`） |
| 6 | 线路夹具收尾按生产顺序（停订阅 task → 清 `handle.peer` → 关 service）并**断言 server task 收敛** | 实测：只要订阅的取消通知送不出去（连接先关）或 `Peer` 克隆未释放，服务端在途 `listen` 不会取消 ⇒ 1s 窗口内不收敛。生产 `remove_server`/`set_disabled` 正是先停订阅 task 再关连接（`lifecycle.rs:121,239`），`shutdown()` 先清 `peer`（`:342-352`） |
| 7 | 线路测试拆为两个文件（父 + 子模块） | 单文件超 1000 行（STD-SIZE-001） |
| 8 | `assembly_test_baseline.rs` 的 Permission/AskUser 位置期望 13/12 → 12/11 | GitWatch 槽位删除导致前移 1（计划 §3.E 未列位置断言） |

### 8.5 未验证项 / 风险

1. **非 workspace 工具的 git 改动延迟可见**（D-1 固有，计划 §6 风险 2）：登记为已知语义收窄，未做端到端反例。
2. **池级 `shutdown()` 与活跃订阅的收敛顺序（既有属性，本变更使其可达）**：`McpClientPool::shutdown()` 的顺序是「先关全部 client service → 再 `close_builtin_tasks()`」，而 `begin_shutdown()` 不停订阅后台 task（`lifecycle.rs:282-300,338-421`）；当 `workspace` 实例有活跃 `subscriptions/listen` 时，取消通知在 service 关闭后已无法送达 ⇒ 服务端在途 `listen` 请求不取消 ⇒ `supervisor.close(BUILTIN_CONVERGE_TIMEOUT)` 走 **abort** 分支（有界、同进程 task、无孤儿、无数据丢失，但非正常路径且会记一条 warn）。逐实例关闭路径（`remove_server` / `set_disabled`）不受影响。线路夹具已按「先停订阅 task」的生产顺序断言严格收敛（T6/T7b）；**全池关闭 + 活跃订阅**的组合未做端到端用例，建议后续把 `shutdown()` 也改为先停订阅 task（`stop_background(McpTaskKey::Subscription)`）再关 service。
3. **既有客户端代码的订阅 id 读取**：`mcp/client/subscription.rs` 通用分支用 `notif.params.meta`（入站恒 `None`）⇒ 通用提醒 metadata 里的 `subscription_id` 为空串。非本次变更引入，未改（git_watch 路径不依赖它）。
4. **e2e 未运行**：`workspace-slow-git` / `workspace-no-git` 未执行（需 TUI 环境），只按新触发口径更新了注释。
