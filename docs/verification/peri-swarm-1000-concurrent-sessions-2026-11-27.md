# Peri Swarm 1000 并发空 session：PostgreSQL 连接耗尽（修复记录）

- 状态：**修复已实现，并完成 1000 并发运行时复测**（1000/1000 成功，连接耗尽未复现）；复测证据与局限见 §5、§6。
- 日期：2026-11-27
- 基线：`3eecbdede`（`origin/main`）；修复前运行证据来自相关文件完全相同的 `1bbb7dec6` 构建。
- 问题单：[P1：Swarm 每个会话独立打开会话存储](../../spec/issues/2026-11-27-p1-swarm-shared-session-pool.md)
- 范围：`peri-swarm` 的 actor host 装配、部署启动，以及 `peri-resources` 远程会话池容量。

## 1. 环境与测法

| 项 | 值 |
| --- | --- |
| 部署 | `deploy/docker-compose.yml`：2 个 `peri-swarm` 副本 + `postgres:16-alpine` + `rivet-engine` + `nginx` |
| 入口 | `127.0.0.1:8080` |
| 数据库 | PostgreSQL 默认 `max_connections=100`、`reserved_connections=3` |
| 压测脚本 | `python3 /tmp/peri-loadtest/run_test.py --count 1000 --concurrency 50 --tag 20261127-2345` |
| 原始产物 | `/tmp/peri-loadtest/run-20261127-2345/`：`summary.json`、`requests.jsonl`、`samples.jsonl`、`logs/*.log`、`meta_before.json`、`probe_summary.json` |
| 空闲观测 | `/tmp/peri-loadtest/idle-watch.log` |

## 2. 修复前结果

| 指标 | 值 |
| --- | --- |
| 成功 | 57（`201`） |
| 失败 | **943**（全部 `502 upstream_unavailable`，`operation=acp_child`） |
| 总耗时 | 400.99s |
| 网关日志 | 943 条 `swarm ACP host assembly failed … error=无法打开远程会话存储: store operation timed out (NotApplied)`，每条约 20s |
| PostgreSQL 日志 | **12,613 次 `FATAL: sorry, too many clients already`** |
| 结束时连接 | 100/100（`peri_session` idle 74、`peri_swarm_state` active 1 + idle 24 等） |
| 空闲回收 | `session_conns` 74 → 68 → 64 → 16 → 12（约 2 分钟），与 sqlx 默认 `idle_timeout=10min` 一致 |
| 库内会话 | `swarm_sessions`=1013（idle 68 / **creating 943** / running 2） |

## 3. 根因

1. 每个 ACP child 在 `peri-swarm/src/actor/runtime.rs` 的装配路径上独立调用
   `Resources::open_deployment_in_remote_environment`，bootstrap 已打开的部署级
   `Resources` 在 `into_parts()` 后被丢弃。
2. 每份 facade 建立独立 sqlx 池（`max_connections(2)`、`min_connections(0)`），
   `connect_with` 在创建池时至少建立一条连接；连接归还后进入 idle 队列，默认
   10 分钟才回收。因此每个空 session 在存活期至少占 1 条连接。
3. 2 个副本 × 每个 child 1 条连接超过 PostgreSQL 100 槽位后，新 child 在 20s
   预算内反复收到 `too many clients already`，池超时被映射为 `Timeout`，网关输出
   502 `upstream_unavailable`。
4. 创建中断恢复路径最多再打开 2 次，会放大连接压力（本次压测未触发）。

## 4. 修复内容

### 4.1 每进程一份会话存储门面/池

- `ActorDeployment::session_resources()`：部署级业务句柄，默认 `None`。
- `StaticDeployment` / `AcpSwarmService` 各新增 `new_with_session_resources(...)`；
  既有 `new(...)` 委托为 `None`，测试与独立 actor 的构造点不变。
- `SwarmActor::create` 用 `deployment.session_resources()` 构造 service；create 与
  reattach 两条路径统一经 `AcpSwarmService::child_config(...)` 传播句柄。
- `runtime::resolve_session_resources`：
  1. 先校验 `machine_id` / `workspace_root` 必须同时存在或同时缺失（半配置仍
     fail-closed，即使注入了句柄）；
  2. 注入句柄 ⇒ 直接 clone，**不打开任何存储**，关闭权为 `None`；
  3. 未注入 ⇒ 原 per-child 打开路径，`into_parts()` 返回业务句柄 + 关闭权。
- `HostAssemblyInput.session_store_shutdown` 在注入路径为 `None`：第一个 child
  退出不会关闭全进程共享池。部署在 `drain` 后消费唯一关闭权。
- 本地 SQLite 仍 per-child；未注入行为不变。

### 4.2 池容量策略

- 新增 `SessionStorePoolPolicy`：`Default = max_connections(2)`（精确保持
  TUI/CLI/stdio 现有行为），`new(n)` 拒绝 0，Swarm 传 `new(32)`。
- 传递链：
  `Resources::open_deployment_in_remote_environment_with_pool` →
  `sessions::open_remote_in_environment` → `remote::composition` →
  `RemoteSessionData::open_with_machine_and_policy` → `RemoteConnectionFactory`
  → `generation::ConnectionFactory::connect` → `connection::connect` →
  `native_postgres::connect`。
- 重连来自同一工厂，沿用打开时的策略，不会回到默认 2。
- 连接预算（两副本、默认 `max_connections=100`）：
  `2 × (32 + 8) + 3 = 83 ≤ 100`；8 是状态库池上限
  （`peri-swarm/src/store/postgres/mod.rs`），3 是 PostgreSQL 保留槽。32 是上限
  不是预留，`min_connections(0)` 允许空闲池回落到 0。
- `REQUEST_BUDGET`（20s）、`acquire_timeout` 与错误码映射均未改动。

## 5. 修复后复测

镜像由修复分支 `fix/swarm-session-store-pool` 构建为 `peri-swarm:local`
（`sha256:2adb9281dc49333e02ab0e15087872c1ce4f8da46c3d2f5e140f4eb301e6226d`），
重启 postgres 与两个网关副本后建立干净基线。压测命令：

```bash
python3 /tmp/peri-loadtest/run_test.py --count 1000 --concurrency 50 --tag 20261128-0039
```

原始产物：`/tmp/peri-loadtest/run-20261128-0039/`（`summary.json`、`requests.jsonl`、
`samples.jsonl`、`logs/*.log`、`meta_before.json`、`probe_summary.json`）。

| 指标 | 值 |
| --- | --- |
| 成功 / 失败 | **1000（201）/ 0** |
| 总耗时 / 吞吐 | **7.08s / 141.2 次/秒**（脚本 latency 字段因时钟 bug 为负，未采用） |
| PostgreSQL `too many clients already` | **0**（测试窗口内 `docker compose logs postgres` 检索） |
| 网关 `ACP host assembly failed` / `store operation timed out` | **0 / 0** |
| `retired before use` / `session resources are closed` | **0 / 0** |
| `peri_session` 连接（测试结束时） | 网关-1 **32** + 网关-2 **32** = **64**（池上限）；另有主机侧 `dbx` 11 条 |
| `peri_swarm_state` 连接（测试结束时） | 网关-1 **8** + 网关-2 **8** = **16**（池上限）；另有 `dbx` 7 条 |
| 新 session 最终状态 | 1000 全部 `idle`（按 `lt-20261128-0039-%` 过滤）；旧 943 `creating` 未清理，不计入本次结果 |
| 容器峰值 | gateway-1 98.9% CPU / 727.7 MiB；gateway-2 93.3% / 716.4 MiB；postgres 92.3% / 305.5 MiB；rivet 135.4% / 865.7 MiB；nginx 2.9% / 4.3 MiB |
| 磁盘增量 | workspace 1,154 → 2,154（+1,000 目录）；postgres 数据目录 84,716 → 126,340 KB（+40.6 MiB）；rivet 150,692 → 164,532 KB（+13.5 MiB） |
| OOM / 重启 | 无（两副本 `OOMKilled=false`、`RestartCount=0`） |

连接拆分说明（测试结束时）：`pg_stat_activity` 的 105 行包含后台 worker；按客户端
地址拆分后，网关自身为会话库 32+32、状态库 8+8，分别为两副本的池上限。主机侧
`dbx`（数据库 GUI，进程名 `dbx`）额外持有 11 条会话库与 7 条状态库连接，压缩了
总余量；测试期间没有任何客户端收到 `too many clients already`，1000 个创建请求
全部成功。

## 6. 证据局限与残留风险

- 修复前运行时数字来自 `1bbb7dec6` 构建，修复后数字来自
  `fix/swarm-session-store-pool` 构建的镜像；两次部署的相关源码相同，但不是同一个
  镜像/进程。
- 测试环境有一个主机侧 `dbx` 数据库客户端，测试期间持有 11 条会话库 + 7 条状态库
  连接。网关自身仍在 32+32 / 8+8 的池上限内；`dbx` 压缩了总余量，因此本记录不能
  证明“无外部客户端时总连接数一定低于 83”，只能证明网关池上限与 1000 创建成功。
- 1000 个 session 都是空会话，测试窗口约 7 秒，没有执行 turn；本记录证明的是创建/
  连接路径，不是 1000 个同时执行 turn 的 Agent 吞吐，也不是长时内存。
- 网关峰值 727.7 MiB / 约 510 个 host（每副本）≈ 1.4 MiB/host，与修复前 57 个
  live session 折算的 4.24 MiB/会话不一致；两次样本的 host 状态与基线不同，不能
  据此外推 1000 个 live Agent 的内存。
- 1020 条 `命令注册冲突` / `configuration operation failed` WARN 每个 host 一条，
  修复前后都存在，属本地配置噪声，不是本修复引入；不计为失败。
- `requests.jsonl` 的 `elapsed` 与 `summary.json` 的 latency 字段因脚本时钟 bug 为
  负，未采用；只使用 wall/throughput 与请求状态。
- **连接代际失效粒度变化**：共享 facade 后，一次 20s 超时会退役全进程连接代际，
  其它 session 可能短暂收到 `Unavailable("... retired before use")`。本次复测 0 次
  该错误，但这是“没有发生超时”的观察，不是对故障隔离的验证。
- **进程退出顺序**：bootstrap 在 `drain` 后关闭部署级池；10s grace 后 `abort`
  是否已释放全部 child 未验证。若仍有 child 存活，它会拿到 `Unavailable` 而不是
  继续工作；进程退出即停是可接受语义，但未在新链路下复测。
- **1000 个 live Agent 的内存/吞吐**：本记录只覆盖空 session 创建；1000 个同时执行
  turn 的 Actor 吞吐、长时内存与 RSS 上界仍是独立缺口。
- 未引入 PgBouncer、未改 compose 或 PostgreSQL `max_connections`、未拆分
  advisory 写锁、未改 schema；503 映射、`/metrics` 与创建闸门不在本次范围。
