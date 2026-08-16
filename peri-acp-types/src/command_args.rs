//! 命令参数 schema 契约（设计 docs/design/command-system.md §73「Execution 层」：
//! ArgsSchema 完整模型，投影协议成员 §85 `args?`）。
//!
//! 模型第一版即完整——TUI 补全 / 校验器依赖其形状，残缺模型是破坏性变更；
//! wire 投影经 `_meta` 通道携带本模型，序列化形态（externally tagged + 字段
//! 原样键名）由本文件锁定，后续阶段不得再调。

use serde::{Deserialize, Serialize};

/// 命令参数 schema（设计 §73：`{ positionals, named, flags }`，serde 完整模型）。
///
/// 三个维度全部可选（缺省 = 空），`serde` 默认值保证 wire 兼容：
/// 旧投影不含 args 字段时反序列化即得全默认。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ArgsSchema {
    /// 位置参数（按声明顺序匹配）。
    #[serde(default)]
    pub positionals: Vec<ArgSpec>,
    /// 命名参数（按 `name` 字段匹配）。
    #[serde(default)]
    pub named: Vec<ArgSpec>,
    /// 布尔开关参数（presence-only）。
    #[serde(default)]
    pub flags: Vec<FlagSpec>,
}

/// 位置参数 / 命名参数共用形态。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArgSpec {
    /// 参数名（positional 为形参名，named 为 `--name` 的长形态）。
    pub name: String,
    /// 是否必填（缺省 = 可选）。
    #[serde(default)]
    pub required: bool,
    /// 参数值类型。
    pub kind: ArgKind,
    /// 人类可读描述（补全列表 / 校验器错误信息复用）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// 参数值类型（设计 §73：String | Int | Choice | Path）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ArgKind {
    /// 自由文本。
    String,
    /// 整数。
    Int,
    /// 枚举候选（TUI 补全 / 校验器依赖；空候选列表 = 运行时由 handler
    /// 补充候选——设计 §73 未定义，此语义为本阶段定案，Phase 5 解析器沿用）。
    Choice(Vec<String>),
    /// 文件路径（第一版校验存在性，补全留待 TUI 能力，设计 §73）。
    Path,
}

/// 布尔开关参数（presence-only；如需带值开关，后续以 `#[serde(default)]`
/// 新字段扩展——serde 默认值保证 wire 兼容）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlagSpec {
    /// 长形态（如 `--force`）。
    pub name: String,
    /// 短形态（如 `-f`，含连字符的展示形态；wire 原样携带）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short: Option<String>,
    /// 人类可读描述。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// 参数解析结果（[`ArgsSchema::parse`]；positionals 按声明顺序匹配，
/// named 按 `name` 字段匹配，flags 按长/短形态 presence-only）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedArgs {
    /// 位置参数值（按声明顺序）。
    pub positionals: Vec<String>,
    /// 命名参数（`--name value`；pair = (声明名, 值)）。
    pub named: Vec<(String, String)>,
    /// 命中的布尔开关（presence-only，存声明名）。
    pub flags: Vec<String>,
}

/// 单参数值校验（String 恒过；Int 解析；Choice 枚举；Path 存在性——
/// 设计 §73「第一版 Positional / Named / Flag + Choice 校验，Path 校验
/// 存在性」；Choice 空候选列表 = 运行时由 handler 补充候选，跳过校验）。
fn validate_arg_value(spec: &ArgSpec, value: &str) -> Result<(), String> {
    match &spec.kind {
        ArgKind::String => Ok(()),
        ArgKind::Int => value
            .parse::<i64>()
            .map(|_| ())
            .map_err(|_| format!("argument '{}' 需要整数，得到: '{}'", spec.name, value)),
        ArgKind::Choice(candidates) => {
            // 空候选列表 = 运行时由 handler 补充候选，跳过校验（与命中候选同返回）
            if candidates.is_empty() || candidates.iter().any(|c| c == value) {
                Ok(())
            } else {
                Err(format!(
                    "argument '{}' 必须在候选 {:?} 中，得到: '{}'",
                    spec.name, candidates, value
                ))
            }
        }
        ArgKind::Path => {
            if std::path::Path::new(value).exists() {
                Ok(())
            } else {
                Err(format!("argument '{}' 路径不存在: '{}'", spec.name, value))
            }
        }
    }
}

impl ArgsSchema {
    /// 按声明解析参数文本（设计 §73「解析器分阶段实现，第一版 Positional /
    /// Named / Flag + Choice 校验」；Phase 5 Step 6 拦截层统一调用）。
    ///
    /// 词法约定：`split_whitespace` 切分（与注册表 [`crate::command_registry::CommandRegistry::resolve`]
    /// 的 args trim 语义一致，不变式 3）；`--name` 长形态 / `-x` 短形态为
    /// named/flag，其余为 positional。
    ///
    /// 完全默认 schema（positionals/named/flags 全空，如 `/bg` 的 free-form）
    /// = 零校验，全部 token 归 positionals 原样返回。
    ///
    /// 失败返回用户可见错误信息（不含命令名前缀——由拦截层拼接）。
    pub fn parse(&self, args: &str) -> Result<ParsedArgs, String> {
        let tokens: Vec<&str> = args.split_whitespace().collect();

        // free-form（无任何声明）：零校验，全部 token 原样归 positionals。
        if self.positionals.is_empty() && self.named.is_empty() && self.flags.is_empty() {
            return Ok(ParsedArgs {
                positionals: tokens.into_iter().map(str::to_string).collect(),
                named: Vec::new(),
                flags: Vec::new(),
            });
        }

        let mut parsed = ParsedArgs {
            positionals: Vec::new(),
            named: Vec::new(),
            flags: Vec::new(),
        };
        let mut i = 0;
        while i < tokens.len() {
            let tok = tokens[i];
            if let Some(long) = tok.strip_prefix("--") {
                if let Some(spec) = self.named.iter().find(|n| n.name == long) {
                    let value = tokens
                        .get(i + 1)
                        .ok_or_else(|| format!("missing value for --{long}"))?;
                    validate_arg_value(spec, value)?;
                    parsed.named.push((spec.name.clone(), value.to_string()));
                    i += 2;
                } else if self.flags.iter().any(|f| f.name == long) {
                    parsed.flags.push(long.to_string());
                    i += 1;
                } else {
                    return Err(format!("unknown option: --{long}"));
                }
            } else if tok.starts_with('-') && tok.len() > 1 {
                // 短形态：FlagSpec.short 含连字符的完整展示（如 "-f"）。
                if let Some(spec) = self.flags.iter().find(|f| f.short.as_deref() == Some(tok)) {
                    parsed.flags.push(spec.name.clone());
                    i += 1;
                } else {
                    return Err(format!("unknown option: {tok}"));
                }
            } else if parsed.positionals.len() < self.positionals.len() {
                let spec = &self.positionals[parsed.positionals.len()];
                validate_arg_value(spec, tok)?;
                parsed.positionals.push(tok.to_string());
                i += 1;
            } else {
                return Err(format!("unexpected argument: {tok}"));
            }
        }

        // 必填校验（positionals 按序；named 按声明名）。
        for (idx, spec) in self.positionals.iter().enumerate() {
            if spec.required && parsed.positionals.len() <= idx {
                return Err(format!(
                    "missing required positional argument: {}",
                    spec.name
                ));
            }
        }
        for spec in &self.named {
            if spec.required && !parsed.named.iter().any(|(n, _)| n == &spec.name) {
                return Err(format!("missing required named argument: --{}", spec.name));
            }
        }
        Ok(parsed)
    }
}

#[cfg(test)]
#[path = "command_args_test.rs"]
mod tests;
