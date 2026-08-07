//! peri-workflow 作为 resource 实现接入（伞形 PRD 决策 20：既有 crate 归位）。
//!
//! 门面镜像：Workflow 编排能力（runner / registry / protocol / tool）以本模块为
//! 唯一引用入口，消费方（Middleware 等）经 Resources 门面使用、不直接依赖
//! peri-workflow crate。实例化与持有（runner/registry 生命周期）随装配归位
//! （L5）后收口至 Resources context，本模块仅为类型/能力出口，不解释业务语义。

pub use peri_workflow::error;
pub use peri_workflow::journal;
pub use peri_workflow::progress;
pub use peri_workflow::protocol;
pub use peri_workflow::registry;
pub use peri_workflow::rpc;
pub use peri_workflow::runner;
pub use peri_workflow::tool;
