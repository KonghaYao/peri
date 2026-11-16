//! ratatui-kit SetupWizard —— 完整交互式配置向导。
//!
//! 配置向导：Language → Choose → Preset/Form/ChatGpt 引导 → Done。
//! 状态存储在 `SETUP_WIZARD` atom 中，显隐由 `WIZARD_ACTIVE` 控制。

mod handler;
mod render;

#[expect(
    clippy::module_inception,
    reason = "The style guide keeps the facade declarative and names the implementation leaf after its primary type."
)]
mod setup_wizard;
use self::setup_wizard as wizard;
pub use wizard::*;
