use crate::i18n;
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderCategory, ReminderDelivery, ReminderFilter, ReminderSeverity,
    SystemReminder,
};
use std::sync::atomic::{AtomicU64, Ordering};

use super::fold::{FoldState, fold_state_code};
use super::hash::{tui_hash_combine, tui_hash_str};

static NEXT_REMINDER_ID: AtomicU64 = AtomicU64::new(1);

fn next_reminder_id() -> u64 {
    NEXT_REMINDER_ID.fetch_add(1, Ordering::Relaxed)
}

/// 独立的 canonical/legacy reminder 展示模型，不附着用户气泡。
#[derive(Debug, Clone)]
pub struct TuiSystemReminder {
    pub wire: Option<SystemReminder>,
    pub summary: String,
    pub body: String,
    pub category: String,
    pub source: String,
    pub severity: ReminderSeverity,
    pub required: bool,
    pub legacy: bool,
    /// 仅用于当前 TUI session 的折叠身份，不进入 wire 或内容哈希。
    pub reminder_id: u64,
    pub fold: FoldState,
    pub content_hash: u64,
}

/// Local, non-serializable proof that a structured reminder passed the live ACP
/// session ownership gate. Wire decoding can never construct this wrapper.
#[derive(Debug)]
pub(crate) struct DisplayTrusted(SystemReminder);

impl DisplayTrusted {
    pub(crate) fn after_current_session_gate(reminder: SystemReminder) -> Self {
        Self(reminder)
    }
}

impl TuiSystemReminder {
    /// Untrusted wire/DTO ingress. Required is always downgraded before display policy.
    pub fn from_wire(mut reminder: SystemReminder) -> Option<Self> {
        if reminder.delivery == ReminderDelivery::Required {
            reminder.delivery = ReminderDelivery::Configurable;
        }
        Self::from_display_ingress(reminder, false)
    }

    /// Structured ingress carrying local proof from the current-session ownership gate.
    pub(crate) fn from_trusted_structured(trusted: DisplayTrusted) -> Option<Self> {
        Self::from_display_ingress(trusted.0, true)
    }

    fn from_display_ingress(reminder: SystemReminder, trusted: bool) -> Option<Self> {
        reminder.validate().ok()?;
        let filter = ReminderFilter {
            minimum_severity: if reminder.category == ReminderCategory::Security {
                Some(ReminderSeverity::Warning)
            } else {
                None
            },
            ..Default::default()
        };
        if !reminder.audiences.contains(ReminderAudience::Tui)
            || reminder.delivery == ReminderDelivery::DiagnosticOnly
            || filter
                .minimum_severity
                .is_some_and(|minimum| reminder.severity < minimum)
        {
            return None;
        }
        let summary = reminder.summary.clone().unwrap_or_else(|| {
            reminder
                .body
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or_default()
                .to_string()
        });
        let mut vm = Self {
            category: format!("{:?}", reminder.category),
            source: reminder.source.0.clone(),
            severity: reminder.severity,
            required: trusted && reminder.delivery == ReminderDelivery::Required,
            body: reminder.body.clone(),
            summary,
            legacy: false,
            reminder_id: next_reminder_id(),
            fold: FoldState::Collapsed,
            content_hash: 0,
            wire: Some(reminder),
        };
        vm.recompute_hash();
        Some(vm)
    }

    pub fn legacy(text: String) -> Self {
        let mut vm = Self {
            summary: text.clone(),
            body: text,
            category: i18n::tr("reminder-system-reminder"),
            source: i18n::tr("reminder-legacy-source"),
            severity: ReminderSeverity::Info,
            required: false,
            legacy: true,
            reminder_id: next_reminder_id(),
            fold: FoldState::Collapsed,
            content_hash: 0,
            wire: None,
        };
        vm.recompute_hash();
        vm
    }

    pub fn recompute_hash(&mut self) {
        let mut h = tui_hash_str(&self.summary);
        h = tui_hash_combine(h, tui_hash_str(&self.body));
        h = tui_hash_combine(h, tui_hash_str(&self.category));
        h = tui_hash_combine(h, tui_hash_str(&self.source));
        h = tui_hash_combine(h, self.severity as u64);
        h = tui_hash_combine(h, u64::from(self.required));
        h = tui_hash_combine(h, u64::from(self.legacy));
        h = tui_hash_combine(h, fold_state_code(self.fold));
        self.content_hash = h;
    }
}

tui_impl_partial_eq!(TuiSystemReminder: wire, summary, body, category, source, severity, required, legacy, reminder_id, fold);

/// System-reminder 分类——10 种从 `<system-reminder>` 标签检测到的类型。
#[derive(Debug, Clone, PartialEq)]
pub enum ReminderType {
    /// Channel（微信/Slack/飞书等）来源消息
    ChannelMessage(String),
    /// Cron 定时任务注入
    CronReminder,
    /// 后台任务完成通知
    BgTaskCompleted,
    /// Fork 模式背景 Agent 注入
    ForkMode,
    /// 上下文压缩摘要
    ContextCompacted,
    /// CONTINUATION_HINT 系统提示
    ContinuationHint,
    /// 信任边界声明
    TrustBoundary,
    /// 工具相关系统提醒
    ToolReminder,
    /// 子 Agent 结果摘要
    SubagentResult,
    /// 未匹配分类的兜底类型
    GenericReminder,
}

impl ReminderType {
    /// 中文标签，用于缩略渲染第一行。
    /// 返回 `String` 而非 `&'static str` 是为了 `ChannelMessage` 的动态 source。
    pub fn label(&self) -> String {
        match self {
            ReminderType::ChannelMessage(source) => format!("Channel ({})", source),
            ReminderType::CronReminder => i18n::tr("reminder-cron-task"),
            ReminderType::BgTaskCompleted => i18n::tr("reminder-bg-task"),
            ReminderType::ForkMode => i18n::tr("reminder-fork-mode"),
            ReminderType::ContextCompacted => i18n::tr("reminder-context-compaction"),
            ReminderType::ContinuationHint => i18n::tr("reminder-system-prompt"),
            ReminderType::TrustBoundary => i18n::tr("reminder-trust-boundary"),
            ReminderType::ToolReminder => i18n::tr("reminder-tool-reminder"),
            ReminderType::SubagentResult => i18n::tr("reminder-subagent-result"),
            ReminderType::GenericReminder => i18n::tr("reminder-system-reminder"),
        }
    }
}

/// 从 `<system-reminder>` 标签解析的信息——类型 + 摘要文本。
#[derive(Debug, Clone, PartialEq)]
pub struct ReminderInfo {
    pub reminder_type: ReminderType,
    /// 首非空行数据摘要，截断到 200 字符
    pub summary: String,
}

// ---------------------------------------------------------------------------
// system-reminder 检测函数
// ---------------------------------------------------------------------------

/// 提取 `<system-reminder>` 标签间的内部文本（首个匹配）。
fn extract_reminder_inner(text: &str) -> Option<String> {
    let tag = "<system-reminder>";
    let close_tag = "</system-reminder>";
    let start = text.find(tag)?;
    let content_start = start + tag.len();
    let end = text[content_start..].find(close_tag)?;
    Some(text[content_start..content_start + end].trim().to_string())
}

/// 从 reminder 内部文本提取 channel 来源短名。
fn extract_channel_source(inner: &str) -> Option<String> {
    // 匹配 plugin:name:name 格式
    if let Some(plugin_pos) = inner.find("plugin:") {
        let after = &inner[plugin_pos + "plugin:".len()..];
        if let Some(colon_pos) = after.find(':') {
            let raw = &after[..colon_pos];
            // 映射到显示名
            let display = match raw {
                "weixin" | "wechat" => i18n::tr("channel-wechat"),
                "slack" => "Slack".to_string(),
                "feishu" => i18n::tr("channel-feishu"),
                "dingtalk" => i18n::tr("channel-dingtalk"),
                "telegram" => "Telegram".to_string(),
                other => other.to_string(),
            };
            return Some(display.to_string());
        }
    }

    // channel source 关键词直搜
    let lower = inner.to_lowercase();
    for (kw, display) in &[
        ("weixin", i18n::tr("channel-wechat")),
        ("wechat", i18n::tr("channel-wechat")),
        ("slack", "Slack".to_string()),
        ("feishu", i18n::tr("channel-feishu")),
        ("dingtalk", i18n::tr("channel-dingtalk")),
        ("telegram", "Telegram".to_string()),
    ] {
        if lower.contains(kw) {
            return Some(display.to_string());
        }
    }

    None
}

/// 按优先级分类 reminder 类型。
fn classify_reminder_type(inner: &str, _full_text: &str) -> ReminderType {
    if inner.contains("CONTINUATION_HINT") {
        return ReminderType::ContinuationHint;
    }
    if let Some(source) = extract_channel_source(inner) {
        return ReminderType::ChannelMessage(source);
    }
    let lower = inner.to_lowercase();
    if lower.contains("cron") || lower.contains("scheduled") {
        ReminderType::CronReminder
    } else if lower.contains("background") || lower.contains("bgtask") || inner.contains("后台") {
        ReminderType::BgTaskCompleted
    } else if lower.contains("fork") {
        ReminderType::ForkMode
    } else if lower.contains("compact") || inner.contains("压缩") {
        ReminderType::ContextCompacted
    } else if inner.contains("Trust boundary") || inner.contains("信任边界") {
        ReminderType::TrustBoundary
    } else if lower.contains("tool") || inner.contains("工具") {
        ReminderType::ToolReminder
    } else if lower.contains("subagent") || lower.contains("sub_agent") || inner.contains("子Agent")
    {
        ReminderType::SubagentResult
    } else {
        ReminderType::GenericReminder
    }
}

/// 从 reminder 内部文本提取摘要：首非空行，截断到 200 字符。
fn extract_summary(inner: &str) -> String {
    let first_line = inner.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let trimmed = first_line.trim();
    if trimmed.chars().count() <= 200 {
        trimmed.to_string()
    } else {
        let trunc: String = trimmed.chars().take(200).collect();
        format!("{}…", trunc)
    }
}

/// 公开入口：从用户消息文本中检测 `<system-reminder>` 标签。
/// 返回 `Some(ReminderInfo)` 若存在合法标签，否则 `None`。
pub fn detect_reminder(text: &str) -> Option<ReminderInfo> {
    let inner = extract_reminder_inner(text)?;
    let reminder_type = classify_reminder_type(&inner, text);
    let summary = extract_summary(&inner);
    Some(ReminderInfo {
        reminder_type,
        summary,
    })
}
