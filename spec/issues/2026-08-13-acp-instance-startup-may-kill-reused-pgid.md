# acp-instance 启动清理可能杀死复用 PGID 的无关进程

**状态**：Verified  
**优先级**：高  
**创建日期**：2026-08-13

## 问题描述

当 `acp-instance` 用包含旧 `watermark.json` 的 data-dir 启动时，它会对每个记录的 PGID 直接发送 `SIGKILL`。PGID 是可复用的短生命周内核标识，仅凭数值不能证明目标仍是原 ACP 进程组。复制 data-dir 做验证或长时间停机后重启会放大误杀无关进程的风险；期望只在可验证进程组所有权时清理。

## 风险描述

- 旧 watermark 仅保存 `pgid`，没有进程出生时间、data-dir 身份或所有权指纹。
- 启动路径无条件调用 `kill(-pgid, SIGKILL)`，且无论发送是否成功都记录为已清理。
- 当 data-dir 是另一个目录的副本时，它不应继承原目录的进程管理权。
- 即使没有攻击者，PID/PGID 自然复用也可以触发误杀；这是本地可用性与数据安全边界。

## 影响范围

- 同一用户下已复用相同 PGID 的任意进程组。
- 开发/验证时复制的 instance data-dir。
- daemon 崩溃后较晚重启的生产运行。

## 涉及文件

- `acp-hub/instance/src/hub.rs` — 启动清理与进程派生编排。
- `acp-hub/instance/src/buffer.rs` — watermark 持久化 schema。
- `acp-hub/instance/src/child.rs` — 进程组创建、身份读取与信号发送。
- `acp-hub/instance/src/buffer_test.rs` — 旧/新 schema 兼容。
- `acp-hub/instance/src/hub_test.rs` — 启动清理的所有权决策。
- `acp-hub/dev.sh` — 开发环境 instance data-dir 隔离。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-13 | — | Open | agent | 运行隔离验证中发现 watermark 副本会触发无条件 PGID 清理 |
| 2026-08-13 | Open | Fixed | agent | 改为 data-dir 身份 + leader 出生指纹的 fail-closed 所有权校验 |
| 2026-08-13 | Fixed | Verified | agent | 51 个 instance 库测试、7 个 child 集成测试、真实崩溃重启与 workspace clippy 全部通过 |

## 修复记录

### 修复 #1（2026-08-13）

- **操作人**：agent
- **用户原意**：交付一套在开发、崩溃恢复与数据副本验证中都值得信任的 acp-hub 工程。
- **修复内容**：watermark 增加向后兼容的 data-dir `(dev,ino)` 和 leader 出生指纹；macOS 用 `proc_pidinfo`，Linux 用 boot id + `/proc/<pid>/stat` start ticks；启动仅在目录与进程身份双重匹配时发信号，否则 fail closed。增加同 data-dir daemon owner lock，清理权一次性消费但保留 epoch/lastSeq。移除 server 集成测试 harness 中第二套裸 PGID 清理。
- **涉及 commit**：未提交
- **验证状态**：已验证

### 验证 #1（2026-08-13）—— Verified

- 精确所有权匹配的真实进程组会被清理；目录身份不同、旧格式、出生指纹不同和 leader 已消失均不会向未证明目标发信号。
- 同一 data-dir 的第二个 daemon 启动失败，owner lock 与 watermark 均为 `0600`。
- macOS 真实 `proc_pidinfo` 路径和 Linux `/proc stat` 纯解析器均有回归；Linux 夹具覆盖进程名中的空格与右括号。
- server+instance T-06 真实 `SIGKILL` 崩溃重启路径通过，epoch/Registry 一致性保持。
