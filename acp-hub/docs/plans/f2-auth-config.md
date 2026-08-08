# F2 设计：server 配置与认证模块（config + auth）

> 状态：设计稿（对应 Feature F2）
> 日期：2026-08-07
> 权威来源：`docs/architecture.md`（v2.4）§4.7/§9.2/§9.2.1/§9.2.2/§9.3/§9.4/§9.5/§9.6/§16/§17.1/§12
> 约束：**忠于架构文档**。文档未指明处的命名/参数选择均标注「【决策】」并给出依据；§16 全表逐项映射为 Rust 字段；复用 `acp-hub-proto` 已有类型（`hmac.rs`/`conn.rs`/`whitelist.rs`/`machine.rs`/`version.rs`/`protocol.rs`），不重复实现密码原语。
> 范围：server crate 的 `config` 与 `auth` 两个模块 + `main.rs` 的 CLI/装配（见 §1.2 边界声明）。**不修改** `lib.rs`、`Cargo.toml`（新依赖见 §7，由主管统一处理）。

---

## 1. 目标与范围

### 1.1 目标

1. **Config**：§16 全表配置项的加载管线（CLI > 环境变量 > `config.toml` > 默认值）、目录解析与 0600 权限、tracing 初始化。
2. **Auth**：token 模型（machine/full/read-only 三级）、TokenStore（生成/校验/宽限期轮换/吊销）、machine 双向认证服务端流程（HMAC challenge-response）、nonce 防重放（TTL 30s）、审计最小集与失败计数、ConnectionCtx。
3. 非回环拒绝策略的**接口挂钩点**（默认拒绝，§9.5）——gateway 是 F5，本设计只给接口。

### 1.2 边界声明

| 归属 | 内容 | 落点 |
|------|------|------|
| F2 内 | CLI 解析、子命令分发（`run`/`token list`/`token generate`/`token revoke`）、tracing 初始化、启动时 token bootstrap | `server/src/main.rs`（现为骨架占位，非他 feature 模块） |
| F5（gateway） | ws 生命周期、连接注册/配额、帧级授权（`m1_check` + read-only 拒 action）、hello 幂等 fencing | `server/src/channel/gateway`；本设计只给调用点与接口 |
| F5/control | spawn env 白名单在 `session/create` 的消费、command 队列 | 本设计只给 `Config` 侧判定接口 |
| F3（machine） | machine 侧校验 `auth_response`、不执行 spawn/kill 前认证、env 双端再校验 | machine crate |
| F-observability | 指标聚合（§17.1 计数从结构化日志/计数原语聚合） | 本设计提供计数原语与审计事件面 |

## 2. 模块结构总览

占位单文件扩展为目录（`git rm` 原单文件后建目录，仅限本 feature 模块）：

```
server/src/
├── main.rs            # F2 修改：clap CLI、config 加载、tracing 初始化、token 子命令分发、run 装配
├── config.rs → config/
│   ├── mod.rs         # Config 结构体（§16 全表）、FsyncMode、加载管线、目录解析、日志初始化
│   ├── duration.rs    # 时长字符串解析（"5s"/"16ms"/"24h"/"90d"，serde 自定义反序列化）
│   └── config_test.rs
└── auth.rs → auth/
    ├── mod.rs         # TokenRole / TokenRecord / TokenInfo / TokenStore / AuthService / ConnectionCtx / AuthError
    ├── nonce.rs       # NonceRegistry（包装 proto::SeenNonces + TTL 30s 时间戳）
    ├── audit.rs       # audit() 结构化日志最小集（§9.4）
    ├── auth_test.rs   # TokenStore / AuthService / 脱敏断言
    └── nonce_test.rs  # NonceRegistry 单测
```

测试沿用仓库规范：`*_test.rs` 同目录 `#[path]` 引入；`tempfile`/`serial_test` 已预填 dev-dependencies。

**复用 proto 类型**（不再实现）：

- `hmac::generate_challenge_nonce/generate_session_context/derive_mac_key/mac_input/compute_mac/verify_mac/SeenNonces/NONCE_TTL`；
- `conn::Auth/AuthResponse/CLOSE_CONFIG_FATAL(4502)`；
- `machine::MachineHello`（`nonce` 为 base64）；
- `whitelist::Role`（线级连接角色，与 token 角色映射见 §4.1）；
- `version::PROTOCOL_VERSION`、`protocol::Defaults`（§16 协议参数默认值常量）。

---

## 3. Config 模块

### 3.1 Config 结构体（§16 全表映射）

```rust
/// §16 全表项。字段一律 snake_case（配置文件为内部格式，非线协议，不强制 camelCase）。
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    // ---- 网络（§16）----
    pub listen_addr: IpAddr,            // 默认 127.0.0.1（M1 本机；M2 显式改 0.0.0.0）
    pub listen_port: u16,               // 默认 8456
    // ---- 目录（§16）----
    pub data_dir: PathBuf,              // 默认 ~/.local/share/acp-hub/（XDG 语义，§3.5）
    pub config_dir: PathBuf,            // 默认 ~/.config/acp-hub/（XDG 语义，§3.5）
    // ---- 协议参数（默认值引自 proto::Defaults，server 可覆盖）----
    pub heartbeat_interval: Duration,   // proto Defaults::HEARTBEAT_INTERVAL = 5s（§16/§7.1）
    pub offline_timeout: Duration,      // proto Defaults::OFFLINE_TIMEOUT = 30s
    pub buffer_limit_bytes: usize,      // proto Defaults::BUFFER_LIMIT_BYTES = 10MB（§8.5）
    pub buffer_limit_frames: usize,     // proto Defaults::BUFFER_LIMIT_FRAMES = 万条
    pub max_frame_bytes: usize,         // proto Defaults::MAX_FRAME_BYTES = 1MB
    pub ring_buffer_capacity: usize,    // proto Defaults::RING_BUFFER_CAPACITY = 500 条（§8.5）
    // ---- server 运维配置（§16 其余项）----
    pub command_queue_cap: usize,       // 64（§7.4）
    pub connection_quota: usize,        // 200（§8.6）
    pub backpressure_soft_bytes: usize, // 64KB（§8.6）
    pub backpressure_hard_bytes: usize, // 128KB（§8.6，校验 soft <= hard）
    pub microbatch_window: Duration,    // 16ms（§6.4）
    pub replay_window: Duration,        // 10s（§8.6）
    pub permission_timeout: Duration,   // 5min（§7.1）
    pub cancel_timeout: Duration,       // 10s（§7.1）
    pub spawn_timeout: Duration,        // 10s（§6.2）
    pub initialize_timeout: Duration,   // 10s（§6.2）
    pub binding_timeout: Duration,      // 30s（§6.2）
    pub fsync_mode: FsyncMode,          // per-commit（batch 需显式声明并降级 Ack 语义，§8.4）
    pub compact_trigger_bytes: usize,   // 64MB（§8.4）
    pub compact_max_age: Duration,      // 24h（§8.4）
    pub disk_budget_bytes: usize,       // 2GB（§8.4）
    pub archive_retention: Duration,    // 90 天（§8.4）
    pub spawn_env_allowlist: BTreeSet<String>, // 空（仅继承基集，§9.6）
    pub allow_non_loopback: bool,       // false（§9.5，显式声明才接受非回环连接）
    // ---- 日志（非 §16 表项，server 本地默认）----
    pub log_level: String,              // 默认 "info"
}
```

```rust
/// §16 fsync 模式：batch 需显式声明并降级 Ack 语义（§8.4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FsyncMode { PerCommit, Batch }
```

**时长在 toml/CLI 的形态**【决策】：toml 用可读字符串（`"500ms"`/`"5s"`/`"16ms"`/`"24h"`/`"90d"`，支持 `ns/us/ms/s/m/h/d` 后缀），经 `config/duration.rs` 自定义 serde 反序列化——§16 表格即此形态（"5s / 30s"、"90 天"），比整型毫秒可维护。非法字符串 → 启动错误，不静默取默认。

### 3.2 加载管线（CLI > 环境变量 > 配置文件 > 默认值）

```
默认值表 (fn defaults(), §16 / proto::Defaults)
   └─ < 配置文件 (~/.config/acp-hub/config.toml，存在才读；未知键 → 启动失败，§3.4)
        └─ < 环境变量（仅 CLI 暴露的标量集，clap `env` feature 注入）
             └─ < CLI 显式参数（clap，Option<T>）
                  → Config（不可变，启动时一次性构建）
```

实现要点：

1. **合并顺序**：`FileConfig`（全 `Option`，serde `deny_unknown_fields`）先解析；`CliArgs`（全 `Option`，无 `default_value`）用 `#[arg(env = "ACP_HUB_*")]` 获得环境变量回退；`Option::or(file).or(defaults)` 逐字段合并。clap 的 env 语义天然保证 **CLI 显式 > env > 无**。
2. **环境变量命名**：`ACP_HUB_LISTEN_ADDR` / `ACP_HUB_LISTEN_PORT` / `ACP_HUB_DATA_DIR` / `ACP_HUB_CONFIG_DIR` / `ACP_HUB_LOG_LEVEL`。**【决策】** 环境变量只覆盖与 CLI 相同的标量集；列表/表结构（`spawn_env_allowlist`）与 Duration 类项仅走配置文件——env 表达不了集合，避免为 env 发明逗号分隔语法。
3. **验证不变量**（加载后、启动前）：`backpressure_soft <= backpressure_hard`、`connection_quota > 0`、端口非 0、超时组全部 > 0。违反 → 启动错误（fail-fast，不静默降级）。
4. `--config <path>` 仅 CLI 提供（无 env），覆盖配置文件路径本身。

### 3.3 CLI 与子命令（main.rs）

```
acp-hub-server [run] [--listen <addr>] [--port <port>] [--config <path>]
               [--data-dir <dir>] [--log-level <lvl>] [--json-log]
acp-hub-server token list
acp-hub-server token generate --name <name> [--role machine|full|read-only]
acp-hub-server token revoke <token_id>
```

- **`run` 为默认子命令**【决策】：`acp-hub-server` 直接启动服务（常驻进程主形态），`token` 子命令组管理凭据。clap `subcommand_required = false` + 默认 `run`。
- 全局参数：`--config`（配置文件路径）、`--log-level`、`--json-log`（与 machine/main.rs 既有形态一致）。
- `token generate` 输出：仅 stdout 打印一次完整 token（供复制到 machine 配置 / TUI 配置），同时打审计 `token.generate`（含 token_id，**不含 token 本体**）；`token list` 只输出 `TokenInfo`（无 token 本体，§9.2.1「视图对象只暴露 token_id」）。
- **启动 bootstrap**【决策】：`run` 启动时若 `tokens.toml` 中不存在任何未吊销 `machine` 角色 token，自动生成一个（name = `"bootstrap-machine"`）并打印到 stderr——machine 是 outbound 无交互端，无法自行引导；client token **不**自动生成（TUI 是交互端，由运维 `token generate` 显式签发）。「自动生成的 token 会打印」在启动横幅中明示，配合 §9.3 日志脱敏（token 本体**不进日志**，只进终端一次）。

### 3.4 toml 解析与未知键取舍

**【决策】未知键 → 启动失败**（`deny_unknown_fields` + 错误信息列出所有未知键）。

依据：配置键承载安全语义（`allow_non_loopback`、`spawn_env_allowlist`），未知键静默忽略会把拼写错误（如 `allow_nonloopback`）伪装成「安全默认」——本项目安全方向要求 fail-closed（§9.5 默认拒绝精神一致）；且配置文件是运维唯一长期资产，typo 应立即暴露而非运行时才显形。代价（升级时旧配置兼容）由错误信息明确列出未知键缓解。

### 3.5 目录解析与权限

- **路径语义**【决策】：跨平台统一 **XDG 语义**——`XDG_CONFIG_HOME`/`XDG_DATA_HOME` 环境变量优先，否则 `$HOME/.config/acp-hub`、`$HOME/.local/share/acp-hub`（home 经 `dirs-next` 的 `home_dir()` 解析，满足任务要求的 dirs-next 使用）。依据：架构 §16 字面即 XDG 路径（`~/.config/...`/`~/.local/share/...`），跨平台确定性与文档一致性优先；macOS 上 dirs-next 的 `config_dir()` 返回 `~/Library/Application Support`，与文档字面不符且难以在测试中注入。
- **权限**【决策】：「0600」按「仅属主可访问」语义落地——**目录 0700**（0600 目录缺 `x` 位，属主自身也无法遍历/建文件，非功能权限）、**秘密文件（tokens.toml）严格 0600**。目录创建 `create_dir_all` 后统一 `chmod 0700`；token 文件写入后 `chmod 0600`（unix `PermissionsExt`）。已存在目录**不强制改权限**（可能是有意的多用户目录），仅新建时收紧。
- 数据目录/配置目录均按上述规则；测试用注入 env 的临时 HOME 断言权限位。

### 3.6 日志初始化（config 模块提供）

```rust
pub fn init_tracing(cfg: &Config, json_log: bool) -> Result<(), InitError> // try_init 防测试双初始化
```

优先级：`RUST_LOG` env（`EnvFilter::try_from_default_env`）> CLI `--log-level` > 配置 `log_level` > `info`。输出形态与 machine/main.rs 一致（fmt 或 json 到 stderr）。**脱敏纪律**（§9.3）：`tracing` 字段只记关联 ID/状态/耗时/大小，token/正文/参数永不落日志——见 §4.9 的测试断言。

### 3.7 跨模块接口（供 F5/control 消费）

```rust
impl Config {
    /// §9.5 非回环拒绝：回环地址恒放行；非回环仅在 allow_non_loopback=true 时放行。
    pub fn allow_peer(&self, peer: &SocketAddr) -> bool {
        peer.ip().is_loopback() || self.allow_non_loopback
    }
    /// §9.6 env 白名单：键在 allowlist 内或为基集（PATH/HOME/LANG）才允许。
    pub fn is_env_key_allowed(&self, key: &str) -> bool;
}
```

**基线集常量**【决策】：基集取 `["PATH", "HOME", "LANG"]`（§9.6「如 PATH/HOME/LANG」示例），`spawn_env_allowlist` 是**增补**集合；键名匹配大小写敏感。machine 侧双端再校验（§9.6）属 F3。

---

## 4. auth 模块

### 4.1 TokenRole（token 角色）与线级 Role 的映射

```rust
/// token 三级角色（§9.2.2 + §9.5）。串行化为 kebab-case 字符串。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TokenRole {
    Machine,    // 收 spawn/kill、上报事件/心跳（§9.2）
    Full,       // client：读全部 Doc + 发 Action（TUI）
    ReadOnly,   // client：仅读 yjs 状态与订阅事件流（M3 Web 面板，M1 预留档位）
}

impl TokenRole {
    /// 线级连接角色（whitelist::Role）：machine→Machine；full/read-only→Client。
    pub fn wire_role(self) -> whitelist::Role;
    /// 是否可发 Action（machine/full 可；read-only 不可，M1 即强制）。
    pub fn can_send_action(self) -> bool;
    /// HMAC 派生 role 字符串（§9.2：仅 machine 走双向认证，取值恒为 "machine"）。
    pub fn as_str(self) -> &'static str; // "machine" | "full" | "read-only"
}
```

**关系说明**：`whitelist::Role { Client, Machine }` 是**线级**连接角色（`m1_check` 用，两值）；`TokenRole` 是**身份级**角色（三值）。`TokenRole::wire_role()` 是唯一映射点——read-only 与 full 在线级同属 Client，其写权限差异由 gateway 在帧级用 `can_send_action()` 强制（§5 挂钩点）。

### 4.2 TokenRecord / TokenInfo

```rust
/// 存储态记录（含 token 本体，仅内存与 0600 文件内存在）。
pub struct TokenRecord {
    pub id: String,                // uuid v4（视图/审计/吊销引用键）
    pub role: TokenRole,
    pub name: String,              // machine：hostname；client：运维命名（如 "桌面 TUI"）
    pub token: String,             // 32B CSPRNG，base64（44 字符，§9.2.1「32B CSPRNG」）
    pub created_at: DateTime<Utc>, // RFC3339
    pub revoked: bool,
}

/// 对外视图（§9.2.1：只暴露 token_id，绝不暴露 token 本体）。
pub struct TokenInfo { pub id: String, pub role: TokenRole, pub name: String,
                       pub created_at: DateTime<Utc>, pub revoked: bool }
```

- `token` 生成：`rand::rng().fill_bytes` 32B → base64 标准字母表；生成时断言长度 44 并防碰撞重试（hashset 查重）。
- **存储路径**：`<config_dir>/tokens.toml`（0600）。**【决策】** 格式选 TOML（与 config 同一 serde 栈、运维可手修恢复；密钥材料靠 0600 + 目录 0700 保护，不做加密——单用户本机威胁模型，§9.1）。

### 4.3 TokenStore

```rust
pub struct TokenStore { path: PathBuf, records: Vec<TokenRecord>, last_mtime: Option<SystemTime> }

impl TokenStore {
    /// 加载：文件不存在 → 空 store；存在且坏格式/坏 token 长度 → Err（拒绝启动，不静默覆盖）。
    pub fn load(path: &Path) -> Result<Self, StoreError>;
    /// 生成并持久化（原子写，§4.3.2）。
    pub fn generate(&mut self, role: TokenRole, name: &str) -> Result<TokenRecord, StoreError>;
    /// 吊销并持久化（幂等：已吊销/不存在返回 Ok(None)，不报错）。
    pub fn revoke(&mut self, id: &str) -> Result<Option<TokenRecord>, StoreError>;
    /// 视图列表（无 token 本体）。
    pub fn list(&self) -> Vec<TokenInfo>;
    /// 校验：常量时间比较 + 角色 + 吊销态；**每次调用先按 mtime 惰性重载**（§4.3.3）。
    pub fn validate(&mut self, candidate: &str, required: TokenRole) -> Result<TokenRecord, AuthError>;
}
```

#### 4.3.1 文件格式

```toml
# <config_dir>/tokens.toml（0600，仅属主可读）
version = 1

[[tokens]]
id = "b1c2…uuid"
role = "machine"          # machine | full | read-only
name = "desktop-01"
token = "8Xh…/44 字符 base64"
created_at = "2026-08-07T10:00:00Z"
revoked = false
```

#### 4.3.2 持久化与原子写

所有写路径统一 `persist()`：序列化 → 写同目录临时文件（`.tmp` 后缀）→ `fsync` → `rename` 覆盖 → 目录 `fsync`。崩溃不产生半文件；token 是安全关键资产，fsync 不可省（server 重启后丢失新 token = 机器被锁，§9.2.1 备份清单的一部分）。文件头 `version = 1`，未来格式演进可迁移。

**【决策】并发模型**：单 server 进程持有内存 store；CLI `token generate/revoke` 直写同一文件。不做文件锁（后置，见 §8 待确认项）——单运维人员本机场景下，mtime 惰性重载（§4.3.3）已消除「CLI 改完 server 不认」的主分歧；「server 运行中 CLI 生成 → server 内存未同步 → 双方同时写」的竞态窗口接受为已知限制（两写互相覆盖的概率与影响均低，且 token 文件可手修恢复）。

#### 4.3.3 宽限期轮换与 mtime 惰性重载

- **宽限期轮换**（§9.2.1）：store 天然支持共存——`validate` 对所有**未吊销** token 依次比较，新旧 token 同时有效；运维流程 = `token generate`（新）→ 逐机切换 → `token revoke`（旧，即刻生效）。**禁止**「先删后建」式静态轮换（文档明示）。
- **mtime 惰性重载**【决策】：`validate()` 前 `stat` 文件 mtime，与 `last_mtime` 不一致则重载文件并合并——CLI 的 generate/revoke 对运行中 server **即时生效**，无需控制面通道（M1 无控制面，这是最小实现）。重载失败（文件被手改坏）→ 保持旧内存态 + `error!` 审计（不静默，也不因手改挂掉服务）。
- **常量时间比较**【决策】：token 为固定长度（44 字符 base64，加载时断言），候选与存量记录做常量时间比较（`subtle::ConstantTimeEq`，新依赖见 §7；若主管不批则退化为手写 XOR-fold 常量时间比较）。杜绝经「校验耗时」枚举合法 token 的时序侧信道。

#### 4.3.4 启动 bootstrap

`run` 装配时调用 `store.ensure_machine_token()`：无未吊销 machine token → `generate(Machine, "bootstrap-machine")` + 打印完整 token 到 stderr + 审计 `token.generate`。见 §3.3。

### 4.4 NonceRegistry（challenge 防重放，TTL 30s）

proto 的 `SeenNonces` 是**无时间戳**的 HashSet（纯内存容器）；「30s 过期」的判定在 server auth 模块落地：

```rust
/// 判定结果（与 AuthError 区分：前者是正反判定，后者是错误面）。
pub enum NonceVerdict { Accepted, Replay, Expired }

/// proto::SeenNonces + 时间戳包装：非重复 + 30s 窗口 + 过期清理。
pub struct NonceRegistry { seen: HashMap<[u8; 32], Instant> }

impl NonceRegistry {
    /// 判定并登记：未见过且在窗口内 → Accepted 并记录；见过 → Replay；
    /// 见过但已过期 → 视为新 nonce 处理（TTL 语义，§9.2「短期有效窗口 30s 过期」）。
    pub fn check_and_mark(&mut self, nonce: &[u8; 32], now: Instant) -> NonceVerdict;
    /// 惰性 + 周期清理：按 now 清除过期条目（防止内存无限增长）。
    pub fn sweep(&mut self, now: Instant);
}
```

- **窗口语义**【决策】：「30s 有效」= nonce 在**登记时刻起 30s 内**有效；过期后同 nonce 再次提交按新 nonce 处理（Accepted）——机器 30s 后重连必然携带新 nonce（每次连接新生成，§9.2 步骤 1），无冲突。
- **连接断开即失效**（§9.2 协议级属性）：由全局 30s TTL 自然覆盖（nonce 生命周期 ≤ 连接生命周期 + 30s 余量），**不需要**按连接删除——全局单例注册表更简单，且 hello fencing（§4.5）后旧连接的 nonce 即便重放也因新 hello 的幂等替换失效。
- **调用时机**：`authenticate_machine` 在 **token 校验之前**先 `check_and_mark`——认证失败的 nonce 同样登记，防「失败后重放成功路径」；sweep 由 server 周期任务调用（与心跳同 tick 即可，F5 装配）。

### 4.5 AuthService：machine 双向认证服务端流程

```rust
pub struct AuthService { store: TokenStore, nonces: NonceRegistry, stats: AuthStats }

impl AuthService {
    /// machine 连接认证（§9.2 步骤 1–2）。成功返回连接上下文 + auth_response 载荷。
    pub async fn authenticate_machine(&mut self, hello: &MachineHello, peer: SocketAddr)
        -> Result<MachineAuthOk, AuthError>;
    /// client 连接认证（单向，`auth` 帧）。成功返回连接上下文。
    pub async fn authenticate_client(&mut self, auth: &Auth, peer: SocketAddr)
        -> Result<ConnectionCtx, AuthError>;
}

/// 成功产物：连接上下文 + 待 gateway 下发的 auth_response。
pub struct MachineAuthOk {
    pub ctx: ConnectionCtx,
    pub response: AuthResponse, // { session_context: b64, hmac: b64 }
}
```

**服务端时序**（对应 §9.2 步骤 1–3，权威时钟在 server，§4.7）：

```
machine ── machine/hello { token, nonce(base64), hostname, caps, ... } ──► server
1. nonce: base64 解码 → [u8;32]（失败 → BadNonceEncoding）
2. NonceRegistry.check_and_mark(now)  → Replay/Expired → 拒绝
3. TokenStore.validate(token, Machine) → UnknownToken / RevokedToken / RoleMismatch → 拒绝
4. session_context = generate_session_context()          (32B CSPRNG)
5. key = derive_mac_key(token_bytes, "machine")          (HKDF，§9.2 顾问3)
6. input = mac_input(&nonce, &ctx, &PROTOCOL_VERSION.to_string(), "machine")
7. hmac_b64 = base64(compute_mac(key, input))
8. 下发 auth_response { session_context: b64, hmac: hmac_b64 }        ← server 身份证明
9. 注册连接（gateway F5：hello 幂等替换 fencing、注册 ConnectionCtx）
   machine 校验通过前不执行任何 spawn/kill（machine 侧行为，F3）
```

**版本绑定**（§9.2 协议级属性）：MAC 输入含 `protocol_version`，server 恒用本地 `PROTOCOL_VERSION`（=1，双方共享同一 proto crate 常量）计算；machine 侧用同一常量校验——版本不一致 → MAC 校验失败，**天然拒绝**。显式「版本不匹配拒绝」检查需 hello 携带版本字段，而 `MachineHello` 现无该字段（f1 待确认项；见 §8）。

**错误面与关闭码**：

```rust
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum AuthError {
    #[error("nonce 编码非法")]          BadNonceEncoding,   // 非 base64 / 非 32B
    #[error("nonce 重放")]              ReplayNonce,
    #[error("nonce 过期")]              ExpiredNonce,
    #[error("token 未登记")]            UnknownToken,       // 与 RevokedToken 分开计数（泄露检测，§9.2.1）
    #[error("token 已吊销")]            RevokedToken,
    #[error("角色不匹配")]              RoleMismatch,       // machine/hello 用 client token 等
    #[error("store 错误: {0}")]         Store(#[from] StoreError),
}
```

- **Display 与错误传播脱敏**：`AuthError` 及其所有变体**不携带 token 本体**（只在调用方闭包内保留 `token_id` 用于审计）；Display 字符串不含任何凭证材料（§9.3）。
- **失败语义**（§9.2）：任何失败 → 关闭连接 + **审计 `auth.machine` failed** + 失败计数递增（§4.8），不静默。machine 用关闭码 **4502**（`CLOSE_CONFIG_FATAL`，文档明示）；client 认证失败**【决策】**同用 4502——token 错误属配置性永久失败，重连无益（与 4502 语义一致；1011/1013 留给配额/通用瞬时失败）。

### 4.6 client 认证（单向）

`authenticate_client`：`auth { token }` → `store.validate(token, required)`——required 取 `Full | ReadOnly`（Machine 角色 token 提交 client 认证 → `RoleMismatch`，防 token 跨面复用）。read-only 的帧级写限制由 gateway 用 `ConnectionCtx.can_send_action()` 强制（§5）。client **无** HMAC（§9.2 明示仅覆盖 machine 连接）。

### 4.7 ConnectionCtx（连接上下文）

```rust
/// 认证通过后的连接身份上下文（gateway F5 持有，贯穿连接生命周期）。
pub struct ConnectionCtx {
    pub token_id: String,
    pub role: TokenRole,             // 身份级角色（§9.5：token 即身份）
    pub name: String,                // TokenRecord.name
    pub peer: SocketAddr,            // 绑定信息：远端地址（非回环拒绝判定输入，§3.7）
    pub hostname: Option<String>,    // 绑定信息：machine 专属（hello.hostname）
    pub established_at: DateTime<Utc>,
}
impl ConnectionCtx {
    pub fn wire_role(&self) -> whitelist::Role;   // §4.1 映射
    pub fn can_send_action(&self) -> bool;        // machine/full 可；read-only 不可
}
```

### 4.8 审计最小集与认证失败计数

**审计事件**（§9.4：动作类型/commandId/token_id/结果/耗时；§9.3：不记正文/凭证）：

```rust
/// auth/audit.rs —— 结构化操作日志最小集。
/// 触发点：auth.machine / auth.client / token.generate / token.revoke / conn.open / conn.close
/// （后两者由 F5 gateway 复用同一 helper 记录）。
pub fn audit(action: &str, command_id: Option<&str>, token_id: Option<&str>,
             result: &str, took: Duration);
// tracing::info!(target: "acp_hub.audit", action, command_id = ?, token_id = ?, result, duration_ms)
```

- **失败计数**（§17.1「认证失败次数（按 token_id）」）：`AuthStats` 内存计数（`Mutex<HashMap<String /*token_id*/, u64>>` + 全局 `AtomicU64`），认证失败时递增；审计事件携带 `auth_failed_total` 快照字段，结构化日志即聚合事实源（§17.1 注明 tracing 字段可聚合，M1 不建独立指标系统）。**【决策】** 计数原语先行，对外暴露（`/metrics` 等）后置到 F-observability。
- 未知 token 的失败计数 key 用 `"<unknown>"`——泄露检测（§9.2.1）依赖「按 token_id 可查失败次数」，未知 token 无 id，需与已知 token 的失败区分呈现。

### 4.9 脱敏约束（§9.3 落地到本模块）

1. 日志/审计/错误 Display 中**永不出现** token 本体、nonce、派生密钥、HMAC 输出；
2. `TokenInfo` 无 token 字段（结构级保证，编译期不可外泄）；
3. `AuthError` 变体不携带凭证材料（见 §4.5）；
4. 认证失败日志最多携带：token_id、角色、失败原因、peer 地址；
5. 测试断言见 §6（捕获日志事件断言不含 token 子串）。

---

## 5. 非回环拒绝挂钩点（F5 gateway 接口）

gateway（F5）在 ws 接入路径上按序调用，本设计只定义接口与调用点，不实现：

```
ws 连接建立（gateway）
  ├─ 1. peer 地址判定：Config::allow_peer(&peer) == false
  │       → 立即关闭（1011 通用失败）+ conn.rejected 审计计数【决策：关闭码用 1011，
  │         非回环拒绝是瞬时/环境性失败而非配置错误，重连语义留给客户端退避；4502 保留给认证失败】
  │      （判定先于认证——非回环拒绝不泄露「这里有个 server」之外的信息，不消耗认证面）
  ├─ 2. machine：AuthService::authenticate_machine(hello, peer) → MachineAuthOk
  │        → 下发 auth_response → 注册连接（hello 幂等替换 fencing 由 gateway 执行）
  │      client：AuthService::authenticate_client(auth, peer) → ConnectionCtx
  ├─ 3. 后续每帧：whitelist::m1_check(tag, ctx.wire_role(), dir)
  │        + ctx.can_send_action()（read-only 的 action 帧 → 拒绝，M1 即强制，§9.2.2）
  └─ 4. 断开：audit("conn.close", …, token_id, result, took)
```

判定逻辑唯一权威在 `Config::allow_peer`（§3.7）——默认监听 127.0.0.1 是第一道门（非回环根本连不上），`allow_non_loopback` 是显式开 LAN 后的第二道门（§9.5 语义），两层都保留。

---

## 6. 测试清单

### 6.1 config 测试（config_test.rs）

| # | 用例 | 断言 |
|---|------|------|
| C1 | 优先级 | 默认 < 配置文件 < env（`ACP_HUB_*`）< CLI 显式，逐层注入断言最终字段值（`serial_test` 防 env 污染） |
| C2 | toml 全表 | §16 全项 round-trip；缺省文件 → 全默认；空文件 → 全默认 |
| C3 | 未知键 | `foo = 1` → 启动失败，错误信息含键名（fail-fast，§3.4） |
| C4 | 坏输入 | 非法 toml / 类型错误 / 非法时长串 → 清晰错误；`fsync = "batch"` 可解析、`"weird"` 拒绝 |
| C5 | 时长解析 | `"500ms"/"5s"/"16ms"/"24h"/"90d"` 与非法后缀矩阵 |
| C6 | 权限 | 临时 HOME 注入 env：config/data 目录 mode 0700；tokens.toml 生成后 mode 0600（unix `PermissionsExt`） |
| C7 | 不变量 | `backpressure_soft > hard`、`connection_quota = 0` → 启动错误 |
| C8 | allow_peer | 回环恒放行；非回环默认拒绝；`allow_non_loopback=true` 后放行（含 IPv6 loopback） |
| C9 | env 白名单 | 基集（PATH/HOME/LANG）恒允许；允许表增补生效；表外拒绝；大小写敏感 |

### 6.2 auth 测试（auth_test.rs）

| # | 用例 | 断言 |
|---|------|------|
| T1 | 生成 | 32B→base64 44 字符；两次生成不同；字段（id/role/name/created_at）正确；落盘→重载一致 |
| T2 | 校验 | 正确 token 通过（返回记录）；未知 → `UnknownToken`；吊销后 → `RevokedToken` |
| T3 | 宽限期轮换 | 新旧并存均有效（旋转流程：generate → 新旧都过 → revoke 旧 → 旧失效新有效） |
| T4 | 原子写 | persist 后文件可解析；tmp 文件不残留；写失败（目录只读）→ Err 且原文件完好 |
| T5 | mtime 重载 | 外部改写文件（模拟 CLI revoke）→ 下一次 validate 拒绝被吊销 token |
| T6 | 角色映射 | `TokenRole::wire_role/can_send_action` 全组合表（machine→M/可写，full→C/可写，read-only→C/只读） |
| T7 | 常量时间比较 | 功能正确性：同 token 匹配 / 异 token 不匹配 / 等长前缀差异不匹配（长度防御：加载时非 44 字符 → Err） |
| T8 | 脱敏 | `TokenInfo` 无 token 字段（编译期）；`AuthError` Display 与审计事件字符串不含 token 子串；捕获 tracing 事件（测试用 fmt writer 收集）断言字段集合 ⊆ {action, command_id, token_id, result, duration_ms} |

### 6.3 NonceRegistry 测试（nonce_test.rs）

| # | 用例 | 断言 |
|---|------|------|
| N1 | 单次使用 | check_and_mark 两次 → Accepted, Replay |
| N2 | TTL | 窗口内 Accepted；`now + 30s` 后同 nonce → Accepted（新窗口语义，§4.4） |
| N3 | sweep | 过期条目清除后 `len` 归零；sweep 幂等 |
| N4 | 时钟注入 | `now: Instant` 参数注入，无真实 sleep（纯单测） |

### 6.4 握手测试（auth_test.rs，握手流程不依赖 ws——直接调 AuthService + 用 proto 原语重算验证）

| # | 用例 | 断言 |
|---|------|------|
| H1 | 成功路径 | hello → `MachineAuthOk`；用 `mac_input/derive_mac_key/verify_mac` 独立重算 `auth_response.hmac` 校验通过；`session_context` 32B |
| H2 | 错误 token | `UnknownToken`；审计 `auth.machine` failed；`AuthStats` 全局与 `<unknown>` 计数 +1 |
| H3 | 重放 | 同 nonce 二次 hello → `ReplayNonce` 拒绝（§4.8 向量 8/12 场景） |
| H4 | 过期 nonce | 登记后推进时钟 30s → 过期路径（新 nonce 语义） |
| H5 | nonce 编码 | 非 base64 / 非 32B → `BadNonceEncoding` |
| H6 | 错误角色 | client token 提交 machine/hello → `RoleMismatch`；machine token 提交 client 认证 → `RoleMismatch` |
| H7 | 版本绑定 | 机侧用错误版本字符串重算 MAC → `verify_mac` 失败（字节级，§4.8 向量 12） |
| H8 | 未知身份 | 未登记 token → `UnknownToken` + 计数 |
| H9 | 失败计数 | 每种失败路径：`AuthStats` 按 token_id 递增；吊销 token 与未知 token 分开计数 |
| H10 | 关闭码 | 认证失败映射 `CLOSE_CONFIG_FATAL`(4502)（常量断言，实际关闭由 F5 执行） |

### 6.5 集成挂靠（不在本 feature，标注入册）

f1 §12.2 向量 8（双向认证：重放旧握手/错误角色/过期 challenge/未知身份 → 拒绝关闭 + 审计计数）、12（HMAC 字节级向量）的 server 侧部分由本模块承担，gateway 接通后跑 e2e。

---

## 7. 依赖变更（由主管统一处理，本 feature 不碰 Cargo.toml）

| 依赖 | 用途 | 说明 |
|------|------|------|
| `toml = { version = "0.8", features = ["serde"] }`（workspace + server） | config.toml / tokens.toml 解析 | workspace 现无 toml |
| `subtle = "2"`（workspace + server，dev 亦用） | token 常量时间比较（§4.3.3） | 若不批，退化为手写 XOR-fold 常量时间比较（本模块内私有实现，无公共 API 依赖） |

其余依赖（tokio/serde/tracing/clap/chrono/uuid/dirs-next/rand/sha2/hmac/hkdf/base64/tempfile/serial_test）已预填，无需变更。

---

## 8. 与相邻 feature 的边界与待确认项

1. **`machine/hello` 无显式协议版本字段**：MAC 输入版本绑定靠共享常量天然成立；若未来版本协商需要显式字段，需 f1 在 hello 增字段（f1-proto §14 待确认项同族），server 侧在 `authenticate_machine` 步骤 3 前加「hello.version == PROTOCOL_VERSION」校验即可（挂钩点已留）。
2. **token 文件锁**（CLI 与运行中 server 并发写竞态）：本设计接受为已知限制（§4.3.2），排期时可升级为 `fs2` 独占锁——优先级低，先不动。
3. **非回环拒绝关闭码**（1011 vs 4502）：本设计取 1011（§5 决策），F5 gateway 实现时如有异议可回议，接口不变。
4. **`auth_response` 的 M1 白名单归属**：f1 已裁决（whitelist.rs 注：§4.8 machine 表未列但按 §9.2 处理），无 F2 动作。
5. **audit helper 归属**：本设计放 `auth/audit.rs`；F5 gateway 复用同一 helper 记录 conn.open/close。若后续 feature 需要全局限流/汇总，可上移到共享模块（supervisor 裁决）。
