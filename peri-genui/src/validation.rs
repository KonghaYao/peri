use serde_json::Value;

use crate::{library::Schema, types::NodeValue, ErrorCode, Library, ParseError, MAX_DEPTH};

pub(crate) struct Validation<'a> {
    pub library: &'a Library,
    pub errors: Vec<ParseError>,
    pub partial: bool,
    pub statement_id: Option<String>,
}

impl<'a> Validation<'a> {
    pub fn new(library: &'a Library, partial: bool) -> Self {
        Self {
            library,
            errors: Vec::new(),
            partial,
            statement_id: None,
        }
    }

    pub fn issue(
        &mut self,
        code: ErrorCode,
        component: &str,
        path: &str,
        message: impl Into<String>,
    ) {
        self.errors.push(ParseError {
            code,
            component: component.into(),
            path: path.into(),
            message: message.into(),
            statement_id: self.statement_id.clone(),
            severity: None,
        });
    }

    pub fn mismatch(&mut self, component: &str, path: &str, expected: &str, actual: &str) {
        self.issue(
            ErrorCode::TypeMismatch,
            component,
            path,
            format!("字段 {path} 需要 {expected}，实际为 {actual}"),
        );
    }

    pub fn validate(
        &mut self,
        value: NodeValue,
        schema: &Schema,
        component: &str,
        path: &str,
        depth: usize,
    ) -> Option<NodeValue> {
        if depth >= MAX_DEPTH {
            self.issue(
                ErrorCode::ParseFailed,
                component,
                path,
                "schema 校验超过安全深度",
            );
            return None;
        }
        if value.is_null() || matches!(value, NodeValue::Expr(_)) {
            return Some(value);
        }
        if let NodeValue::Element { element, .. } = &value {
            if schema.composite() {
                let names = schema.slot_names();
                if !names.is_empty()
                    && names
                        .iter()
                        .all(|n| n.is_some_and(|n| self.library.components.contains_key(n)))
                    && !names
                        .iter()
                        .flatten()
                        .any(|name| *name == element.type_name)
                {
                    self.mismatch(
                        component,
                        path,
                        &names.into_iter().flatten().collect::<Vec<_>>().join(" | "),
                        &format!("component {}", element.type_name),
                    );
                    return None;
                }
                return Some(value);
            }
            if schema.ty.is_some() || schema.enum_values.is_some() || schema.const_value.is_some() {
                self.mismatch(
                    component,
                    path,
                    schema.ty.as_deref().unwrap_or("literal"),
                    &format!("component {}", element.type_name),
                );
                return None;
            }
            return Some(value);
        }
        if schema.composite() {
            let names = schema.slot_names();
            if !names.is_empty()
                && names
                    .iter()
                    .all(|n| n.is_some_and(|n| self.library.components.contains_key(n)))
            {
                self.mismatch(
                    component,
                    path,
                    &names.into_iter().flatten().collect::<Vec<_>>().join(" | "),
                    kind(&value),
                );
                return None;
            }
            return Some(value);
        }
        match schema.ty.as_deref() {
            Some("array") => {
                let Some(values) = into_array(value.clone()) else {
                    self.mismatch(component, path, "array", kind(&value));
                    return None;
                };
                let Some(items) = &schema.items else {
                    return Some(NodeValue::Array(values));
                };
                let values = values
                    .into_iter()
                    .enumerate()
                    .filter_map(|(i, value)| {
                        self.validate(value, items, component, &format!("{path}/{i}"), depth + 1)
                    })
                    .collect();
                Some(NodeValue::Array(values))
            }
            Some("object") => self.object(value, schema, component, path, depth),
            _ => {
                let value_json = value.public_json();
                let allowed = schema
                    .enum_values
                    .as_ref()
                    .cloned()
                    .or_else(|| schema.const_value.clone().map(|v| vec![v]));
                if let Some(allowed) = allowed {
                    if !self.partial && !allowed.iter().any(|a| literal_equal(a, &value_json)) {
                        self.mismatch(
                            component,
                            path,
                            &format!("enum {allowed:?}"),
                            &value_json.to_string(),
                        );
                        return None;
                    }
                }
                if let Some(expected @ ("string" | "boolean" | "number" | "integer")) =
                    schema.ty.as_deref()
                {
                    let expected = if expected == "integer" {
                        "number"
                    } else {
                        expected
                    };
                    if kind(&value) != expected {
                        self.mismatch(component, path, expected, kind(&value));
                        return None;
                    }
                }
                Some(value)
            }
        }
    }

    fn object(
        &mut self,
        value: NodeValue,
        schema: &Schema,
        component: &str,
        path: &str,
        depth: usize,
    ) -> Option<NodeValue> {
        let mut values = match value {
            NodeValue::Object(values) => values,
            NodeValue::Json(Value::Object(values)) => values
                .into_iter()
                .map(|(k, v)| (k, NodeValue::Json(v)))
                .collect(),
            other => {
                self.mismatch(component, path, "object", kind(&other));
                return None;
            }
        };
        let mut invalid = false;
        if !self.partial {
            for key in &schema.required {
                if values.get(key).is_none_or(NodeValue::is_null) {
                    if let Some(default) =
                        schema.properties.get(key).and_then(|s| s.default.clone())
                    {
                        values.insert(key.clone(), NodeValue::Json(default));
                    } else {
                        let code = if values.contains_key(key) {
                            ErrorCode::NullRequired
                        } else {
                            ErrorCode::MissingRequired
                        };
                        self.issue(
                            code,
                            component,
                            &format!("{path}/{key}"),
                            format!("缺少非空必填字段 {key}"),
                        );
                        invalid = true;
                    }
                }
            }
        }
        for (key, sub) in &schema.properties {
            let Some(value) = values.get(key).cloned() else {
                continue;
            };
            match self.validate(value, sub, component, &format!("{path}/{key}"), depth + 1) {
                Some(next) => {
                    values.insert(key.clone(), next);
                }
                None => {
                    if let Some(default) = &sub.default {
                        values.insert(key.clone(), NodeValue::Json(default.clone()));
                    } else if schema.required.contains(key) {
                        invalid = true;
                    } else {
                        values.shift_remove(key);
                    }
                }
            }
        }
        (!invalid).then_some(NodeValue::Object(values))
    }
}

fn into_array(value: NodeValue) -> Option<Vec<NodeValue>> {
    match value {
        NodeValue::Array(values) => Some(values),
        NodeValue::Json(Value::Array(values)) => {
            Some(values.into_iter().map(NodeValue::Json).collect())
        }
        _ => None,
    }
}

pub(crate) fn kind(value: &NodeValue) -> &'static str {
    match value {
        NodeValue::Array(_) | NodeValue::Json(Value::Array(_)) => "array",
        NodeValue::Json(Value::String(_)) => "string",
        NodeValue::Json(Value::Number(_)) => "number",
        NodeValue::Json(Value::Bool(_)) => "boolean",
        NodeValue::Json(Value::Null) => "null",
        _ => "object",
    }
}

fn literal_equal(a: &Value, b: &Value) -> bool {
    if a.is_number() && b.is_number() {
        a.as_f64() == b.as_f64()
    } else {
        a == b
    }
}
