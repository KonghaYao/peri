use std::cmp::Ordering;

use icu_collator::{options::CollatorOptions, Collator};
use serde_json::{Number, Value};

use crate::EvalError;

pub(crate) const FUNCTIONS: &[(&str, &str)] = &[
    ("Count", "array"),
    ("First", "array"),
    ("Last", "array"),
    ("Sum", "numbers"),
    ("Avg", "numbers"),
    ("Min", "numbers"),
    ("Max", "numbers"),
    ("Sort", "array, field, direction?"),
    ("Filter", "array, field, operator, value"),
    ("Round", "number, decimals?"),
    ("Abs", "number"),
    ("Floor", "number"),
    ("Ceil", "number"),
    ("Each", "array, varName, template"),
];
pub(crate) fn is_builtin(name: &str) -> bool {
    FUNCTIONS.iter().any(|(n, _)| *n == name) || is_action(name)
}
pub(crate) fn is_action(name: &str) -> bool {
    matches!(
        name,
        "Action" | "Run" | "ToAssistant" | "OpenUrl" | "Set" | "Reset"
    )
}
pub(crate) fn is_reserved(name: &str) -> bool {
    matches!(name, "Query" | "Mutation")
}

pub(crate) fn number(n: f64) -> Value {
    if n.is_finite() && n.fract() == 0.0 && n >= i64::MIN as f64 && n < i64::MAX as f64 {
        Value::Number(Number::from(n as i64))
    } else {
        Number::from_f64(n)
            .map(Value::Number)
            .unwrap_or(Value::Null)
    }
}

pub(crate) fn js_number(v: &Value) -> f64 {
    match v {
        Value::Null => 0.0,
        Value::Bool(v) => f64::from(*v),
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::String(s) => {
            let s = s.trim();
            if s.is_empty() {
                return 0.0;
            }
            for (prefix, radix) in [
                ("0x", 16),
                ("0X", 16),
                ("0b", 2),
                ("0B", 2),
                ("0o", 8),
                ("0O", 8),
            ] {
                if let Some(rest) = s.strip_prefix(prefix) {
                    return u64::from_str_radix(rest, radix)
                        .map(|v| v as f64)
                        .unwrap_or(f64::NAN);
                }
            }
            s.parse().unwrap_or(f64::NAN)
        }
        Value::Array(_) => js_number(&Value::String(js_string(v))),
        Value::Object(_) => f64::NAN,
    }
}

pub(crate) fn to_number(v: &Value) -> f64 {
    if matches!(v, Value::Array(_) | Value::Object(_)) {
        return 0.0;
    }
    let number = js_number(v);
    if number.is_nan() {
        0.0
    } else {
        number
    }
}

pub(crate) fn js_string(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Array(values) => values.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
        Value::Number(n) => n.as_f64().map(|n| n.to_string()).unwrap_or_default(),
        Value::Bool(v) => v.to_string(),
    }
}

pub(crate) fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.0 && !n.is_nan()),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

pub(crate) fn loose_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, _) | (_, Value::Null) => a.is_null() && b.is_null(),
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Number(_), Value::Number(_))
        | (Value::Bool(_), _)
        | (_, Value::Bool(_))
        | (Value::Number(_), Value::String(_))
        | (Value::String(_), Value::Number(_)) => js_number(a) == js_number(b),
        (Value::Object(_) | Value::Array(_), Value::String(_) | Value::Number(_)) => {
            loose_eq(&Value::String(js_string(a)), b)
        }
        (Value::String(_) | Value::Number(_), Value::Object(_) | Value::Array(_)) => {
            loose_eq(a, &Value::String(js_string(b)))
        }
        _ => false,
    }
}

pub(crate) fn field(value: &Value, path: &str) -> Value {
    path.split('.')
        .fold(value.clone(), |v, key| direct_field(&v, key))
}

pub(crate) fn direct_field(value: &Value, key: &str) -> Value {
    match value {
        Value::Array(values) if key == "length" => Value::from(values.len()),
        Value::Array(values) => key
            .parse::<usize>()
            .ok()
            .and_then(|i| values.get(i))
            .cloned()
            .unwrap_or(Value::Null),
        Value::String(s) if key == "length" => Value::from(s.encode_utf16().count()),
        Value::String(s) => key
            .parse::<usize>()
            .ok()
            .and_then(|i| s.chars().nth(i))
            .map(|c| Value::String(c.to_string()))
            .unwrap_or(Value::Null),
        Value::Object(values) => values.get(key).cloned().unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

pub(crate) fn evaluate(name: &str, args: &[Value]) -> Result<Value, EvalError> {
    let null = Value::Null;
    let arg = |i| args.get(i).unwrap_or(&null);
    let array = arg(0).as_array().map(Vec::as_slice).unwrap_or(&[]);
    let result = match name {
        "Count" => Value::from(array.len()),
        "First" => array.first().cloned().unwrap_or(Value::Null),
        "Last" => array.last().cloned().unwrap_or(Value::Null),
        "Sum" => number(array.iter().map(to_number).sum()),
        "Avg" => number(if array.is_empty() {
            0.0
        } else {
            array.iter().map(to_number).sum::<f64>() / array.len() as f64
        }),
        "Min" | "Max" => {
            let n = array
                .iter()
                .map(to_number)
                .reduce(|a, b| if name == "Min" { a.min(b) } else { a.max(b) })
                .unwrap_or(0.0);
            number(n)
        }
        "Sort" => {
            if !arg(0).is_array() {
                return Ok(arg(0).clone());
            }
            // ICU 使用固定根排序规则，避免宿主 locale 令相同程序产生不同结果。
            let collator = Collator::try_new(Default::default(), CollatorOptions::default())
                .map_err(|e| EvalError::runtime(e.to_string()))?;
            let path = js_string(arg(1));
            let desc = js_string(arg(2)) == "desc";
            let mut values = array.to_vec();
            stable_sort(&mut values, &|a, b| {
                let a = if path.is_empty() {
                    a.clone()
                } else {
                    field(a, &path)
                };
                let b = if path.is_empty() {
                    b.clone()
                } else {
                    field(b, &path)
                };
                let numeric = |v: &Value| {
                    v.is_number()
                        || v.as_str()
                            .is_some_and(|s| !s.is_empty() && !js_number(v).is_nan())
                };
                let order = if numeric(&a) && numeric(&b) {
                    to_number(&a)
                        .partial_cmp(&to_number(&b))
                        .unwrap_or(Ordering::Equal)
                } else {
                    collator.compare(&js_string(&a), &js_string(&b))
                };
                if desc {
                    order.reverse()
                } else {
                    order
                }
            });
            Value::Array(values)
        }
        "Filter" => {
            let path = js_string(arg(1));
            let operator = if arg(2).is_null() {
                "==".into()
            } else {
                js_string(arg(2))
            };
            Value::Array(
                array
                    .iter()
                    .filter(|item| {
                        let value = if path.is_empty() {
                            (*item).clone()
                        } else {
                            field(item, &path)
                        };
                        match operator.as_str() {
                            "==" => loose_eq(&value, arg(3)),
                            "!=" => !loose_eq(&value, arg(3)),
                            ">" => to_number(&value) > to_number(arg(3)),
                            "<" => to_number(&value) < to_number(arg(3)),
                            ">=" => to_number(&value) >= to_number(arg(3)),
                            "<=" => to_number(&value) <= to_number(arg(3)),
                            "contains" => js_string(&value).contains(&js_string(arg(3))),
                            _ => false,
                        }
                    })
                    .cloned()
                    .collect(),
            )
        }
        "Round" | "Abs" | "Floor" | "Ceil" => elementwise(arg(0), &|n| match name {
            "Abs" => n.abs(),
            "Floor" => n.floor(),
            "Ceil" => n.ceil(),
            _ => {
                let factor = 10_f64.powf(to_number(arg(1)));
                (n * factor + 0.5).floor() / factor
            }
        }),
        _ => Value::Null,
    };
    Ok(result)
}

// JS 的混合类型比较可能不传递；合并排序不要求全序，也不会因此 panic。
fn stable_sort(values: &mut [Value], compare: &impl Fn(&Value, &Value) -> Ordering) {
    let middle = values.len() / 2;
    if middle == 0 {
        return;
    }
    let (left, right) = values.split_at_mut(middle);
    stable_sort(left, compare);
    stable_sort(right, compare);
    let sorted = values.to_vec();
    let (mut left, mut right) = (0, middle);
    for value in values {
        if left < middle
            && (right == sorted.len()
                || compare(&sorted[left], &sorted[right]) != Ordering::Greater)
        {
            *value = sorted[left].clone();
            left += 1;
        } else {
            *value = sorted[right].clone();
            right += 1;
        }
    }
}

fn elementwise(value: &Value, f: &impl Fn(f64) -> f64) -> Value {
    if let Value::Array(values) = value {
        Value::Array(values.iter().map(|v| elementwise(v, f)).collect())
    } else {
        number(f(to_number(value)))
    }
}
