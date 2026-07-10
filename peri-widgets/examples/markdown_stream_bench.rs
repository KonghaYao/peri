//! Markdown 流式渲染微基准（example binary，无需 criterion）。
//!
//! 模拟 TUI 流式输出场景：每个 text chunk 累积后都重新解析整个文本。
//! 测量 `parse_markdown` 在两种条件下的耗时：
//!   - 冷启动（每帧前清缓存）：模拟无缓存的全量重建
//!   - 命中缓存（LRU 全局共享）：模拟增量更新期间前缀重复命中
//!
//! 运行：`cargo run --release --example markdown_stream_bench -p peri-widgets --features markdown-highlight`

use std::time::Instant;

use peri_widgets::markdown::{DefaultMarkdownTheme, ThemeMarkdownAdapter};
use peri_widgets::theme::DarkTheme;

const SAMPLE_MD: &str = include_str!("sample_markdown.md");

fn main() {
    println!("=== peri-widgets markdown 流式渲染基准 ===\n");
    println!("样本大小: {} 字节", SAMPLE_MD.len());

    let theme = DefaultMarkdownTheme;
    let adapter = ThemeMarkdownAdapter(&DarkTheme);

    // 模拟一次完整 AI turn：80 个 chunks，每个 chunk 累积
    let chunks: Vec<String> = (1..=80)
        .map(|i| {
            let bytes = SAMPLE_MD.as_bytes();
            let take = (bytes.len() * i / 80).max(1);
            String::from_utf8(bytes[..take].to_vec()).unwrap()
        })
        .collect();

    // 测试 1：冷启动（unique input 强制 cache miss）—— 模拟无缓存全量重建
    // 每次拼接一个 unique 后缀，保证 cache 100% miss
    let start = Instant::now();
    for (i, chunk) in chunks.iter().enumerate() {
        let unique = format!("{}<!-- {} -->", chunk, i);
        let _ = peri_widgets::markdown::parse_markdown(&unique, &theme, 100);
    }
    let cold = start.elapsed();
    println!("\n[1] 冷启动（unique input，模拟无缓存全量重建）");
    println!("    总耗时: {:.2?}", cold);
    println!("    每帧平均: {:.2?}", cold / chunks.len() as u32);

    // 测试 2：命中缓存（LRU 全局共享）—— 模拟增量更新
    // 同样的 chunks 序列，第二次解析时大部分内容已经缓存过
    let start = Instant::now();
    for chunk in &chunks {
        let _ = peri_widgets::markdown::parse_markdown(chunk, &theme, 100);
    }
    let warm = start.elapsed();
    println!("\n[2] 热启动（同 input 第二次解析，LRU 全局共享）");
    println!("    总耗时: {:.2?}", warm);
    println!("    每帧平均: {:.2?}", warm / chunks.len() as u32);

    // 测试 3：相同内容重复解析 —— 缓存命中率 100%
    let last = chunks.last().unwrap();
    let start = Instant::now();
    for _ in 0..1000 {
        let _ = peri_widgets::markdown::parse_markdown(last, &theme, 100);
    }
    let cached = start.elapsed();
    println!("\n[3] 完全命中缓存（相同内容 1000 次）");
    println!("    总耗时: {:.2?}", cached);
    println!("    每次平均: {:.2?}", cached / 1000);

    // 测试 4：DarkTheme adapter 与 Default theme 对比
    let start = Instant::now();
    for (i, chunk) in chunks.iter().enumerate() {
        let unique = format!("{}<!-- {} -->", chunk, i);
        let _ = peri_widgets::markdown::parse_markdown(&unique, &adapter, 100);
    }
    let cold_adapter = start.elapsed();
    println!("\n[4] 冷启动（DarkTheme 适配器，unique input）");
    println!("    总耗时: {:.2?}", cold_adapter);
    println!("    每帧平均: {:.2?}", cold_adapter / chunks.len() as u32);

    println!("\n=== 结论 ===");
    println!("冷启动每帧:   {:.2?}", cold / chunks.len() as u32);
    println!("热启动每帧:   {:.2?}", warm / chunks.len() as u32);
    println!(
        "加速比:       {:.1}x",
        cold.as_secs_f64() / warm.as_secs_f64()
    );
}
