# acp-hub 术语集合（定制版）

> 状态：v1.0 定稿（2026-08-09）
> 定位：本仓库**唯一权威术语表**。代码标识符、ws 协议帧、磁盘持久化格式、
> 文档一律以本表为准。旧术语仅在「历史数据/迁移说明」中出现。
> 原则：**session 一词特指 ACP 进程内的会话**，其余原 session 概念全部更名。

---

## 1. 实体定义

| 术语 | 定义 | 身份标识 | 取代旧术语 |
| --- | --- | --- | --- |
| **chat**（对话） | server 侧对话容器：一次用户对话的持久化身份（UUID），面板左侧列表条目；对应 `chats/{chat_id}/` 目录 + 双 Doc + registry 摘要 | `chat_id`（UUID，server 生成） | session（hub 侧）/ `session_id` |
| **instance**（实例） | 一个 ws 连接所注册的 machine：运行 ACP 进程的宿主 daemon，outbound ws 连 server，接收 spawn/kill 指令 | `instance_id` | machine / `machine_id` |
| **session**（会话） | **ACP 进程内的会话**：agent 进程收到 `session/new` 后建立，`session/prompt` 等 JSON-RPC 方法的作用域 | `session_id`（agent 返回） | acp_session_id（所指实体，名不变） |
| turn（轮） | 一轮 prompt → 回复 | `turn_id` | 不变 |
| entry / block / tool_call | 消息条目 / 内容块 / 工具调用 | `entry_id` / `block_id` | 不变 |

**归属关系**：一个 chat → 归属 1 个 instance（`instance_id` 字段，create 时指定，缺省 `local`）+ 绑定 1 个 ACP session（binding：`session_id → chat_id`）。instance 可同时承载多个 chat（心跳 `alive_sessions`），ACP 一进程一会话。

## 2. 代码/协议/存储映射表（旧 → 新）

### 2.1 核心类型与变量

| 旧 | 新 |
| --- | --- |
| `SessionRegistry` / `SessionEntry` / `SessionState` | `ChatRegistry` / `ChatRecord` / `ChatState` |
| `session_id`（UUID，server 生成） | `chat_id` |
| `acp_session_id`（变量/字段名） | `session_id`（ACP 会话；wire 上 ACP `sessionId` 字段不变） |
| `SessionSummary`（registry 摘要） | `ChatSummary` |
| `MachineRegistry` / `MachineEntry` / `MachineState` | `InstanceRegistry` / `InstanceRecord` / `InstanceState` |
| `MachineView` / `machine_id` / `session_count` | `InstanceView` / `instance_id` / `chat_count` |
| `MachineConfig` / `MachineHello` / `MachineHeartbeat` / `MachineSpawn` / `MachineKill` / `MachineSpawnAck` / `MachineKillAck` / `MachineForwardAck` / `MachineProcessExit` | 对应 `Instance*` 前缀 |
| `TokenRole::Machine` / wire `"machine"` | `TokenRole::Instance` / wire `"instance"` |
| `DEFAULT_MACHINE_ID = "local"` | `DEFAULT_INSTANCE_ID = "local"` |

### 2.2 ws 协议帧（instance 侧 → server）

| 旧 | 新 |
| --- | --- |
| `machine/hello` / `machine/heartbeat` / `machine/spawn` / `machine/spawn_ack` / `machine/kill` / `machine/kill_ack` / `machine/event` / `machine/process_exit` / `machine/buffer_sync` / `machine/forward` / `machine/forward_ack` / `machine/unknown` | 全部 `instance/*` 同名方法 |
| ws 连接路径 `/machine` | `/instance` |

### 2.3 磁盘持久化格式

| 旧 | 新 |
| --- | --- |
| `sessions/{sid}/` 目录 | `chats/{chat_id}/` 目录 |
| `DocId::session` → `session:{sid}` | `DocId::session` → `session:{chat_id}`（控制状态 Doc） |
| `DocId::chat` → `chat:{sid}` | `chat:{chat_id}`（**前缀不变**，消息时间线 Doc） |
| updates.log doc id 字节：`0=chat, 1=session` | `0=chat, 1=session` |
| registry.log `sessions` map | `chats` map |
| registry.log `machines` map / `machine_id` 字段 | `instances` map / `instance_id` 字段 |
| `machine.token` 文件 | `instance.token` |
| `tokens.toml` 中 `role = "machine"` | `role = "instance"` |

### 2.4 工程/部署

| 旧 | 新 |
| --- | --- |
| `machine/` crate、`acp-machine` 二进制 | `instance/` crate、`acp-instance` 二进制 |
| `MACHINE_TOKEN_FILE` / `MACHINE_LOG` 环境变量（dev.sh） | `INSTANCE_TOKEN_FILE` / `INSTANCE_LOG` |
| 数据目录 `~/.local/share/acp-hub/machine/` | `~/.local/share/acp-hub/instance/` |
| 文档模块名 `f6-machine.md` | `f6-instance.md` |

## 3. 保持不变（session 一词的合法使用域）

以下**保留原词**——它们本就属于 ACP 会话语义，符合「session 特指 ACP 内会话」：

- ACP JSON-RPC 方法：`session/new`、`session/prompt`、`session/cancel`、`session/update`、`session/create`、`session/close`
- 心跳 `alive_sessions`（instance 上报其管理的 ACP 会话清单）
- binding 概念（`session_id → chat_id` 映射，§6.1 规则 5：acp 会话 id 只用于协议投递）
- `turn` / `entry` / `block` / `gap` / `permission` / `outbox` / `epoch` / `seq` 等既有术语

## 4. 命名约定

- `chat_id`：server 容器 UUID（原 session_id）；`chats/` 目录、`chat:` Doc 前缀
- `session_id`：ACP 进程内会话（原 acp_session_id 所指）；仅 ACP 侧代码允许
- `instance_id`：ws 注册的 machine（原 machine_id）
- UI 中文文案：会话列表 → **对话**列表；机器 → **实例**
