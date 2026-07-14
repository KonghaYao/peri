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

    if raw_lines.len() == 1 {
        // 单行代码块：inline code style
        if let Some(hl_lines) = highlighted {
            return hl_lines;
        }
        return vec![Line::from(Span::styled(
            raw_lines[0].clone(),
            theme.inline_code_style,
        ))];
    }

    // 多行代码块：每行加 `│ ` 前缀
    let prefix_style = theme.rule_style;
    let prefix = Span::styled("│ ", prefix_style);

    if let Some(hl_lines) = highlighted {
        hl_lines
            .into_iter()
            .map(|line| {
                let mut spans = vec![prefix.clone()];
                spans.extend(line.spans);
                Line::from(spans)
            })
            .collect()
    } else {
        raw_lines
            .iter()
            .map(|raw| {
                Line::from(vec![
                    prefix.clone(),
                    Span::styled(raw.clone(), Style::default()),
                ])
            })
            .collect()
    }
}

// ── 测试 ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// 测试辅助：构造代码行 Vec。
    fn make_lines(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// 测试辅助：清空全局缓存（每个测试隔离）。
    fn reset_cache() {
        HIGHLIGHT_CACHE.write().clear();
    }

    #[test]
    #[serial]
    fn test_highlight_cache_hit_on_same_input() {
        reset_cache();
        let lines = make_lines(&["let x = 1;", "let y = 2;"]);
        // 第一次 miss
        let (r1, hit1) = highlight_code_block_with_hit("rust", &lines);
        assert!(!hit1, "首次调用应 miss");
        assert!(r1.is_some(), "rust 应能高亮");
        // 第二次相同输入 → hit
        let (r2, hit2) = highlight_code_block_with_hit("rust", &lines);
        assert!(hit2, "相同 (lang, lines) 第二次应命中缓存");
        assert_eq!(r1, r2, "命中缓存应返回一致结果");
    }

    #[test]
    #[serial]
    fn test_highlight_cache_miss_on_different_lang() {
        reset_cache();
        let lines = make_lines(&["let x = 1;"]);
        let (_, hit1) = highlight_code_block_with_hit("rust", &lines);
        assert!(!hit1);
        // 同 content 不同 lang → miss
        let (_, hit2) = highlight_code_block_with_hit("python", &lines);
        assert!(!hit2, "不同 lang 应 miss");
    }

    #[test]
    #[serial]
    fn test_highlight_cache_miss_on_different_content() {
        reset_cache();
        let lines_a = make_lines(&["let x = 1;"]);
        let lines_b = make_lines(&["let x = 2;"]);
        let (_, hit1) = highlight_code_block_with_hit("rust", &lines_a);
        assert!(!hit1);
        let (_, hit2) = highlight_code_block_with_hit("rust", &lines_b);
        assert!(!hit2, "同 lang 不同 content 应 miss");
    }

    #[test]
    #[serial]
    fn test_highlight_cache_none_result_cached() {
        reset_cache();
        // 未知 lang（"totally-not-a-real-lang"）→ find_syntax_by_token 返回 None
        let lines = make_lines(&["some code"]);
        let (r1, hit1) = highlight_code_block_with_hit("totally-unknown-lang-xyz", &lines);
        assert!(!hit1);
        assert!(r1.is_none(), "未知 lang 应返回 None");
        // 第二次：None 结果也应被缓存（避免反复调 find_syntax_by_token）
        let (r2, hit2) = highlight_code_block_with_hit("totally-unknown-lang-xyz", &lines);
        assert!(hit2, "None 结果也应命中缓存");
        assert!(r2.is_none());
    }

    #[test]
    #[serial]
    fn test_highlight_cache_lru_eviction() {
        reset_cache();
        // 填满 32 条
        for i in 0..32 {
            let lines = make_lines(&[&format!("line {i}")]);
            let (_, hit) = highlight_code_block_with_hit("rust", &lines);
            assert!(!hit, "第 {i} 条首次插入应 miss");
        }
        // 第 33 条 → 淘汰最旧（line 0）
        let lines_33 = make_lines(&["line 32"]);
        let (_, hit_33) = highlight_code_block_with_hit("rust", &lines_33);
        assert!(!hit_33, "第 33 条应 miss");
        // 验证 line 0 已被淘汰（重新查应 miss）
        let lines_0 = make_lines(&["line 0"]);
        let (_, hit_0) = highlight_code_block_with_hit("rust", &lines_0);
        assert!(!hit_0, "被淘汰的最旧条目重查应 miss");
        // 验证 line 31 仍在缓存
        let lines_31 = make_lines(&["line 31"]);
        let (_, hit_31) = highlight_code_block_with_hit("rust", &lines_31);
        assert!(hit_31, "未淘汰的条目应命中");
    }

    #[test]
    #[serial]
    fn test_hash_raw_lines_distinguishes_adjacent_splits() {
        // 防御性测试：分隔符 0xA 应让 ["ab","c"] 与 ["a","bc"] 产生不同 hash
        let h1 = hash_raw_lines(&make_lines(&["ab", "c"]));
        let h2 = hash_raw_lines(&make_lines(&["a", "bc"]));
        assert_ne!(h1, h2, "相邻串拼接歧义应被分隔符消除");
    }

    #[test]
    #[serial]
    fn test_hash_raw_lines_same_input_same_hash() {
        let h1 = hash_raw_lines(&make_lines(&["fn main() {}", "    println!(\"hi\");"]));
        let h2 = hash_raw_lines(&make_lines(&["fn main() {}", "    println!(\"hi\");"]));
        assert_eq!(h1, h2);
    }
}
