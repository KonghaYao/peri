use peri_agent::messages::{BaseMessage, ContentBlock, MessageContent};

use crate::{app::App, command::Command};

pub struct GcCommand;

impl Command for GcCommand {
    fn name(&self) -> &str {
        "gc"
    }

    fn description(&self, _lc: &crate::i18n::LcRegistry) -> String {
        "手动触发内存回收并显示 RSS 变化和数据结构诊断".to_string()
    }

    fn aliases(&self) -> Vec<&str> {
        vec![]
    }

    fn execute(&self, app: &mut App, _args: &str) {
        let stats_before = crate::alloc_config::query_stats();
        let os_rss_before = crate::alloc_config::os_rss_mb();

        // ── 诊断：各数据结构大小 ──
        let active = app.active();
        let origin_count = active.agent.origin_messages.len();
        let origin_bytes = estimate_messages_heap(&active.agent.origin_messages);
        let (completed_count, completed_bytes) = active.messages.pipeline.completed_stats();
        let vm_count = active.messages.view_messages.len();

        // ── Markdown/Diff 缓存诊断 ──
        let md_cache_len = peri_widgets::markdown::cache::MarkdownCache::global().len();
        let md_cache_cap = 256usize; // CACHE_CAPACITY

        let mut lines = Vec::new();

        crate::alloc_config::alloc_collect();

        let stats_after = crate::alloc_config::query_stats();
        let os_rss_after = crate::alloc_config::os_rss_mb();

        // ── RSS 汇总 ──
        match (stats_before, stats_after) {
            (Some(before), Some(after)) => {
                let delta = before.current_rss as isize - after.current_rss as isize;
                let sign = if delta >= 0 { "+" } else { "" };
                lines.push(format!(
                    "RSS: {} → {} ({sign}{})",
                    fmt_bytes(before.current_rss),
                    fmt_bytes(after.current_rss),
                    fmt_bytes(delta.unsigned_abs()),
                ));
                let alloc_delta = after.current_allocated as isize - after.current_rss as isize;
                if alloc_delta != 0 {
                    lines.push(format!(
                        "jemalloc allocated: {} (与 RSS 差 {})",
                        fmt_bytes(after.current_allocated),
                        fmt_bytes(alloc_delta.unsigned_abs()),
                    ));
                }
            }
            _ => lines.push("RSS: 不可用（Windows 不支持）".to_string()),
        }

        if let (Some(before), Some(after)) = (os_rss_before, os_rss_after) {
            let delta = before as isize - after as isize;
            let sign = if delta >= 0 { "+" } else { "" };
            lines.push(format!(
                "OS RSS: {} → {} ({sign}{})",
                fmt_mb(before),
                fmt_mb(after),
                fmt_mb_from_usize(delta.unsigned_abs()),
            ));
        }

        // ── 数据结构诊断 ──
        lines.push(String::new());
        lines.push("── 数据结构诊断 ──".to_string());
        lines.push(format!(
            "origin_messages:  {} 条, ~{}",
            origin_count,
            fmt_bytes(origin_bytes),
        ));
        lines.push(format!(
            "pipeline.completed: {} 条, ~{}",
            completed_count,
            fmt_bytes(completed_bytes),
        ));
        lines.push(format!("view_messages:     {} 条 VM", vm_count,));

        // 检查重复
        if origin_count > 0 && completed_count > 0 {
            let overlap = if origin_count == completed_count {
                "完全相同 ⚠️"
            } else {
                "部分重叠"
            };
            lines.push(format!(
                "origin vs completed: {} ({}/{} 条)",
                overlap, origin_count, completed_count,
            ));
        }

        // ── 渲染缓存诊断 ──
        lines.push(String::new());
        lines.push("── 渲染缓存 ──".to_string());
        lines.push(format!("markdown_cache: {md_cache_len}/{md_cache_cap} 条"));

        // ── jemalloc breakdown（关键：allocated vs active vs resident）──
        if let Some(bd) = crate::alloc_config::query_breakdown() {
            lines.push(String::new());
            lines.push("── jemalloc 明细 ──".to_string());
            lines.push(format!(
                "allocated: {} (应用实际分配)",
                fmt_bytes(bd.allocated)
            ));
            lines.push(format!("active:    {} (活跃页)", fmt_bytes(bd.active)));
            lines.push(format!("resident:  {} (物理驻留)", fmt_bytes(bd.resident)));
            lines.push(format!(
                "metadata:  {} (jemalloc 元数据)",
                fmt_bytes(bd.metadata)
            ));
            lines.push(format!("mapped:    {} (映射)", fmt_bytes(bd.mapped)));
            lines.push(format!(
                "retained:  {} (保留未归还 OS)",
                fmt_bytes(bd.retained)
            ));
            // 关键指标
            let frag = bd.active.saturating_sub(bd.allocated);
            let waste = bd.resident.saturating_sub(bd.active);
            lines.push(format!(
                "碎片: active-allocated={} | resident-active={}",
                fmt_bytes(frag),
                fmt_bytes(waste),
            ));
            // OS RSS vs jemalloc resident
            if let Some(ref s) = stats_after {
                let os_gap = s.current_rss.saturating_sub(bd.resident);
                lines.push(format!(
                    "OS RSS({}) - jemalloc resident({}) = {}",
                    fmt_bytes(s.current_rss),
                    fmt_bytes(bd.resident),
                    fmt_bytes(os_gap),
                ));
            }
        }

        // ── jemalloc 全量 stats → tracing ──
        if cfg!(not(target_os = "windows")) {
            lines.push(String::new());
            lines.push("── jemalloc 全量统计（见日志）──".to_string());
            tracing::info!("=== /gc jemalloc full stats dump ===");
            crate::alloc_config::dump_stats();
            tracing::info!("=== /gc jemalloc full stats end ===");
        }

        // ── 已知 vs 未识别 ──
        if let Some(bd) = crate::alloc_config::query_breakdown() {
            let known_bytes = origin_bytes + completed_bytes;
            let gap = bd.allocated.saturating_sub(known_bytes);
            lines.push(format!(
                "消息估算: {} | allocated 内未识别: {}",
                fmt_bytes(known_bytes),
                fmt_bytes(gap),
            ));
        }

        // UI 反馈：走 ephemeral SystemNote VM 路径，不污染 BaseMessage[] / Prompt Cache。
        app.push_system_note(lines.join("\n"));
    }
}

// ── 内存估算 ──────────────────────────────────────────────────────────────────

/// 估算 BaseMessage slice 的堆内存占用（字节）
pub fn estimate_messages_heap(msgs: &[BaseMessage]) -> usize {
    let mut total = 0usize;
    for msg in msgs {
        total += estimate_message_content_heap(msg.message_content());
        // tool_calls
        if let BaseMessage::Ai { tool_calls, .. } = msg {
            for tc in tool_calls {
                total += tc.id.len() + tc.name.len() + estimate_json_heap(&tc.arguments);
            }
        }
        // tool_call_id
        if let BaseMessage::Tool { tool_call_id, .. } = msg {
            total += tool_call_id.len();
        }
        total += std::mem::size_of::<BaseMessage>(); // enum 本身
    }
    total
}

fn estimate_message_content_heap(mc: &MessageContent) -> usize {
    match mc {
        MessageContent::Text(s) => s.len(),
        MessageContent::Blocks(blocks) => {
            let mut size = blocks.capacity() * std::mem::size_of::<ContentBlock>();
            for b in blocks {
                size += estimate_content_block_heap(b);
            }
            size
        }
        MessageContent::Raw(vals) => {
            vals.capacity() * std::mem::size_of::<serde_json::Value>()
                + vals.iter().map(estimate_json_heap).sum::<usize>()
        }
    }
}

fn estimate_content_block_heap(b: &ContentBlock) -> usize {
    match b {
        ContentBlock::Text { text } => text.len(),
        ContentBlock::Image { source } => match source {
            peri_agent::messages::ImageSource::Base64 { media_type, data } => {
                media_type.len() + data.len()
            }
            peri_agent::messages::ImageSource::Url { url } => url.len(),
        },
        ContentBlock::Document { source, title } => {
            let src = match source {
                peri_agent::messages::DocumentSource::Base64 { media_type, data } => {
                    media_type.len() + data.len()
                }
                peri_agent::messages::DocumentSource::Url { url } => url.len(),
                peri_agent::messages::DocumentSource::Text { text } => text.len(),
            };
            src + title.as_ref().map_or(0, |t| t.len())
        }
        ContentBlock::ToolUse { id, name, input } => {
            id.len() + name.len() + estimate_json_heap(input)
        }
        ContentBlock::ToolResult {
            content,
            tool_use_id,
            ..
        } => {
            tool_use_id.len()
                + content.capacity() * std::mem::size_of::<ContentBlock>()
                + content
                    .iter()
                    .map(estimate_content_block_heap)
                    .sum::<usize>()
        }
        ContentBlock::Reasoning { text, signature } => {
            text.len() + signature.as_ref().map_or(0, |s| s.len())
        }
        ContentBlock::Unknown(v) => estimate_json_heap(v),
    }
}

fn estimate_json_heap(v: &serde_json::Value) -> usize {
    match v {
        serde_json::Value::String(s) => s.len(),
        serde_json::Value::Object(map) => map
            .iter()
            .map(|(k, v)| k.len() + estimate_json_heap(v))
            .sum(),
        serde_json::Value::Array(arr) => {
            arr.capacity() * std::mem::size_of::<serde_json::Value>()
                + arr.iter().map(estimate_json_heap).sum::<usize>()
        }
        _ => 0,
    }
}

// ── 格式化 ────────────────────────────────────────────────────────────────────

fn fmt_bytes(bytes: usize) -> String {
    const KB: usize = 1024;
    const MB: usize = 1024 * KB;
    if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.0} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

fn fmt_mb(mb: u64) -> String {
    if mb >= 1024 {
        format!("{:.1} GB", mb as f64 / 1024.0)
    } else {
        format!("{mb} MB")
    }
}

fn fmt_mb_from_usize(mb: usize) -> String {
    fmt_mb(mb as u64)
}
