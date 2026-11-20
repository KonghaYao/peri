//! Mermaid 语句级增量确认。
//!
//! [Why 自己拆语句] 上游 0.58.0 的流图拆分只看引号与括号，`A -- x;y --> B`
//! 和 `A -->|x;y| B` 会在标签分号处断句，产生并不存在的节点。这里按图型维护
//! 扫描游标，只提交完整且能识别的语句；作用域（`subgraph/end`、`alt/end` 等）
//! 在最外层闭合后才整组进入已确认前缀。

/// 首行声明的图型。只有流图与时序图支持边生成边预览；其余类型在围栏闭合后
/// 整份渲染。`SourceOnly` 只留给库能画但当前不接入的类型（`block`）和未知类型。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DiagramMode {
    /// 首行还不完整，无法判断类型。
    #[default]
    Unknown,
    Flowchart,
    Sequence,
    State,
    Class,
    Er,
    Pie,
    Journey,
    Gantt,
    Timeline,
    GitGraph,
    Mindmap,
    Quadrant,
    Requirement,
    Sankey,
    XyChart,
    Architecture,
    Packet,
    /// 其他类型（含 `block` / `block-beta`）保留源码。
    SourceOnly,
}

impl DiagramMode {
    #[must_use]
    pub fn streams(self) -> bool {
        matches!(self, Self::Flowchart | Self::Sequence)
    }

    #[must_use]
    pub fn renders_when_closed(self) -> bool {
        matches!(
            self,
            Self::Flowchart
                | Self::Sequence
                | Self::State
                | Self::Class
                | Self::Er
                | Self::Pie
                | Self::Journey
                | Self::Gantt
                | Self::Timeline
                | Self::GitGraph
                | Self::Mindmap
                | Self::Quadrant
                | Self::Requirement
                | Self::Sankey
                | Self::XyChart
                | Self::Architecture
                | Self::Packet
        )
    }
}

/// 只检查首行是否已经完整到达；未完整的行仍可能被下一个 token 改写。
#[must_use]
pub fn detect_mode(source: &str, terminal: bool) -> DiagramMode {
    let Some(line) = first_content_line(source, terminal) else {
        return DiagramMode::Unknown;
    };
    let keyword = line
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    match keyword.as_str() {
        "graph" | "flowchart" => DiagramMode::Flowchart,
        "sequencediagram" => DiagramMode::Sequence,
        "statediagram" | "statediagram-v2" => DiagramMode::State,
        "classdiagram" => DiagramMode::Class,
        "erdiagram" => DiagramMode::Er,
        "pie" => DiagramMode::Pie,
        "journey" => DiagramMode::Journey,
        "gantt" => DiagramMode::Gantt,
        "timeline" => DiagramMode::Timeline,
        "gitgraph" => DiagramMode::GitGraph,
        "mindmap" => DiagramMode::Mindmap,
        "quadrantchart" => DiagramMode::Quadrant,
        "requirementdiagram" => DiagramMode::Requirement,
        "sankey" | "sankey-beta" => DiagramMode::Sankey,
        "xychart" | "xychart-beta" => DiagramMode::XyChart,
        "architecture" | "architecture-beta" => DiagramMode::Architecture,
        "packet" | "packet-beta" => DiagramMode::Packet,
        _ => DiagramMode::SourceOnly,
    }
}

fn first_content_line(source: &str, terminal: bool) -> Option<&str> {
    for segment in source.split_inclusive('\n') {
        let completed = segment.ends_with('\n') || terminal;
        if !completed {
            return None;
        }
        let line = segment.trim_end_matches('\n').trim_end_matches('\r').trim();
        if line.is_empty() || line.starts_with("%%") {
            continue;
        }
        return Some(line);
    }
    None
}

/// 闭合围栏的完整语义校验。流图/时序图走与流式相同的语句识别器；
/// 状态图、类图、ER 图交给上游解析器验收。
pub fn validate_final(source: &str, mode: DiagramMode) -> Result<(), String> {
    if !mode.streams() {
        return Ok(());
    }
    let mut stream = MermaidStream::new();
    let update = stream.observe(source, true);
    if update.mode == DiagramMode::Unknown {
        return Err("diagram header is missing or incomplete".to_string());
    }
    if let Some(reason) = update.paused {
        return Err(reason.to_string());
    }
    if let Some(reason) = update.final_issue {
        return Err(reason.to_string());
    }
    Ok(())
}

/// 最外层尚未闭合的作用域，供预览界面显示组名与内部语句数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenScopeView<'a> {
    pub label: &'a str,
    pub statements: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct StreamUpdate<'a> {
    pub mode: DiagramMode,
    /// 已经确认、可以交给渲染器的源码前缀。
    pub trusted: &'a str,
    /// 已确认前缀在原始正文中的结束字节位置。
    pub trusted_end: usize,
    /// 已经消费的语句数（含作用域内部）。只供测试诊断。
    #[cfg(test)]
    pub statements: usize,
    /// 是否已有可画的节点/参与者/消息/边。
    pub drawable: bool,
    pub paused: Option<&'a str>,
    pub open_scope: Option<OpenScopeView<'a>>,
    /// 仅在 `terminal` 为真时给出：未闭合作用域或未完成语句。
    pub final_issue: Option<&'a str>,
}

/// 一个 Mermaid 正文的增量确认状态。接收完整正文，只扫描新增字节。
#[derive(Debug, Default)]
pub struct MermaidStream {
    mode: DiagramMode,
    source_seen: String,
    scanned: usize,
    trusted: String,
    trusted_end: usize,
    statements: usize,
    drawable: bool,
    header_seen: bool,
    messages_started: bool,
    paused: Option<String>,
    incomplete: Option<String>,
    final_issue: Option<String>,
    scopes: Vec<Scope>,
}

#[derive(Debug)]
struct Scope {
    kind: ScopeKind,
    label: String,
    raw_start: usize,
    statements: usize,
    drawable: bool,
}

#[derive(Debug)]
enum ScopeKind {
    Subgraph,
    Sequence(SequenceBlock),
    Box,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SequenceBlock {
    Loop,
    Alt,
    Opt,
    Par,
    Critical,
    Break,
    Rect,
}

impl SequenceBlock {
    fn continuation(self) -> Option<&'static str> {
        match self {
            Self::Alt => Some("else"),
            Self::Par => Some("and"),
            Self::Critical => Some("option"),
            Self::Loop | Self::Opt | Self::Break | Self::Rect => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Loop => "loop",
            Self::Alt => "alt",
            Self::Opt => "opt",
            Self::Par => "par",
            Self::Critical => "critical",
            Self::Break => "break",
            Self::Rect => "rect",
        }
    }
}

impl MermaidStream {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 观察整份正文。`terminal` 表示正文已经真实闭合（或消息 EOF）。
    pub fn observe(&mut self, source: &str, terminal: bool) -> StreamUpdate<'_> {
        if !source.starts_with(self.source_seen.as_str()) {
            *self = Self::new();
        }
        if self.source_seen.as_str() != source {
            self.source_seen.clear();
            self.source_seen.push_str(source);
        }
        if self.mode == DiagramMode::Unknown {
            self.mode = detect_mode(source, terminal);
        }
        if self.mode.streams() && self.paused.is_none() {
            self.scan(source, terminal);
        }
        self.final_issue = if terminal && self.mode.streams() && self.paused.is_none() {
            self.terminal_issue(source)
        } else {
            None
        };
        StreamUpdate {
            mode: self.mode,
            trusted: &self.trusted,
            trusted_end: self.trusted_end,
            #[cfg(test)]
            statements: self.statements,
            drawable: self.drawable,
            paused: self.paused.as_deref(),
            open_scope: self.scopes.last().map(|scope| OpenScopeView {
                label: &scope.label,
                statements: scope.statements,
            }),
            final_issue: self.final_issue.as_deref(),
        }
    }

    fn terminal_issue(&self, source: &str) -> Option<String> {
        if let Some(reason) = &self.incomplete {
            return Some(reason.clone());
        }
        if let Some(scope) = self.scopes.last() {
            return Some(match &scope.kind {
                ScopeKind::Subgraph => format!("unclosed subgraph {:?}", scope.label),
                ScopeKind::Sequence(block) => {
                    format!("unclosed {} block", block.label())
                }
                ScopeKind::Box => "unclosed box group".to_string(),
            });
        }
        if self.scanned < source.len() && !source[self.scanned..].trim().is_empty() {
            return Some("incomplete trailing statement".to_string());
        }
        None
    }

    fn scan(&mut self, source: &str, terminal: bool) {
        loop {
            let mut start = self.scanned;
            while let Some(character) = source[start..].chars().next() {
                if !character.is_whitespace() {
                    break;
                }
                start += character.len_utf8();
            }
            if start >= source.len() {
                self.scanned = source.len();
                self.incomplete = None;
                break;
            }
            match if self.mode == DiagramMode::Sequence {
                scan_sequence_statement(source, start, terminal)
            } else {
                scan_flow_statement(source, start, terminal)
            } {
                StatementScan::None => {
                    self.incomplete = None;
                    break;
                }
                StatementScan::Incomplete(reason) => {
                    self.incomplete = Some(reason);
                    break;
                }
                StatementScan::Statement {
                    start,
                    text_end,
                    consumed_end,
                } => {
                    self.incomplete = None;
                    let statement = source[start..text_end].trim();
                    if statement.is_empty() {
                        self.scanned = consumed_end;
                        continue;
                    }
                    if !self.accept(source, start, statement, consumed_end) {
                        break;
                    }
                    self.scanned = consumed_end;
                }
            }
        }
    }

    fn accept(&mut self, source: &str, start: usize, statement: &str, consumed_end: usize) -> bool {
        if self.mode == DiagramMode::Sequence {
            self.accept_sequence(source, start, statement, consumed_end)
        } else {
            self.accept_flow(source, start, statement, consumed_end)
        }
    }

    fn accept_flow(
        &mut self,
        source: &str,
        start: usize,
        statement: &str,
        consumed_end: usize,
    ) -> bool {
        match classify_flow(statement) {
            FlowClass::Ignorable => true,
            FlowClass::Unknown => {
                self.pause(statement);
                false
            }
            FlowClass::Header => {
                if self.header_seen || !self.scopes.is_empty() {
                    self.pause(statement);
                    return false;
                }
                self.header_seen = true;
                self.commit(statement, consumed_end, false);
                true
            }
            FlowClass::Subgraph(label) => {
                self.scopes.push(Scope {
                    kind: ScopeKind::Subgraph,
                    label,
                    raw_start: start,
                    statements: 0,
                    drawable: false,
                });
                true
            }
            FlowClass::End => {
                if self.scopes.is_empty() {
                    self.pause(statement);
                    return false;
                }
                self.close_scope(source, consumed_end);
                true
            }
            FlowClass::Directive => {
                self.commit_or_count(statement, consumed_end, false);
                true
            }
            FlowClass::Drawable => {
                self.commit_or_count(statement, consumed_end, true);
                true
            }
        }
    }

    fn accept_sequence(
        &mut self,
        source: &str,
        start: usize,
        statement: &str,
        consumed_end: usize,
    ) -> bool {
        let in_box = matches!(
            self.scopes.last().map(|scope| &scope.kind),
            Some(ScopeKind::Box)
        );
        match classify_sequence(statement, self.messages_started) {
            SequenceClass::Ignorable => true,
            SequenceClass::Unknown => {
                self.pause(statement);
                false
            }
            SequenceClass::Header => {
                if self.header_seen || !self.scopes.is_empty() {
                    self.pause(statement);
                    return false;
                }
                self.header_seen = true;
                self.commit(statement, consumed_end, false);
                true
            }
            SequenceClass::ScopeOpen(block, label) => {
                if in_box {
                    self.pause(statement);
                    return false;
                }
                self.scopes.push(Scope {
                    kind: ScopeKind::Sequence(block),
                    label,
                    raw_start: start,
                    statements: 0,
                    drawable: false,
                });
                true
            }
            SequenceClass::Box => {
                if in_box {
                    self.pause(statement);
                    return false;
                }
                self.scopes.push(Scope {
                    kind: ScopeKind::Box,
                    label: "box".to_string(),
                    raw_start: start,
                    statements: 0,
                    drawable: false,
                });
                true
            }
            SequenceClass::Continuation(keyword) => {
                if in_box {
                    self.pause(statement);
                    return false;
                }
                let Some(scope) = self.scopes.last() else {
                    self.pause(statement);
                    return false;
                };
                let matches = match &scope.kind {
                    ScopeKind::Sequence(block) => block.continuation() == Some(keyword),
                    ScopeKind::Subgraph | ScopeKind::Box => false,
                };
                if !matches {
                    self.pause(statement);
                    return false;
                }
                true
            }
            SequenceClass::End => {
                if self.scopes.is_empty() {
                    self.pause(statement);
                    return false;
                }
                self.close_scope(source, consumed_end);
                true
            }
            SequenceClass::Directive => {
                let drawable = is_sequence_message_directive(statement);
                if in_box && !drawable {
                    self.pause(statement);
                    return false;
                }
                self.commit_or_count(statement, consumed_end, drawable);
                true
            }
            SequenceClass::Message => {
                if in_box {
                    self.pause(statement);
                    return false;
                }
                self.messages_started = true;
                self.commit_or_count(statement, consumed_end, true);
                true
            }
        }
    }

    fn pause(&mut self, statement: &str) {
        self.paused = Some(format!(
            "unrecognized statement: {}",
            statement.chars().take(80).collect::<String>()
        ));
    }

    fn commit_or_count(&mut self, statement: &str, consumed_end: usize, drawable: bool) {
        if self.scopes.is_empty() {
            self.commit(statement, consumed_end, drawable);
        } else {
            self.count_statement(drawable);
        }
    }

    fn count_statement(&mut self, drawable: bool) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.statements += 1;
            scope.drawable |= drawable;
            return;
        }
        self.statements += 1;
        self.drawable |= drawable;
    }

    fn commit(&mut self, statement: &str, consumed_end: usize, drawable: bool) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.statements += 1;
            scope.drawable |= drawable;
            return;
        }
        if !self.trusted.is_empty() && !self.trusted.ends_with('\n') {
            self.trusted.push('\n');
        }
        self.trusted.push_str(statement);
        self.trusted.push('\n');
        self.trusted_end = consumed_end;
        self.statements += 1;
        self.drawable |= drawable;
    }

    fn close_scope(&mut self, source: &str, consumed_end: usize) {
        let scope = self
            .scopes
            .pop()
            .expect("close_scope only runs after an opener was recorded");
        if let Some(parent) = self.scopes.last_mut() {
            parent.statements += scope.statements.saturating_add(1);
            parent.drawable |= scope.drawable;
            return;
        }
        if !self.trusted.is_empty() && !self.trusted.ends_with('\n') {
            self.trusted.push('\n');
        }
        self.trusted
            .push_str(&source[scope.raw_start..consumed_end]);
        if !self.trusted.ends_with('\n') {
            self.trusted.push('\n');
        }
        self.trusted_end = consumed_end;
        self.statements += scope.statements.saturating_add(1);
        self.drawable |= scope.drawable;
    }
}

enum StatementScan {
    None,
    Incomplete(String),
    Statement {
        start: usize,
        text_end: usize,
        consumed_end: usize,
    },
}

mod flow;
mod sequence;

use flow::{FlowClass, classify_flow, scan_flow_statement};
use sequence::{
    SequenceClass, classify_sequence, is_sequence_message_directive, scan_sequence_statement,
};

#[cfg(test)]
#[path = "../stream_test.rs"]
mod tests;
