# F6 设计：acp-machine（transport / buffer / auth / child 改造 + hub 主循环）

> 状态：设计稿（对应 Feature F6）
> 日期：2026-08-07
> 权威来源：`docs/architecture.md`（v2.4）§3.1/§3.3/§4.5/§4.5.1/§4.7/§4.8/§7.1/§7.5/§7.6/§8.3/§8.5/§9.2/§9.6/§16/§17.1/§17.2；对照 `docs/plans/f3-persist.md` §6（server 侧水位语义）
> 约束：**忠于架构文档**。文档未指明处的命名/参数选择均标注「【决策】」并给出依据；复用 `acp-hub-proto` 已有类型（`machine.rs`/`conn.rs`/`hmac.rs`/`frame.rs`/`whitelist.rs`/`version.rs`/`protocol.rs`），不重复实现密码原语与帧模型。
> 范围：machine crate 的 `transport`/`buffer`/`auth`/`child`/`hub` 五个模块 + `main.rs`（CLI/装配）。**不修改** `lib.rs`、`Cargo.toml`（新依赖见 §12，由主管统一处理）、proto crate、server crate 与其他 feature 模块文件。
> 模块归属：`global.rs`/`router.rs` 为废弃模块（内容替换为占位壳，`lib.rs` 声明不动，测试文件 `git rm`）。

---

## 1. 目标与范围

### 1.1 目标

将 machine crate 从旧单机 stdio 桥接器改造为 **machine daemon**：outbound ws 连 server、按 session 管理 ACP 进程树、透明转发 + 断线缓冲，完成 §4.5/§4.5.1/§8.3/§8.5 的全部 machine 侧职责。

### 1.2 边界声明

| 归属 | 内容 | 落点 |
|------|------|------|
| F6 内 | ws 连接/重连/心跳（transport）、断线缓冲 + 环形滑窗 + 水位（buffer）、machine 侧双向认证（auth）、进程管理改造 + 孤儿清理（child）、会话表 + seq/epoch + 转发调度 + 主事件循环（hub）、CLI/配置（main.rs） | machine crate |
| server（F3/F5） | `machine/hello` 的幂等 fencing、`(epoch, last_seq)` 持久化与水位对齐、buffer_sync epoch 校验与补推消费、degraded 判定、offline 判定、`pending_close` 补发 kill、恢复对账（§7.5/§7.6/§8.3） | server crate |
| server（F5） | 下行 ACP 指令（prompt/cancel/resolve）的线帧形态（**未决冲突 1**）、滑窗重发请求帧（**未决冲突 2**） | server crate |
| F1（proto） | machine 帧结构、HMAC 原语（已定稿，本设计只消费） | proto crate |

### 1.3 未决冲突（需主管裁决，本设计不越权实现）

1. **下行 ACP 转发帧缺口**：架构 §4.3（`session/prompt` 等「转发到目标 machine」）与 §4.4（L1/L2 投递、machine 写 ACP stdin）要求下行 ACP 指令帧，但 M1 machine 帧集（§4.8 与 `whitelist.rs` machine 方向表）**只有** `machine/spawn`/`machine/kill`/`auth_response`。影响：F6 无法闭环「prompt → ACP」链路。本设计在 `child.rs` 保留 stdin 写入能力并在 `hub.rs` 预留 `handle_downlink` 接口（帧类型到位即可接入）；**建议**：新增 `machine/forward { command_id, session_id, frame }`（command_id 幂等，与 spawn/kill 同族），由 F5/F1 裁决。
2. **滑窗重发请求帧缺失**：§8.5「server 发现缺口时请求滑窗重发」无线帧（M1 白名单无）。machine 侧提供 `ring_snapshot(session_id)` 查询接口备用，重发触发帧待 F5 定。
3. **缺口计数上报字段缺失**：§3.3「无法提取 sessionId 的帧丢弃并记本地缺口计数（随 `machine/hello` 上报）」，但 `MachineHello` 无对应字段。M1 本地计数 + 结构化日志；若需线上报需扩展 proto（主管裁决）。
4. **buffer_sync 无 ack**：协议无 `buffer_sync_ack`。已【决策】以「同一 ws 连接帧序 + server 每 session 串行消费者」保证补推先于实时（§7.4），不新增帧；若 server 侧要求显式完成确认，需新增帧。

---

## 2. 模块结构总览

```
machine/src/
├── main.rs            # F6 重写：CLI（--server-url/--token-file/--data-dir/...）+ 配置 + tracing + 启动 hub
├── hub.rs             # F6 改造：MachineConfig、Sessions 会话表（进程 + seq/epoch + 发送队列）、
│                      #   spawn/kill 幂等处理、帧转发调度、补推协调、主事件循环（取代旧 stdio 循环）
├── child.rs           # F6 改造：AcpProcess（spawn 进程组 / kill 进程组 / stdin 写 / stdout 读 / wait 监控）
│                      #   旧 JSON-RPC pending/oneshot 请求-响应匹配删除（§9）
├── transport.rs       # F6 实现（占位→实体）：outbound ws、指数退避重连、心跳、发送队列、关闭码策略
├── buffer.rs          # F6 实现（占位→实体）：per-session 分桶缓冲（内存+磁盘两级）、分类丢弃、
│                      #   环形滑窗 500、水位文件（epoch/last_seq/pid）、启动清理
├── auth.rs            # F6 实现（占位→实体）：hello 构造（nonce 每次新生成）、auth_response HMAC 校验、
│                      #   握手超时、认证状态机
├── error.rs           # 保留（JSON-RPC 工具函数与新错误码），§9
├── global.rs          # 废弃：内容替换为占位壳（旧 initialize/session_list/commands_list 不再需要）
├── router.rs          # 废弃：内容替换为占位壳（旧 spawn+initialize 时序由 server 驱动）
├── router_test.rs / global_test.rs   # git rm（随废弃模块移除）
└── bin/test_child.rs  # 保留（假 ACP 进程，集成测试用）
```

测试沿用仓库规范：`*_test.rs` 同目录 `#[path]` 引入；`tempfile`/`serial_test` 已预填 dev-dependencies；重连/心跳用 `tokio::time::pause`（tokio 已开 `test-util`）。

**复用 proto 类型**（不再实现）：

- `machine::{MachineHello, MachineHeartbeat, MachineEvent, MachineBufferSync, BufferedFrame, MachineSpawn, MachineKill, MachineSpawnAck, MachineKillAck, MachineProcessExit}`；
- `conn::AuthResponse`、`conn::CLOSE_CONFIG_FATAL(4502)` 等关闭码；
- `hmac::{generate_challenge_nonce, derive_mac_key, mac_input, compute_mac, verify_mac, CHALLENGE_NONCE_LEN, SESSION_CONTEXT_LEN}`；
- `frame::{Frame, ProtoError}`（`Frame::parse` 为唯一入站解析入口，未知 tag → `Unsupported` 计数）；
- `whitelist::{m1_check, Role, Direction, M1Check}`（入站帧 M1 + 方向校验，防 server 被冒充时的畸形帧）；
- `version::PROTOCOL_VERSION`；`protocol::Defaults`（心跳 5s、缓冲 10MB/万条、单帧 1MB、滑窗 500 的默认值来源）。

---

## 3. 连接生命周期（§7.1/§8.3/§9.2）

```
                ┌────────────────────────────────────────────────┐
                │  transport::connect(url)                       │
                │  （失败 → 退避 1s→2s→4s…→60s 上限，重试）        │
                ▼                                                │
        ┌─ CONNECTED ── auth::handshake ──┬─ 成功 ──► READY      │
        │   (ws 建立)   hello(nonce 新)    │                      │
        │              auth_response 校验  │                     │
        │                                 └─ 失败/超时/4502 ─► STOP（审计日志，不再自动重连）
        ▼
   DISCONNECTED ──► 缓冲模式（child 帧 → buffer）
        │
        └── 重连（新 nonce、重新握手）──► READY ──► 补推 → 实时
```

1. **启动**：读配置（server_url/token 文件/data_dir）→ 清理上代残留（§8）→ 启动 hub 主循环。
2. **连接**：`transport` 建立 ws（URL 路径 `/machine`，【决策】§3.3，便于 server 侧路由；若 F5 采用不同路径，配置项可覆盖）→ 进入 `auth::handshake`。
3. **双向认证**（§9.2）：发 `machine/hello`（token + 本次连接新生成的 nonce + hostname + caps + `buffered`/`buffer_lost`/`stream_epochs`）→ 等 `auth_response`（超时【决策】10s）→ 校验 HMAC（§5）。**校验通过前不执行任何 spawn/kill**（spawn/kill 分发只在本设计认证状态为 `Authenticated` 后启用）。
4. **就绪 → 补推**：对每个有待补推帧的 session 发 `machine/buffer_sync`（§6），排空后恢复该 session 实时转发（§8.5 补推纪律）。
5. **运行**：收 spawn/kill（应答 spawn_ack/kill_ack）、上报 machine/event、process_exit、每 5s 发 heartbeat。
6. **断连**：ws 错误/关闭 → 缓冲模式（child 帧入 buffer，不丢失在预算内）→ 退避重连 → 重新握手 → 补推 → 实时。
7. **认证失败 / server 以 4502 关闭**（§4.7：配置性永久失败）：停止自动重连，记录错误与审计日志（§9.2 步骤 3 语义）。4500/4501 是 server→client（TUI）关闭码，machine 侧不适用【决策】；1011/1013/其他 → 退避重连。

**注意**：§4.6 的 `ready` 快照握手是 client（TUI）连接语义（`whitelist.rs` machine 方向表不含 `ready`/`keep_alive`/`pong`），machine 侧不实现；machine 心跳用 §4.5 的 `machine/heartbeat`。

---

## 4. 模块设计

### 4.1 child.rs —— AcpProcess（改造）

保留 `spawn_child` 骨架，改造为进程面（不含会话逻辑，session_id 仅为标签）：

```rust
pub struct AcpProcess {
    process: Mutex<Child>,
    stdin: Mutex<BufWriter<ChildStdin>>,
    session_id: String,
    pgid: i32,                 // 进程组 id（= 子进程 pid）
    state: Mutex<ProcessState>, // Running / Exited(Option<i32>)
}

/// stdout 读取任务产出：原始 ACP 帧行（dumb 透传，§3.3）
pub struct ChildEvent {
    pub session_id: String,
    pub frame: serde_json::Value,
}
```

- **spawn**：`Command::new(cmd[0]).args(&cmd[1..]).current_dir(cwd).envs(env)` + `.process_group(0)`（Unix，子进程自建进程组，pgid = 子进程 pid；macOS 支持）+ `.kill_on_drop(true)`（§7.5 语义，daemon 正常退出时兜底）+ `.stderr(Stdio::piped())` 独立读任务（stderr 仅日志，不上行，防阻塞）。
- **stdout 读取任务**：逐行读取（沿用 JSON-RPC 行协议）→ **sessionId 提取**（§3.3 最小协议面：原始 `{type,payload}` 与 JSON-RPC 包裹双格式，`payload.sessionId`/`params.sessionId`）→ 产出 `ChildEvent`；**无法提取 sessionId 的帧丢弃并记本地缺口计数**（冲突 3，本地计数 + 日志）。不再做 pending/id 匹配（旧逻辑删除，理由见 §9）。
- **kill（进程组 kill）**：`kill(grace)` → `SIGTERM(-pgid)` → 宽限 `grace`（缺省【决策】3s，server 可经 `machine/kill.grace` 覆盖）→ `SIGKILL(-pgid)`。已退出 → 立即成功（幂等）。实现用 `libc::kill(-pgid, sig)`（**新增依赖 `libc`**，§12）。
- **wait/监控**：stdout EOF 后 `process.wait()` → 状态迁移 `Exited(code)` → 经通道上报（hub 组装 `machine/process_exit`）。`kill_on_drop` 保留（Drop 时 kill 直接子进程；SIGKILL 场景下由进程组/水位清理兜底，§8）。
- **stdin 写**：保留 `write_line(&Value)`（写原样 JSON 行 + flush，字节级成功即 §4.4 L2 的 machine 侧语义）；`send_request`/`send_notification`/pending/oneshot 删除（响应匹配归 server 侧，§9）。

### 4.2 hub.rs —— MachineConfig + Sessions + 主循环（改造）

旧 stdio 主循环（IDE stdin 读/写）删除，替换为 machine daemon 主循环。同时承载旧 `SessionRouter` 的职责（会话表）。

```rust
pub struct MachineConfig {
    pub server_url: String,          // ws://host:port/machine
    pub token: String,               // 从 token 文件读入（0600），不落日志
    pub data_dir: PathBuf,           // 默认 ~/.local/share/acp-hub/machine/（0600）
    pub heartbeat_interval: Duration, // proto::Defaults::HEARTBEAT_INTERVAL (5s)
    pub reconnect_base: Duration,    // 1s（§7.1）
    pub reconnect_max: Duration,     // 60s（§7.1）
    pub auth_timeout: Duration,      // 10s【决策】
    pub buffer_limit_bytes: usize,   // proto::Defaults::BUFFER_LIMIT_BYTES (10MB)
    pub buffer_limit_frames: usize,  // proto::Defaults::BUFFER_LIMIT_FRAMES (万条)
    pub mem_buffer_bytes: usize,     // 5MB【决策】= 10MB/2（§8.5 内存+磁盘合计口径，§4.4.2）
    pub max_frame_bytes: usize,      // proto::Defaults::MAX_FRAME_BYTES (1MB)
    pub ring_capacity: usize,        // proto::Defaults::RING_BUFFER_CAPACITY (500)
    pub kill_grace: Duration,        // 3s【决策】
}

struct SessionEntry {
    acp: Option<AcpProcess>,         // None = 进程已退出但会话状态保留（供重建 epoch+1）
    stream: StreamState,             // { epoch, next_seq, last_sent_seq }
    send: SessionSendQueue,          // per-session 有序发送队列（缓冲帧 + 实时帧统一）
    buffered: bool,                  // 是否有待补推（hello.buffered）
}
```

**seq/epoch 分配**（§4.5.1，详见 §5）：

- session 新开：`epoch = 1`，`next_seq = 1`（首帧 seq=1，见 §5 依据），`last_sent_seq = 0`。
- 进程重建（同 session_id 再次 spawn）：`epoch = 水位记录的 epoch + 1`，`next_seq = 1`。
- 每帧上行：`seq = next_seq; next_seq += 1`。

**spawn/kill 幂等处理**（§4.5/§6.2）：

- `machine/spawn`：`Sessions` 查 `session_id`——已存在 → 不二次起进程，直接 `spawn_ack{ok:true}`（§4.5 幂等语义）；不存在 → env 白名单双端校验（§9.6，§4.5 本模块）→ `AcpProcess::spawn` → 成功 `spawn_ack{ok:true}` / 失败 `spawn_ack{ok:false, error: 脱敏原因}`。**不**在 machine 侧执行 initialize（§6.2 的 spawn→binding 时序由 server 驱动，machine 保持 dumb）。
- `machine/kill`：已存在 → `AcpProcess::kill(grace)` → `kill_ack{ok:true}`；不存在/已退出 → `kill_ack{ok:true}`（幂等，目标不存在视为已达成【决策】）。
- **认证通过前**收到 spawn/kill：丢弃 + 计数 + 告警日志（不执行，§9.2 步骤 3）。
- 下行 ACP 指令帧（未决冲突 1）：`handle_downlink(session_id, frame)` 接口——写 ACP stdin（`write_line`），写失败（进程已退出）→ 上报失败语义（§4.4 L2：ACP 子进程退出写失败 → 上报失败）。

**转发调度（补推纪律的 machine 侧实现）**：

- 每 session 一条有序流：child 帧 → `SessionSendQueue`（在线 = 实时模式直接发 `machine/event`；断线 = 帧进入 `buffer.rs` 的 pending 段并挂到发送队列）。
- **在线**：帧序列化后 > `max_frame_bytes` → 跳过 + gap 计数 + 告警（§8.5「超限直接跳过并记 gap」，**跳过而非截断**，架构为准）；否则入发送队列 → transport writer 写成功后 `last_sent_seq = seq`，并同步写入该 session 环形滑窗（§4.4.3）。
- **断线**：帧（同超限检查）→ `buffer::push`（分桶 + 预算 + 丢弃策略）。滑窗照常维护（常驻内存最后 500 条）。
- **重连就绪后**：对每个 `buffered` session，从 pending 首帧起分批组 `machine/buffer_sync`（§6）发完 → 清 `buffered` → 该 session 转实时。补推期间新帧追加 pending 尾部，随补推续发，保证序（§6.1）。
- 心跳：每 `heartbeat_interval` 发 `machine/heartbeat { load, alive_sessions }`；`load`【决策】= `min(100, alive × 20)` 粗略线性（§17.1 无精确语义）；`alive_sessions` = `Sessions` 中有 `AcpProcess` 的 session_id 列表。
- `process_exit`：child wait 完成 → 组装 `machine/process_exit { session_id, code }` 上报 → 水位更新（§5.3）→ 该 session 缓冲文件删除（§8.5）→ `acp = None`（会话条目保留，供重建 epoch+1；`hello.stream_epochs` 只含当前存活 session【决策】）。

**主循环**（取代旧 `run_hub`）：

```
select! {
    frame = ws_rx.recv()      => 入站帧分发（auth/spawn/kill/下行 forward 预留）
    evt = child_events.recv() => 转发调度（在线实时 / 断线缓冲）
    _ = heartbeat_tick        => 发 heartbeat
    state = transport.events  => 连接状态迁移（Connected/Authenticated/Disconnected/Stopped）
    _ = ctrl_c                => 优雅关闭：kill 所有 session → 关 ws → 退出
}
```

### 4.3 transport.rs —— outbound ws + 重连 + 心跳（占位 → 实体）

```rust
pub enum TransportEvent {
    Connected,              // ws 建立（auth 前）
    Authenticated,          // 认证通过（hub 才进入 READY/补推）
    Disconnected,           // 断线（hub 切缓冲模式）
    Stopped(StoppedReason), // 停止重连（AuthFailed / ConfigFatal4502 / Shutdown）
    Frame(Box<Frame>),      // 入站帧（解析后）
    AuthTimeout,
}
```

- **连接**：`tokio_tungstenite::connect_async`（tokio-tungstenite 已预填）；发送/接收拆双任务，共用 `mpsc` 发送队列（`mpsc::Sender<Frame>` + writer task），**单连接多路复用**由 `Frame` 枚举天然承载（§3.1/§4.2）。
- **指数退避重连**（§7.1）：`base=1s` 起、×2、上限 60s；连接成功且认证通过后重置为 base；不引入随机抖动【决策】（单机场景无惊群问题，忠于文档序列 1s→2s→4s…→60s）。
- **心跳**：心跳定时器在 hub（§4.2），transport 只负责帧收发——【决策】心跳帧构造在 hub，避免 transport 依赖会话表；transport 暴露 `is_connected()` 供 hub 决定实时/缓冲。
- **关闭码策略**（§4.7）：收到 4502（配置性失败）/ 认证失败（HMAC 校验失败，§9.2）→ `Stopped`；其余关闭码/网络错误 → `Disconnected` → 退避重连。
- **入站校验**：每帧 `Frame::parse`（未知 tag → 计数，不 panic）+ `whitelist::m1_check(tag, Role::Machine, Direction::Outbound)`（方向校验，防异常帧）。
- **断线检测**：读任务返回错误 / 写任务 send 失败 → `Disconnected`。

### 4.4 buffer.rs —— 分桶缓冲 + 滑窗 + 水位（占位 → 实体）

#### 4.4.1 断线缓冲（§8.3/§8.5）

```rust
pub struct SessionBuffer {
    pub session_id: String,
    mem: VecDeque<BufferedFrame>,     // 内存段（预算 mem_buffer_bytes 与 frames 上限的一半【决策】）
    disk: Option<BufWriter<File>>,    // 磁盘溢出段（append 日志，0600）
    mem_bytes: usize, disk_bytes: usize, frames: usize,
    dropped_event: u64, dropped_control: u64, dropped_oversize: u64, // §17.1 指标
}

pub struct Buffer { /* session_id → SessionBuffer，总预算核算 */ }
```

- **两级存储**：内存 VecDeque 优先；内存达 `mem_buffer_bytes`（5MB，§8.5「内存+磁盘合计 10MB」口径的内存半区【决策】）→ 新帧追加到磁盘 append 日志（`{data_dir}/buffer/{session_id}.buf`，0600，内容 = `BufferedFrame` 序列化 + u32 长度前缀；复用 proto 帧序列化，任务要求；无 CRC【决策】——崩溃即弃的临时溢出文件，不承担持久化恢复职责，§3.3「缓冲不跨重启保留」）。
- **预算与丢弃**（§8.5）：总预算 = 内存 + 磁盘合计 ≤ 10MB 且 ≤ 万条（任一超限触发）。丢弃优先级：
  1. **单帧超 1MB**（序列化后）：不入缓冲、不转发，跳过 + gap 计数 + 告警日志；
  2. 超预算：**事件类优先丢弃**（delta 语义，§8.5「delta 类帧优先丢弃、控制帧/终态帧最后丢弃」的 machine 侧可执行近似，见下）；仍超限 → 丢弃最旧帧（含控制类），控制丢弃计数；
  3. 分类规则【决策】——machine 是 dumb pipe（§3.3 禁止解析事件语义），用**信封结构性分类**而非语义解析：JSON-RPC 包裹（含 `"jsonrpc"` 键）且有 `"id"`（请求/响应）→ 控制类（最后丢弃）；无 `id` 的通知 / 原始 `{type,payload}` 帧 → 事件类（优先丢弃）。「终态帧最后丢弃」的完整语义由 server 侧 gap 呈现兜底（§8.5「不假装完整」）。
- **读取**：`drain()` 按序产出 pending 帧（首帧 seq 连续递增），供 buffer_sync 分批；`peek_first_seq()` 供 from_seq 计算。
- **清理**：session 结束（process_exit 上报后）/ kill 完成后删除缓冲文件与内存段（§8.5）；daemon 启动时删除整个 `{data_dir}/buffer/` 目录（§3.3 缓冲不跨重启）。
- **告警（§17.2 degraded 信号）**：丢弃发生即记结构化日志（session_id/bytes/frames/dropped 分类计数，§17.1 指标）；server 侧 `degraded` 由「该 session 补推出现 gap」链路驱动（machine 丢弃 → buffer_sync 缺口 → server gap → degraded），machine 侧不直接上报状态（冲突 3）。

#### 4.4.2 环形滑窗（§8.5）

```rust
pub struct RingBuffer { frames: VecDeque<BufferedFrame>, cap: usize }  // 默认 500
```

- 每 session 一个，常驻内存，**在线与断线均写入**（在线 = 已发送帧；断线 = 缓冲帧）——覆盖「server 崩溃前已收未落盘段」的兜底（server 发现缺口时请求重发，冲突 2，machine 侧仅提供 `ring_snapshot(session_id) -> Vec<BufferedFrame>` 查询接口备用）。
- 满则淘汰最旧（pop_front）。

#### 4.4.3 水位文件（§5.3）

`{data_dir}/watermark.json`（0600）：

```json
{ "sessions": { "<session_id>": { "epoch": 2, "last_seq": 137, "pgid": 4321 } } }
```

- **用途**：a) epoch 跨重启单调（重启/重建后 `epoch = 水位 epoch + 1`，防止与 server 持久化的旧 `(epoch, last_seq)` 混淆为同代际——§4.5.1 判定的正确性前提）；b) `pgid` 供启动时清理上代残留（§8）；c) `last_seq` 为诊断参考（非权威，权威在 server 侧 f3-persist 水位）。
- **更新时机**：epoch 变更时（spawn 新 session / 进程重建 / 进程退出）写盘（append 语义用临时文件 + rename【决策】）；last_seq 高频不落盘。
- **重启加载**：读水位 → `buffer/` 目录整体删除（buffer_lost: true 语义，§3.3）→ 对水位中记录的 pgid 执行 `kill(-pgid, SIGKILL)`（ESRCH 忽略，安全幂等）→ 重启后首次 spawn 某 session_id 时 `epoch = 水位 + 1`。

### 4.5 auth.rs —— machine 侧双向认证（占位 → 实体）

```rust
pub enum AuthState { PendingHello, AwaitingResponse, Authenticated, Failed(AuthFailure) }

pub struct AuthClient { token: String, /* 从配置注入，不落日志 */ }

/// 构造 hello：nonce 每次连接新生成（32B CSPRNG → base64，§9.2）
pub fn build_hello(&self, ctx: &HelloCtx) -> MachineHello;
/// 校验 auth_response（常量时间，§9.2 顾问3）：
///   key = derive_mac_key(token_bytes, "machine")
///   input = mac_input(nonce_bytes, context_bytes, PROTOCOL_VERSION, "machine")
///   verify_mac(key, input, response.hmac)   // session_context 来自应答（server 生成，§9.2）
pub fn verify_auth_response(&self, nonce: &[u8; 32], resp: &AuthResponse) -> Result<(), AuthError>;
```

- 校验内容：`hmac` base64 合法、长度 32B、MAC 匹配（`verify_mac` 内建常量时间比较）；`session_context` base64 解码为 32B。
- **校验失败 / 握手超时（10s）**：断开 + 审计计数日志（token_id 级别，不含 token 本体）+ `TransportEvent::Stopped(AuthFailed)`——**不自动重连**（防冒充 server 反复投毒；§9.2「校验失败即断开（关闭码 4502 + 审计计数）」的 machine 侧对应，4502 是本机主动关闭语义【决策】）。
- **重连时重新握手**：新 nonce、新连接上下文；旧握手报文天然失效（nonce 单次使用 + 连接绑定，§9.2 协议级属性在 server 侧 enforce，machine 侧以新 nonce 保证自己不重放）。

---

## 5. seq 与 epoch 分配（§4.5.1）

| 场景 | epoch | seq |
|------|-------|-----|
| session 新开（首次 spawn） | 1 | 首帧 1，逐帧 +1 |
| ACP 子进程重建（同 session_id 重新 spawn） | 水位记录 + 1 | 重置为 1 |
| daemon 重启后重建 | 水位记录 + 1 | 重置为 1 |
| 同一代际内断线/重连 | 不变 | 连续（缓冲帧 seq 与实时帧同一序列） |
| 会话退出/清理 | 写入水位（epoch 保留，供未来重建 +1） | last_seq 写入水位（诊断） |

**seq 起点依据**：架构未明示 seq 首值；`f3-persist.md` §6 对齐——server 侧无日志时 `(epoch=0, last_seq=0)`，补推 `from_seq = last_seq + 1 = 1`，故 **machine 首帧 seq = 1**（计数器初始 0，第一帧分配 1）。`epoch` 起点 1 为架构明示（§4.5.1「session 新开为 1」）。

**水位跨重启**（§4.4.3）：epoch 必须持久化（否则重启后 epoch 回到 1，会与 server 持久化的旧 `(epoch=1, last_seq)` 误判为同代际，§4.5.1 的 epoch 判定失效）；缓冲不跨重启（§3.3）。

**last_sent_seq**：在线时每帧 ws 写成功即推进（内存）；断线瞬间 = 已确认送达的最大 seq；重连补推 `from_seq = last_sent_seq + 1`（= pending 首帧 seq）。

---

## 6. 缓冲与补推协议细节（§8.5）

### 6.1 补推顺序保证（无 ack 的成立条件）

协议无 `buffer_sync_ack`（冲突 4）。保证链：

1. **machine 侧**：每 session 一条有序流——断线帧进 pending（seq 连续），重连后 pending 帧按 seq 序分批组 `machine/buffer_sync` 发出，**该 session 的实时帧排在 pending 之后**（补推期间新帧追加 pending 尾部，随补推续发；pending 清空才转实时 `machine/event`）——即「先排空 buffer_sync 再恢复实时转发」（§8.5 补推纪律的 machine 侧实现）。
2. **传输侧**：同一 ws 连接内帧序由 TCP 保证（writer task 串行写）。
3. **server 侧**：每 session 串行消费者（§7.4）按到达序处理 → 补推帧先到、实时帧后到，天然「补推先于实时」；重复段（from_seq 重叠）由聚合器幂等（§6.3）兜底。

### 6.2 buffer_sync 帧参数

- `session_id`、`epoch`（当前流纪元，server 校验，不一致拒绝该批，§4.5.1）；
- `from_seq` = 本批首帧 seq = `last_sent_seq + 1`；
- `frames[]` = 按 seq 升序连续帧；
- **分批**【决策】：单批 frames 序列化合计 ≤ 512KB 或 ≤ 256 帧（先达者），跨批保持发送顺序；
- 每批发送成功（ws 写成功）后推进 `last_sent_seq`；发送中断（再次断线）→ 未发帧保留在 pending，重连后重发（from_seq 不变）。

### 6.3 hello 上报

`buffered` = 任一 session pending 非空；`buffer_lost` = 本次 daemon 启动是否发生过缓冲丢弃（重启后 true，§7.5）；`stream_epochs` = 存活 session 的当前 epoch 映射。

---

## 7. spawn/kill 幂等执行与上报（§4.5）

| 指令 | 前置校验 | 行为 | 应答 |
|------|---------|------|------|
| `machine/spawn` | 认证已通过；env 键白名单（§9.6 双端校验：基集 `PATH`/`HOME`/`LANG`/`SHELL` + env `ACP_HUB_ENV_ALLOWLIST` 追加【决策】；值校验：UTF-8 + 长度 ≤ 4096【决策】）；`cwd` 存在性【决策】 | 会话表命中 → 返回现有句柄；未命中 → spawn（进程组、kill_on_drop）→ 水位写 epoch | `spawn_ack{ok:true}` / `{ok:false, error: 脱敏原因}`（不返回 env 值/token 相关细节，§9.3） |
| `machine/kill` | 认证已通过 | 命中 → 组级 SIGTERM→grace→SIGKILL；未命中/已退出 → 视为已达成 | `kill_ack{ok:true}`（幂等，proto 注释「已死成功返回」） |
| 进程退出（自发） | — | wait 完成 → `process_exit{session_id, code}` → 水位更新 → 缓冲清理 → 会话条目保留（acp=None） | 上行 `machine/process_exit` |

「按 session_id 幂等」是 server 安全重发的前提（§4.5「server 可安全重发」）；machine 侧不查 `command_id` 去重（重复 spawn 因 session_id 命中自然幂等；重复 kill 同）。

---

## 8. 孤儿进程清理（§7.5，三层语义）

1. **kill_on_drop**（正常路径）：daemon 优雅退出 / session 句柄 drop → 直接子进程被杀（沿用 child.rs:45 语义）。
2. **进程组 kill**（session 级路径）：spawn 时 `process_group(0)`，kill 走 `kill(-pgid)` → 整棵进程树（ACP + 其孙进程）一并终止；防止 kill ACP 后孙进程（shell/工具）成孤儿。
3. **启动时清理残留**（daemon 崩溃路径，§7.5 步骤 2 的 machine 侧补充）：水位文件记录 pgid；启动时对残留 pgid `kill(-pgid, SIGKILL)`（ESRCH 忽略）+ 删除 `buffer/` 目录。server 侧「对已标记 interrupted 但 machine 声称存活」的 session 下发 `machine/kill`（§7.5 步骤 3）是**主兜底**（覆盖水位丢失/pid 重用等 machine 侧不可靠场景）；本设计双层都做，P9 验收（kill -9 machine daemon 演练）任一闭环即可。
   - pid 重用风险【决策】：M1 接受（水位清理是尽力而为的补充，权威对账在 server 侧）。

---

## 9. 旧代码处置

| 文件 | 决策 | 说明 |
|------|------|------|
| `child.rs` | **保留改造** | 见 §4.1 迁移清单：保留 spawn/kill_on_drop/stdio 行协议/read/wait；新增进程组（spawn `process_group(0)`、kill `kill(-pgid)`）；删除 `next_id`/`pending`/`send_request`/`send_notification` 的请求-响应匹配（旧 hub 侧 RPC 模式；新架构 initialize/prompt 由 server 驱动，machine 不再自行发起 ACP 请求）；保留 `write_line` 供下行 forward 帧（冲突 1）使用；stdout 解析不再区分 pending，全部原样上行 + sessionId 提取 |
| `hub.rs` | **保留改造** | 旧 stdio 主循环（IDE stdin 读/写、child_msg 转发 stdout）删除；变身为 machine daemon 主循环（§4.2），并吸收旧 `SessionRouter` 的会话表职责 |
| `router.rs` | **废弃**（占位壳） | 旧 spawn+initialize 时序、`session/new` 透传、RouterEvent 均不再需要（server 全权驱动）；`git rm router_test.rs` |
| `global.rs` | **废弃**（占位壳） | `handle_initialize`/`handle_session_list`/`handle_commands_list` 是单机 stdio 面的能力声明，machine daemon 不再应答 JSON-RPC；`git rm global_test.rs` |
| `error.rs` | **保留** | `extract_session_id`（§3.3 双格式 sessionId 提取的参考实现）、`ok_response/error_response`（若未来下行 forward 需要应答）；新增 machine 内部错误码（缓冲满/认证失败/进程组 kill 失败等，`thiserror` 枚举放各模块，不扩展 JSON-RPC 码面） |
| `bin/test_child.rs` | **保留** | 假 ACP 进程（含 `--crash-after` 崩溃模拟），供集成测试 |
| `main.rs` | **重写** | CLI：`--server-url`（env `ACP_HUB_SERVER_URL`）、`--token-file`（env `ACP_HUB_TOKEN_FILE`，必填）、`--data-dir`、`--log-level`、`--json-log`；优先级 CLI > env > 默认。**不引入配置文件**（Cargo.toml 无 toml 依赖，M1 machine 侧最小面【决策】；如需 `config.toml` 对齐 server 侧，记录依赖由主管处理） |

---

## 10. 配置与默认值（§16 对齐）

machine 侧只消费 §16 中与 machine 相关的默认值（来源 `proto::Defaults`），不重复定义：

| 项 | 默认值 | 来源 |
|----|--------|------|
| 心跳间隔 | 5s | §16 / proto::Defaults |
| 重连退避 | 1s 起 ×2 → 60s 上限 | §7.1 |
| 缓冲上限（内存+磁盘合计） | 10MB / 万条 | §16 / proto::Defaults |
| 内存半区 | 5MB【决策】 | §8.5 口径拆分 |
| 单帧上限 | 1MB（超限跳过 + gap） | §16 / proto::Defaults |
| 环形滑窗 | 500 条 | §16 / proto::Defaults |
| 握手超时 | 10s【决策】 | 对齐 spawn 超时组（§16） |
| kill grace | 3s【决策】 | server 可经 `kill.grace` 覆盖 |
| env 白名单（machine 侧） | 基集 + `ACP_HUB_ENV_ALLOWLIST`【决策】 | §9.6 双端校验 |
| 数据目录 | `~/.local/share/acp-hub/machine/`（0600） | §16 语义 |

---

## 11. 测试清单

| # | 用例 | 断言要点 | 层次 |
|---|------|---------|------|
| T1 | 重连退避（§7.1） | 1s→2s→4s→…→60s 上限；连接成功且认证通过后重置为 1s；4502/AuthFailed → 停止重连（`Stopped`） | transport 单测（`tokio::time::pause` + 本地 ws stub） |
| T2 | 心跳 | 每 5s 发 `machine/heartbeat`；内容含 load 与 alive_sessions | hub 集成（stub server） |
| T3 | 缓冲分桶（§8.5） | 两 session 帧互不串扰（独立 seq/容量/水位） | buffer 单测 |
| T4 | 缓冲丢弃（§8.5） | 超预算：事件类（通知/原始帧）先丢、控制类（带 id）最后丢；单帧超 1MB 跳过 + gap 计数；条数超限触发 | buffer 单测 |
| T5 | 磁盘溢出（§8.3） | 内存满 → append 写盘 → drain 顺序一致；合计预算生效；文件 0600；清理删除 | buffer 单测（tempfile） |
| T6 | seq 单调（§4.5.1） | 新 session 首帧 seq=1 逐帧 +1；重建（水位）后 epoch+1 且 seq 重置 1 | hub 单测（假 AcpProcess/水位） |
| T7 | spawn 幂等 | 同 session_id 二次 spawn → 仅一个进程 + 两次 `spawn_ack ok` | hub 集成（test_child 计数） |
| T8 | kill 幂等 + 进程组 | 已死 kill → ok；kill 后孙进程（shell 起的 sleep）同组被杀 | child 集成 |
| T9 | process_exit | 正常退出 code 0 / `--crash-after` 崩溃 code 1 → 上报 `(session_id, code)`；会话条目保留供重建 | hub 集成 |
| T10 | buffer_sync 补推（§8.5） | 断线 → 缓冲 → 重连认证 → `from_seq = last_sent+1`、帧按 seq 升序分批 → 补推期间新帧续接 → pending 清空转实时；全程顺序断言 | hub 集成（stub server 模拟断线） |
| T11 | 双向认证（§9.2） | hello 每次新 nonce；正确 auth_response → Authenticated；伪造 HMAC/错误长度 → Failed + 审计日志；认证通过前收到 spawn → 不执行 + 计数 | auth 单测 + hub 集成 |
| T12 | 孤儿清理（§7.5） | drop 后进程死（kill_on_drop）；启动时水位残留 pgid 被杀（ESRCH 安全）；buffer/ 目录启动时删除 | child/启动集成 |
| T13 | 水位（§5.3） | epoch 变更持久化；重启加载 epoch+1、seq 重置；`buffer_lost: true` 上报 | hub 单测（tempfile） |
| T14 | 全链路 E2E | stub ws server 驱动：hello → auth_response → spawn → 帧转发（machine/event 带 seq/epoch）→ 断线缓冲 → 重连补推 → kill → process_exit | 集成（`bin/test_child.rs` + stub server） |

---

## 12. 新增依赖（交主管处理）

| 依赖 | 用途 | 说明 |
|------|------|------|
| `libc`（workspace） | `kill(-pgid, SIGTERM/SIGKILL)` 进程组 kill | 预填依赖无替代；tokio `Child::kill` 只杀直接子进程 |

---

## 13. 关键决策摘要

1. **补推顺序保证 = ws 帧序 + server 串行消费者**，无需 buffer_sync_ack（§6.1）。
2. **seq 首帧 = 1**（与 f3-persist 的 `from_seq = last_seq + 1 = 1` 对齐）；epoch 水位必须跨重启持久化（防止与 server 旧代际记录误判同代际）。
3. **丢弃分类用信封结构性规则**（有 `id` = 控制类）而非事件语义解析——忠于 §3.3 dumb pipe 最小协议面；「终态帧最后丢弃」由 server gap 兜底。
4. **单帧超限 = 跳过 + gap 计数**（§8.5 原文），不做截断。
5. **孤儿清理三层**：kill_on_drop（正常）→ 进程组 kill（session 级）→ 启动时水位 pgid 清理（崩溃路径，server 对账 kill 为主兜底）。
6. **下行 ACP 转发帧是 M1 帧集缺口**（冲突 1），child.rs 预留 stdin 写入接口，建议新增 `machine/forward` 帧，待 F5/F1 裁决。

---

## 14. 日志与脱敏（§9.3）

- target 统一 `acp_hub::machine`；只记：session_id / command_id / epoch / seq / 字节数 / 条数 / 耗时 / 退出码 / 丢弃计数（分类）/ token_id（如可得，从配置文件名派生）。
- **禁止记录**：ACP 帧正文、token 与派生密钥、env 值、cwd 完整路径（如记录则脱敏主目录）。
- 审计面：认证失败（含原因类别与计数）、spawn/kill 结果、缓冲丢弃（§17.1 指标字段来源）。
