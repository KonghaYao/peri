//! Smart Compact（未实现）
//!
//! 计划使用 LLM 决策保留消息 id + 未选中标 `excluded` + 追加 system-reminder。
//! 当前分支逻辑在 mod.rs::run_compact 中降级为 Micro Compact。
//!
//! 注意：CompactStarted/MessagesCompacted 事件已支持 `CompactStrategy::Smart` 枚举值，
//! 但运行时尚无路径产生此策略——实现后移除 mod.rs 中的降级逻辑即可。

// TODO: 实现 Smart Compact 策略
