//! Localized relative timestamps for thread rows.

use crate::i18n;
use crate::kit::atoms::ThreadSummary;
use chrono::{DateTime, Utc};

/// 会话更新时间的相对描述；缺失更新时间用 `common-na`。
pub(super) fn relative_time_label(entry: &ThreadSummary) -> String {
    entry
        .updated_at
        .map(format_relative_time)
        .unwrap_or_else(|| i18n::tr("common-na"))
}

/// 相对时间文案（刚刚 / N 分钟前 / N 小时前 / N 天前）——取自
/// `thread-browser-time-*`，两种语言各自成句。
pub(super) fn format_relative_time(updated: DateTime<Utc>) -> String {
    let now: DateTime<Utc> = peri_time::now_wall().into();
    let minutes = now.signed_duration_since(updated).num_minutes().max(0);
    let (key, count) = match minutes {
        0 => return i18n::tr("thread-browser-time-just-now"),
        m if m < 60 => ("thread-browser-time-minutes", m),
        m if m < 60 * 24 => ("thread-browser-time-hours", m / 60),
        m => ("thread-browser-time-days", m / (60 * 24)),
    };
    let suffix = if count == 1 { "" } else { "s" };
    i18n::tr_args(
        key,
        &[
            ("count".into(), count.into()),
            // 英文复数后缀；中文 message 不引用该参数。
            ("suffix".into(), suffix.into()),
        ],
    )
}
