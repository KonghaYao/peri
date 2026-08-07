# ACP Hub 设计文档

> 状态: 设计完成 | 日期: 2026-07-17

## 概述

ACP Hub 是一个 ACP (Agent Communication Protocol) session 分流器。它作为主进程暴露单一 stdio 接口，将不同 session 的请求路由到独立的 ACP 子进程中执行。每个 session 对应一个子进程，实现故障隔离、资源隔离和 per-session 配置。

## 需求背景

当前 Perihelion 的 ACP stdio 模式中，所有 session 共享同一进程内的 `SessionManager`。需要 per-session 独立进程来提供：

- **故障隔离**：一个 session 崩溃不影响其他 session
- **资源隔离**：每个 session 有独立的内存/CPU 限制
- **per-session 配置**：不同 session 可有不同的工作目录、环境变量、provider 配置

## 客户端模型

- 单个 IDE（如 Zed/VSCode）启动 ACP Hub 作为子进程
- IDE 通过 Hub 的单一 stdio 管理多个 session
- IDE 感知不到多进程的存在，看到一个统一的 ACP server

## 架构概览

```
┌──────────────────────────────────────────────────────────┐
│  IDE (Zed / VSCode / ...)                                │
│  spawn("acp-hub", ["--", "peri", "acp"])                 │
└────────────────────┬─────────────────────────────────────┘
                     │ stdin/stdout (ACP JSON-RPC)
                     ▼
┌──────────────────────────────────────────────────────────┐
│  acp-hub (主进程)                                        │
│                                                          │
│  ┌──────────────┐  ┌──────────────────────────────────┐ │
│  │ 全局处理器    │  │  SessionRouter                   │ │
│  │ initialize   │  │  session_id -> ChildProcess       │ │
│  │ session/list │  │                                  │ │
│  │ commands/... │  │  spawn / kill / health           │ │
│  └──────────────┘  └────────┬─────────────────────────┘ │
│                              │                           │
└──────────────────────────────┼───────────────────────────┘
                               │ stdin/stdout pipe x N
              ┌────────────────┼──────────────────┐
              ▼                ▼                   ▼
       ┌──────────┐    ┌──────────┐        ┌──────────┐
       │ peri acp │    │ peri acp │        │ peri acp │
       │ session A│    │ session B│   ...  │ session N│
       └──────────┘    └──────────┘        └──────────┘
```

**核心设计决策**：

1. Hub 是独立二进制 `acp-hub`，不在现有 crate 里加代码
2. 子进程启动命令由 IDE 传入（非硬编码）：`acp-hub -- <child-command> [args...]`
3. 子进程是完整 ACP server，Hub 对每个子进程先发 `initialize`，再转发 `session/new`
4. Hub 只依赖 `serde_json` 做 JSON 解析，不依赖 `agent-client-protocol` SDK，保持协议版本无关

## 消息流

以 session/new + prompt 为例：

```
IDE                    acp-hub                    子进程 (peri acp)
 │                        │                            │
 │-- initialize -------->│                            │
 │<- capabilities ------│                            │
 │                        │                            │
 │-- session/new ------->│                            │
 │                        │-- spawn ------------------>│
 │                        │-- initialize ------------->│
 │                        │<- capabilities ------------│
 │                        │-- session/new ------------>│
 │                        │<- session_id: "abc" -------│
 │<- session_id: "abc" --│                            │
 │                        │                            │
 │-- prompt(sid:"abc")-->│                            │
 │                        │-- prompt(sid:"abc")------->│
 │                        │<- text chunks -------------│
 │<- text chunks --------│                            │
 │                        │                            │
 │-- session/close ----->│                            │
 │                        │-- session/close ---------->│
 │                        │-- kill ------------------->│
 │<- OK -----------------│                            │
```

---

## 进程管理与生命周期

### SessionRouter 核心结构

```
session_id -> ChildHandle
               ├── process: Child (tokio::process::Child)
               ├── stdin:  ChildStdin  (BufWriter)
               ├── stdout: BufReader<ChildStdout>
               ├── req_id: AtomicI64  (请求 ID 自增)
               ├── pending: HashMap<RequestId, oneshot::Sender>
               └── created_at: Instant
```

### 生命周期状态机

```
                   session/new
  [不存在] --------------------------> [启动中]
                                          |
                                   initialize 完成
                                          |
                                          v
                        session/close   [就绪]
                      +---------------     |
                      v                    | crash/异常
                   [关闭中]                 |
                      |                    v
                      v                 [已崩溃]
                   [已销毁]                 |
                                      通知 IDE
                                          |
                                      清理映射
                                          v
                                      [不存在]
```

### 创建流程 (session/new)

1. IDE 发 `session/new` -> Hub 解析出 `cwd`
2. Hub 调用 `Command::new(args[0]).args(&args[1..]).cwd(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).kill_on_drop(true).spawn()`
3. Hub 向子进程发 `initialize`，等待响应
4. Hub 向子进程发 `session/new`（透传 IDE 参数），等待 `session_id`
5. 注册映射 `session_id -> ChildHandle`，转发 `session_id` 给 IDE

### 销毁流程 (session/close)

1. Hub 向子进程发 `session/close`
2. 等待子进程正常退出（timeout 5s）
3. 超时则 `kill()` 强杀
4. 清理映射，`ChildHandle` 析构（`kill_on_drop(true)` 兜底）
5. 返回 OK 给 IDE

### 崩溃处理

1. 子进程 stdout 关闭或 `wait()` 返回非零 -> 判定为 crash
2. Hub 向 IDE 发 `session/update` 通知：`{error: "child process crashed", exit_code: 1}`
3. 清理映射

### 并发模型

一个 `tokio::select!` 主循环：

```
loop {
    select! {
        msg = hub_stdin.recv()         -> 处理 IDE 消息
        (sid, msg) = child_rx.recv()   -> 转发子进程响应给 IDE
        sid = crash_rx.recv()          -> 处理子进程崩溃
        _ = shutdown_rx.recv()         -> Hub 自身优雅退出
    }
}
```

- 每个子进程有一个后台 task 持续读其 stdout，解析 JSON-RPC 行
- 用 `mpsc::unbounded_channel` 汇聚子进程响应到主循环

### 配置注入

- `cwd`：从 `session/new` 的 `params.cwd` 提取
- 环境变量：Hub 自身环境透传，`session/new` 的 `params.env` 可额外追加
- 子进程自己读各自的配置文件（如 peri 读 `~/.peri/settings.json`）

---

## 消息路由与转发

### 路由决策表

| 消息类型 | 判断依据 | 处理方式 |
|---------|---------|---------|
| `initialize` | `method == "initialize"` | Hub 自响应 |
| `session/new` | `method == "session/new"` | Hub 自处理，spawn 子进程 |
| `session/close` | `method == "session/close"` | Hub 自处理，杀子进程 |
| `session/list` | `method == "session/list"` | Hub 自响应，聚合子进程状态 |
| `commands/list` | `method == "commands/list"` | Hub 自响应（静态列表） |
| session 请求 | `params.session_id` 存在 | 转发到子进程 |
| session 通知 | `params.session_id` 存在 | 转发到子进程 |
| 未知 | 都不匹配 | 返回 error `MethodNotFound` |

### 请求转发（ID 映射）

```
IDE                     Hub                    子进程
 │-- req(id:1, prompt)-->│                       │
 │                        │-- req(id:7, prompt)-->│   <- Hub 生成新 id
 │                        │<- resp(id:7, chunks)--│
 │<- resp(id:1, chunks)--│                       <- Hub 映射回原 id
```

- Hub 维护 `ide_req_id -> child_req_id` 映射
- 响应 `result` 内容原样透传，不做修改

### 通知转发

通知无 `id` 字段，直接透传，无需 ID 映射。

### 全局消息自处理

- **`initialize`**：返回固定能力集 + `capabilities.experimental` 中标记 `"acp-hub": true`
- **`session/list`**：返回 `[{session_id, cwd, created_at, status: "ready"|"crashed"}, ...]`
- **`commands/list`**：返回静态通用命令列表（`/clear`、`/compact`、`/model` 等），不做子进程命令聚合

### 边界情况

| 场景 | 处理 |
|------|------|
| 收到未知 session_id 的请求 | 返回 error `{code: -32000, message: "session not found"}` |
| 子进程响应超时 | timeout 后返回 error，连续 3 次超时视为 crash |
| 子进程主动发 `session/new` | 忽略或返回 error |
| 子进程启动超时 | session/new 阶段 10s 超时，超时返回 error，kill 子进程 |
| IDE 发不含 `session_id` 的非全局消息 | 返回 error `InvalidParams` |

---

## 错误处理

### 错误码

| 类别 | 错误 | Code |
|------|------|------|
| JSON-RPC | ParseError | -32700 |
| JSON-RPC | InvalidRequest | -32600 |
| JSON-RPC | MethodNotFound | -32601 |
| JSON-RPC | InvalidParams | -32602 |
| JSON-RPC | InternalError | -32603 |
| 业务 | SessionNotFound | -32000 |
| 业务 | SessionCrashed | -32001 |
| 业务 | SpawnFailed | -32002 |
| 业务 | ChildTimeout | -32003 |
| 业务 | ChildExited | -32004 |

### 优雅退出

收到 SIGTERM/SIGINT 时：

1. 通知所有子进程 `session/close`（并发，各自 3s 超时）
2. 超时的子进程直接 `kill()`
3. `wait()` 所有子进程回收僵尸
4. Hub 自身 `exit(0)`

---

## 可观测性

### 日志

- 用 `tracing` + `tracing-subscriber`，输出到 **stderr**
- 默认 JSON 格式，`--pretty` 切换为人类可读
- 关键日志点：spawn / exit / crash / 路由决策
- 默认 `info`，`RUST_LOG=acp_hub=debug` 开启详细路由日志

### 就绪信号

Hub 启动后在 stderr 输出：`[acp-hub] ready, pid=12345, child_cmd="peri acp"`

---

## 项目结构与 CLI

### Crate 规划

```
perihelion/
└── crates/
    └── acp-hub/                  <- 新增
        ├── Cargo.toml
        └── src/
            ├── main.rs           <- 入口 + CLI 解析
            ├── hub.rs            <- Hub 主循环
            ├── router.rs         <- SessionRouter (映射表 + 路由决策)
            ├── child.rs          <- ChildHandle (spawn/kill/health)
            ├── proxy.rs          <- 消息转发 (ID 映射 + 透传)
            ├── global.rs         <- 全局请求处理
            └── error.rs          <- 错误码定义
```

### 依赖

```toml
[dependencies]
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["json", "env-filter"] }
clap = { version = "4", features = ["derive"] }
anyhow = "1"
uuid = { version = "1", features = ["v4"] }
```

**不依赖 `agent-client-protocol` SDK**，只用 `serde_json::Value` 做路由和透传，保持协议版本无关。

未提供 `-- <child-command>` 时，Hub 启动报错退出。

### CLI 接口

```
acp-hub [OPTIONS] -- <child-command> [child-args...]

OPTIONS:
  --pretty            人类可读日志（默认 JSON）
  --log-level LEVEL   日志级别 [default: info]
  --spawn-timeout SEC 子进程启动超时秒数 [default: 10]
  --child-timeout SEC 子进程请求超时秒数 [default: 300]

ARGS:
  <child-command>     子进程启动命令（-- 之后）
  [child-args...]     子进程参数

示例:
  acp-hub -- peri acp
  acp-hub --pretty -- peri acp --model claude-sonnet-4-20250514
  acp-hub -- my-custom-agent --port 8080
```

### stdin/stdout 约定

- **Hub stdout** -> ACP JSON-RPC 消息（每行一个 JSON）
- **Hub stderr** -> 日志输出
- **子进程 stdin/stdout** -> 同上，完全标准 ACP

---

## 测试策略

### 测试层次

| 层 | 内容 | 工具 |
|----|------|------|
| 单元测试 | 路由决策、ID 映射、错误码序列化 | `cargo test` |
| 集成测试 | Hub + 模拟子进程端到端 | `cargo test` |

### 模拟子进程 (test-child)

极简 ACP echo 程序：
- `initialize` -> 返回固定 capabilities
- `session/new` -> 返回 `{session_id: "test-001"}`
- `prompt` -> 逐行返回 `session/update` 通知
- `session/close` -> 返回 OK 然后 `exit(0)`
- 支持 `--crash-after=N`：处理 N 条消息后 `exit(1)` 模拟崩溃

### P0 测试用例

| 测试 | 场景 |
|------|------|
| `test_initialize` | Hub 收到 initialize -> 返回能力声明 |
| `test_session_new_spawns_child` | session/new -> spawn 子进程 -> 收到 session_id |
| `test_session_new_spawn_fail` | 子进程命令不存在 -> 返回 SpawnFailed |
| `test_prompt_forward` | prompt -> 转发子进程 -> 流式响应回 IDE |
| `test_prompt_unknown_session` | 发给不存在的 session_id -> SessionNotFound |
| `test_child_crash_detection` | 子进程 exit(1) -> Hub 通知 IDE + 清理映射 |
| `test_session_close_kills_child` | session/close -> 子进程退出 -> 映射清理 |
| `test_session_list` | session/list -> 返回活跃 session 列表 |
| `test_graceful_shutdown` | Hub 收到 SIGTERM -> 通知所有子进程 -> 退出 |
| `test_id_mapping` | 并发 3 个请求到同一 session -> ID 映射不串号 |

### MVP 范围外

- 子进程心跳/健康检查
- 进程池预热
- 请求优先级队列
- 协议版本协商/降级
- session 迁移
- 性能 benchmark
