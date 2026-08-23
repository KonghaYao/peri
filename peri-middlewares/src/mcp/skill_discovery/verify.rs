// ─── 共享：frontmatter 解析 / 身份 / 校验 / 消歧 ──────────────────────────

use std::path::PathBuf;

use gray_matter::{engine::YAML, Matter};
use peri_acp_types::{
    mcp_skills::mcp_skill_name,
    skills::{SkillMetadata, SkillOrigin, SkillResource, SkillSource},
};
use sha2::{Digest, Sha256};

use super::legacy_scan::is_skill_scheme;

/// 纯函数：gray_matter 解析 frontmatter 为 verbatim JSON map（loader.rs
/// 同款；YAML→JSON 值渲染由 serde_yaml 完成——数字 42 → Number、字符串
/// "42" → String）。YAML 非法 / 无 frontmatter → None。
pub(super) fn parse_skill_frontmatter_map(
    content: &str,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    let matter = Matter::<YAML>::new();
    let result: gray_matter::ParsedEntity = matter.parse(content).ok()?;
    let data = result.data?;
    data.deserialize().ok()
}

/// 纯函数：frontmatter 逐字段全量比对（规范 MUST：host 拉取 SKILL.md 后须
/// 与条目 frontmatter 逐字段比对，**任何**差异——含附加字段如
/// license/metadata——验证失败、MUST NOT load）。键集合须完全一致，值经
/// [`frontmatter_values_equal`] 宽松归一（容忍 YAML→JSON 渲染差异）。
pub(super) fn frontmatter_maps_equal(
    a: &serde_json::Map<String, serde_json::Value>,
    b: &serde_json::Map<String, serde_json::Value>,
) -> bool {
    a.len() == b.len()
        && a.iter()
            .all(|(k, v)| b.get(k).is_some_and(|bv| frontmatter_values_equal(v, bv)))
}

/// 纯函数：frontmatter 值严格比较（规范 "identical in content" 字面）。
///
/// 决策（2026-08-15 第二轮 review 定案）：
/// - 数字 ↔ 字符串**跨类型不相等**（42 ≠ "42"）——两侧类型不一致即内容
///   差异，按规范拒绝；
/// - Number vs Number 保留 serde_json 混合 f64 比较（1 == 1.0 成立）；
/// - String vs String 做**尾随空白归一**（YAML block scalar 渲染差异：
///   比较前两侧 trim_end；不做 trim 全量——仅尾随）；
/// - 对象/数组逐字段递归；null vs 缺键由键集合长度区分。
pub(super) fn frontmatter_values_equal(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    match (a, b) {
        (serde_json::Value::Null, serde_json::Value::Null) => true,
        (serde_json::Value::Bool(x), serde_json::Value::Bool(y)) => x == y,
        (serde_json::Value::String(x), serde_json::Value::String(y)) => {
            x.trim_end() == y.trim_end()
        }
        (serde_json::Value::Number(x), serde_json::Value::Number(y)) => number_eq(x, y),
        (serde_json::Value::Array(x), serde_json::Value::Array(y)) => {
            x.len() == y.len()
                && x.iter()
                    .zip(y.iter())
                    .all(|(xv, yv)| frontmatter_values_equal(xv, yv))
        }
        (serde_json::Value::Object(x), serde_json::Value::Object(y)) => {
            frontmatter_maps_equal(x, y)
        }
        // 其余跨类型（含 Number vs String）→ 不相等。
        _ => false,
    }
}

/// 纯函数：Number vs Number 混合比较——serde_json `Number` 的 `PartialEq`
/// 是严格同变体（`PosInt(1) != Float(1.0)`）；决策保留混合 f64 比较
/// （1 == 1.0 成立）：先按整数精确比对，再走 Float-vs-Int 精确分支，最后
/// 退回纯浮点比较。
pub(super) fn number_eq(a: &serde_json::Number, b: &serde_json::Number) -> bool {
    if let (Some(x), Some(y)) = (a.as_i64(), b.as_i64()) {
        return x == y;
    }
    if let (Some(x), Some(y)) = (a.as_u64(), b.as_u64()) {
        return x == y;
    }
    // Float-vs-Int 精确分支（2026-08-15 第三轮 review）：f64 兜底对
    // >2^53 的 Float-vs-Int 会因 f64 舍入误判相等（如 2^53+1 Float 舍入后
    // 与 2^53 Int 的 f64 表示相同）。Float 绝对值 ≤ 2^53 时 f64 可精确表示
    // 该整数，转 i64/u64 精确比较；超出 f64 精确整数域 → 保守判不等
    // （拒绝）。该分支是 Float-vs-Int 的**最终判定**，不再回退 f64 兜底
    // （兜底正是舍入误判来源）。
    if a.is_f64() != b.is_f64() {
        let (f, int) = if a.is_f64() {
            (a.as_f64().unwrap(), b)
        } else {
            (b.as_f64().unwrap(), a)
        };
        return float_vs_int(f, int);
    }
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// 纯函数：Float 侧与 Int 侧的精确比较（`number_eq` 的 Float-vs-Int 分支）。
/// Float 非有限 / 非整数 / 绝对值 > 2^53（超出 f64 精确整数域）→ false
/// （保守拒绝）。
fn float_vs_int(f: f64, int: &serde_json::Number) -> bool {
    const MAX_EXACT_INT: f64 = 9_007_199_254_740_992.0; // 2^53
    if !f.is_finite() || f.abs() > MAX_EXACT_INT || f.fract() != 0.0 {
        return false;
    }
    if let Some(i) = int.as_i64() {
        return f as i64 == i;
    }
    if let Some(u) = int.as_u64() {
        return f as u64 == u;
    }
    false
}

/// 纯函数：`skill://` URI 路径段（scheme 大小写不敏感 + `strip_suffix(
/// "/SKILL.md")`，按 `/` 拆分；至少 1 段）。非 `skill://` scheme 或结构
/// 非法 → None。
fn uri_skill_segments(uri: &str) -> Option<Vec<String>> {
    if !is_skill_scheme(uri) {
        return None;
    }
    let path = uri.get(8..)?.strip_suffix("/SKILL.md")?;
    if path.is_empty() {
        return None;
    }
    Some(path.split('/').map(String::from).collect())
}

/// 纯函数：URI 最终段（= skill name，规范：最终段 MUST 等于 frontmatter name）。
fn uri_final_segment(uri: &str) -> Option<String> {
    uri_skill_segments(uri).and_then(|segments| segments.last().cloned())
}

/// 纯函数：非 `[a-zA-Z0-9_-]` 字符替换为 `'_'`（Agent Skills name 命名规则
/// 的宽松近似，用于与 URI 段做等价比对）。
fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// 纯函数：sha256 digest 校验（格式条文为 `sha256:{64 位小写 hex}`）。
/// **接受大写 hex 为互操作宽容**——server 生成大写 hex digest 是合法场景，
/// 校验前统一转小写比较（`is_ascii_hexdigit` 已覆盖大小写，非 hex 字符拒绝）。
/// 内容侧统一以 bytes 计算：Text 用 UTF-8 bytes、Blob 用 base64 解码后的
/// raw bytes——与 server 计算 digest 的字节一致。发现侧（skill_discovery）
/// 与读取面（resource_tool 完整性校验）共用。
pub(crate) fn verify_digest_bytes(content: &[u8], expected: &str) -> bool {
    let Some(hex) = expected.strip_prefix("sha256:") else {
        return false;
    };
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return false;
    }
    let mut hasher = Sha256::new();
    hasher.update(content);
    let actual = hasher.finalize();
    let actual_hex: String = actual.iter().map(|b| format!("{b:02x}")).collect();
    actual_hex == hex.to_ascii_lowercase()
}

/// 纯函数：文本内容 digest 校验（`verify_digest_bytes` 的 UTF-8 便捷面）。
pub(crate) fn verify_digest(content: &str, expected: &str) -> bool {
    verify_digest_bytes(content.as_bytes(), expected)
}

/// 纯函数：身份校验 + 构建 metadata。
///
/// 注册名 = `mcp__<server>__<name>`（name = frontmatter `name`，经与 URI
/// 最终段一致性校验——frontmatter 是不可信内容，提示注入防御）；URI 最终段
/// 与 name（sanitize 后）不一致 → 拒绝。origin 保留完整 SKILL.md URI；
/// resources 存条目完整资源清单（读取面按它做内容绑定校验）。
pub(super) fn build_metadata(
    server: &str,
    uri: &str,
    name: &str,
    description: &str,
    content: &str,
    resources: Vec<SkillResource>,
) -> Option<SkillMetadata> {
    let final_segment = uri_final_segment(uri)?;
    if sanitize_name(&final_segment) != sanitize_name(name) {
        tracing::warn!(
            server,
            %uri,
            "MCP skill uri 最终段与 frontmatter name 不一致，拒绝加载"
        );
        return None;
    }
    Some(SkillMetadata {
        name: mcp_skill_name(server, &sanitize_name(name)),
        aliases: Vec::new(),
        description: description.trim().to_string(),
        path: PathBuf::new(),
        source: SkillSource::Mcp,
        plugin_name: None,
        origin: Some(SkillOrigin::Mcp {
            server: server.to_string(),
            uri: uri.to_string(),
        }),
        content: Some(content.to_string()),
        // MCP 来源：完整 resources 集（读取面按它做内容绑定校验）
        resources,
    })
}

/// legacy 入口：frontmatter 解析（name/description 必填）+ 身份校验 +
/// 构建（兼容既有测试/调用面）。legacy 无 resources 清单 → 空 vec。
pub(crate) fn parse_mcp_skill_md(content: &str, server: &str, uri: &str) -> Option<SkillMetadata> {
    let fm = parse_skill_frontmatter_map(content)?;
    let name = fm.get("name")?.as_str()?;
    let description = fm.get("description")?.as_str()?;
    build_metadata(server, uri, name, description, content, vec![])
}

/// 纯函数：同一 server 内注册名冲突消歧。同名组 >1 时组内全部改用完整路径
/// 段形式（`mcp__<server>__<sanitized path segments>`）——规范：MUST
/// disambiguate（如按可区分路径段），不得静默丢弃或偏爱其一。非 `skill://`
/// scheme 无路径段可用 → 保留原名（host 不得假定名称唯一，按规范允许）。
pub(super) fn disambiguate_names(
    server: &str,
    mut entries: Vec<SkillMetadata>,
) -> Vec<SkillMetadata> {
    let mut groups: std::collections::BTreeMap<String, Vec<usize>> = Default::default();
    for (i, entry) in entries.iter().enumerate() {
        groups.entry(entry.name.clone()).or_default().push(i);
    }
    for (_, indices) in groups {
        if indices.len() < 2 {
            continue;
        }
        for i in indices {
            let path_name = entries[i]
                .origin
                .as_ref()
                .and_then(|origin| match origin {
                    SkillOrigin::Mcp { uri, .. } => uri_skill_segments(uri),
                })
                .map(|segments| {
                    segments
                        .iter()
                        .map(|s| sanitize_name(s))
                        .collect::<Vec<_>>()
                        .join("_")
                });
            match path_name {
                Some(path) => entries[i].name = mcp_skill_name(server, &path),
                None => tracing::warn!(
                    server,
                    skill = %entries[i].name,
                    "MCP skill 同名冲突且 uri 无路径段可消歧（名称不保证唯一）"
                ),
            }
        }
    }
    entries
}
