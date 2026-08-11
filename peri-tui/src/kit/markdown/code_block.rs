use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, LazyLock};

use parking_lot::RwLock;
use ratatui::{
    style::Style,
    text::{Line, Span},
};
use ratatui_kit_markdown::MarkdownTheme;
use syntect::{easy::HighlightLines, highlighting::ThemeSet, parsing::SyntaxSet};

// ── syntect 全局单例 ───────────────────────────────────────────────

pub(crate) static SYNTAX_SET: LazyLock<SyntaxSet> =
    LazyLock::new(SyntaxSet::load_defaults_newlines);
pub(crate) static THEME_SET: LazyLock<ThemeSet> = LazyLock::new(ThemeSet::load_defaults);

// ── 高亮结果缓存（LRU，上限 32 条）───────────────────────────────────
//
// [Why] 流式 markdown 渲染期间，每个 token 触发整段 text 的 parse_markdown，
// 进而调用 highlight_code_block。但已闭合的 code block 内容完全不变——
// 反复跑 syntect 是 CPU 最大头（5-10ms+ 大代码块）。
//
// [Key] (lang.to_string(), hash(raw_lines))。同 (lang, content) 永远产同结果（纯函数）。
// [Value] Option<Arc<Vec<Line>>>：None 表示无可用 syntax（避免反复 find_syntax_by_token）。
// [LRU] 读 + 写均更新顺序；满时淘汰最旧。
//
// [TRAP] parking_lot::RwLock 而非 std——std RwLockReadGuard 非 Send，未来若移入
// async 上下文会编译报错（CLAUDE.md 已记录）。
type CacheValue = Option<Arc<Vec<Line<'static>>>>;

static HIGHLIGHT_CACHE: LazyLock<RwLock<HlCache>> = LazyLock::new(|| RwLock::new(HlCache::new()));

struct HlCache {
    cap: usize,
    entries: HashMap<(String, u64), CacheValue>,
    /// LRU 顺序：末尾为最近访问，头部为最旧。
    order: Vec<(String, u64)>,
}

impl HlCache {
    fn new() -> Self {
        Self {
            cap: 32,
            entries: HashMap::new(),
            order: Vec::with_capacity(33),
        }
    }

    /// 查询并将 key 提升到 LRU 末尾。返回 Some(Clone) 表示命中，None 表示未命中。
    fn get(&mut self, key: &(String, u64)) -> Option<CacheValue> {
        if let Some(v) = self.entries.get(key).cloned() {
            self.order.retain(|k| k != key);
            self.order.push(key.clone());
            Some(v)
        } else {
            None
        }
    }

    /// 插入新条目；若已满则淘汰 order 头部最旧条目。
    fn insert(&mut self, key: (String, u64), val: CacheValue) {
        if !self.entries.contains_key(&key)
            && self.entries.len() >= self.cap
            && let Some(evicted) = self.order.first().cloned()
        {
            self.entries.remove(&evicted);
            self.order.remove(0);
        }
        self.entries.insert(key.clone(), val);
        self.order.retain(|k| k != &key);
        self.order.push(key);
    }

    /// 清空缓存（测试辅助）。
    #[cfg(test)]
    fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }
}

// ── 代码块高亮 ──────────────────────────────────────────────────────

pub(crate) fn highlight_code_block(lang: &str, raw_lines: &[String]) -> Option<Vec<Line<'static>>> {
    let key_hash = hash_raw_lines(raw_lines);
    let key = (lang.to_string(), key_hash);

    // 1. 查缓存：命中则直接 clone 返回
    if let Some(cached) = HIGHLIGHT_CACHE.write().get(&key) {
        return cached.map(|arc| (*arc).clone());
    }

    // 2. miss → 跑 syntect
    let result = highlight_code_block_inner(lang, raw_lines);
    let arc_result = result.clone().map(Arc::new);
    HIGHLIGHT_CACHE.write().insert(key, arc_result);
    result
}

/// 与 `highlight_code_block` 同逻辑，但额外返回是否命中缓存（仅供测试断言）。
#[cfg(test)]
pub(crate) fn highlight_code_block_with_hit(
    lang: &str,
    raw_lines: &[String],
) -> (Option<Vec<Line<'static>>>, bool) {
    let key_hash = hash_raw_lines(raw_lines);
    let key = (lang.to_string(), key_hash);

    if let Some(cached) = HIGHLIGHT_CACHE.write().get(&key) {
        return (cached.map(|arc| (*arc).clone()), true);
    }
    let result = highlight_code_block_inner(lang, raw_lines);
    let arc_result = result.clone().map(Arc::new);
    HIGHLIGHT_CACHE.write().insert(key, arc_result);
    (result, false)
}

/// 真正跑 syntect 的实现。
fn highlight_code_block_inner(lang: &str, raw_lines: &[String]) -> Option<Vec<Line<'static>>> {
    let ss = &*SYNTAX_SET;
    let syntax = ss.find_syntax_by_token(lang)?;
    let theme = &THEME_SET.themes["base16-ocean.dark"];
    let mut highlighter = HighlightLines::new(syntax, theme);

    // 获取主题默认前景色，用于判断哪些文本未被语法高亮着色
    let default_fg = theme.settings.foreground;

    let mut result = Vec::with_capacity(raw_lines.len());
    for line_text in raw_lines {
        let ranges = highlighter.highlight_line(line_text, ss).ok()?;
        let spans: Vec<Span<'static>> = ranges
            .iter()
            .map(|(style, text)| {
                // 未被语法高亮着色的文本 → 使用终端默认色（Color::Reset）
                let is_default = default_fg.is_none_or(|df| {
                    style.foreground.r == df.r
                        && style.foreground.g == df.g
                        && style.foreground.b == df.b
                });
                if is_default {
                    Span::raw(text.to_string())
                } else {
                    let color = ratatui::style::Color::Rgb(
                        style.foreground.r,
                        style.foreground.g,
                        style.foreground.b,
                    );
                    Span::styled(text.to_string(), Style::default().fg(color))
                }
            })
            .collect();
        result.push(Line::from(spans));
    }
    Some(result)
}

/// hash raw_lines：逐行 hash + 分隔符 0xA，避免相邻串拼接歧义。
fn hash_raw_lines(raw_lines: &[String]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for line in raw_lines {
        line.hash(&mut h);
        0xAu8.hash(&mut h);
    }
    h.finish()
}

pub(crate) fn code_block_lines(
    lang: &str,
    raw_lines: &[String],
    theme: &MarkdownTheme,
) -> Vec<Line<'static>> {
    let lang_clean = lang.trim();
    let highlighted = highlight_code_block(lang_clean, raw_lines);

    // [Slice 3] §6.2：code block 使用 surface.sunken 背景——所有行（前缀 + 代码）
    // 统一加下沉背景，复制时背景不进入剪贴板（§9 由复制层剥离）。
    let sunken_bg = peri_theme::atoms::THEME_ATOM
        .state()
        .read()
        .semantic
        .surface
        .sunken;
    let patch_bg = |style: Style| -> Style { style.bg(sunken_bg) };

    if raw_lines.len() == 1 {
        // 单行代码块：inline code style
        if let Some(hl_lines) = highlighted {
            return hl_lines
                .into_iter()
                .map(|line| {
                    let mut spans = Vec::with_capacity(line.spans.len());
                    for span in line.spans {
                        spans.push(Span::styled(span.content, patch_bg(span.style)));
                    }
                    Line::from(spans)
                })
                .collect();
        }
        return vec![Line::from(Span::styled(
            raw_lines[0].clone(),
            patch_bg(theme.inline_code_style),
        ))];
    }

    // 多行代码块：每行加 `│ ` 前缀
    let prefix_style = patch_bg(theme.rule_style);
    let prefix = Span::styled("│ ", prefix_style);

    if let Some(hl_lines) = highlighted {
        hl_lines
            .into_iter()
            .map(|line| {
                let mut spans = vec![prefix.clone()];
                for span in line.spans {
                    spans.push(Span::styled(span.content, patch_bg(span.style)));
                }
                Line::from(spans)
            })
            .collect()
    } else {
        raw_lines
            .iter()
            .map(|raw| {
                Line::from(vec![
                    prefix.clone(),
                    Span::styled(raw.clone(), Style::default().bg(sunken_bg)),
                ])
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "code_block_test.rs"]
mod tests;
