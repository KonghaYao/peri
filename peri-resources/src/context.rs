//! Resources context — 外部系统访问通道的唯一实例化入口。
//!
//! 本迁移点先落 context 形状 + 唯一实例化入口（TUI 启动处）；
//! Controller/Runtime 建成后消费方随 L2/L3/L5 跟进接入（属预期过渡态，
//! 接口按目标态设计，避免二次返工）。

mod resources;

#[cfg(test)]
use resources::SessionStoreDeployment;

#[path = "context/sqlite.rs"]
mod sqlite;

pub use resources::Resources;

pub use resources::SessionStoreShutdownOwner;

pub use resources::StoreOpenFailure;

pub use resources::classify_open_failure;

#[cfg(test)]
#[path = "context_test.rs"]
mod tests;
