//! Setup Wizard —— 首次启动配置向导。
//!
//! 完整交互式向导流程：
//! 1. Language  → 选择界面语言（en / zh-CN）
//! 2. Choose    → 选择配置来源（手动输入 / 从 Claude Code 迁移 / ChatGPT 订阅 / 供应商预设）
//!    ChatGpt  → 订阅引导确认后交给 Login；首次配置继续使用 Model
//! 3. Preset    → 从内置预设选一家，预填端点与四档模型
//! 4. Form      → 多 Provider 列表浏览 + 编辑详情
//! 5. Done      → 确认并保存
//!
//! 状态通过 `atoms::SETUP_WIZARD` 管理，组件通过 `WIZARD_ACTIVE` 控制显隐。

mod provider_presets;
mod setup_wizard_state;

pub use provider_presets::{PROVIDER_PRESETS, ProviderPreset};
pub use setup_wizard_state::*;
