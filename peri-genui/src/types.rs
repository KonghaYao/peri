use std::sync::Arc;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;

use crate::{Expr, Library, Statement, StatementKind};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorCode {
    MissingRequired,
    NullRequired,
    UnknownComponent,
    InlineReserved,
    ExcessArgs,
    TypeMismatch,
    UnknownFunction,
    QueryCycle,
    NoRoot,
    ParseFailed,
    RuntimeError,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParseError {
    pub code: ErrorCode,
    pub component: String,
    pub path: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub statement_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Element {
    pub type_name: String,
    pub props: IndexMap<String, Value>,
    pub partial: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub statement_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    Run {
        #[serde(rename = "statementId")]
        statement_id: String,
        #[serde(rename = "refType")]
        ref_type: StatementKind,
    },
    ContinueConversation {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        context: Option<Value>,
    },
    OpenUrl {
        url: String,
    },
    Set {
        target: String,
        #[serde(rename = "valueAST")]
        value: Expr,
    },
    Reset {
        targets: Vec<String>,
    },
    CustomAction {
        name: String,
        params: IndexMap<String, Value>,
    },
}

#[derive(Debug, Clone, PartialEq, Error, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum EvalError {
    #[error("{feature} 执行未实现（statement: {statement_id}）")]
    Unsupported {
        feature: String,
        statement_id: String,
    },
    #[error("{message}")]
    Runtime {
        code: ErrorCode,
        message: String,
        statement_id: Option<String>,
        component: Option<String>,
        path: Option<String>,
    },
    #[error("表达式超过安全预算")]
    LimitExceeded,
}

impl EvalError {
    pub(crate) fn runtime(message: impl Into<String>) -> Self {
        Self::Runtime {
            code: ErrorCode::RuntimeError,
            message: message.into(),
            statement_id: None,
            component: None,
            path: None,
        }
    }

    pub(crate) fn fixture(&self) -> Value {
        match self {
            Self::Unsupported {
                feature,
                statement_id,
            } => {
                json!({"source": "runtime", "code": "unsupported", "statementId": statement_id, "component": feature})
            }
            Self::Runtime {
                code,
                statement_id,
                component,
                path,
                ..
            } => {
                let mut out = json!({"source": "runtime", "code": code});
                if let Some(id) = statement_id {
                    out["statementId"] = json!(id);
                }
                if let Some(name) = component {
                    out["component"] = json!(name);
                }
                if let Some(path) = path {
                    out["path"] = json!(path);
                }
                out
            }
            Self::LimitExceeded => json!({"source": "runtime", "code": "runtime-error"}),
        }
    }
}

#[derive(Debug, Clone)]
pub struct EvalResult {
    pub root: Option<Element>,
    pub errors: Vec<EvalError>,
    pub(crate) tree: Option<NodeValue>,
}

#[derive(Debug, Clone)]
pub struct ParseResult {
    pub root: Option<Element>,
    pub errors: Vec<ParseError>,
    pub unresolved: Vec<String>,
    pub orphans: Vec<String>,
    pub incomplete: bool,
    pub state: IndexMap<String, Value>,
    pub statements: IndexMap<String, Statement>,
    pub root_expression: Option<Expr>,
    pub(crate) entry: Option<String>,
    pub(crate) tree: Option<NodeValue>,
    pub(crate) library: Arc<Library>,
}

impl ParseResult {
    /// 估计结果独占保留的堆字节，包含公开投影、内部树和 AST；不包含共享 Library。
    /// String/Vec 按容量计，映射桶按条目估计；这不是 allocator 或 RSS 测量。
    pub fn retained_bytes(&self) -> usize {
        crate::memory::retained_bytes(self)
    }

    /// 使用声明的 state 默认值求值；自定义函数应调用带 EvalContext 的版本。
    pub fn to_fixture_json(&self) -> Value {
        self.to_fixture_json_with_context(&crate::EvalContext::default())
    }

    pub fn to_fixture_json_with_context(&self, context: &crate::EvalContext) -> Value {
        let evaluated = crate::evaluate_root(self, context);
        let mut error_objects: Vec<Value> = self
            .errors
            .iter()
            .map(|e| {
                let mut value =
                    json!({"source": "parser", "code": e.code, "component": e.component});
                if let Some(id) = &e.statement_id {
                    value["statementId"] = json!(id);
                }
                if !e.path.is_empty() {
                    value["path"] = json!(e.path);
                }
                value
            })
            .collect();
        error_objects.extend(evaluated.errors.iter().map(EvalError::fixture));
        let errors: Vec<_> = error_objects
            .iter()
            .map(|e| {
                let mut value = json!({"code": e["code"]});
                if let Some(id) = e.get("statementId") {
                    value["statementId"] = id.clone();
                }
                value
            })
            .collect();
        json!({
            "root": crate::evaluator::summarize(evaluated.tree.as_ref(), self, context),
            "errors": errors,
            "errorObjects": error_objects,
            "unresolved": self.unresolved,
            "orphans": self.orphans,
            "incomplete": self.incomplete,
            "state": self.state,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum NodeValue {
    Json(Value),
    Array(Vec<NodeValue>),
    Object(IndexMap<String, NodeValue>),
    Element {
        element: Element,
        props: IndexMap<String, NodeValue>,
    },
    Expr(Expr),
    Plan(Vec<Action>),
    Binding {
        target: String,
        expr: Expr,
    },
}

impl NodeValue {
    pub(crate) fn null() -> Self {
        Self::Json(Value::Null)
    }
    pub(crate) fn is_null(&self) -> bool {
        matches!(self, Self::Json(Value::Null))
    }
    pub(crate) fn public_json(&self) -> Value {
        match self {
            Self::Json(value) => value.clone(),
            Self::Array(values) => Value::Array(values.iter().map(Self::public_json).collect()),
            Self::Object(values) => Value::Object(
                values
                    .iter()
                    .map(|(k, v)| (k.clone(), v.public_json()))
                    .collect(),
            ),
            Self::Element { element, props } => {
                json!({"type": "element", "typeName": element.type_name, "props": props.iter().map(|(k, v)| (k.clone(), v.public_json())).collect::<serde_json::Map<_, _>>(), "partial": element.partial, "statementId": element.statement_id})
            }
            Self::Expr(expr) => serde_json::to_value(expr).unwrap_or(Value::Null),
            Self::Plan(steps) => json!({"steps": steps}),
            Self::Binding { target, expr } => {
                json!({"__reactive": "assign", "target": target, "expr": expr})
            }
        }
    }

    pub(crate) fn element(&self) -> Option<Element> {
        if let Self::Element { element, props } = self {
            let mut out = element.clone();
            out.props = props
                .iter()
                .map(|(k, v)| (k.clone(), v.public_json()))
                .collect();
            Some(out)
        } else {
            None
        }
    }
}
