use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::Value;

use crate::{
    builtins::{self, is_action, js_string, loose_eq, number, to_number, truthy},
    library::CallDef,
    types::NodeValue,
    validation::Validation,
    Element, ErrorCode, EvalError, EvalResult, Expr, Library, ParseResult, StatementKind,
    MAX_DEPTH, MAX_NODES, MAX_VALUE_BYTES,
};

pub type RuntimeFunction =
    Arc<dyn Fn(&IndexMap<String, Value>) -> crate::Result<Value> + Send + Sync>;

#[derive(Clone, Default)]
pub struct EvalContext {
    pub state: IndexMap<String, Value>,
    pub extra_scope: IndexMap<String, Value>,
    pub references: IndexMap<String, Value>,
    pub functions: IndexMap<String, RuntimeFunction>,
}

impl EvalContext {
    // 复用同一求值器和安全预算，让宿主点击时读取最新 state，而不重写表达式语义。
    pub fn evaluate_expression(&self, result: &ParseResult, expr: &Expr) -> crate::Result<Value> {
        let mut engine = Engine::new(&result.library, &result.state, self);
        let value = engine.expr(expr, 0)?;
        if let Some(error) = engine.errors.into_iter().next() {
            return Err(error);
        }
        Ok(value.public_json())
    }
}

pub fn evaluate_root(result: &ParseResult, context: &EvalContext) -> EvalResult {
    let mut engine = Engine::new(&result.library, &result.state, context);
    engine.statement_id = result.entry.clone();
    // 即使声明尚未被根读取，也不能让调用方误以为后台执行已受到支持。
    for statement in result.statements.values() {
        if matches!(
            statement.kind,
            StatementKind::Query | StatementKind::Mutation
        ) {
            engine.report(EvalError::Unsupported {
                feature: format!("{:?}", statement.kind),
                statement_id: statement.id.clone(),
            });
        }
    }
    let value = if let Some(tree) = &result.tree {
        engine.value(tree, false, 0)
    } else if let Some(expr) = &result.root_expression {
        engine.expr(expr, 0)
    } else {
        Ok(NodeValue::null())
    };
    let tree = match value {
        Ok(value @ NodeValue::Element { .. }) => Some(value),
        Ok(_) => {
            if result.root_expression.is_some() {
                engine.report(EvalError::Runtime {
                    code: ErrorCode::NoRoot,
                    message: "root 不求值为组件".into(),
                    statement_id: Some("root".into()),
                    component: None,
                    path: None,
                });
            }
            None
        }
        Err(error) => {
            engine.report(error);
            None
        }
    };
    EvalResult {
        root: tree.as_ref().and_then(NodeValue::element),
        errors: engine.errors,
        tree,
    }
}

pub(crate) fn default_value(
    value: &NodeValue,
    library: &Library,
    state: &IndexMap<String, Value>,
) -> crate::Result<Value> {
    match value {
        NodeValue::Expr(expr) => {
            let mut only_functions = true;
            expr.visit(&mut |e| {
                if let Expr::Call { name, .. } = e {
                    only_functions &= builtins::FUNCTIONS.iter().any(|(n, _)| *n == name);
                }
            });
            if !only_functions {
                return Ok(Value::Null);
            }
            let context = EvalContext::default();
            let mut engine = Engine::new(library, state, &context);
            engine.value(value, false, 0).map(|v| v.public_json())
        }
        NodeValue::Array(values) => Ok(Value::Array(
            values
                .iter()
                .map(|v| default_value(v, library, state))
                .collect::<crate::Result<_>>()?,
        )),
        NodeValue::Object(values) => Ok(Value::Object(
            values
                .iter()
                .map(|(k, v)| Ok((k.clone(), default_value(v, library, state)?)))
                .collect::<crate::Result<_>>()?,
        )),
        other => Ok(other.public_json()),
    }
}

pub(crate) struct Engine<'a> {
    pub library: &'a Library,
    pub state: Cow<'a, IndexMap<String, Value>>,
    pub context: &'a EvalContext,
    pub scope: IndexMap<String, Value>,
    pub errors: Vec<EvalError>,
    statement_id: Option<String>,
    remaining: usize,
}

impl<'a> Engine<'a> {
    fn new(
        library: &'a Library,
        defaults: &'a IndexMap<String, Value>,
        context: &'a EvalContext,
    ) -> Self {
        let state = if context.state.is_empty() {
            Cow::Borrowed(defaults)
        } else {
            let mut state = defaults.clone();
            state.extend(context.state.clone());
            Cow::Owned(state)
        };
        Self {
            library,
            state,
            context,
            scope: IndexMap::new(),
            errors: Vec::new(),
            statement_id: None,
            remaining: MAX_NODES,
        }
    }

    pub fn report(&mut self, error: EvalError) {
        if !self.errors.contains(&error) {
            self.errors.push(error);
        }
    }

    fn enter(&mut self, depth: usize) -> crate::Result<()> {
        if depth >= MAX_DEPTH || self.remaining == 0 {
            return Err(EvalError::LimitExceeded);
        }
        self.remaining -= 1;
        Ok(())
    }

    pub fn expr(&mut self, expr: &Expr, depth: usize) -> crate::Result<NodeValue> {
        self.enter(depth)?;
        let value = match expr {
            Expr::String { v } => NodeValue::Json(Value::String(v.clone())),
            Expr::Number { v } => NodeValue::Json(number(*v)),
            Expr::Bool { v } => NodeValue::Json(Value::Bool(*v)),
            Expr::Null | Expr::Ph { .. } => NodeValue::null(),
            Expr::StateRef { n } => NodeValue::Json(
                self.context
                    .extra_scope
                    .get(n)
                    .filter(|v| !v.is_null())
                    .or_else(|| self.state.get(n))
                    .cloned()
                    .unwrap_or(Value::Null),
            ),
            Expr::Ref { n } => NodeValue::Json(
                self.scope
                    .get(n)
                    .or_else(|| self.context.references.get(n))
                    .cloned()
                    .unwrap_or(Value::Null),
            ),
            Expr::RuntimeRef { n, ref_type } => {
                return Err(EvalError::Unsupported {
                    feature: format!("{ref_type:?}"),
                    statement_id: n.clone(),
                })
            }
            Expr::Array { els } => {
                let mut values = Vec::new();
                let mut bytes = 0usize;
                for expr in els {
                    let value = self.expr(expr, depth + 1)?;
                    bytes = bytes
                        .saturating_add(std::mem::size_of::<NodeValue>())
                        .saturating_add(crate::memory::node_value_bytes(&value));
                    check_value_size(bytes)?;
                    values.push(value);
                }
                NodeValue::Array(values)
            }
            Expr::Object { entries } => {
                let mut values = IndexMap::new();
                let mut bytes = 0usize;
                for (key, expr) in entries {
                    let value = self.expr(expr, depth + 1)?;
                    bytes = bytes
                        .saturating_add(std::mem::size_of::<(String, NodeValue)>())
                        .saturating_add(key.capacity())
                        .saturating_add(crate::memory::node_value_bytes(&value));
                    check_value_size(bytes)?;
                    values.insert(key.clone(), value);
                }
                NodeValue::Object(values)
            }
            Expr::BinOp { op, left, right } => {
                let left = self.expr(left, depth + 1)?;
                let left_json = left.public_json();
                if op == "&&" {
                    return if truthy(&left_json) {
                        self.expr(right, depth + 1)
                    } else {
                        Ok(left)
                    };
                }
                if op == "||" {
                    return if truthy(&left_json) {
                        Ok(left)
                    } else {
                        self.expr(right, depth + 1)
                    };
                }
                let right = self.expr(right, depth + 1)?.public_json();
                if op == "+" && (left_json.is_string() || right.is_string()) {
                    check_value_size(
                        string_bytes(&left_json).saturating_add(string_bytes(&right)),
                    )?;
                }
                NodeValue::Json(binary(op, &left_json, &right))
            }
            Expr::UnaryOp { op, operand } => {
                let value = self.expr(operand, depth + 1)?.public_json();
                NodeValue::Json(if op == "!" {
                    Value::Bool(!truthy(&value))
                } else {
                    number(-to_number(&value))
                })
            }
            Expr::Ternary {
                cond,
                then_expr,
                else_expr,
            } => {
                let condition = self.expr(cond, depth + 1)?.public_json();
                return self.expr(
                    if truthy(&condition) {
                        then_expr
                    } else {
                        else_expr
                    },
                    depth + 1,
                );
            }
            Expr::Member { obj, field } => {
                let object = self.expr(obj, depth + 1)?.public_json();
                let value = if let Value::Array(values) = &object {
                    if field == "length" {
                        Value::from(values.len())
                    } else {
                        Value::Array(
                            values
                                .iter()
                                .map(|v| builtins::direct_field(v, field))
                                .collect(),
                        )
                    }
                } else {
                    builtins::direct_field(&object, field)
                };
                NodeValue::Json(value)
            }
            Expr::Index { obj, index } => {
                let object = self.expr(obj, depth + 1)?.public_json();
                let index = self.expr(index, depth + 1)?.public_json();
                let key = if object.is_array() {
                    to_number(&index).to_string()
                } else {
                    js_string(&index)
                };
                NodeValue::Json(if index.is_null() {
                    Value::Null
                } else {
                    builtins::direct_field(&object, &key)
                })
            }
            Expr::Assign { target, value } => NodeValue::Binding {
                target: target.clone(),
                expr: crate::actions::capture(value, &self.scope),
            },
            Expr::Call {
                name,
                args,
                mapped_props,
                ..
            } => {
                if builtins::is_reserved(name) {
                    return Err(EvalError::Unsupported {
                        feature: name.clone(),
                        statement_id: String::new(),
                    });
                }
                if name == "Each" {
                    return self.each(args, depth);
                }
                if is_action(name) {
                    return self.action(name, args, depth);
                }
                if builtins::FUNCTIONS.iter().any(|(n, _)| *n == name) {
                    let mut values = Vec::new();
                    for arg in args {
                        values.push(self.expr(arg, depth + 1)?.public_json());
                    }
                    NodeValue::Json(builtins::evaluate(name, &values)?)
                } else if let Some(def) = self.library.functions.get(name).cloned() {
                    self.function(name, args, mapped_props.as_ref(), &def, depth)?
                } else if let Some(def) = self.library.actions.get(name).cloned() {
                    self.custom_action(name, args, mapped_props.as_ref(), &def, depth)?
                } else if self.library.components.contains_key(name) {
                    let Some(def) = self.library.components.get(name) else {
                        return Ok(NodeValue::null());
                    };
                    let props = mapped_props.clone().unwrap_or_else(|| {
                        def.params
                            .iter()
                            .zip(args)
                            .map(|(p, e)| (p.name.clone(), e.clone()))
                            .collect()
                    });
                    let props = props
                        .into_iter()
                        .map(|(k, e)| (k, NodeValue::Expr(e)))
                        .collect();
                    self.value(
                        &NodeValue::Element {
                            element: Element {
                                type_name: name.clone(),
                                props: IndexMap::new(),
                                partial: false,
                                statement_id: None,
                            },
                            props,
                        },
                        false,
                        depth + 1,
                    )?
                } else {
                    NodeValue::null()
                }
            }
        };
        Ok(value)
    }

    pub fn value(
        &mut self,
        value: &NodeValue,
        binding: bool,
        depth: usize,
    ) -> crate::Result<NodeValue> {
        self.enter(depth)?;
        match value {
            NodeValue::Expr(Expr::StateRef { n }) if binding => Ok(NodeValue::Binding {
                target: n.clone(),
                expr: Expr::StateRef { n: "$value".into() },
            }),
            NodeValue::Expr(expr) => {
                let value = self.expr(expr, depth + 1)?;
                if let NodeValue::Binding { target, .. } = &value {
                    if !binding {
                        return Ok(NodeValue::Json(
                            self.state.get(target).cloned().unwrap_or(Value::Null),
                        ));
                    }
                }
                Ok(value)
            }
            NodeValue::Array(values) => {
                let mut out = Vec::new();
                let mut bytes = 0usize;
                for value in values {
                    let value = self.value(value, binding, depth + 1)?;
                    bytes = bytes
                        .saturating_add(std::mem::size_of::<NodeValue>())
                        .saturating_add(crate::memory::node_value_bytes(&value));
                    check_value_size(bytes)?;
                    out.push(value);
                }
                Ok(NodeValue::Array(out))
            }
            NodeValue::Object(values) => {
                let mut out = IndexMap::new();
                let mut bytes = 0usize;
                for (key, value) in values {
                    let value = self.value(value, binding, depth + 1)?;
                    bytes = bytes
                        .saturating_add(std::mem::size_of::<(String, NodeValue)>())
                        .saturating_add(key.capacity())
                        .saturating_add(crate::memory::node_value_bytes(&value));
                    check_value_size(bytes)?;
                    out.insert(key.clone(), value);
                }
                Ok(NodeValue::Object(out))
            }
            NodeValue::Element { element, props } => {
                let previous = self.statement_id.clone();
                if element.statement_id.is_some() {
                    self.statement_id = element.statement_id.clone();
                }
                let component = self.library.components.get(&element.type_name).cloned();
                let mut evaluated = IndexMap::new();
                let mut present = HashSet::new();
                let mut invalid = false;
                for (key, value) in props {
                    present.insert(key.clone());
                    let param = component
                        .as_ref()
                        .and_then(|c| c.params.iter().find(|p| p.name == *key));
                    let binding = param.is_some_and(|p| p.binding);
                    let value = match self.value(value, binding, depth + 1) {
                        Ok(value) => value,
                        Err(mut error) => {
                            if let EvalError::Runtime {
                                statement_id,
                                component,
                                path,
                                ..
                            } = &mut error
                            {
                                if statement_id.is_none() {
                                    *statement_id = self.statement_id.clone();
                                }
                                if component.is_none() {
                                    *component = Some(element.type_name.clone());
                                }
                                if path.is_none() {
                                    *path = Some(format!("/{key}"));
                                }
                            }
                            self.report(error);
                            evaluated.insert(key.clone(), NodeValue::null());
                            if param.is_some_and(|p| p.required) {
                                invalid = true;
                            }
                            continue;
                        }
                    };
                    let Some(param) = param else {
                        evaluated.insert(key.clone(), crate::actions::own_steps(value));
                        continue;
                    };
                    let (checked, errors) =
                        if matches!(value, NodeValue::Binding { .. } | NodeValue::Expr(_)) {
                            (Some(value), Vec::new())
                        } else {
                            let mut validation = Validation::new(self.library, false);
                            let checked = validation.validate(
                                value,
                                &param.schema,
                                &element.type_name,
                                &format!("/{key}"),
                                0,
                            );
                            (checked, validation.errors)
                        };
                    for error in errors {
                        self.report_evaluated_prop_error(&element.type_name, key, error);
                    }
                    match checked {
                        Some(value) if value.is_null() && param.required => {
                            if let Some(default) = &param.schema.default {
                                evaluated.insert(key.clone(), NodeValue::Json(default.clone()));
                            } else {
                                self.report_missing_prop(
                                    ErrorCode::NullRequired,
                                    &element.type_name,
                                    key,
                                    format!("缺少非空必填参数 {key}"),
                                );
                                invalid = true;
                            }
                        }
                        Some(value) => {
                            evaluated.insert(key.clone(), crate::actions::own_steps(value));
                        }
                        None => {
                            if let Some(default) = &param.schema.default {
                                evaluated.insert(key.clone(), NodeValue::Json(default.clone()));
                            } else if param.required {
                                invalid = true;
                            }
                        }
                    }
                }
                if let Some(component) = &component {
                    for param in &component.params {
                        if !param.required || present.contains(&param.name) {
                            continue;
                        }
                        if let Some(default) = &param.schema.default {
                            evaluated.insert(param.name.clone(), NodeValue::Json(default.clone()));
                        } else {
                            self.report_missing_prop(
                                ErrorCode::MissingRequired,
                                &element.type_name,
                                &param.name,
                                format!("缺少非空必填参数 {}", param.name),
                            );
                            invalid = true;
                        }
                    }
                }
                self.statement_id = previous;
                if invalid {
                    Ok(NodeValue::null())
                } else {
                    Ok(NodeValue::Element {
                        element: element.clone(),
                        props: evaluated,
                    })
                }
            }
            other => Ok(other.clone()),
        }
    }

    fn report_evaluated_prop_error(
        &mut self,
        element_type: &str,
        key: &str,
        error: crate::ParseError,
    ) {
        self.report(EvalError::Runtime {
            code: error.code,
            message: error.message,
            statement_id: self.statement_id.clone(),
            component: Some(if error.component.is_empty() {
                element_type.to_owned()
            } else {
                error.component
            }),
            path: Some(if error.path.is_empty() {
                format!("/{key}")
            } else {
                error.path
            }),
        });
    }

    fn report_missing_prop(
        &mut self,
        code: ErrorCode,
        element_type: &str,
        key: &str,
        message: String,
    ) {
        self.report(EvalError::Runtime {
            code,
            message,
            statement_id: self.statement_id.clone(),
            component: Some(element_type.to_owned()),
            path: Some(format!("/{key}")),
        });
    }

    fn function(
        &mut self,
        name: &str,
        args: &[Expr],
        mapped: Option<&IndexMap<String, Expr>>,
        def: &CallDef,
        depth: usize,
    ) -> crate::Result<NodeValue> {
        let Some(values) = self.call_args(name, args, mapped, def, false, depth)? else {
            return Ok(NodeValue::null());
        };
        let Some(function) = self.context.functions.get(name) else {
            return Err(EvalError::runtime(format!("@{name} 缺少运行时实现")));
        };
        let result = function(&values)?;
        check_value_size(crate::memory::value_bytes(&result))?;
        if let Some(returns) = &def.returns {
            let mut validation = Validation::new(self.library, false);
            let result = validation.validate(NodeValue::Json(result), returns, name, "/returns", 0);
            if !validation.errors.is_empty() {
                return Err(EvalError::runtime(
                    validation
                        .errors
                        .iter()
                        .map(|e| e.message.as_str())
                        .collect::<Vec<_>>()
                        .join("; "),
                ));
            }
            return Ok(result.unwrap_or_else(NodeValue::null));
        }
        Ok(NodeValue::Json(result))
    }

    pub fn call_args(
        &mut self,
        name: &str,
        args: &[Expr],
        mapped: Option<&IndexMap<String, Expr>>,
        def: &CallDef,
        strict: bool,
        depth: usize,
    ) -> crate::Result<Option<IndexMap<String, Value>>> {
        let named = mapped.cloned().unwrap_or_else(|| {
            def.params
                .properties
                .keys()
                .zip(args)
                .map(|(k, e)| (k.clone(), e.clone()))
                .collect()
        });
        let mut values = IndexMap::new();
        for (key, expr) in named {
            let value = self.expr(&expr, depth + 1)?.public_json();
            if !value.is_null() || def.params.required.contains(&key) {
                values.insert(key, value);
            }
        }
        if def.params.required.iter().any(|k| {
            values.get(k).is_none_or(Value::is_null)
                && def
                    .params
                    .properties
                    .get(k)
                    .is_none_or(|s| s.default.is_none())
        }) {
            return Ok(None);
        }
        let mut validation = Validation::new(self.library, false);
        let checked = validation.validate(
            NodeValue::Object(
                values
                    .into_iter()
                    .map(|(k, v)| (k, NodeValue::Json(v)))
                    .collect(),
            ),
            &def.params,
            name,
            "",
            0,
        );
        if !validation.errors.is_empty() {
            self.report(EvalError::runtime(
                validation
                    .errors
                    .iter()
                    .map(|e| e.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; "),
            ));
            if strict {
                return Ok(None);
            }
        }
        Ok(match checked {
            Some(NodeValue::Object(values)) => Some(
                values
                    .into_iter()
                    .map(|(k, v)| (k, v.public_json()))
                    .collect(),
            ),
            _ => None,
        })
    }

    fn each(&mut self, args: &[Expr], depth: usize) -> crate::Result<NodeValue> {
        let [array, var, template, ..] = args else {
            return Ok(NodeValue::Array(Vec::new()));
        };
        let array = self.expr(array, depth + 1)?.public_json();
        let Some(array) = array.as_array() else {
            return Ok(NodeValue::Array(Vec::new()));
        };
        let name = match var {
            Expr::Ref { n } | Expr::String { v: n } => n,
            _ => return Ok(NodeValue::Array(Vec::new())),
        };
        let mut values = Vec::new();
        let mut bytes = 0usize;
        for item in array {
            let mut scope = self.scope.clone();
            scope.insert(name.clone(), item.clone());
            let template = crate::actions::capture(template, &scope);
            let value = self.expr(&template, depth + 1)?;
            bytes = bytes.saturating_add(
                std::mem::size_of::<NodeValue>()
                    .saturating_add(crate::memory::node_value_bytes(&value)),
            );
            check_value_size(bytes)?;
            values.push(value);
        }
        Ok(NodeValue::Array(values))
    }
}

fn check_value_size(bytes: usize) -> crate::Result<()> {
    if bytes > MAX_VALUE_BYTES {
        return Err(EvalError::LimitExceeded);
    }
    Ok(())
}

fn string_bytes(value: &Value) -> usize {
    match value {
        Value::Null => 0,
        Value::String(text) => text.len(),
        Value::Bool(true) => 4,
        Value::Bool(false) => 5,
        Value::Number(_) => 24,
        Value::Array(values) => values
            .iter()
            .map(string_bytes)
            .sum::<usize>()
            .saturating_add(values.len().saturating_sub(1)),
        Value::Object(_) => 15,
    }
}

fn binary(op: &str, a: &Value, b: &Value) -> Value {
    match op {
        "+" if a.is_string() || b.is_string() => Value::String(js_string(a) + &js_string(b)),
        "+" => number(to_number(a) + to_number(b)),
        "-" => number(to_number(a) - to_number(b)),
        "*" => number(to_number(a) * to_number(b)),
        "/" => number(if to_number(b) == 0.0 {
            0.0
        } else {
            to_number(a) / to_number(b)
        }),
        "%" => number(if to_number(b) == 0.0 {
            0.0
        } else {
            to_number(a) % to_number(b)
        }),
        "==" => Value::Bool(loose_eq(a, b)),
        "!=" => Value::Bool(!loose_eq(a, b)),
        ">" => Value::Bool(to_number(a) > to_number(b)),
        "<" => Value::Bool(to_number(a) < to_number(b)),
        ">=" => Value::Bool(to_number(a) >= to_number(b)),
        "<=" => Value::Bool(to_number(a) <= to_number(b)),
        _ => Value::Null,
    }
}

pub(crate) fn summarize(
    value: Option<&NodeValue>,
    result: &ParseResult,
    context: &EvalContext,
) -> Value {
    let mut engine = Engine::new(&result.library, &result.state, context);
    crate::actions::summary(value, &mut engine)
}
