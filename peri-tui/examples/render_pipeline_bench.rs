//! ViewModels → Vec<Line> 端到端渲染管道基准。
//!
//! 模拟 TUI 真实流式场景：构造一段 ~200 条 ViewModel 的对话历史
//! （含 UserBubble / AssistantBubble / ToolCard / SubAgentGroup 等），
//! 然后对比两种渲染策略：
//!   - 全量重建：每次都渲染全部 200 条（对应 968992e0 坏态）
//!   - 增量重建：只渲染变化的尾部（对应修复后的好态，prefix_stable 跳过）
//!
//! 运行：`cargo run --release --example render_pipeline_bench -p peri-tui`

use std::sync::Arc;
use std::time::Instant;

use peri_tui::kit::markdown::parse_markdown;
use peri_tui::kit::tui_render_unit::{
    TuiAssistantBubble, TuiNoteLevel, TuiRenderUnit, TuiSystemNote, TuiToolCard, TuiUserBubble,
};
use ratatui_kit::prelude::Palette;

fn make_user_text(i: usize) -> String {
    format!(
        "用户消息 #{i}：这是带 **markdown** 的内容，包含 `inline code` 和 [链接](https://example.com)。\n\n- 项目 1\n- 项目 2\n\n```rust\nfn answer() -> i32 {{ 42 }}\n```"
    )
}

fn make_assistant_text(i: usize) -> String {
    format!(
        "AI 回复 #{i}：基于您的请求，我建议**这样做**：

## 详细说明

这是详细说明段落，包含 `代码片段` 和 [文档](https://example.com)。

### 子章节

- 第一项：详细描述
- 第二项：包含 `code`
- 第三项：参考 [链接](https://example.com)

```python
def process(data):
    return [x * 2 for x in data]
```

> 引用块说明一些重要内容。

最后一段总结。"
    )
}

fn make_vm_list(count: usize) -> Vec<TuiRenderUnit> {
    let mut vms = Vec::with_capacity(count);
    for i in 0..count {
        match i % 4 {
            0 => vms.push(TuiRenderUnit::TuiUserBubble(TuiUserBubble {
                text: make_user_text(i),
                reminder: None,
                content_hash: i as u64,
            })),
            1 => vms.push(TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
                text: make_assistant_text(i),
                reasoning: None,
                content_hash: i as u64 + 10000,
            })),
            2 => vms.push(TuiRenderUnit::TuiToolCard(TuiToolCard {
                tool_id: format!("tool_{i}"),
                tool_name: "Read".into(),
                input_summary: format!("file_{}.rs", i),
                output_summary: format!("file contents {}", i),
                is_error: false,
                is_running: false,
                running_duration_ms: Some(120),
                diff: None,
                content_hash: i as u64 + 20000,
                tool_calls_count: 0,
            })),
            _ => vms.push(TuiRenderUnit::TuiSystemNote(TuiSystemNote {
                text: format!("System note {}", i),
                level: TuiNoteLevel::Info,
                content_hash: i as u64 + 30000,
            })),
        }
    }
    vms
}

fn render_vm(vm: &TuiRenderUnit, width: usize) -> Vec<ratatui::text::Line<'static>> {
    let palette = Palette::default();
    match vm {
        TuiRenderUnit::TuiUserBubble(d) => {
            let parsed = parse_markdown(&d.text, width, palette);
            parsed.lines.into_iter().collect()
        }
        TuiRenderUnit::TuiAssistantBubble(d) => {
            let parsed = parse_markdown(&d.text, width, palette);
            parsed.lines.into_iter().collect()
        }
        _ => Vec::new(),
    }
}

fn main() {
    // 触发主题初始化，避免首次访问的额外开销
    let default_palette = Palette::default();

    println!("=== ViewModels → Vec<Line> 渲染管道基准 ===\n");

    let total = 200;
    let vms = make_vm_list(total);
    println!("ViewModel 数量: {}", vms.len());
    println!("宽度: 100 列\n");

    // 模拟流式 80 个 chunk，每个 chunk 触发"重建尾部 1 条"
    let chunk_count = 80;

    // === 场景 A：全量重建（968992e0 坏态）===
    // 每个 chunk 都把所有 ViewModel 重新渲染一遍
    let start = Instant::now();
    for chunk_idx in 0..chunk_count {
        let last_idx = vms.len() - 1;
        let mut all_lines = Vec::new();
        for (i, vm) in vms.iter().enumerate() {
            if i == last_idx {
                if let TuiRenderUnit::TuiAssistantBubble(b) = vm {
                    let bytes = b.text.as_bytes();
                    let take = (bytes.len() * (chunk_idx + 1) / chunk_count).max(1);
                    let truncated = String::from_utf8(bytes[..take].to_vec()).unwrap();
                    let parsed = parse_markdown(&truncated, 100, default_palette);
                    all_lines.extend(parsed.lines.iter().cloned());
                }
            } else {
                all_lines.extend(render_vm(vm, 100));
            }
        }
        let _ = Arc::new(all_lines);
    }
    let full = start.elapsed();

    // === 场景 B：增量重建（修复后好态，content_hash 增量）===
    // 每个 chunk 只渲染变化的尾部 1 条，前 199 条复用
    let mut cached_lines: Vec<Vec<ratatui::text::Line<'static>>> =
        vms.iter().map(|vm| render_vm(vm, 100)).collect();
    let start = Instant::now();
    for chunk_idx in 0..chunk_count {
        let last_idx = vms.len() - 1;
        if let TuiRenderUnit::TuiAssistantBubble(b) = &vms[last_idx] {
            let bytes = b.text.as_bytes();
            let take = (bytes.len() * (chunk_idx + 1) / chunk_count).max(1);
            let truncated = String::from_utf8(bytes[..take].to_vec()).unwrap();
            let parsed = parse_markdown(&truncated, 100, default_palette);
            cached_lines[last_idx] = parsed.lines.into_iter().collect();
        }
        let _all_lines: Vec<&ratatui::text::Line<'static>> =
            cached_lines.iter().flat_map(|v| v.iter()).collect();
    }
    let incremental = start.elapsed();

    println!("[场景 A] 全量重建（每 chunk 渲染全部 {} 条 VM）", total);
    println!("    总耗时: {:.2?}", full);
    println!("    每帧:   {:.2?}", full / chunk_count as u32);

    println!("\n[场景 B] 增量重建（每 chunk 只渲染变化的尾部 1 条）");
    println!("    总耗时: {:.2?}", incremental);
    println!("    每帧:   {:.2?}", incremental / chunk_count as u32);

    println!("\n=== 结论 ===");
    println!(
        "全量重建每帧:     {:.2?} ({:.0} chunks/s 上限)",
        full / chunk_count as u32,
        1.0 / (full / chunk_count as u32).as_secs_f64()
    );
    println!(
        "增量重建每帧:     {:.2?} ({:.0} chunks/s 上限)",
        incremental / chunk_count as u32,
        1.0 / (incremental / chunk_count as u32).as_secs_f64()
    );
    println!(
        "加速比:           {:.1}x",
        full.as_secs_f64() / incremental.as_secs_f64()
    );
}
