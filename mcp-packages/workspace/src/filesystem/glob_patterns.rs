//! glob 过滤器的语法规则：`{a,b}` 交替展开 + `glob::Pattern` 编译。
//!
//! Grep 的 `glob` 与 Glob 的 `pattern` 共用这一处实现。`glob` crate（0.3）没有
//! 交替语义——`{ts,tsx}` 会被当成字面量——所以推荐语法在这里先展开再编译。
//!
//! 只有「本层带逗号」的 `{…}` 才是交替：`foo{bar}`、`[{]foo.rs`、`x[{,}]y` 里的
//! 花括号和逗号都是普通字符，与 glob crate 的字面匹配一致，既不改写也不报错。
//! 展开用显式工作栈迭代（全程无递归），另配长度、链长、输出三重预算，深嵌套或
//! 大量顺序 `{a,b}` 输入只会得到诊断，不会耗尽调用栈。
//! 非法 pattern 一律返回诊断，由工具转成可行动的工具错误：静默丢弃过滤器会把
//! 「过滤后没有匹配」和「过滤器根本没生效」输出成同一个结果。

use std::iter::Peekable;
use std::str::CharIndices;

use glob::Pattern;

/// 交替展开后的 pattern 数上限。`{a,b}{c,d}…` 是指数级组合，超出即报错，
/// 不让一次调用编译出成千上万个 pattern。
const MAX_EXPANDED_PATTERNS: usize = 128;

/// 单个过滤器的字节长度上限。展开每步都要复制整串 pattern，长度先封顶，
/// 超长输入不进入展开；4096 字节已远超任何实用 glob。
const MAX_PATTERN_LEN: usize = 4096;

/// 一条展开链上的交替组数上限（嵌套与顺序都算）。展开工作量上界是
/// 「输出预算 × 链长 × pattern 长度」，链长封顶后深嵌套或大量顺序组的输入
/// 会在有限步内报错，而不是把一次调用拖成长时间 CPU 占用。
const MAX_ALTERNATION_CHAIN: usize = 32;

/// 编译 glob pattern：展开 `{a,b}` 交替后逐个编译。
///
/// 语法错误（模式过长、字符类/交替括号未闭合、展开超限）返回诊断字符串；
/// 调用方负责把它包装成带恢复指引的工具错误。
pub(crate) fn compile_glob_patterns(pattern: &str) -> Result<Vec<Pattern>, String> {
    // 长度在展开之前校验：超长输入不进入展开循环。
    if pattern.len() > MAX_PATTERN_LEN {
        return Err(format!(
            "glob pattern is {} bytes, past the {MAX_PATTERN_LEN}-byte limit: shorten it or split it into several filters",
            pattern.len()
        ));
    }
    let mut expanded = Vec::new();
    expand_alternates(pattern, &mut expanded)?;
    expanded
        .iter()
        .map(|candidate| {
            Pattern::new(candidate)
                .map_err(|error| format!("invalid glob pattern {candidate:?}: {error}"))
        })
        .collect()
}

/// 编译一组 glob 过滤器（`-g` 与 type 映射）：任一分支非法即返回其诊断。
pub(crate) fn compile_glob_filters(filters: &[String]) -> Result<Vec<Pattern>, String> {
    let mut patterns = Vec::new();
    for filter in filters {
        patterns.extend(compile_glob_patterns(filter)?);
    }
    Ok(patterns)
}

/// 展开交替组：候选串放进显式工作栈迭代处理，全程不用递归。
///
/// 递归版本的调用栈会随嵌套深度或顺序 `{a,b}` 个数增长，深输入直接爆栈；迭代版
/// 没有这个失败模式。每次展开都把 `{…}` 换成它内部更短的候选，候选串长度严格
/// 递减，循环必然终止；`chain` 上限再给「一条链上能做多少次展开」封顶。
fn expand_alternates(pattern: &str, out: &mut Vec<String>) -> Result<(), String> {
    // 候选串 + 它已展开的交替组数（链长）。
    let mut pending = vec![(pattern.to_string(), 0usize)];
    while let Some((candidate, chain)) = pending.pop() {
        let Some(group) = find_alternation_group(&candidate)? else {
            if out.len() >= MAX_EXPANDED_PATTERNS {
                return Err(format!(
                    "glob alternation expands past {MAX_EXPANDED_PATTERNS} patterns"
                ));
            }
            out.push(candidate);
            continue;
        };
        if chain >= MAX_ALTERNATION_CHAIN {
            return Err(format!(
                "glob alternation chain longer than {MAX_ALTERNATION_CHAIN} groups: simplify the nested or repeated `{{…}}` groups"
            ));
        }
        let head = &candidate[..group.open];
        let tail = &candidate[group.close + 1..];
        // 逆序入栈：栈是后进先出，出栈顺序与书写顺序一致，展开结果顺序稳定。
        for alternative in group.alternatives(&candidate).iter().rev() {
            pending.push((format!("{head}{alternative}{tail}"), chain + 1));
        }
    }
    Ok(())
}

/// 一个可展开的交替组：`{`/`}` 的字节下标，加上本层逗号（分支分隔符）的下标。
struct AlternationGroup {
    open: usize,
    close: usize,
    /// 本层逗号的字节下标，升序；至少一个，没有逗号的 `{…}` 不是交替组。
    separators: Vec<usize>,
}

impl AlternationGroup {
    /// 按本层逗号切出各分支：`{a,b}` → `a`、`b`。
    fn alternatives<'a>(&self, pattern: &'a str) -> Vec<&'a str> {
        let mut alternatives = Vec::with_capacity(self.separators.len() + 1);
        let mut start = self.open + 1;
        for &separator in &self.separators {
            alternatives.push(&pattern[start..separator]);
            start = separator + 1;
        }
        alternatives.push(&pattern[start..self.close]);
        alternatives
    }
}

/// 找 pattern 里第一个可展开的交替组；没有则返回 `None`。
///
/// 只有本层带逗号的 `{…}` 才展开（`{a,b}`、`*.{ts,{js,jsx}}`）；`foo{bar}`、`{a}`、
/// `[{]foo.rs` 里的花括号按字面交给 glob crate。字符类 `[…]` 整体跳过，其中的
/// `{`、`,`、`}` 都算类内容。多个组可嵌套时取 `{` 最靠左的那个，展开顺序与书写
/// 顺序一致。
///
/// 未闭合但带逗号的 `{` 是笔误而不是字面量（`*.{ts,tsx` 会被 glob 当字面量，
/// 静默匹配不到任何文件），返回诊断让调用方报错。
fn find_alternation_group(pattern: &str) -> Result<Option<AlternationGroup>, String> {
    // 打开中的 `{`：字节下标 + 本层已见逗号的下标。
    let mut open: Vec<(usize, Vec<usize>)> = Vec::new();
    let mut found: Option<AlternationGroup> = None;
    let mut chars = pattern.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        match ch {
            '[' => skip_character_class(&mut chars),
            '{' => open.push((index, Vec::new())),
            ',' => {
                if let Some((_, separators)) = open.last_mut() {
                    separators.push(index);
                }
            }
            '}' => {
                if let Some((start, separators)) = open.pop() {
                    if !separators.is_empty() && found.as_ref().is_none_or(|g| start < g.open) {
                        found = Some(AlternationGroup {
                            open: start,
                            close: index,
                            separators,
                        });
                    }
                }
            }
            _ => {}
        }
    }
    if open.iter().any(|(_, separators)| !separators.is_empty()) {
        return Err(format!(
            "unclosed `{{` in glob pattern {pattern:?}: close it or remove it"
        ));
    }
    Ok(found)
}

/// 跳过 `[` 起始的字符类（`[abc]`、`[!abc]`），停在收尾 `]` 之后。
///
/// 括号位置与 glob crate 的解析对齐：`[` 后紧跟的字符（`[!` 之后再跟一个）即使是
/// `]` 也算类内容，收尾 `]` 从它之后开始找。未闭合的类一直吃到串尾——那种 pattern
/// 会由 `glob::Pattern::new` 报 `invalid range pattern`，这里不重复判定。
fn skip_character_class(chars: &mut Peekable<CharIndices<'_>>) {
    if matches!(chars.peek(), Some((_, '!'))) {
        chars.next();
    }
    // 类内容的首字符可以写成 `]`：无条件吞掉。
    if chars.next().is_none() {
        return;
    }
    for (_, ch) in chars.by_ref() {
        if ch == ']' {
            return;
        }
    }
}

#[cfg(test)]
#[path = "glob_patterns_test.rs"]
mod tests;
