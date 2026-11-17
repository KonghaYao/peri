use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "k")]
pub enum Expr {
    #[serde(rename = "Str")]
    String {
        v: String,
    },
    #[serde(rename = "Num")]
    Number {
        v: f64,
    },
    #[serde(rename = "Bool")]
    Bool {
        v: bool,
    },
    Null,
    #[serde(rename = "Arr")]
    Array {
        els: Vec<Expr>,
    },
    #[serde(rename = "Obj")]
    Object {
        entries: IndexMap<String, Expr>,
    },
    Ref {
        n: String,
    },
    Ph {
        n: String,
    },
    StateRef {
        n: String,
    },
    RuntimeRef {
        n: String,
        #[serde(rename = "refType")]
        ref_type: StatementKind,
    },
    #[serde(rename = "Comp")]
    Call {
        name: String,
        args: Vec<Expr>,
        #[serde(default, rename = "fn", skip_serializing_if = "std::ops::Not::not")]
        function: bool,
        #[serde(
            default,
            rename = "mappedProps",
            skip_serializing_if = "Option::is_none"
        )]
        mapped_props: Option<IndexMap<String, Expr>>,
    },
    BinOp {
        op: String,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    UnaryOp {
        op: String,
        operand: Box<Expr>,
    },
    Ternary {
        cond: Box<Expr>,
        #[serde(rename = "then")]
        then_expr: Box<Expr>,
        #[serde(rename = "else")]
        else_expr: Box<Expr>,
    },
    Member {
        obj: Box<Expr>,
        field: String,
    },
    Index {
        obj: Box<Expr>,
        index: Box<Expr>,
    },
    Assign {
        target: String,
        value: Box<Expr>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StatementKind {
    Value,
    State,
    Query,
    Mutation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Statement {
    pub id: String,
    pub kind: StatementKind,
    pub expr: Expr,
}

impl Expr {
    pub(crate) fn call(name: String, args: Vec<Self>, function: bool) -> Self {
        Self::Call {
            name,
            args,
            function,
            mapped_props: None,
        }
    }

    pub(crate) fn literal(value: &Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Bool(v) => Self::Bool { v: *v },
            Value::Number(v) => Self::Number {
                v: v.as_f64().unwrap_or(0.0),
            },
            Value::String(v) => Self::String { v: v.clone() },
            Value::Array(values) => Self::Array {
                els: values.iter().map(Self::literal).collect(),
            },
            Value::Object(values) => Self::Object {
                entries: values
                    .iter()
                    .map(|(k, v)| (k.clone(), Self::literal(v)))
                    .collect(),
            },
        }
    }

    pub(crate) fn literal_value(&self) -> Option<Value> {
        match self {
            Self::Null => Some(Value::Null),
            Self::String { v } => Some(Value::String(v.clone())),
            Self::Number { v } => Some(crate::builtins::number(*v)),
            Self::Bool { v } => Some(Value::Bool(*v)),
            Self::Array { els } => els
                .iter()
                .map(Self::literal_value)
                .collect::<Option<Vec<_>>>()
                .map(Value::Array),
            Self::Object { entries } => entries
                .iter()
                .map(|(k, v)| Some((k.clone(), v.literal_value()?)))
                .collect::<Option<serde_json::Map<_, _>>>()
                .map(Value::Object),
            _ => None,
        }
    }

    pub(crate) fn visit(&self, f: &mut impl FnMut(&Self)) {
        f(self);
        match self {
            Self::Call {
                args, mapped_props, ..
            } => {
                args.iter().for_each(|e| e.visit(f));
                if let Some(props) = mapped_props {
                    props.values().for_each(|e| e.visit(f));
                }
            }
            Self::Array { els } => els.iter().for_each(|e| e.visit(f)),
            Self::Object { entries } => entries.values().for_each(|e| e.visit(f)),
            Self::BinOp { left, right, .. } => {
                left.visit(f);
                right.visit(f);
            }
            Self::UnaryOp { operand, .. } => operand.visit(f),
            Self::Ternary {
                cond,
                then_expr,
                else_expr,
            } => {
                cond.visit(f);
                then_expr.visit(f);
                else_expr.visit(f);
            }
            Self::Member { obj, .. } => obj.visit(f),
            Self::Index { obj, index } => {
                obj.visit(f);
                index.visit(f);
            }
            Self::Assign { value, .. } => value.visit(f),
            _ => {}
        }
    }
}
