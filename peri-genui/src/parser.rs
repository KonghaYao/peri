use std::{collections::HashSet, sync::Arc};

use indexmap::{IndexMap, IndexSet};

use crate::{
    evaluator::default_value,
    expressions::parse_expression,
    lexer::tokenize,
    materialize::{component_call, Materializer},
    statements::{auto_close, split, strip_comments},
    types::NodeValue,
    ErrorCode, Expr, Library, ParseError, ParseResult, Statement, StatementKind,
};

pub fn parse(source: &str, library: &Library) -> ParseResult {
    parse_inner(source, Arc::new(library.clone()))
}

fn parse_inner(source: &str, library: Arc<Library>) -> ParseResult {
    let cleaned = strip_comments(source);
    let cleaned = cleaned.trim();
    let (closed, incomplete) = auto_close(cleaned);
    let mut result = ParseResult {
        root: None,
        errors: Vec::new(),
        unresolved: Vec::new(),
        orphans: Vec::new(),
        incomplete: incomplete || cleaned.is_empty(),
        state: IndexMap::new(),
        statements: IndexMap::new(),
        root_expression: None,
        entry: None,
        tree: None,
        library: library.clone(),
    };
    let tokens = match tokenize(&closed) {
        Ok(tokens) => tokens,
        Err(message) => {
            result.errors.push(failed(None, message));
            return result;
        }
    };
    for (id, state, tokens) in split(&tokens) {
        let expr = match parse_expression(&tokens) {
            Ok(expr) => expr,
            Err(message) => {
                result.errors.push(failed(Some(id), message));
                continue;
            }
        };
        let kind = match &expr {
            Expr::Call { name, .. } if name == "Query" => StatementKind::Query,
            Expr::Call { name, .. } if name == "Mutation" => StatementKind::Mutation,
            _ if state => StatementKind::State,
            _ => StatementKind::Value,
        };
        result
            .statements
            .insert(id.clone(), Statement { id, kind, expr });
    }
    if result.statements.is_empty() {
        return result;
    }
    let entry = if result.statements.contains_key("root") {
        Some(("root".to_owned(), false))
    } else {
        result.statements.values().find(|s| s.kind == StatementKind::Value && component_call(&s.expr) && matches!(&s.expr, Expr::Call { name, .. } if Some(name.as_str()) == library.root())).map(|s| (s.id.clone(), true))
    };
    result.entry = entry.as_ref().map(|(id, _)| id.clone());
    let mut materializer = Materializer::new(&library, &result.statements, incomplete);
    materializer.unreached = result
        .statements
        .values()
        .filter(|s| {
            s.kind == StatementKind::Value && entry.as_ref().is_none_or(|(id, _)| *id != s.id)
        })
        .map(|s| s.id.clone())
        .collect();
    if let Some((id, _)) = &entry {
        materializer.validation.statement_id = Some(id.clone());
        if let Some(statement) = result.statements.get(id) {
            let mut value = materializer.value(&statement.expr, 0);
            if let NodeValue::Element { element, .. } = &mut value {
                element.statement_id = Some(id.clone());
            }
            match value {
                value @ NodeValue::Element { .. } => {
                    result.root = value.element();
                    result.tree = Some(value);
                }
                NodeValue::Expr(expr) => {
                    let mut contains = false;
                    expr.visit(&mut |e| contains |= component_call(e));
                    if contains {
                        result.root_expression = Some(expr);
                    }
                }
                _ => {}
            }
        }
    }
    let mut state_bytes = 0usize;
    for statement in result.statements.values() {
        if statement.kind == StatementKind::State {
            materializer.validation.statement_id = Some(statement.id.clone());
            // 默认值按声明顺序求一次；运行时自定义回调不参与初始化。
            let value = materializer.value(&statement.expr, 0);
            let value = match default_value(&value, &library, &result.state) {
                Ok(value) => {
                    let bytes = crate::memory::value_bytes(&value);
                    if state_bytes.saturating_add(bytes) > crate::MAX_STATE_BYTES {
                        materializer.validation.issue(
                            ErrorCode::ParseFailed,
                            "",
                            "",
                            format!("state 默认值超过安全预算（{}）", statement.id),
                        );
                        serde_json::Value::Null
                    } else {
                        state_bytes = state_bytes.saturating_add(bytes);
                        value
                    }
                }
                Err(error) => {
                    materializer.validation.issue(
                        ErrorCode::ParseFailed,
                        "",
                        "",
                        format!("state 默认值求值失败（{}）：{error}", statement.id),
                    );
                    serde_json::Value::Null
                }
            };
            result.state.insert(statement.id.clone(), value);
        } else if matches!(
            statement.kind,
            StatementKind::Query | StatementKind::Mutation
        ) {
            materializer.validation.statement_id = Some(statement.id.clone());
            if let Expr::Call { args, .. } = &statement.expr {
                for arg in args {
                    materializer.expr(arg, &HashSet::new(), 0);
                }
            }
        }
    }
    for statement in result.statements.values() {
        statement.expr.visit(&mut |expr| {
            if let Expr::StateRef { n } = expr {
                result
                    .state
                    .entry(n.clone())
                    .or_insert(serde_json::Value::Null);
            }
        });
    }
    materializer.validation.statement_id = entry.as_ref().map(|(id, _)| id.clone());
    let no_root = entry.is_none()
        || (result.root.is_none()
            && result.root_expression.is_none()
            && entry
                .as_ref()
                .is_some_and(|(id, _)| !resolves_component(id, &result.statements)))
        || entry.as_ref().is_some_and(|(_, fallback)| *fallback);
    if no_root {
        materializer.validation.issue(
            ErrorCode::NoRoot,
            library.root().unwrap_or(""),
            "",
            "程序需要有效 root 组件",
        );
        if entry.as_ref().is_some_and(|(_, fallback)| *fallback) && result.root.is_some() {
            if let Some(error) = materializer.validation.errors.last_mut() {
                error.severity = Some("warning".into());
            }
        }
    }
    if entry.is_some() {
        result.orphans = materializer.unreached.iter().cloned().collect();
    }
    result.unresolved = std::mem::take(&mut materializer.unresolved);
    result
        .errors
        .extend(std::mem::take(&mut materializer.validation.errors));
    result
}

fn resolves_component(id: &str, statements: &IndexMap<String, Statement>) -> bool {
    let mut current = id;
    let mut seen = IndexSet::new();
    while seen.insert(current) {
        let Some(statement) = statements.get(current) else {
            return false;
        };
        match &statement.expr {
            Expr::Ref { n } => current = n,
            expr => return component_call(expr),
        }
    }
    false
}

fn failed(statement_id: Option<String>, message: String) -> ParseError {
    ParseError {
        code: ErrorCode::ParseFailed,
        component: String::new(),
        path: String::new(),
        message,
        statement_id,
        severity: None,
    }
}

pub struct StreamParser {
    library: Arc<Library>,
    source: String,
}

impl StreamParser {
    pub fn new(library: Arc<Library>) -> Self {
        Self {
            library,
            source: String::new(),
        }
    }
    pub fn push(&mut self, chunk: &str) -> ParseResult {
        self.source.push_str(chunk);
        self.current(false)
    }
    pub fn set(&mut self, source: &str, complete: bool) -> ParseResult {
        self.source.clear();
        self.source.push_str(source);
        self.current(complete)
    }
    pub fn finish(&mut self) -> ParseResult {
        self.current(true)
    }

    fn current(&self, complete: bool) -> ParseResult {
        let mut result = parse_inner(&self.source, self.library.clone());
        if !complete {
            let tail = self
                .source
                .rsplit(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '$')
                .next()
                .unwrap_or("");
            if tail.starts_with('$')
                && tail.len() > 1
                && result
                    .state
                    .get(tail)
                    .is_some_and(serde_json::Value::is_null)
                && !result.statements.contains_key(tail)
            {
                let prefix = &self.source[..self.source.len().saturating_sub(tail.len())];
                if !prefix.contains(tail) {
                    result.state.shift_remove(tail);
                }
            }
        }
        result
    }
}
