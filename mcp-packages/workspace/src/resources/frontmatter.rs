//! SKILL.md / agent.md 的 YAML frontmatter 解析（W1 provider 面）。
//!
//! 口径：
//! - 只接受 `---` 分隔的 YAML frontmatter（与本仓库既有 `gray_matter::<YAML>`
//!   解析的常见形态一致；`+++` 等其他分隔符不支持，按「无 frontmatter」处理）；
//! - **逐字透传**：frontmatter 转为 JSON object 时不改写字段名与值（X3：wire
//!   不改写；`depends_on` 等编排字段原样保留，解释权在宿主 W2 的 activation）；
//! - `name` / `description` 必填（缺失或非字符串 → 该技能/agent 不公开）；
//! - `description` 在**资源 metadata 投影**处 trim 尾随空白（与既有 loader
//!   `build_summary` 的渲染需求一致）；frontmatter JSON 本身不做 trim。
//!
//! 本模块是纯函数（无 IO），文本由调用方读入。

/// 拆分 frontmatter 与正文；无合法 frontmatter 时返回 `None`。
///
/// 规则：忽略开头 BOM；首行必须是 `---`（允许行尾空白）；其后第一个
/// `---` 行为结束标记；结束标记之后（含换行）是正文。未闭合时返回 `None`。
pub(crate) fn split_frontmatter(content: &str) -> Option<(&str, &str)> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let mut lines = content.split_inclusive('\n');
    let first = lines.next()?;
    if first.trim_end() != "---" {
        return None;
    }
    let frontmatter_start = first.len();
    let mut cursor = frontmatter_start;
    for line in lines {
        if line.trim_end() == "---" {
            let frontmatter = &content[frontmatter_start..cursor];
            let body = &content[cursor + line.len()..];
            return Some((frontmatter, body));
        }
        cursor += line.len();
    }
    None
}

/// 解析 frontmatter 为 JSON object（顶层必须是 YAML mapping）。
pub(crate) fn frontmatter_json(
    content: &str,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    let (frontmatter, _) = split_frontmatter(content)?;
    let value: serde_yaml::Value = serde_yaml::from_str(frontmatter).ok()?;
    let json = yaml_to_json(value)?;
    match json {
        serde_json::Value::Object(map) => Some(map),
        _ => None,
    }
}

/// `(name, description)` 摘要；两者都必须是非空字符串字段。
///
/// `description` 已 trim（资源 metadata 投影口径）；`name` 原样（含 `:` 等字符
/// 由契约层的段校验决定是否公开，本函数不改写）。
pub(crate) fn frontmatter_summary(
    frontmatter: &serde_json::Map<String, serde_json::Value>,
) -> Option<(String, String)> {
    let name = frontmatter.get("name")?.as_str()?;
    let description = frontmatter.get("description")?.as_str()?;
    if name.is_empty() {
        return None;
    }
    Some((name.to_string(), description.trim().to_string()))
}

/// `serde_yaml::Value` → `serde_json::Value`。
///
/// - 非字符串 mapping key 仅接受标量（String / Number / Bool）并转为字符串，
///   其他形态的 key 丢弃该 entry（JSON 无法表达；不影响 `name`/`description`
///   等标量字段的常规用法）；
/// - `Tagged`（`!tag value`）取内部值，不保留 tag 名（JSON 无对应语义）。
fn yaml_to_json(value: serde_yaml::Value) -> Option<serde_json::Value> {
    Some(match value {
        serde_yaml::Value::Null => serde_json::Value::Null,
        serde_yaml::Value::Bool(b) => serde_json::Value::Bool(b),
        serde_yaml::Value::Number(number) => {
            if let Some(i) = number.as_i64() {
                serde_json::Value::Number(i.into())
            } else if let Some(u) = number.as_u64() {
                serde_json::Value::Number(u.into())
            } else {
                let f = number.as_f64()?;
                serde_json::Number::from_f64(f).map(serde_json::Value::Number)?
            }
        }
        serde_yaml::Value::String(s) => serde_json::Value::String(s),
        serde_yaml::Value::Sequence(items) => serde_json::Value::Array(
            items
                .into_iter()
                .map(yaml_to_json)
                .collect::<Option<Vec<_>>>()?,
        ),
        serde_yaml::Value::Mapping(mapping) => {
            let mut object = serde_json::Map::with_capacity(mapping.len());
            for (key, item) in mapping {
                let key = match key {
                    serde_yaml::Value::String(s) => s,
                    serde_yaml::Value::Number(n) => n.to_string(),
                    serde_yaml::Value::Bool(b) => b.to_string(),
                    _ => continue,
                };
                object.insert(key, yaml_to_json(item)?);
            }
            serde_json::Value::Object(object)
        }
        serde_yaml::Value::Tagged(tagged) => yaml_to_json(tagged.value)?,
    })
}

#[cfg(test)]
#[path = "frontmatter_test.rs"]
mod tests;
