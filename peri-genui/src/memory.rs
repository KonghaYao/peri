use std::mem::size_of;

use indexmap::IndexMap;
use serde_json::Value;

use crate::{types::NodeValue, Action, Element, Expr, ParseResult, Statement};

pub(crate) fn retained_bytes(result: &ParseResult) -> usize {
    result
        .root
        .as_ref()
        .map_or(0, Heap::heap)
        .saturating_add(
            result
                .errors
                .capacity()
                .saturating_mul(size_of::<crate::ParseError>()),
        )
        .saturating_add(
            result
                .errors
                .iter()
                .map(|error| {
                    error.component.capacity()
                        + error.path.capacity()
                        + error.message.capacity()
                        + error.statement_id.as_ref().map_or(0, String::capacity)
                        + error.severity.as_ref().map_or(0, String::capacity)
                })
                .sum::<usize>(),
        )
        .saturating_add(result.unresolved.heap())
        .saturating_add(result.orphans.heap())
        .saturating_add(result.state.heap())
        .saturating_add(result.statements.heap())
        .saturating_add(result.root_expression.as_ref().map_or(0, Heap::heap))
        .saturating_add(result.entry.as_ref().map_or(0, String::capacity))
        .saturating_add(result.tree.as_ref().map_or(0, Heap::heap))
}

pub(crate) fn value_bytes(value: &Value) -> usize {
    value.heap()
}

pub(crate) fn node_value_bytes(value: &NodeValue) -> usize {
    value.heap()
}

trait Heap {
    fn heap(&self) -> usize;
}
impl Heap for String {
    fn heap(&self) -> usize {
        self.capacity()
    }
}
impl<T: Heap> Heap for Vec<T> {
    fn heap(&self) -> usize {
        self.iter().fold(
            self.capacity().saturating_mul(size_of::<T>()),
            |bytes, value| bytes.saturating_add(value.heap()),
        )
    }
}
impl<T: Heap> Heap for IndexMap<String, T> {
    fn heap(&self) -> usize {
        // IndexMap 的索引桶不公开，按每项两个 usize 估计，不声称 allocator 精确值。
        self.iter().fold(
            self.capacity()
                .saturating_mul(size_of::<(String, T)>() + 2 * size_of::<usize>()),
            |bytes, (key, value)| {
                bytes
                    .saturating_add(key.capacity())
                    .saturating_add(value.heap())
            },
        )
    }
}
impl Heap for Value {
    fn heap(&self) -> usize {
        match self {
            Self::String(text) => text.heap(),
            Self::Array(values) => values.heap(),
            Self::Object(values) => values.iter().fold(
                values
                    .len()
                    .saturating_mul(size_of::<(String, Value)>() + 3 * size_of::<usize>()),
                |bytes, (key, value)| {
                    bytes
                        .saturating_add(key.capacity())
                        .saturating_add(value.heap())
                },
            ),
            _ => 0,
        }
    }
}
impl Heap for Element {
    fn heap(&self) -> usize {
        self.type_name
            .capacity()
            .saturating_add(self.props.heap())
            .saturating_add(self.statement_id.as_ref().map_or(0, String::capacity))
    }
}
impl Heap for Statement {
    fn heap(&self) -> usize {
        self.id.capacity().saturating_add(self.expr.heap())
    }
}
impl Heap for Expr {
    fn heap(&self) -> usize {
        let boxed = |expr: &Expr| size_of::<Expr>().saturating_add(expr.heap());
        match self {
            Self::String { v } => v.capacity(),
            Self::Number { .. } | Self::Bool { .. } | Self::Null => 0,
            Self::Array { els } => els.heap(),
            Self::Object { entries } => entries.heap(),
            Self::Ref { n }
            | Self::Ph { n }
            | Self::StateRef { n }
            | Self::RuntimeRef { n, .. } => n.capacity(),
            Self::Call {
                name,
                args,
                mapped_props,
                ..
            } => name
                .capacity()
                .saturating_add(args.heap())
                .saturating_add(mapped_props.as_ref().map_or(0, Heap::heap)),
            Self::BinOp { op, left, right } => op
                .capacity()
                .saturating_add(boxed(left))
                .saturating_add(boxed(right)),
            Self::UnaryOp { op, operand } => op.capacity().saturating_add(boxed(operand)),
            Self::Ternary {
                cond,
                then_expr,
                else_expr,
            } => boxed(cond)
                .saturating_add(boxed(then_expr))
                .saturating_add(boxed(else_expr)),
            Self::Member { obj, field } => boxed(obj).saturating_add(field.capacity()),
            Self::Index { obj, index } => boxed(obj).saturating_add(boxed(index)),
            Self::Assign { target, value } => target.capacity().saturating_add(boxed(value)),
        }
    }
}
impl Heap for Action {
    fn heap(&self) -> usize {
        match self {
            Self::Run { statement_id, .. } => statement_id.capacity(),
            Self::ContinueConversation { message, context } => message
                .capacity()
                .saturating_add(context.as_ref().map_or(0, Heap::heap)),
            Self::OpenUrl { url } => url.capacity(),
            Self::Set { target, value } => target.capacity().saturating_add(value.heap()),
            Self::Reset { targets } => targets.heap(),
            Self::CustomAction { name, params } => name.capacity().saturating_add(params.heap()),
        }
    }
}
impl Heap for NodeValue {
    fn heap(&self) -> usize {
        match self {
            Self::Json(value) => value.heap(),
            Self::Array(values) => values.heap(),
            Self::Object(values) => values.heap(),
            Self::Element { element, props } => element.heap().saturating_add(props.heap()),
            Self::Expr(expr) => expr.heap(),
            Self::Plan(actions) => actions.heap(),
            Self::Binding { target, expr } => target.capacity().saturating_add(expr.heap()),
        }
    }
}

#[test]
fn parser_payload_is_charged_separately_from_shared_library() {
    let library = crate::Library::from_json_str(
        r#"{"root":"Text","$defs":{"Text":{"properties":{"text":{"type":"string"}}}}}"#,
    )
    .unwrap();
    let short = crate::parse("root = Text(\"short\")", &library);
    let long = crate::parse(
        &format!("root = Text(\"{}\")", "内容".repeat(1024)),
        &library,
    );
    assert!(long.retained_bytes() > short.retained_bytes() + 6_000);
}
