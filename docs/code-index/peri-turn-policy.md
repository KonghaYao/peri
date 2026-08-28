# peri-turn-policy 代码索引

> 速查表：把「我想做什么」映射到文件。细节以代码和契约测试为准。更新：2026-08-29
> 依据：`docs/design/wasi.md`、`spec/issues/2026-08-28-wasi-p2-node-validation.md`、源码（本 crate 无 CLAUDE.md）

## 架构速览

- 定位：native Peri 与 WASI Component 共用的 deterministic turn-policy kernel；`#![no_std]`、无第三方依赖、`publish = false`。
- 数据流：native `MessageContent::is_empty` / `determine_compact_action` 与 `peri-wasi` WIT adapter → 本 crate 纯函数 → 调用方映射回各自类型。
- 稳定不变量：text 判空不 trim；compact selector 不校验、不 clamp 浮点输入，保留 native Rust 比较语义和 Smart 分支；WIT 边界的输入校验、错误类型与 Smart 收窄不属于本 crate。
- 非目标：不包含 Agent loop、ACP 类型、runtime、I/O、配置默认值或 capability；不是独立产品 API。

## 速查表

| 我想做什么 | 主文件 | 入口/关键函数 | 关键逻辑 |
| --- | --- | --- | --- |
| 改消息 content 判空策略 | `src/content.rs` | `MessageContentShape`；`is_message_content_empty` | `Text(text)` 仅以 `str::is_empty` 判空，纯空白不是空；`Blocks(len)` / `Raw(len)` 只看长度是否为 0。native 投影在 `peri-acp-types/src/messages/content.rs`，WIT 投影在 `peri-wasi/src/lib.rs` |
| 改 compact action 选择 | `src/compact.rs` | `CompactAction`；`select_compact_action(budget, micro_threshold, smart_enabled)` | `budget >= micro_threshold` 时按 Smart 开关返回 Smart/Micro，否则 Skip；函数刻意不做 finite/range 校验，以保持 native NaN/±Infinity 比较语义。WIT adapter 在调用前执行自己的 `[0,1]` 验证 |
| 改共享 kernel 的回归矩阵 | `src/lib_test.rs` | content shape tests；threshold/native non-finite tests | 覆盖空 text、空白 text、blocks/raw 长度、阈值下方/相等、Smart 和 NaN/±Infinity；当前共 8 项单测 |
| 改公共导出面 | `src/lib.rs` | `pub use compact::{...}`；`pub use content::{...}` | crate root 只 re-export 两组纯类型/函数，并保持 `#![no_std]`；新增依赖或 I/O 会扩大 WASI closure，必须先核对 probe 边界 |
| 改 crate 注册或消费方 | `Cargo.toml` + workspace 根 `Cargo.toml`；消费方 `peri-acp-types/Cargo.toml`、`peri-agent/Cargo.toml`、`peri-wasi/Cargo.toml` | workspace member/path dependency | crate 不发布；三类消费方必须共享同一实现，禁止在 WIT adapter 复制一份 policy |

## 跨模块契约

- Content：`peri-acp-types::MessageContent::is_empty` 负责把完整消息类型投影成 `MessageContentShape`；本 crate 不依赖 ACP 类型。`peri-agent` 的 keepgoing 继续通过 `MessageContent::is_empty` 消费该语义（ARC-KEEPGOING-001）。
- Compact：`peri-agent::agent::compact_v2::determine_compact_action` 保留原函数路径与 `CompactAction` 可见路径，只把纯选择委托给本 crate；`CompactConfig` 仍是 `peri-acp-types` 的配置事实源。
- WASI：`peri-wasi` 负责把 WIT DTO 映射到本 crate，并在 Component boundary 验证有限值/范围、禁用 Smart；kernel 不感知 WIT 或 Node。
- Capability：最终 path closure 应为 `peri-wasi → peri-turn-policy`；任何 native runtime、网络、存储、进程或文件系统依赖都违反本探针边界。
