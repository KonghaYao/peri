use indexmap::IndexMap;
use serde_json::{json, Value};

use crate::{
    builtins::js_string, evaluator::Engine, library::CallDef, types::NodeValue, Action, EvalError,
    Expr, MAX_VALUE_BYTES,
};

impl Engine<'_> {
    pub(crate) fn action(
        &mut self,
        name: &str,
        args: &[Expr],
        depth: usize,
    ) -> crate::Result<NodeValue> {
        let steps = match name {
            "Action" => {
                let list = if let Some(arg) = args.first() {
                    self.expr(arg, depth + 1)?
                } else {
                    NodeValue::null()
                };
                if let NodeValue::Array(values) = list {
                    values
                        .into_iter()
                        .flat_map(|v| {
                            if let NodeValue::Plan(steps) = v {
                                steps
                            } else {
                                Vec::new()
                            }
                        })
                        .collect()
                } else {
                    Vec::new()
                }
            }
            "Run" => match args.first() {
                Some(Expr::RuntimeRef { n, ref_type }) => vec![Action::Run {
                    statement_id: n.clone(),
                    ref_type: *ref_type,
                }],
                _ => Vec::new(),
            },
            "Set" => match args {
                [Expr::StateRef { n }, value, ..] => {
                    let mut budget = MAX_VALUE_BYTES;
                    vec![Action::Set {
                        target: n.clone(),
                        value: capture(value, &self.scope, &mut budget)?,
                    }]
                }
                _ => Vec::new(),
            },
            "Reset" => {
                let targets: Vec<_> = args
                    .iter()
                    .filter_map(|e| {
                        if let Expr::StateRef { n } = e {
                            Some(n.clone())
                        } else {
                            None
                        }
                    })
                    .collect();
                if targets.is_empty() {
                    Vec::new()
                } else {
                    vec![Action::Reset { targets }]
                }
            }
            "ToAssistant" => {
                let message = if let Some(arg) = args.first() {
                    js_string(&self.expr(arg, depth + 1)?.public_json())
                } else {
                    String::new()
                };
                let context = if let Some(arg) = args.get(1) {
                    Some(self.expr(arg, depth + 1)?.public_json()).filter(|v| !v.is_null())
                } else {
                    None
                };
                vec![Action::ContinueConversation { message, context }]
            }
            "OpenUrl" => {
                let url = if let Some(arg) = args.first() {
                    js_string(&self.expr(arg, depth + 1)?.public_json())
                } else {
                    String::new()
                };
                vec![Action::OpenUrl { url }]
            }
            _ => Vec::new(),
        };
        Ok(NodeValue::Plan(steps))
    }

    pub(crate) fn custom_action(
        &mut self,
        name: &str,
        args: &[Expr],
        mapped: Option<&IndexMap<String, Expr>>,
        def: &CallDef,
        depth: usize,
    ) -> crate::Result<NodeValue> {
        let steps = self
            .call_args(name, args, mapped, def, true, depth)?
            .map(|params| {
                vec![Action::CustomAction {
                    name: name.into(),
                    params,
                }]
            })
            .unwrap_or_default();
        Ok(NodeValue::Plan(steps))
    }
}

pub(crate) fn capture(
    expr: &Expr,
    scope: &IndexMap<String, Value>,
    budget: &mut usize,
) -> crate::Result<Expr> {
    match expr {
        Expr::Ref { n } => {
            let Some(value) = scope.get(n) else {
                return Ok(expr.clone());
            };
            spend_capture_bytes(budget, crate::memory::value_bytes(value))?;
            Ok(Expr::literal(value))
        }
        Expr::Call {
            name,
            args,
            function,
            mapped_props,
        } => {
            // Each 是绑定边界：input 仍在外层作用域，template 不能看到同名循环变量。
            let (args, mapped_props) = if name == "Each" && args.len() >= 3 {
                let mut inner = scope.clone();
                if let Some(var) = each_var_name(&args[1]) {
                    inner.shift_remove(var);
                }
                let mut captured = Vec::with_capacity(args.len());
                captured.push(capture(&args[0], scope, budget)?);
                captured.push(args[1].clone());
                for arg in &args[2..] {
                    captured.push(capture(arg, &inner, budget)?);
                }
                let mapped = match mapped_props {
                    Some(props) => {
                        let mut captured = IndexMap::new();
                        for (key, expr) in props {
                            captured.insert(key.clone(), capture(expr, &inner, budget)?);
                        }
                        Some(captured)
                    }
                    None => None,
                };
                (captured, mapped)
            } else {
                let mut captured = Vec::with_capacity(args.len());
                for arg in args {
                    captured.push(capture(arg, scope, budget)?);
                }
                let mapped = match mapped_props {
                    Some(props) => {
                        let mut captured = IndexMap::new();
                        for (key, expr) in props {
                            captured.insert(key.clone(), capture(expr, scope, budget)?);
                        }
                        Some(captured)
                    }
                    None => None,
                };
                (captured, mapped)
            };
            Ok(Expr::Call {
                name: name.clone(),
                args,
                function: *function,
                mapped_props,
            })
        }
        Expr::Array { els } => {
            let mut captured = Vec::with_capacity(els.len());
            for expr in els {
                captured.push(capture(expr, scope, budget)?);
            }
            Ok(Expr::Array { els: captured })
        }
        Expr::Object { entries } => {
            let mut captured = IndexMap::new();
            for (key, expr) in entries {
                captured.insert(key.clone(), capture(expr, scope, budget)?);
            }
            Ok(Expr::Object { entries: captured })
        }
        Expr::BinOp { op, left, right } => Ok(Expr::BinOp {
            op: op.clone(),
            left: Box::new(capture(left, scope, budget)?),
            right: Box::new(capture(right, scope, budget)?),
        }),
        Expr::UnaryOp { op, operand } => Ok(Expr::UnaryOp {
            op: op.clone(),
            operand: Box::new(capture(operand, scope, budget)?),
        }),
        Expr::Ternary {
            cond,
            then_expr,
            else_expr,
        } => Ok(Expr::Ternary {
            cond: Box::new(capture(cond, scope, budget)?),
            then_expr: Box::new(capture(then_expr, scope, budget)?),
            else_expr: Box::new(capture(else_expr, scope, budget)?),
        }),
        Expr::Member { obj, field } => {
            let obj = capture(obj, scope, budget)?;
            if let Expr::Object { entries } = &obj {
                if let Some(value) = entries.get(field) {
                    return Ok(value.clone());
                }
            }
            Ok(Expr::Member {
                obj: Box::new(obj),
                field: field.clone(),
            })
        }
        Expr::Index { obj, index } => Ok(Expr::Index {
            obj: Box::new(capture(obj, scope, budget)?),
            index: Box::new(capture(index, scope, budget)?),
        }),
        Expr::Assign { target, value } => Ok(Expr::Assign {
            target: target.clone(),
            value: Box::new(capture(value, scope, budget)?),
        }),
        _ => Ok(expr.clone()),
    }
}

fn spend_capture_bytes(budget: &mut usize, bytes: usize) -> crate::Result<()> {
    if bytes > *budget {
        *budget = 0;
        return Err(EvalError::runtime("生成值超过安全预算"));
    }
    *budget -= bytes;
    Ok(())
}

pub(crate) fn each_var_name(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Ref { n } | Expr::String { v: n } => Some(n),
        _ => None,
    }
}

pub(crate) fn own_steps(value: NodeValue) -> NodeValue {
    // 只有类型化调用能生成自定义 action，数据外形不能授予该身份。
    match value {
        NodeValue::Object(mut fields) => {
            if let Some(NodeValue::Array(steps)) = fields.get_mut("steps") {
                steps.retain(|step| {
                    step.public_json().get("type").and_then(Value::as_str) != Some("custom_action")
                });
            }
            NodeValue::Object(fields)
        }
        NodeValue::Json(Value::Object(mut fields)) => {
            if let Some(Value::Array(steps)) = fields.get_mut("steps") {
                steps.retain(|step| {
                    step.get("type").and_then(Value::as_str) != Some("custom_action")
                });
            }
            NodeValue::Json(Value::Object(fields))
        }
        other => other,
    }
}

pub(crate) fn summary(value: Option<&NodeValue>, engine: &mut Engine<'_>) -> Value {
    match value {
        None => Value::Null,
        Some(NodeValue::Json(value)) => value.clone(),
        Some(NodeValue::Array(values)) => {
            Value::Array(values.iter().map(|v| summary(Some(v), engine)).collect())
        }
        Some(NodeValue::Object(values)) => Value::Object(
            values
                .iter()
                .map(|(k, v)| (k.clone(), summary(Some(v), engine)))
                .collect(),
        ),
        Some(NodeValue::Element { element, props }) => {
            let props = props
                .iter()
                .filter_map(|(k, v)| {
                    let v = summary(Some(v), engine);
                    (!v.is_null()).then(|| (k.clone(), v))
                })
                .collect::<serde_json::Map<_, _>>();
            json!({"type": element.type_name, "props": props})
        }
        Some(NodeValue::Binding { target, .. }) => json!({"$binding": target}),
        Some(NodeValue::Plan(steps)) => json!({"actions": steps.iter().map(|step| {
            match step {
                Action::Run { statement_id, .. } => json!({"action": "Run", "args": [statement_id]}),
                Action::ContinueConversation { message, context } => {
                    let mut args = vec![json!(message)]; if let Some(context) = context { args.push(context.clone()); }
                    json!({"action": "ToAssistant", "args": args})
                }
                Action::OpenUrl { url } => json!({"action": "OpenUrl", "args": [url]}),
                Action::Set { target, value } => json!({"action": "Set", "args": [json!(target), engine.expr(value, 0).map(|v| v.public_json()).unwrap_or(Value::Null)]}),
                Action::Reset { targets } => json!({"action": "Reset", "args": targets}),
                Action::CustomAction { name, params } => json!({"action": name, "args": params.values().collect::<Vec<_>>()}),
            }
        }).collect::<Vec<_>>()}),
        Some(NodeValue::Expr(expr)) => {
            let value = engine.expr(expr, 0).unwrap_or_else(|_| NodeValue::null());
            summary(Some(&value), engine)
        }
    }
}
