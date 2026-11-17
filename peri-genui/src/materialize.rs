use std::collections::{HashMap, HashSet};

use indexmap::{IndexMap, IndexSet};
use serde_json::Value;

use crate::{
    builtins::{is_action, is_builtin, is_reserved},
    library::CallDef,
    types::NodeValue,
    validation::Validation,
    Element, ErrorCode, Expr, Library, Statement, StatementKind, MAX_DEPTH, MAX_NODES,
};

pub(crate) struct Materializer<'a> {
    pub validation: Validation<'a>,
    pub unresolved: Vec<String>,
    pub unreached: IndexSet<String>,
    pub statements: &'a IndexMap<String, Statement>,
    visited: HashSet<String>,
    memo: HashMap<(bool, String), NodeValue>,
    remaining: usize,
    bytes_remaining: usize,
}

impl<'a> Materializer<'a> {
    pub fn new(
        library: &'a Library,
        statements: &'a IndexMap<String, Statement>,
        partial: bool,
    ) -> Self {
        Self {
            validation: Validation::new(library, partial),
            unresolved: Vec::new(),
            unreached: IndexSet::new(),
            statements,
            visited: HashSet::new(),
            memo: HashMap::new(),
            remaining: MAX_NODES,
            bytes_remaining: crate::MAX_STATE_BYTES,
        }
    }

    fn budget_error(&mut self) {
        if !self
            .validation
            .errors
            .iter()
            .any(|e| e.code == ErrorCode::ParseFailed)
        {
            self.validation
                .issue(ErrorCode::ParseFailed, "", "", "引用展开超过安全预算");
        }
    }

    fn enter(&mut self, depth: usize) -> bool {
        if depth >= MAX_DEPTH || self.remaining == 0 {
            self.budget_error();
            return false;
        }
        self.remaining -= 1;
        true
    }

    fn spend_bytes(&mut self, bytes: usize) -> bool {
        if bytes > self.bytes_remaining {
            self.bytes_remaining = 0;
            self.budget_error();
            return false;
        }
        self.bytes_remaining -= bytes;
        true
    }

    fn resolve(&mut self, name: &str, expression: bool, depth: usize) -> NodeValue {
        if self.visited.contains(name) || !self.statements.contains_key(name) {
            self.unresolved.push(name.into());
            return if expression {
                NodeValue::Expr(Expr::Ph { n: name.into() })
            } else {
                NodeValue::null()
            };
        }
        self.unreached.shift_remove(name);
        let Some(statement) = self.statements.get(name) else {
            return NodeValue::null();
        };
        if matches!(
            statement.kind,
            StatementKind::Query | StatementKind::Mutation
        ) {
            return NodeValue::Expr(Expr::RuntimeRef {
                n: name.into(),
                ref_type: statement.kind,
            });
        }
        let key = (expression, name.to_owned());
        if let Some((weight, bytes)) = self
            .memo
            .get(&key)
            .map(|value| (weight(value), crate::memory::node_value_bytes(value)))
        {
            if weight <= self.remaining {
                self.remaining -= weight;
                if !self.spend_bytes(bytes) {
                    return NodeValue::null();
                }
                return self.memo.get(&key).cloned().unwrap_or_else(NodeValue::null);
            }
            self.remaining = 0;
            self.enter(depth);
            return NodeValue::null();
        }
        let expr = statement.expr.clone();
        self.visited.insert(name.into());
        let previous = self.validation.statement_id.replace(name.into());
        let before = self.unresolved.len();
        let mut value = if expression {
            NodeValue::Expr(self.expr(&expr, &HashSet::new(), depth + 1))
        } else {
            self.value(&expr, depth + 1)
        };
        if let NodeValue::Element { element, .. } = &mut value {
            element.statement_id = Some(name.into());
        }
        if self.unresolved.len() == before && self.remaining > 0 {
            self.memo.insert(key, value.clone());
        }
        self.validation.statement_id = previous;
        self.visited.remove(name);
        value
    }

    pub fn value(&mut self, expr: &Expr, depth: usize) -> NodeValue {
        if !self.enter(depth) {
            return NodeValue::null();
        }
        match expr {
            Expr::Ref { n } => self.resolve(n, false, depth),
            Expr::Ph { .. } | Expr::Null => NodeValue::null(),
            Expr::String { v } => {
                if !self.spend_bytes(v.len()) {
                    return NodeValue::null();
                }
                NodeValue::Json(Value::String(v.clone()))
            }
            Expr::Number { v } => NodeValue::Json(crate::builtins::number(*v)),
            Expr::Bool { v } => NodeValue::Json(Value::Bool(*v)),
            Expr::Array { els } => {
                let mut values = Vec::new();
                for expr in els {
                    if matches!(expr, Expr::Ph { .. }) {
                        continue;
                    }
                    let value = self.value(expr, depth + 1);
                    if value.is_null() && matches!(expr, Expr::Ref { .. } | Expr::Call { .. }) {
                        continue;
                    }
                    values.push(value);
                }
                NodeValue::Array(values)
            }
            Expr::Object { entries } => {
                let mut fields = IndexMap::new();
                for (k, e) in entries {
                    if !self.spend_bytes(k.len()) {
                        return NodeValue::null();
                    }
                    fields.insert(k.clone(), self.value(e, depth + 1));
                }
                NodeValue::Object(fields)
            }
            Expr::Call {
                name,
                args,
                function,
                ..
            } => {
                if *function && !self.runtime_call(name) {
                    return self.unknown_function(name);
                }
                if is_reserved(name) {
                    self.validation.issue(
                        ErrorCode::InlineReserved,
                        name,
                        "",
                        format!("{name} 必须在顶层声明"),
                    );
                    return NodeValue::null();
                }
                if self.runtime_call(name) {
                    return NodeValue::Expr(self.expr(expr, &HashSet::new(), depth + 1));
                }
                self.component(name, args, depth)
            }
            _ => NodeValue::Expr(self.expr(expr, &HashSet::new(), depth + 1)),
        }
    }

    fn component(&mut self, name: &str, args: &[Expr], depth: usize) -> NodeValue {
        let Some(def) = self.validation.library.components.get(name).cloned() else {
            self.validation.issue(
                ErrorCode::UnknownComponent,
                name,
                "",
                format!("未知组件 {name}"),
            );
            return NodeValue::null();
        };
        let mut props = IndexMap::new();
        let mut invalid = false;
        for (param, arg) in def.params.iter().zip(args) {
            let value = self.value(arg, depth + 1);
            props.insert(param.name.clone(), value.clone());
            let checked = self.validation.validate(
                value,
                &param.schema,
                name,
                &format!("/{}", param.name),
                0,
            );
            match checked {
                Some(value) => {
                    props.insert(param.name.clone(), value);
                }
                None if param.schema.default.is_some() => {
                    props.insert(
                        param.name.clone(),
                        NodeValue::Json(param.schema.default.clone().unwrap_or(Value::Null)),
                    );
                }
                None if param.required => invalid = true,
                None => {
                    props.shift_remove(&param.name);
                }
            }
        }
        if args.len() > def.params.len() {
            self.excess(name, def.params.len(), args.len());
        }
        for param in &def.params {
            if !param.required || !props.get(&param.name).is_none_or(NodeValue::is_null) {
                continue;
            }
            if let Some(default) = &param.schema.default {
                props.insert(param.name.clone(), NodeValue::Json(default.clone()));
            } else {
                let code = if props.contains_key(&param.name) {
                    ErrorCode::NullRequired
                } else {
                    ErrorCode::MissingRequired
                };
                self.validation.issue(
                    code,
                    name,
                    &format!("/{}", param.name),
                    format!("缺少非空必填参数 {}", param.name),
                );
                invalid = true;
            }
        }
        if invalid {
            return NodeValue::null();
        }
        let element = Element {
            type_name: name.into(),
            props: IndexMap::new(),
            partial: self.validation.partial,
            statement_id: None,
        };
        NodeValue::Element { element, props }
    }

    pub fn expr(&mut self, expr: &Expr, scope: &HashSet<String>, depth: usize) -> Expr {
        if !self.enter(depth) {
            return Expr::Null;
        }
        match expr {
            Expr::Ref { n } if !scope.contains(n) => match self.resolve(n, true, depth) {
                NodeValue::Expr(expr) => expr,
                _ => Expr::Null,
            },
            Expr::Call {
                name,
                args,
                function,
                ..
            } => {
                if *function && !self.runtime_call(name) {
                    self.unknown_function(name);
                    return Expr::Null;
                }
                if is_reserved(name) {
                    self.validation.issue(
                        ErrorCode::InlineReserved,
                        name,
                        "",
                        format!("{name} 必须在顶层声明"),
                    );
                    return Expr::Null;
                }
                let outer_scope = scope.clone();
                let mut inner_scope = scope.clone();
                if name == "Each" {
                    if let Some(var) = args.get(1).and_then(crate::actions::each_var_name) {
                        inner_scope.insert(var.to_owned());
                    }
                }
                let lowered: Vec<_> = args
                    .iter()
                    .enumerate()
                    .map(|(i, e)| match (name.as_str(), i) {
                        ("Each", 0) => self.expr(e, &outer_scope, depth + 1),
                        ("Each", 1) => e.clone(),
                        ("Each", _) => self.expr(e, &inner_scope, depth + 1),
                        _ => self.expr(e, scope, depth + 1),
                    })
                    .collect();
                let custom = self
                    .validation
                    .library
                    .functions
                    .get(name)
                    .or_else(|| self.validation.library.actions.get(name))
                    .cloned();
                if custom.is_some() || self.validation.library.components.contains_key(name) {
                    // args 与 mappedProps 同时保留，复制前也计入预算，防止嵌套调用指数膨胀。
                    let mut copies = 0;
                    for expr in &lowered {
                        expr.visit(&mut |_| copies += 1);
                    }
                    if copies > self.remaining {
                        self.remaining = 0;
                        self.enter(depth);
                        return Expr::Null;
                    }
                    self.remaining -= copies;
                    let mut copy_bytes = 0usize;
                    for expr in &lowered {
                        expr.visit(&mut |e| match e {
                            Expr::String { v } => {
                                copy_bytes = copy_bytes.saturating_add(v.len());
                            }
                            Expr::Object { entries } => {
                                for key in entries.keys() {
                                    copy_bytes = copy_bytes.saturating_add(key.len());
                                }
                            }
                            _ => {}
                        });
                    }
                    if !self.spend_bytes(copy_bytes) {
                        return Expr::Null;
                    }
                }
                if let Some(def) = custom {
                    let Some(mapped) = self.map_call(name, &lowered, &def) else {
                        return if self.validation.library.actions.contains_key(name) {
                            Expr::call(
                                "Action".into(),
                                vec![Expr::Array { els: Vec::new() }],
                                false,
                            )
                        } else {
                            Expr::Null
                        };
                    };
                    return Expr::Call {
                        name: name.clone(),
                        args: lowered,
                        function: *function,
                        mapped_props: Some(mapped),
                    };
                }
                let mapped_props = self.validation.library.components.get(name).map(|def| {
                    def.params
                        .iter()
                        .zip(&lowered)
                        .map(|(p, e)| (p.name.clone(), e.clone()))
                        .collect()
                });
                if mapped_props.is_none() && !is_builtin(name) {
                    self.validation.issue(
                        ErrorCode::UnknownComponent,
                        name,
                        "",
                        format!("未知组件 {name}"),
                    );
                }
                Expr::Call {
                    name: name.clone(),
                    args: lowered,
                    function: *function,
                    mapped_props,
                }
            }
            Expr::Array { els } => Expr::Array {
                els: els.iter().map(|e| self.expr(e, scope, depth + 1)).collect(),
            },
            Expr::Object { entries } => {
                let mut fields = IndexMap::new();
                for (k, e) in entries {
                    if !self.spend_bytes(k.len()) {
                        return Expr::Null;
                    }
                    fields.insert(k.clone(), self.expr(e, scope, depth + 1));
                }
                Expr::Object { entries: fields }
            }
            Expr::BinOp { op, left, right } => Expr::BinOp {
                op: op.clone(),
                left: Box::new(self.expr(left, scope, depth + 1)),
                right: Box::new(self.expr(right, scope, depth + 1)),
            },
            Expr::UnaryOp { op, operand } => Expr::UnaryOp {
                op: op.clone(),
                operand: Box::new(self.expr(operand, scope, depth + 1)),
            },
            Expr::Ternary {
                cond,
                then_expr,
                else_expr,
            } => Expr::Ternary {
                cond: Box::new(self.expr(cond, scope, depth + 1)),
                then_expr: Box::new(self.expr(then_expr, scope, depth + 1)),
                else_expr: Box::new(self.expr(else_expr, scope, depth + 1)),
            },
            Expr::Member { obj, field } => Expr::Member {
                obj: Box::new(self.expr(obj, scope, depth + 1)),
                field: field.clone(),
            },
            Expr::Index { obj, index } => Expr::Index {
                obj: Box::new(self.expr(obj, scope, depth + 1)),
                index: Box::new(self.expr(index, scope, depth + 1)),
            },
            Expr::Assign { target, value } => Expr::Assign {
                target: target.clone(),
                value: Box::new(self.expr(value, scope, depth + 1)),
            },
            Expr::String { v } => {
                if !self.spend_bytes(v.len()) {
                    return Expr::Null;
                }
                Expr::String { v: v.clone() }
            }
            _ => expr.clone(),
        }
    }

    fn map_call(
        &mut self,
        name: &str,
        args: &[Expr],
        def: &CallDef,
    ) -> Option<IndexMap<String, Expr>> {
        let names: Vec<_> = def.params.properties.keys().cloned().collect();
        if args.len() > names.len() {
            self.excess(name, names.len(), args.len());
        }
        let before = self.validation.errors.len();
        let mut mapped = IndexMap::new();
        let mut invalid = false;
        for (i, key) in names.iter().enumerate() {
            let Some(schema) = def.params.properties.get(key) else {
                continue;
            };
            let arg = args.get(i);
            if let Some(arg) = arg {
                mapped.insert(key.clone(), arg.clone());
            }
            let literal = arg.map_or(Some(Value::Null), Expr::literal_value);
            let Some(literal) = literal else {
                continue;
            };
            let required = def.params.required.contains(key);
            if literal.is_null() {
                if required && schema.default.is_none() {
                    let code = if arg.is_some() {
                        ErrorCode::NullRequired
                    } else {
                        ErrorCode::MissingRequired
                    };
                    self.validation.issue(
                        code,
                        name,
                        &format!("/{key}"),
                        format!("缺少非空必填参数 {key}"),
                    );
                    invalid = true;
                }
                continue;
            }
            match self.validation.validate(
                NodeValue::Json(literal),
                schema,
                name,
                &format!("/{key}"),
                0,
            ) {
                Some(value) => {
                    mapped.insert(key.clone(), Expr::literal(&value.public_json()));
                }
                None if schema.default.is_some() => {
                    mapped.insert(
                        key.clone(),
                        Expr::literal(schema.default.as_ref().unwrap_or(&Value::Null)),
                    );
                }
                None if required => invalid = true,
                None => {
                    mapped.shift_remove(key);
                }
            }
        }
        if self.validation.library.actions.contains_key(name)
            && self.validation.errors.len() > before
        {
            invalid = true;
        }
        (!invalid).then_some(mapped)
    }

    fn runtime_call(&self, name: &str) -> bool {
        is_builtin(name)
            || is_reserved(name)
            || self.validation.library.functions.contains_key(name)
            || self.validation.library.actions.contains_key(name)
    }
    fn unknown_function(&mut self, name: &str) -> NodeValue {
        self.validation.issue(
            ErrorCode::UnknownFunction,
            name,
            "",
            format!("未知函数 @{name}"),
        );
        NodeValue::null()
    }
    fn excess(&mut self, name: &str, declared: usize, got: usize) {
        self.validation.issue(
            ErrorCode::ExcessArgs,
            name,
            "",
            format!("{name} 接受 {declared} 个参数，实际为 {got}，多余参数已丢弃"),
        );
    }
}

fn weight(value: &NodeValue) -> usize {
    match value {
        NodeValue::Array(values) => 1 + values.iter().map(weight).sum::<usize>(),
        NodeValue::Object(values) | NodeValue::Element { props: values, .. } => {
            1 + values.values().map(weight).sum::<usize>()
        }
        NodeValue::Expr(expr) => {
            let mut n = 0;
            expr.visit(&mut |_| n += 1);
            n
        }
        _ => 1,
    }
}

pub(crate) fn component_call(expr: &Expr) -> bool {
    matches!(expr, Expr::Call { name, function: false, .. } if !is_builtin(name) && !is_reserved(name) && !is_action(name))
}
