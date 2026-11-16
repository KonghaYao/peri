//! Full Compact 实现
//!
//! 完整流程：
//! 1. 从包含 canonical reminder 的可见模型上下文派生摘要请求
//! 2. LLM 生成结构化摘要；未完成时在同一次 Full 内用**有界恢复循环**补齐
//! 3. 后处理摘要
//! 4. 快照内自有的普通历史和 reminder 标 excluded（保留 System / ancestor），同一批 flags
//!    里写下取代关系：被归档条目记下第 5 步那条摘要的 id
//! 5. 追加 Human 摘要消息（带 CONTINUATION_HINT，wrap 在 system-reminder 标签中），id 在
//!    提交前先分配，供第 4 步引用
//!
//! 历史工具调用及结果留存在 transcript 中；不会从计算实例本机重新读取 Workspace 文件。
//!
//! 恢复循环只把已生成的正文片段回填进临时请求；未完成摘要、错误字符串与
//! 隐藏思考都不进入正式 transcript，也不执行摘要响应里的工具调用。只有正常
//! 通过校验的非空摘要才经现有原子提交替换历史。

mod full_compact;

#[path = "summary.rs"]
mod summary;
#[cfg(test)]
use full_compact::SUMMARY_RECOVERY_PROMPT;

#[cfg(test)]
use full_compact::SUMMARY_FINAL_PROMPT;

#[cfg(test)]
pub(super) use full_compact::full_compact_inner;

pub(super) use full_compact::full_compact_inner_with_protected;

#[cfg(test)]
use full_compact::summary_instruction;

#[cfg(test)]
use full_compact::build_summary_message;

/// 提取旧格式压缩文件/Skill 元信息（事实源 peri-acp-types::compact）
pub use peri_acp_types::compact::{extract_file_info, extract_skill_names};

#[cfg(test)]
#[path = "full_test.rs"]
mod tests;

#[cfg(test)]
#[path = "full_protected_result_test.rs"]
mod protected_result_tests;

#[cfg(test)]
#[path = "full_report_test.rs"]
mod report_tests;

#[cfg(test)]
#[path = "full_continuation_test.rs"]
mod continuation_tests;
