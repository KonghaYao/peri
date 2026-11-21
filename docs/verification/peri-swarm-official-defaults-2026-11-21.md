# Peri Swarm 官方默认值与小差异对齐验收

- 状态：代码与契约测试证据记录；未运行真实 Loom/Rivet/Bun engine、真实模型链或
  真实 SDK 客户端探针
- 日期：2026-11-21
- 基线：`71cf3a067`（分支 `feat/swarm-official-defaults`），本记录与实现同一提交
- 范围：只对齐以下 5 项官方默认值/小形状差异，不扩展 `requires_action`、批量
  events、官方 item 联合、turns 的 usage/subagent/queued/waiting、hosted 环境详情、
  时间字段改 Unix 秒或 `sessions.update` 未映射字段：
  1. `items.list` 默认 `order=desc`，显式 `order=asc` 仍可用；
  2. `prompt_cache_options: {}` 等于官方默认 `{"ttl":"30m"}`，`null` 清空，省略保持；
  3. SSE 查询接受并忽略官方 `include_obfuscation`；
  4. 会话投影补官方默认字段 `required_actions=[]`、`vault_ids=[]`、`usage=null`、
     `error=null`；
  5. `environment.type=none` 只输出 `{"type":"none"}`，hosted 的 `id` 不变。
- 参考源码（只读，未修改）：
  `/Users/mino/code/peri/.claude/dogfood/node_modules/openai`（`openai@7.60.0`）
- 历史证据：[P1-6 会话资源验收](peri-swarm-session-resources-2026-11-21.md) 保留原样，
  本记录不覆盖其结论。

## 1. 官方依据

| 事实 | 源码位置 |
| --- | --- |
| `ItemListParams.order` 默认 `desc`（最新优先） | `resources/beta/agents/sessions/items.d.ts` `ItemListParams`（`Defaults to \`desc\``） |
| `EventStreamParams.include_obfuscation` 是 snake_case 查询参数，默认 `true` | `resources/beta/agents/sessions/events.d.ts` `EventStreamParams` |
| create/update 的 `PromptCacheOptions.ttl` 可省略，省略时默认 30 分钟 | `resources/beta/agents/sessions/sessions.d.ts` `SessionCreateParams.PromptCacheOptions`、`SessionUpdateParams.PromptCacheOptions`（`Defaults to 30 minutes`） |
| `AgentSession` 必需字段含 `required_actions`、`vault_ids`、`usage`、`error` | `resources/beta/agents/agents.d.ts` `AgentSession` |
| `EnvironmentResourceNone` 只有 `type: 'none'`，没有 `id` | `resources/beta/agents/agents.d.ts` `Environment.EnvironmentResourceNone` |

## 2. 行为落地

| 项 | 实现 |
| --- | --- |
| items 默认顺序 | `peri-swarm/src/http/request.rs` 的 `resolve_items_window` 在省略 `order` 时返回 `ItemOrder::Desc`；显式 `asc`/`desc` 仍由 `ItemOrder::parse` 校验。`after`/`afterSeq` 仍按所选顺序排他。 |
| prompt cache 默认 TTL | `resolve_prompt_cache_options` 对无 `ttl` 的对象返回 `Some(PromptCacheOptions::new(PromptCacheTtl::ThirtyMinutes))`；`null` 仍返回 `None`，由 create/update 的 `UpdateField` 映射为清空。`metadata: {}` 仍是空/清空。 |
| SSE `include_obfuscation` | `EventsQuery` 增加 `#[serde(default)] pub include_obfuscation: Option<bool>`；`handlers::stream_events` 的查询解析接受该参数，后续游标/订阅逻辑不读取它。网关不生成混淆填充，事件内容与不带参数一致。 |
| 会话默认字段 | `SessionView` 增加 `required_actions: Vec<Value>`、`vault_ids: Vec<String>`、`usage: Option<Value>`、`error: Option<String>`；`required_actions`/`vault_ids` 显式使用官方 snake_case 名，四个字段都有 `#[serde(default)]`。`project_session` 固定填 `[]`/`[]`/`null`/`null`，不伪造用量、审批或错误。 |
| environment none | `EnvironmentRef::none()` 返回 `id: None`；`id` 加 `#[serde(default, skip_serializing_if = "Option::is_none")]`。`project_session` 把 `kind=None` 的旧行归一化为 `id=None`，旧持久数据不再泄露 `id`。hosted 仍输出 `id=logicalEnvId`。 |

重复 create 的身份比较在解析阶段完成：`{}` 先解析成 `Set(30m)`，再与行内值比较；
`null` 仍是 `Clear`，省略仍是 `Keep`。因此 `{}` 对无值会话是冲突、对 30m 会话是
相同值，`null` 对无值会话匹配、对 30m 会话冲突。

## 3. 契约测试

| 行为 | 测试 |
| --- | --- |
| items 默认 `desc` | `items_pagination_test::default_items_order_is_desc` |
| 显式 `asc` 与游标推进 | `items_pagination_test::explicit_ascending_pages_cover_every_item_once`，以及同一文件内改为显式 `order=asc` 的分页/行预算/`after=<itemId>` 用例 |
| `include_obfuscation` 不再 400 且不改变流 | `streaming_test::include_obfuscation_is_accepted_and_ignored`（`false` 与 `true` 都对比无参数回放帧） |
| `prompt_cache_options: {}` 默认 30m | `session_resources_test::prompt_cache_options_empty_object_uses_the_official_default_ttl` |
| attach 身份比较 | `session_resources_test::create_attach_distinguishes_omitted_and_explicit_identity_fields`（`{}`=30m、`null` 清空、省略保持） |
| 会话默认字段 | `session_resources_test::create_and_retrieve_expose_empty_official_default_fields` |
| `none` 环境形状 | 同上测试、`http_contract_test::create_returns_the_documented_shape_and_sdk_aliases`、`openai_hosted_test::a_none_environment_never_calls_the_bun_bridge` |
| 旧 none 行归一化 | `service::tests::project_session_normalizes_legacy_none_environment_and_official_defaults` |

### 补充：`sessions.update` 的 `{}` 与无 body 同为 no-op

官方 SDK 默认路径 `sessions.update(id)` 会发送 JSON `{}`（`application/json`）。
此前的 `parse_update_body` 只把空字节流与 `null` 视为无 body，`{}` 会继续进入
`update_session`：即使没有字段变化也会写 `last_activity_at`，有未关闭 turn 时还会
返回 `409 turn_in_progress`。这与官方默认调用形状不符。

现在 `parse_update_body` 在 `null` 判断旁把 JSON 空对象也返回 `Ok(None)`，与无 body
走 handler 的同一个 early return：只读当前投影，不执行在途/恢复准入，不写任何行。
`{"metadata":{}}` 等非空对象不受影响，仍按现有三态处理。

测试：

- `session_resources_test::update_accepts_an_absent_or_null_body_as_a_no_op`
  的循环新增 `"JSON empty object"`，与无 body、`null` 一样断言 stored row（含
  `last_activity_at`）不变；
- `session_resources_test::empty_object_update_is_a_noop_while_a_turn_is_open`
  在 `TurnPolicy::Manual` 留下未关闭 turn 后，对 `{}`、无 body、`null` 三种形状均
  断言 `200`、`status=in_progress`，且 `last_activity_at` 与行状态不被写入。

全量结果：**332 passed, 0 failed, 6 ignored**。

## 4. 执行命令与结果

所有命令在 `/Users/mino/code/peri/.worktrees/swarm-official-defaults` 执行：

```sh
export PATH="$HOME/.local/share/uv/python/cpython-3.12.14-macos-aarch64-none/bin:/Users/mino/.cargo/bin:$PATH"
export CARGO_HOME=/tmp/cargo-home-rsproxy

cargo test -p peri-swarm
cargo clippy -p peri-swarm --all-targets -- -D warnings
cargo fmt -p peri-swarm -- --check
MISE_DISABLE_TOOLS=rust npx -y lefthook@1.13.6 run pre-commit
```

结果：

| 命令 | 结果 |
| --- | --- |
| `cargo test -p peri-swarm` | **332 passed, 0 failed, 6 ignored** |
| `cargo clippy -p peri-swarm --all-targets -- -D warnings` | 通过（0 error） |
| `cargo fmt -p peri-swarm -- --check` | 通过 |
| `lefthook run pre-commit`（npx 1.13.6，`MISE_DISABLE_TOOLS=rust`） | **全部 6 个命令通过**：`check`、`clippy`、`feature-catalog`、`fmt`、`layer-imports`、`tui-components` |

分项计数：

| 目标 | passed | failed | ignored |
| --- | --- | --- | --- |
| lib 单测 | 237 | 0 | 0 |
| `actor_runtime_test` | 16 | 0 | 1 |
| `http_contract_test` | 25 | 0 | 0 |
| `items_pagination_test` | 13 | 0 | 0 |
| `loom_store_test` | 0 | 0 | 3 |
| `mcp_injection_test` | 4 | 0 | 0 |
| `openai_hosted_test` | 7 | 0 | 0 |
| `rivet_actor_test` | 0 | 0 | 2 |
| `session_resources_test` | 15 | 0 | 0 |
| `streaming_test` | 15 | 0 | 0 |

已知既有 flake：`openai_hosted_test::an_anonymous_hosted_session_reserves_a_bun_workspace`
在默认并行下偶发失败、串行通过；本轮全量运行未复现，不作为本次回归。

### lefthook

`MISE_DISABLE_TOOLS=rust npx -y lefthook@1.13.6 run pre-commit` 对最终暂存内容通过
全部 6 个命令。首次运行因本环境无法解析 `static.crates.io` / `static.rust-lang.org`，
`cargo-rmcp-patched.sh` 需要的本地 patch 缓存不在该 worktree 的 `target/` 中；从主
worktree 复制同一 SHA 的构建缓存（不进入提交）后重跑通过。另修正了
`feature-catalog` 对 F-SWARM-003 可见行宽度的 104 字符告警。`git commit` 若报
`Can't find lefthook in PATH`，按仓库约定视为环境问题，不以 `--no-verify` 绕过。

## 5. 未验证限制

- 未启动真实 Loom/Rivet/Bun engine，也未运行真实模型链；`prompt_cache_options` 的
  TTL 只验证持久化与回显，不证明模型链消费该 TTL。
- 未用真实 `openai@7.60.0` SDK 进程执行探针；本轮依据是其 TypeScript 类型与注释。
- `include_obfuscation` 只验证“接受且不改变帧”，未生成混淆填充，因为网关本来就不
  实现该能力。
- 会话默认字段只是形状对齐：`usage`/`error` 不反映真实 token 或失败详情；SSE 官方
  `agent.session.*` 帧中的 `usage`/`error` 语义未在本轮改变。
- 旧持久行的 `none` 环境只在投影层归一化，未迁移数据库中的 `environment` JSON 列。
- hosted 环境的 `files`/`network`/`packages` 与顶层 `sandbox.state` 实时状态仍是
  已知差异，本轮未实现。
