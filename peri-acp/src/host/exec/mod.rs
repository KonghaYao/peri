//! 3.0 批 2：Agent 层执行核心的 ACP 侧过渡宿主。
//!
//! 本模块承载深绑 Agent 层执行类型的代码（执行本体，尚未迁出）：
//! - [`executor`]：`run_session_loop` 编排（EventSink / Langfuse / SessionManager）
//! - [`executor_helpers`]：v2 执行子流程（build_and_execute_agent_v2 / pump / collect）
//! - [`stage_builder`]：StageContext 装配（原 `agent::builder`）
//! - [`forwarder`]：EventBus v2 → ExecutorEvent 转发（Langfuse 旁路消费）
//! - [`workflow_agent`]：workflow engine agent() 回调执行器
//! - [`bg`]：/bg 命令（SubAgent 发起，经装配注入的 spawner）
//! - [`compact_pipeline`]：/compact 命令的 v2 执行体
//!
//! 依赖声明：本模块对 Agent 层类型使用全路径引用（`peri_agent::…`）——
//! 过渡宿主豁免至 L5（`spec/issues/2026-08-05-3.0-acp-events-session-batch2.md`
//! 豁免清单：执行本体随 L5 executor 拆分物理迁入 peri-agent，届时引用消失）；
//! 事件发射/执行发起已统一经 Controller
//! （`Controller::publish_event` / `Controller::run_session`）。
//!
//! 归位说明（L5：executor 拆分）：上述执行本体随 L5 物理迁入 peri-agent
//! session 工厂（§2 聚合根 / §21 未决项 executor 拆分），本模块是过渡宿主；
//! 协议化入口（`crate::session::executor` 薄壳）与调用方路径保持不变。

pub mod bg;
pub mod compact_pipeline;
pub mod executor;
pub mod forwarder;
pub mod prompt_handle;
pub mod stage_builder;
pub mod workflow_agent;
